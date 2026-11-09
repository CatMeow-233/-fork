//! Listing configured marketplaces and reading plugin details from them.

use super::*;

impl PluginsManager {
    pub fn list_marketplaces_for_context(
        &self,
        context: &PluginMarketplaceContext,
        include_openai_curated: bool,
    ) -> Result<ConfiguredMarketplaceListOutcome, MarketplaceError> {
        context.list_marketplaces(self, include_openai_curated)
    }

    pub fn list_marketplaces_for_config(
        &self,
        config: &PluginsConfigInput,
        additional_roots: &[AbsolutePathBuf],
        include_openai_curated: bool,
    ) -> Result<ConfiguredMarketplaceListOutcome, MarketplaceError> {
        if !config.plugins_enabled {
            return Ok(ConfiguredMarketplaceListOutcome::default());
        }

        let plugin_states = self.configured_plugin_states(config);
        self.list_marketplaces_for_config_with_states(
            config,
            additional_roots,
            include_openai_curated,
            &plugin_states,
        )
    }

    pub(super) fn list_marketplaces_for_config_with_states(
        &self,
        config: &PluginsConfigInput,
        additional_roots: &[AbsolutePathBuf],
        include_openai_curated: bool,
        plugin_states: &ConfiguredPluginStates,
    ) -> Result<ConfiguredMarketplaceListOutcome, MarketplaceError> {
        let marketplace_roots =
            self.marketplace_roots(config, additional_roots, include_openai_curated);
        let marketplace_outcome = self.list_marketplaces_with_policy(config, &marketplace_roots)?;
        let mut seen_plugin_keys = HashSet::new();
        let marketplaces = marketplace_outcome
            .marketplaces
            .into_iter()
            .filter_map(|marketplace| {
                let marketplace_name = marketplace.name.clone();
                let plugins = marketplace
                    .plugins
                    .into_iter()
                    .filter_map(|plugin| {
                        let plugin_key = format!("{}@{marketplace_name}", plugin.name);
                        if !seen_plugin_keys.insert(plugin_key.clone()) {
                            return None;
                        }
                        if !self.restriction_product_matches(plugin.policy.products.as_deref()) {
                            return None;
                        }
                        let plugin_id =
                            PluginId::new(plugin.name.clone(), marketplace_name.clone()).ok();
                        let installed = plugin_states.installed.contains(&plugin_key);
                        let installed_version = installed.then_some(()).and_then(|_| {
                            plugin_id
                                .as_ref()
                                .and_then(|plugin_id| self.store.active_plugin_version(plugin_id))
                        });
                        let enabled = plugin_states.enabled.contains(&plugin_key);
                        let mut interface = plugin.interface;
                        let mut local_version = plugin.local_version;
                        let manifest_fallback = plugin.manifest_fallback.clone();
                        if installed
                            && plugin.source.is_install_materialized()
                            && let Some(plugin_id) = plugin_id.as_ref()
                            && let Some(plugin_root) = self.store.active_plugin_root(plugin_id)
                            && let Some(manifest) = self
                                .store
                                .manifest_cache
                                .load(plugin_root.as_path())
                                .map(|loaded| loaded.manifest)
                        {
                            local_version = manifest.version.clone();
                            let marketplace_category = interface
                                .as_ref()
                                .and_then(|interface| interface.category.clone());
                            interface = plugin_interface_with_marketplace_category(
                                manifest.interface,
                                marketplace_category,
                            );
                        }

                        Some(ConfiguredMarketplacePlugin {
                            // Enabled state is keyed by `<plugin>@<marketplace>`, so duplicate
                            // plugin entries from duplicate marketplace files intentionally
                            // resolve to the first discovered source.
                            id: plugin_key,
                            installed_version,
                            installed,
                            enabled,
                            name: plugin.name,
                            local_version,
                            source: plugin.source,
                            policy: plugin.policy,
                            keywords: plugin.keywords,
                            interface,
                            manifest_fallback,
                        })
                    })
                    .collect::<Vec<_>>();

                (!plugins.is_empty()).then_some(ConfiguredMarketplace {
                    name: marketplace.name,
                    path: marketplace.path,
                    interface: marketplace.interface,
                    plugins,
                })
            })
            .collect();

        Ok(ConfiguredMarketplaceListOutcome {
            marketplaces,
            errors: marketplace_outcome.errors,
        })
    }

    pub fn discover_marketplaces_for_config(
        &self,
        config: &PluginsConfigInput,
        additional_roots: &[AbsolutePathBuf],
    ) -> Result<MarketplaceListOutcome, MarketplaceError> {
        if !config.plugins_enabled {
            return Ok(MarketplaceListOutcome::default());
        }

        let marketplace_roots = self.marketplace_roots(
            config,
            additional_roots,
            /*include_openai_curated*/ true,
        );
        self.list_marketplaces_with_policy(config, &marketplace_roots)
    }

