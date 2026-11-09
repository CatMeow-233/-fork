//! Forks an existing thread into a new thread for `thread/fork`.

use super::*;

impl ThreadRequestProcessor {
    pub(super) async fn thread_fork_inner(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadForkParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
    ) -> Result<(), JSONRPCErrorError> {
        if params.experimental_disable_prediction_mcp && !params.experimental_prediction_mode {
            return Err(invalid_request(
                "`experimentalDisablePredictionMcp` requires `experimentalPredictionMode: true`",
            ));
        }
        let has_config_overrides = params.model.is_some()
            || params.model_provider.is_some()
            || params.service_tier.is_some()
            || params.cwd.is_some()
            || params.runtime_workspace_roots.is_some()
            || params.approval_policy.is_some()
            || params.approvals_reviewer.is_some()
            || params.sandbox.is_some()
            || params.permissions.is_some()
            || params.base_instructions.is_some()
            || params.developer_instructions.is_some()
            || params
                .config
                .as_ref()
                .is_some_and(|config| config.keys().any(|key| key != "thread_unload_delay_secs"));
        if params.experimental_prediction_mode && has_config_overrides {
            return Err(invalid_request(
                "`experimentalPredictionMode` cannot be combined with configuration overrides",
            ));
        }
        if params.experimental_prediction_mode && !params.ephemeral {
            return Err(invalid_request(
                "`experimentalPredictionMode` requires `ephemeral: true`",
            ));
        }
        let ThreadForkParams {
            thread_id,
            experimental_prediction_mode,
            experimental_disable_prediction_mcp,
            last_turn_id,
            before_turn_id,
            path,
            model,
            model_provider,
            service_tier,
            cwd,
            runtime_workspace_roots,
            approval_policy,
            approvals_reviewer,
            sandbox,
            permissions,
            config: cli_overrides,
            base_instructions,
            developer_instructions,
            ephemeral,
            attach_to_source,
            thread_source,
            exclude_turns,
            defer_goal_continuation,
        } = params;
        let include_turns = !exclude_turns;
        if attach_to_source {
            if ephemeral || defer_goal_continuation {
                return Err(invalid_request(
                    "attachToSource cannot be combined with ephemeral or deferGoalContinuation",
                ));
            }
            if self.state_db.is_none() || !self.thread_store.as_any().is::<LocalThreadStore>() {
                return Err(unsupported_thread_store_operation("attached threads"));
            }
        }
        if sandbox.is_some() && permissions.is_some() {
            return Err(invalid_request(
                "`permissions` cannot be combined with `sandbox`",
            ));
        }
        if experimental_prediction_mode
            && (path.is_some() || last_turn_id.is_some() || before_turn_id.is_some())
        {
            return Err(invalid_request(
                "`experimentalPredictionMode` requires a thread id without a path or turn cutoff",
            ));
        }
        let source_thread = self
            .read_stored_thread_for_resume(
                &thread_id,
                path.as_ref(),
                /*include_history*/ false,
            )
            .await?;
        let loaded_parent = self
            .thread_manager
            .get_thread(source_thread.thread_id)
            .await
            .ok();
        let inherited_fork = if experimental_prediction_mode {
            // Keep inherited state and its preparation off the fork handler's stack.
            let (mut options, settings) = Box::pin(async {
                self.thread_manager
                    .fork_options_from_parent(source_thread.thread_id)
                    .await
                    .map(|(options, settings)| (Box::new(options), Box::new(settings)))
            })
            .await
            .map_err(|err| match err.details() {
                CodexErrorDetails::ThreadNotFound(_) => {
                    invalid_request("`experimentalPredictionMode` requires a loaded parent")
                }
                _ => internal_error(format!("failed to inherit parent state: {err}")),
            })?;
            options.config.ephemeral = ephemeral;
            // Runtime retention does not change the inherited prompt or cache key.
            if let Some(value) = cli_overrides
                .as_ref()
                .and_then(|config| config.get("thread_unload_delay_secs"))
            {
                let seconds = value.as_u64().ok_or_else(|| {
                    invalid_request("`thread_unload_delay_secs` must be a non-negative integer")
                })?;
                let delay = Duration::from_secs(seconds);
                if std::time::Instant::now().checked_add(delay).is_none() {
                    return Err(invalid_request("`thread_unload_delay_secs` is too large"));
                }
                options.config.thread_unload_delay = delay;
            }
            options
                .thread_extension_init
                .insert(codex_core::compact::AutoCompactionDisabled);
            if experimental_disable_prediction_mcp
                && let Some(parent) = loaded_parent.as_ref()
                && parent
                    .conversation_history_snapshot()
                    .await
                    .items()
                    .any(|item| matches!(item, ResponseItem::AdditionalTools { .. }))
                && let Some(model) = options.config.model.as_deref()
                && self
                    .thread_manager
                    .get_models_manager()
                    .get_model_info(model, &options.config.to_models_manager_config())
                    .await
                    .use_responses_lite
            {
                options
                    .thread_extension_init
                    .insert(codex_core::McpServersDisabled);
            }
            options
                .config
                .features
                .enable(Feature::ReasoningEffortOverride)
                .map_err(|err| invalid_request(err.to_string()))?;
            Some((options, settings))
        } else {
            None
        };
        let paginated_source = matches!(source_thread.history_mode, ThreadHistoryMode::Paginated);
        if attach_to_source && !paginated_source {
            return Err(invalid_request(
                "attachToSource requires a paginated parent",
            ));
        }
        if last_turn_id.is_some() && before_turn_id.is_some() {
            return Err(invalid_request(
                "`beforeTurnId` cannot be combined with `lastTurnId`",
            ));
        }
        if ephemeral && defer_goal_continuation {
            return Err(invalid_request(
                "`deferGoalContinuation` cannot be combined with `ephemeral`",
            ));
        }
        if paginated_source && ephemeral && include_turns {
            return Err(invalid_request(
                "ephemeral paginated thread/fork requires `excludeTurns: true`",
            ));
        }
        if paginated_source && include_turns {
            self.send_deprecation_notice(
                request_id.connection_id,
                PAGINATED_FULL_HISTORY_DEPRECATION_SUMMARY,
            )
            .await;
        }
        let source_thread_id = source_thread.thread_id;
        let source_thread_name = source_thread
            .name
            .as_deref()
            .and_then(codex_core::util::normalize_thread_name);
        let mut prepared_fork = if paginated_source {
            let boundary = match (last_turn_id.as_deref(), before_turn_id.as_deref()) {
                (Some(turn_id), None) => {
                    codex_thread_store::ForkBoundary::ThroughTurn(turn_id.to_string())
                }
                (None, Some(turn_id)) => {
                    codex_thread_store::ForkBoundary::BeforeTurn(turn_id.to_string())
                }
                (None, None) => codex_thread_store::ForkBoundary::Latest,
                (Some(_), Some(_)) => unreachable!("fork boundaries are mutually exclusive"),
            };
            Some(
                self.thread_store
                    .prepare_fork(codex_thread_store::PrepareForkParams {
                        thread_id: source_thread_id,
                        boundary,
                    })
                    .await
                    .map_err(|err| match err {
                        ThreadStoreError::InvalidRequest { message } => invalid_request(message),
                        ThreadStoreError::ThreadNotFound { thread_id } => {
                            invalid_request(format!("no rollout found for thread id {thread_id}"))
                        }
                        ThreadStoreError::Unsupported { .. } => {
                            method_not_found("paginated_threads is not supported yet")
                        }
                        err => internal_error(format!("failed to prepare paginated fork: {err}")),
                    })?,
            )
        } else {
            None
        };
        let source_history_items = if let Some(prepared_fork) = prepared_fork.as_ref() {
            Arc::clone(&prepared_fork.model_context)
        } else {
            let mut source_thread = self
                .read_stored_thread_for_resume(
                    &thread_id,
                    path.as_ref(),
                    /*include_history*/ true,
                )
                .await?;
            Arc::new(
                source_thread
                    .history
                    .take()
                    .map(|history| history.items)
                    .ok_or_else(|| {
                        internal_error(format!(
                            "thread {source_thread_id} did not include persisted history"
                        ))
                    })?,
            )
        };
        let history_cwd = Some(source_thread.cwd.clone());

        // Persist Windows sandbox mode.
        let mut cli_overrides = cli_overrides.unwrap_or_default();
        if cfg!(windows) && !experimental_prediction_mode {
            let mode = self.config.permissions.windows_sandbox_mode.or_else(|| {
                match WindowsSandboxLevel::from_config(&self.config) {
                    WindowsSandboxLevel::Elevated => Some(WindowsSandboxModeToml::Elevated),
                    WindowsSandboxLevel::RestrictedToken => {
                        Some(WindowsSandboxModeToml::Unelevated)
                    }
                    WindowsSandboxLevel::Disabled => None,
                }
            });
            if let Some(mode) = mode {
                cli_overrides.insert("windows.sandbox".to_string(), serde_json::json!(mode));
            }
        }
        let request_overrides = if cli_overrides.is_empty() {
            None
        } else {
            Some(cli_overrides)
        };
        let runtime_workspace_roots = runtime_workspace_roots.map(resolve_runtime_workspace_roots);
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
            /*personality*/ None,
        );
        typesafe_overrides.ephemeral = ephemeral.then_some(true);
        let restore_approval_policy = typesafe_overrides.approval_policy.is_none();
        let restore_approvals_reviewer = typesafe_overrides.approvals_reviewer.is_none()
            && !request_overrides
                .as_ref()
                .is_some_and(|overrides| overrides.contains_key("approvals_reviewer"));
        let restore_permission_profile =
            !has_permission_override(request_overrides.as_ref(), &typesafe_overrides);
        let needs_latest_settings =
            restore_approval_policy || restore_approvals_reviewer || restore_permission_profile;
        let loaded_parent_settings = if paginated_source && needs_latest_settings {
            if let Some(parent) = loaded_parent.as_ref() {
                let snapshot = parent.thread_settings_snapshot().await;
                Some(PersistedResumeSettings::from(&snapshot))
            } else {
                None
            }
        } else {
            None
        };
        let latest_context = if paginated_source
            && needs_latest_settings
            && loaded_parent.is_none()
            && (last_turn_id.is_some() || before_turn_id.is_some())
        {
            Some(Arc::new(
                self.thread_store
                    .load_latest_model_context(StoreLoadThreadHistoryParams {
                        thread_id: source_thread_id,
                        include_archived: true,
                    })
                    .await
                    .map_err(thread_store_resume_read_error)?
                    .items,
            ))
        } else {
            None
        };
        // The fork cutoff can remove the only TurnContext that records the selected version.
        // Recover it from the untrimmed source or live parent, independently of permission overrides.
        let source_multi_agent_version = InitialHistory::Resumed(ResumedHistory {
            history_revision: None,
            conversation_id: source_thread_id,
            history: Arc::clone(latest_context.as_ref().unwrap_or(&source_history_items)),
            rollout_path: source_thread.rollout_path.clone(),
        })
        .get_multi_agent_version()
        .or_else(|| {
            loaded_parent
                .as_ref()
                .and_then(|parent| parent.multi_agent_version())
        });
        let persisted_settings = loaded_parent_settings.or_else(|| {
            let history = latest_context
                .as_deref()
                .unwrap_or_else(|| source_history_items.as_ref());
            PersistedResumeSettings::from_history(history, source_thread_id)
        });
        if let Some(persisted_settings) = persisted_settings {
            if restore_approval_policy {
                typesafe_overrides.approval_policy = Some(persisted_settings.approval_policy);
            }
            if restore_approvals_reviewer {
                typesafe_overrides.approvals_reviewer = persisted_settings.approvals_reviewer;
            }
            apply_persisted_permission_profile(
                &persisted_settings,
                request_overrides.as_ref(),
                &mut typesafe_overrides,
            );
        }
        let (inherited_options, inherited_settings) = inherited_fork.unzip();
        let options = if let Some(options) = inherited_options {
            *options
        } else {
            // Derive a Config using the same logic as new conversation, honoring overrides if provided.
            let config = self
                .config_manager
                .load_for_cwd(request_overrides, typesafe_overrides, history_cwd)
                .await
                .map_err(|err| config_load_error(&err))?;
            StartThreadOptions {
                client_mcp_extensions,
                ..StartThreadOptions::new(config)
            }
        };
        let goals_enabled = options.config.features.enabled(Feature::Goals);

