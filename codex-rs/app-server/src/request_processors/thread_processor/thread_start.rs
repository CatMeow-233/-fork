//! Creates new threads for `thread/start` and builds their config overrides.

use super::*;

impl ThreadRequestProcessor {
    pub(super) async fn thread_start_inner(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadStartParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
        request_context: RequestContext,
    ) -> Result<(), JSONRPCErrorError> {
        let ThreadStartParams {
            model,
            model_provider,
            allow_provider_model_fallback,
            service_tier,
            cwd,
            runtime_workspace_roots,
            approval_policy,
            approvals_reviewer,
            sandbox,
            permissions,
            config,
            service_name,
            base_instructions,
            developer_instructions,
            dynamic_tools,
            selected_capability_roots,
            mock_experimental_field: _mock_experimental_field,
            experimental_raw_events,
            personality,
            multi_agent_mode: _multi_agent_mode,
            ephemeral,
            history_mode,
            session_start_source,
            thread_source,
            project_id,
            daybreak_enabled,
            environments,
        } = params;
        if matches!(
            history_mode,
            Some(codex_app_server_protocol::ThreadHistoryMode::Paginated)
        ) && !self.thread_store.supports_paginated_history_lists()
        {
            return Err(invalid_request(
                "paginated threads require thread/turns/list and thread/items/list support",
            ));
        }
        if sandbox.is_some() && permissions.is_some() {
            return Err(invalid_request(
                "`permissions` cannot be combined with `sandbox`",
            ));
        }
        if let Some(project_id) = project_id.as_ref() {
            if project_id.is_empty() {
                return Err(invalid_request("projectId must not be empty"));
            }
            let project = self
                .thread_store
                .read_project(project_id.clone())
                .await
                .map_err(|err| match err {
                    ThreadStoreError::Unsupported { operation } => {
                        unsupported_thread_store_operation(operation)
                    }
                    err => internal_error(format!("failed to read project: {err}")),
                })?;
            if project.is_none() {
                return Err(invalid_request(format!("project not found: {project_id}")));
            }
        }
        let runtime_workspace_roots = runtime_workspace_roots.map(resolve_runtime_workspace_roots);
        let environment_requests =
            resolve_turn_environment_requests(self.thread_manager.as_ref(), environments)?;
        let mut typesafe_overrides = self.build_thread_config_overrides(
            model,
            model_provider,
            service_tier,
            cwd,
            runtime_workspace_roots,
            approval_policy,
            approvals_reviewer,
            sandbox,
            permissions,
            base_instructions,
            developer_instructions,
            personality,
        );
        typesafe_overrides.ephemeral = ephemeral;
        let listener_task_context = ListenerTaskContext {
            thread_manager: Arc::clone(&self.thread_manager),
            thread_state_manager: self.thread_state_manager.clone(),
            outgoing: Arc::clone(&self.outgoing),
            pending_thread_unloads: Arc::clone(&self.pending_thread_unloads),
            thread_watch_manager: self.thread_watch_manager.clone(),
            codex_home: self.config.codex_home.to_path_buf(),
            skills_watcher: Arc::clone(&self.skills_watcher),
            turn_cost_worker: self.turn_cost_worker.clone(),
        };
        let request_trace = request_context.request_trace();
        let config_manager = self.config_manager.clone();
        let thread_store = Arc::clone(&self.thread_store);
        let initial_config_warnings = Arc::clone(&self.initial_config_warnings);
        let outgoing = Arc::clone(&listener_task_context.outgoing);
        let error_request_id = request_id.clone();
        let thread_start_task = async move {
            if let Err(error) = Self::thread_start_task(
                listener_task_context,
                thread_store,
                config_manager,
                request_id,
                app_server_client_name,
                app_server_client_version,
                client_mcp_extensions,
                config,
                typesafe_overrides,
                dynamic_tools,
                selected_capability_roots.unwrap_or_default(),
                history_mode.map(Into::into),
                session_start_source,
                thread_source.map(Into::into),
                project_id,
                daybreak_enabled,
                environment_requests,
                service_name,
                allow_provider_model_fallback,
                experimental_raw_events,
                request_trace,
                initial_config_warnings,
            )
            .await
            {
                outgoing.send_error(error_request_id, error).await;
            }
        };
        self.background_tasks
            .spawn(thread_start_task.instrument(request_context.span()))
            .await
            .map_err(|_| internal_error("thread startup task stopped before completing"))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn thread_start_task(
        listener_task_context: ListenerTaskContext,
        thread_store: Arc<dyn ThreadStore>,
        config_manager: ConfigManager,
        request_id: ConnectionRequestId,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
        config_overrides: Option<HashMap<String, serde_json::Value>>,
        typesafe_overrides: ConfigOverrides,
        dynamic_tools: Option<Vec<DynamicToolSpec>>,
        selected_capability_roots: Vec<SelectedCapabilityRoot>,
        history_mode: Option<ThreadHistoryMode>,
        session_start_source: Option<codex_app_server_protocol::ThreadStartSource>,
        thread_source: Option<codex_protocol::protocol::ThreadSource>,
        project_id: Option<String>,
        daybreak_enabled: Option<bool>,
        environment_requests: Option<Vec<TurnEnvironmentRequest>>,
        service_name: Option<String>,
        allow_provider_model_fallback: bool,
        experimental_raw_events: bool,
        request_trace: Option<W3cTraceContext>,
        initial_config_warnings: Arc<Vec<ConfigWarningNotification>>,
    ) -> Result<(), JSONRPCErrorError> {
        let thread_start_started_at = std::time::Instant::now();
        let requested_cwd = typesafe_overrides.cwd.clone();
        let mut config = config_manager
            .load_with_overrides(config_overrides.clone(), typesafe_overrides.clone())
            .await
            .map_err(|err| config_load_error(&err))?;
        if config.ephemeral && daybreak_enabled.is_some() {
            return Err(invalid_request(
                "daybreakEnabled is not supported for ephemeral threads",
            ));
        }
        // Project-local config can launch host processes, so only the effective
        // permissions after managed constraints can imply project trust.
        let effective_permission_profile = config.permissions.effective_permission_profile();
        let effective_permissions_trust_project = match &effective_permission_profile {
            codex_protocol::models::PermissionProfile::Disabled
            | codex_protocol::models::PermissionProfile::External { .. } => true,
            codex_protocol::models::PermissionProfile::Managed { .. } => {
                effective_permission_profile
                    .file_system_sandbox_policy()
                    .can_write_local_path_with_cwd(config.cwd.as_path(), config.cwd.as_path())
            }
        };

        if requested_cwd.is_some()
            && config.active_project.trust_level.is_none()
            && !config.config_layer_stack.is_projectless()
            && effective_permissions_trust_project
        {
            let trust_target = resolve_root_git_project_for_trust(LOCAL_FS.as_ref(), &config.cwd)
                .await
                .unwrap_or_else(|| config.cwd.clone());
            let current_cli_overrides = config_manager.current_cli_overrides();
            let cli_overrides_with_trust;
            let cli_overrides_for_reload = if let Err(err) =
                codex_core::config::set_project_trust_level(
                    &listener_task_context.codex_home,
                    trust_target.as_path(),
                    TrustLevel::Trusted,
                ) {
                warn!(
                    "failed to persist trusted project state for {}; continuing with in-memory trust for this thread: {err}",
                    trust_target.display()
                );
                let mut project = toml::map::Map::new();
                project.insert(
                    "trust_level".to_string(),
                    TomlValue::String("trusted".to_string()),
                );
                let mut projects = toml::map::Map::new();
                projects.insert(
                    project_trust_key(trust_target.as_path()),
                    TomlValue::Table(project),
                );
                cli_overrides_with_trust = current_cli_overrides
                    .iter()
                    .cloned()
                    .chain(std::iter::once((
                        "projects".to_string(),
                        TomlValue::Table(projects),
                    )))
                    .collect::<Vec<_>>();
                cli_overrides_with_trust.as_slice()
            } else {
                current_cli_overrides.as_slice()
            };

            config = config_manager
                .load_with_cli_overrides(
                    cli_overrides_for_reload,
                    config_overrides,
                    typesafe_overrides,
                    /*fallback_cwd*/ None,
                )
                .await
                .map_err(|err| config_load_error(&err))?;
        }

        // Thread config can include project-local warnings absent at initialization.
        let mut config_warnings = config
            .startup_warnings
            .iter()
            .map(|summary| ConfigWarningNotification {
                summary: summary.clone(),
                details: None,
                path: None,
                range: None,
            })
            .collect::<Vec<_>>();
        if let Ok(Some(err)) =
            codex_core::check_execpolicy_for_warnings(&config.config_layer_stack).await
        {
            config_warnings.push(crate::exec_policy_config_warning(&err));
        }
        for notification in config_warnings {
            if !initial_config_warnings.contains(&notification) {
                listener_task_context
                    .outgoing
                    .send_server_notification_to_connections(
                        &[request_id.connection_id],
                        ServerNotification::ConfigWarning(notification),
                    )
                    .await;
            }
        }

        let environment_requests = environment_requests.unwrap_or_else(|| {
            listener_task_context
                .thread_manager
                .default_environment_requests(&config.cwd, &config.workspace_roots)
        });
        let dynamic_tools = dynamic_tools.unwrap_or_default();
        if !dynamic_tools.is_empty() {
            validate_dynamic_tools(&dynamic_tools).map_err(invalid_request)?;
        }
        // Count callable functions rather than top-level namespace containers.
        let dynamic_tool_count: usize = dynamic_tools
            .iter()
            .map(|tool| match tool {
                DynamicToolSpec::Function(_) => 1,
                DynamicToolSpec::Namespace(namespace) => namespace.tools.len(),
            })
            .sum();
        let history_mode = history_mode.or_else(|| {
            (!config.ephemeral && thread_store.supports_paginated_history_lists())
                .then_some(ThreadHistoryMode::Paginated)
        });
        let mut thread_extension_init = ExtensionDataInit::new();
        if !selected_capability_roots.is_empty() {
            thread_extension_init.insert(selected_capability_roots);
        }
        let mut start_options = StartThreadOptions::new(config);
        let reserved_thread_id = if start_options.config.ephemeral {
            None
        } else {
            stage_pending_thread_metadata(
                listener_task_context.thread_manager.as_ref(),
                thread_store.as_ref(),
                StoreThreadMetadataPatch {
                    project_id: project_id.clone().map(Some),
                    daybreak_enabled,
                    ..Default::default()
                },
                "thread/start",
            )
            .await?
        };
        let thread_id = reserved_thread_id
            .unwrap_or_else(|| listener_task_context.thread_manager.reserve_thread_id());
        start_options.reserved_thread_id = Some(thread_id);
        // Startup can prewarm the Responses socket before start_thread returns.
        // Register the creating client first so that handshake can request attestation.
        listener_task_context
            .thread_state_manager
            .try_add_connection_to_thread(thread_id, request_id.connection_id)
            .await;
        let create_thread_started_at = std::time::Instant::now();
        let new_thread = listener_task_context
            .thread_manager
            .start_thread(StartThreadOptions {
                allow_provider_model_fallback,
                initial_history: match session_start_source
                    .unwrap_or(codex_app_server_protocol::ThreadStartSource::Startup)
                {
                    codex_app_server_protocol::ThreadStartSource::Startup => InitialHistory::New,
                    codex_app_server_protocol::ThreadStartSource::Clear => InitialHistory::Cleared,
                },
                history_mode,
                thread_source,
                dynamic_tools,
                metrics_service_name: service_name,
                parent_trace: request_trace,
                environments: Some(environment_requests),
                thread_extension_init,
                client_mcp_extensions,
                ..start_options
            })
            .instrument(tracing::info_span!(
                "app_server.thread_start.create_thread",
                otel.name = "app_server.thread_start.create_thread",
                thread_start.dynamic_tool_count = dynamic_tool_count,
            ))
            .await;
        let NewThread {
            thread_id,
            thread,
            session_configured,
            ..
        } = match new_thread {
            Ok(new_thread) => new_thread,
            Err(err) => {
                listener_task_context
                    .thread_state_manager
                    .remove_thread_state(thread_id)
                    .await;
                remove_pending_thread_metadata(thread_store.as_ref(), reserved_thread_id).await;
                return Err(match err.details() {
                    CodexErrorDetails::InvalidRequest(message) => invalid_request(message.clone()),
                    CodexErrorDetails::UnsupportedOperation(message) => {
                        method_not_found(message.clone())
                    }
                    _ => internal_error(format!("error creating thread: {err}")),
                });
            }
        };
        let session_telemetry = thread.session_telemetry();
        session_telemetry.record_startup_phase(
            "thread_start_create_thread",
            create_thread_started_at.elapsed(),
            Some("ready"),
        );

        Self::set_app_server_client_info(
            thread.as_ref(),
            app_server_client_name,
            app_server_client_version,
        )
        .await?;

        let instruction_sources = thread.legacy_instruction_sources().await;
        let config_snapshot = thread
            .config_snapshot()
            .instrument(tracing::info_span!(
                "app_server.thread_start.config_snapshot",
                otel.name = "app_server.thread_start.config_snapshot",
            ))
            .await;
        let mut thread = build_thread_from_snapshot(
            thread_id,
            session_configured.session_id.to_string(),
            thread.multi_agent_version(),
            &config_snapshot,
            session_configured.rollout_path.clone(),
        );
        thread.project_id = project_id.clone();
        thread.daybreak_enabled = daybreak_enabled;

        // Auto-attach a thread listener when starting a thread.
        log_listener_attach_result(
            super::thread_lifecycle::ensure_conversation_listener(
                listener_task_context.clone(),
                thread_id,
                request_id.connection_id,
                experimental_raw_events,
            )
            .instrument(tracing::info_span!(
                "app_server.thread_start.attach_listener",
                otel.name = "app_server.thread_start.attach_listener",
                thread_start.experimental_raw_events = experimental_raw_events,
            ))
            .await,
            thread_id,
            request_id.connection_id,
            "thread",
        );

        listener_task_context
            .thread_watch_manager
            .upsert_thread_silently(&thread.id)
            .instrument(tracing::info_span!(
                "app_server.thread_start.upsert_thread",
                otel.name = "app_server.thread_start.upsert_thread",
            ))
            .await;

        thread.status = resolve_thread_status(
            listener_task_context
                .thread_watch_manager
                .loaded_status_for_thread(&thread.id)
                .instrument(tracing::info_span!(
                    "app_server.thread_start.resolve_status",
                    otel.name = "app_server.thread_start.resolve_status",
                ))
                .await,
            /*has_in_progress_turn*/ false,
        );

        let sandbox = config_snapshot.sandbox_policy().into();
        let cwd = config_snapshot.cwd().clone();
        let active_permission_profile =
            thread_response_active_permission_profile(config_snapshot.active_permission_profile);
        let thread_originator = config_snapshot.originator.clone();

        let response = ThreadStartResponse {
            thread: thread.clone(),
            disabled_plugin_ids: config_snapshot.disabled_plugin_ids,
            model: config_snapshot.model,
            model_provider: config_snapshot.model_provider_id,
            service_tier: config_snapshot.service_tier,
            cwd,
            runtime_workspace_roots: config_snapshot.workspace_roots,
            instruction_sources,
            approval_policy: config_snapshot.approval_policy.into(),
            approvals_reviewer: config_snapshot.approvals_reviewer.into(),
            sandbox,
            active_permission_profile,
            reasoning_effort: config_snapshot.reasoning_effort,
            multi_agent_mode: MultiAgentMode::ExplicitRequestOnly,
        };
        let notif = thread_started_notification(thread);
        listener_task_context
            .outgoing
            .send_response_with_thread_originator(request_id, response, thread_originator)
            .instrument(tracing::info_span!(
                "app_server.thread_start.send_response",
                otel.name = "app_server.thread_start.send_response",
            ))
            .await;

        listener_task_context
            .outgoing
            .send_server_notification(ServerNotification::ThreadStarted(notif))
            .instrument(tracing::info_span!(
                "app_server.thread_start.notify_started",
                otel.name = "app_server.thread_start.notify_started",
            ))
            .await;
        session_telemetry.record_startup_phase(
            "thread_start_total",
            thread_start_started_at.elapsed(),
            Some("ready"),
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_thread_config_overrides(
        &self,
        model: Option<String>,
        model_provider: Option<String>,
        service_tier: Option<Option<String>>,
        cwd: Option<String>,
        runtime_workspace_roots: Option<Vec<AbsolutePathBuf>>,
        approval_policy: Option<codex_app_server_protocol::AskForApproval>,
        approvals_reviewer: Option<codex_app_server_protocol::ApprovalsReviewer>,
        sandbox: Option<SandboxMode>,
        permissions: Option<String>,
        base_instructions: Option<String>,
        developer_instructions: Option<String>,
        personality: Option<Personality>,
    ) -> ConfigOverrides {
        ConfigOverrides {
            model,
            model_provider,
            service_tier,
            cwd: cwd.map(PathBuf::from),
            workspace_roots: runtime_workspace_roots,
            default_permissions: permissions,
            approval_policy: approval_policy
                .map(codex_app_server_protocol::AskForApproval::to_core),
            approvals_reviewer: approvals_reviewer
                .map(codex_app_server_protocol::ApprovalsReviewer::to_core),
            sandbox_mode: sandbox.map(SandboxMode::to_core),
            codex_linux_sandbox_exe: self.arg0_paths.codex_linux_sandbox_exe.clone(),
            base_instructions,
            developer_instructions,
            personality,
            ..Default::default()
        }
    }
}