    pub(crate) async fn tool_suggest_metadata_for_marketplace_plugin(
        &self,
        marketplace_name: &str,
        plugin: &ConfiguredMarketplacePlugin,
        skill_config_rules: &SkillConfigRules,
    ) -> Result<PluginCapabilitySummary, MarketplaceError> {
        let fragment = self
            .tool_suggest_metadata_cache
            .metadata_for_plugin(
                marketplace_name,
                plugin,
                self.restriction_product,
                self.skill_root_loader.as_ref(),
            )
            .await?;
        Ok(fragment.project(skill_config_rules, self.auth_mode()))
    }

    pub async fn read_plugin_for_config(
        &self,
        config: &PluginsConfigInput,
        request: &PluginReadRequest,
    ) -> Result<PluginReadOutcome, MarketplaceError> {
        if !config.plugins_enabled {
            return Err(MarketplaceError::PluginsDisabled);
        }

        let plugin = find_marketplace_plugin(&request.marketplace_path, &request.plugin_name)?;
        MarketplacePolicy::from_requirements(config.config_layer_stack.requirements())
            .validate_install(
                &config.config_layer_stack,
                self.codex_home.as_path(),
                &request.marketplace_path,
                &plugin.plugin_id.marketplace_name,
            )
            .map_err(|message| MarketplaceError::InvalidMarketplaceFile {
                path: request.marketplace_path.to_path_buf(),
                message,
            })?;
        if !self.restriction_product_matches(plugin.policy.products.as_deref()) {
            return Err(MarketplaceError::PluginNotFound {
                plugin_name: plugin.plugin_id.plugin_name,
                marketplace_name: plugin.plugin_id.marketplace_name,
            });
        }

        let marketplace_name = plugin.plugin_id.marketplace_name.clone();
        let plugin_key = plugin.plugin_id.as_key();
        let manifest_fallback = plugin
            .manifest_fallback
            .contents_if_has_metadata()
            .map(|_| plugin.manifest_fallback.clone());
        let plugin_states = self.configured_plugin_states(config);
        let installed = plugin_states.installed.contains(&plugin_key);
        let installed_version = if installed {
            self.store.active_plugin_version(&plugin.plugin_id)
        } else {
            None
        };
        let plugin = self
            .read_plugin_detail_for_marketplace_plugin(
                config,
                &marketplace_name,
                ConfiguredMarketplacePlugin {
                    id: plugin_key.clone(),
                    name: plugin.plugin_id.plugin_name,
                    local_version: plugin
                        .manifest
                        .as_ref()
                        .and_then(|manifest| manifest.version.clone()),
                    installed_version,
                    source: plugin.source,
                    policy: plugin.policy,
                    interface: plugin.interface,
                    keywords: plugin
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.keywords.clone())
                        .unwrap_or_default(),
                    manifest_fallback,
                    installed,
                    enabled: plugin_states.enabled.contains(&plugin_key),
                },
            )
            .await?;