        let fallback_model_provider = options.config.model_provider_id.clone();
        let parent_trace = self.request_trace_context(&request_id).await;
        let thread_source = thread_source.map(Into::into);

        let mut history_items = if prepared_fork.is_some() {
            source_history_items
        } else {
            let source_history_items = Arc::unwrap_or_clone(source_history_items);
            let history_items = match (last_turn_id.as_deref(), before_turn_id.as_deref()) {
                (Some(last_turn_id), None) => {
                    truncate_rollout_after_turn_id(source_history_items, last_turn_id)
                        .map_err(|err| core_thread_write_error("truncate thread for fork", err))?
                }
                (None, Some(before_turn_id)) => {
                    truncate_rollout_before_turn_id(source_history_items, before_turn_id)
                        .map_err(|err| core_thread_write_error("truncate thread for fork", err))?
                }
                (None, None) => source_history_items,
                (Some(_), Some(_)) => unreachable!("fork boundaries are mutually exclusive"),
            };
            Arc::new(history_items)
        };
        if let Some(multi_agent_version) = source_multi_agent_version
            && (last_turn_id.is_some() || before_turn_id.is_some())
        {
            // Update only the fork's in-memory metadata; the source rollout stays unchanged.
            for item in Arc::make_mut(&mut history_items) {
                if let RolloutItem::SessionMeta(meta) = item
                    && meta.meta.id == source_thread_id
                {
                    meta.meta.multi_agent_version = Some(multi_agent_version);
                }
            }
            if let Some(prepared_fork) = prepared_fork.as_mut() {
                prepared_fork.model_context = Arc::clone(&history_items);
            }
        }

