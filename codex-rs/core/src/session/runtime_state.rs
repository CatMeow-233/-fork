//! Session runtime accessors: managed network proxy, rollout persistence, hooks, and config reload.

use super::*;

impl Session {
    pub(crate) async fn app_server_client_metadata(&self) -> AppServerClientMetadata {
        let state = self.state.lock().await;
        AppServerClientMetadata {
            client_name: state.session_configuration.app_server_client_name.clone(),
            client_version: state
                .session_configuration
                .app_server_client_version
                .clone(),
        }
    }

    pub(super) fn managed_network_proxy_active_for_permission_profile(
        permission_profile: &PermissionProfile,
    ) -> bool {
        !matches!(permission_profile, PermissionProfile::Disabled)
    }

    /// Builds the `x-codex-beta-features` header value for this session.
    ///
    /// `ModelClient` is session-scoped and intentionally does not depend on the full `Config`, so
    /// we precompute the comma-separated list of enabled experimental feature keys at session
    /// creation time and thread it into the client. Remote compaction stays advertised unconditionally.
    pub(super) fn build_model_client_beta_features_header(config: &Config) -> Option<String> {
        Some(
            FEATURES
                .iter()
                .filter(|spec| {
                    spec.id == Feature::RemoteCompactionV2
                        || (spec.stage.experimental_menu_description().is_some()
                            && config.features.enabled(spec.id))
                })
                .map(|spec| spec.key)
                .collect::<Vec<_>>()
                .join(","),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start_managed_network_proxy(
        spec: &crate::config::NetworkProxySpec,
        exec_policy: &codex_execpolicy::Policy,
        permission_profile: &PermissionProfile,
        windows_sandbox_type: SandboxType,
        network_policy_decider: Option<Arc<dyn codex_network_proxy::NetworkPolicyDecider>>,
        blocked_request_observer: Option<Arc<dyn codex_network_proxy::BlockedRequestObserver>>,
        managed_network_requirements_enabled: bool,
        audit_metadata: NetworkProxyAuditMetadata,
    ) -> anyhow::Result<(StartedNetworkProxy, SessionNetworkProxyRuntime)> {
        let spec = spec
            .with_exec_policy_network_rules(exec_policy)
            .map_err(|err| {
                tracing::warn!(
                    "failed to apply execpolicy network rules to managed proxy; continuing with configured network policy: {err}"
                );
                err
            })
            .unwrap_or_else(|_| spec.clone());
        let network_proxy = spec
            .start_proxy(
                permission_profile,
                managed_proxy_routing_for_windows_sandbox(windows_sandbox_type),
                local_binding_policy_for_sandbox(windows_sandbox_type, Some(std::env::consts::OS)),
                network_policy_decider,
                blocked_request_observer,
                managed_network_requirements_enabled,
                audit_metadata,
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to start managed network proxy: {err}"))?;
        let session_network_proxy = {
            let proxy = network_proxy.proxy();
            SessionNetworkProxyRuntime {
                http_addr: proxy.http_addr().to_string(),
                socks_addr: proxy.socks_addr().to_string(),
            }
        };
        Ok((network_proxy, session_network_proxy))
    }

    pub(super) async fn refresh_managed_network_proxy_for_current_permission_profile(&self) {
        let Ok(_refresh_guard) = self.managed_network_proxy_refresh_lock.acquire().await else {
            error!("managed network proxy refresh semaphore closed");
            return;
        };
        let session_configuration = {
            let state = self.state.lock().await;
            state.session_configuration.clone()
        };
        let Some(spec) = session_configuration
            .original_config_do_not_use
            .permissions
            .network
            .as_ref()
            .cloned()
        else {
            self.services.network_proxy.store(None);
            self.services
                .turn_environments
                .set_snapshot_credential_broker(SnapshotCredentialBrokerState::Inactive);
            return;
        };

        let spec = match spec
            .recompute_for_permission_profile(&session_configuration.permission_profile())
        {
            Ok(spec) => spec,
            Err(err) => {
                warn!("failed to rebuild managed network proxy policy for sandbox change: {err}");
                return;
            }
        };
        let current_exec_policy = self.services.exec_policy.current();
        let spec = match spec.with_exec_policy_network_rules(current_exec_policy.as_ref()) {
            Ok(spec) => spec,
            Err(err) => {
                warn!(
                    "failed to apply execpolicy network rules while refreshing managed network proxy: {err}"
                );
                spec
            }
        };
        // Disabled specs still carry managed requirements and constraints, but they do not have
        // listeners and must not be exposed as active managed proxy runtimes.
        if !spec.enabled() {
            self.services.network_proxy.store(None);
            self.services
                .turn_environments
                .set_snapshot_credential_broker(SnapshotCredentialBrokerState::Inactive);
            return;
        }
        let managed_proxy_routing =
            managed_proxy_routing_for_windows_sandbox(session_configuration.windows_sandbox_type);
        let started_proxy = self.services.network_proxy.load_full();
        let network_policy_decider = started_proxy
            .as_ref()
            .and_then(|started_proxy| started_proxy.network_policy_decider());
        if let Some(started_proxy) = started_proxy
            && started_proxy.proxy().managed_proxy_routing() == managed_proxy_routing
        {
            if let Err(err) = spec.apply_to_started_proxy(started_proxy.as_ref()).await {
                warn!("failed to refresh managed network proxy for sandbox change: {err}");
            } else {
                self.services
                    .turn_environments
                    .set_snapshot_credential_broker(if spec.credential_broker_enabled() {
                        SnapshotCredentialBrokerState::Ready(started_proxy.proxy())
                    } else {
                        SnapshotCredentialBrokerState::Inactive
                    });
            }
            return;
        }

        match Self::start_managed_network_proxy(
            &spec,
            current_exec_policy.as_ref(),
            &session_configuration.permission_profile(),
            session_configuration.windows_sandbox_type,
            network_policy_decider,
            Some(build_blocked_request_observer(Arc::clone(
                &self.services.network_approval,
            ))),
            self.services.managed_network_requirements_configured,
            self.services.network_proxy_audit_metadata.clone(),
        )
        .await
        {
            Ok((started_proxy, _session_network_proxy)) => {
                if spec.credential_broker_enabled() {
                    self.services
                        .turn_environments
                        .set_snapshot_credential_broker(SnapshotCredentialBrokerState::Ready(
                            started_proxy.proxy(),
                        ));
                }
                self.services
                    .network_proxy
                    .store(Some(Arc::new(started_proxy)));
            }
            Err(err) => {
                self.services
                    .turn_environments
                    .set_snapshot_credential_broker(if spec.credential_broker_enabled() {
                        SnapshotCredentialBrokerState::Unavailable
                    } else {
                        SnapshotCredentialBrokerState::Inactive
                    });
                warn!("failed to start managed network proxy for sandbox change: {err}");
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn codex_home(&self) -> AbsolutePathBuf {
        let state = self.state.lock().await;
        state.session_configuration.codex_home().clone()
    }

    pub(crate) fn subscribe_elicitation_pause_state(&self) -> watch::Receiver<bool> {
        self.services.elicitations.subscribe()
    }

    pub(crate) fn mark_interrupted(&self) {
        self.agent_status.send_replace(AgentStatus::Interrupted);
    }

    pub(crate) fn is_interrupted(&self) -> bool {
        matches!(*self.agent_status.borrow(), AgentStatus::Interrupted)
    }

    pub(crate) fn get_tx_event(&self) -> Sender<Event> {
        self.tx_event.clone()
    }

    pub(crate) fn state_db(&self) -> Option<state_db::StateDbHandle> {
        self.services.state_db.clone()
    }

    pub(crate) fn live_thread_for_persistence(
        &self,
        operation: &str,
    ) -> anyhow::Result<&LiveThread> {
        self.live_thread()
            .ok_or_else(|| anyhow::anyhow!("Session persistence is disabled; cannot {operation}."))
    }

    pub(crate) fn live_thread(&self) -> Option<&LiveThread> {
        self.services.live_thread.as_ref()
    }

    pub(crate) async fn set_thread_memory_mode(
        self: &Arc<Self>,
        mode: ThreadMemoryMode,
    ) -> anyhow::Result<()> {
        handlers::persist_thread_memory_mode_update(self, mode).await
    }

    /// Flush rollout writes and return the final durability-barrier result.
    #[instrument(name = "session.flush_rollout", level = "trace", skip_all)]
    pub(crate) async fn flush_rollout(&self) -> std::io::Result<()> {
        if let Some(live_thread) = self.live_thread() {
            live_thread.flush().await.map_err(std::io::Error::other)
        } else {
            Ok(())
        }
    }

    pub(crate) async fn try_ensure_rollout_materialized(
        &self,
        context: PersistContext,
    ) -> std::io::Result<()> {
        if let Some(live_thread) = self.live_thread() {
            live_thread
                .persist(context)
                .await
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    }

    pub(crate) async fn ensure_rollout_materialized(&self, context: PersistContext) {
        if let Err(e) = self.try_ensure_rollout_materialized(context).await {
            warn!("failed to materialize thread persistence: {e}");
        }
    }

    pub(super) fn next_internal_sub_id(&self) -> String {
        let id = self
            .next_internal_sub_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("auto-compact-{id}")
    }

    pub(crate) async fn get_base_instructions(&self) -> BaseInstructions {
        let state = self.state.lock().await;
        BaseInstructions {
            text: state.session_configuration.base_instructions.clone(),
            provenance: state.base_instructions_provenance.clone(),
        }
    }

    /// Render the request copy without changing instructions persisted or inherited by forks.
    pub(crate) async fn get_prompt_base_instructions(&self) -> BaseInstructions {
        let config = self.get_config().await;
        let instructions = self.get_base_instructions().await;
        if !config.update_plan_enabled
            && config.model_catalog.is_none()
            && matches!(
                instructions.provenance,
                Some(BaseInstructionsProvenance::Model { .. })
            )
        {
            BaseInstructions {
                text: codex_prompts::without_update_plan_instructions(&instructions.text),
                ..instructions
            }
        } else {
            instructions
        }
    }

    // Merges connector IDs into the session-level explicit connector selection.
    #[tracing::instrument(
        level = "trace",
        skip_all,
        fields(connector_count = connector_ids.len())
    )]
    pub(crate) async fn merge_connector_selection(
        &self,
        connector_ids: HashSet<String>,
    ) -> HashSet<String> {
        let mut state = self.state.lock().await;
        state.merge_connector_selection(connector_ids)
    }

    // Returns the connector IDs currently selected for this session.
    pub(crate) async fn get_connector_selection(&self) -> HashSet<String> {
        let state = self.state.lock().await;
        state.get_connector_selection()
    }

    // Clears connector IDs that were accumulated for explicit selection.
    pub(crate) async fn clear_connector_selection(&self) {
        let mut state = self.state.lock().await;
        state.clear_connector_selection();
    }

    pub(crate) async fn refresh_hooks(&self, mut config: Arc<Config>) {
        loop {
            let disabled_plugin_ids = self.state.lock().await.active_disabled_plugin_ids.clone();
            let environments = self.services.turn_environments.snapshot().await;
            let hooks_config = build_hooks_config(
                config.as_ref(),
                self.services.plugins_manager.as_ref(),
                environments.single_local_environment(),
                &disabled_plugin_ids,
            )
            .await;

            let state = self.state.lock().await;
            if Arc::ptr_eq(
                &state.session_configuration.original_config_do_not_use,
                &config,
            ) && state.active_disabled_plugin_ids == disabled_plugin_ids
            {
                let hooks = self.hooks().reconfigured(hooks_config);
                self.services.hooks.store(Arc::new(hooks));
                return;
            }
            // Unrelated updates can replace the owner too. Rebuild current inputs
            // so they cannot strand a pending plugin-policy refresh.
            config = Arc::clone(&state.session_configuration.original_config_do_not_use);
        }
    }

    pub(super) fn emit_config_changed_contributors(
        &self,
        previous_config: Option<&Config>,
        new_config: Option<&Config>,
    ) {
        let (Some(previous_config), Some(new_config)) = (previous_config, new_config) else {
            return;
        };
        if previous_config == new_config {
            return;
        }
        for contributor in self.services.extensions.config_contributors() {
            contributor.on_config_changed(
                &self.services.session_extension_data,
                &self.services.thread_extension_data,
                previous_config,
                new_config,
            );
        }
    }

    pub(crate) async fn reload_user_config_layer(&self) {
        // Refresh layer-backed runtime state for an existing session, including enabled plugin,
        // skill, hook, and MCP feature state. Other feature gates and legacy notify settings
        // remain session-static.
        //
        // Prefer `refresh_runtime_config()` when the host can already provide a materialized
        // config snapshot. This file-based path exists for legacy local reload flows.
        // Retry from the current owner if another publication wins while files are read.
        loop {
            let (expected_config, config_toml_paths) = {
                let state = self.state.lock().await;
                let config = Arc::clone(&state.session_configuration.original_config_do_not_use);
                let user_config_paths = config
                    .config_layer_stack
                    .all_layers_low_to_high()
                    .filter_map(|layer| match &layer.name {
                        ConfigLayerSource::User { file, .. } => Some(file.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let paths = if user_config_paths.is_empty() {
                    vec![
                        state
                            .session_configuration
                            .codex_home
                            .join(CONFIG_TOML_FILE),
                    ]
                } else {
                    user_config_paths
                };
                (config, paths)
            };

            let mut reloaded_user_configs = Vec::with_capacity(config_toml_paths.len());
            for config_toml_path in config_toml_paths {
                let user_config = match std::fs::read_to_string(&config_toml_path) {
                    Ok(contents) => match toml::from_str::<toml::Value>(&contents) {
                        Ok(config) => config,
                        Err(err) => {
                            warn!("failed to parse user config while reloading layer: {err}");
                            return;
                        }
                    },
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        toml::Value::Table(Default::default())
                    }
                    Err(err) => {
                        warn!("failed to read user config while reloading layer: {err}");
                        return;
                    }
                };
                let Some(config_dir) = config_toml_path.parent() else {
                    warn!("user config path has no parent directory");
                    return;
                };
                let user_config = match codex_config::loader::resolve_relative_paths_in_config_toml(
                    user_config,
                    &config_dir,
                ) {
                    Ok(config) => config,
                    Err(err) => {
                        warn!("failed to resolve paths while reloading user config: {err}");
                        return;
                    }
                };
                reloaded_user_configs.push((config_toml_path, user_config));
            }

            let mut next_config = expected_config.as_ref().clone();
            for (config_toml_path, user_config) in reloaded_user_configs {
                let config_layer_stack = match next_config
                    .config_layer_stack
                    .with_user_config(&config_toml_path, user_config)
                {
                    Ok(config_layer_stack) => config_layer_stack,
                    Err(err) => {
                        warn!("failed to validate user config while reloading layer: {err}");
                        return;
                    }
                };
                next_config.config_layer_stack = config_layer_stack;
            }
            match self
                .refresh_config(
                    expected_config,
                    next_config,
                    crate::config::RuntimeConfigRefresh::UserFiles,
                )
                .await
            {
                crate::ConfigRefreshOutcome::Stale => continue,
                crate::ConfigRefreshOutcome::Published | crate::ConfigRefreshOutcome::Rejected => {
                    return;
                }
            }
        }
    }

    /// Record a terminal CodexErr before the app-server completion notification is reduced.
    pub(crate) fn track_turn_codex_error(&self, turn_context: &TurnContext, error: &CodexErr) {
        self.services
            .analytics_events_client
            .track_turn_codex_error(TurnCodexErrorFact::from_codex_err(
                self.thread_id.to_string(),
                turn_context.sub_id.clone(),
                error,
            ));
    }

    /// Returns the reviewer pool installed by the Guardian extension.
    pub(crate) fn guardian_review_session(&self) -> Option<Arc<GuardianReviewSessionManager>> {
        self.services
            .thread_extension_data
            .get::<GuardianReviewSessionManager>()
    }

    pub(crate) async fn is_private_guardian_reviewer(&self) -> bool {
        self.state
            .lock()
            .await
            .session_configuration
            .trusted_guardian_reviewer
    }

    pub(crate) async fn mcp_dependency_prompted(&self) -> HashSet<String> {
        let state = self.state.lock().await;
        state.mcp_dependency_prompted()
    }

    pub(crate) async fn record_mcp_dependency_prompted<I>(&self, names: I)
    where
        I: IntoIterator<Item = String>,
    {
        let mut state = self.state.lock().await;
        state.record_mcp_dependency_prompted(names);
    }

    pub(crate) async fn record_memory_citation_for_turn(&self, sub_id: &str) {
        let turn_state = self
            .input_queue
            .turn_state_for_sub_id(&self.active_turn, sub_id)
            .await;
        let Some(turn_state) = turn_state else {
            return;
        };
        turn_state.lock().await.has_memory_citation = true;
    }

    pub async fn interrupt_task(self: &Arc<Self>) {
        info!("interrupt received: abort current task, if any");
        let had_active_turn = self.active_turn.lock().await.is_some();
        self.abort_all_tasks(TurnAbortReason::Interrupted).await;
        if !had_active_turn {
            self.cancel_mcp_startup();
        }
    }

    pub(crate) fn hooks(&self) -> Arc<Hooks> {
        self.services.hooks.load_full()
    }

    pub(crate) fn user_shell(&self) -> Arc<shell::Shell> {
        Arc::clone(&self.services.user_shell)
    }

    pub(crate) async fn current_rollout_path(&self) -> anyhow::Result<Option<PathBuf>> {
        let Some(live_thread) = self.live_thread() else {
            return Ok(None);
        };
        live_thread.local_rollout_path().await.map_err(Into::into)
    }

    pub(crate) async fn hook_transcript_path(&self) -> Option<PathBuf> {
        let rollout_path = match self.current_rollout_path().await {
            Ok(Some(path)) => path,
            Ok(None) => return None,
            Err(err) => {
                warn!("{err}");
                return None;
            }
        };
        self.ensure_rollout_materialized(PersistContext::Standard)
            .await;
        Some(rollout_path)
    }

    pub(crate) async fn take_pending_session_start_source(
        &self,
    ) -> Option<codex_hooks::SessionStartSource> {
        let mut state = self.state.lock().await;
        state.take_pending_session_start_source()
    }
}
