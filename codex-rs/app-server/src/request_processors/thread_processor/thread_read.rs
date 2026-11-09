//! Thread read, turn and item paging, and occurrence search requests.

use super::*;

impl ThreadRequestProcessor {
    pub(super) async fn thread_read_response_inner(
        &self,
        params: ThreadReadParams,
    ) -> Result<ThreadReadResponse, JSONRPCErrorError> {
        let ThreadReadParams {
            thread_id,
            include_turns,
        } = params;

        let thread_uuid = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;

        let thread = self
            .read_thread_view(thread_uuid, include_turns)
            .await
            .map_err(thread_read_view_error)?;
        let read_state = super::thread_read_state::snapshots(
            self.state_db.as_ref(),
            std::slice::from_ref(&thread),
        )
        .await
        .and_then(|mut states| states.remove(&thread.id));
        Ok(ThreadReadResponse { thread, read_state })
    }

    /// Builds the API view for `thread/read` from persisted metadata plus optional live state.
    pub(in crate::request_processors) async fn read_thread_view(
        &self,
        thread_id: ThreadId,
        include_turns: bool,
    ) -> Result<Thread, ThreadReadViewError> {
        // Read staging first: persistence can consume it while the stored thread is read.
        let pending_daybreak_enabled = self
            .thread_store
            .read_pending_thread_metadata(thread_id)
            .await
            .map_err(|err| {
                ThreadReadViewError::Internal(format!(
                    "failed to read pending thread metadata: {err}"
                ))
            })?
            .and_then(|metadata| metadata.daybreak_enabled);
        let loaded_thread = self.thread_manager.get_thread(thread_id).await.ok();
        let mut thread = if include_turns {
            if let Some(loaded_thread) = loaded_thread.as_ref() {
                // Loaded thread with turns: use persisted metadata when it exists,
                // but reconstruct turns from the live ThreadStore history.
                let persisted_thread = self
                    .load_persisted_thread_for_read(thread_id, /*include_turns*/ false)
                    .await?;
                self.load_live_thread_view(
                    thread_id,
                    include_turns,
                    loaded_thread,
                    persisted_thread,
                )
                .await?
            } else if let Some(thread) = self
                .load_persisted_thread_for_read(thread_id, include_turns)
                .await?
            {
                // Unloaded thread with turns: load metadata and history together
                // from the ThreadStore.
                thread
            } else {
                return Err(ThreadReadViewError::InvalidRequest(format!(
                    "thread not loaded: {thread_id}"
                )));
            }
        } else if let Some(thread) = self
            .load_persisted_thread_for_read(thread_id, include_turns)
            .await?
        {
            if let Some(loaded_thread) = loaded_thread.as_ref() {
                self.load_live_thread_view(thread_id, include_turns, loaded_thread, Some(thread))
                    .await?
            } else {
                thread
            }
        } else if let Some(loaded_thread) = loaded_thread.as_ref() {
            // Loaded metadata-only read before persistence is materialized: build
            // the response from the live thread snapshot.
            self.load_live_thread_view(
                thread_id,
                include_turns,
                loaded_thread,
                /*persisted_thread*/ None,
            )
            .await?
        } else {
            return Err(ThreadReadViewError::InvalidRequest(format!(
                "thread not loaded: {thread_id}"
            )));
        };

        thread.daybreak_enabled = thread.daybreak_enabled.or(pending_daybreak_enabled);

        let has_live_in_progress_turn = if let Some(loaded_thread) = loaded_thread.as_ref() {
            matches!(loaded_thread.agent_status().await, AgentStatus::Running)
        } else {
            false
        };

        let thread_status = self
            .thread_watch_manager
            .loaded_status_for_thread(&thread.id)
            .await;

        set_thread_status_and_interrupt_stale_turns(
            &mut thread,
            thread_status,
            has_live_in_progress_turn,
        );
        Ok(thread)
    }