        let ephemeral_preview = if ephemeral {
            if paginated_source && last_turn_id.is_none() && before_turn_id.is_none() {
                source_thread.preview.clone()
            } else {
                preview_from_rollout_items(&history_items)
            }
        } else {
            String::new()
        };
        let ephemeral_turns = if ephemeral && include_turns {
            build_legacy_api_turns_from_rollout_items(&history_items)
        } else {
            Vec::new()
        };
        let ephemeral_token_usage_turn_id = (ephemeral && include_turns)
            .then(|| restored_token_usage_turn_id(&history_items, ephemeral_turns.as_slice()));
        let token_usage_history_items = paginated_source.then(|| Arc::clone(&history_items));
        let inherited_project_id = source_thread.project_id.clone();
        let reserved_thread_id = if options.config.ephemeral {
            None
        } else {
            stage_pending_thread_metadata(
                self.thread_manager.as_ref(),
                self.thread_store.as_ref(),
                StoreThreadMetadataPatch {
                    project_id: inherited_project_id.clone().map(Some),
                    daybreak_enabled: source_thread.daybreak_enabled,
                    ..Default::default()
                },
                "thread/fork",
            )
            .await?
        };

        let fork_options = StartThreadOptions {
            attached_thread: attach_to_source,
            thread_source,
            parent_trace,
            reserved_thread_id,
            ..options
        };
        let new_thread = if let Some(prepared_fork) = prepared_fork {
            self.thread_manager
                .fork_prepared_thread(fork_options, prepared_fork)
                .await
        } else {
            self.thread_manager
                .fork_thread_from_history(
                    ForkSnapshot::Interrupted,
                    fork_options,
                    InitialHistory::Resumed(ResumedHistory {
                        history_revision: None,
                        conversation_id: source_thread_id,
                        history: history_items,
                        rollout_path: source_thread.rollout_path.clone(),
                    }),
                )
                .await
        };
        let NewThread {
            thread_id,
            thread: forked_thread,
            session_configured,
            ..
        } = match new_thread {
            Ok(new_thread) => new_thread,
            Err(err) => {
                remove_pending_thread_metadata(self.thread_store.as_ref(), reserved_thread_id)
                    .await;
                return Err(match err.details() {
                    CodexErrorDetails::Io(_) | CodexErrorDetails::Json(_) => {
                        invalid_request(format!("failed to load thread {source_thread_id}: {err}"))
                    }
                    CodexErrorDetails::InvalidRequest(message) => invalid_request(message.clone()),
                    _ => internal_error(format!("error forking thread: {err}")),
                });
            }
        };

