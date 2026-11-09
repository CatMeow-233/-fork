//! Text, cursor, attachment, history, and draft accessors that the bottom pane reads and restores.

use super::*;

impl ChatComposer {
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_options(area, ComposerRenderOptions::default())
    }

    /// Returns true if the composer currently contains no user-entered input.
    pub(crate) fn is_empty(&self) -> bool {
        self.draft.textarea.is_empty() && !self.draft.is_bash_mode && self.attachments.is_empty()
    }

    /// Record local persistent-history metadata so the composer can navigate
    /// cross-session history.
    pub(crate) fn set_history_metadata(
        &mut self,
        thread_id: ThreadId,
        log_id: u64,
        entry_count: usize,
    ) {
        self.history.set_metadata(thread_id, log_id, entry_count);
    }

    /// Integrate an asynchronous response to an on-demand history lookup.
    ///
    /// If the entry is present and the offset still matches the active history cursor, the
    /// composer rehydrates the entry immediately. This path intentionally routes through
    /// [`Self::apply_history_entry`] so cursor placement remains aligned with keyboard history
    /// recall semantics.
    pub(crate) fn on_history_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
    ) -> bool {
        match self
            .history
            .on_entry_response(log_id, offset, entry, &self.app_event_tx)
        {
            HistoryEntryResponse::Found(entry) => {
                // Persistent ↑/↓ history is text-only (backwards-compatible and avoids persisting
                // attachments), but local in-session ↑/↓ history can rehydrate elements and image paths.
                self.apply_history_entry(entry);
                true
            }
            HistoryEntryResponse::Search(result) => {
                self.apply_history_search_result(result);
                true
            }
            HistoryEntryResponse::Ignored => false,
        }
    }

    pub(crate) fn on_history_batch_response(
        &mut self,
        log_id: u64,
        cursor: HistoryBatchCursor,
        entries: Vec<crate::app_event::HistoryBatchEntryResponse>,
        next_older_cursor: Option<HistoryBatchCursor>,
    ) -> bool {
        let result = self.history.on_batch_response(
            log_id,
            cursor,
            entries,
            next_older_cursor,
            &self.app_event_tx,
        );
        self.apply_history_batch_result(result)
    }

    /// Applies a failed batch lookup without conflating it with history exhaustion.
    ///
    /// The history state machine either schedules a bounded retry, preserves an existing match, or
    /// returns the search UI to its idle draft when no match has been selected yet.
    pub(crate) fn on_history_batch_error(
        &mut self,
        log_id: u64,
        cursor: HistoryBatchCursor,
    ) -> bool {
        let result = self
            .history
            .on_batch_error(log_id, cursor, &self.app_event_tx);
        self.apply_history_batch_result(result)
    }

    fn apply_history_batch_result(&mut self, result: Option<HistorySearchResult>) -> bool {
        let Some(result) = result else {
            return false;
        };
        self.apply_history_search_result(result);
        true
    }

    pub(crate) fn record_local_user_message_history(&mut self, entry: HistoryEntry) {
        self.history.record_local_submission(entry);
    }

    pub(crate) fn record_replayed_user_message_history(&mut self, entry: HistoryEntry) {
        self.history.record_replayed_submission(entry);
    }

    pub fn handle_paste_image_path(&mut self, pasted: &str) -> bool {
        let Some(path_buf) = normalize_pasted_path(pasted) else {
            return false;
        };

        // Opening a serial device or FIFO can block the input loop.
        if !path_buf.is_file() {
            return false;
        }

        match image::image_dimensions(&path_buf) {
            Ok((width, height)) => {
                tracing::info!("OK: {pasted}");
                tracing::debug!("image dimensions={}x{}", width, height);
                let format = pasted_image_format(&path_buf);
                tracing::debug!("attached image format={}", format.label());
                self.attach_image(path_buf);
                true
            }
            Err(err) => {
                tracing::trace!("ERR: {err}");
                false
            }
        }
    }

    /// Replace the composer content with text from an external editor.
    /// Clears pending paste placeholders and keeps only attachments whose
    /// placeholder labels still appear in the new text. Image placeholders
    /// are renumbered to `[Image #M+1]..[Image #N]` (where `M` is the number of
    /// remote images). Cursor is placed at the end after rebuilding elements.
    pub(crate) fn apply_external_edit(&mut self, text: String) {
        self.note_sparkle_replaced_text(&text);
        self.vim_history = VimHistory::default();
        self.draft.pending_pastes.clear();
        let (text, _) = self.imported_text_for_textarea(text, Vec::new());

        // Count placeholder occurrences in the new text.
        let mut placeholder_counts: HashMap<String, usize> = HashMap::new();
        for placeholder in self
            .attachments
            .local_images
            .iter()
            .map(|image| &image.placeholder)
        {
            if placeholder_counts.contains_key(placeholder) {
                continue;
            }
            let count = text.match_indices(placeholder).count();
            if count > 0 {
                placeholder_counts.insert(placeholder.clone(), count);
            }
        }

        // Keep attachments only while we have matching occurrences left.
        let mut kept_images = Vec::new();
        for img in self.attachments.local_images.drain(..) {
            if let Some(count) = placeholder_counts.get_mut(&img.placeholder)
                && *count > 0
            {
                *count -= 1;
                kept_images.push(img);
            }
        }
        self.attachments.local_images = kept_images;

        // Import literally so placeholders remain atomic and Replace recovery starts empty.
        self.draft.textarea_state.get_mut().follow_cursor();
        self.draft.textarea.set_text_clearing_elements("");
        let mut remaining: HashMap<&str, usize> = HashMap::new();
        for img in &self.attachments.local_images {
            *remaining.entry(img.placeholder.as_str()).or_insert(0) += 1;
        }

        let mut occurrences: Vec<(usize, &str)> = Vec::new();
        for placeholder in remaining.keys() {
            for (pos, _) in text.match_indices(placeholder) {
                occurrences.push((pos, *placeholder));
            }
        }
        occurrences.sort_unstable_by_key(|(pos, _)| *pos);

        let mut idx = 0usize;
        for (pos, ph) in occurrences {
            let Some(count) = remaining.get_mut(ph) else {
                continue;
            };
            if *count == 0 {
                continue;
            }
            if pos > idx {
                self.draft.textarea.insert_str_at(idx, &text[idx..pos]);
            }
            self.draft.textarea.insert_element(ph);
            *count -= 1;
            idx = pos + ph.len();
        }
        if idx < text.len() {
            self.draft.textarea.insert_str_at(idx, &text[idx..]);
        }

        // Keep local image placeholders normalized in attachment order after the
        // remote-image prefix.
        self.attachments
            .relabel_local_images(&mut self.draft.textarea);
        self.draft
            .textarea
            .set_cursor(self.draft.textarea.text().len());
        self.sync_popups();
    }

    /// Enable or disable Vim editing for the composer textarea.
    ///
    /// The composer flushes buffered typing and clears paste-burst state when the mode
    /// changes because Vim normal mode treats rapid character sequences as
    /// commands, not as candidate literal paste text. It also resets transient
    /// footer mode so the visible hints match the new editing surface.
    pub(crate) fn set_vim_enabled(&mut self, enabled: bool) {
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }
        self.draft.textarea.enable_vim_search();
        self.draft.textarea.set_vim_enabled(enabled);
        self.vim_history = VimHistory::default();
        self.draft.paste_burst.clear_after_explicit_paste();
        self.footer.mode = reset_mode_after_activity(self.footer.mode);
    }

    /// Enable Vim while keeping already-active text entry in insert mode.
    pub(crate) fn enable_vim_in_insert_mode(&mut self) {
        self.set_vim_enabled(/*enabled*/ true);
        self.resume_text_entry();
    }

    /// Resume text entry after a parent view takes focus, preserving Vim undo history.
    pub(crate) fn resume_text_entry(&mut self) {
        self.draft.textarea.enter_vim_insert_mode();
    }

    pub(crate) fn take_kill_buffer_snapshot(&mut self) -> KillBufferSnapshot {
        vim_register::take_snapshot(self)
    }

    pub(crate) fn restore_kill_buffer_snapshot(&mut self, snapshot: KillBufferSnapshot) {
        vim_register::restore_snapshot(self, snapshot);
    }

    /// Restore draft history transferred from the startup composer.
    pub(crate) fn restore_startup_local_history(
        &mut self,
        startup_local_history: Vec<HistoryEntry>,
    ) {
        for entry in startup_local_history {
            self.history.record_local_submission(entry);
        }
    }

    /// Toggle Vim editing and return the new enabled state.
    ///
    /// This is the app-level command target for the configurable Vim toggle
    /// keybinding; callers should use the returned value for status messages
    /// instead of rereading state after additional composer mutations.
    pub(crate) fn toggle_vim_enabled(&mut self) -> bool {
        let enabled = !self.draft.textarea.is_vim_enabled();
        self.set_vim_enabled(enabled);
        enabled
    }

    /// Return whether Vim editing is enabled.
    pub(crate) fn is_vim_enabled(&self) -> bool {
        self.draft.textarea.is_vim_enabled()
    }

    /// Return whether Escape should be routed to the textarea before popups.
    ///
    /// Vim insert mode owns Escape as a transition back to normal mode. The app
    /// event layer asks this before running generic Escape behavior so the same
    /// key does not both leave insert mode and dismiss unrelated UI.
    pub(crate) fn should_handle_vim_insert_escape(&self, key_event: KeyEvent) -> bool {
        self.draft
            .textarea
            .should_handle_vim_insert_escape(key_event)
    }

    pub(crate) fn vim_mode_indicator_span(&self) -> Option<Span<'static>> {
        self.draft.textarea.vim_mode_indicator_span()
    }

    pub(super) fn mode_indicator_line(&self, show_cycle_hint: bool) -> Option<Line<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let Some(vim_mode) = self.vim_mode_indicator_span() {
            spans.push(vim_mode);
        }
        if let Some(indicators) = status_line_right_indicator_line(
            self.footer.collaboration_mode_indicator,
            self.footer.goal_status_indicator.as_ref(),
            self.footer.ide_context_active,
            show_cycle_hint,
        ) {
            if !spans.is_empty() {
                spans.push(" | ".dim());
            }
            spans.extend(indicators.spans);
        }
        if spans.is_empty() {
            None
        } else {
            Some(Line::from(spans))
        }
    }

    pub(super) fn right_footer_line_with_context(&self) -> Line<'static> {
        let mut line = if self.footer.context_window_pending {
            Line::default()
        } else {
            context_window_line(
                self.footer.context_window_percent,
                self.footer.context_window_used_tokens,
            )
        };
        if let Some(vim_mode) = self.vim_mode_indicator_span() {
            line.spans.push(" | ".dim());
            line.spans.push(vim_mode);
        }
        line
    }

    pub(crate) fn current_text_with_pending(&self) -> String {
        let text = self.current_text();
        if self.draft.pending_pastes.is_empty() {
            return text;
        }

        let (text, _) = Self::expand_pending_pastes(
            &text,
            self.current_text_elements(),
            &self.draft.pending_pastes,
        );
        text
    }

    /// Returns whether the composer currently accepts interactive draft edits.
    pub(crate) fn input_enabled(&self) -> bool {
        self.draft.input_enabled
    }

    pub(crate) fn pending_pastes(&self) -> Vec<(String, String)> {
        self.draft.pending_pastes.clone()
    }

    pub(crate) fn set_pending_pastes(&mut self, pending_pastes: Vec<(String, String)>) {
        let text = self.current_text();
        self.draft.pending_pastes = pending_pastes
            .into_iter()
            .filter(|(placeholder, _)| text.contains(placeholder))
            .collect();
    }

    pub(crate) fn set_remote_image_urls(&mut self, urls: Vec<String>) {
        if !self.sparkle.history_preview && !urls.is_empty() {
            self.dismiss_sparkle();
        }
        self.attachments
            .set_remote_image_urls(urls, &mut self.draft.textarea);
        self.sync_popups();
    }

    pub(crate) fn remote_image_urls(&self) -> Vec<String> {
        self.attachments.remote_image_urls()
    }

    pub(crate) fn take_remote_image_urls(&mut self) -> Vec<String> {
        let urls = self
            .attachments
            .take_remote_image_urls(&mut self.draft.textarea);
        self.sync_popups();
        urls
    }

    /// Replace the entire composer content with `text` and reset cursor.
    ///
    /// This is the "fresh draft" path: it discards active history search and
    /// clears pending paste payloads and mention link targets. Callers restoring
    /// a previously submitted draft that must keep sigiled mention target
    /// resolution should use
    /// [`Self::set_text_content_with_mention_bindings`] instead.
    pub(crate) fn set_text_content(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        if self.history_search.take().is_some() {
            self.history.reset_navigation();
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            Vec::new(),
        );
    }

    /// Restore draft content; clear pending input and undo history. The cursor starts at zero.
    pub(crate) fn set_text_content_with_mention_bindings(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
    ) {
        self.set_text_content_with_shell_prefix(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
            ShellPrefixHandling::Interpret,
        );
    }

    pub(in crate::bottom_pane) fn set_text_content_with_shell_prefix(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
        shell_prefix: ShellPrefixHandling,
    ) {
        if !self.sparkle.history_preview {
            if local_image_paths.is_empty() {
                self.note_sparkle_replaced_text(&text);
            } else {
                self.dismiss_sparkle();
            }
        }
        // Clear any existing content, placeholders, and attachments first.
        self.footer.flash = None;
        self.vim_history = VimHistory::default();
        self.draft.textarea_state.get_mut().follow_cursor();
        self.draft.textarea.set_text_clearing_elements("");
        self.draft.is_bash_mode = false;
        self.draft.pending_pastes.clear();
        self.draft.mention_bindings.clear();
        self.draft.skill_completion_space = None;

        let (text, text_elements) = match shell_prefix {
            ShellPrefixHandling::Interpret => self.imported_text_for_textarea(text, text_elements),
            ShellPrefixHandling::Literal => (text, text_elements),
        };
        self.draft
            .textarea
            .set_text_with_elements(&text, &text_elements);
        self.attachments
            .reset_local_images(local_image_paths, &mut self.draft.textarea);

        self.bind_mentions_from_snapshot(mention_bindings);
        self.draft.textarea.set_cursor(/*pos*/ 0);
        self.sync_popups();
    }

    pub(crate) fn current_cursor(&self) -> usize {
        self.draft.textarea.cursor() + if self.draft.is_bash_mode { 1 } else { 0 }
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.current_cursor()
    }

    pub(super) fn history_navigation_cursor(&self) -> usize {
        if self.draft.is_bash_mode && self.draft.textarea.cursor() == 0 {
            0
        } else if self.draft.textarea.is_vim_normal_mode()
            && !self.draft.textarea.text().is_empty()
            && self.draft.textarea.cursor() == self.draft.textarea.vim_normal_end_cursor()
        {
            self.current_text().len()
        } else {
            self.current_cursor()
        }
    }

    pub(crate) fn set_current_cursor(&mut self, cursor: usize) {
        self.draft.skill_completion_space = None;
        let visible_cursor = if self.draft.is_bash_mode {
            cursor.saturating_sub(1)
        } else {
            cursor
        };
        self.draft
            .textarea
            .set_cursor(visible_cursor.min(self.draft.textarea.text().len()));
    }

    pub(super) fn current_text_elements(&self) -> Vec<TextElement> {
        let shift = if self.draft.is_bash_mode { 1 } else { 0 };
        self.draft
            .textarea
            .text_elements()
            .into_iter()
            .filter_map(|element| Self::shift_text_element(element, shift))
            .collect()
    }

    fn shift_text_element(element: TextElement, shift: isize) -> Option<TextElement> {
        let start = element.byte_range.start.checked_add_signed(shift)?;
        let end = element.byte_range.end.checked_add_signed(shift)?;
        if start >= end {
            return None;
        }

        Some(element.map_range(|_| (start..end).into()))
    }

    pub(in crate::bottom_pane) fn snapshot_draft(&self) -> ComposerDraft {
        let (mention_bindings, mention_ranges) = self.snapshot_mention_bindings_with_ranges();
        ComposerDraft {
            is_bash_mode: self.draft.is_bash_mode,
            text: self.current_text(),
            text_elements: self.current_text_elements(),
            local_image_paths: self.attachments.local_image_paths(),
            remote_image_urls: self.attachments.remote_image_urls(),
            mention_bindings,
            mention_ranges,
            pending_pastes: self.draft.pending_pastes.clone(),
            cursor: self.current_cursor(),
        }
    }

    pub(in crate::bottom_pane) fn restore_draft(&mut self, draft: ComposerDraft) {
        let ComposerDraft {
            is_bash_mode,
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            mention_ranges,
            pending_pastes,
            cursor,
        } = draft;
        self.set_remote_image_urls(remote_image_urls);
        self.set_text_content_with_shell_prefix(
            text,
            text_elements,
            local_image_paths,
            Vec::new(),
            if is_bash_mode {
                ShellPrefixHandling::Interpret
            } else {
                ShellPrefixHandling::Literal
            },
        );
        for (binding, range) in mention_bindings.into_iter().zip(mention_ranges) {
            if let Some(id) = self.draft.textarea.element_id_for_exact_range(range) {
                self.draft.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
            }
        }
        self.set_pending_pastes(pending_pastes);
        self.set_current_cursor(cursor);
        self.sync_popups();
    }

    /// Update the placeholder text without changing input enablement.
    pub(crate) fn set_placeholder_text(&mut self, placeholder: String) {
        self.placeholder_text = placeholder;
    }

    pub(crate) fn set_parent_owned_thread(&mut self) {
        self.blocks_direct_input = true;
        self.placeholder_text = "Viewing sub-agent — direct input is disabled".to_string();
    }

    /// Move the cursor to the end of the current text buffer.
    pub(crate) fn move_cursor_to_end(&mut self) {
        self.draft
            .textarea
            .set_cursor(self.draft.textarea.text().len());
        self.sync_popups();
    }

    fn move_cursor_to_history_entry_end(&mut self) {
        let cursor = if self.draft.textarea.is_vim_normal_mode() {
            self.draft.textarea.vim_normal_end_cursor()
        } else {
            self.draft.textarea.text().len()
        };
        self.draft.textarea.set_cursor(cursor);
        self.sync_popups();
    }

    /// Convert canonical composer text into the textarea's internal representation.
    ///
    /// Shell mode stores the leading `!` as prompt state instead of editable text,
    /// so full-buffer imports must absorb that prefix before rebuilding the textarea.
    fn imported_text_for_textarea(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
    ) -> (String, Vec<TextElement>) {
        if self.config.shell_commands_enabled
            && let Some(stripped) = text.strip_prefix('!')
        {
            self.draft.is_bash_mode = true;
            (
                stripped.to_string(),
                text_elements
                    .into_iter()
                    .filter_map(|element| Self::shift_text_element(element, /*shift*/ -1))
                    .collect(),
            )
        } else {
            self.draft.is_bash_mode = false;
            (text, text_elements)
        }
    }

    /// Flush buffered typing before clearing so cancellation preserves the complete draft in history.
    pub(crate) fn clear_for_ctrl_c(&mut self) -> Option<String> {
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }
        if self.is_empty() {
            return None;
        }
        let previous = self.current_text();
        let text_elements = self.current_text_elements();
        let local_image_paths = self.attachments.local_image_paths();
        let pending_pastes = std::mem::take(&mut self.draft.pending_pastes);
        let remote_image_urls = self.attachments.remote_image_urls();
        let mention_bindings = self.snapshot_mention_bindings();
        self.set_text_content(String::new(), Vec::new(), Vec::new());
        self.attachments.clear_remote_image_urls();
        self.history.reset_navigation();
        self.history.record_local_submission(HistoryEntry {
            text: previous.clone(),
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        });
        Some(previous)
    }

    /// Get the current composer text.
    pub(crate) fn current_text(&self) -> String {
        if self.draft.is_bash_mode {
            format!("!{}", self.draft.textarea.text())
        } else {
            self.draft.textarea.text().to_string()
        }
    }

    /// Recall content at its history boundary, preserving pending pastes and omitting images
    /// in plain-text editors.
    pub(super) fn apply_history_entry(&mut self, entry: HistoryEntry) {
        let HistoryEntry {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        } = entry;
        self.set_remote_image_urls(remote_image_urls);
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.set_pending_pastes(pending_pastes);
        if !self.config.image_paste_enabled {
            for image in &self.attachments.local_images {
                if let Some(element) = self.draft.textarea.text_elements().into_iter().find(|e| {
                    e.placeholder(self.draft.textarea.text()) == Some(image.placeholder.as_str())
                }) {
                    let range = element.byte_range;
                    self.draft
                        .textarea
                        .replace_range(range.start..range.end, "");
                }
            }
            self.attachments = AttachmentState::default();
        }
        self.history.record_recalled_text(self.current_text());
        self.move_cursor_to_history_entry_end();
    }

    pub(crate) fn text_elements(&self) -> Vec<TextElement> {
        self.current_text_elements()
    }

    pub(crate) fn local_image_paths(&self) -> Vec<PathBuf> {
        self.attachments.local_image_paths()
    }

    pub(crate) fn status_line_content(&self) -> Option<&FooterContent> {
        self.footer.status_line_value.as_ref()
    }

    pub(crate) fn local_images(&self) -> &[LocalImageAttachment] {
        &self.attachments.local_images
    }

    #[cfg(test)]
    pub(crate) fn mention_bindings(&self) -> Vec<MentionBinding> {
        self.snapshot_mention_bindings()
    }

    pub(crate) fn take_recent_submission_mention_bindings(&mut self) -> Vec<MentionBinding> {
        std::mem::take(&mut self.draft.recent_submission_mention_bindings)
    }

    /// Commit the staged slash-command draft to local Up-arrow recall.
    ///
    /// Call this after command dispatch. Calling it more than once is harmless because the pending
    /// slot is consumed on the first call.
    pub(crate) fn record_pending_slash_command_history(&mut self) {
        if let Some(entry) = self.pending_slash_command_history.take() {
            self.history.record_local_submission(entry);
        }
    }

    /// Insert an attachment placeholder and track it for the next submission.
    pub fn attach_image(&mut self, path: PathBuf) {
        self.dismiss_sparkle();
        let started_vim_edit = self.begin_direct_vim_edit();
        let elements_before = self.draft.textarea.element_payloads();
        self.attachments
            .attach_image(&mut self.draft.textarea, path);
        self.reconcile_deleted_elements(elements_before);
        if started_vim_edit {
            self.finish_vim_edit();
        }
    }

    #[cfg(test)]
    pub fn take_recent_submission_images(&mut self) -> Vec<PathBuf> {
        self.attachments.take_recent_submission_images()
    }

    pub fn take_recent_submission_images_with_placeholders(&mut self) -> Vec<LocalImageAttachment> {
        std::mem::take(&mut self.attachments.local_images)
    }
}
