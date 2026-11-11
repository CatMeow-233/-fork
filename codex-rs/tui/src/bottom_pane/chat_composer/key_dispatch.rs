//! Top-level key routing: paste-burst flushing, popup-specific key handlers, and the no-popup path.

use super::*;

impl ChatComposer {
    /// Flushes any due paste-burst state.
    ///
    /// Call this from a UI tick to turn paste-burst transient state into explicit textarea edits:
    ///
    /// - If a burst times out, integrate it via [`Self::apply_paste`]. Its key events have already
    ///   been classified; [`Self::handle_paste`] classifies explicit terminal pastes instead.
    /// - If only the first ASCII char was held (flicker suppression) and no burst followed, emit it
    ///   as normal typed input.
    ///
    /// This also allows a single "held" ASCII char to render even when it turns out not to be part
    /// of a paste burst.
    pub(crate) fn flush_paste_burst_if_due(&mut self) -> bool {
        self.handle_paste_burst_flush(tokio::time::Instant::now().into_std())
    }

    /// Returns whether the composer is currently in any paste-burst related transient state.
    ///
    /// This includes actively buffering, having a non-empty burst buffer, or holding the first
    /// ASCII char for flicker suppression.
    pub(crate) fn is_in_paste_burst(&self) -> bool {
        self.draft.paste_burst.is_active()
    }

    /// Returns a delay that reliably exceeds the paste-burst timing threshold.
    ///
    /// Use this in tests to avoid boundary flakiness around the `PasteBurst` timeout.
    pub(crate) fn recommended_paste_flush_delay() -> Duration {
        PasteBurst::recommended_flush_delay()
    }

    /// Integrate results from an asynchronous file search.
    pub(crate) fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        // Only apply if user is still editing a token starting with `query`.
        let current_opt = if self.mentions_v2_enabled {
            self.current_mentions_v2_token()
        } else {
            Self::current_at_token(&self.draft.textarea)
        };
        let Some(current_token) = current_opt else {
            return;
        };

        if !current_token.starts_with(&query) {
            return;
        }