        if let Some(settings) = inherited_settings {
            // Construct the restore future off the fork handler's stack
            Box::pin(async { Box::pin(forked_thread.restore_thread_settings(*settings)).await })
                .await
                .map_err(|err| invalid_request(err.to_string()))?;
        }

        Self::set_app_server_client_info(
            forked_thread.as_ref(),
            app_server_client_name,
            app_server_client_version,
        )
        .await?;
        if session_configured.rollout_path.is_some()
            && let Some(name) = source_thread_name.clone()
        {
            self.thread_manager
                .update_thread_metadata(
                    thread_id,
                    StoreThreadMetadataPatch {
                        name: Some(Some(name)),
                        ..Default::default()
                    },
                    /*include_archived*/ true,
                )
                .await
                .map_err(|err| core_thread_write_error("inherit source thread name", err))?;
        }
        let inherited_goal = if defer_goal_continuation
            && session_configured.rollout_path.is_some()
            && goals_enabled
        {
            if let Some(state_db) = forked_thread.state_db().or_else(|| self.state_db.clone()) {
                self.thread_goal_processor
                    .flush_goal_progress_for_fork(source_thread_id)
                    .await
                    .map_err(|err| {
                        internal_error(format!("failed to flush source thread goal: {err}"))
                    })?;
                inherit_thread_goal_snapshot(&state_db, source_thread_id, thread_id)
                    .await
                    .map_err(|err| {
                        internal_error(format!("failed to inherit source thread goal: {err}"))
                    })?
            } else {
                false
            }
        } else {
            false
        };
        if inherited_goal {
            self.thread_goal_processor
                .restore_inherited_goal_runtime(thread_id)
                .await;
        }