        Ok(PluginReadOutcome {
            marketplace_name,
            marketplace_path: Some(request.marketplace_path.clone()),
            plugin,
        })
    }

    #[instrument(level = "trace", skip_all)]
    pub async fn read_plugin_detail_for_marketplace_plugin(
        &self,
        config: &PluginsConfigInput,
        marketplace_name: &str,
        plugin: ConfiguredMarketplacePlugin,
    ) -> Result<PluginDetail, MarketplaceError> {
        if !self.restriction_product_matches(plugin.policy.products.as_deref()) {
            return Err(MarketplaceError::PluginNotFound {
                plugin_name: plugin.name,
                marketplace_name: marketplace_name.to_string(),
            });
        }

        let plugin_id =
            PluginId::new(plugin.name.clone(), marketplace_name.to_string()).map_err(|err| {
                match err {
                    PluginIdError::Invalid(message) => MarketplaceError::InvalidPlugin(message),
                }
            })?;
        let plugin_key = plugin_id.as_key();
        if plugin.source.is_install_materialized() && !plugin.installed {
            let description = remote_plugin_install_required_description(&plugin.source);
            return Ok(PluginDetail {
                id: plugin_key,
                name: plugin.name,
                local_version: None,
                description: Some(description),
                source: plugin.source,
                policy: plugin.policy,
                interface: plugin.interface,
                keywords: plugin.keywords,
                installed: plugin.installed,
                enabled: plugin.enabled,
                skills: Vec::new(),
                disabled_skill_paths: HashSet::new(),
                onboarding_skill: None,
                hooks: Vec::new(),
                apps: Vec::new(),
                app_category_by_id: HashMap::new(),
                mcp_server_names: Vec::new(),
                details_unavailable_reason: Some(
                    PluginDetailsUnavailableReason::InstallRequiredForRemoteSource,
                ),
            });
        }

        let source_path = if plugin.source.is_install_materialized() && plugin.installed {
            self.store.active_plugin_root(&plugin_id).ok_or_else(|| {
                MarketplaceError::InvalidPlugin(format!(
                    "installed plugin cache entry is missing for {plugin_key}"
                ))
            })?
        } else {
            let codex_home = self.codex_home.clone();
            let source = plugin.source.clone();
            let materialized = tokio::task::spawn_blocking(move || {
                materialize_marketplace_plugin_source(codex_home.as_path(), &source)
            })
            .await
            .map_err(|err| {
                MarketplaceError::InvalidPlugin(format!(
                    "failed to materialize plugin source: {err}"
                ))
            })?
            .map_err(MarketplaceError::InvalidPlugin)?;
            materialized.path.clone()
        };
        if !source_path.as_path().is_dir() {
            return Err(MarketplaceError::InvalidPlugin(
                "path does not exist or is not a directory".to_string(),
            ));
        }
        let loaded_manifest =
            if codex_utils_plugins::find_plugin_manifest_path(source_path.as_path()).is_some() {
                self.store.manifest_cache.load(source_path.as_path())
            } else {
                plugin
                    .manifest_fallback
                    .as_ref()
                    .and_then(|fallback| fallback.parse_for_plugin_root(source_path.as_path()))
                    .map(|manifest| crate::manifest::LoadedPluginManifest {
                        manifest,
                        format: PluginManifestFormat::Legacy,
                    })
            }
            .ok_or_else(|| {
                MarketplaceError::InvalidPlugin("missing or invalid plugin.json".to_string())
            })?;
        let manifest_format = loaded_manifest.format;
        let manifest = loaded_manifest.manifest;
        let description = manifest.description.clone();
        let marketplace_category = plugin
            .interface
            .as_ref()
            .and_then(|interface| interface.category.clone());
        let interface = plugin_interface_with_marketplace_category(
            manifest.interface.clone(),
            marketplace_category,
        );
        let plugin_identity = PluginIdentity {
            plugin_id: plugin_id.as_key(),
            remote_plugin_id: self.remote_plugin_id_for(&plugin_id),
        };
        let skill_config_rules = skill_config_rules_from_stack(&config.config_layer_stack);
        let resolved_skills = load_plugin_skill_inventory(
            &source_path,
            &plugin_identity,
            &manifest,
            manifest_format,
            self.restriction_product,
            /*plugin_skill_snapshots*/ None,
            self.skill_root_loader.as_ref(),
        )
        .await
        .resolve(&skill_config_rules);
        let onboarding_skill = manifest.paths.onboarding_skill.as_ref().and_then(|path| {
            let plugin_root = source_path.canonicalize().ok()?;
            let path = path.canonicalize().ok()?;
            if !path.as_path().starts_with(plugin_root.as_path()) {
                return None;
            }
            let path_uri = PathUri::from_abs_path(&path);
            resolved_skills
                .skills
                .iter()
                .any(|skill| skill.path_to_skills_md == path_uri)
                .then_some(path)
        });
        let plugin_data_root = self.store.plugin_data_root(&plugin_id);
        let (hook_sources, _hook_load_warnings) = if manifest_format == PluginManifestFormat::Legacy
        {
            load_plugin_hooks(&source_path, &plugin_id, &plugin_data_root, &manifest.paths)
        } else {
            (Vec::new(), Vec::new())
        };
        let hooks = plugin_hook_declarations(&hook_sources)
            .into_iter()
            .map(|hook| PluginHookSummary {
                key: hook.key,
                event_name: hook.event_name,
            })
            .collect();
        let auth_mode = self.auth_mode();
        let mut app_declarations = if plugin_supports_apps(manifest_format, &plugin_id) {
            load_plugin_apps_from_manifest(source_path.as_path(), &manifest.paths).await
        } else {
            Vec::new()
        };
        let mcp_data_root = (manifest_format == PluginManifestFormat::AgentPlugin)
            .then(|| self.store.mcp_data_root(&plugin_id, manifest_format));
        let mut mcp_servers = load_plugin_mcp_servers_from_manifest_with_format(
            source_path.as_path(),
            &manifest.paths,
            /*plugin_policy*/ None,
            mcp_data_root.as_deref(),
            manifest_format,
        )
        .await;
        apply_app_mcp_routing_policy(
            &mut app_declarations,
            &mut mcp_servers,
            auth_mode,
            /*plugin_active*/ true,
        );
        let apps = app_connector_ids_from_declarations(&app_declarations);
        let mut seen_app_connector_ids = HashSet::new();
        let mut app_category_by_id = HashMap::new();
        for app in &app_declarations {
            if seen_app_connector_ids.insert(app.connector_id.0.as_str())
                && let Some(category) = &app.category
            {
                app_category_by_id.insert(app.connector_id.0.clone(), category.clone());
            }
        }
        let mut mcp_server_names = mcp_servers.into_keys().collect::<Vec<_>>();
        mcp_server_names.sort_unstable();
        mcp_server_names.dedup();

        Ok(PluginDetail {
            id: plugin.id,
            name: plugin.name,
            local_version: manifest.version.clone(),
            description,
            source: plugin.source,
            policy: plugin.policy,
            interface,
            keywords: manifest.keywords,
            installed: plugin.installed,
            enabled: plugin.enabled,
            skills: resolved_skills.skills,
            disabled_skill_paths: resolved_skills.disabled_skill_paths,
            onboarding_skill,
            hooks,
            apps,
            app_category_by_id,
            mcp_server_names,
            details_unavailable_reason: None,
        })
    }

    fn marketplace_roots(
        &self,
        config: &PluginsConfigInput,
        additional_roots: &[AbsolutePathBuf],
        include_openai_curated: bool,
    ) -> Vec<AbsolutePathBuf> {
        // Treat the curated catalog as an extra marketplace root so plugin listing can surface it
        // without requiring every caller to know where it is stored.
        let mut roots = additional_roots.to_vec();
        roots.extend(installed_marketplace_roots_from_layer_stack(
            &config.config_layer_stack,
            self.codex_home.as_path(),
        ));
        let curated_marketplace_path = if include_openai_curated {
            match target_curated_marketplace(self.auth_mode()) {
                TargetCuratedMarketplace::OpenAi | TargetCuratedMarketplace::OpenAiWithRemote => {
                    let curated_repo_root = curated_plugins_repo_path(self.codex_home.as_path());
                    curated_repo_root.is_dir().then_some(curated_repo_root)
                }
                TargetCuratedMarketplace::OpenAiApi => {
                    let api_marketplace_path =
                        curated_plugins_api_marketplace_path(self.codex_home.as_path());
                    api_marketplace_path
                        .is_file()
                        .then_some(api_marketplace_path)
                }
            }
        } else {
            None
        };
        if let Some(curated_marketplace_path) = curated_marketplace_path
            && let Ok(curated_marketplace_path) =
                AbsolutePathBuf::try_from(curated_marketplace_path)
        {
            roots.push(curated_marketplace_path);
        }
        roots.sort_unstable();
        roots.dedup();
        roots
    }

    fn list_marketplaces_with_policy(
        &self,
        config: &PluginsConfigInput,
        roots: &[AbsolutePathBuf],
    ) -> Result<MarketplaceListOutcome, MarketplaceError> {
        let mut outcome =
            list_marketplaces_with_cache(roots, home_dir().as_deref(), &self.store.manifest_cache)?;
        let policy = MarketplacePolicy::from_requirements(config.config_layer_stack.requirements());
        outcome.marketplaces.retain(|marketplace| {
            policy
                .validate_install(
                    &config.config_layer_stack,
                    self.codex_home.as_path(),
                    &marketplace.path,
                    &marketplace.name,
                )
                .is_ok()
        });
        Ok(outcome)
    }
}

