//! Session settings updates, configuration snapshots, and feature accessors.

use super::*;

impl Session {
    pub(super) async fn previous_turn_settings(&self) -> Option<PreviousTurnSettings> {
        let state = self.state.lock().await;
        state.previous_turn_settings()
    }

    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) async fn set_previous_turn_settings(
        &self,
        previous_turn_settings: Option<PreviousTurnSettings>,
    ) {
        let mut state = self.state.lock().await;
        state.set_previous_turn_settings(previous_turn_settings);
    }

    pub(crate) async fn update_settings(
        &self,
        updates: SessionSettingsUpdate,
    ) -> ConstraintResult<SessionSettingsCommit> {
        let Some(commit) = self.update_settings_if(updates, |_, _| true).await? else {
            unreachable!("unconditional settings updates must commit");
        };
        Ok(commit)
    }

    /// Evaluates the caller's synchronous predicate against the current configuration
    /// and validated candidate under the same state lock used for publication.
    /// This prevents a stale admission decision. Rejection has no settings or runtime
    /// effects.
    ///
    /// The predicate must be fast and side-effect-free. It must not block, acquire
    /// other locks, or call back into `Session`.
    pub(super) async fn update_settings_if(
        &self,
        updates: SessionSettingsUpdate,
        should_commit: impl FnOnce(&SessionConfiguration, &SessionConfiguration) -> bool + Send,
    ) -> ConstraintResult<Option<SessionSettingsCommit>> {
        let notify_config_contributors = !self.services.extensions.config_contributors().is_empty();
        let windows_sandbox_changed;
        let mut tier_acceptance = None;
        let (
            commit,
            previous_config,
            new_config,
            permission_profile_changed,
            mcp_inputs_changed,
            credential_masking_warning,
        ) = {
            let mut state = self.state.lock().await;
            let mut updated =
                match self.apply_session_settings(&state.session_configuration, &updates) {
                    Ok(updated) => updated,
                    Err(err) => {
                        warn!("rejected session settings update: {err}");
                        return Err(err);
                    }
                };

            if let Some(catalog) = self.services.models_manager.refreshed_model_catalog() {
                model_catalog_settings::normalize(&mut updated.step_settings, &catalog);
            }

            if !should_commit(&state.session_configuration, &updated) {
                return Ok(None);
            }

            let credential_masking_warning = updated
                .original_config_do_not_use
                .credential_masking_warning(&updated.permission_profile())
                .filter(|warning| {
                    Some(*warning)
                        != state
                            .session_configuration
                            .original_config_do_not_use
                            .credential_masking_warning(
                                &state.session_configuration.permission_profile(),
                            )
                });
            let previous_config = notify_config_contributors
                .then(|| self.build_effective_session_config(&state.session_configuration));
            let previous_permission_profile = state.session_configuration.permission_profile();
            let updated_permission_profile = updated.permission_profile();
            let permission_profile_changed =
                previous_permission_profile != updated_permission_profile;
            let root_service_tier_changed = updated.parent_thread_id.is_none()
                && state.session_configuration.step_settings.service_tier
                    != updated.step_settings.service_tier;
            let mcp_inputs_changed = self.mcp_inputs_differ(&state.session_configuration, &updated);
            if mcp_inputs_changed {
                self.mark_mcp_runtime_dirty();
            }
            // Save new environment defaults for future turns. The running turn keeps its own.
            windows_sandbox_changed =
                state.session_configuration.windows_sandbox_level != updated.windows_sandbox_level;
            self.services
                .turn_environments
                .set_configured_thread_defaults(updated.inferred_environment_config());
            if updated.environments != state.session_configuration.environments {
                self.services
                    .turn_environments
                    .update_configured_selections(&updated.environments);
            }
            state.session_configuration = updated;
            if root_service_tier_changed && !state.shared_service_tier_pending {
                state.service_tier_acceptance = crate::agent::host::propagate_config_update(
                    self,
                    AgentConfigUpdate::ServiceTier(
                        state
                            .session_configuration
                            .step_settings
                            .service_tier
                            .clone(),
                    ),
                );
            }
            if root_service_tier_changed || updates.step_settings.service_tier.is_some() {
                tier_acceptance = state.service_tier_acceptance.clone();
            }
            let new_config = notify_config_contributors
                .then(|| self.build_effective_session_config(&state.session_configuration));
            let commit = SessionSettingsCommit {
                configuration: state.session_configuration.clone(),
                snapshot: state
                    .session_configuration
                    .thread_settings_snapshot(&state.session_configuration.environments),
            };
            (
                commit,
                previous_config,
                new_config,
                permission_profile_changed,
                mcp_inputs_changed,
                credential_masking_warning,
            )
        };
        if windows_sandbox_changed {
            crate::windows_sandbox::log_windows_sandbox_change(
                commit.configuration.codex_home.as_path(),
                commit.configuration.windows_sandbox_type,
                commit.configuration.windows_sandbox_level,
            );
        }
        self.emit_config_changed_contributors(previous_config.as_ref(), new_config.as_ref());
        if permission_profile_changed {
            self.refresh_managed_network_proxy_for_current_permission_profile()
                .await;
        }
        if let Some(message) = credential_masking_warning {
            warn!("{message}");
            self.send_event_raw(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::Warning(WarningEvent {
                    message: message.to_string(),
                }),
            })
            .await;
        }
        if mcp_inputs_changed {
            self.schedule_mcp_prewarm();
        }
        if let Some(accepted) = tier_acceptance {
            accepted.await;
        }
        Ok(Some(commit))
    }

    pub(crate) async fn preview_settings(
        &self,
        updates: &SessionSettingsUpdate,
    ) -> ConstraintResult<ThreadConfigSnapshot> {
        let configuration = {
            let state = self.state.lock().await;
            self.apply_session_settings(&state.session_configuration, updates)?
        };
        Ok(self
            .effective_thread_config_snapshot(&configuration, configuration.environments.clone())
            .await)
    }

    pub(crate) async fn selected_model_info(&self) -> Arc<ModelInfo> {
        let (settings, overrides) = {
            let state = self.state.lock().await;
            let config = &state.session_configuration;
            (
                Arc::clone(&config.step_settings),
                config.model_info_overrides.clone(),
            )
        };
        settings
            .resolve_model_info(self.services.models_manager.as_ref(), &overrides)
            .await
            .model
    }

    pub(crate) async fn thread_config_snapshot(&self) -> ThreadConfigSnapshot {
        self.resume_config_snapshot().await.0
    }

    pub(crate) async fn resume_config_snapshot(&self) -> (ThreadConfigSnapshot, Option<String>) {
        let configuration = self.state.lock().await.session_configuration.clone();
        let selected_service_tier = configuration.step_settings.service_tier.clone();
        let snapshot = self
            .effective_thread_config_snapshot(&configuration, configuration.environments.clone())
            .await;
        (snapshot, selected_service_tier)
    }

    /// Captures configured child bindings, or the active turn's bindings for legacy inheritance.
    pub(crate) async fn inherited_environment_snapshot(&self) -> TurnEnvironmentSnapshot {
        let snapshot = {
            let _state = self.state.lock().await;
            if self.features.enabled(Feature::SubagentThreadEnvironments) {
                return self.services.turn_environments.configured_snapshot_now();
            }
            self.services.turn_environments.snapshot()
        };
        snapshot.await
    }

    pub(crate) async fn configured_environment_selections(&self) -> Vec<TurnEnvironmentSelection> {
        let state = self.state.lock().await;
        state.session_configuration.environments.clone()
    }

    pub(crate) async fn thread_settings_snapshot(&self) -> ThreadSettingsSnapshot {
        let state = self.state.lock().await;
        state
            .session_configuration
            .thread_settings_snapshot(&state.session_configuration.environments)
    }

    /// Keep this guard through registration and publication to order the initial tier
    /// with settings commits from the already-running submission loop.
    pub(crate) async fn lock_pending_service_tier(
        &self,
    ) -> Option<(MutexGuard<'_, SessionState>, AgentConfigUpdate)> {
        let state = self.state.lock().await;
        if !state.shared_service_tier_pending {
            return None;
        }
        let update = AgentConfigUpdate::ServiceTier(
            state
                .session_configuration
                .step_settings
                .service_tier
                .clone(),
        );
        Some((state, update))
    }

    pub(crate) async fn restorable_thread_settings(&self) -> CodexThreadSettingsOverrides {
        let state = self.state.lock().await;
        state
            .session_configuration
            .restorable_thread_settings(state.session_configuration.environments.clone())
    }

    pub(crate) async fn set_app_server_client_info(
        &self,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        mcp_elicitations_auto_deny: bool,
    ) -> ConstraintResult<()> {
        self.update_settings(SessionSettingsUpdate {
            app_server_client_name,
            app_server_client_version,
            ..Default::default()
        })
        .await?;
        self.services
            .mcp_runtime
            .set_elicitations_auto_deny(mcp_elicitations_auto_deny);
        Ok(())
    }

    pub(crate) async fn instruction_sources(&self) -> Vec<PathUri> {
        self.services
            .agents_md_manager
            .get_loaded()
            .await
            .as_ref()
            .map_or_else(Vec::new, |instructions| instructions.sources().collect())
    }

    #[cfg(test)]
    pub(crate) async fn set_session_startup_prewarm(
        &self,
        startup_prewarm: SessionStartupPrewarmHandle,
    ) {
        let mut state = self.state.lock().await;
        state.set_session_startup_prewarm(startup_prewarm);
    }

    pub(crate) async fn take_session_startup_prewarm(&self) -> Option<SessionStartupPrewarmHandle> {
        let mut state = self.state.lock().await;
        state.take_session_startup_prewarm()
    }

    pub(crate) async fn fork_config(
        &self,
    ) -> (
        Config,
        ThreadConfigSnapshot,
        WindowsSandboxLevel,
        TurnEnvironmentSnapshot,
        ReasoningEffortPin,
    ) {
        let (configuration, environments, mut reasoning_effort_pin) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.clone(),
                self.services.turn_environments.snapshot_now(),
                state.reasoning_effort_pin.clone(),
            )
        };
        let config = self.build_effective_session_config(&configuration);
        if !config.features.enabled(Feature::ReasoningEffortOverride) {
            let model = configuration
                .step_settings
                .resolve_model_info(
                    self.services.models_manager.as_ref(),
                    &configuration.model_info_overrides,
                )
                .await;
            if let Some(effort) = config
                .model_reasoning_effort
                .clone()
                .or_else(|| model.default_reasoning_level.clone())
            {
                // Without overrides, the parent's selected effort is its request baseline.
                reasoning_effort_pin = ReasoningEffortPin::Active {
                    model: model.slug.clone(),
                    effort: model.resolve_reasoning_effort(effort),
                };
            }
        }
        (
            config,
            self.effective_thread_config_snapshot(&configuration, environments.all_selections())
                .await,
            configuration.windows_sandbox_level,
            environments,
            reasoning_effort_pin,
        )
    }

    pub(crate) async fn get_config(&self) -> std::sync::Arc<Config> {
        let state = self.state.lock().await;
        state
            .session_configuration
            .original_config_do_not_use
            .clone()
    }

    pub(crate) async fn inherited_instructions(&self) -> SessionInstructions {
        self.services
            .agents_md_manager
            .inherited_instructions()
            .await
    }

    pub(crate) async fn emit_instruction_warnings(&self, warnings: Vec<String>) {
        for message in warnings {
            self.send_event_raw(Event {
                id: INITIAL_SUBMIT_ID.to_owned(),
                msg: EventMsg::Warning(WarningEvent { message }),
            })
            .await;
        }
    }

    pub(crate) async fn provider(&self) -> ModelProviderInfo {
        let state = self.state.lock().await;
        state.session_configuration.provider.info().clone()
    }

    pub fn enabled(&self, feature: Feature) -> bool {
        self.features.enabled(feature)
    }

    pub(crate) fn features(&self) -> ManagedFeatures {
        self.features.clone()
    }

    pub(crate) async fn collaboration_mode(&self) -> CollaborationMode {
        let state = self.state.lock().await;
        state
            .session_configuration
            .step_settings
            .collaboration_mode
            .clone()
    }

    pub(crate) fn multi_agent_version(&self) -> Option<MultiAgentVersion> {
        self.multi_agent_version.get().copied()
    }

    pub(crate) fn set_multi_agent_version_if_unset(
        &self,
        multi_agent_version: MultiAgentVersion,
    ) -> MultiAgentVersion {
        *self.multi_agent_version.get_or_init(|| multi_agent_version)
    }

    pub(crate) fn resolve_multi_agent_version_for_model(
        &self,
        model_info: &ModelInfo,
        config: &Config,
    ) -> MultiAgentVersion {
        if let Some(multi_agent_version) = self.multi_agent_version() {
            return config.multi_agent_version_for_model(Some(multi_agent_version));
        }

        let selected = config.multi_agent_version_for_model(model_info.multi_agent_version);

        self.set_multi_agent_version_if_unset(selected)
    }
}
