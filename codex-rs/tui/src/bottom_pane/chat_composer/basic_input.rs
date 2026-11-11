//! Textarea edits: paste-burst and IME input, element reconciliation, and empty-prompt shortcuts.

use super::*;

impl ChatComposer {
    #[inline]
    fn clamp_to_char_boundary(text: &str, pos: usize) -> usize {
        let mut p = pos.min(text.len());
        if p < text.len() && !text.is_char_boundary(p) {
            p = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= p)
                .last()
                .unwrap_or(0);
        }
        p
    }

    /// Handle non-ASCII character input (often IME) while still supporting paste-burst detection.
    ///
    /// This handler exists because non-ASCII input often comes from IMEs, where characters can
    /// legitimately arrive in short bursts that should **not** be treated as paste.
    ///
    /// The key differences from the ASCII path:
    ///
    /// - We never hold the first character (`PasteBurst::on_plain_char_no_hold`), because holding a
    ///   non-ASCII char can feel like dropped input.
    /// - If a burst is detected, we may need to retroactively remove already-inserted text before
    ///   the cursor and move it into the paste buffer (see `PasteBurst::decide_begin_buffer`).
    ///
    /// Because this path mixes "insert immediately" with "maybe retro-grab later", it must clamp
    /// the cursor to a UTF-8 char boundary before slicing `textarea.text()`.
    #[inline]
    fn handle_non_ascii_char(&mut self, input: KeyEvent, now: Instant) -> (InputResult, bool) {
        if self.draft.disable_paste_burst {
            // When burst detection is disabled, treat IME/non-ASCII input as normal typing.
            // In particular, do not retro-capture or buffer already-inserted prefix text.
            self.draft.textarea.input(input);
            let text_after = self.draft.textarea.text();
            self.draft
                .pending_pastes
                .retain(|(placeholder, _)| text_after.contains(placeholder));
            return (InputResult::None, true);
        }
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            if self.draft.paste_burst.try_append_char_if_active(ch, now) {
                return (InputResult::None, true);
            }
            // Non-ASCII input often comes from IMEs and can arrive in quick bursts.
            // We do not want to hold the first char (flicker suppression) on this path, but we
            // still want to detect paste-like bursts. Before applying any non-ASCII input, flush
            // any existing burst buffer (including a pending first char from the ASCII path) so
            // we don't carry that transient state forward.
            self.flush_pending_paste_burst();
            if let Some(decision) = self.draft.paste_burst.on_plain_char_no_hold(now) {
                match decision {
                    CharDecision::BufferAppend => {
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        // For non-ASCII we inserted prior chars immediately, so if this turns out
                        // to be paste-like we need to retroactively grab & remove the already-
                        // inserted prefix from the textarea before buffering the burst.
                        let cur = self.draft.textarea.cursor();
                        let txt = self.draft.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) = self.draft.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            retro_chars as usize,
                        ) {
                            if grab.grabbed.is_empty()
                                || self.draft.textarea.retract_paste_burst(grab.start_byte)
                            {
                                self.draft.paste_burst.append_char_to_buffer(ch, now);
                                return (InputResult::None, true);
                            }
                            self.draft.paste_burst.clear_after_explicit_paste();
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    _ => unreachable!("on_plain_char_no_hold returned unexpected variant"),
                }
            }
        }
        self.flush_pending_paste_burst();
        let elements_before = self.draft.textarea.element_payloads();
        self.draft.textarea.input(input);
        self.reconcile_deleted_elements(elements_before);
        (InputResult::None, true)
    }

    /// Handles keys that mutate the textarea, including paste-burst detection.
    ///
    /// Acts as the lowest-level keypath for keys that mutate the textarea. It is also where plain
    /// character streams are converted into explicit paste operations on terminals that do not
    /// reliably provide bracketed paste.
    ///
    /// Ordering is important:
    ///
    /// - Always flush any *due* paste burst first so buffered text does not lag behind unrelated
    ///   edits.
    /// - Then handle the incoming key, intercepting only text-producing character input.
    /// - For non-text keys, flush via `flush_before_modified_input()` before applying the key;
    ///   otherwise `clear_window_after_non_char()` can leave buffered text waiting without a
    ///   timestamp to time out against.
    pub(super) fn handle_input_basic(&mut self, input: KeyEvent) -> (InputResult, bool) {
        // Ignore key releases here to avoid treating them as additional input
        // (e.g., appending the same character twice via paste-burst logic).
        if !matches!(input.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return (InputResult::None, false);
        }

        self.handle_input_basic_with_time(input, tokio::time::Instant::now().into_std())
    }

    pub(super) fn handle_input_basic_with_time(
        &mut self,
        input: KeyEvent,
        now: Instant,
    ) -> (InputResult, bool) {
        if !self.draft.textarea.is_vim_normal_mode() {
            self.begin_vim_edit(input);
        }
        // If we have a buffered non-bracketed paste burst and enough time has
        // elapsed since the last char, flush it before handling a new input.
        self.handle_paste_burst_flush(now);

        if !matches!(input.code, KeyCode::Esc) {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }

        // If we're capturing a burst and receive Enter, accumulate it instead of inserting.
        if matches!(input.code, KeyCode::Enter)
            && !self.draft.disable_paste_burst
            && self.draft.paste_burst.is_active()
            && self
                .draft
                .paste_burst
                .append_control_char_if_active('\n', now)
        {
            return (InputResult::None, true);
        }

        let has_non_text_modifier = has_ctrl_or_alt(input.modifiers)
            || input
                .modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META);

        let completion_punctuation = match input.code {
            KeyCode::Char(ch) if !has_non_text_modifier => self
                .draft
                .skill_completion_space_for_char(ch)
                .map(|space| (space, ch)),
            _ => None,
        };
        self.draft.skill_completion_space = None;

        // Intercept text-producing Char inputs to optionally accumulate into a burst buffer.
        //
        // This preserves plain, Shift, and Windows AltGr input while keeping shortcut modifiers
        // out of the burst detector and flushing any in-flight text before a non-text key.
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            if !has_non_text_modifier
                && !self.draft.disable_paste_burst
                && self.draft.textarea.allows_paste_burst()
            {
                // Keep IME input responsive. Skill-completion punctuation needs classification
                // before it can replace the inserted separator.
                if !ch.is_ascii() && completion_punctuation.is_none() {
                    return self.handle_non_ascii_char(input, now);
                }

                match self.draft.paste_burst.on_plain_char(ch, now) {
                    CharDecision::BufferAppend => {
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        let cur = self.draft.textarea.cursor();
                        let txt = self.draft.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) = self.draft.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            retro_chars as usize,
                        ) {
                            if grab.grabbed.is_empty()
                                || self.draft.textarea.retract_paste_burst(grab.start_byte)
                            {
                                self.draft.paste_burst.append_char_to_buffer(ch, now);
                                return (InputResult::None, true);
                            }
                            self.draft.paste_burst.clear_after_explicit_paste();
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    CharDecision::BeginBufferFromPending => {
                        // First char was held; now append the current one.
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    CharDecision::RetainFirstChar => {
                        // Keep the first fast char pending momentarily.
                        self.draft.skill_completion_space =
                            completion_punctuation.map(|(space, _)| space);
                        return (InputResult::None, true);
                    }
                }
            }
            self.flush_pending_paste_burst();
        }

        // Flush any buffered burst before applying a non-char input (arrow keys, etc).
        //
        // `clear_window_after_non_char()` clears `last_plain_char_time`. If we cleared that while
        // `PasteBurst.buffer` is non-empty, `flush_if_due()` would no longer have a timestamp to
        // time out against, and the buffered paste could remain stuck until another plain char
        // arrives.
        if !matches!(input.code, KeyCode::Char(_) | KeyCode::Enter)
            && let Some(pasted) = self.draft.paste_burst.flush_before_modified_input()
        {
            self.apply_paste(pasted);
        }
        // For non-char inputs (or after flushing), handle normally.
        // Track element removals so we can drop any corresponding placeholders without scanning
        // the full text. (Placeholders are atomic elements; when deleted, the element disappears.)
        let elements_before = if self.draft.pending_pastes.is_empty() && self.attachments.is_empty()
        {
            None
        } else {
            Some(self.draft.textarea.element_payloads())
        };

        if self.draft.is_bash_mode
            && self.draft.textarea.vim_query().is_none()
            && self.draft.textarea.mouse_selection_range().is_none()
            && self.draft.textarea.is_delete_backward_key(input)
            && self.draft.textarea.cursor() == 0
        {
            let started_vim_edit = self.begin_direct_vim_edit();
            if self.draft.textarea.is_vim_replace_mode() {
                self.draft.textarea.input(input);
            }
            self.draft.is_bash_mode = false;
            if started_vim_edit {
                self.finish_vim_edit();
            }
            return (InputResult::None, true);
        }

        let prefix_insertion_revision = self.draft.textarea.prefix_insertion_revision();
        self.begin_vim_edit(input);
        if self.draft.textarea.is_vim_normal_mode()
            && !self.draft.textarea.is_vim_operator_pending()
            && self.vim_normal_keymap.repeat_last_change.is_pressed(input)
        {
            self.draft.textarea.set_cursor(self.draft.textarea.cursor());
            if let Some(edits) = self.draft.textarea.begin_vim_repeat() {
                for edit in edits {
                    let elements = self.draft.textarea.element_payloads();
                    let prefix_insertion_revision = self.draft.textarea.prefix_insertion_revision();
                    if !self.draft.textarea.apply_vim_edit(&edit) {
                        break;
                    }
                    vim_register::sync(self);
                    if self.draft.textarea.prefix_insertion_revision() != prefix_insertion_revision
                    {
                        self.sync_bash_mode_from_text();
                    }
                    self.reconcile_deleted_elements(elements);
                }
                self.draft.textarea.finish_vim_repeat();
            }
        } else {
            let textarea = &mut self.draft.textarea;
            let before_len = textarea.text().len();
            textarea.input(input);
            // Check that the key inserted punctuation, since an editor binding can consume it.
            if let Some((space, ch)) = completion_punctuation
                && textarea.text().len() == before_len + ch.len_utf8()
                && textarea.cursor() == space + 1 + ch.len_utf8()
                && textarea
                    .text()
                    .get(space + 1..)
                    .is_some_and(|suffix| suffix.starts_with(ch))
            {
                textarea.replace_range(space..space + 1, "");
            }
            vim_register::sync(self);
        }
        if self.draft.textarea.prefix_insertion_revision() != prefix_insertion_revision {
            self.sync_bash_mode_from_text();
        }
        if let Some(elements_before) = elements_before {
            self.reconcile_deleted_elements(elements_before);
        }
        self.finish_vim_edit();

        // Update the paste-burst heuristic for text, shortcut, and non-char events.
        match input.code {
            KeyCode::Char(_) => {
                if has_non_text_modifier {
                    self.draft.paste_burst.clear_window_after_non_char();
                }
            }
            KeyCode::Enter => {
                // Keep burst window alive (supports blank lines in paste).
            }
            _ => {
                // Other keys: clear burst window (buffer should have been flushed above if needed).
                self.draft.paste_burst.clear_window_after_non_char();
            }
        }

        (InputResult::None, true)
    }

    pub(super) fn sync_bash_mode_from_text(&mut self) {
        if self.config.shell_commands_enabled
            && !self.draft.is_bash_mode
            && self.draft.textarea.text().starts_with('!')
        {
            self.draft.textarea.absorb_prompt_prefix(/*prefix_len*/ 1);
            self.draft.is_bash_mode = true;
        }
    }

    pub(super) fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
        let elements_after: HashSet<String> =
            self.draft.textarea.element_payloads().into_iter().collect();

        let removed_payloads = elements_before
            .into_iter()
            .filter(|payload| !elements_after.contains(payload))
            .collect::<Vec<_>>();
        for removed in &removed_payloads {
            self.draft.pending_pastes.retain(|(ph, _)| ph != removed);
        }
        self.attachments
            .remove_deleted_local_placeholders(&removed_payloads, &mut self.draft.textarea);
    }

    /// Handle empty-prompt agents navigation and shortcut-overlay toggling or dismissal.
    ///
    /// This only toggles when the composer is empty and no paste burst is in
    /// progress, so typing/pasting `?` still inserts text instead of opening
    /// help. The bound key list intentionally supports terminal-variant
    /// modifier reporting (for example `?` vs `shift-?`).
    pub(super) fn handle_empty_prompt_shortcut(&mut self, key_event: &KeyEvent) -> bool {
        if self.shortcut_overlay_visible() && key_hint::plain(KeyCode::Esc).is_press(*key_event) {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
            return true;
        }
        if key_event.kind != KeyEventKind::Press {
            return false;
        }

        if self.handle_agents_navigation(*key_event) {
            return true;
        }

        let Some(next) = self.empty_prompt_shortcut_toggle(key_event) else {
            return false;
        };
        self.footer.mode = next;
        true
    }

    pub(super) fn empty_prompt_shortcut_toggle(&self, key_event: &KeyEvent) -> Option<FooterMode> {
        if key_event.kind == KeyEventKind::Press
            && self.footer.hint_override.is_none()
            && self.toggle_shortcuts_keys.is_pressed(*key_event)
            && self.is_empty()
            && !self.is_in_paste_burst()
        {
            let next = toggle_shortcut_mode(
                self.footer.mode,
                self.quit_shortcut_hint_visible(),
                self.is_empty(),
            );
            (next != self.footer.mode).then_some(next)
        } else {
            None
        }
    }
}
