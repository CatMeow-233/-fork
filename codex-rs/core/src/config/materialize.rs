//! Materializes a `Config` from merged `ConfigToml`, harness overrides, and the layer stack.

use super::*;

impl Config {
    #[cfg(test)]
    pub(super) async fn load_from_base_config_with_overrides(
        cfg: ConfigToml,
        overrides: ConfigOverrides,
        codex_home: AbsolutePathBuf,
    ) -> std::io::Result<Self> {
        // Note this ignores requirements.toml enforcement for tests.
        let config_layer_stack = ConfigLayerStack::default();
        Self::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            cfg,
            overrides,
            codex_home,
            config_layer_stack,
        )
        .await
    }

    pub(crate) async fn load_config_with_layer_stack(
        fs: &dyn ExecutorFileSystem,
        mut cfg: ConfigToml,
        overrides: ConfigOverrides,
        codex_home: AbsolutePathBuf,
        config_layer_stack: ConfigLayerStack,
    ) -> std::io::Result<Self> {
        config_layer_stack.validate_project_mitm_profiles()?;
        // Keep the large config-construction future off small test thread stacks.
        Box::pin(async move {
        if cfg.experimental_thread_store_endpoint.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`experimental_thread_store_endpoint` is no longer supported; remove it from config.toml",
            ));
        }

        validate_model_providers(&cfg.model_providers)
            .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
        for (slot, dial) in cfg.tui.iter().flat_map(|tui| &tui.quick_dials) {
            if !matches!(slot.as_bytes(), [b'0'..=b'9']) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("tui.presets.{slot}: slot must be a single digit from 0 to 9"),
                ));
            }
            dial.validate().map_err(|reason| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("tui.presets.{slot}: {reason}"),
                )
            })?;
        }
        if cfg.model_post_turn_compact_threshold_percent.is_some_and(|percent| percent > 100) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "model_post_turn_compact_threshold_percent must be between 0 and 100",
            ));
        }
        let async_compaction_threshold_percent = cfg.features.as_ref()
            .and_then(|features| features.async_compaction.as_ref())
            .and_then(|feature| match feature {
                FeatureToml::Enabled(_) => None,
                FeatureToml::Config(config) => config.model_async_compact_threshold_percent,
            })
            .unwrap_or(DEFAULT_ASYNC_COMPACTION_THRESHOLD_PERCENT);
        if !(1..=100).contains(&async_compaction_threshold_percent) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "model_async_compact_threshold_percent must be between 1 and 100",
            ));
        }
        if let Some(responses_api_metadata) = cfg.responses_api_metadata.as_ref() {
            validate_extra_metadata(responses_api_metadata.iter()).map_err(|message| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, message)
            })?;
        }
        validate_managed_developer_instructions(
            config_layer_stack
                .requirements()
                .additional_developer_instructions
                .as_ref(),
        )?;
        let orchestrator = cfg.orchestrator.as_ref();
        let cloud_skill_enabled = cfg
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.skills.as_ref())
            .and_then(|skills| skills.enabled)
            .unwrap_or(true);
        let orchestrator_mcp_enabled =
            resolve_feature_enabled(orchestrator.and_then(|value| value.mcp.as_ref()));
        let mut startup_warnings = config_layer_stack
            .startup_warnings()
            .unwrap_or_default()
            .to_vec();
        let configured_sqlite_home = cfg.sqlite_home.clone();
        requirements::apply_to_config(
            &mut cfg,
            config_layer_stack.requirements(),
            &mut startup_warnings,
        );
        // Packaged defaults supply a URL even when no override was configured.
        let chatgpt_base_url_is_explicit =
            config_layer_stack.requirements().chatgpt_base_url.is_some()
                || config_layer_stack
                    .layers_high_to_low()
                    .find(|layer| layer.config.get("chatgpt_base_url").is_some())
                    .map_or(cfg.chatgpt_base_url.is_some(), |layer| {
                        !matches!(layer.name, ConfigLayerSource::PackagedDefaults { .. })
                    });
        // Destructure every field to ensure ConfigRequirements additions are
        // either applied above or handled while constructing the final Config.
        let ConfigRequirements {
            allowed_login_methods: _,
            allowed_chatgpt_workspaces: _,
            cli_auth_credentials_store,
            chatgpt_base_url: _,
            sqlite_home: _,
            unsafe_enforce_sqlite_journal_mode,
            log_dir: _,
            model_catalog_json: _,
            model_provider: _,
            model_providers: _,
            check_for_update_on_startup: _,
            allow_login_shell: _,
            feedback: _,
            otel: _,
            approval_policy: mut constrained_approval_policy,
            approvals_reviewer: mut constrained_approvals_reviewer,
            auto_review_required_models: _,
            permission_profile: mut constrained_permission_profile,
            windows_sandbox_mode: mut constrained_windows_sandbox_mode,
            web_search_mode: mut constrained_web_search_mode,
            allow_managed_hooks_only: _,
            allow_appshots: _,
            allow_remote_control: _,
            computer_use: _,
            feature_requirements,
            managed_hooks: _,
            mcp_servers,
            plugins: _,
            marketplaces: _,
            exec_policy: _,
            enforce_residency,
            network: network_requirements,
            application: _,
            filesystem: filesystem_requirements,
            additional_developer_instructions: _,
            guardian_policy_config_source: _,
            guardian_extra_policy_source: _,
        } = config_layer_stack.requirements().clone();

        // Destructure ConfigOverrides fully to ensure all overrides are applied.
        let ConfigOverrides {
            model,
            review_model: override_review_model,
            cwd,
            approval_policy: approval_policy_override,
            approvals_reviewer: approvals_reviewer_override,
            sandbox_mode,
            permission_profile,
            default_permissions: default_permissions_override,
            persisted_permission_profile_id,
            persisted_permission_profile,
            model_provider,
            service_tier: service_tier_override,
            codex_self_exe,
            codex_linux_sandbox_exe,
            main_execve_wrapper_exe: _,
            base_instructions,
            developer_instructions,
            personality,
            compact_prompt,
            show_raw_agent_reasoning,
            tools_web_search_request: override_tools_web_search_request,
            ephemeral,
            bypass_hook_trust,
            additional_writable_roots,
            workspace_roots: workspace_roots_override,
        } = overrides;
        let bypass_hook_trust = bypass_hook_trust.unwrap_or_default();

        if bypass_hook_trust {
            startup_warnings.push(
                "`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation."
                    .to_string(),
            );
        }

        if sandbox_mode.is_some() && permission_profile.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`sandbox_mode` and `permission_profile` overrides cannot both be set",
            ));
        }
        if sandbox_mode.is_some() && default_permissions_override.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`sandbox_mode` and `default_permissions` overrides cannot both be set",
            ));
        }
        if permission_profile.is_some() && default_permissions_override.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`permission_profile` and `default_permissions` overrides cannot both be set",
            ));
        }
        if let Some(profile) = cfg.profile.as_deref() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "legacy `profile = \"{profile}\"` config is no longer supported; use `--profile {profile}` with `{profile}.config.toml` instead"
                ),
            ));
        }

        let tool_suggest = resolve_tool_suggest_config(&cfg, &config_layer_stack);
        let feature_overrides = FeatureOverrides {
            web_search_request: override_tools_web_search_request,
        };

        let mut configured_features = Features::from_sources(
            FeatureConfigSource {
                features: cfg.features.as_ref(),
                experimental_use_unified_exec_tool: cfg.experimental_use_unified_exec_tool,
            },
            FeatureConfigSource {
                ..Default::default()
            },
            feature_overrides,
        );
        if cfg.agents.as_ref().and_then(|agents| agents.enabled) == Some(true)
            && !configured_features.enabled(Feature::Collab)
            && managed_features::project_overrides_legacy_subagent_opt_out(&config_layer_stack)
        {
            configured_features.enable(Feature::Collab);
        }
        let runtime_auth_mode = managed_features::resolve_runtime_auth_mode(
            &cfg, feature_requirements.as_ref(),
        )?;
        let features = ManagedFeatures::from_configured_with_warnings(
            configured_features,
            feature_requirements,
            &mut startup_warnings,
        )?;
        if let Some(FeatureToml::Config(guardian)) = cfg
            .features
            .as_ref()
            .and_then(|features| features.guardianv2.as_ref())
            && guardian.async_classifier_mode.is_some()
        {
            startup_warnings.push(
                "`[features.guardianv2].async_classifier_mode` is deprecated and ignored. Async classification always uses conversations; remove this setting from config.toml or profile configuration files.".to_owned(),
            );
        }
        let mcp_enterprise_managed_auth = McpEnterpriseManagedAuthConfig::resolve(
            &config_layer_stack,
            cfg.mcp_enterprise_managed_auth.as_ref(),
            &cfg.mcp_servers,
            features.enabled(Feature::UseXaa),
        )?;
        let non_prefixed_mcp_tool_servers = if features.enabled(Feature::NonPrefixedMcpToolNames) {
            cfg.features
                .as_ref()
                .and_then(|features| features.non_prefixed_mcp_tool_names.as_ref())
                .and_then(|feature| match feature {
                    FeatureToml::Enabled(_) => None,
                    FeatureToml::Config(config) => config.server_names.clone(),
                })
        } else {
            None
        };
        let respect_system_proxy = features.enabled(Feature::RespectSystemProxy);
        let enable_network_proxy = features.enabled(Feature::NetworkProxy);
        let allow_mxc =
            cfg.windows.as_ref().and_then(|windows| windows.allow_mxc) != Some(false);
        if !allow_mxc && resolve_windows_sandbox_mode(&cfg) == Some(WindowsSandboxModeToml::Mxc) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "windows.sandbox = \"mxc\" is not allowed when windows.allow_mxc = false",
            ));
        }
        let PreparedWindowsSandboxConfig {
            mode: windows_sandbox_mode,
            sandbox_type: windows_sandbox_type,
            level: windows_sandbox_level,
        } = prepare_windows_sandbox_config(
            resolve_windows_sandbox_mode(&cfg),
            WindowsSandboxLevel::from_features(&features),
            &mut constrained_windows_sandbox_mode,
            &mut startup_warnings,
        )?;
        let resolved_cwd = AbsolutePathBuf::try_from(normalize_for_native_workdir({
            use std::env;

            match cwd {
                None => {
                    tracing::info!("cwd not set, using current dir");
                    env::current_dir()?
                }
                Some(p) if p.is_absolute() => p,
                Some(p) => {
                    // Resolve relative path against the current working directory.
                    tracing::info!("cwd is relative, resolving against current dir");
                    let mut current = env::current_dir()?;
                    current.push(p);
                    current
                }
            }
        }))?;
        let requested_additional_writable_roots: Vec<AbsolutePathBuf> = additional_writable_roots
            .into_iter()
            .map(|path| AbsolutePathBuf::resolve_path_against_base(path, resolved_cwd.as_path()))
            .collect();
        let repo_root = resolve_root_git_project_for_trust(fs, &resolved_cwd).await;
        let active_project = cfg
            .get_active_project(
                resolved_cwd.as_path(),
                repo_root.as_ref().map(AbsolutePathBuf::as_path),
            )
            .unwrap_or(ProjectConfig { trust_level: None });
        let permission_config_syntax = resolve_permission_config_syntax(
            &config_layer_stack,
            &cfg,
            sandbox_mode,
        );
        let requirements_toml = config_layer_stack.requirements_toml();
        let persisted_permission_profile = persisted_permission_profile.filter(|profile| {
            sandbox_mode.is_none()
                && permission_profile.is_none()
                && default_permissions_override.is_none()
                && persisted_permission_profile_id.is_none()
                && requirements_toml.allowed_permission_profiles.is_none()
                // Executor-owned enforcement and materialized grants are not portable
                // thread defaults. Reject the original mode before preserving denials.
                && !matches!(profile, PermissionProfile::External { .. })
                && !profile.file_system_sandbox_policy().entries.iter().any(|entry| {
                    entry.access.can_read()
                        && matches!(entry.path, FileSystemPath::Path { .. } | FileSystemPath::GlobPattern { .. })
                })
                && permission_profile_catalog::permission_profile_satisfies_requirements(
                    &config_layer_stack,
                    profile,
                )
        });
        let persisted_permission_profile_id = if sandbox_mode.is_some()
            || permission_profile.is_some()
            || default_permissions_override.is_some()
        {
            None
        } else {
            persisted_permission_profile_id.as_deref()
        };
        let permission_path_context = ConfigPathContext::new(
            PathConvention::native(),
            Some(PathUri::from_abs_path(&resolved_cwd)),
            AbsolutePathBufGuard::home_directory()
                .and_then(|home| PathUri::from_host_native_path(home).ok()),
        );
        let effective_permission_selection = resolve_effective_permission_selection(
            cfg.permissions.as_ref(),
            default_permissions_override.as_deref(),
            persisted_permission_profile_id.map(|profile_id| (profile_id, &permission_path_context)),
            cfg.default_permissions.as_deref(),
            requirements_toml,
            &mut startup_warnings,
        )?;
        if effective_permission_selection.has_profiles()
            && !matches!(
                permission_config_syntax,
                Some(PermissionConfigSyntax::Legacy)
            )
            && effective_permission_selection.selected_profile_id.is_none()
            && !effective_permission_selection.requirements_force_profile_selection
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "config defines `[permissions]` profiles but does not set `default_permissions`",
            ));
        }

        let mut memories = cfg.memories.clone().unwrap_or_default();
        // Fresh profiles have no V1 context to preserve or warm up before using V2.
        // Keep explicit selections and transition opt-outs, and fail closed on IO errors.
        if features.enabled(Feature::MemoryTool)
            && memories.version.is_none()
            && memories.dual_write != Some(false)
            && matches!(
                tokio::fs::try_exists(codex_home.join(MemoryVersion::V1.directory_name())).await,
                Ok(false)
            )
        {
            memories.version = Some(MemoryVersion::V2);
        }
        let memories_config: MemoriesConfig = memories.into();
        let memories_root = codex_home.join(memories_config.version.directory_name());

        let profiles_are_active = effective_permission_selection.profiles_are_active(
            default_permissions_override.as_deref(),
            permission_config_syntax,
        );
        let prefer_mxc = features.enabled(Feature::PreferMxc)
            && config_allows_mxc(
                &constrained_windows_sandbox_mode,
                &effective_permission_selection,
                profiles_are_active,
                permission_profile.as_ref(),
                network_requirements.as_ref(),
                &cfg,
                enable_network_proxy,
            )?
            && codex_sandboxing::windows_mxc_available();
        let local_windows_sandbox_type =
            resolve_windows_sandbox_type(windows_sandbox_type, prefer_mxc);
        let legacy_windows_sandbox_level = windows_sandbox_level_for_legacy_checks(
            local_windows_sandbox_type,
            windows_sandbox_level,
        );
        let explicit_permission_profile_mode = effective_permission_selection
            .persisted_profile_id_was_provided
            || default_permissions_override.is_some()
            || matches!(
                permission_config_syntax,
                Some(PermissionConfigSyntax::Profiles)
            );
        let custom_permission_profiles = permission_profile_catalog_from_permissions(
            &config_layer_stack,
            effective_permission_selection.profiles.as_ref(),
            &permission_path_context,
        )?
        .into_iter()
        .filter(|profile| !is_builtin_permission_profile_name(&profile.id))
        .collect();
        let using_implicit_builtin_profile = !effective_permission_selection
            .persisted_profile_id_was_provided
            && permission_config_syntax.is_none()
            && effective_permission_selection.selected_profile_id.is_none();
        let should_seed_legacy_workspace_roots = effective_permission_selection
            .selected_profile_id
            .is_none()
            && matches!(
                permission_config_syntax,
                None | Some(PermissionConfigSyntax::Legacy)
            );
        let legacy_workspace_roots_explicit = should_seed_legacy_workspace_roots
            && cfg
                .sandbox_workspace_write
                .as_ref()
                .is_some_and(|sandbox_workspace_write| {
                    !sandbox_workspace_write.writable_roots.is_empty()
                });
        let workspace_roots_explicit = workspace_roots_override.is_some()
            || !requested_additional_writable_roots.is_empty()
            || legacy_workspace_roots_explicit;
        let mut workspace_roots = match workspace_roots_override {
            Some(workspace_roots) => workspace_roots,
            None => {
                let mut workspace_roots = vec![resolved_cwd.clone()];
                workspace_roots.extend(requested_additional_writable_roots.clone());
                if should_seed_legacy_workspace_roots
                    && let Some(sandbox_workspace_write) = cfg.sandbox_workspace_write.as_ref()
                {
                    workspace_roots.extend(sandbox_workspace_write.writable_roots.clone());
                }
                workspace_roots
            }
        };
        dedupe_absolute_paths(&mut workspace_roots);
        let (
            configured_network_proxy_config,
            mut permission_profile,
            mut file_system_sandbox_policy,
            mut active_permission_profile,
            mut profile_workspace_roots,
        ) = if let Some(permission_profile) = permission_profile {
            let (file_system_sandbox_policy, _network_sandbox_policy) =
                permission_profile.to_runtime_permissions();
            let configured_network_proxy_config =
                if profile_allows_configured_network_proxy(&permission_profile)
                    && profiles_are_active
                {
                    // PermissionProfile carries the active network sandbox bit, not the configured
                    // proxy/allowlist policy. Keep that config so active profiles can round-trip
                    // without broadening network behavior.
                    let default_permissions = effective_permission_selection
                        .selected_profile_id
                        .unwrap_or_else(|| {
                            default_builtin_permission_profile_name(
                                &active_project,
                                legacy_windows_sandbox_level,
                            )
                        });
                    network_proxy_config_for_profile_selection(
                        effective_permission_selection.profiles.as_ref(),
                        default_permissions,
                    )?
                } else {
                    NetworkProxyConfig::default()
                };
            (
                configured_network_proxy_config,
                permission_profile,
                file_system_sandbox_policy,
                None,
                Vec::new(),
            )
        } else if profiles_are_active {
            let default_permissions = effective_permission_selection
                .selected_profile_id
                .unwrap_or_else(|| {
                    default_builtin_permission_profile_name(
                        &active_project,
                        legacy_windows_sandbox_level,
                    )
                });
            let builtin_workspace_write_settings = if using_implicit_builtin_profile {
                cfg.sandbox_workspace_write.as_ref().map(|settings| WorkspaceWriteSettings {
                    writable_roots: settings.writable_roots.iter().map(PathUri::from_abs_path).collect(),
                    network_access: settings.network_access,
                    exclude_tmpdir_env_var: settings.exclude_tmpdir_env_var,
                    exclude_slash_tmp: settings.exclude_slash_tmp,
                })
            } else {
                None
            };
            let configured_network_proxy_config = network_proxy_config_for_profile_selection(
                effective_permission_selection.profiles.as_ref(),
                default_permissions,
            )?;
            let CompiledPermissionProfile {
                permission_profile,
                workspace_roots: configured_workspace_roots,
            } = compile_permission_profile(
                effective_permission_selection.profiles.as_ref(),
                default_permissions,
                &permission_path_context,
                builtin_workspace_write_settings.as_ref(),
                &mut startup_warnings,
            )?;
            let file_system_sandbox_policy = permission_profile.file_system_sandbox_policy();
            let active_permission_profile = if using_implicit_builtin_profile
                && default_permissions == BUILT_IN_WORKSPACE_PROFILE
                && cfg.sandbox_workspace_write.is_some()
            {
                // The implicit built-in profile preserves legacy
                // `[sandbox_workspace_write]` customizations, but explicitly
                // selecting `:workspace` intentionally ignores those legacy
                // settings. Do not advertise a re-selectable active profile
                // when doing so would lose roots, network, or tmp settings.
                None
            } else {
                let selected_profile_extends = cfg
                    .permissions
                    .as_ref()
                    .and_then(|permissions| permissions.entries.get(default_permissions))
                    .and_then(|profile| profile.extends.clone());
                Some(ActivePermissionProfile {
                    id: default_permissions.to_string(),
                    extends: selected_profile_extends,
                })
            };
            (
                configured_network_proxy_config,
                permission_profile,
                file_system_sandbox_policy,
                active_permission_profile,
                configured_workspace_roots,
            )
        } else {
            let configured_network_proxy_config = NetworkProxyConfig::default();
            // No named `[permissions]` profile is active, but permissions
            // should still flow through the canonical profile representation.
            // Derive the old `sandbox_mode` defaults as a profile first, then
            // keep a legacy-compatible projection only for the remaining code
            // paths that still speak `SandboxPolicy`.
            let mut permission_profile = cfg
                .derive_permission_profile(
                    sandbox_mode,
                    legacy_windows_sandbox_level,
                    Some(&active_project),
                    Some(&constrained_permission_profile),
                )
                .await;
            // The legacy-derived profiles above are expected to be
            // representable as `SandboxPolicy`. This guard keeps the old safe
            // fallback behavior if future changes make this branch derive a
            // profile with split-only filesystem semantics, such as root write
            // with carveouts or writes that are not expressible as
            // workspace-write roots.
            if let Err(err) = permission_profile.to_legacy_sandbox_policy(resolved_cwd.as_path()) {
                tracing::warn!(
                    error = %err,
                    "derived permission profile cannot be represented as a legacy sandbox policy; falling back to read-only"
                );
                permission_profile = PermissionProfile::read_only();
            }
            let (file_system_sandbox_policy, _network_sandbox_policy) =
                permission_profile.to_runtime_permissions();
            (
                configured_network_proxy_config,
                permission_profile,
                file_system_sandbox_policy,
                None,
                Vec::new(),
            )
        };
        if let Some(saved) = persisted_permission_profile {
            let (mut saved_file_system, saved_network) = saved.to_runtime_permissions();
            saved_file_system.preserve_deny_read_restrictions_from(&file_system_sandbox_policy);
            let restored = PermissionProfile::from_runtime_permissions_with_enforcement(
                saved.enforcement(),
                &saved_file_system,
                saved_network,
            );
            if permission_profile_catalog::permission_profile_satisfies_requirements(
                &config_layer_stack,
                &restored,
            ) {
                permission_profile = restored;
                file_system_sandbox_policy = saved_file_system;
                active_permission_profile = None;
                profile_workspace_roots.clear();
            }
        }
        let prepared_network = PreparedNetworkConfig::from_inputs(NetworkConfigInputs {
            configured_proxy: configured_network_proxy_config,
            feature_enabled: enable_network_proxy,
            features: cfg.features.as_ref(),
            candidate_permission_profile: &permission_profile,
            credential_broker_base_url: cfg.openai_base_url.as_deref(),
        });
        if cfg.approval_policy == Some(AskForApproval::UnlessTrusted) {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                UnsupportedUntrustedApprovalPolicyError,
            ));
        }
        let approval_policy_was_explicit =
            approval_policy_override.is_some() || cfg.approval_policy.is_some();
        let mut approval_policy = approval_policy_override
            .or(cfg.approval_policy)
            .unwrap_or_else(|| {
                if active_project.is_trusted() {
                    AskForApproval::OnRequest
                } else if active_project.is_untrusted() {
                    AskForApproval::UnlessTrusted
                } else {
                    AskForApproval::default()
                }
            });
        if !approval_policy_was_explicit
            && let Err(err) = constrained_approval_policy.can_set(&approval_policy)
        {
            tracing::warn!(
                error = %err,
                "default approval policy is disallowed by requirements; falling back to required default"
            );
            approval_policy = constrained_approval_policy.value();
        }
        let approvals_reviewer_was_explicit =
            approvals_reviewer_override.is_some() || cfg.approvals_reviewer.is_some();
        let mut approvals_reviewer = approvals_reviewer_override
            .or(cfg.approvals_reviewer)
            .unwrap_or(ApprovalsReviewer::User);
        let requested_approvals_reviewer = approvals_reviewer;
        if !approvals_reviewer_was_explicit
            && let Err(err) = constrained_approvals_reviewer.can_set(&approvals_reviewer)
        {
            tracing::warn!(
                error = %err,
                "default approvals reviewer is disallowed by requirements; falling back to required default"
            );
            approvals_reviewer = constrained_approvals_reviewer.value();
        }
        let web_search_mode =
            resolve_web_search_mode(&cfg, &features).unwrap_or(WebSearchMode::Cached);
        let web_search_config = resolve_web_search_config(&cfg);
        let experimental_request_user_input_enabled =
            resolve_experimental_request_user_input_enabled(&cfg);
        let update_plan_enabled = resolve_update_plan_enabled(&cfg);
        let tool_registry = ToolRegistryConfig {
            error_on_tool_collisions: cfg
                .features
                .as_ref()
                .and_then(|features| features.tool_registry.as_ref())
                .and_then(|config| config.error_on_tool_collisions)
                .unwrap_or_default(),
            turn_metadata_includes_tool_info: cfg
                .features
                .as_ref()
                .and_then(|features| features.tool_registry.as_ref())
                .and_then(|config| config.turn_metadata_includes_tool_info)
                .unwrap_or_default(),
        };
        let code_mode = resolve_code_mode_config(&cfg);
        let multi_agent_v2 = resolve_multi_agent_v2_config(&cfg);
        let token_budget = resolve_token_budget_config(&cfg, &features)?;
        let rollout_budget = resolve_rollout_budget_config(&cfg, &features)?;
        let current_time_reminder = resolve_current_time_reminder_config(&cfg, &features)?;
        let sleep_tool_mode = cfg
            .features
            .as_ref()
            .and_then(|features| features.sleep_tool.as_ref())
            .and_then(|feature| match feature {
                FeatureToml::Enabled(_) => None,
                FeatureToml::Config(config) => config.mode,
            })
            .unwrap_or_default();
        let terminal_resize_reflow = resolve_terminal_resize_reflow_config(&cfg);

        let agent_roles =
            load_agent_roles(fs, &cfg, &config_layer_stack, &mut startup_warnings).await?;

        let openai_base_url = cfg
            .openai_base_url
            .clone()
            .filter(|value| !value.is_empty());

        let mut model_providers =
            merge_configured_model_providers(built_in_model_providers(openai_base_url), cfg.model_providers)
                .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidData, message))?;

        if let Some(provider) = model_providers.get_mut(GCP_PROVIDER_ID) {
            apply_default_gcp_auth(provider, &resolved_cwd);
        }

        let model_provider_id = config_layer_stack.required_model_provider().map(str::to_string)
            .or(model_provider)
            .or(cfg.model_provider)
            .unwrap_or_else(|| "openai".to_string());
        let model_provider = model_providers
            .get(&model_provider_id)
            .ok_or_else(|| {
                let message = if model_provider_id == LEGACY_OLLAMA_CHAT_PROVIDER_ID {
                    OLLAMA_CHAT_PROVIDER_REMOVED_ERROR.to_string()
                } else {
                    format!("Model provider `{model_provider_id}` not found")
                };
                std::io::Error::new(std::io::ErrorKind::NotFound, message)
            })?
            .clone();

        let shell_environment_policy = ShellEnvironmentPolicy::from(cfg.shell_environment_policy);
        let allow_login_shell = cfg.allow_login_shell.unwrap_or(true);

        let history = cfg.history.unwrap_or_default();

        if multi_agent_v2.max_concurrent_threads_per_session == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.max_concurrent_threads_per_session must be at least 1",
            ));
        }
        validate_multi_agent_v2_wait_timeout(
            "features.multi_agent_v2.min_wait_timeout_ms",
            multi_agent_v2.min_wait_timeout_ms,
        )?;
        validate_multi_agent_v2_wait_timeout(
            "features.multi_agent_v2.max_wait_timeout_ms",
            multi_agent_v2.max_wait_timeout_ms,
        )?;
        validate_multi_agent_v2_wait_timeout(
            "features.multi_agent_v2.default_wait_timeout_ms",
            multi_agent_v2.default_wait_timeout_ms,
        )?;
        if multi_agent_v2.min_wait_timeout_ms > multi_agent_v2.max_wait_timeout_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.min_wait_timeout_ms must be at most features.multi_agent_v2.max_wait_timeout_ms",
            ));
        }
        if multi_agent_v2.default_wait_timeout_ms < multi_agent_v2.min_wait_timeout_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.default_wait_timeout_ms must be at least features.multi_agent_v2.min_wait_timeout_ms",
            ));
        }
        if multi_agent_v2.default_wait_timeout_ms > multi_agent_v2.max_wait_timeout_ms {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.multi_agent_v2.default_wait_timeout_ms must be at most features.multi_agent_v2.max_wait_timeout_ms",
            ));
        }
        validate_multi_agent_v2_tool_namespace(multi_agent_v2.tool_namespace.as_deref())?;
        let agents_enabled = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.enabled)
            .unwrap_or(true);
        let agent_max_threads = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.max_concurrent_threads_per_session);
        if agent_max_threads == Some(0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "agents.max_concurrent_threads_per_session must be at least 1",
            ));
        }
        let agent_max_depth = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.max_depth)
            .unwrap_or(DEFAULT_AGENT_MAX_DEPTH);
        let agent_default_subagent_model = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.default_subagent_model.clone());
        let agent_default_subagent_reasoning_effort = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.default_subagent_reasoning_effort.clone());
        let agent_interrupt_message_enabled = cfg
            .agents
            .as_ref()
            .and_then(|agents| agents.interrupt_message)
            .unwrap_or(true);
        let background_terminal_max_timeout = cfg
            .background_terminal_max_timeout
            .unwrap_or(DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS)
            .max(MIN_EMPTY_YIELD_TIME_MS);
        let thread_unload_delay =
            Duration::from_secs(cfg.thread_unload_delay_secs.unwrap_or(/*default*/ 60));
        if std::time::Instant::now()
            .checked_add(thread_unload_delay)
            .is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "thread_unload_delay_secs is too large",
            ));
        }

        let ghost_snapshot = {
            let mut config = GhostSnapshotConfig::default();
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(ignore_over_bytes) = ghost_snapshot.ignore_large_untracked_files
            {
                config.ignore_large_untracked_files = if ignore_over_bytes > 0 {
                    Some(ignore_over_bytes)
                } else {
                    None
                };
            }
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(threshold) = ghost_snapshot.ignore_large_untracked_dirs
            {
                config.ignore_large_untracked_dirs =
                    if threshold > 0 { Some(threshold) } else { None };
            }
            if let Some(ghost_snapshot) = cfg.ghost_snapshot.as_ref()
                && let Some(disable_warnings) = ghost_snapshot.disable_warnings
            {
                config.disable_warnings = disable_warnings;
            }
            config
        };

        let forced_chatgpt_workspace_id = cfg
            .forced_chatgpt_workspace_id
            .clone()
            .map(codex_config::config_toml::ForcedChatgptWorkspaceIds::into_vec)
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|values| !values.is_empty());

        let forced_login_method = cfg.forced_login_method;

        let model = model.or(cfg.model);
        let notices = cfg.notice.unwrap_or_default();
        let service_tier = match service_tier_override {
            Some(Some(service_tier)) => Some(service_tier),
            Some(None) => Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()),
            None => cfg.service_tier,
        };
        let service_tier = service_tier.and_then(|service_tier| {
            match ServiceTier::from_request_value(&service_tier) {
                Some(ServiceTier::Fast) => features
                    .enabled(Feature::FastMode)
                    .then(|| ServiceTier::Fast.request_value().to_string()),
                Some(ServiceTier::Flex) => Some(ServiceTier::Flex.request_value().to_string()),
                None if service_tier == "ultrafast" => features
                    .enabled(Feature::UltrafastMode)
                    .then_some(service_tier),
                None => Some(service_tier),
            }
        });

        let compact_prompt = compact_prompt.or(cfg.compact_prompt).and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });

        // Load base instructions override from a file if specified. If the
        // path is relative, resolve it against the effective cwd so the
        // behaviour matches other path-like config values.
        let model_instructions_path = cfg.model_instructions_file.as_ref();
        let file_base_instructions = Self::try_read_non_empty_file(
            fs,
            model_instructions_path,
            "model instructions file",
        )
        .await?;
        let base_instructions = base_instructions
            .or(file_base_instructions)
            .or(cfg.instructions.clone());
        let base_instructions_provenance = base_instructions
            .as_ref()
            .map(|_| BaseInstructionsProvenance::Custom);
        let developer_instructions = developer_instructions.or(cfg.developer_instructions);
        let include_permissions_instructions = cfg.include_permissions_instructions.unwrap_or(true);
        let include_apps_instructions = cfg.include_apps_instructions.unwrap_or(true);
        let include_collaboration_mode_instructions =
            cfg.include_collaboration_mode_instructions.unwrap_or(true);
        let include_skill_instructions = cfg
            .skills
            .as_ref()
            .and_then(|skills| skills.include_instructions)
            .unwrap_or(true);
        let skill_max_context_tokens = cfg
            .skills
            .as_ref()
            .and_then(|skills| skills.max_context_tokens);
        let include_environment_context = cfg.include_environment_context.unwrap_or(true);
        let include_environment_context_time = cfg.include_environment_context_time.unwrap_or(true);
        let (guardian_policy_config, guardian_extra_policy) = resolve_guardian_policy_text(
            config_layer_stack.requirements_toml(), cfg.auto_review.as_ref(),
        );
        let guardian_transcript_mode = cfg
            .features
            .as_ref()
            .and_then(|features| features.guardianv2.as_ref())
            .and_then(|feature| match feature {
                FeatureToml::Config(config) => config.transcript_mode,
                FeatureToml::Enabled(_) => None,
            })
            .unwrap_or_default();
        let guardian_sync_elastic_budget = cfg.features.as_ref()
            .and_then(|features| features.guardianv2.as_ref())
            .and_then(|feature| match feature {
                FeatureToml::Config(guardian) if guardian.sync_elastic_budgeting == Some(true) => Some(guardian),
                FeatureToml::Config(_) | FeatureToml::Enabled(_) => None,
            })
            .map(|guardian| {
                let sections = guardian.sync_elastic_sections.as_ref().filter(|sections| !sections.is_empty())
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData,
                        "sync_elastic_sections is required when sync_elastic_budgeting is enabled"))?;
                let mut decoded_sections = BTreeMap::new();
                for (name, limits) in sections {
                    let section = codex_guardian_context::ElasticSection::deserialize(name.as_str().into_deserializer())
                        .map_err(|error: serde::de::value::Error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                    let mut section_limits = codex_guardian_context::ElasticSectionLimits::new(
                        limits.min_total_section_tokens,
                        limits.max_section_entry_tokens,
                    ).map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                    if let Some(maximum) = limits.max_total_section_tokens {
                        section_limits = section_limits.with_max_total_section_tokens(maximum)
                            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                    }
                    if decoded_sections.insert(section, section_limits).is_some() {
                        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                            format!("duplicate sync_elastic_sections section: {name}")));
                    }
                }
                let policy = codex_guardian_context::ElasticBudgetConfig { sections: decoded_sections };
                policy.validate().map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                Ok::<_, std::io::Error>(policy)
            }).transpose()?;
        let guardian_policy_template = cfg
            .auto_review
            .as_ref()
            .and_then(|auto_review| {
                normalize_guardian_policy_config(
                    auto_review.experimental_policy_template.as_deref(),
                )
            });
        let guardian_conversation_history_prompt = cfg.auto_review.as_ref().and_then(|auto_review| {
            normalize_guardian_policy_config(
                auto_review.experimental_conversation_history_prompt.as_deref(),
            )
        });
        let guardian_conversation_history_max_output_tokens = cfg
            .auto_review
            .as_ref()
            .and_then(|auto_review| auto_review.conversation_history_max_output_tokens);
        let personality = personality.or(cfg.personality);

        let experimental_compact_prompt_path = cfg.experimental_compact_prompt_file.as_ref();
        let file_compact_prompt = Self::try_read_non_empty_file(
            fs,
            experimental_compact_prompt_path,
            "experimental compact prompt file",
        )
        .await?;
        let compact_prompt = compact_prompt.or(file_compact_prompt);
        let review_model = override_review_model.or(cfg.review_model);

        let check_for_update_on_startup = cfg.check_for_update_on_startup.unwrap_or(true);
        let model_catalog = load_model_catalog(cfg.model_catalog_json.clone())?;

        let log_dir = cfg
            .log_dir
            .as_ref()
            .map(AbsolutePathBuf::to_path_buf)
            .unwrap_or_else(|| codex_home.join("log").to_path_buf());
        let sqlite_home_env = resolve_sqlite_home_env(&resolved_cwd);
        requirements::push_sqlite_home_env_override_warning(
            configured_sqlite_home.as_ref(),
            sqlite_home_env.as_deref(),
            config_layer_stack.requirements().sqlite_home.as_ref(),
            &mut startup_warnings,
        );
        let sqlite_home = cfg
            .sqlite_home
            .as_ref()
            .cloned()
            .or(sqlite_home_env)
            .unwrap_or_else(|| codex_home.clone());
        let enforce_delete_on_nfs =
            unsafe_enforce_sqlite_journal_mode.is_some_and(|required| required.value);
        let mut sqlite = codex_state::SqliteConfig::from_sqlite_home_with_nfs_enforcement(
            sqlite_home,
            enforce_delete_on_nfs,
        );
        let requirements = config_layer_stack.requirements_toml();
        if cfg.log_dir.is_some() && requirements.sqlite_home.is_none() {
            sqlite = sqlite.with_local_logs_home(log_dir.clone());
        }
        let original_permission_profile = permission_profile.clone();
        apply_requirement_constrained_value(
            "approval_policy",
            approval_policy,
            &mut constrained_approval_policy,
            &mut startup_warnings,
        )?;
        if let Some(Sourced {
            value: filesystem_requirements,
            source: filesystem_requirements_source,
        }) = filesystem_requirements.as_ref()
            && !filesystem_requirements.deny_read.is_empty()
        {
            let requirement_source = filesystem_requirements_source.clone();
            constrained_permission_profile
                .value
                .add_validator(move |permission_profile| {
                    validate_permission_profile_for_deny_read(
                        permission_profile,
                        &requirement_source,
                    )
                })
                .map_err(std::io::Error::from)?;
        }
        apply_requirement_constrained_value(
            "approvals_reviewer",
            approvals_reviewer,
            &mut constrained_approvals_reviewer,
            &mut startup_warnings,
        )?;
        let permission_profile_was_constrained = apply_requirement_constrained_value(
            "permission_profile",
            permission_profile,
            &mut constrained_permission_profile,
            &mut startup_warnings,
        )?;
        if permission_profile_was_constrained
            && sandbox_mode_requirement_for_permission_profile(&original_permission_profile)
                == SandboxModeRequirement::DangerFullAccess
            && constrained_permission_profile.get() == &PermissionProfile::read_only()
            && constrained_approval_policy.value() == AskForApproval::Never
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`approval_policy = \"never\"` cannot be used because requirements do not allow `sandbox_mode = \"danger-full-access\"`; Codex would fall back to read-only permissions with approvals disabled. Choose an `approval_policy` based on what you need, such as `on-request`, or choose an allowed sandbox mode.",
            ));
        }
        if permission_profile_was_constrained {
            // The selected profile no longer describes the effective
            // permissions after requirements forced a fallback.
            active_permission_profile = None;
            profile_workspace_roots.clear();
        }
        apply_requirement_constrained_value(
            "web_search_mode",
            web_search_mode,
            &mut constrained_web_search_mode,
            &mut startup_warnings,
        )?;

        let mcp_servers = constrain_mcp_servers(cfg.mcp_servers.clone(), mcp_servers.as_ref())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{e}")))?;

        let network_permission_profile = constrained_permission_profile.get().clone();
        let network = build_network_proxy_spec(
            prepared_network.configured_proxy,
            network_requirements,
            &network_permission_profile,
            &shell_environment_policy.r#set,
            &features,
            cfg.features.as_ref(),
        )?;
        let mut helper_readable_roots: Vec<_> = std::env::var_os("PATH")
            .as_deref()
            .and_then(|path| permissions::active_arg0_helper_dir(&codex_home, path))
            .into_iter()
            .collect();
        if features.enabled(Feature::MemoryTool) && memories_config.use_memories {
            helper_readable_roots.push(memories_root);
        }
        let effective_permission_profile = constrained_permission_profile.value.get().clone();
        let (mut effective_file_system_sandbox_policy, effective_network_sandbox_policy) =
            effective_permission_profile.to_runtime_permissions();
        if effective_permission_profile != original_permission_profile {
            effective_file_system_sandbox_policy
                .preserve_deny_read_restrictions_from(&file_system_sandbox_policy);
        }
        let managed_deny_read_policy = filesystem_requirements
            .as_ref()
            .filter(|Sourced { value, .. }| !value.deny_read.is_empty())
            .map(|Sourced { value, .. }| -> std::io::Result<_> {
                let mut policy = FileSystemSandboxPolicy::restricted(Vec::new());
                value.apply_to_policy(&mut policy, codex_utils_path_uri::PathConvention::native())?;
                Ok(Arc::new(policy))
            })
            .transpose()?;
        if let Some(managed_deny_read_policy) = managed_deny_read_policy.as_ref() {
            effective_file_system_sandbox_policy
                .preserve_deny_read_restrictions_from(managed_deny_read_policy);
        }
        let effective_file_system_sandbox_policy = effective_file_system_sandbox_policy
            .with_additional_readable_roots(resolved_cwd.as_path(), &helper_readable_roots);
        let effective_permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            effective_permission_profile.enforcement(),
            &effective_file_system_sandbox_policy,
            effective_network_sandbox_policy,
        );
        constrained_permission_profile
            .value
            .set(effective_permission_profile)
            .map_err(std::io::Error::from)?;
        if let Some(Sourced {
            source: requirement_source,
            ..
        }) = filesystem_requirements.as_ref()
            && let Some(managed_file_system_policy) = managed_deny_read_policy.as_ref()
        {
            let cwd = PathUri::from_abs_path(&resolved_cwd);
            let user_home_dir = PathUri::from_host_native_path("~").ok();
            let temporary_directories = std::env::var_os("TMPDIR")
                .filter(|path| !path.is_empty())
                .and_then(|path| AbsolutePathBuf::from_absolute_path(PathBuf::from(path)).ok())
                .map(PathUri::from)
                .into_iter()
                .collect::<Vec<_>>();
            let context = FileSystemSandboxPolicyContext {
                cwd: &cwd,
                workspace_roots: std::slice::from_ref(&cwd),
                user_home_dir: user_home_dir.as_ref(),
                temporary_directories: Some(&temporary_directories),
            };
            let validator = DenyReadValidator::new(managed_file_system_policy, &context)
                .map_err(std::io::Error::other)?;
            let requirement_source = requirement_source.clone();
            constrained_permission_profile
                .value
                .add_validator(move |permission_profile| {
                    let mut file_system_policy = permission_profile.file_system_sandbox_policy();
                    // Preserve the native conversion boundary before the shared URI checks.
                    // Mandatory entries are Deny entries, so their identity stays unchanged.
                    file_system_policy.entries.retain_mut(|entry| {
                        if entry.access.can_read()
                            && let FileSystemPath::Path { path } = &mut entry.path
                        {
                            let Ok(native_path) = path.to_abs_path() else {
                                return false;
                            };
                            *path = PathUri::from(native_path);
                        }
                        true
                    });
                    let context = FileSystemSandboxPolicyContext {
                        cwd: &cwd,
                        workspace_roots: std::slice::from_ref(&cwd),
                        user_home_dir: user_home_dir.as_ref(),
                        temporary_directories: Some(&temporary_directories),
                    };
                    validator.validate(&file_system_policy, &context).map_err(|violation| {
                        let candidate = match violation {
                            DenyReadViolation::MissingRequiredDeny => "missing managed deny".to_string(),
                            DenyReadViolation::ReadablePath(path) => path.to_abs_path().map_or_else(
                                |_| path.to_string(),
                                |path| path.to_string_lossy().into_owned(),
                            ),
                        };
                        ConstraintError::InvalidValue {
                            field_name: "permissions.filesystem",
                            candidate,
                            allowed: "all managed deny_read restrictions".to_string(),
                            requirement_source: requirement_source.clone(),
                        }
                    })
                })
                .map_err(std::io::Error::from)?;
        }

        let permission_profile_state = PermissionProfileState::from_constrained_active_profile(
            constrained_permission_profile.value,
            active_permission_profile,
            profile_workspace_roots,
        )
        .map_err(std::io::Error::from)?;
        let otel = otel::resolve_config(cfg.otel.unwrap_or_default(), &mut startup_warnings);
        let mut config = Self {
            plugins: cfg.plugins,
            prefer_mxc,
            model,
            daybreak_enabled: cfg.daybreak.unwrap_or(false),
            service_tier,
            review_model,
            model_context_window: cfg.model_context_window,
            model_auto_compact_token_limit: cfg.model_auto_compact_token_limit,
            model_auto_compact_token_limit_scope: cfg
                .model_auto_compact_token_limit_scope
                .unwrap_or_default(),
            model_post_turn_compact_threshold_percent: cfg
                .model_post_turn_compact_threshold_percent
                .unwrap_or_default(),
            async_compaction_threshold_percent,
            model_provider_id,
            model_provider,
            cwd: resolved_cwd,
            workspace_roots: workspace_roots.clone(),
            workspace_roots_explicit,
            startup_warnings,
            permissions: Permissions {
                approval_policy: constrained_approval_policy.value,
                permission_profile_state,
                managed_deny_read_policy,
                workspace_roots,
                network,
                allow_login_shell,
                shell_environment_policy,
                windows_sandbox_mode,
                windows_sandbox_type,
            },
            explicit_permission_profile_mode,
            custom_permission_profiles,
            approvals_reviewer: constrained_approvals_reviewer.value(),
            requested_approvals_reviewer: (requested_approvals_reviewer
                != constrained_approvals_reviewer.value())
            .then_some(requested_approvals_reviewer),
            enforce_residency: enforce_residency.value,
            notify: cfg.notify,
            base_instructions,
            base_instructions_provenance,
            personality,
            developer_instructions,
            compact_prompt,
            include_permissions_instructions,
            include_apps_instructions,
            include_collaboration_mode_instructions,
            include_skill_instructions,
            skill_max_context_tokens,
            cloud_skill_enabled,
            orchestrator_mcp_enabled,
            include_environment_context,
            include_environment_context_time,
            // The config.toml omits "_mode" because it's a config file. However, "_mode"
            // is important in code to differentiate the mode from the store implementation.
            cli_auth_credentials_store_mode: match cli_auth_credentials_store {
                Some(required) => required.value,
                None => resolve_cli_auth_credentials_store_mode(
                    cfg.cli_auth_credentials_store.unwrap_or_default(),
                    env!("CARGO_PKG_VERSION"),
                ),
            },
            mcp_servers,
            non_prefixed_mcp_tool_servers,
            mcp_enterprise_managed_auth,
            // The config.toml omits "_mode" because it's a config file. However, "_mode"
            // is important in code to differentiate the mode from the store implementation.
            mcp_oauth_credentials_store_mode: resolve_mcp_oauth_credentials_store_mode(
                cfg.mcp_oauth_credentials_store.unwrap_or_default(),
                env!("CARGO_PKG_VERSION"),
            ),
            mcp_oauth_callback_port: cfg.mcp_oauth_callback_port,
            mcp_oauth_callback_url: cfg.mcp_oauth_callback_url.clone(),
            mcp_optional_startup_grace: cfg
                .mcp_optional_startup_grace_ms
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_OPTIONAL_MCP_STARTUP_GRACE),
            model_providers,
            project_doc_max_bytes: cfg.project_doc_max_bytes.unwrap_or(AGENTS_MD_MAX_BYTES),
            project_doc_fallback_filenames: cfg
                .project_doc_fallback_filenames
                .unwrap_or_default()
                .into_iter()
                .filter_map(|name| {
                    let trimmed = name.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                })
                .collect(),
            tool_output_token_limit: cfg.tool_output_token_limit,
            agents_enabled,
            agent_max_threads,
            agent_default_subagent_model,
            agent_default_subagent_reasoning_effort,
            agent_max_depth,
            agent_roles,
            max_goal_token_budget: cfg
                .goals
                .as_ref()
                .and_then(|goals| goals.max_goal_token_budget)
                .map(|max_goal_token_budget| {
                    i64::try_from(max_goal_token_budget.get()).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "goals.max_goal_token_budget exceeds the maximum supported token budget",
                        )
                    })
                })
                .transpose()?,
            memories: memories_config,
            agent_interrupt_message_enabled,
            codex_home,
            sqlite,
            log_dir,
            config_layer_stack,
            application_network_policy: Default::default(),
            application_auth_route_config: None,
            history,
            ephemeral: ephemeral.unwrap_or_default(),
            allow_persistent_dynamic_tool_updates: false,
            extra_config: None,
            bypass_hook_trust,
            file_opener: cfg.file_opener.unwrap_or(UriBasedFileOpener::VsCode),
            codex_self_exe,
            codex_linux_sandbox_exe,
            main_execve_wrapper_exe: None,
            zsh_path: None,

            hide_agent_reasoning: cfg.hide_agent_reasoning.unwrap_or(false),
            show_raw_agent_reasoning: cfg
                .show_raw_agent_reasoning
                .or(show_raw_agent_reasoning)
                .unwrap_or(false),
            guardian_policy_config,
            guardian_policy_revision: 0,
            guardian_extra_policy,
            guardian_policy_template,
            guardian_transcript_mode,
            guardian_sync_elastic_budget,
            guardian_conversation_history_prompt,
            guardian_conversation_history_max_output_tokens,
            guardian_review_timeout: cfg
                .auto_review
                .as_ref()
                .and_then(|auto_review| auto_review.timeout_ms)
                .map_or(Some(codex_guardian_reviewer::REVIEW_TIMEOUT), |timeout_ms| {
                    (timeout_ms != 0).then(|| Duration::from_millis(u64::from(timeout_ms)))
                }),
            guardian_circuit_break_action: cfg
                .auto_review
                .as_ref()
                .and_then(|auto_review| auto_review.circuit_break_action)
                .unwrap_or_default(),
            model_reasoning_effort: cfg.model_reasoning_effort,
            plan_mode_reasoning_effort: cfg.plan_mode_reasoning_effort,
            model_reasoning_summary: cfg.model_reasoning_summary,
            model_catalog,
            model_verbosity: cfg.model_verbosity,
            chatgpt_base_url_is_explicit,
            chatgpt_base_url: cfg
                .chatgpt_base_url
                .unwrap_or_else(|| DEFAULT_CHATGPT_BASE_URL.to_string()),
            respect_system_proxy,
            apps_mcp_product_sku: cfg.apps_mcp_product_sku.clone(),
            responses_api_metadata: cfg.responses_api_metadata.unwrap_or_default(),
            realtime_audio: cfg
                .audio
                .map_or_else(RealtimeAudioConfig::default, |audio| RealtimeAudioConfig {
                    microphone: audio.microphone,
                    microphone_channel: audio.microphone_channel,
                    speaker: audio.speaker,
                }),
            experimental_realtime_ws_base_url: cfg.experimental_realtime_ws_base_url,
            experimental_realtime_webrtc_call_base_url: cfg
                .experimental_realtime_webrtc_call_base_url,
            experimental_realtime_ws_model: cfg.experimental_realtime_ws_model,
            realtime: cfg
                .realtime
                .map_or_else(RealtimeConfig::default, |realtime| {
                    let defaults = RealtimeConfig::default();
                    RealtimeConfig {
                        version: realtime.version.unwrap_or(defaults.version),
                        session_type: realtime.session_type.unwrap_or(defaults.session_type),
                        transport: realtime.transport.unwrap_or(defaults.transport),
                        voice: realtime.voice,
                    }
                }),
            experimental_realtime_ws_backend_prompt: cfg.experimental_realtime_ws_backend_prompt,
            experimental_realtime_ws_startup_context: cfg.experimental_realtime_ws_startup_context,
            experimental_realtime_start_instructions: cfg.experimental_realtime_start_instructions,
            experimental_thread_store: thread_store_config(cfg.experimental_thread_store),
            forced_chatgpt_workspace_id,
            forced_login_method,
            web_search_mode: constrained_web_search_mode.value,
            web_search_config,
            experimental_request_user_input_enabled,
            update_plan_enabled,
            tool_registry,
            code_mode,
            background_terminal_max_timeout,
            thread_unload_delay,
            ghost_snapshot,
            multi_agent_v2,
            token_budget,
            token_budget_startup_config: None,
            rollout_budget,
            current_time_reminder,
            sleep_tool_mode,
            runtime_auth_mode,
            features,
            runtime_feature_defaults: BTreeMap::new(),
            suppress_unstable_features_warning: cfg
                .suppress_unstable_features_warning
                .unwrap_or(false),
            active_project,
            notices,
            check_for_update_on_startup,
            disable_paste_burst: cfg
                .tui
                .as_ref()
                .and_then(|tui| tui.disable_paste_burst)
                .or(cfg.disable_paste_burst)
                .unwrap_or(false),
            analytics_enabled: cfg.analytics.as_ref().and_then(|a| a.enabled),
            feedback_enabled: cfg
                .feedback
                .as_ref()
                .and_then(|feedback| feedback.enabled)
                .unwrap_or(true),
            tool_suggest,
            tui_notifications: cfg
                .tui
                .as_ref()
                .map(|t| t.notification_settings.clone())
                .unwrap_or_default(),
            animations: cfg.tui.as_ref().map(|t| t.animations).unwrap_or(true),
            tui_effects: cfg.tui.as_ref().map(|t| t.effects).unwrap_or_default(),
            tui_rendering: cfg.tui.as_ref().map(|t| t.rendering).unwrap_or_default(),
            show_tooltips: cfg.tui.as_ref().map(|t| t.show_tooltips).unwrap_or(true),
            tui_show_server_version_notice: cfg
                .tui
                .as_ref()
                .map(|t| t.show_server_version_notice)
                .unwrap_or(true),
            tui_auto_recap: cfg.tui.as_ref().map(|t| t.auto_recap).unwrap_or(/*default*/ true),
            model_availability_nux: cfg
                .tui
                .as_ref()
                .map(|t| t.model_availability_nux.clone())
                .unwrap_or_default(),
            tui_question_esc_back: cfg.tui.as_ref().map(|t| t.question_esc_back).unwrap_or(true),
            tui_vim_mode_default: cfg
                .tui
                .as_ref()
                .map(|t| t.vim_mode_default)
                .unwrap_or(false),
            tui_raw_output_mode: cfg
                .tui
                .as_ref()
                .map(|t| t.raw_output_mode)
                .unwrap_or(false),
            tui_fullscreen_transcript: cfg
                .tui
                .as_ref()
                .is_none_or(|tui| tui.fullscreen_transcript),
            tui_sidebar_width_percent: cfg.tui.as_ref().and_then(|tui| tui.sidebar_width_percent),
            tui_mouse_scroll_speed: cfg.tui.as_ref().and_then(|tui| tui.mouse_scroll_speed),
            tui_copy_on_select: cfg
                .tui
                .as_ref()
                .map(|tui| tui.copy_on_select)
                .unwrap_or_default(),
            tui_copy_on_select_clear_selection: cfg
                .tui
                .as_ref()
                .is_some_and(|tui| tui.copy_on_select_clear_selection),
            tui_right_click_paste: cfg
                .tui
                .as_ref()
                .map(|tui| tui.right_click_paste)
                .unwrap_or_default(),
            tui_alternate_screen: cfg
                .tui
                .as_ref()
                .map(|t| t.alternate_screen)
                .unwrap_or_default(),
            tui_status_line: cfg.tui.as_ref().and_then(|t| t.status_line.clone()),
            tui_status_line_use_colors: cfg
                .tui
                .as_ref()
                .map(|t| t.status_line_use_colors)
                .unwrap_or(true),
            tui_terminal_title: cfg.tui.as_ref().and_then(|t| t.terminal_title.clone()),
            tui_theme: cfg.tui.as_ref().and_then(|t| t.theme.clone()),
            tui_pet: cfg.tui.as_ref().and_then(|t| t.pet.clone()),
            tui_pet_anchor: cfg
                .tui
                .as_ref()
                .map(|t| t.pet_anchor)
                .unwrap_or_default(),
            tui_session_picker_view: cfg
                .tui
                .as_ref()
                .and_then(|t| t.session_picker_view)
                .unwrap_or_default(),
            tui_agents_overview_grouping: cfg.tui.as_ref().map(|t| t.agents_overview_grouping).unwrap_or_default(),
            tui_resume_cwd: cfg.tui.as_ref().and_then(|t| t.resume_cwd),
            terminal_resize_reflow,
            tui_keymap: cfg
                .tui
                .as_ref()
                .map(|t| t.keymap.clone())
                .unwrap_or_default(),
            tui_quick_dials: cfg
                .tui
                .as_ref()
                .map(|t| t.quick_dials.clone())
                .unwrap_or_default(),
            otel,
        };
        if let Some(message) = config.credential_masking_warning(config.permissions.permission_profile()) {
            tracing::warn!("{message}");
            config.startup_warnings.push(message.to_string());
        }
        Ok(config)
        })
        .await
    }

    /// If `path` is `Some`, attempts to read the file at the given path and
    /// returns its contents as a trimmed `String`. If the file is empty, or
    /// is `Some` but cannot be read, returns an `Err`.
    async fn try_read_non_empty_file(
        fs: &dyn ExecutorFileSystem,
        path: Option<&AbsolutePathBuf>,
        context: &str,
    ) -> std::io::Result<Option<String>> {
        let Some(path) = path else {
            return Ok(None);
        };

        let path_uri = PathUri::from_abs_path(path);
        let contents = fs
            .read_file_text(&path_uri, ReadFileOptions::default(), /*sandbox*/ None)
            .await
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to read {context} {}: {e}", path.display()),
                )
            })?;

        let s = contents.trim().to_string();
        if s.is_empty() {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{context} is empty: {}", path.display()),
            ))
        } else {
            Ok(Some(s))
        }
    }
}
