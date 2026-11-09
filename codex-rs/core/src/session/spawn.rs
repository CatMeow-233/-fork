//! Session construction (`Session::spawn`) and the `SessionIo` submission handle.

use super::*;

impl Session {
    pub(crate) async fn resolve_startup_model(
        models_manager: &dyn ModelsManager,
        config: &Config,
        session_source: &SessionSource,
        allow_provider_model_fallback: bool,
    ) -> CodexResult<(String, CatalogModel)> {
        // Child catalogs are initialized by their selector. Reading only memory here
        // preserves the loaded snapshot shared with the parent.
        let refresh_strategy = if session_source.is_non_root_agent() {
            RefreshStrategy::InMemory
        } else {
            RefreshStrategy::OnlineIfUncached
        };
        if !session_source.is_non_root_agent() {
            let _ = models_manager
                .list_models(refresh_strategy, config.http_client_factory())
                .await;
        }
        // The refresh above already loaded (or failed to load) the catalog. Reading it
        // from memory avoids a second cache parse and, after a failed fetch, a second
        // network attempt with its own timeout before the session can start.
        let model = models_manager
            .get_default_model(
                &config.model,
                allow_provider_model_fallback,
                RefreshStrategy::InMemory,
                config.http_client_factory(),
            )
            .await;
        if model.trim().is_empty() {
            return Err(CodexErr::InvalidRequest(
                "No models are available. Set `model` explicitly or check your model catalog configuration.".to_string(),
            ));
        }
        let model_info = models_manager
            .get_model_info(model.as_str(), &config.to_models_manager_config())
            .await;
        Ok((model, model_info))
    }

