//! Resumes running or stored threads and loads their initial history.

use super::*;

impl ThreadRequestProcessor {
    pub(super) async fn thread_resume_inner(
        &self,
        target: &ThreadResumeTarget,
        params: &ThreadResumeParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
        prepared_config: &mut Option<PreparedResumeConfig>,
    ) -> Result<ControlFlow<()>, JSONRPCErrorError> {
        if let Ok(thread_id) = ThreadId::from_string(&params.thread_id)
            && self
                .pending_thread_unloads
                .lock()
                .await
                .contains_key(&thread_id)
        {
            return Err(thread_closing_for_resume(thread_id));
        }

        if params.sandbox.is_some() && params.permissions.is_some() {
            return Err(invalid_request(
                "`permissions` cannot be combined with `sandbox`",
            ));
        }
        let redact_resume_payloads =
            should_redact_thread_resume_payloads(app_server_client_name.as_deref());

        let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
        let stored_thread_from_running_probe = match target {
            ThreadResumeTarget::Client(request_id) => match self
                .resume_running_thread(
                    request_id,
                    params,
                    app_server_client_name.clone(),
                    app_server_client_version.clone(),
                    /*cold_resume_history*/ None,
                )
                .await?
            {
                RunningThreadResumeResult::Handled(completion) => {
                    // The listener may need this permit to finish the response.
                    drop(_thread_list_state_permit);
                    // A stopped listener can discard an accepted command before responding.
                    completion
                        .await
                        .map_err(|_| thread_closing_for_resume(&params.thread_id))?;
                    return Ok(ControlFlow::Break(()));
                }
                RunningThreadResumeResult::NotRunning(stored_thread) => stored_thread,
            },
            ThreadResumeTarget::DaemonRecovery(saved) => {
                let thread_id = ThreadId::from_string(&params.thread_id)
                    .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
                // Recheck under the same permit as client resume, including after config loading.
                if self.thread_manager.get_thread(thread_id).await.is_ok() {
                    if let Some(saved) = saved {
                        self.continue_daemon_turn(&params.thread_id, saved.clone())
                            .await;
                    }
                    return Ok(ControlFlow::Break(()));
                }
                None
            }
        };

        let ThreadResumeParams {
            thread_id,
            history,
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
            config: mut request_overrides,
            base_instructions,
            developer_instructions,
            personality,
            exclude_turns,
            initial_turns_page,
        } = params.clone();
        let include_turns = !exclude_turns;

        let prepared_model_context = prepared_config
            .as_mut()
            .and_then(|prepared| prepared.model_context.take());
        let (thread_history, resume_source_thread) = if let Some(history) = history {
            (
                self.resume_thread_from_history(history.as_slice()).await?,
                None,
            )
        } else {
            let stored_thread = match stored_thread_from_running_probe {
                Some(stored_thread) => *stored_thread,
                None => {
                    self.read_stored_thread_for_resume(
                        &thread_id,
                        path.as_ref(),
                        /*include_history*/ false,
                    )
                    .await?
                }
            };
            thread_resume_account::check(
                &self.config.features,
                app_server_client_name.as_deref(),
                &self.auth_manager,
                &stored_thread,
            )?;
            let (thread_history, stored_thread) = self
                .load_resume_initial_history_from_stored_thread(
                    stored_thread,
                    prepared_model_context,
                )
                .await?;
            (thread_history, Some(stored_thread))
        };
        // Path-based resume can use an empty request thread ID. Coordinate once its real
        // identity is known; unrelated loaded threads never wait for this cold startup.
        let _goal_resume_guard = if let InitialHistory::Resumed(resumed) = &thread_history {
            let guard = self
                .thread_state_manager
                .lock_goal_resume(resumed.conversation_id)
                .await;
            let state = self
                .thread_state_manager
                .thread_state(resumed.conversation_id)
                .await;
            let drain = state.lock().await.pending_shutdown_drain();
            if let Some(mut drain) = drain {
                // Core can remove the old runtime before its listener forwards ShutdownComplete.
                let _ = drain.wait_for(|drained| *drained).await;
            }
            Some(guard)
        } else {
            None
        };
        if let InitialHistory::Resumed(resumed) = &thread_history
            && self
                .pending_thread_unloads
                .lock()
                .await
                .contains_key(&resumed.conversation_id)
        {
            return Err(thread_closing_for_resume(resumed.conversation_id));
        }
        let paginated_thread_id = resume_source_thread.as_ref().and_then(|thread| {
            matches!(thread.history_mode, ThreadHistoryMode::Paginated).then_some(thread.thread_id)
        });
        let paginated_resume = paginated_thread_id.is_some();
        if paginated_resume
            && include_turns
            && prepared_config.is_none()
            && let ThreadResumeTarget::Client(request_id) = target
        {
            self.send_deprecation_notice(
                request_id.connection_id,
                PAGINATED_FULL_HISTORY_DEPRECATION_SUMMARY,
            )
            .await;
        }

        // Parent-owned V2 children must resume through their owner, not caller configuration.
        if let InitialHistory::Resumed(resumed_history) = &thread_history
            && let Some((source, _)) = thread_history.get_resumed_session_sources()
            && !can_accept_direct_input(thread_history.get_multi_agent_version(), &source)
        {
            let ThreadResumeTarget::Client(request_id) = target else {
                return Ok(ControlFlow::Break(()));
            };
            let child_thread_id = resumed_history.conversation_id;
            self.thread_manager
                .ensure_multi_agent_v2_child_loaded(child_thread_id)
                .await
                .map_err(|err| {
                    tracing::warn!(
                        thread_id = %child_thread_id,
                        error = %err,
                        "failed to resume a multi-agent v2 child through its parent"
                    );
                    invalid_request(
                        "cannot resume an unloaded multi-agent v2 sub-agent through its parent; resume the parent first, or use thread/read to inspect it",
                    )
                })?;

            let cold_resume_history = paginated_resume.then(|| thread_history.get_rollout_items());
            // Attach to the resolved child with only the caller's history-paging preferences.
            let attach_params = ThreadResumeParams {
                thread_id: child_thread_id.to_string(),
                exclude_turns,
                initial_turns_page,
                ..Default::default()
            };
            return match self
                .resume_running_thread(
                    request_id,
                    &attach_params,
                    app_server_client_name,
                    app_server_client_version,
                    cold_resume_history,
                )
                .await?
            {
                RunningThreadResumeResult::Handled(completion) => {
                    drop(_thread_list_state_permit);
                    completion
                        .await
                        .map_err(|_| thread_closing_for_resume(child_thread_id))?;
                    Ok(ControlFlow::Break(()))
                }
                RunningThreadResumeResult::NotRunning(_) => Err(invalid_request(
                    "cannot resume an unloaded multi-agent v2 sub-agent through its parent; resume the parent first, or use thread/read to inspect it",
                )),
            };
        }

        // Copied or referenced history can contain another thread's settings. Only snapshots
        // explicitly owned by this thread can restore its cwd and workspace folders.
        let history_settings = if let InitialHistory::Resumed(resumed) = &thread_history {
            latest_owned_thread_settings(&resumed.history, resumed.conversation_id)
        } else {
            None
        };
        let history_cwd = history_settings
            .map(|settings| settings.cwd.to_path_buf())
            .or_else(|| thread_history.session_cwd());
        let mut runtime_workspace_roots =
            runtime_workspace_roots.map(resolve_runtime_workspace_roots);
        if runtime_workspace_roots.is_none() {
            // A retained owned snapshot is authoritative. Only use startup metadata when
            // no such snapshot exists; missing roots do not resurrect an older selection.
            let saved_workspaces: Option<(&Path, Vec<&Path>)> = match history_settings {
                Some(settings) => settings.runtime_workspace_roots.as_ref().map(|roots| {
                    (
                        settings.cwd.as_path(),
                        roots.iter().map(AbsolutePathBuf::as_path).collect(),
                    )
                }),
                None => {
                    if let InitialHistory::Resumed(resumed) = &thread_history
                        && let Some(RolloutItem::SessionMeta(meta)) = resumed.history.first()
                        && meta.meta.id == resumed.conversation_id
                        && let Some(roots) = &meta.meta.runtime_workspace_roots
                    {
                        Some((
                            meta.meta.cwd.as_path(),
                            roots.iter().map(PathBuf::as_path).collect(),
                        ))
                    } else {
                        None
                    }
                }
            };
            if let Some((saved_cwd, saved_roots)) = saved_workspaces {
                let resume_cwd = resolve_request_cwd(cwd.as_ref().map(PathBuf::from))?;
                // Retarget before validation so an explicit cwd can replace a
                // startup cwd recorded on another operating system.
                let restored_roots = path_utils::replace_path_and_deduplicate(
                    saved_roots,
                    saved_cwd,
                    resume_cwd.as_ref().map_or(saved_cwd, AbsolutePathBuf::as_path),
                )
                    .into_iter()
                    .map(|root| {
                        AbsolutePathBuf::from_absolute_path_checked(root).map_err(|err| {
                            invalid_params(format!(
                                "cannot restore workspace root `{}` on this host: {err}. Pass `runtimeWorkspaceRoots` with valid local paths or an empty list.",
                                root.display()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                // Validation can normalize distinct saved paths to the same root.
                runtime_workspace_roots = Some(resolve_runtime_workspace_roots(restored_roots));
            }
        }
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
        if typesafe_overrides.approval_policy.is_none()
            && let Some(value) = request_overrides
                .as_mut()
                .and_then(|overrides| overrides.remove("approval_policy"))
        {
            let approval_policy = serde_json::from_value(value).map_err(|err| {
                invalid_params(format!("invalid `approval_policy` config override: {err}"))
            })?;
            typesafe_overrides.approval_policy = Some(approval_policy);
        }
        let has_explicit_model_resume_override =
            has_model_resume_override(request_overrides.as_ref(), &typesafe_overrides);
        let persisted_model_settings = latest_persisted_model_settings(&thread_history);
        let has_override = |key| {
            request_overrides
                .as_ref()
                .is_some_and(|overrides| overrides.contains_key(key))
        };
        let restore_service_tier =
            typesafe_overrides.service_tier.is_none() && !has_override("service_tier");
        let restore_summary = !has_override("model_reasoning_summary");
        let restore_personality =
            typesafe_overrides.personality.is_none() && !has_override("personality");
        let persisted_settings = match &thread_history {
            InitialHistory::Resumed(resumed) => {
                PersistedResumeSettings::from_history(&resumed.history, resumed.conversation_id)
            }
            InitialHistory::New | InitialHistory::Cleared | InitialHistory::Forked(_) => None,
        };
        let persisted_metadata = self
            .load_and_apply_persisted_resume_metadata(
                &thread_history,
                persisted_settings.as_ref(),
                &mut request_overrides,
                &mut typesafe_overrides,
            )
            .await;

        let clear_reasoning_effort = !has_explicit_model_resume_override
            && persisted_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.reasoning_effort.is_none());
        let config_state = ResumeConfigState {
            history_cwd: history_cwd.clone(),
            workspace_roots: typesafe_overrides.workspace_roots.clone(),
            persisted_metadata,
            persisted_settings,
        };
        let mut config = match prepared_config.take() {
            Some(prepared) if prepared.state == config_state => prepared.config,
            _ => {
                // Config loading can call back into Desktop; release both locks during host work.
                drop(_goal_resume_guard);
                drop(_thread_list_state_permit);
                let config = self
                    .config_manager
                    .load_for_resume(
                        resume_source_thread
                            .as_ref()
                            .map(|thread| thread.model_provider.as_str())
                            .filter(|provider| !provider.is_empty())
                            .or_else(|| {
                                config_state
                                    .persisted_metadata
                                    .as_ref()
                                    .map(|metadata| metadata.model_provider.as_str())
                            })
                            .or_else(|| match &thread_history {
                                InitialHistory::Resumed(resumed) => {
                                    resumed.history.iter().find_map(|item| match item {
                                        RolloutItem::SessionMeta(meta) => {
                                            meta.meta.model_provider.as_deref()
                                        }
                                        _ => None,
                                    })
                                }
                                _ => None,
                            }),
                        request_overrides,
                        typesafe_overrides,
                        history_cwd,
                    )
                    .await
                    .map_err(|err| config_load_error(&err))?;
                *prepared_config = Some(PreparedResumeConfig {
                    state: config_state,
                    config,
                    model_context: if paginated_resume
                        && let InitialHistory::Resumed(history) = thread_history
                    {
                        Some(history)
                    } else {
                        None
                    },
                });
                // Recheck the thread and history after a possible archive, delete, or aliased resume.
                return Ok(ControlFlow::Continue(()));
            }
        };
        if clear_reasoning_effort {
            config.model_reasoning_effort = None;
        }
        if let Some(settings) = persisted_model_settings {
            if restore_service_tier {
                config.service_tier.clone_from(&settings.service_tier);
            }
            if restore_summary {
                config.model_reasoning_summary = settings.reasoning_summary;
            }
            if restore_personality {
                config.personality = settings.personality;
            }
        }
        let response_history = thread_history.clone();

        match self
            .thread_manager
            .start_thread(StartThreadOptions {
                initial_history: thread_history,
                parent_trace: match target {
                    ThreadResumeTarget::Client(request_id) => {
                        self.request_trace_context(request_id).await
                    }
                    ThreadResumeTarget::DaemonRecovery(_) => None,
                },
                client_mcp_extensions,
                ..StartThreadOptions::new(config)
            })
            .await
        {
            Ok(NewThread {
                thread_id,
                thread: codex_thread,
                session_configured,
                ..
            }) => {
                let ThreadResumeTarget::Client(request_id) = target else {
                    // Observe lifecycle events without attaching a client subscription.
                    self.thread_watch_manager
                        .upsert_thread(&thread_id.to_string())
                        .await;
                    // Invoke idle work before arming subscriber-based unloading.
                    codex_thread
                        .emit_thread_idle_lifecycle_if_idle(ThreadIdleCause::Completed)
                        .await;
                    if let ThreadResumeTarget::DaemonRecovery(Some(saved)) = target {
                        self.continue_daemon_turn(&thread_id.to_string(), saved.clone())
                            .await;
                    }
                    let state = self.thread_state_manager.thread_state(thread_id).await;
                    self.ensure_listener_task_running(thread_id, Arc::clone(&codex_thread), state)
                        .await?;
                    return Ok(ControlFlow::Break(()));
                };
                let request_id = request_id.clone();
                if let Err(err) = Self::set_app_server_client_info(
                    codex_thread.as_ref(),
                    app_server_client_name,
                    app_server_client_version,
                )
                .await
                {
                    self.outgoing.send_error(request_id, err).await;
                    return Ok(ControlFlow::Break(()));
                }
                let instruction_sources = codex_thread.legacy_instruction_sources().await;
                let SessionConfiguredEvent { rollout_path, .. } = session_configured;
                let Some(rollout_path) = rollout_path else {
                    let error =
                        internal_error(format!("rollout path missing for thread {thread_id}"));
                    self.outgoing.send_error(request_id, error).await;
                    return Ok(ControlFlow::Break(()));
                };
                // Paginated JSONL is canonical, but its SQLite projection can lag after a
                // previous write failure. Persist after reopening the live writer so legacy
                // response hydration reads the latest durable turns and items.
                if paginated_resume
                    && let Err(error) = self
                        .thread_store
                        .persist_thread(thread_id, PersistContext::Standard)
                        .await
                        .map_err(thread_store_resume_read_error)
                {
                    self.outgoing.send_error(request_id, error).await;
                    return Ok(ControlFlow::Break(()));
                }
                let materialized_turns = if paginated_resume && include_turns {
                    match self.paginated_thread_full_turns(thread_id).await {
                        Ok(turns) => Some(turns),
                        Err(error) => {
                            self.outgoing.send_error(request_id, error).await;
                            return Ok(ControlFlow::Break(()));
                        }
                    }
                } else {
                    None
                };
                // Auto-attach a thread listener when resuming a thread.
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

                let mut thread = match self
                    .load_thread_from_resume_source_or_send_internal(
                        thread_id,
                        codex_thread.as_ref(),
                        &response_history,
                        rollout_path.as_path(),
                        resume_source_thread,
                        include_turns && !paginated_resume,
                    )
                    .await
                {
                    Ok(thread) => thread,
                    Err(message) => {
                        self.outgoing
                            .send_error(request_id, internal_error(message))
                            .await;
                        return Ok(ControlFlow::Break(()));
                    }
                };
                thread.thread_source = codex_thread
                    .config_snapshot()
                    .await
                    .thread_source
                    .map(Into::into);
                if let Some(materialized_turns) = materialized_turns {
                    thread.turns = materialized_turns;
                }

                self.thread_watch_manager.upsert_thread(&thread.id).await;

                let thread_status = self
                    .thread_watch_manager
                    .loaded_status_for_thread(&thread.id)
                    .await;

                set_thread_status_and_interrupt_stale_turns(
                    &mut thread,
                    thread_status,
                    /*has_live_in_progress_turn*/ false,
                );
                let config_snapshot = codex_thread.config_snapshot().await;
                let (turns_backwards_cursor, items_backwards_cursor) =
                    if matches!(config_snapshot.history_mode, ThreadHistoryMode::Paginated) {
                        match Self::paginated_resume_backwards_cursors(
                            self.thread_store.as_ref(),
                            thread_id,
                        )
                        .await
                        {
                            Ok(cursors) => cursors,
                            Err(error) => {
                                self.outgoing.send_error(request_id, error).await;
                                return Ok(ControlFlow::Break(()));
                            }
                        }
                    } else {
                        (None, None)
                    };
                let sandbox = config_snapshot.sandbox_policy().into();
                let active_permission_profile = thread_response_active_permission_profile(
                    config_snapshot.active_permission_profile,
                );
                let mut initial_turns_page = if let Some(params) = initial_turns_page.as_ref() {
                    let initial_turns_page_result = if paginated_resume {
                        self.paginated_resume_initial_turns_page(thread_id, params)
                            .await
                    } else {
                        build_thread_resume_initial_turns_page(
                            response_history.get_rollout_items(),
                            thread.status.clone(),
                            /*has_live_running_thread*/ false,
                            /*active_turn*/ None,
                            params,
                        )
                    };
                    match initial_turns_page_result {
                        Ok(page) => Some(page),
                        Err(error) => {
                            self.outgoing.send_error(request_id, error).await;
                            return Ok(ControlFlow::Break(()));
                        }
                    }
                } else {
                    None
                };
                let token_usage_turn_id = (include_turns || paginated_resume)
                    .then(|| {
                        let turns = if thread.turns.is_empty() {
                            initial_turns_page
                                .as_ref()
                                .map_or(&[][..], |page| page.data.as_slice())
                        } else {
                            thread.turns.as_slice()
                        };
                        restored_token_usage_turn_id(response_history.get_rollout_items(), turns)
                    })
                    .filter(|turn_id| !turn_id.is_empty())
                    .filter(|_| !paginated_resume || thread.forked_from_id.is_none());
                if redact_resume_payloads {
                    redact_thread_resume_payloads(&mut thread.turns);
                    if let Some(initial_turns_page) = initial_turns_page.as_mut() {
                        redact_thread_resume_payloads(&mut initial_turns_page.data);
                    }
                }

                let thread_originator = config_snapshot.originator.clone();
                let response = ThreadResumeResponse {
                    thread,
                    disabled_plugin_ids: config_snapshot.disabled_plugin_ids,
                    model: session_configured.model,
                    model_provider: session_configured.model_provider_id,
                    service_tier: session_configured.service_tier,
                    cwd: session_configured.cwd,
                    runtime_workspace_roots: config_snapshot.workspace_roots,
                    instruction_sources,
                    approval_policy: session_configured.approval_policy.into(),
                    approvals_reviewer: session_configured.approvals_reviewer.into(),
                    sandbox,
                    active_permission_profile,
                    reasoning_effort: session_configured.reasoning_effort,
                    collaboration_mode: Some(config_snapshot.collaboration_mode),
                    multi_agent_mode: MultiAgentMode::ExplicitRequestOnly,
                    initial_turns_page,
                    turns_backwards_cursor,
                    items_backwards_cursor,
                };

                let connection_id = request_id.connection_id;
                self.outgoing
                    .send_response_with_thread_originator(request_id, response, thread_originator)
                    .await;
                // `excludeTurns` is explicitly the cheap resume path, so avoid
                // rebuilding history only to attribute a replayed usage update.
                if let Some(token_usage_turn_id) = token_usage_turn_id {
                    // The client needs restored usage before it starts another turn.
                    // Sending after the response preserves JSON-RPC request ordering while
                    // still filling the status line before the next turn lifecycle begins.
                    send_thread_token_usage_update_to_connection(
                        &self.outgoing,
                        connection_id,
                        thread_id,
                        codex_thread.as_ref(),
                        token_usage_turn_id,
                    )
                    .await;
                }
                self.thread_goal_processor
                    .emit_resume_goal_snapshot(thread_id)
                    .await;
                codex_thread
                    .emit_thread_idle_lifecycle_if_idle(ThreadIdleCause::Completed)
                    .await;
            }
            Err(err) => {
                let error = match err.details() {
                    CodexErrorDetails::InvalidRequest(message) => invalid_request(message.clone()),
                    _ => internal_error(format!("error resuming thread: {err}")),
                };
                return Err(error);
            }
        }
        Ok(ControlFlow::Break(()))
    }

    async fn load_and_apply_persisted_resume_metadata(
        &self,
        thread_history: &InitialHistory,
        persisted_settings: Option<&PersistedResumeSettings>,
        request_overrides: &mut Option<HashMap<String, serde_json::Value>>,
        typesafe_overrides: &mut ConfigOverrides,
    ) -> Option<ThreadMetadata> {
        let InitialHistory::Resumed(resumed_history) = thread_history else {
            return None;
        };
        if let Some(persisted_settings) = persisted_settings {
            if typesafe_overrides.approval_policy.is_none() {
                typesafe_overrides.approval_policy = Some(persisted_settings.approval_policy);
            }
            if typesafe_overrides.approvals_reviewer.is_none()
                && !request_overrides
                    .as_ref()
                    .is_some_and(|overrides| overrides.contains_key("approvals_reviewer"))
            {
                typesafe_overrides.approvals_reviewer = persisted_settings.approvals_reviewer;
            }
            apply_persisted_permission_profile(
                persisted_settings,
                request_overrides.as_ref(),
                typesafe_overrides,
            );
        }
        let state_db_ctx = self.state_db.clone()?;
        let persisted_metadata = state_db_ctx
            .get_thread(resumed_history.conversation_id)
            .await
            .ok()
            .flatten()?;
        merge_persisted_resume_metadata(request_overrides, typesafe_overrides, &persisted_metadata);
        Some(persisted_metadata)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "listener state creation must be serialized against terminal listener cleanup"
    )]
    #[tracing::instrument(level = "trace", skip_all)]
    async fn resume_running_thread(
        &self,
        request_id: &ConnectionRequestId,
        params: &ThreadResumeParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        cold_resume_history: Option<&[RolloutItem]>,
    ) -> Result<RunningThreadResumeResult, JSONRPCErrorError> {
        let running_thread = if params.history.is_some() {
            if let Ok(existing_thread_id) = ThreadId::from_string(&params.thread_id)
                && self
                    .thread_manager
                    .get_thread(existing_thread_id)
                    .await
                    .is_ok()
            {
                return Err(invalid_request(format!(
                    "cannot resume thread {existing_thread_id} with history while it is already running"
                )));
            }
            None
        } else if let Ok(existing_thread_id) = ThreadId::from_string(&params.thread_id)
            && let Ok(existing_thread) = self.thread_manager.get_thread(existing_thread_id).await
        {
            let source_thread = self
                .read_stored_thread_for_resume(
                    &params.thread_id,
                    /*path*/ None,
                    /*include_history*/ false,
                )
                .await?;
            Some((existing_thread_id, existing_thread, source_thread))
        } else {
            let source_thread = self
                .read_stored_thread_for_resume(
                    &params.thread_id,
                    params.path.as_ref(),
                    /*include_history*/ false,
                )
                .await?;
            let existing_thread_id = source_thread.thread_id;
            match self.thread_manager.get_thread(existing_thread_id).await {
                Ok(existing_thread) => Some((existing_thread_id, existing_thread, source_thread)),
                Err(_) => {
                    return Ok(RunningThreadResumeResult::NotRunning(Some(Box::new(
                        source_thread,
                    ))));
                }
            }
        };

        if let Some((existing_thread_id, existing_thread, mut source_thread)) = running_thread {
            thread_resume_account::check(
                &self.config.features,
                app_server_client_name.as_deref(),
                &self.auth_manager,
                &source_thread,
            )?;
            let paginated_resume =
                matches!(source_thread.history_mode, ThreadHistoryMode::Paginated);
            let existing_thread_rollout_path = existing_thread.rollout_path();
            let active_path = existing_thread_rollout_path
                .as_ref()
                .or(source_thread.rollout_path.as_ref());
            if let (Some(requested_path), Some(active_path)) = (params.path.as_ref(), active_path)
                && !path_utils::paths_match_after_normalization(requested_path, active_path)
            {
                return Err(invalid_request(format!(
                    "cannot resume running thread {existing_thread_id} with stale path: requested `{}`, active `{}`",
                    requested_path.display(),
                    active_path.display()
                )));
            }
            let (config_snapshot, selected_service_tier) =
                existing_thread.resume_config_snapshot().await;
            let mismatch_details = collect_resume_override_mismatches(
                params,
                &config_snapshot,
                selected_service_tier.as_deref(),
            );
            if !mismatch_details.is_empty() {
                let has_subscribers = !self
                    .thread_state_manager
                    .subscribed_connection_ids(existing_thread_id)
                    .await
                    .is_empty();
                let loaded_status = self
                    .thread_watch_manager
                    .loaded_status_for_thread(&existing_thread_id.to_string())
                    .await;
                let is_running =
                    matches!(existing_thread.agent_status().await, AgentStatus::Running);

                // Parent-owned V2 children must not be rebuilt from public resume overrides.
                if can_accept_direct_input(
                    existing_thread.multi_agent_version(),
                    &config_snapshot.session_source,
                ) && !has_subscribers
                    && matches!(loaded_status, ThreadStatus::Idle)
                    && !is_running
                {
                    // A loaded idle thread is only a cache entry. Shut it down
                    // before removing it so cold resume cannot duplicate a
                    // thread that timed out during shutdown.
                    match wait_for_thread_shutdown(&existing_thread).await {
                        ThreadShutdownResult::Complete => {
                            self.thread_manager.remove_thread(&existing_thread_id).await;
                            self.finalize_thread_teardown(existing_thread_id).await;
                            // Shutdown can flush newer rollout items, so reload the
                            // stored thread before starting the replacement session.
                            return Ok(RunningThreadResumeResult::NotRunning(None));
                        }
                        ThreadShutdownResult::SubmitFailed => {
                            warn!("failed to submit Shutdown to thread {existing_thread_id}");
                        }
                        ThreadShutdownResult::TimedOut => {
                            warn!("thread {existing_thread_id} shutdown timed out");
                        }
                    }
                }

                // Preserve rejoin semantics when another client can still observe
                // the loaded thread or shutdown did not complete.
                tracing::warn!(
                    "thread/resume overrides ignored for loaded thread {}: {}",
                    existing_thread_id,
                    mismatch_details.join("; ")
                );
            }
            self.config_manager
                .check_thread_model_provider(existing_thread.config().await.as_ref())
                .await
                .map_err(|err| config_load_error(&err))?;
            let redact_resume_payloads =
                should_redact_thread_resume_payloads(app_server_client_name.as_deref());
            let include_turns = !params.exclude_turns;
            if paginated_resume && include_turns {
                self.send_deprecation_notice(
                    request_id.connection_id,
                    PAGINATED_FULL_HISTORY_DEPRECATION_SUMMARY,
                )
                .await;
            }
            let needs_history =
                !paginated_resume && (include_turns || params.initial_turns_page.is_some());
            if needs_history {
                let source_thread_id = source_thread.thread_id.to_string();
                let source_rollout_path = source_thread.rollout_path.clone();
                source_thread = self
                    .read_stored_thread_for_resume(
                        &source_thread_id,
                        source_rollout_path.as_ref(),
                        /*include_history*/ true,
                    )
                    .await?;
            }
            if paginated_resume && (include_turns || params.initial_turns_page.is_some()) {
                self.thread_store
                    .persist_thread(existing_thread_id, PersistContext::Standard)
                    .await
                    .map_err(thread_store_resume_read_error)?;
            }
            let history_items = if needs_history {
                source_thread
                    .history
                    .take()
                    .map(|history| history.items)
                    .ok_or_else(|| {
                        internal_error(format!(
                            "thread {existing_thread_id} did not include persisted history"
                        ))
                    })?
            } else {
                Vec::new()
            };

            let thread_state = {
                let pending = self.pending_thread_unloads.lock().await;
                if pending.contains_key(&existing_thread_id)
                    || matches!(existing_thread.agent_status().await, AgentStatus::Shutdown)
                {
                    return Err(thread_closing_for_resume(existing_thread_id));
                }
                self.thread_state_manager
                    .thread_state(existing_thread_id)
                    .await
            };
            self.ensure_listener_task_running(
                existing_thread_id,
                existing_thread.clone(),
                thread_state.clone(),
            )
            .await?;
            Self::set_app_server_client_info(
                existing_thread.as_ref(),
                app_server_client_name,
                app_server_client_version,
            )
            .await?;

            let mut thread_summary = self.stored_thread_to_api_thread(
                source_thread,
                config_snapshot.model_provider_id.as_str(),
                /*include_turns*/ false,
            );
            thread_summary.session_id = existing_thread.startup_metadata().session_id.to_string();
            thread_summary.thread_source = config_snapshot.thread_source.clone().map(Into::into);
            apply_live_thread_settings(&mut thread_summary, &config_snapshot);
            thread_summary.can_accept_direct_input = Some(can_accept_direct_input(
                existing_thread.multi_agent_version(),
                &config_snapshot.session_source,
            ));
            let instruction_sources = existing_thread.legacy_instruction_sources().await;

            let listener_command_tx = {
                let thread_state = thread_state.lock().await;
                thread_state.listener_command_tx()
            };
            let Some(listener_command_tx) = listener_command_tx else {
                return Err(thread_closing_for_resume(existing_thread_id));
            };

            let (emit_thread_goal_update, thread_goal_state_db) = self
                .thread_goal_processor
                .pending_resume_goal_state(existing_thread.as_ref())
                .await;
            let paginated_turns = if paginated_resume && include_turns {
                Some(self.paginated_thread_full_turns(existing_thread_id).await?)
            } else {
                None
            };
            let paginated_initial_turns_page = if paginated_resume {
                match params.initial_turns_page.as_ref() {
                    Some(params) => Some(
                        self.paginated_resume_initial_turns_page(existing_thread_id, params)
                            .await?,
                    ),
                    None => None,
                }
            } else {
                None
            };
            let cold_resume_token_usage_turn_id = cold_resume_history
                .map(|history| {
                    let turns = paginated_turns
                        .as_deref()
                        .filter(|turns| !turns.is_empty())
                        .unwrap_or_else(|| {
                            paginated_initial_turns_page
                                .as_ref()
                                .map_or(&[][..], |page| page.data.as_slice())
                        });
                    restored_token_usage_turn_id(history, turns)
                })
                .filter(|turn_id| !turn_id.is_empty())
                .filter(|_| !paginated_resume || thread_summary.forked_from_id.is_none());
            let paginated_initial_turns_page_with_active_slot = if paginated_resume {
                match params.initial_turns_page.as_ref() {
                    Some(params)
                        if matches!(
                            params.sort_direction.unwrap_or(SortDirection::Desc),
                            SortDirection::Desc
                        ) =>
                    {
                        Some(
                            self.paginated_resume_initial_turns_page_with_active_slot(
                                existing_thread_id,
                                params,
                            )
                            .await?,
                        )
                    }
                    Some(_) | None => None,
                }
            } else {
                None
            };
            let resume_cursor_store = paginated_resume.then(|| Arc::clone(&self.thread_store));

            let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
            let command = crate::thread_state::ThreadListenerCommand::SendThreadResumeResponse {
                request: Box::new(crate::thread_state::PendingThreadResumeRequest {
                    request_id: request_id.clone(),
                    history_items,
                    cold_resume_token_usage_turn_id,
                    config_snapshot,
                    instruction_sources,
                    thread_summary,
                    emit_thread_goal_update,
                    thread_goal_state_db,
                    include_turns,
                    initial_turns_page: params.initial_turns_page.clone(),
                    paginated_turns,
                    paginated_initial_turns_page,
                    paginated_initial_turns_page_with_active_slot,
                    resume_cursor_store,
                    redact_resume_payloads,
                }),
                completion_tx,
            };
            if listener_command_tx.send(command).is_err() {
                return Err(thread_closing_for_resume(existing_thread_id));
            }
            return Ok(RunningThreadResumeResult::Handled(completion_rx));
        }
        Ok(RunningThreadResumeResult::NotRunning(None))
    }

    #[tracing::instrument(level = "trace", skip_all)]
    async fn resume_thread_from_history(
        &self,
        history: &[ResponseItem],
    ) -> Result<InitialHistory, JSONRPCErrorError> {
        if history.is_empty() {
            return Err(invalid_request("history must not be empty"));
        }
        Ok(InitialHistory::Forked(
            history
                .iter()
                .cloned()
                .map(|item| RolloutItem::ResponseItem(item.into()))
                .collect(),
        ))
    }

    pub(super) async fn load_resume_initial_history_from_stored_thread(
        &self,
        stored_thread: StoredThread,
        prepared_model_context: Option<ResumedHistory>,
    ) -> Result<(InitialHistory, StoredThread), JSONRPCErrorError> {
        if matches!(stored_thread.history_mode, ThreadHistoryMode::Paginated) {
            if let Some(context) = prepared_model_context
                && context.conversation_id == stored_thread.thread_id
                && context.rollout_path == stored_thread.rollout_path
                && let Some(revision) = context.history_revision.as_ref()
                && self
                    .thread_store
                    .is_model_context_current(
                        StoreLoadThreadHistoryParams {
                            thread_id: stored_thread.thread_id,
                            include_archived: true,
                        },
                        revision.clone(),
                    )
                    .await
                    .unwrap_or(false)
            {
                return Ok((InitialHistory::Resumed(context), stored_thread));
            }
            let model_context = self
                .thread_store
                .load_latest_model_context(StoreLoadThreadHistoryParams {
                    thread_id: stored_thread.thread_id,
                    include_archived: true,
                })
                .await
                .map_err(thread_store_resume_read_error)?;
            let history = InitialHistory::Resumed(ResumedHistory {
                history_revision: model_context.revision,
                conversation_id: model_context.thread_id,
                history: Arc::new(model_context.items),
                rollout_path: stored_thread.rollout_path.clone(),
            });
            return Ok((history, stored_thread));
        }

        drop(prepared_model_context);
        let thread_id = stored_thread.thread_id.to_string();
        let rollout_path = stored_thread.rollout_path.clone();
        let mut stored_thread = self
            .read_stored_thread_for_resume(
                &thread_id,
                rollout_path.as_ref(),
                /*include_history*/ true,
            )
            .await?;
        let history = self
            .stored_thread_to_initial_history(&mut stored_thread)
            .await?;
        Ok((history, stored_thread))
    }

    pub(super) async fn read_stored_thread_for_resume(
        &self,
        thread_id: &str,
        path: Option<&PathBuf>,
        include_history: bool,
    ) -> Result<StoredThread, JSONRPCErrorError> {
        let result = if let Some(path) = path {
            self.thread_store
                .read_thread_by_rollout_path(StoreReadThreadByRolloutPathParams {
                    rollout_path: path.clone(),
                    include_archived: true,
                    include_history,
                })
                .await
        } else {
            let existing_thread_id = match ThreadId::from_string(thread_id) {
                Ok(id) => id,
                Err(err) => {
                    return Err(invalid_request(format!("invalid session id: {err}")));
                }
            };
            let params = StoreReadThreadParams {
                thread_id: existing_thread_id,
                include_archived: true,
                include_history,
            };
            self.thread_store.read_thread(params).await
        };

        let stored_thread = result.map_err(thread_store_resume_read_error)?;
        if let Some(requested_path) = path
            && matches!(stored_thread.history_mode, ThreadHistoryMode::Paginated)
        {
            let current_thread = self
                .thread_store
                .read_thread(StoreReadThreadParams {
                    thread_id: stored_thread.thread_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
                .map_err(thread_store_resume_read_error)?;
            if let Some(current_path) = current_thread.rollout_path.as_ref()
                && !path_utils::paths_match_after_normalization(
                    codex_rollout::plain_rollout_path(requested_path).as_path(),
                    codex_rollout::plain_rollout_path(current_path).as_path(),
                )
            {
                return Err(invalid_request(format!(
                    "cannot resume paginated thread {} with stale path: requested {}, current {}; omit path and resume by thread id",
                    stored_thread.thread_id,
                    requested_path.display(),
                    current_path.display()
                )));
            }
        }
        if stored_thread.archived_at.is_some() {
            let thread_id = stored_thread.thread_id;
            return Err(invalid_request(format!(
                "session {thread_id} is archived. Run `codex unarchive {thread_id}` to unarchive it first."
            )));
        }

        Ok(stored_thread)
    }

    #[tracing::instrument(level = "trace", skip_all)]
    async fn stored_thread_to_initial_history(
        &self,
        stored_thread: &mut StoredThread,
    ) -> Result<InitialHistory, JSONRPCErrorError> {
        let thread_id = stored_thread.thread_id;
        let history = stored_thread.history.take().ok_or_else(|| {
            internal_error(format!(
                "thread {thread_id} did not include persisted history"
            ))
        })?;
        Ok(InitialHistory::Resumed(ResumedHistory {
            history_revision: history.revision,
            conversation_id: thread_id,
            history: Arc::new(history.items),
            rollout_path: stored_thread.rollout_path.clone(),
        }))
    }

    fn stored_thread_to_api_thread(
        &self,
        stored_thread: StoredThread,
        fallback_provider: &str,
        include_turns: bool,
    ) -> Thread {
        let (mut thread, history) =
            thread_from_stored_thread(stored_thread, fallback_provider, &self.config.cwd);
        if include_turns && let Some(history) = history {
            populate_thread_turns_from_history(
                &mut thread,
                &history.items,
                /*active_turn*/ None,
            );
        }
        thread
    }

    pub(super) async fn read_stored_thread_for_new_fork(
        &self,
        thread_id: ThreadId,
        include_history: bool,
    ) -> Result<StoredThread, JSONRPCErrorError> {
        self.thread_store
            .read_thread(StoreReadThreadParams {
                thread_id,
                include_archived: true,
                include_history,
            })
            .await
            .map_err(thread_store_resume_read_error)
    }

    pub(super) async fn load_thread_from_resume_source_or_send_internal(
        &self,
        thread_id: ThreadId,
        thread: &CodexThread,
        thread_history: &InitialHistory,
        rollout_path: &Path,
        resume_source_thread: Option<StoredThread>,
        include_turns: bool,
    ) -> std::result::Result<Thread, String> {
        let config_snapshot = thread.config_snapshot().await;
        let session_id = thread.startup_metadata().session_id.to_string();
        let can_accept_direct_input = can_accept_direct_input(
            thread.multi_agent_version(),
            &config_snapshot.session_source,
        );
        let thread = match thread_history {
            InitialHistory::Resumed(resumed) => {
                let fallback_provider = config_snapshot.model_provider_id.as_str();
                if let Some(stored_thread) = resume_source_thread {
                    let stored_thread =
                        if let Some(rollout_path) = stored_thread.rollout_path.clone() {
                            self.thread_store
                                .read_thread_by_rollout_path(StoreReadThreadByRolloutPathParams {
                                    rollout_path,
                                    include_archived: true,
                                    include_history: false,
                                })
                                .await
                                .unwrap_or(StoredThread {
                                    history: None,
                                    ..stored_thread
                                })
                        } else {
                            self.thread_store
                                .read_thread(StoreReadThreadParams {
                                    thread_id: stored_thread.thread_id,
                                    include_archived: true,
                                    include_history: false,
                                })
                                .await
                                .unwrap_or(StoredThread {
                                    history: None,
                                    ..stored_thread
                                })
                        };
                    Ok(thread_from_stored_thread(
                        stored_thread,
                        fallback_provider,
                        &self.config.cwd,
                    )
                    .0)
                } else {
                    match self
                        .thread_store
                        .read_thread(StoreReadThreadParams {
                            thread_id: resumed.conversation_id,
                            include_archived: true,
                            include_history: false,
                        })
                        .await
                    {
                        Ok(stored_thread) => Ok(thread_from_stored_thread(
                            stored_thread,
                            fallback_provider,
                            &self.config.cwd,
                        )
                        .0),
                        Err(read_err) => {
                            Err(format!("failed to read thread from store: {read_err}"))
                        }
                    }
                }
            }
            InitialHistory::Forked(items) => {
                let mut thread = build_thread_from_snapshot(
                    thread_id,
                    session_id.clone(),
                    thread.multi_agent_version(),
                    &config_snapshot,
                    Some(rollout_path.into()),
                );
                thread.preview = preview_from_rollout_items(items);
                Ok(thread)
            }
            InitialHistory::New | InitialHistory::Cleared => Err(format!(
                "failed to build resume response for thread {thread_id}: initial history missing"
            )),
        };
        let mut thread = thread?;
        apply_live_thread_settings(&mut thread, &config_snapshot);
        thread.can_accept_direct_input = Some(can_accept_direct_input);
        thread.id = thread_id.to_string();
        thread.session_id = session_id;
        thread.path = Some(rollout_path.to_path_buf());
        if include_turns {
            let history_items = thread_history.get_rollout_items();
            populate_thread_turns_from_history(
                &mut thread,
                history_items,
                /*active_turn*/ None,
            );
        }
        self.attach_thread_name(thread_id, &mut thread).await;
        Ok(thread)
    }

    pub(super) async fn attach_thread_name(&self, thread_id: ThreadId, thread: &mut Thread) {
        if let Ok(stored_thread) = self
            .thread_store
            .read_thread(StoreReadThreadParams {
                thread_id,
                include_archived: true,
                include_history: false,
            })
            .await
            && let Some(title) = stored_thread.name.as_deref().map(str::trim)
            && !title.is_empty()
        {
            if stored_thread.history_mode == ThreadHistoryMode::Paginated {
                thread.name = Some(title.to_string());
            } else {
                set_thread_name_from_title(thread, title.to_string());
            }
        }
    }
}