    async fn load_persisted_thread_for_read(
        &self,
        thread_id: ThreadId,
        include_turns: bool,
    ) -> Result<Option<Thread>, ThreadReadViewError> {
        let fallback_provider = self.config.model_provider_id.as_str();
        if include_turns {
            let Some(stored_thread) = self
                .read_stored_thread_for_read(thread_id, /*include_history*/ false)
                .await?
            else {
                return Ok(None);
            };
            if matches!(stored_thread.history_mode, ThreadHistoryMode::Paginated) {
                let (mut thread, _) =
                    thread_from_stored_thread(stored_thread, fallback_provider, &self.config.cwd);
                thread.turns = self
                    .paginated_thread_full_turns(thread_id)
                    .await
                    .map_err(ThreadReadViewError::JsonRpc)?;
                return Ok(Some(thread));
            }
        }
        let Some(stored_thread) = self
            .read_stored_thread_for_read(thread_id, /*include_history*/ include_turns)
            .await?
        else {
            return Ok(None);
        };
        let (mut thread, history) =
            thread_from_stored_thread(stored_thread, fallback_provider, &self.config.cwd);
        if include_turns && let Some(history) = history {
            thread.turns = build_legacy_api_turns_from_rollout_items(&history.items);
        }
        Ok(Some(thread))
    }

    async fn read_stored_thread_for_read(
        &self,
        thread_id: ThreadId,
        include_history: bool,
    ) -> Result<Option<StoredThread>, ThreadReadViewError> {
        match self
            .thread_store
            .read_thread(StoreReadThreadParams {
                thread_id,
                include_archived: true,
                include_history,
            })
            .await
        {
            Ok(stored_thread) => Ok(Some(stored_thread)),
            Err(ThreadStoreError::InvalidRequest { message })
                if message == format!("no rollout found for thread id {thread_id}") =>
            {
                Ok(None)
            }
            Err(ThreadStoreError::ThreadNotFound {
                thread_id: missing_thread_id,
            }) if missing_thread_id == thread_id => Ok(None),
            Err(ThreadStoreError::InvalidRequest { message }) => {
                Err(ThreadReadViewError::InvalidRequest(message))
            }
            Err(ThreadStoreError::Unsupported { operation }) => {
                Err(ThreadReadViewError::Unsupported(operation))
            }
            Err(err) => Err(ThreadReadViewError::Internal(format!(
                "failed to read thread: {err}"
            ))),
        }
    }

    /// Builds a `thread/read` view from a loaded thread plus optional persisted metadata.
    async fn load_live_thread_view(
        &self,
        thread_id: ThreadId,
        include_turns: bool,
        loaded_thread: &CodexThread,
        persisted_thread: Option<Thread>,
    ) -> Result<Thread, ThreadReadViewError> {
        let config_snapshot = loaded_thread.config_snapshot().await;
        if include_turns && config_snapshot.ephemeral {
            return Err(ThreadReadViewError::InvalidRequest(
                "ephemeral threads do not support includeTurns".to_string(),
            ));
        }
        let fallback_thread =
            build_thread_from_loaded_snapshot(thread_id, &config_snapshot, loaded_thread);
        let mut thread = if let Some(mut thread) = persisted_thread {
            if thread.path.is_none() {
                thread.path = fallback_thread.path.clone();
            }
            thread.session_id.clone_from(&fallback_thread.session_id);
            thread.ephemeral = fallback_thread.ephemeral;
            thread.can_accept_direct_input = fallback_thread.can_accept_direct_input;
            thread
        } else {
            fallback_thread
        };
        apply_live_thread_settings(&mut thread, &config_snapshot);
        self.apply_thread_read_store_fields(thread_id, &mut thread, include_turns, loaded_thread)
            .await?;
        Ok(thread)
    }

