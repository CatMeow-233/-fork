//! Archive, rename, metadata, memory, revert, compact, and shell requests on existing threads.

use super::*;

impl ThreadRequestProcessor {
    pub(super) async fn thread_archive_response(
        &self,
        params: ThreadArchiveParams,
    ) -> Result<(ThreadArchiveResponse, Vec<String>), JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(format!("invalid session id: {err}")))?;

        let subtree_thread_ids = self.state_db_spawn_subtree_thread_ids(thread_id).await?;

        // Fresh threads have no rollout until their first turn. Materialize the
        // loaded persistent thread before looking it up for archival.
        if let Ok(thread) = self.thread_manager.get_thread(thread_id).await
            && !thread.config_snapshot().await.ephemeral
        {
            self.thread_store
                .persist_thread(thread_id, PersistContext::Standard)
                .await
                .map_err(|err| thread_store_mutation_error("archive", err))?;
        }

        let mut archive_thread_ids = Vec::new();
        match self
            .thread_store
            .read_thread(StoreReadThreadParams {
                thread_id,
                include_archived: false,
                include_history: false,
            })
            .await
        {
            Ok(thread) => {
                if thread.archived_at.is_none() {
                    archive_thread_ids.push(thread_id);
                }
            }
            Err(err) => return Err(thread_store_mutation_error("archive", err)),
        }
        for descendant_thread_id in subtree_thread_ids.iter().copied().skip(1) {
            match self
                .thread_store
                .read_thread(StoreReadThreadParams {
                    thread_id: descendant_thread_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
            {
                Ok(thread) => {
                    if thread.archived_at.is_none() {
                        archive_thread_ids.push(descendant_thread_id);
                    }
                }
                Err(err) => {
                    warn!(
                        "failed to read spawned descendant thread {descendant_thread_id} while archiving {thread_id}: {err}"
                    );
                }
            }
        }

        if archive_thread_ids.is_empty() {
            return Ok((ThreadArchiveResponse {}, Vec::new()));
        }

        archive_thread_ids[1..].reverse();
        // Collaboration may resume an archived descendant without unarchiving it.
        self.prepare_thread_for_archive(thread_id).await?;
        for &descendant_thread_id in subtree_thread_ids.iter().skip(1).rev() {
            self.prepare_thread_for_archive(descendant_thread_id)
                .await?;
        }

        let archived_thread_ids = self
            .thread_store
            .archive_threads(StoreArchiveThreadsParams {
                thread_ids: archive_thread_ids,
                writer_lock_thread_ids: subtree_thread_ids,
            })
            .await
            .map_err(|err| thread_store_mutation_error("archive", err))?
            .into_iter()
            .map(|thread_id| thread_id.to_string())
            .collect();
        Ok((ThreadArchiveResponse {}, archived_thread_ids))
    }

    pub(in crate::request_processors) async fn state_db_spawn_subtree_thread_ids(
        &self,
        thread_id: ThreadId,
    ) -> Result<Vec<ThreadId>, JSONRPCErrorError> {
        self.thread_manager
            .list_agent_subtree_thread_ids(thread_id)
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to list spawned descendants for thread id {thread_id}: {err}"
                ))
            })
    }

    pub(super) async fn thread_increment_elicitation_inner(
        &self,
        params: ThreadIncrementElicitationParams,
    ) -> Result<ThreadIncrementElicitationResponse, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        let count = thread
            .increment_out_of_band_elicitation_count()
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to increment out-of-band elicitation counter: {err}"
                ))
            })?;
        Ok(ThreadIncrementElicitationResponse {
            count,
            paused: count > 0,
        })
    }

    pub(super) async fn thread_decrement_elicitation_inner(
        &self,
        params: ThreadDecrementElicitationParams,
    ) -> Result<ThreadDecrementElicitationResponse, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        let count = thread
            .decrement_out_of_band_elicitation_count()
            .await
            .map_err(|err| match err.details() {
                CodexErrorDetails::InvalidRequest(message) => invalid_request(message.clone()),
                _ => internal_error(format!(
                    "failed to decrement out-of-band elicitation counter: {err}"
                )),
            })?;
        Ok(ThreadDecrementElicitationResponse {
            count,
            paused: count > 0,
        })
    }

    pub(super) async fn thread_set_name_response_inner(
        &self,
        params: ThreadSetNameParams,
    ) -> Result<(ThreadSetNameResponse, Option<ThreadNameUpdatedNotification>), JSONRPCErrorError>
    {
        let ThreadSetNameParams { thread_id, name } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let Some(name) = codex_core::util::normalize_thread_name(&name) else {
            return Err(invalid_request("thread name must not be empty"));
        };

        let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
        self.thread_manager
            .update_thread_metadata(
                thread_id,
                StoreThreadMetadataPatch {
                    name: Some(Some(name.clone())),
                    ..Default::default()
                },
                /*include_archived*/ false,
            )
            .await
            .map_err(|err| core_thread_write_error("set thread name", err))?;

        Ok((
            ThreadSetNameResponse {},
            Some(ThreadNameUpdatedNotification {
                thread_id: thread_id.to_string(),
                thread_name: Some(name),
            }),
        ))
    }

    pub(super) async fn thread_memory_mode_set_response_inner(
        &self,
        params: ThreadMemoryModeSetParams,
    ) -> Result<ThreadMemoryModeSetResponse, JSONRPCErrorError> {
        let ThreadMemoryModeSetParams { thread_id, mode } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;

        self.thread_manager
            .update_thread_metadata(
                thread_id,
                StoreThreadMetadataPatch {
                    memory_mode: Some(mode.to_core()),
                    ..Default::default()
                },
                /*include_archived*/ false,
            )
            .await
            .map_err(|err| core_thread_write_error("set thread memory mode", err))?;

        Ok(ThreadMemoryModeSetResponse {})
    }

    pub(super) async fn thread_metadata_update_response_inner(
        &self,
        params: ThreadMetadataUpdateParams,
    ) -> Result<ThreadMetadataUpdateResponse, JSONRPCErrorError> {
        let ThreadMetadataUpdateParams {
            thread_id,
            project_id,
            git_info,
            daybreak_enabled,
        } = params;
        let thread_uuid = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;

        if let Some(project_id) = project_id.as_ref()
            && !project_id.is_empty()
        {
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

        if git_info.is_none() && project_id.is_none() && daybreak_enabled.is_none() {
            return Err(invalid_request(
                "thread metadata update must include at least one field",
            ));
        }

        let git_info = git_info
            .map(
                |ThreadMetadataGitInfoUpdateParams {
                     sha,
                     branch,
                     origin_url,
                 }| {
                    if sha.is_none() && branch.is_none() && origin_url.is_none() {
                        return Err(invalid_request("gitInfo must include at least one field"));
                    }

                    let origin_url =
                        Self::normalize_thread_metadata_git_field(origin_url, "gitInfo.originUrl")?;
                    let origin_url = match origin_url {
                        Some(Some(origin_url)) => {
                            Some(Some(SanitizedGitUrl::try_from(origin_url).map_err(
                                |_| invalid_request("gitInfo.originUrl must be a valid Git remote"),
                            )?))
                        }
                        Some(None) => Some(None),
                        None => None,
                    };

                    Ok(StoreGitInfoPatch {
                        sha: Self::normalize_thread_metadata_git_field(sha, "gitInfo.sha")?,
                        branch: Self::normalize_thread_metadata_git_field(
                            branch,
                            "gitInfo.branch",
                        )?,
                        origin_url,
                    })
                },
            )
            .transpose()?;

        let project_update: StoreClearableField<String> = if let Some(project_id) = project_id {
            if project_id.is_empty() {
                Some(None)
            } else {
                Some(Some(project_id))
            }
        } else {
            None
        };
        let updated_thread = {
            let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
            let previous_project_id = if project_update.is_some() {
                Some(
                    self.thread_store
                        .read_thread(StoreReadThreadParams {
                            thread_id: thread_uuid,
                            include_archived: true,
                            include_history: false,
                        })
                        .await
                        .map_err(|err| match err {
                            ThreadStoreError::ThreadNotFound { .. } => {
                                invalid_request(format!("thread not found: {thread_id}"))
                            }
                            ThreadStoreError::Unsupported { operation } => {
                                unsupported_thread_store_operation(operation)
                            }
                            err => internal_error(format!("failed to read thread metadata: {err}")),
                        })?
                        .project_id,
                )
            } else {
                None
            };
            let patch = StoreThreadMetadataPatch {
                git_info,
                project_id: project_update.clone(),
                daybreak_enabled,
                ..Default::default()
            };
            let updated_thread = self
                .thread_manager
                .update_thread_metadata(thread_uuid, patch, /*include_archived*/ true)
                .await
                .map_err(|err| core_thread_write_error("update thread metadata", err))?;
            if let Some(project_id) = project_update.as_ref()
                && previous_project_id.as_ref() != Some(project_id)
            {
                self.outgoing
                    .send_server_notification(ServerNotification::ThreadProjectUpdated(
                        ThreadProjectUpdatedNotification {
                            thread_id: thread_id.clone(),
                            project_id: project_id.clone(),
                        },
                    ))
                    .await;
            }
            updated_thread
        };
        let (mut thread, _) = thread_from_stored_thread(
            updated_thread,
            self.config.model_provider_id.as_str(),
            &self.config.cwd,
        );
        if let Ok(loaded_thread) = self.thread_manager.get_thread(thread_uuid).await {
            thread.session_id = loaded_thread.startup_metadata().session_id.to_string();
            let config_snapshot = loaded_thread.config_snapshot().await;
            apply_live_thread_settings(&mut thread, &config_snapshot);
        }
        self.attach_thread_name(thread_uuid, &mut thread).await;
        thread.status = resolve_thread_status(
            self.thread_watch_manager
                .loaded_status_for_thread(&thread.id)
                .await,
            /*has_in_progress_turn*/ false,
        );

        Ok(ThreadMetadataUpdateResponse { thread })
    }

    fn normalize_thread_metadata_git_field(
        value: Option<Option<String>>,
        name: &str,
    ) -> Result<Option<Option<String>>, JSONRPCErrorError> {
        match value {
            Some(Some(value)) => {
                let value = value.trim().to_string();
                if value.is_empty() {
                    return Err(invalid_request(format!("{name} must not be empty")));
                }
                Ok(Some(Some(value)))
            }
            Some(None) => Ok(Some(None)),
            None => Ok(None),
        }
    }

    pub(super) async fn thread_unarchive_response(
        &self,
        params: ThreadUnarchiveParams,
    ) -> Result<(ThreadUnarchiveResponse, Vec<ThreadId>), JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|err| invalid_request(format!("invalid session id: {err}")))?;

        let subtree_thread_ids = self
            .state_db_spawn_subtree_thread_ids(thread_id)
            .await
            .unwrap_or_else(|err| {
                warn!("failed to list spawned descendants while unarchiving {thread_id}: {err:?}");
                vec![thread_id]
            });
        let fallback_provider = self.config.model_provider_id.clone();
        let (stored_thread, restored_thread_ids) = self
            .thread_store
            .unarchive_threads(
                StoreArchiveThreadParams { thread_id },
                subtree_thread_ids.into_iter().skip(1).collect(),
            )
            .await
            .map_err(|err| thread_store_mutation_error("unarchive", err))?;
        let (mut thread, _) =
            thread_from_stored_thread(stored_thread, fallback_provider.as_str(), &self.config.cwd);

        if let Ok(loaded_thread) = self.thread_manager.get_thread(thread_id).await {
            let config_snapshot = loaded_thread.config_snapshot().await;
            apply_live_thread_settings(&mut thread, &config_snapshot);
        }
        thread.status = resolve_thread_status(
            self.thread_watch_manager
                .loaded_status_for_thread(&thread.id)
                .await,
            /*has_in_progress_turn*/ false,
        );
        self.attach_thread_name(thread_id, &mut thread).await;
        Ok((ThreadUnarchiveResponse { thread }, restored_thread_ids))
    }

    pub(super) async fn thread_revert_response(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadRevertParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
    ) -> Result<(ThreadRevertResponse, String), JSONRPCErrorError> {
        let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
        let ThreadRevertParams {
            thread_id,
            before_turn_id,
        } = params;
        let (thread_id, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;
        let config_snapshot = thread.config_snapshot().await;
        if config_snapshot.ephemeral {
            return Err(invalid_request(
                "ephemeral threads do not support thread/revert",
            ));
        }
        if !matches!(config_snapshot.history_mode, ThreadHistoryMode::Paginated) {
            return Err(invalid_request(
                "thread/revert only supports paginated threads",
            ));
        }
        let mut config = thread.config().await.as_ref().clone();
        // Give the replacement its own pending lifetime before removal revokes the old runtime.
        let ema_cancellation = config
            .config_layer_stack
            .mcp_ema_cancellation()
            .for_startup();
        config.config_layer_stack = config
            .config_layer_stack
            .with_mcp_ema_cancellation(ema_cancellation);
        let runtime_snapshot = ThreadRevertRuntimeSnapshot {
            config,
            settings: thread.restorable_thread_settings().await,
            client_mcp_extensions: thread.client_mcp_extensions(),
        };

        // Subscribe before shutdown so a pending idle unload either rejects this request or can
        // no longer race the replacement runtime. The same listener then drains Core's shutdown
        // events before we replace it.
        if matches!(
            self.ensure_conversation_listener(
                thread_id,
                request_id.connection_id,
                /*raw_events_enabled*/ false,
            )
            .await?,
            EnsureConversationListenerResult::ConnectionClosed
        ) {
            return Err(internal_error(format!(
                "connection closed before thread {thread_id} could be reverted"
            )));
        }
        let thread_state = self.thread_state_manager.thread_state(thread_id).await;
        let mut shutdown_drain = thread_state
            .lock()
            .await
            .shutdown_drain
            .clone()
            .ok_or_else(|| {
                internal_error(format!(
                    "thread {thread_id} has no listener to drain shutdown events"
                ))
            })?;

        match wait_for_thread_shutdown(&thread).await {
            ThreadShutdownResult::Complete => {}
            ThreadShutdownResult::SubmitFailed => {
                return Err(internal_error(format!(
                    "failed to shut down thread {thread_id} before revert"
                )));
            }
            ThreadShutdownResult::TimedOut => {
                return Err(internal_error(format!(
                    "timed out shutting down thread {thread_id} before revert"
                )));
            }
        }
        tokio::time::timeout(
            Duration::from_secs(10),
            shutdown_drain.wait_for(|drained| *drained),
        )
        .await
        .map_err(|_| {
            internal_error(format!(
                "timed out waiting for thread {thread_id} listener to drain shutdown events"
            ))
        })?
        .map_err(|_| {
            internal_error(format!(
                "thread {thread_id} listener stopped before draining shutdown events"
            ))
        })?;
        // The listener is drained: even an interrupted turn is durable now. Inspect only the
        // suffix being reverted and remember the revision, not a collection of turn IDs.
        let receipt_to_retire = async {
            let Some(db) = self.state_db.as_ref() else {
                return anyhow::Ok(None);
            };
            let Some(state) = db
                .thread_read_states(&[thread_id])
                .await?
                .remove(&thread_id)
            else {
                return Ok(None);
            };
            let Some(turn_id) = state.first_unread_turn.as_ref().filter(|id| !id.is_empty()) else {
                return Ok(None);
            };
            let mut cursor = None;
            loop {
                let page = self
                    .thread_store
                    .list_turns(codex_thread_store::ListTurnsParams {
                        thread_id,
                        include_archived: false,
                        cursor: cursor.clone(),
                        page_size: THREAD_TURNS_MAX_LIMIT,
                        sort_direction: codex_thread_store::SortDirection::Desc,
                        items_view: codex_thread_store::StoredTurnItemsView::NotLoaded,
                    })
                    .await?;
                for turn in page.turns {
                    if turn.turn_id == *turn_id {
                        return Ok(Some(state.revision));
                    }
                    if turn.turn_id == before_turn_id {
                        return Ok(None);
                    }
                }
                let Some(next) = page.next_cursor else {
                    return Ok(None);
                };
                if cursor.as_ref() == Some(&next) {
                    anyhow::bail!("thread/revert turn cursor did not advance");
                }
                cursor = Some(next);
            }
        }
        .await;
        if self
            .thread_manager
            .remove_thread(&thread_id)
            .await
            .is_none()
        {
            return Err(internal_error(format!(
                "thread {thread_id} disappeared before revert"
            )));
        }
        // Keep thread state and subscriptions across the internal reload. Full teardown would
        // force clients to call thread/resume after a successful revert.
        self.outgoing
            .cancel_requests_for_thread(thread_id, /*error*/ None)
            .await;

        let revert_result = self
            .thread_store
            .revert_thread(codex_thread_store::RevertThreadParams {
                thread_id,
                before_turn_id,
                multi_agent_version: thread.multi_agent_version(),
            })
            .await
            .map_err(|err| thread_store_mutation_error("revert", err));
        // Receipt lookup or update errors cannot prevent reload or misreport a committed revert.
        // A concurrent mark wins over this cleanup, even while the thread was being truncated.
        if revert_result.is_ok()
            && let Some(db) = self.state_db.as_ref()
        {
            let retirement = match receipt_to_retire {
                Ok(Some(revision)) => db
                    .update_thread_read_state(
                        thread_id,
                        &revision,
                        codex_state::ReadStateOperation::Read,
                    )
                    .await
                    .map(|update| matches!(update, codex_state::ReadStateUpdate::Applied(_))),
                Ok(None) => Ok(false),
                Err(err) => Err(err),
            };
            match retirement {
                Ok(true) => {
                    super::thread_read_state::notify(
                        db,
                        &self.thread_state_manager,
                        &self.outgoing,
                        thread_id,
                    )
                    .await;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!("reverted history but could not update unread position: {err}")
                }
            }
        }
        let response = self
            .reload_paginated_thread(
                request_id,
                thread_id,
                runtime_snapshot,
                app_server_client_name,
                app_server_client_version,
            )
            .await?;
        revert_result?;
        Ok((response, thread_id.to_string()))
    }

    async fn reload_paginated_thread(
        &self,
        request_id: &ConnectionRequestId,
        thread_id: ThreadId,
        runtime_snapshot: ThreadRevertRuntimeSnapshot,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
    ) -> Result<ThreadRevertResponse, JSONRPCErrorError> {
        let ThreadRevertRuntimeSnapshot {
            mut config,
            settings,
            client_mcp_extensions,
        } = runtime_snapshot;
        if let Some(collaboration_mode) = settings.collaboration_mode.as_ref() {
            config.model = Some(collaboration_mode.model().to_string());
        }
        if let Some(service_tier) = settings.service_tier.as_ref() {
            config.service_tier.clone_from(service_tier);
        }
        config.personality = settings.personality;
        let thread_id_string = thread_id.to_string();
        let stored_thread = self
            .read_stored_thread_for_resume(
                thread_id_string.as_str(),
                /*path*/ None,
                /*include_history*/ false,
            )
            .await?;
        let (thread_history, resume_source_thread) = self
            .load_resume_initial_history_from_stored_thread(
                stored_thread,
                /*prepared_model_context*/ None,
            )
            .await?;
        let response_history = thread_history.clone();
        let NewThread {
            thread_id: resumed_thread_id,
            thread: codex_thread,
            session_configured,
            ..
        } = self
            .thread_manager
            .resume_thread_with_history(
                config,
                thread_history,
                self.auth_manager.clone(),
                self.request_trace_context(request_id).await,
                client_mcp_extensions,
            )
            .await
            .map_err(|err| internal_error(format!("error reloading thread after revert: {err}")))?;
        if resumed_thread_id != thread_id {
            return Err(internal_error(format!(
                "thread {thread_id} reloaded as {resumed_thread_id} after revert"
            )));
        }
        codex_thread
            .restore_thread_settings(settings)
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to restore thread settings after revert: {err}"
                ))
            })?;
        // Replace the resume-time checkpoint written from the original config.
        codex_thread
            .checkpoint_thread_settings()
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to persist restored thread settings after revert: {err}"
                ))
            })?;
        Self::set_app_server_client_info(
            codex_thread.as_ref(),
            app_server_client_name,
            app_server_client_version,
        )
        .await?;
        let SessionConfiguredEvent { rollout_path, .. } = session_configured;
        let rollout_path = rollout_path.ok_or_else(|| {
            internal_error(format!(
                "rollout path missing after reloading thread {thread_id}"
            ))
        })?;
        // Revert keeps the existing thread state and subscriptions across the internal reload.
        // Start the replacement listener from that state instead of depending on the requesting
        // connection still being open.
        let thread_state = self.thread_state_manager.thread_state(thread_id).await;
        self.ensure_listener_task_running(thread_id, Arc::clone(&codex_thread), thread_state)
            .await?;
        let mut thread = self
            .load_thread_from_resume_source_or_send_internal(
                thread_id,
                codex_thread.as_ref(),
                &response_history,
                rollout_path.as_path(),
                Some(resume_source_thread),
                /*include_turns*/ false,
            )
            .await
            .map_err(internal_error)?;
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
        let (turns_backwards_cursor, items_backwards_cursor) =
            Self::paginated_resume_backwards_cursors(self.thread_store.as_ref(), thread_id).await?;
        Ok(ThreadRevertResponse {
            thread,
            turns_backwards_cursor,
            items_backwards_cursor,
        })
    }

    pub(super) async fn thread_compact_start_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadCompactStartParams,
    ) -> Result<ThreadCompactStartResponse, JSONRPCErrorError> {
        let ThreadCompactStartParams { thread_id } = params;

        let (_, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;
        self.config_manager
            .check_thread_model_provider(thread.config().await.as_ref())
            .await
            .map_err(|error| config_load_error(&error))?;
        self.submit_core_op(request_id, thread.as_ref(), Op::Compact)
            .await
            .map_err(|err| internal_error(format!("failed to start compaction: {err}")))?;
        Ok(ThreadCompactStartResponse {})
    }

    pub(super) async fn thread_background_terminals_clean_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadBackgroundTerminalsCleanParams,
    ) -> Result<ThreadBackgroundTerminalsCleanResponse, JSONRPCErrorError> {
        let ThreadBackgroundTerminalsCleanParams { thread_id } = params;

        let (_, thread) = self.load_thread(&thread_id).await?;
        self.submit_core_op(request_id, thread.as_ref(), Op::CleanBackgroundTerminals)
            .await
            .map_err(|err| {
                internal_error(format!("failed to clean background terminals: {err}"))
            })?;
        Ok(ThreadBackgroundTerminalsCleanResponse {})
    }

    pub(super) async fn thread_background_terminals_list_inner(
        &self,
        params: ThreadBackgroundTerminalsListParams,
    ) -> Result<ThreadBackgroundTerminalsListResponse, JSONRPCErrorError> {
        let ThreadBackgroundTerminalsListParams {
            thread_id,
            cursor,
            limit,
        } = params;

        let (_, thread) = self.load_thread(&thread_id).await?;
        let terminals = thread
            .list_background_terminals()
            .await
            .into_iter()
            .map(|terminal| ThreadBackgroundTerminal {
                item_id: terminal.item_id,
                process_id: terminal.process_id,
                command: terminal.command,
                cwd: terminal.cwd.into(),
                os_pid: None,
                cpu_percent: None,
                rss_kb: None,
            })
            .collect::<Vec<_>>();

        let (data, next_cursor) = paginate_background_terminals(&terminals, cursor, limit)?;

        Ok(ThreadBackgroundTerminalsListResponse { data, next_cursor })
    }

    pub(super) async fn thread_background_terminals_terminate_inner(
        &self,
        params: ThreadBackgroundTerminalsTerminateParams,
    ) -> Result<ThreadBackgroundTerminalsTerminateResponse, JSONRPCErrorError> {
        let ThreadBackgroundTerminalsTerminateParams {
            thread_id,
            process_id,
        } = params;
        let process_id = process_id.parse::<i32>().map_err(|err| {
            invalid_request(format!("invalid background terminal process id: {err}"))
        })?;

        let (_, thread) = self.load_thread(&thread_id).await?;
        let terminated = thread.terminate_background_terminal(process_id).await;
        Ok(ThreadBackgroundTerminalsTerminateResponse { terminated })
    }

    pub(super) async fn thread_shell_command_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadShellCommandParams,
    ) -> Result<ThreadShellCommandResponse, JSONRPCErrorError> {
        let ThreadShellCommandParams {
            thread_id,
            command,
            timeout_ms,
        } = params;
        let command = command.trim().to_string();
        if command.is_empty() {
            return Err(invalid_request("command must not be empty"));
        }

        let timeout_ms = timeout_ms
            .map(|timeout_ms| {
                u64::try_from(timeout_ms).map_err(|_| {
                    invalid_params(format!(
                        "thread/shellCommand timeoutMs must be non-negative, got {timeout_ms}"
                    ))
                })
            })
            .transpose()?;

        let (_, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;

        // `thread/shellCommand` is app-server's local-host shell escape hatch,
        // not the normal turn-selected shell tool path.
        if self
            .thread_manager
            .environment_manager()
            .try_local_environment()
            .is_none()
        {
            return Err(internal_error("local environment is not configured"));
        }

        self.submit_core_op(
            request_id,
            thread.as_ref(),
            Op::RunUserShellCommand {
                command,
                timeout_ms,
            },
        )
        .await
        .map_err(|err| internal_error(format!("failed to start shell command: {err}")))?;
        Ok(ThreadShellCommandResponse {})
    }

    pub(super) async fn thread_approve_guardian_denied_action_inner(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadApproveGuardianDeniedActionParams,
    ) -> Result<ThreadApproveGuardianDeniedActionResponse, JSONRPCErrorError> {
        let ThreadApproveGuardianDeniedActionParams { thread_id, event } = params;
        let event = serde_json::from_value(event)
            .map_err(|err| invalid_request(format!("invalid Guardian denial event: {err}")))?;
        let (_, thread) = self.load_thread(&thread_id).await?;
        ensure_direct_input_allowed(thread.as_ref()).await?;

        self.submit_core_op(
            request_id,
            thread.as_ref(),
            Op::ApproveGuardianDeniedAction { event },
        )
        .await
        .map_err(|err| internal_error(format!("failed to approve Guardian denial: {err}")))?;
        Ok(ThreadApproveGuardianDeniedActionResponse {})
    }
}