        match &mut self.popups.active {
            ActivePopup::File(popup) => {
                popup.set_matches(&query, matches);
            }
            ActivePopup::MentionV2(popup) => {
                popup.set_file_matches(&query, matches);
            }
            _ => {}
        }
    }

    pub(super) fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix = 0usize;

        for (placeholder, _) in &self.draft.pending_pastes {
            if placeholder == &base {
                max_suffix = max_suffix.max(1);
                continue;
            }
            if let Some(suffix) = placeholder.strip_prefix(&prefix)
                && let Ok(value) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(value);
            }
        }

        if max_suffix == 0 {
            base
        } else {
            format!("{base} #{}", max_suffix + 1)
        }
    }

    /// Handle a key event coming from the main UI.
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if !self.draft.input_enabled {
            return (InputResult::None, false);
        }

        if matches!(key_event.kind, KeyEventKind::Release) {
            return (InputResult::None, false);
        }

        let is_char = matches!(key_event.code, KeyCode::Char(_));
        if !is_char {
            if self.draft.skill_completion_space.is_some() && self.draft.paste_burst.is_active() {
                self.handle_paste_burst_flush(tokio::time::Instant::now().into_std());
            }
            self.draft.skill_completion_space = None;
        }

        if self.history_search.is_none()
            && !self.popups.active()
            && self.config.slash_commands_enabled
            && self.is_empty()
            && self.draft.textarea.wants_vim_search_key(key_event)
            && !self.draft.textarea.is_vim_operator_pending()
            && key_event.code == KeyCode::Char('/')
            && key_event.modifiers.is_empty()
        {
            self.draft.textarea.enter_vim_insert_mode();
        }

        self.draft.textarea_state.get_mut().follow_cursor();
        let before = self.before_sparkle_key(key_event);
        let result = self.handle_key_event_inner(key_event);
        if is_char && !self.draft.paste_burst.is_active() {
            self.draft.skill_completion_space = None;
        }
        self.after_sparkle_key(before, &result.0);
        result
    }

    fn handle_key_event_inner(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.history_search.is_none()
            && !self.popups.active()
            && self.draft.textarea.wants_vim_search_key(key_event)
        {
            return self.handle_input_basic(key_event);
        }

        if self.history_search.is_some() {
            return self.handle_history_search_key(key_event);
        }

        if self.handle_vim_history_key(key_event) {
            return (InputResult::None, true);
        }

        if Self::is_history_search_key(&key_event, &self.history_search_previous_keys) {
            return self.begin_history_search();
        }

        if self.handle_paste_tab(key_event, tokio::time::Instant::now().into_std()) {
            return (InputResult::None, true);
        }

        let result = match &mut self.popups.active {
            ActivePopup::Command(_) => self.handle_key_event_with_slash_popup(key_event),
            ActivePopup::File(_) => self.handle_key_event_with_file_popup(key_event),
            ActivePopup::Skill(_) => self.handle_key_event_with_skill_popup(key_event),
            ActivePopup::MentionV2(_) => self.handle_key_event_with_mentions_v2_popup(key_event),
            ActivePopup::None => self.handle_key_event_without_popup(key_event),
        };
        self.reset_vim_mode_after_successful_dispatch(&result.0);
        // Update (or hide/show) popup after processing the key.
        self.sync_popups();
        result
    }

    /// Whether a popup or query owns input.
    pub(crate) fn popup_active(&self) -> bool {
        self.query_active() || self.popups.active()
    }

    pub(crate) fn query_active(&self) -> bool {
        self.history_search.is_some() || self.draft.textarea.vim_query().is_some()
    }

    /// Handle key events when file search popup is visible.
    fn handle_key_event_with_file_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_empty_prompt_shortcut(&key_event) {
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            let next_mode = esc_hint_mode(self.footer.mode, self.is_task_running);
            if next_mode != self.footer.mode {
                self.footer.mode = next_mode;
                return (InputResult::None, true);
            }
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
        let ActivePopup::File(popup) = &mut self.popups.active else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some((range, query)) = completion_target::current_prefixed_token_range(
                    &self.draft.textarea,
                    '@',
                    /*allow_empty*/ false,
                ) {
                    self.popups.dismissed_file_token =
                        Some(DismissedToken::new(&self.draft.textarea, range, query));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                let Some(sel) = popup.selected_match() else {
                    self.popups.active = ActivePopup::None;
                    return if key_event.code == KeyCode::Enter {
                        self.handle_key_event_without_popup(key_event)
                    } else {
                        (InputResult::None, true)
                    };
                };

                let sel_path = sel.to_string_lossy().to_string();
                if let Some((token_range, _)) =
                    self.current_editable_at_token_range_with_options(/*allow_empty*/ false)
                {
                    self.insert_selected_file_path(token_range, &sel_path);
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        }
    }

    /// Handle key events when the legacy skill mention popup is visible.
    fn handle_key_event_with_skill_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_empty_prompt_shortcut(&key_event) {
            return (InputResult::None, true);
        }
        self.footer.mode = reset_mode_after_activity(self.footer.mode);

        let ActivePopup::Skill(popup) = &mut self.popups.active else {
            unreachable!();
        };

        let mut selected_mention: Option<(String, Option<String>)> = None;
        let mut close_popup = false;

        let result = match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some(target) = self.current_mention_target() {
                    self.popups.dismissed_mention_token = Some(DismissedToken::new(
                        &self.draft.textarea,
                        target.range,
                        target.query,
                    ));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(mention) = popup.selected_mention() {
                    selected_mention = Some((mention.insert_text.clone(), mention.path.clone()));
                }
                close_popup = true;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        };

        if close_popup {
            if let Some((insert_text, path)) = selected_mention
                && let Some(target) = self.current_mention_target()
            {
                self.insert_selected_mention(target.range, &insert_text, path.as_deref());
                if let Some(path) = path {
                    self.mention_usage.record(&path);
                }
            }
            self.popups.active = ActivePopup::None;
        }

        result
    }

    fn handle_key_event_with_mentions_v2_popup(
        &mut self,
        key_event: KeyEvent,
    ) -> (InputResult, bool) {
        if self.handle_empty_prompt_shortcut(&key_event) {
            return (InputResult::None, true);
        }
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
        let can_switch_search_mode = self.current_mentions_v2_token_range().is_some();

        let ActivePopup::MentionV2(popup) = &mut self.popups.active else {
            unreachable!();
        };

        let mut selected: Option<MentionV2Selection> = None;
        let mut close_popup = false;
        let mut submit_without_popup = false;

        let result = match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if can_switch_search_mode {
                    popup.previous_search_mode();
                    (InputResult::None, true)
                } else {
                    self.handle_input_basic(key_event)
                }
            }
            KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if can_switch_search_mode {
                    popup.next_search_mode();
                    (InputResult::None, true)
                } else {
                    self.handle_input_basic(key_event)
                }
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some((range, query)) = self.current_mentions_v2_token_range() {
                    self.popups.dismissed_mention_token =
                        Some(DismissedToken::new(&self.draft.textarea, range, query));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                selected = popup.selected();
                close_popup = true;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                selected = popup.selected();
                close_popup = true;
                submit_without_popup = selected.is_none();
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        };

        if close_popup {
            let token_range = self
                .current_editable_at_token_range_with_options(/*allow_empty*/ true)
                .map(|(range, _)| range);
            if let (Some(selected), Some(token_range)) = (selected, token_range) {
                match selected {
                    MentionV2Selection::File(path) => {
                        self.insert_selected_file_path(
                            token_range,
                            path.to_string_lossy().as_ref(),
                        );
                    }
                    MentionV2Selection::Tool { insert_text, path } => {
                        self.insert_selected_mention(token_range, &insert_text, path.as_deref());
                    }
                }
            }
            self.popups.active = ActivePopup::None;
            if submit_without_popup {
                return self.handle_key_event_without_popup(key_event);
            }
        }

        result
    }

    fn handle_remote_image_selection_key(
        &mut self,
        key_event: &KeyEvent,
    ) -> Option<(InputResult, bool)> {
        if self.draft.textarea.mouse_selection_range().is_some() {
            return None;
        }
        let removes_remote_image = matches!(key_event.code, KeyCode::Delete | KeyCode::Backspace)
            && self.attachments.selected_remote_image_index.is_some();
        let started_vim_edit = removes_remote_image && self.begin_direct_vim_edit();
        let result = self
            .attachments
            .handle_remote_image_selection_key(key_event, &mut self.draft.textarea);
        if started_vim_edit {
            self.finish_vim_edit();
        }
        result
    }

    /// Handle key event when no popup is visible.
    pub(super) fn handle_key_event_without_popup(
        &mut self,
        key_event: KeyEvent,
    ) -> (InputResult, bool) {
        if let Some((result, redraw)) = self.handle_remote_image_selection_key(&key_event) {
            return (result, redraw);
        }
        if self.attachments.selected_remote_image_index.is_some() {
            self.attachments.clear_remote_image_selection();
        }
        if self.handle_empty_prompt_shortcut(&key_event) {
            return (InputResult::None, true);
        }
        if self.draft.is_bash_mode && key_event.code == KeyCode::Esc {
            self.flush_pending_paste_burst();
            if self.draft.textarea.is_empty() {
                self.draft.is_bash_mode = false;
                return (InputResult::None, true);
            }
        }
        if self.should_handle_vim_insert_escape(key_event) {
            return self.handle_input_basic(key_event);
        }
        if self.draft.textarea.is_vim_normal_mode() && self.draft.textarea.is_vim_operator_pending()
        {
            return self.handle_input_basic(key_event);
        }
        if self.config.shell_commands_enabled
            && self.draft.textarea.is_vim_normal_mode()
            && self.is_empty()
            && matches!(
                key_event,
                KeyEvent {
                    code: KeyCode::Char('!'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }
            )
        {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
            self.draft.is_bash_mode = true;
            self.draft.textarea.enter_vim_insert_mode();
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            if self.is_empty() {
                let next_mode = esc_hint_mode(self.footer.mode, self.is_task_running);
                if next_mode != self.footer.mode {
                    self.footer.mode = next_mode;
                    return (InputResult::None, true);
                }
            }
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
        if key_event.kind == KeyEventKind::Repeat
            && (self.queue_keys.is_pressed(key_event) || self.submit_keys.is_pressed(key_event))
        {
            return (InputResult::None, true);
        }
        if self.queue_keys.is_pressed(key_event)
            && (self.is_task_running || self.queue_submissions || !self.is_bang_shell_command())
        {
            return self.handle_submission(self.is_task_running || self.queue_submissions);
        }

        if self.submit_keys.is_pressed(key_event) {
            return self.handle_submission(self.queue_submissions);
        }

        if let KeyEvent {
            code: KeyCode::Char('d'),
            modifiers: crossterm::event::KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            ..
        } = key_event
            && self.is_empty()
        {
            return (InputResult::None, false);
        }

        let (history_up_pressed, history_down_pressed) = if self.draft.textarea.is_vim_normal_mode()
        {
            if self.draft.textarea.is_vim_operator_pending() {
                (false, false)
            } else {
                (
                    self.vim_normal_keymap.move_up.is_pressed(key_event),
                    self.vim_normal_keymap.move_down.is_pressed(key_event),
                )
            }
        } else {
            (
                self.editor_keymap.move_up.is_pressed(key_event),
                self.editor_keymap.move_down.is_pressed(key_event),
            )
        };
        if history_up_pressed || history_down_pressed {
            if self.draft.textarea.mouse_selection_range().is_none()
                && self.history.should_handle_navigation(
                    &self.current_text(),
                    self.history_navigation_cursor(),
                )
            {
                let replace_entry = if history_up_pressed {
                    self.history.navigate_up(&self.app_event_tx)
                } else {
                    self.history.navigate_down(&self.app_event_tx)
                };
                if let Some(entry) = replace_entry {
                    self.apply_history_entry(entry);
                    return (InputResult::None, true);
                }
            }
            return self.handle_input_basic(key_event);
        }

        self.handle_input_basic(key_event)
    }

    pub(super) fn is_bang_shell_command(&self) -> bool {
        self.config.shell_commands_enabled && self.draft.is_bash_mode
    }

    pub(super) fn shell_mode_footer_line(&self) -> Option<Line<'static>> {
        self.is_bang_shell_command()
            .then_some(())
            .map(|_| Line::from(vec![Span::from("Shell mode").light_red()]))
    }
}