        let instruction_sources = forked_thread.legacy_instruction_sources().await;

        // Auto-attach a conversation listener when forking a thread.
        log_listener_attach_result(
            self.ensure_conversation_listener(
                thread_id,
                request_id.connection_id,
                /*raw_events_enabled*/ false,
            )
            .await,
            thread_id,
            request_id.connection_id,
            "thread",
        );

        let config_snapshot = forked_thread.config_snapshot().await;

        // Persistent forks materialize their own rollout immediately. Ephemeral forks stay
        // pathless, so their visible history is projected before the source history is consumed.
        let (mut thread, mut token_usage_turn_id) = if session_configured.rollout_path.is_some() {
            let stored_thread = self
                .read_stored_thread_for_new_fork(thread_id, include_turns && !paginated_source)
                .await?;
            let (mut thread, history) = thread_from_stored_thread(
                stored_thread,
                fallback_model_provider.as_str(),
                &self.config.cwd,
            );
            if include_turns && let Some(history) = history.as_ref() {
                populate_thread_turns_from_history(
                    &mut thread,
                    &history.items,
                    /*active_turn*/ None,
                );
            }
            let token_usage_turn_id = include_turns.then(|| {
                restored_token_usage_turn_id(
                    history
                        .as_ref()
                        .map(|history| history.items.as_slice())
                        .or_else(|| token_usage_history_items.as_deref().map(Vec::as_slice))
                        .unwrap_or(&[]),
                    thread.turns.as_slice(),
                )
            });
            (thread, token_usage_turn_id)
        } else {
            let mut thread = build_thread_from_snapshot(
                thread_id,
                session_configured.session_id.to_string(),
                forked_thread.multi_agent_version(),
                &config_snapshot,
                /*path*/ None,
            );
            thread.preview = ephemeral_preview;
            thread.forked_from_id = Some(source_thread_id.to_string());
            thread.turns = ephemeral_turns;
            (thread, ephemeral_token_usage_turn_id)
        };
        if paginated_source && include_turns {
            thread.turns = self.paginated_thread_full_turns(thread_id).await?;
            token_usage_turn_id = Some(restored_token_usage_turn_id(
                token_usage_history_items
                    .as_deref()
                    .map_or(&[], Vec::as_slice),
                thread.turns.as_slice(),
            ));
        }
        let token_usage_turn_id =
            token_usage_turn_id.filter(|id| thread.turns.iter().any(|turn| turn.id == *id));
        if let Some(name) = source_thread_name {
            set_thread_name_from_title(&mut thread, name);
        }
        thread.can_accept_direct_input = Some(can_accept_direct_input(
            forked_thread.multi_agent_version(),
            &config_snapshot.session_source,
        ));
        thread.session_id = session_configured.session_id.to_string();
        thread.thread_source = config_snapshot.thread_source.clone().map(Into::into);
        apply_live_thread_settings(&mut thread, &config_snapshot);
        if thread.path.is_none() {
            thread.project_id = inherited_project_id.clone();
        }