    async fn apply_thread_read_store_fields(
        &self,
        thread_id: ThreadId,
        thread: &mut Thread,
        include_turns: bool,
        loaded_thread: &CodexThread,
    ) -> Result<(), ThreadReadViewError> {
        self.attach_thread_name(thread_id, thread).await;

        if include_turns {
            if matches!(
                thread.history_mode,
                codex_app_server_protocol::ThreadHistoryMode::Paginated
            ) {
                self.thread_store
                    .persist_thread(thread_id, PersistContext::Standard)
                    .await
                    .map_err(|err| thread_read_history_load_error(thread_id, err))?;
                thread.turns = self
                    .paginated_thread_full_turns(thread_id)
                    .await
                    .map_err(ThreadReadViewError::JsonRpc)?;
                return Ok(());
            }
            let history = loaded_thread
                .load_history(/*include_archived*/ true)
                .await
                .map_err(|err| thread_read_history_load_error(thread_id, err))?;
            thread.turns = build_legacy_api_turns_from_rollout_items(&history.items);
        }

        Ok(())
    }

    pub(super) async fn thread_turns_list_response_inner(
        &self,
        params: ThreadTurnsListParams,
    ) -> Result<ThreadTurnsListResponse, JSONRPCErrorError> {
        let ThreadTurnsListParams {
            thread_id,
            cursor,
            limit,
            sort_direction,
            items_view,
        } = params;
        let thread_uuid = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        match self
            .thread_store
            .read_thread(StoreReadThreadParams {
                thread_id: thread_uuid,
                include_archived: true,
                include_history: false,
            })
            .await
        {
            Ok(thread) if thread.history_mode == ThreadHistoryMode::Paginated => {
                return self
                    .paginated_thread_turns_list_response(
                        thread_uuid,
                        cursor,
                        limit,
                        sort_direction,
                        items_view,
                    )
                    .await;
            }
            Ok(_) => {}
            Err(ThreadStoreError::InvalidRequest { message })
                if message == format!("no rollout found for thread id {thread_uuid}") => {}
            Err(ThreadStoreError::ThreadNotFound { thread_id }) if thread_id == thread_uuid => {}
            Err(ThreadStoreError::InvalidRequest { message }) => {
                return Err(invalid_request(message));
            }
            Err(ThreadStoreError::Unsupported { operation }) => {
                return Err(unsupported_thread_store_operation(operation));
            }
            Err(err) => return Err(internal_error(format!("failed to read thread: {err}"))),
        }

        let items = self
            .load_thread_turns_list_history(thread_uuid)
            .await
            .map_err(thread_read_view_error)?;
        // This API optimizes network transfer by letting clients page through a
        // thread's turns incrementally, but it still replays the entire rollout on
        // every request. Rollback and compaction events can change earlier turns, so
        // the server has to rebuild the full turn list until turn metadata is indexed
        // separately.
        let loaded_thread = self.thread_manager.get_thread(thread_uuid).await.ok();
        let has_live_running_thread = match loaded_thread.as_ref() {
            Some(thread) => matches!(thread.agent_status().await, AgentStatus::Running),
            None => false,
        };
        let active_turn = if loaded_thread.is_some() {
            // Persisted history may not yet include the currently running turn. The
            // app-server listener has already projected live turn events into ThreadState,
            // so merge that in-memory snapshot before paginating.
            let thread_state = self.thread_state_manager.thread_state(thread_uuid).await;
            let state = thread_state.lock().await;
            state.active_turn_snapshot()
        } else {
            None
        };
        build_thread_turns_page_response(
            &items,
            self.thread_watch_manager
                .loaded_status_for_thread(&thread_uuid.to_string())
                .await,
            has_live_running_thread,
            active_turn,
            ThreadTurnsPageOptions {
                cursor: cursor.as_deref(),
                limit,
                sort_direction: sort_direction.unwrap_or(SortDirection::Desc),
                items_view: items_view.unwrap_or(TurnItemsView::Summary),
            },
        )
    }