    /// Spawn and initialize a new session.
    /// Hide the concrete startup future from callers while keeping initialization lazy.
    #[inline(never)]
    pub(crate) fn spawn(
        args: SessionSpawnArgs,
    ) -> BoxFuture<'static, CodexResult<(Arc<Self>, SessionIo)>> {
        let tree_shutdown = args.agent_control.runtime().shutdown.clone();
        Box::pin(async move {
            let parent_trace = match args.parent_trace {
                Some(trace) => {
                    if codex_otel::context_from_w3c_trace_context(&trace).is_some() {
                        Some(trace)
                    } else {
                        warn!("ignoring invalid thread spawn trace carrier");
                        None
                    }
                }
                None => None,
            };
            let thread_spawn_span = info_span!("thread_spawn", otel.name = "thread_spawn");
            if let Some(trace) = parent_trace.as_ref() {
                let _ = set_parent_from_w3c_trace_context(&thread_spawn_span, trace);
            }
            let spawn = Self::spawn_internal(SessionSpawnArgs {
                parent_trace,
                ..args
            })
            .instrument(thread_spawn_span);
            tokio::select! {
                biased;
                _ = tree_shutdown.cancelled() => Err(CodexErr::TurnAborted),
                result = spawn => result,
            }
        })
    }

    async fn spawn_internal(args: SessionSpawnArgs) -> CodexResult<(Arc<Self>, SessionIo)> {
        args.config
            .config_layer_stack
            .ensure_config_server_applied()?;
        args.config.validate_windows_mxc_requirement()?;
        let SessionSpawnArgs {
            attached_thread,
            startup,
            config,
            prompt_cache_key,
            allow_provider_model_fallback,
            instructions,
            installation_id,
            auth_manager,
            runtime_auth,
            models_manager,
            git_root_discovery,
            environment_manager,
            skills_service,
            plugins_manager,
            mcp_manager,
            code_mode_session_provider,
            extensions,
            conversation_history,
            resume_preparation,
            history_initialization,
            disabled_plugin_ids,
            requested_history_mode,
            fork_persistence,
            session_source,
            forked_from_thread_id,
            parent_thread_id,
            thread_source,
            originator,
            agent_control,
            dynamic_tools,
            metrics_service_name,
            user_shell_override,
            inherited_exec_policy,
            inherited_environments,
            parent_rollout_thread_trace,
            parent_trace: _,
            environment_requests,
            mut thread_extension_init,
            turn_extension_init,
            client_mcp_extensions,
            reserved_thread_id,
            analytics_events_client,
            image_store,
            thread_store,
            attestation_provider,
            external_time_provider,
            inherited_multi_agent_version,
            git_enrichment_policy,
            windows_sandbox_proxy_settings_mode,
        } = args;
        let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = async_channel::unbounded();

        let isolation = thread_extension_init
            .get::<codex_extension_api::SessionIsolation>()
            .map(|policy| *policy)
            .unwrap_or_default();
        // Manager-owned threads and inline delegates bind roots at the same startup boundary.
        let selected_capability_roots =
            match thread_extension_init.get::<Vec<SelectedCapabilityRoot>>() {
                Some(roots) => roots.as_ref().clone(),
                None => {
                    // Resume restores this thread's saved roots, including roots for environments
                    // it will select again later. New children inherit roots regardless of copied history.
                    let roots = if !matches!(&conversation_history, InitialHistory::Resumed(_))
                        && isolation == codex_extension_api::SessionIsolation::Inherit
                        && let Some(environments) = &inherited_environments
                    {
                        environments.selected_capability_roots()
                    } else {
                        conversation_history.get_selected_capability_roots()
                    };
                    if !roots.is_empty() {
                        thread_extension_init.insert(roots.clone());
                    }
                    roots
                }
            };
        let environment_selections = environment_requests
            .into_iter()
            .map(|request| TurnEnvironmentSelection::new(request, &selected_capability_roots))
            .collect::<Vec<_>>();
        // Enforce snapshot-only instructions for both managed and inline isolated sessions.
        let instructions = if isolation == codex_extension_api::SessionIsolation::Isolated {
            SessionInstructions {
                user: instructions.user,
                thread: instructions.thread,
                project_snapshot: instructions.project_snapshot,
                ..Default::default()
            }
        } else {
            instructions
        };
        let exec_policy = if isolation == codex_extension_api::SessionIsolation::Isolated {
            let source_requirements = config
                .config_layer_stack
                .requirements()
                .exec_policy
                .as_deref()
                .cloned()
                .map(Arc::new);
            let managed_policy = source_requirements
                .as_deref()
                .map_or_else(codex_execpolicy::Policy::empty, |policy| {
                    policy.as_ref().clone()
                });
            Arc::new(ExecPolicyManager::from_snapshot(
                Arc::new(managed_policy),
                source_requirements,
            ))
        } else if let Some(exec_policy) = &inherited_exec_policy {
            Arc::clone(exec_policy)
        } else {
            if !config
                .config_layer_stack
                .ignore_user_and_project_exec_policy_rules()
            {
                let codex_home = config.codex_home.clone();
                let policy_path = default_policy_path(codex_home.as_path());
                if let Err(err) = prefix_rule_migration(
                    codex_home.as_path(),
                    policy_path.as_path(),
                    BANNED_PREFIX_SUGGESTIONS,
                )
                .await
                {
                    tracing::warn!(error = %err, "failed to run prefix rule migration");
                }
            }
            Arc::new(
                ExecPolicyManager::load(&config.config_layer_stack)
                    .await
                    .map_err(|err| CodexErr::Fatal(format!("failed to load rules: {err}")))?,
            )
        };

        let mut config = Arc::new(config);
        let configured_runtime = crate::runtime_auth::runtime_for_config(
            &runtime_auth,
            config.as_ref(),
            Arc::clone(&auth_manager),
            &installation_id,
        );
        let (model, model_info) = Self::resolve_startup_model(
            models_manager.as_ref(),
            &config,
            &session_source,
            allow_provider_model_fallback,
        )
        .await?;
        let requested_approvals_reviewer = config
            .requested_approvals_reviewer
            .unwrap_or(config.approvals_reviewer);
        let trusted_guardian_reviewer = crate::guardian::is_basic_session_source(&session_source)
            && !matches!(conversation_history, InitialHistory::Resumed(_));
        if config
            .config_layer_stack
            .requirements()
            .auto_review_required_for_model(&model)
            && !trusted_guardian_reviewer
        {
            let permission_profile = config.permissions.effective_permission_profile();
            if permission_profile.enforcement() != SandboxEnforcement::External
                && permission_profile.network_sandbox_policy().is_enabled()
                && {
                    let file_system = permission_profile.file_system_sandbox_policy();
                    file_system.has_full_disk_write_access()
                        // Preserve the legacy fallback for writable URIs that cannot be projected locally.
                        && file_system.entries.iter().all(|entry| entry.legacy_writable_path().is_ok())
                }
            {
                let config = Arc::make_mut(&mut config);
                let permission_profile = PermissionProfile::workspace_write();
                config
                    .permissions
                    .set_permission_profile(permission_profile.clone())
                    .map_err(|err| CodexErr::InvalidRequest(err.to_string()))?;
                if let Some(network) = config.permissions.network.as_ref() {
                    config.permissions.network = Some(
                        network
                            .recompute_for_permission_profile(&permission_profile)
                            .map_err(|err| CodexErr::InvalidRequest(err.to_string()))?,
                    );
                }
            }
        }
        let reviewer = crate::config::resolve_guardian_reviewer(
            requested_approvals_reviewer,
            (!trusted_guardian_reviewer).then_some(model.as_str()),
            config.config_layer_stack.requirements(),
        )
        .map_err(|err| CodexErr::InvalidRequest(err.to_string()))?;
        {
            let config = Arc::make_mut(&mut config);
            config.requested_approvals_reviewer = Some(requested_approvals_reviewer);
            config.approvals_reviewer = reviewer;
        }
        if allow_provider_model_fallback
            && let Some(requested_model) = config.model.as_ref()
            && model != *requested_model
        {
            info!(
                model_provider = %config.model_provider_id,
                requested_model,
                fallback_model = %model,
                "replaced unavailable requested model with provider default"
            );
        }

        let auth = auth_manager.auth_cached();
        // Forked subagents keep their parent's activation with the copied history.
        // Fresh children restore configured preferences before applying startup defaults.
        let inherits_token_budget = matches!(&conversation_history, InitialHistory::Forked(_))
            && config.token_budget_startup_config.is_some();
        if !inherits_token_budget {
            Arc::make_mut(&mut config)
                .prepare_token_budget_for_startup()
                .map_err(|err| CodexErr::InvalidRequest(err.to_string()))?;
            // Resolve activation for this runtime, including when resuming saved history.
            token_budget::apply_experimental_context(
                Arc::make_mut(&mut config),
                auth.as_ref(),
                &model_info,
            )?;
            token_budget::apply_model_defaults(Arc::make_mut(&mut config), &model_info);
        }
        let configured_config = Arc::clone(&config);
        let multi_agent_version = config.multi_agent_version_override().or_else(|| {
            resolve_multi_agent_version(&conversation_history, inherited_multi_agent_version)
        });
        let history_mode = conversation_history.get_history_mode(
            requested_history_mode.unwrap_or_else(|| thread_store.default_history_mode()),
        );
        let base_instructions = config
            .base_instructions
            .clone()
            .or_else(|| conversation_history.get_base_instructions().map(|s| s.text))
            .unwrap_or_else(|| render_model_instructions(&model_info));

        // A fork records its initial catalog in the new thread's creation metadata.
        let dynamic_tools = if dynamic_tools.is_empty()
            && matches!(conversation_history, InitialHistory::Forked(_))
        {
            conversation_history.get_dynamic_tools().unwrap_or_default()
        } else {
            dynamic_tools
        };
        let disabled_plugin_ids = disabled_plugin_ids.unwrap_or_else(|| {
            let settings_owner = match &conversation_history {
                InitialHistory::Resumed(resumed) => Some(resumed.conversation_id),
                InitialHistory::Forked(_) => forked_from_thread_id,
                InitialHistory::New | InitialHistory::Cleared => None,
            };
            settings_owner
                .and_then(|thread_id| {
                    codex_history::latest_disabled_plugin_ids(
                        conversation_history.get_rollout_items(),
                        thread_id,
                    )
                })
                .map(<[String]>::to_vec)
                .unwrap_or_default()
        });
        // TODO (aibrahim): Consolidate config.model and config.model_reasoning_effort into config.collaboration_mode
        // to avoid extracting these fields separately and constructing CollaborationMode here.
        let persisted_collaboration_mode = match &conversation_history {
            InitialHistory::Resumed(resumed) => resumed
                .history
                .iter()
                .rev()
                .find_map(|item| match item {
                    RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
                        if event.thread_id == Some(resumed.conversation_id) =>
                    {
                        Some(&event.thread_settings.collaboration_mode)
                    }
                    _ => None,
                })
                .or_else(|| {
                    resumed
                        .history
                        .iter()
                        .rev()
                        .find_map(|item| match item {
                            RolloutItem::TurnContext(context) => {
                                Some(context.collaboration_mode.as_ref())
                            }
                            _ => None,
                        })
                        .flatten()
                }),
            InitialHistory::New | InitialHistory::Cleared | InitialHistory::Forked(_) => None,
        };
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: model.clone(),
                reasoning_effort: config.model_reasoning_effort.clone(),
                developer_instructions: None,
            },
        };
        let collaboration_mode = persisted_collaboration_mode.map_or(collaboration_mode, |saved| {
            saved.with_updates(
                Some(model.clone()),
                Some(config.model_reasoning_effort.clone()),
                /*developer_instructions*/ None,
            )
        });
        let initial_service_tier_warning = unsupported_service_tier_warning(
            config.service_tier.as_deref(),
            &config.features,
            &model_info,
        );
        let storage_originator = AuthStorageOriginator::from_client_name(&originator);
        let root_capabilities_restricted = session_source.is_non_root_agent()
            || thread_extension_init
                .get::<RestrictRootCapabilities>()
                .is_some();
        let enable_full_access_form_input = !root_capabilities_restricted
            && matches!(thread_source.as_ref(), Some(ThreadSource::User))
            && client_mcp_extensions.contains(OPENAI_STANDARD_FORM_INPUT_EXTENSION_ID);
        let provider_auth = codex_model_provider::ProviderAuthContext {
            credentials: Arc::clone(&auth_manager),
            runtime_auth: configured_runtime.map(|runtime| (runtime, config.runtime_auth_mode)),
        };
        // Replace any inherited context before startup hooks see this thread's effective auth.
        thread_extension_init.insert(provider_auth.clone());
        let session_configuration = SessionConfiguration {
            turn_extension_init,
            provider: provider_auth.model_provider(config.model_provider.clone()),
            step_settings: Arc::new(StepSettings {
                collaboration_mode,
                reasoning_summary: config.model_reasoning_summary,
                service_tier: config.service_tier.clone(),
                personality: config.personality,
                approval_policy: config.permissions.approval_policy.clone(),
                approvals_reviewer: config.approvals_reviewer,
            }),
            environments: environment_selections.clone(),
            model_info_overrides: config.to_models_manager_config().into(),
            developer_instructions: config.developer_instructions.clone(),
            base_instructions,
            permission_profile_state: session_permission_profile_state_from_config(&config)?,
            allow_login_shell: config.permissions.allow_login_shell,
            shell_environment_policy: config.permissions.shell_environment_policy.clone(),
            windows_sandbox_level: WindowsSandboxLevel::from_config(&config),
            windows_sandbox_type: config.effective_local_windows_sandbox_type(),
            use_legacy_landlock: config.features.use_legacy_landlock(),
            legacy_fallback_cwd: config.cwd.clone(),
            runtime_workspace_roots: config.workspace_roots.clone(),
            codex_home: config.codex_home.clone(),
            thread_name: None,
            disabled_plugin_ids,
            original_config_do_not_use: Arc::clone(&config),
            metrics_service_name,
            app_server_client_name: None,
            app_server_client_version: None,
            trusted_guardian_reviewer,
            session_source,
            root_capabilities_restricted,
            history_mode,
            forked_from_thread_id,
            parent_thread_id,
            thread_source,
            originator,
            dynamic_tools: Arc::from(dynamic_tools),
            user_shell_override,
        };
        session_configuration
            .validate(&environment_selections)
            .map_err(|err| CodexErr::InvalidRequest(err.to_string()))?;
        crate::windows_sandbox::log_windows_sandbox_startup(&config);

        // Generate a unique ID for the lifetime of this session.
        let session_source_clone = session_configuration.session_source.clone();
        let (agent_status_tx, agent_status_rx) = watch::channel(AgentStatus::PendingInit);

        let session = Box::pin(Session::new(
            startup.clone(),
            session_configuration,
            attached_thread,
            prompt_cache_key,
            &environment_selections,
            config.clone(),
            instructions,
            installation_id,
            auth_manager.clone(),
            runtime_auth,
            models_manager.clone(),
            git_root_discovery,
            model_info,
            exec_policy,
            tx_event.clone(),
            agent_status_tx.clone(),
            conversation_history,
            history_initialization,
            fork_persistence,
            session_source_clone,
            skills_service,
            plugins_manager,
            mcp_manager.clone(),
            code_mode_session_provider,
            extensions,
            thread_extension_init,
            client_mcp_extensions,
            agent_control,
            reserved_thread_id,
            environment_manager,
            inherited_environments,
            analytics_events_client,
            image_store,
            thread_store,
            resume_preparation,
            parent_rollout_thread_trace,
            attestation_provider,
            external_time_provider,
            multi_agent_version,
            git_enrichment_policy,
            windows_sandbox_proxy_settings_mode,
        ))
        .await
        .map_err(|e| {
            error!("Failed to create session: {e:#}");
            match e.downcast::<CodexErr>() {
                Ok(error) => error,
                Err(error) => map_session_init_error(&error, &config.codex_home),
            }
        })?;
        if let Some(message) = initial_service_tier_warning {
            session
                .send_event_raw(Event {
                    id: INITIAL_SUBMIT_ID.to_owned(),
                    msg: EventMsg::Warning(WarningEvent { message }),
                })
                .await;
        }
        let thread_id = session.thread_id;

        // This task will run until Op::Shutdown is received.
        let tree_teardown = startup
            .as_ref()
            .and_then(|startup| startup.session_teardown(thread_id));
        let session_for_loop = Arc::clone(&session);
        let session_loop_handle = tokio::spawn(Box::pin(storage_originator.scope(async move {
            submission_loop(session_for_loop, configured_config, rx_sub)
                .instrument(info_span!("session_loop", thread_id = %thread_id))
                .await;
            if let Some(tree_teardown) = tree_teardown {
                tree_teardown.complete();
            }
        })));
        let io = SessionIo {
            tx_sub,
            rx_event,
            agent_status: agent_status_rx,
            session_loop_termination: session_loop_termination_from_handle(session_loop_handle),
        };

        if let Some(startup) = startup {
            let _ = startup.io.set(io.clone());
            startup.persistence.lock().await.commit();
        }
        // A required MCP server must not block startup waiting for form input.
        if enable_full_access_form_input {
            session.services.mcp_runtime.enable_full_access_form_input();
        }
        Ok((session, io))
    }
}