        self.thread_watch_manager
            .upsert_thread_silently(&thread.id)
            .await;

        thread.status = resolve_thread_status(
            self.thread_watch_manager
                .loaded_status_for_thread(&thread.id)
                .await,
            /*has_in_progress_turn*/ false,
        );
        let sandbox = config_snapshot.sandbox_policy().into();
        let active_permission_profile =
            thread_response_active_permission_profile(config_snapshot.active_permission_profile);
        let thread_originator = config_snapshot.originator.clone();
        let response = ThreadForkResponse {
            thread: thread.clone(),
            disabled_plugin_ids: config_snapshot.disabled_plugin_ids,
            model: config_snapshot.model.clone(),
            model_provider: session_configured.model_provider_id,
            service_tier: config_snapshot.service_tier.clone(),
            cwd: session_configured.cwd,
            runtime_workspace_roots: config_snapshot.workspace_roots,
            instruction_sources,
            approval_policy: session_configured.approval_policy.into(),
            approvals_reviewer: session_configured.approvals_reviewer.into(),
            sandbox,
            active_permission_profile,
            reasoning_effort: config_snapshot.reasoning_effort.clone(),
            multi_agent_mode: MultiAgentMode::ExplicitRequestOnly,
        };

        let notif = thread_started_notification(thread);
        let connection_id = request_id.connection_id;
        self.outgoing
            .send_response_with_thread_originator(request_id, response, thread_originator)
            .await;
        // `excludeTurns` is the cheap fork path, so skip restored usage replay
        // instead of rebuilding history only to attribute a historical update.
        if let Some(token_usage_turn_id) = token_usage_turn_id {
            // Mirror the resume contract for forks: the new thread is usable as soon
            // as the response arrives, so restored usage must follow immediately.
            send_thread_token_usage_update_to_connection(
                &self.outgoing,
                connection_id,
                thread_id,
                forked_thread.as_ref(),
                token_usage_turn_id,
            )
            .await;
        }

        if !attach_to_source {
            self.outgoing
                .send_server_notification(ServerNotification::ThreadStarted(notif))
                .await;
        }
        if inherited_goal {
            self.thread_goal_processor
                .emit_thread_goal_snapshot(thread_id)
                .await;
        }
        Ok(())
    }
}