    pub(super) async fn thread_search_occurrences_response_inner(
        &self,
        params: ThreadSearchOccurrencesParams,
    ) -> Result<ThreadSearchOccurrencesResponse, JSONRPCErrorError> {
        let ThreadSearchOccurrencesParams {
            thread_id,
            search_term,
            cursor,
            limit,
        } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        if search_term.trim().is_empty() {
            return Err(invalid_request(
                "thread/searchOccurrences requires a non-empty searchTerm",
            ));
        }
        let page_size = limit
            .map(|value| value as usize)
            .unwrap_or(THREAD_SEARCH_OCCURRENCES_DEFAULT_LIMIT)
            .clamp(1, THREAD_SEARCH_OCCURRENCES_MAX_LIMIT);
        let page = self
            .thread_store
            .search_thread_occurrences(StoreSearchThreadOccurrencesParams {
                thread_id,
                search_term,
                cursor,
                page_size,
            })
            .await
            .map_err(|err| match err {
                ThreadStoreError::InvalidRequest { message } => invalid_request(message),
                ThreadStoreError::Unsupported { operation } => {
                    unsupported_thread_store_operation(operation)
                }
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    invalid_request(format!("no rollout found for thread id {thread_id}"))
                }
                err => internal_error(format!("failed to search thread occurrences: {err}")),
            })?;
        Ok(ThreadSearchOccurrencesResponse {
            data: page
                .items
                .into_iter()
                .map(|item| ThreadSearchOccurrence {
                    turn_id: item.turn_id,
                    item_id: item.item_id,
                    snippet: item.snippet,
                    snippet_match_range: ThreadSearchTextRange {
                        start: item.snippet_match_range.start,
                        end: item.snippet_match_range.end,
                    },
                    turn_cursor: item.turn_cursor,
                })
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    async fn paginated_thread_turns_list_response(
        &self,
        thread_id: ThreadId,
        cursor: Option<String>,
        limit: Option<u32>,
        sort_direction: Option<SortDirection>,
        items_view: Option<TurnItemsView>,
    ) -> Result<ThreadTurnsListResponse, JSONRPCErrorError> {
        let items_view = items_view.unwrap_or(TurnItemsView::Summary);
        let page_size = thread_turns_page_size(limit);
        let sort_direction = match sort_direction.unwrap_or(SortDirection::Desc) {
            SortDirection::Asc => StoreSortDirection::Asc,
            SortDirection::Desc => StoreSortDirection::Desc,
        };
        // `Full` is only a temporary compatibility path. Keep it out of ThreadStore's API:
        // load turn shells here, then hydrate their items below.
        let stored_items_view = match items_view {
            TurnItemsView::NotLoaded => StoredTurnItemsView::NotLoaded,
            TurnItemsView::Summary => StoredTurnItemsView::Summary,
            TurnItemsView::Full => StoredTurnItemsView::NotLoaded,
        };
        let page = self
            .thread_store
            .list_turns(StoreListTurnsParams {
                thread_id,
                include_archived: true,
                cursor,
                page_size,
                sort_direction,
                items_view: stored_items_view,
            })
            .await
            .map_err(|err| match err {
                ThreadStoreError::InvalidRequest { message } => invalid_request(message),
                ThreadStoreError::Unsupported { operation } => {
                    unsupported_thread_store_operation(operation)
                }
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    invalid_request(format!("no rollout found for thread id {thread_id}"))
                }
                err => internal_error(format!("failed to list thread history: {err}")),
            })?;
        let mut turns = Vec::with_capacity(page.turns.len());
        for turn in page.turns {
            let mut turn = stored_turn_to_api_turn(turn, items_view)?;
            if matches!(items_view, TurnItemsView::Full) {
                turn.items = self
                    .paginated_turn_full_items(thread_id, turn.id.as_str())
                    .await?;
            }
            turns.push(turn);
        }
        let loaded_thread = self.thread_manager.get_thread(thread_id).await.ok();
        let has_live_running_thread = match loaded_thread.as_ref() {
            Some(thread) => matches!(thread.agent_status().await, AgentStatus::Running),
            None => false,
        };
        normalize_thread_turns_status(
            &mut turns,
            self.thread_watch_manager
                .loaded_status_for_thread(&thread_id.to_string())
                .await,
            has_live_running_thread,
        );
        Ok(ThreadTurnsListResponse {
            data: turns,
            next_cursor: page.next_cursor,
            backwards_cursor: page.backwards_cursor,
        })
    }

    // Older clients still request `itemsView: "full"` from turn pages. Keep this
    // app-server-only hydration path until those clients use `thread/items/list`.
    async fn paginated_turn_full_items(
        &self,
        thread_id: ThreadId,
        turn_id: &str,
    ) -> Result<Vec<ThreadItem>, JSONRPCErrorError> {
        let mut cursor = None;
        let mut items = Vec::new();
        loop {
            let page = self
                .thread_store
                .list_items(StoreListItemsParams {
                    item_ids: None,
                    thread_id,
                    turn_id: Some(turn_id.to_string()),
                    include_archived: true,
                    position: cursor.clone().map(StoreListItemsPosition::Cursor),
                    page_size: THREAD_ITEMS_MAX_LIMIT,
                    sort_direction: StoreSortDirection::Asc,
                    sort_key: StoreItemSortKey::CreatedAtOrdinal,
                    after_updated_at_ordinal: None,
                    before_updated_at_ordinal: None,
                })
                .await
                .map_err(paginated_history_list_error)?;
            for item in &page.items {
                items.push(stored_thread_item_to_api_item(item)?);
            }
            let Some(next_cursor) = page.next_cursor else {
                return Ok(items);
            };
            if cursor.as_ref() == Some(&next_cursor) {
                return Err(internal_error(format!(
                    "failed to load full turn items for {turn_id}: thread store returned a repeated cursor"
                )));
            }
            cursor = Some(next_cursor);
        }
    }

    // Older clients expect full `thread.turns` from resume and `thread/read(includeTurns=true)`.
    // Keep this slow compatibility path until all clients page history directly.
    pub(super) async fn paginated_thread_full_turns(
        &self,
        thread_id: ThreadId,
    ) -> Result<Vec<Turn>, JSONRPCErrorError> {
        let mut cursor = None;
        let mut turns = Vec::new();
        loop {
            let page = self
                .paginated_thread_turns_list_response(
                    thread_id,
                    cursor.clone(),
                    Some(THREAD_TURNS_MAX_LIMIT as u32),
                    Some(SortDirection::Asc),
                    Some(TurnItemsView::Full),
                )
                .await?;
            turns.extend(page.data);
            let Some(next_cursor) = page.next_cursor else {
                return Ok(turns);
            };
            if cursor.as_ref() == Some(&next_cursor) {
                return Err(internal_error(format!(
                    "failed to load full thread turns for {thread_id}: thread store returned a repeated cursor"
                )));
            }
            cursor = Some(next_cursor);
        }
    }

    pub(super) async fn paginated_resume_initial_turns_page(
        &self,
        thread_id: ThreadId,
        params: &ThreadResumeInitialTurnsPageParams,
    ) -> Result<codex_app_server_protocol::TurnsPage, JSONRPCErrorError> {
        self.paginated_thread_turns_list_response(
            thread_id,
            /*cursor*/ None,
            params.limit,
            params.sort_direction,
            params.items_view,
        )
        .await
        .map(Into::into)
    }

    pub(super) async fn paginated_resume_initial_turns_page_with_active_slot(
        &self,
        thread_id: ThreadId,
        params: &ThreadResumeInitialTurnsPageParams,
    ) -> Result<codex_app_server_protocol::TurnsPage, JSONRPCErrorError> {
        // A running resume overlays the newest live turn on this durable page.
        // Reserve one row so the overlay keeps the requested limit and the
        // durable next cursor still starts after the last returned stored turn.
        let page_size = thread_turns_page_size(params.limit);
        if page_size == 1 {
            // ThreadStore does not accept an empty page. Use its backwards cursor as
            // the next cursor so the omitted durable row is returned next.
            let mut page = self
                .paginated_resume_initial_turns_page(thread_id, params)
                .await?;
            page.next_cursor = page.backwards_cursor.clone();
            page.data.clear();
            return Ok(page);
        }

        let mut params = params.clone();
        params.limit = Some((page_size - 1) as u32);
        self.paginated_resume_initial_turns_page(thread_id, &params)
            .await
    }

    pub(in crate::request_processors) async fn paginated_resume_backwards_cursors(
        thread_store: &dyn ThreadStore,
        thread_id: ThreadId,
    ) -> Result<(Option<String>, Option<String>), JSONRPCErrorError> {
        let turns_page = thread_store
            .list_turns(StoreListTurnsParams {
                thread_id,
                include_archived: true,
                cursor: None,
                page_size: 1,
                sort_direction: StoreSortDirection::Desc,
                items_view: StoredTurnItemsView::NotLoaded,
            })
            .await
            .map_err(paginated_history_list_error)?;
        let items_page = thread_store
            .list_items(StoreListItemsParams {
                item_ids: None,
                thread_id,
                turn_id: None,
                include_archived: true,
                position: None,
                page_size: 1,
                sort_direction: StoreSortDirection::Desc,
                sort_key: StoreItemSortKey::CreatedAtOrdinal,
                after_updated_at_ordinal: None,
                before_updated_at_ordinal: None,
            })
            .await
            .map_err(paginated_history_list_error)?;
        Ok((turns_page.backwards_cursor, items_page.backwards_cursor))
    }

    pub(super) async fn thread_items_list_response_inner(
        &self,
        params: ThreadItemsListParams,
    ) -> Result<ThreadItemsListResponse, JSONRPCErrorError> {
        let ThreadItemsListParams {
            thread_id,
            turn_id,
            cursor,
            limit,
            sort_direction,
            defer_output,
        } = params;
        let defer_command_output =
            turn_id.is_some() && defer_output.is_some_and(|options| options.command_execution);
        let position = cursor.map(|cursor| match cursor {
            ThreadItemsListCursor::Opaque(cursor) => StoreListItemsPosition::Cursor(cursor),
            ThreadItemsListCursor::Anchor(ThreadItemsListAnchor::Item { item_id }) => {
                StoreListItemsPosition::ItemAnchor { item_id }
            }
        });
        let has_anchor = matches!(position, Some(StoreListItemsPosition::ItemAnchor { .. }));
        if has_anchor && turn_id.as_deref().is_none_or(str::is_empty) {
            return Err(invalid_params(
                "turnId is required when cursor is an item anchor",
            ));
        }
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let page_size = limit
            .map(|value| value as usize)
            .unwrap_or(THREAD_ITEMS_DEFAULT_LIMIT)
            .clamp(1, THREAD_ITEMS_MAX_LIMIT);
        let page = self
            .thread_store
            .list_items(StoreListItemsParams {
                item_ids: None,
                thread_id,
                turn_id,
                include_archived: true,
                position,
                page_size,
                sort_direction: match sort_direction.unwrap_or(SortDirection::Asc) {
                    SortDirection::Asc => StoreSortDirection::Asc,
                    SortDirection::Desc => StoreSortDirection::Desc,
                },
                sort_key: StoreItemSortKey::CreatedAtOrdinal,
                after_updated_at_ordinal: None,
                before_updated_at_ordinal: None,
            })
            .await
            .map_err(|err| match err {
                ThreadStoreError::InvalidRequest { message } if has_anchor => {
                    invalid_params(message)
                }
                ThreadStoreError::InvalidRequest { message } => invalid_request(message),
                ThreadStoreError::Unsupported { .. } => {
                    method_not_found("thread/items/list is not supported yet")
                }
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    invalid_request(format!("no rollout found for thread id {thread_id}"))
                }
                err => internal_error(format!("failed to list thread items: {err}")),
            })?;
        let mut data = Vec::with_capacity(page.items.len());
        for stored_item in page.items {
            let mut entry = stored_thread_item_to_entry(stored_item)?;
            if defer_command_output
                && entry.completed_at_ms.is_some()
                && let ThreadItemPayload::CommandExecution {
                    status: codex_app_server_protocol::CommandExecutionStatus::Completed,
                    exit_code: Some(0),
                    aggregated_output,
                    ..
                } = &mut entry.item.payload
                && aggregated_output
                    .as_ref()
                    .is_some_and(|output| !output.is_empty())
            {
                *aggregated_output = None;
                entry.output_deferred = true;
            }
            data.push(entry);
        }

        Ok(ThreadItemsListResponse {
            data,
            next_cursor: page.next_cursor,
            backwards_cursor: page.backwards_cursor,
        })
    }