impl SessionIo {
    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub(crate) async fn submit(
        &self,
        op: impl Into<WithTurnExtensionData<Op>>,
    ) -> CodexResult<String> {
        self.submit_with_trace(
            op, /*trace*/ None, /*parent_turn_id*/ None, /*root_turn_id*/ None,
            /*residency_guard*/ None, /*message_delivery*/ None,
        )
        .await
    }

    pub(crate) async fn submit_with_trace(
        &self,
        op: impl Into<WithTurnExtensionData<Op>>,
        trace: Option<W3cTraceContext>,
        parent_turn_id: Option<String>,
        root_turn_id: Option<String>,
        residency_guard: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
        message_delivery: Option<crate::agent_communication::MessageDelivery>,
    ) -> CodexResult<String> {
        let id = new_submission_id();
        let WithTurnExtensionData {
            request: op,
            turn_extension_init,
        } = op.into();
        let sub = Submission {
            id: id.clone(),
            op,
            turn_extension_init,
            trace,
            parent_turn_id,
            root_turn_id,
            residency_guard,
            message_delivery,
        };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    /// Use sparingly: prefer `submit()` so submission IDs are generated consistently.
    pub(crate) async fn submit_with_id(&self, mut sub: Submission) -> CodexResult<()> {
        if sub.trace.is_none() {
            sub.trace = current_span_w3c_trace_context();
        }
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(())
    }

    /// Submits an ordered turn-input call and waits only for Core's routing decision.
    ///
    /// Once queued, dropping the waiter does not retract the call. If the
    /// session loop exits before replying, the caller gets `InternalAgentDied`.
    pub(crate) async fn submit_turn_input(
        &self,
        request: impl Into<WithTurnExtensionData<TurnInputRequest>>,
        mode: TurnInputMode,
    ) -> CodexResult<TurnInputSubmission> {
        let id = new_submission_id();
        let (reply_tx, reply_rx) = oneshot::channel();
        let WithTurnExtensionData {
            mut request,
            turn_extension_init,
        } = request.into();
        let trace = request.trace.take();
        self.submit_with_id(Submission {
            id,
            op: Op::TurnInput {
                request: Box::new(request),
                mode,
                reply: reply_tx,
            },
            trace,
            turn_extension_init,
            parent_turn_id: None,
            root_turn_id: None,
            residency_guard: None,
            message_delivery: None,
        })
        .await?;
        reply_rx.await.unwrap_or(Err(CodexErr::InternalAgentDied))
    }

    pub(crate) async fn submit_recover_turn(
        &self,
        thread_settings: ThreadSettingsOverrides,
        start_options: TurnStartOptions,
        trace: Option<W3cTraceContext>,
        turn_id: String,
        turn_extension_init: Option<ExtensionDataInit>,
    ) -> CodexResult<TurnInputSubmission> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.submit_with_id(Submission {
            id: turn_id,
            op: Op::RecoverTurn {
                thread_settings,
                start_options,
                reply: reply_tx,
            },
            trace,
            turn_extension_init,
            parent_turn_id: None,
            root_turn_id: None,
            residency_guard: None,
            message_delivery: None,
        })
        .await?;
        reply_rx.await.unwrap_or(Err(CodexErr::InternalAgentDied))
    }

