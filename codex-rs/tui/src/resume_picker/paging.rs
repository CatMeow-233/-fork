//! Starts listings, requests further pages, and applies background loader events.

use super::*;

impl PickerState {
    pub(super) fn start_initial_load(&mut self) {
        self.relative_time_reference = Some(Utc::now());
        self.reset_pagination();
        self.unread.clear();
        self.all_rows.clear();
        self.filtered_rows.clear();
        self.seen_rows.clear();
        self.local_cwd_matches.clear();
        self.selected = 0;
        self.pending_page_down_target = None;
        self.frozen_footer_percent = None;

        let search_token = if self.query.is_empty() {
            self.search_state = SearchState::Idle;
            None
        } else {
            let token = self.allocate_search_token();
            self.search_state = SearchState::Active { token };
            Some(token)
        };

        let request_token = self.allocate_request_token();
        self.listing_token = request_token;
        let mode = self.initial_page_mode;
        self.pagination
            .start_load(request_token, search_token, mode);
        self.request_frame();

        (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
            cursor: None,
            request_token,
            listing_token: self.listing_token,
            search_token,
            mode,
            filter_mode: self.filter_mode,
            cwd_filter: self.active_cwd_filter(),
            status: self.status,
            provider_filter: self.provider_filter.clone(),
            sort_key: self.sort_key,
        }));
    }

    pub(super) async fn handle_background_event(
        &mut self,
        event: BackgroundEvent,
    ) -> Result<Option<SessionSelection>> {
        match event {
            BackgroundEvent::PartialPage {
                listing_token,
                page,
            } => {
                if listing_token != self.listing_token {
                    return Ok(None);
                }
                self.ingest_page(page);
                self.continue_search_if_needed();
            }
            BackgroundEvent::SourceError {
                listing_token,
                next_cursor,
            } => {
                if listing_token != self.listing_token {
                    return Ok(None);
                }
                self.pagination.next_cursor = next_cursor;
                self.continue_search_if_needed();
                self.inline_error =
                    Some("Could not load one backend. Reopen the picker to retry.".into());
                self.request_frame();
            }
            BackgroundEvent::Page {
                request_token,
                search_token,
                mode,
                page,
            } => {
                let Some(pending) = self.pagination.finish_load(request_token) else {
                    return Ok(None);
                };
                self.pagination.next_page_mode = mode;
                let page_has_rows = matches!(&page, Ok(page) if !page.rows.is_empty());
                // Fall back only when the initial DB listing is unusable. Once SQLite returns
                // rows, its pagination is authoritative and an empty later page ends the list.
                let should_restart_from_store =
                    mode == PageLoadMode::StateDbOnly && self.all_rows.is_empty() && !page_has_rows;
                if should_restart_from_store {
                    let request_token = self.allocate_request_token();
                    let search_token = pending.search_token.or(search_token);
                    self.pagination.reset();
                    self.pagination.start_load(
                        request_token,
                        search_token,
                        PageLoadMode::StoreDefault,
                    );
                    (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
                        cursor: None,
                        request_token,
                        listing_token: self.listing_token,
                        search_token,
                        mode: PageLoadMode::StoreDefault,
                        filter_mode: self.filter_mode,
                        cwd_filter: self.active_cwd_filter(),
                        status: self.status,
                        provider_filter: self.provider_filter.clone(),
                        sort_key: self.sort_key,
                    }));
                    return Ok(None);
                }
                let page = match page {
                    Ok(page) => page,
                    Err(_) if !self.all_rows.is_empty() => {
                        self.pagination.next_cursor = None;
                        self.pending_page_down_target = None;
                        self.frozen_footer_percent = None;
                        self.search_state = SearchState::Idle;
                        self.inline_error = Some("Could not load more sessions".to_string());
                        self.request_frame();
                        return Ok(None);
                    }
                    Err(err) => return Err(err.into()),
                };
                self.ingest_page(page);
                self.complete_pending_page_down();
                let completed_token = self.search_state.active_token();
                self.continue_search_if_token_matches(completed_token);
            }
            BackgroundEvent::Preview { thread_id, preview } => {
                self.transcript_previews.insert(
                    thread_id,
                    match preview {
                        Ok(lines) => TranscriptPreviewState::Loaded(lines),
                        Err(_) => TranscriptPreviewState::Failed,
                    },
                );
                self.request_frame();
            }
            BackgroundEvent::Acknowledged {
                backend_id,
                thread_id,
                revision,
            } => {
                self.update_unread((backend_id, thread_id), revision);
                self.request_frame();
            }
            BackgroundEvent::Transcript {
                backend_id,
                thread_id,
                transcript,
            } => {
                let key = (backend_id, thread_id);
                if self.pending_transcript_open != Some(key)
                    || !matches!(
                        self.transcript_cells.get(&key),
                        Some(SessionTranscriptState::Loading)
                    )
                {
                    return Ok(None);
                }
                self.pending_transcript_cancellation = None;
                match transcript {
                    Ok(cells) => {
                        self.update_unread(key, cells.unread_revision.clone());
                        self.transcript_cells
                            .insert(key, SessionTranscriptState::Loaded(cells));
                        self.open_pending_transcript_if_ready();
                    }
                    Err(_) => {
                        self.transcript_cells
                            .insert(key, SessionTranscriptState::Failed);
                        self.chord_matcher.cancel();
                        self.pending_transcript_open = None;
                        self.transcript_loading_frame_shown = false;
                        self.inline_error = Some("Could not load transcript preview".to_string());
                    }
                }
                self.request_frame();
            }
            BackgroundEvent::Archive { thread_id, result } => {
                self.handle_archive_result(thread_id, result);
            }
            BackgroundEvent::Unarchive { thread_id, result } => {
                return Ok(self.handle_unarchive_result(thread_id, result));
            }
        }
        Ok(None)
    }

    pub(super) fn reset_pagination(&mut self) {
        self.pagination.reset();
        self.frozen_footer_percent = None;
    }

    pub(super) fn ingest_page(&mut self, page: PickerPage) {
        let selected_key = self
            .filtered_rows
            .get(self.selected)
            .and_then(Row::seen_key);
        let pending_page_down_distance = self
            .pending_page_down_target
            .map(|target| target.saturating_sub(self.selected));
        let PickerPage {
            rows,
            mut read_states,
            history_modes,
            next_cursor,
            num_scanned_files,
            reached_scan_cap,
        } = page;
        self.pagination
            .complete_page(next_cursor, num_scanned_files, reached_scan_cap);
        for mut row in rows {
            if let Some(thread_id) = row.thread_id {
                row.history_mode = history_modes.get(&thread_id).copied();
                self.update_unread(
                    (row.backend_id.unwrap_or(self.backend_id), thread_id),
                    unread_revision(read_states.remove(&thread_id.to_string())),
                );
            }
            if let Some(cwd) = row.cwd.as_ref()
                && !self.local_cwd_matches.contains_key(cwd)
            {
                let matches = self.row_matches_local_cwd(&row);
                self.local_cwd_matches.insert(cwd.clone(), matches);
            }
            if let Some(seen_key) = row.seen_key() {
                if self.seen_rows.insert(seen_key.clone()) {
                    self.all_rows.push(row);
                } else if let Some(existing) = self.all_rows.iter_mut().find(|existing| {
                    existing.seen_key().as_ref() == Some(&seen_key)
                        && existing.backend_id == row.backend_id
                        && existing.thread_id == row.thread_id
                }) {
                    // Preserve the pre-catalog behavior where metadata from a later page wins.
                    existing.history_mode = row.history_mode;
                }
            } else {
                self.all_rows.push(row);
            }
        }

        let sort_key = self.sort_key;
        self.all_rows.sort_by_key(|row| {
            std::cmp::Reverse(match sort_key {
                ThreadSortKey::CreatedAt => row.created_at,
                ThreadSortKey::UpdatedAt
                | ThreadSortKey::RecencyAt
                | ThreadSortKey::SectionPosition => row.updated_at.or(row.created_at),
            })
        });
        self.apply_filter();
        if let Some(selected_key) = selected_key
            && let Some(index) = self
                .filtered_rows
                .iter()
                .position(|row| row.seen_key().as_ref() == Some(&selected_key))
        {
            self.selected = index;
            if let Some(distance) = pending_page_down_distance {
                self.pending_page_down_target = Some(index.saturating_add(distance));
            }
            self.ensure_selected_visible();
            self.request_frame();
        }
    }

    fn complete_pending_page_down(&mut self) {
        let Some(target) = self.pending_page_down_target else {
            return;
        };
        if self.filtered_rows.is_empty() {
            return;
        }

        let max_index = self.filtered_rows.len().saturating_sub(1);
        if target > max_index && self.pagination.next_cursor.is_some() {
            self.load_more_if_needed(LoadTrigger::Scroll);
            return;
        }

        self.pending_page_down_target = None;
        self.selected = target.min(max_index);
        self.ensure_selected_visible();
        self.maybe_load_more_for_scroll();
        self.request_frame();
    }

    pub(super) fn maybe_load_more_for_scroll(&mut self) {
        if self.pagination.is_loading() || self.pagination.next_cursor.is_none() {
            return;
        }
        if self.filtered_rows.is_empty() {
            return;
        }
        let remaining = self.filtered_rows.len().saturating_sub(self.selected + 1);
        if remaining <= LOAD_NEAR_THRESHOLD {
            self.load_more_if_needed(LoadTrigger::Scroll);
        }
    }

    pub(super) fn load_more_if_needed(&mut self, trigger: LoadTrigger) {
        let Some((cursor, mode)) = self.pagination.next_page() else {
            return;
        };
        self.freeze_footer_percent();
        let request_token = self.allocate_request_token();
        let search_token = match trigger {
            LoadTrigger::Scroll => None,
            LoadTrigger::Search { token } => Some(token),
        };
        self.pagination
            .start_load(request_token, search_token, mode);
        self.request_frame();

        (self.picker_loader)(PickerLoadRequest::Page(PageLoadRequest {
            cursor: Some(cursor),
            request_token,
            listing_token: self.listing_token,
            search_token,
            mode,
            filter_mode: self.filter_mode,
            cwd_filter: self.active_cwd_filter(),
            status: self.status,
            provider_filter: self.provider_filter.clone(),
            sort_key: self.sort_key,
        }));
    }

    fn freeze_footer_percent(&mut self) {
        let list_height = self.view_rows.unwrap_or_default().min(u16::MAX as usize) as u16;
        self.frozen_footer_percent = Some(picker_footer_scroll_percent(self, list_height));
    }

    fn allocate_request_token(&mut self) -> usize {
        let token = self.next_request_token;
        self.next_request_token = self.next_request_token.wrapping_add(1);
        token
    }

    pub(super) fn allocate_search_token(&mut self) -> usize {
        let token = self.next_search_token;
        self.next_search_token = self.next_search_token.wrapping_add(1);
        token
    }
}