    pub(in crate::request_processors) async fn read_thread_items(
        store: &dyn ThreadStore,
        params: ThreadItemsReadParams,
    ) -> Result<ThreadItemsReadResponse, JSONRPCErrorError> {
        if !(1..=THREAD_ITEMS_MAX_LIMIT).contains(&params.item_ids.len()) {
            return Err(invalid_request(
                "itemIds must contain 1 to 100 IDs".to_string(),
            ));
        }
        let ThreadItemsReadParams {
            thread_id,
            turn_id,
            item_ids,
        } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let page_size = item_ids.len();
        let page = store
            .list_items(StoreListItemsParams {
                item_ids: Some(item_ids),
                thread_id,
                turn_id: Some(turn_id),
                include_archived: true,
                position: None,
                page_size,
                sort_direction: StoreSortDirection::Asc,
                sort_key: StoreItemSortKey::CreatedAtOrdinal,
                after_updated_at_ordinal: None,
                before_updated_at_ordinal: None,
            })
            .await
            .map_err(|err| match err {
                ThreadStoreError::InvalidRequest { message } => invalid_request(message),
                ThreadStoreError::Unsupported { .. } => {
                    method_not_found("thread/items/read is not supported yet")
                }
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    invalid_request(format!("no rollout found for thread id {thread_id}"))
                }
                err => internal_error(format!("failed to read thread items: {err}")),
            })?;
        let data = page
            .items
            .into_iter()
            .map(stored_thread_item_to_entry)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ThreadItemsReadResponse { data })
    }

    async fn load_thread_turns_list_history(
        &self,
        thread_id: ThreadId,
    ) -> Result<Vec<RolloutItem>, ThreadReadViewError> {
        match self
            .thread_store
            .read_thread(StoreReadThreadParams {
                thread_id,
                include_archived: true,
                include_history: true,
            })
            .await
        {
            Ok(stored_thread) => {
                let history = stored_thread.history.ok_or_else(|| {
                    ThreadReadViewError::Internal(format!(
                        "thread store did not return history for thread {thread_id}"
                    ))
                })?;
                return Ok(history.items);
            }
            Err(ThreadStoreError::InvalidRequest { message })
                if message == format!("no rollout found for thread id {thread_id}") => {}
            Err(ThreadStoreError::ThreadNotFound {
                thread_id: missing_thread_id,
            }) if missing_thread_id == thread_id => {}
            Err(ThreadStoreError::InvalidRequest { message }) => {
                return Err(ThreadReadViewError::InvalidRequest(message));
            }
            Err(ThreadStoreError::Unsupported { operation }) => {
                return Err(ThreadReadViewError::Unsupported(operation));
            }
            Err(err) => {
                return Err(ThreadReadViewError::Internal(format!(
                    "failed to read thread: {err}"
                )));
            }
        }

        let thread = self
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| {
                ThreadReadViewError::InvalidRequest(format!("thread not loaded: {thread_id}"))
            })?;
        let config_snapshot = thread.config_snapshot().await;
        if config_snapshot.ephemeral {
            return Err(ThreadReadViewError::InvalidRequest(
                "ephemeral threads do not support thread/turns/list".to_string(),
            ));
        }

        thread
            .load_history(/*include_archived*/ true)
            .await
            .map(|history| history.items)
            .map_err(|err| thread_turns_list_history_load_error(thread_id, err))
    }
}