pub(crate) fn remote_plugin_install_required_description(
    source: &MarketplacePluginSource,
) -> String {
    let source_description = match source {
        MarketplacePluginSource::Git {
            url,
            path,
            ref_name,
            sha,
        } => {
            let mut parts = vec![url.clone()];
            if let Some(path) = path {
                parts.push(format!("path `{path}`"));
            }
            if let Some(ref_name) = ref_name {
                parts.push(format!("ref `{ref_name}`"));
            }
            if let Some(sha) = sha {
                parts.push(format!("sha `{sha}`"));
            }
            parts.join(", ")
        }
        MarketplacePluginSource::Local { path } => path.as_path().display().to_string(),
        MarketplacePluginSource::Npm {
            package,
            version,
            registry,
        } => {
            let mut parts = vec![package.clone()];
            if let Some(version) = version {
                parts.push(format!("version `{version}`"));
            }
            if let Some(registry) = registry {
                parts.push(format!("registry `{registry}`"));
            }
            parts.join(", ")
        }
    };

    let source_kind = if matches!(source, MarketplacePluginSource::Npm { .. }) {
        "an npm plugin"
    } else {
        "a cross-repo plugin"
    };
    format!(
        "This is {source_kind}. Install it to view more detailed information. The source of the plugin is {source_description}."
    )
}
