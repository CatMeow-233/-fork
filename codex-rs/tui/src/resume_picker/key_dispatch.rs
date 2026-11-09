//! Routes picker key chords, keys, and pastes to selection, query, and toolbar actions.

use super::*;

impl PickerState {
    pub(super) fn route_key_chord(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        if matches!(&self.overlay, Some(Overlay::Transcript(overlay)) if overlay.owns_interaction_key(key))
        {
            self.chord_matcher.cancel();
            return Some(key);
        }
        let context = if matches!(&self.overlay, Some(Overlay::Transcript(overlay)) if overlay.is_search_editing())
        {
            crate::keymap::KeymapContext::Editor
        } else if self.overlay.is_some() {
            crate::keymap::KeymapContext::Pager
        } else {
            crate::keymap::KeymapContext::List
        };
        match self.chord_matcher.advance(
            key,
            &self.keymap.chords,
            crate::keymap::KeymapContextSet::new(context),
        ) {
            crate::keymap::KeyChordMatch::PassThrough => Some(key),
            crate::keymap::KeyChordMatch::Completed(dispatch_event) => Some(dispatch_event),
            crate::keymap::KeyChordMatch::Pending(_)
            | crate::keymap::KeyChordMatch::Cancelled
            | crate::keymap::KeyChordMatch::Ignored => None,
        }
    }