    pub(crate) async fn shutdown_and_wait(&self) -> CodexResult<()> {
        let session_loop_termination = self.session_loop_termination.clone();
        match self.submit(Op::Shutdown).await {
            Ok(_) => {}
            Err(err) if matches!(err.details(), CodexErrorDetails::InternalAgentDied) => {}
            Err(err) => return Err(err),
        }
        session_loop_termination.await;
        Ok(())
    }

    pub(crate) async fn next_event(&self) -> CodexResult<Event> {
        let event = self
            .rx_event
            .recv()
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(event)
    }

    pub(crate) async fn agent_status(&self) -> AgentStatus {
        self.agent_status.borrow().clone()
    }
}

/// Generate a core submission ID. App-server exposes submission IDs that
/// create turns as a public-facing turn ID.
///
/// Some use cases take advantage of the fact that these are UUID7 which
/// encodes a timestamp, so think carefully before changing this.
pub(crate) fn new_submission_id() -> String {
    Uuid::now_v7().to_string()
}

pub(crate) fn get_service_tier(
    configured_service_tier: Option<String>,
    features: &Features,
    model_info: &ModelInfo,
) -> Option<String> {
    let service_tier = configured_service_tier?;
    if service_tier == ServiceTier::Flex.request_value() {
        return Some(service_tier);
    }
    if features.service_tier_enabled(&service_tier)
        && (service_tier == SERVICE_TIER_DEFAULT_REQUEST_VALUE
            || model_info.supports_service_tier(&service_tier))
    {
        return Some(service_tier);
    }
    None
}

fn unsupported_service_tier_warning(
    configured_service_tier: Option<&str>,
    features: &Features,
    model_info: &ModelInfo,
) -> Option<String> {
    let service_tier = configured_service_tier.filter(|service_tier| {
        features.service_tier_enabled(service_tier)
            && *service_tier != SERVICE_TIER_DEFAULT_REQUEST_VALUE
            && !model_info.supports_service_tier(service_tier)
    })?;
    Some(format!(
        "Configured service tier `{service_tier}` is not advertised as supported for model `{}` and will be omitted from requests.",
        model_info.slug
    ))
}

fn session_permission_profile_state_from_config(
    config: &Config,
) -> CodexResult<PermissionProfileState> {
    Ok(config.permissions.permission_profile_state().clone())
}

#[cfg(test)]
pub(crate) fn completed_session_loop_termination() -> SessionLoopTermination {
    futures::future::ready(()).boxed().shared()
}

pub(crate) fn session_loop_termination_from_handle(
    handle: JoinHandle<()>,
) -> SessionLoopTermination {
    async move {
        let _ = handle.await;
    }
    .boxed()
    .shared()
}
