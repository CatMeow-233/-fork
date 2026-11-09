//! Thread list, search, loaded-list, and conversation summary requests.

use super::*;

impl ThreadRequestProcessor {
    pub(super) async fn thread_list_response_inner(
        &self,
        params: ThreadListParams,
    ) -> Result<ThreadListResponse, JSONRPCErrorError> {
        let ThreadListParams {
            current_account_only,
            cursor,
            limit,
            excluded_thread_ids,
            sort_key,
            sort_direction,
            model_providers,
            source_kinds,
            originators,
            archived,
            section_id,
            project_id,
            cwd,
            use_state_db_only,
            search_term,
            parent_thread_id,
            ancestor_thread_id,
            attached_to_thread_id,
        } = params;
        let excluded_thread_ids = excluded_thread_ids.unwrap_or_default();
        if excluded_thread_ids.len() > THREAD_LIST_MAX_EXCLUDED_IDS {
            return Err(invalid_params(format!(
                "excludedThreadIds accepts at most {THREAD_LIST_MAX_EXCLUDED_IDS} entries"
            )));
        }
        let excluded_thread_ids = excluded_thread_ids
            .iter()
            .map(|id| {
                ThreadId::from_string(id)
                    .map_err(|err| invalid_params(format!("invalid excluded thread id: {err}")))
            })
            .collect::<Result<HashSet<_>, _>>()?;
        if originators
            .as_ref()
            .is_some_and(|values| !values.is_empty())
        {
            return Err(invalid_params(
                "originator filtering is not supported by the local app-server",
            ));
        }
        if project_id.is_some() && !self.thread_store.supports_projects() {
            return Err(unsupported_thread_store_operation("projects"));
        }
        if let Some(Some(project_id)) = project_id.as_ref() {
            if project_id.is_empty() {
                return Err(invalid_params("projectId must not be empty"));
            }
            match self.thread_store.read_project(project_id.clone()).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(invalid_params(format!("project not found: {project_id}")));
                }
                Err(ThreadStoreError::Unsupported { operation }) => {
                    return Err(unsupported_thread_store_operation(operation));
                }
                Err(err) => {
                    return Err(internal_error(format!("failed to read project: {err}")));
                }
            }
        }
        let cwd_filters = normalize_thread_list_cwd_filters(cwd)?;
        let relation_filter = match (parent_thread_id, ancestor_thread_id, attached_to_thread_id) {
            (Some(_), Some(_), None) => {
                return Err(invalid_request(
                    "parentThreadId and ancestorThreadId are mutually exclusive",
                ));
            }
            (Some(parent_thread_id), None, None) => {
                Some(StoreThreadRelationFilter::DirectChildrenOf(
                    ThreadId::from_string(&parent_thread_id).map_err(|err| {
                        invalid_request(format!("invalid parent thread id: {err}"))
                    })?,
                ))
            }
            (None, Some(ancestor_thread_id), None) => {
                Some(StoreThreadRelationFilter::DescendantsOf(
                    ThreadId::from_string(&ancestor_thread_id).map_err(|err| {
                        invalid_request(format!("invalid ancestor thread id: {err}"))
                    })?,
                ))
            }
            (None, None, Some(parent)) => {
                if self.state_db.is_none() || !self.thread_store.as_any().is::<LocalThreadStore>() {
                    return Err(unsupported_thread_store_operation("attached threads"));
                }
                Some(StoreThreadRelationFilter::AttachedThreadsOf(
                    ThreadId::from_string(&parent).map_err(|err| {
                        invalid_request(format!("invalid attached thread parent id: {err}"))
                    })?,
                ))
            }
            (_, _, Some(_)) => {
                return Err(invalid_request(
                    "attachedToThreadId cannot be combined with parentThreadId or ancestorThreadId",
                ));
            }
            (None, None, None) => None,
        };

        let requested_page_size = limit
            .map(|value| value as usize)
            .unwrap_or(THREAD_LIST_DEFAULT_LIMIT)
            .clamp(1, THREAD_LIST_MAX_LIMIT);
        let store_sort_key = match sort_key.unwrap_or(ThreadSortKey::CreatedAt) {
            ThreadSortKey::CreatedAt => StoreThreadSortKey::CreatedAt,
            ThreadSortKey::UpdatedAt => StoreThreadSortKey::UpdatedAt,
            ThreadSortKey::RecencyAt => StoreThreadSortKey::RecencyAt,
            ThreadSortKey::SectionPosition => StoreThreadSortKey::SectionPosition,
        };
        let sort_direction = sort_direction.unwrap_or(match store_sort_key {
            StoreThreadSortKey::SectionPosition => SortDirection::Asc,
            StoreThreadSortKey::CreatedAt
            | StoreThreadSortKey::UpdatedAt
            | StoreThreadSortKey::RecencyAt => SortDirection::Desc,
        });
        let (stored_threads, next_cursor) = self
            .list_threads_common(
                requested_page_size,
                cursor,
                store_sort_key,
                sort_direction,
                ThreadListFilters {
                    account: self.local_history_account(current_account_only),
                    model_providers,
                    source_kinds,
                    archived: archived.unwrap_or(false),
                    section_id,
                    project_id,
                    cwd_filters,
                    search_term,
                    use_state_db_only,
                    relation_filter,
                    excluded_thread_ids,
                },
            )
            .await?;
        let backwards_cursor = stored_threads.first().and_then(|thread| {
            thread_backwards_cursor_for_sort_key(thread, store_sort_key, sort_direction)
        });
        let mut data = Vec::with_capacity(stored_threads.len());
        let fallback_provider = self.config.model_provider_id.clone();

        for stored_thread in stored_threads {
            let (thread, _) = thread_from_stored_thread(
                stored_thread,
                fallback_provider.as_str(),
                &self.config.cwd,
            );
            data.push(thread);
        }

        enrich_loaded_threads(
            &self.thread_manager,
            &self.thread_watch_manager,
            data.as_mut_slice(),
            |thread| thread,
        )
        .await;
        let read_states = super::thread_read_state::snapshots(self.state_db.as_ref(), &data).await;
        Ok(ThreadListResponse {
            data,
            read_states,
            next_cursor,
            backwards_cursor,
        })
    }

    pub(super) async fn thread_search_response_inner(
        &self,
        params: ThreadSearchParams,
    ) -> Result<ThreadSearchResponse, JSONRPCErrorError> {
        let ThreadSearchParams {
            current_account_only,
            cursor,
            limit,
            sort_key,
            sort_direction,
            source_kinds,
            archived,
            search_term,
        } = params;
        let search_term = search_term.trim().to_string();
        let search_term = (!search_term.is_empty())
            .then_some(search_term)
            .ok_or_else(|| invalid_request("thread/search requires a non-empty searchTerm"))?;
        let requested_page_size = limit
            .map(|value| value as usize)
            .unwrap_or(THREAD_LIST_DEFAULT_LIMIT)
            .clamp(1, THREAD_LIST_MAX_LIMIT);
        let store_sort_key = match sort_key.unwrap_or(ThreadSearchSortKey::CreatedAt) {
            ThreadSearchSortKey::CreatedAt => StoreThreadSortKey::CreatedAt,
            ThreadSearchSortKey::UpdatedAt => StoreThreadSortKey::UpdatedAt,
            ThreadSearchSortKey::RecencyAt => StoreThreadSortKey::RecencyAt,
        };
        let store_sort_direction = sort_direction.unwrap_or(SortDirection::Desc);
        let (allowed_sources, source_kind_filter) = compute_source_filters(source_kinds);
        let mut cursor_obj = cursor;
        let mut last_cursor = cursor_obj.clone();
        let mut remaining = requested_page_size;
        let account = self.local_history_account(current_account_only);
        let mut search_results = Vec::with_capacity(requested_page_size);
        let mut next_cursor = None;

        while remaining > 0 {
            let page = self
                .thread_store
                .search_threads(StoreSearchThreadsParams {
                    account: account.clone(),
                    page_size: remaining.min(THREAD_LIST_MAX_LIMIT),
                    cursor: cursor_obj.clone(),
                    sort_key: store_sort_key,
                    sort_direction: match store_sort_direction {
                        SortDirection::Asc => StoreSortDirection::Asc,
                        SortDirection::Desc => StoreSortDirection::Desc,
                    },
                    allowed_sources: allowed_sources.clone(),
                    archived: archived.unwrap_or(false),
                    search_term: search_term.clone(),
                })
                .await
                .map_err(thread_store_list_error)?;

            for result in page.items {
                let source = with_thread_spawn_agent_metadata(
                    result.thread.source.clone(),
                    result.thread.agent_nickname.clone(),
                    result.thread.agent_role.clone(),
                );
                if source_kind_filter
                    .as_ref()
                    .is_none_or(|filter| source_kind_matches(&source, filter))
                {
                    search_results.push(result);
                    if search_results.len() >= requested_page_size {
                        break;
                    }
                }
            }

            remaining = requested_page_size.saturating_sub(search_results.len());
            next_cursor = page.next_cursor;
            if remaining == 0 {
                break;
            }

            let Some(cursor_val) = next_cursor.clone() else {
                break;
            };
            if last_cursor.as_ref() == Some(&cursor_val) {
                next_cursor = None;
                break;
            }
            last_cursor = Some(cursor_val.clone());
            cursor_obj = Some(cursor_val);
        }

        let backwards_cursor = search_results.first().and_then(|result| {
            thread_backwards_cursor_for_sort_key(
                &result.thread,
                store_sort_key,
                store_sort_direction,
            )
        });
        let fallback_provider = self.config.model_provider_id.clone();
        let mut data = Vec::with_capacity(search_results.len());
        for result in search_results {
            let (thread, _) = thread_from_stored_thread(
                result.thread,
                fallback_provider.as_str(),
                &self.config.cwd,
            );
            data.push(ThreadSearchResult {
                thread,
                snippet: result.snippet,
            });
        }

        enrich_loaded_threads(
            &self.thread_manager,
            &self.thread_watch_manager,
            data.as_mut_slice(),
            |result| &mut result.thread,
        )
        .await;

        Ok(ThreadSearchResponse {
            data,
            next_cursor,
            backwards_cursor,
        })
    }

    pub(super) async fn thread_loaded_list_response_inner(
        &self,
        params: ThreadLoadedListParams,
    ) -> Result<ThreadLoadedListResponse, JSONRPCErrorError> {
        let ThreadLoadedListParams { cursor, limit } = params;
        let mut data = Vec::new();
        for thread_id in self.thread_manager.list_thread_ids().await {
            if let Ok(thread) = self.thread_manager.get_thread(thread_id).await
                && thread
                    .thread_extension_data()
                    .get::<SampleThread>()
                    .is_none()
            {
                data.push(thread_id.to_string());
            }
        }

        if data.is_empty() {
            return Ok(ThreadLoadedListResponse {
                data,
                next_cursor: None,
            });
        }

        data.sort();
        let total = data.len();
        let start = match cursor {
            Some(cursor) => {
                let cursor = match ThreadId::from_string(&cursor) {
                    Ok(id) => id.to_string(),
                    Err(_) => return Err(invalid_request(format!("invalid cursor: {cursor}"))),
                };
                match data.binary_search(&cursor) {
                    Ok(idx) => idx + 1,
                    Err(idx) => idx,
                }
            }
            None => 0,
        };

        let effective_limit = limit.unwrap_or(total as u32).max(1) as usize;
        let end = start.saturating_add(effective_limit).min(total);
        let page = data[start..end].to_vec();
        let next_cursor = page.last().filter(|_| end < total).cloned();

        Ok(ThreadLoadedListResponse {
            data: page,
            next_cursor,
        })
    }

    pub(super) async fn get_thread_summary_response_inner(
        &self,
        params: GetConversationSummaryParams,
    ) -> Result<GetConversationSummaryResponse, JSONRPCErrorError> {
        let fallback_provider = self.config.model_provider_id.as_str();
        let read_result = match params {
            GetConversationSummaryParams::ThreadId { conversation_id } => self
                .thread_store
                .read_thread(StoreReadThreadParams {
                    thread_id: conversation_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
                .map_err(|err| conversation_summary_thread_id_read_error(conversation_id, err)),
            GetConversationSummaryParams::RolloutPath { rollout_path } => {
                let Some(local_thread_store) = self
                    .thread_store
                    .as_any()
                    .downcast_ref::<LocalThreadStore>()
                else {
                    return Err(invalid_request(
                        "rollout path queries are only supported with the local thread store",
                    ));
                };

                local_thread_store
                    .read_thread_by_rollout_path(
                        rollout_path.clone(),
                        /*include_archived*/ true,
                        /*include_history*/ false,
                    )
                    .await
                    .map_err(|err| conversation_summary_rollout_path_read_error(&rollout_path, err))
            }
        };

        let stored_thread = read_result?;
        let summary = summary_from_stored_thread(stored_thread, fallback_provider);
        Ok(GetConversationSummaryResponse { summary })
    }

    /// Local discovery matches each recorded creator ID to the current login. Threads with
    /// no creator IDs or predating enforcement are shared; API-key logins see all local history.
    fn local_history_account(&self, current_account_only: bool) -> Option<LocalHistoryAccount> {
        if !current_account_only || !self.thread_store.as_any().is::<LocalThreadStore>() {
            return None;
        }
        let auth = self.auth_manager.auth_cached();
        if auth.as_ref().is_some_and(CodexAuth::is_api_key_auth) {
            return None;
        }
        Some(LocalHistoryAccount {
            user_id: auth.as_ref().and_then(CodexAuth::get_chatgpt_user_id),
            account_id: auth.as_ref().and_then(CodexAuth::get_account_id),
        })
    }

    async fn list_threads_common(
        &self,
        requested_page_size: usize,
        cursor: Option<String>,
        sort_key: StoreThreadSortKey,
        sort_direction: SortDirection,
        filters: ThreadListFilters,
    ) -> Result<(Vec<StoredThread>, Option<String>), JSONRPCErrorError> {
        let ThreadListFilters {
            account,
            model_providers,
            source_kinds,
            archived,
            section_id,
            project_id,
            cwd_filters,
            search_term,
            use_state_db_only,
            relation_filter,
            excluded_thread_ids,
        } = filters;
        let mut cursor_obj = cursor;
        let mut seen_cursors: HashSet<String> = cursor_obj.iter().cloned().collect();
        let mut remaining = requested_page_size;
        let mut items = Vec::with_capacity(requested_page_size);
        let mut next_cursor: Option<String> = None;

        let model_provider_filter = match model_providers {
            Some(providers) => {
                if providers.is_empty() {
                    None
                } else {
                    Some(providers)
                }
            }
            None if relation_filter.is_some() => None,
            None => Some(vec![self.config.model_provider_id.clone()]),
        };
        let (allowed_sources_vec, source_kind_filter) =
            if relation_filter.is_some() && source_kinds.is_none() {
                (Vec::new(), None)
            } else {
                compute_source_filters(source_kinds)
            };
        let allowed_sources = allowed_sources_vec.as_slice();
        let store_sort_direction = match sort_direction {
            SortDirection::Asc => StoreSortDirection::Asc,
            SortDirection::Desc => StoreSortDirection::Desc,
        };

        while remaining > 0 {
            let page_size = remaining.min(THREAD_LIST_MAX_LIMIT);
            let page = self
                .thread_store
                .list_threads(StoreListThreadsParams {
                    page_size,
                    cursor: cursor_obj.clone(),
                    sort_key,
                    sort_direction: store_sort_direction,
                    allowed_sources: allowed_sources.to_vec(),
                    model_providers: model_provider_filter.clone(),
                    cwd_filters: cwd_filters.clone(),
                    archived,
                    section: section_id.clone(),
                    project_id: project_id.clone(),
                    search_term: search_term.clone(),
                    use_state_db_only,
                    relation_filter,
                })
                .await
                .map_err(thread_store_list_error)?;

            let page_thread_ids: Vec<_> = page.items.iter().map(|item| item.thread_id).collect();
            let mut original_paths = None;
            let mut filtered = Vec::with_capacity(page.items.len());
            for it in page.items {
                if let Some(account) = &account
                    && !account
                        .includes(
                            &self.config.codex_home,
                            it.thread_id,
                            it.rollout_path.as_deref(),
                            &page_thread_ids,
                            &mut original_paths,
                        )
                        .await
                {
                    continue;
                }
                let source = with_thread_spawn_agent_metadata(
                    it.source.clone(),
                    it.agent_nickname.clone(),
                    it.agent_role.clone(),
                );
                if !excluded_thread_ids.contains(&it.thread_id)
                    && source_kind_filter
                        .as_ref()
                        .is_none_or(|filter| source_kind_matches(&source, filter))
                    && cwd_filters.as_ref().is_none_or(|expected_cwds| {
                        expected_cwds.iter().any(|expected_cwd| {
                            path_utils::paths_match_after_normalization(&it.cwd, expected_cwd)
                        })
                    })
                {
                    filtered.push(it);
                    if filtered.len() >= remaining {
                        break;
                    }
                }
            }
            items.extend(filtered);
            remaining = requested_page_size.saturating_sub(items.len());

            next_cursor = page.next_cursor;
            let Some(cursor_val) = next_cursor.clone() else {
                break;
            };
            // Check full pages too: returning a repeated cursor would loop on the next request.
            if !seen_cursors.insert(cursor_val.clone()) {
                return Err(internal_error("thread listing returned a repeated cursor"));
            }
            if remaining == 0 {
                break;
            }
            cursor_obj = Some(cursor_val);
        }

        Ok((items, next_cursor))
    }
}