    fn handle_transcript_loading_key(&mut self, key: KeyEvent) -> Option<SessionSelection> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => Some(SessionSelection::Exit),
            key if self.keymap.list.cancel.is_pressed(key) => {
                if let Some(key) = self.pending_transcript_open.take()
                    && matches!(
                        self.transcript_cells.get(&key),
                        Some(SessionTranscriptState::Loading)
                    )
                {
                    self.transcript_cells.remove(&key);
                }
                if let Some(cancellation) = self.pending_transcript_cancellation.take() {
                    let _ = cancellation.send(());
                }
                self.transcript_loading_frame_shown = false;
                self.request_frame();
                None
            }
            _ => None,
        }
    }

    pub(super) async fn handle_key(&mut self, key: KeyEvent) -> Result<Option<SessionSelection>> {
        self.inline_error = None;
        if self.is_transcript_loading() {
            return Ok(self.handle_transcript_loading_key(key));
        }
        if !self.keymap.list.page_down.is_pressed(key) {
            self.pending_page_down_target = None;
        }
        // The session picker is always searchable, so plain text belongs to
        // the query first. Modified list bindings still route through the
        // runtime keymap below.
        let allow_plain_char_navigation = !is_plain_text_key_event(key);
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(SessionSelection::Exit));
            }
            _ if self.keymap.list.cancel.is_pressed(key) => {
                if self.query.is_empty() {
                    return Ok(Some(SessionSelection::StartFresh));
                }
                self.clear_query_preserving_selection();
            }
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_selected_transcript();
            }
            KeyEvent {
                code: KeyCode::Char('e'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_selected_expansion();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0014}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^T */ => {
                self.open_selected_transcript();
            }
            KeyEvent {
                code: KeyCode::Char('\u{0005}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^E */ => {
                self.toggle_selected_expansion();
            }
            KeyEvent {
                code: KeyCode::Char('o'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_density().await;
            }
            KeyEvent {
                code: KeyCode::Char('\u{000f}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^O */ => {
                self.toggle_density().await;
            }
            _ if self.keymap.list.accept.is_pressed(key)
                && !matches!(self.archive_state, archive::ArchiveState::Idle) => {}
            _ if self.keymap.list.accept.is_pressed(key) => {
                if let Some(row) = self.filtered_rows.get(self.selected) {
                    let path = row.path.clone();
                    if let Some(thread_id) = row.thread_id {
                        if self.status == SessionStatus::Archived {
                            self.request_unarchive(thread_id, row.backend_id.unwrap_or(self.backend_id));
                            return Ok(None);
                        }
                        return Ok(Some(self.action.selection(SessionTarget {
                            backend_id: row.backend_id.unwrap_or(self.backend_id),
                            path,
                            thread_id,
                            cwd: row.cwd.clone(),
                            history_mode: row.history_mode,
                        })));
                    }
                    self.inline_error = Some(match path {
                        Some(path) => {
                            format!("Failed to read session metadata from {}", path.display())
                        }
                        None => {
                            String::from("Failed to read session metadata from selected session")
                        }
                    });
                    self.request_frame();
                }
            }
            _ if allow_plain_char_navigation && self.keymap.list.move_up.is_pressed(key) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.ensure_selected_visible();
                }
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.keymap.list.move_down.is_pressed(key) => {
                if self.selected + 1 < self.filtered_rows.len() {
                    self.selected += 1;
                    self.ensure_selected_visible();
                }
                self.maybe_load_more_for_scroll();
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.keymap.list.page_up.is_pressed(key) => {
                let step = self.view_rows.unwrap_or(10).max(1);
                if self.selected > 0 {
                    self.selected = self.selected.saturating_sub(step);
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            }
            _ if allow_plain_char_navigation && self.keymap.list.jump_top.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    self.selected = 0;
                    self.ensure_selected_visible();
                    self.request_frame();
                }
            _ if allow_plain_char_navigation && self.keymap.list.jump_bottom.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    self.selected = self.filtered_rows.len().saturating_sub(1);
                    self.ensure_selected_visible();
                    self.maybe_load_more_for_scroll();
                    self.request_frame();
                }
            _ if allow_plain_char_navigation && self.keymap.list.page_down.is_pressed(key)
                && !self.filtered_rows.is_empty() => {
                    let step = self.view_rows.unwrap_or(10).max(1);
                    let target = self.selected.saturating_add(step);
                    let max_index = self.filtered_rows.len().saturating_sub(1);
                    if target > max_index && self.pagination.next_cursor.is_some() {
                        self.pending_page_down_target = Some(target);
                        self.load_more_if_needed(LoadTrigger::Scroll);
                    } else {
                        self.selected = target.min(max_index);
                        self.ensure_selected_visible();
                        self.maybe_load_more_for_scroll();
                    }
                    self.request_frame();
                }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                self.focus_next_toolbar_control();
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::BackTab,
                ..
            } => {
                self.focus_previous_toolbar_control();
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.keymap.list.move_left.is_pressed(key) => {
                self.change_focused_toolbar_value(ToolbarValueDirection::Previous);
                self.request_frame();
            }
            _ if allow_plain_char_navigation && self.keymap.list.move_right.is_pressed(key) => {
                self.change_focused_toolbar_value(ToolbarValueDirection::Next);
                self.request_frame();
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                let mut new_query = self.query.clone();
                new_query.pop();
                self.set_query(new_query);
            }
            _ if self.archive_shortcut_available()
                && crate::key_hint::ctrl(KeyCode::Char('a')).is_press(key) =>
            {
                self.request_archive_for_selected_session();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            }
                // basic text input for search
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT)
                => {
                    let mut new_query = self.query.clone();
                    new_query.push(c);
                    self.set_query(new_query);
                }
            _ => {}
        }
        Ok(None)
    }

    pub(super) fn handle_paste(&mut self, pasted: String) {
        if self.is_transcript_loading() {
            return;
        }
        let Some(pasted) = normalize_pasted_search_query(&pasted) else {
            return;
        };
        let mut new_query = self.query.clone();
        if !new_query.is_empty() && !new_query.ends_with(char::is_whitespace) {
            new_query.push(' ');
        }
        new_query.push_str(&pasted);
        self.set_query(new_query);
    }

    /// Cycles the sort order between creation time and last-updated time.
    ///
    /// Triggers a full reload because the backend must re-sort all sessions.
    /// The existing `all_rows` are cleared and pagination restarts from the
    /// beginning with the new sort key.
    fn toggle_sort_key(&mut self) {
        self.sort_key = match self.sort_key {
            ThreadSortKey::CreatedAt => ThreadSortKey::UpdatedAt,
            ThreadSortKey::UpdatedAt
            | ThreadSortKey::RecencyAt
            | ThreadSortKey::SectionPosition => ThreadSortKey::CreatedAt,
        };
        self.start_initial_load();
    }

    pub(super) fn change_filter_mode(&mut self, direction: ToolbarValueDirection) {
        let next_filter_mode = self.filter_mode.change(
            direction,
            self.filter_cwd.as_deref(),
            self.repo_filter_available,
        );
        if self.filter_mode == next_filter_mode {
            return;
        }
        self.filter_mode = next_filter_mode;
        self.start_initial_load();
    }

    pub(super) fn toggle_status(&mut self) {
        self.status = match self.status {
            SessionStatus::Active => SessionStatus::Archived,
            SessionStatus::Archived => SessionStatus::Active,
        };
        self.start_initial_load();
    }

    pub(super) fn active_cwd_filter(&self) -> Option<PathBuf> {
        match self.filter_mode {
            SessionFilterMode::Cwd | SessionFilterMode::Repo => self.filter_cwd.clone(),
            SessionFilterMode::All => None,
        }
    }

    fn focus_previous_toolbar_control(&mut self) {
        self.toolbar_focus = self.toolbar_focus.previous(self.action);
    }

    fn focus_next_toolbar_control(&mut self) {
        self.toolbar_focus = self.toolbar_focus.next(self.action);
    }

    fn change_focused_toolbar_value(&mut self, direction: ToolbarValueDirection) {
        match self.toolbar_focus {
            ToolbarControl::Sort => self.toggle_sort_key(),
            ToolbarControl::Filter => self.change_filter_mode(direction),
            ToolbarControl::Status => self.toggle_status(),
        }
    }

    async fn toggle_density(&mut self) {
        self.density = self.density.toggle();
        self.ensure_selected_visible();
        if let Err(err) = self.persist_density().await {
            warn!(error = %err, "failed to persist session picker view mode");
            self.inline_error = Some(format!("Failed to save view mode: {err}"));
        }
        self.request_frame();
    }

    async fn persist_density(&self) -> Result<()> {
        let Some(persistence) = &self.view_persistence else {
            return Ok(());
        };

        ConfigEditsBuilder::new(&persistence.codex_home)
            .set_session_picker_view(SessionPickerViewMode::from(self.density))
            .apply()
            .await
            .map_err(|err| color_eyre::eyre::eyre!("failed to write config.toml: {err}"))?;

        Ok(())
    }
}
