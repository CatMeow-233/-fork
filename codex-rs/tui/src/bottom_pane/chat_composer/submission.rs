//! Submission preparation, slash-command dispatch, and staged slash-command history.

use super::*;

impl ChatComposer {
    /// Prepare text for submission/queuing. Returns None if submission should be suppressed.
    /// On success, clears pending paste payloads because placeholders have been expanded.
    ///
    /// When `record_history` is true, the final submission is stored for ↑/↓ recall.
    pub(super) fn prepare_submission_text(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        self.prepare_submission_text_with_options(
            record_history,
            SlashValidation::Immediate,
            PendingPasteHandling::Expand,
        )
    }

    fn prepare_submission_text_with_options(
        &mut self,
        record_history: bool,
        slash_validation: SlashValidation,
        pending_paste_handling: PendingPasteHandling,
    ) -> Option<(String, Vec<TextElement>)> {
        let mut text = self.current_text();
        let original_input = text.clone();
        let original_text_elements = self.current_text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self.attachments.local_image_paths();
        let original_pending_pastes = self.draft.pending_pastes.clone();
        let mut text_elements = original_text_elements.clone();
        let input_starts_with_space = original_input.starts_with(' ');
        self.draft.recent_submission_mention_bindings.clear();
        self.draft.textarea.set_text_clearing_elements("");
        self.draft.is_bash_mode = false;

        if pending_paste_handling == PendingPasteHandling::Expand
            && !self.draft.pending_pastes.is_empty()
        {
            // Expand placeholders so element byte ranges stay aligned.
            let (expanded, expanded_elements) =
                Self::expand_pending_pastes(&text, text_elements, &self.draft.pending_pastes);
            text = expanded;
            text_elements = expanded_elements;
        }

        if self.config.trim_submission {
            let expanded_input = text.clone();
            text = text.trim().to_string();
            text_elements = Self::trim_text_elements(&expanded_input, &text, text_elements);
        }

        if slash_validation == SlashValidation::Immediate
            && let SubmissionValidation::UnknownCommand(name) = self
                .slash_input()
                .validate_submission(&text, input_starts_with_space)
        {
            let message = format!(
                r#"Unrecognized command '/{name}'. Type "/" for a list of supported commands."#
            );
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_info_event(message, /*hint*/ None),
            )));
            self.set_text_content_with_mention_bindings(
                original_input.clone(),
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.draft
                .pending_pastes
                .clone_from(&original_pending_pastes);
            self.draft.textarea.set_cursor(original_input.len());
            return None;
        }

        let actual_chars = text.chars().count();
        if actual_chars > MAX_USER_INPUT_TEXT_CHARS {
            let message = user_input_too_large_message(actual_chars);
            self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                history_cell::new_error_event(message),
            )));
            self.set_text_content_with_mention_bindings(
                original_input.clone(),
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.draft
                .pending_pastes
                .clone_from(&original_pending_pastes);
            self.draft.textarea.set_cursor(original_input.len());
            // Embedded composers may cover the transcript that receives the full error.
            if self.footer.hint_override.is_some() {
                self.show_footer_flash(
                    Line::from(
                        format!("Message too long; limit {MAX_USER_INPUT_TEXT_CHARS} characters")
                            .red(),
                    ),
                    Duration::from_secs(5),
                );
            }
            return None;
        }
        self.attachments
            .prune_local_images_for_submission(&text, &text_elements);
        if text.trim().is_empty() && self.attachments.is_empty() {
            return None;
        }
        self.draft.recent_submission_mention_bindings = original_mention_bindings.clone();
        if record_history && (!text.is_empty() || !self.attachments.is_empty()) {
            let preserve_literal_paste = self.slash_commands_enabled()
                && text.starts_with('!')
                && !original_input.trim_start().starts_with('!');
            let (history_text, history_text_elements) = if preserve_literal_paste {
                let history_text = original_input.trim().to_string();
                let history_text_elements = Self::trim_text_elements(
                    &original_input,
                    &history_text,
                    original_text_elements,
                );
                (history_text, history_text_elements)
            } else {
                (text.clone(), text_elements.clone())
            };
            self.history.record_local_submission(HistoryEntry {
                text: history_text,
                text_elements: history_text_elements,
                local_image_paths: self.attachments.local_image_paths(),
                remote_image_urls: self.attachments.remote_image_urls(),
                mention_bindings: original_mention_bindings,
                pending_pastes: if pending_paste_handling == PendingPasteHandling::Preserve
                    || preserve_literal_paste
                {
                    original_pending_pastes.clone()
                } else {
                    Vec::new()
                },
            });
        }
        self.draft.pending_pastes.clear();
        Some((text, text_elements))
    }

    /// Common logic for handling message submission/queuing.
    /// Returns the appropriate InputResult based on `should_queue`.
    pub(super) fn handle_submission(&mut self, should_queue: bool) -> (InputResult, bool) {
        let result =
            self.handle_submission_with_time(should_queue, tokio::time::Instant::now().into_std());
        self.reset_vim_mode_after_successful_dispatch(&result.0);
        result
    }

    /// Prepare a startup-confirmed draft through the normal queue parser, independent of keymaps.
    pub(in crate::bottom_pane) fn prepare_startup_submission(&mut self) -> InputResult {
        // Startup's plain-text editor cannot show shell mode; require confirmation here.
        if self.is_bang_shell_command() {
            return InputResult::None;
        }
        self.handle_submission(/*should_queue*/ true).0
    }

    pub(super) fn reset_vim_mode_after_successful_dispatch(&mut self, result: &InputResult) {
        if matches!(
            result,
            InputResult::Submitted { .. }
                | InputResult::Queued { .. }
                | InputResult::Command(_)
                | InputResult::ServiceTierCommand(_)
                | InputResult::CommandWithArgs(_, _, _)
        ) && self.config.reset_vim_on_submission
        {
            self.reset_vim_mode();
        }
    }

    pub(super) fn handle_submission_with_time(
        &mut self,
        should_queue: bool,
        now: Instant,
    ) -> (InputResult, bool) {
        self.handle_paste_burst_flush(now);

        if !should_queue && self.handle_paste_enter(now) {
            return (InputResult::None, true);
        }

        if let Some(result) = self.handle_parent_owned_submission() {
            return result;
        }
        if should_queue {
            self.flush_pending_paste_burst();
            let visible_shell_command = self.is_bang_shell_command();
            let original_input = self.current_text();
            let original_text_elements = self.current_text_elements();
            let original_pending_pastes = self.draft.pending_pastes.clone();
            let raw_text = self.draft.textarea.text();
            let defer_slash_validation = self.slash_input().should_parse_on_dequeue(raw_text);
            let preserve_pending_pastes = defer_slash_validation
                && !self.draft.pending_pastes.is_empty()
                && parse_slash_name(raw_text)
                    .is_some_and(|(name, _, _)| name == SlashCommand::Goal.command());
            let pending_pastes = if preserve_pending_pastes {
                self.draft.pending_pastes.clone()
            } else {
                Vec::new()
            };
            if let Some((text, text_elements)) = self.prepare_submission_text_with_options(
                /*record_history*/ true,
                if defer_slash_validation {
                    SlashValidation::Deferred
                } else {
                    SlashValidation::Immediate
                },
                if preserve_pending_pastes {
                    PendingPasteHandling::Preserve
                } else {
                    PendingPasteHandling::Expand
                },
            ) {
                let action = if text.starts_with('!') && !visible_shell_command {
                    QueuedInputAction::Literal
                } else {
                    slash_input::queued_input_action(&text, defer_slash_validation)
                };
                let (text, text_elements, pending_pastes) = if action == QueuedInputAction::Literal
                {
                    let text = original_input.trim().to_string();
                    let text_elements =
                        Self::trim_text_elements(&original_input, &text, original_text_elements);
                    (text, text_elements, original_pending_pastes)
                } else {
                    (text, text_elements, pending_pastes)
                };
                return (
                    InputResult::Queued {
                        text,
                        text_elements,
                        action,
                        pending_pastes,
                    },
                    true,
                );
            }
            return (InputResult::None, true);
        }

        // If the first line is a bare built-in slash command (no args),
        // dispatch it even when the slash popup isn't visible. This preserves
        // the workflow: type a prefix ("/di"), press Tab to complete to
        // "/diff ", then press Enter/Ctrl+Shift+Q to run it. Tab moves the cursor beyond
        // the '/name' token and our caret-based heuristic hides the popup,
        // but Enter/Ctrl+Shift+Q should still dispatch the command rather than submit
        // literal text.
        if let Some(result) = self.try_dispatch_bare_slash_command() {
            return (result, true);
        }

        let visible_shell_command = self.is_bang_shell_command();
        let original_input = self.current_text();
        let original_text_elements = self.current_text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self.attachments.local_image_paths();
        let original_pending_pastes = self.draft.pending_pastes.clone();
        if let Some(result) = self.try_dispatch_slash_command_with_args() {
            return (result, true);
        }

        if let Some((text, text_elements)) =
            self.prepare_submission_text(/*record_history*/ true)
        {
            if self.slash_commands_enabled() && text.starts_with('!') && !visible_shell_command {
                let text = original_input.trim().to_string();
                let text_elements =
                    Self::trim_text_elements(&original_input, &text, original_text_elements);
                (
                    InputResult::Queued {
                        text,
                        text_elements,
                        action: QueuedInputAction::Literal,
                        pending_pastes: original_pending_pastes,
                    },
                    true,
                )
            } else {
                // Do not clear local attachments here; ChatWidget drains them via
                // take_recent_submission_images().
                (
                    InputResult::Submitted {
                        text,
                        text_elements,
                    },
                    true,
                )
            }
        } else {
            // Restore suppressed input, preserving validation feedback.
            let flash = self.footer.flash.take();
            self.set_text_content_with_mention_bindings(
                original_input,
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.draft.pending_pastes = original_pending_pastes;
            self.footer.flash = flash;
            (InputResult::None, true)
        }
    }

    fn handle_parent_owned_submission(&mut self) -> Option<(InputResult, bool)> {
        if !self.blocks_direct_input {
            return None;
        }

        let text = self.current_text();
        let allowed_slash_command = parse_slash_name(&text).is_some_and(|(name, args, _)| {
            matches!(
                self.slash_input().command(name),
                Some(SlashCommandItem::Builtin(command))
                    if name == command.command()
                        && parent_owned_command_is_allowed(command, args)
            )
        });
        if text.starts_with('/') && allowed_slash_command {
            return None;
        }

        Some((InputResult::ParentOwnedInputBlocked, true))
    }

    /// Check if the first line is a bare slash command (no args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    pub(super) fn try_dispatch_bare_slash_command(&mut self) -> Option<InputResult> {
        let command = self
            .slash_input()
            .bare_command(self.draft.textarea.text())?;
        if self.reject_slash_command_if_unavailable(&command) {
            self.stage_slash_command_history(&command);
            self.record_pending_slash_command_history();
            return Some(InputResult::None);
        }
        self.stage_slash_command_history(&command);
        if !matches!(command, SlashCommandItem::Builtin(cmd) if cmd.requires_dispatch_validation())
        {
            self.draft.textarea.set_text_clearing_elements("");
            self.draft.is_bash_mode = false;
        }
        Some(match command {
            SlashCommandItem::Builtin(cmd) => InputResult::Command(cmd),
            SlashCommandItem::ServiceTier(command) => InputResult::ServiceTierCommand(command),
        })
    }

    /// Check if the input is a slash command with args (e.g., /review args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    pub(super) fn try_dispatch_slash_command_with_args(&mut self) -> Option<InputResult> {
        let text = self.draft.textarea.text().to_string();
        let inline_command = self.slash_input().inline_command(&text)?;
        let command = inline_command.command;
        if self.reject_slash_command_if_unavailable(&command) {
            self.stage_slash_command_history(&command);
            self.record_pending_slash_command_history();
            return Some(InputResult::None);
        }

        self.stage_slash_command_history(&command);

        let mut args_elements = slash_input::args_elements(
            inline_command.rest,
            inline_command.rest_offset,
            &self.draft.textarea.text_elements(),
        );
        let trimmed_rest = inline_command.rest.trim();
        args_elements = Self::trim_text_elements(inline_command.rest, trimmed_rest, args_elements);
        let SlashCommandItem::Builtin(cmd) = command else {
            return None;
        };
        Some(InputResult::CommandWithArgs(
            cmd,
            trimmed_rest.to_string(),
            args_elements,
        ))
    }

    /// Expand pending placeholders and extract normalized inline-command args.
    ///
    /// Inline-arg commands are initially dispatched using the raw draft so command rejection does
    /// not consume user input. Once a command needs its args, this helper performs the usual
    /// submission preparation (paste expansion, element trimming) and rebases element ranges from
    /// full-text offsets to command-arg offsets.
    ///
    /// Callers that already staged slash-command history should normally pass `false` for
    /// `record_history`; otherwise a command such as `/plan investigate` would be entered into
    /// local recall through both the slash-command path and the message-submission path.
    pub(crate) fn prepare_inline_args_submission(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        let (prepared_text, prepared_elements) = self.prepare_submission_text(record_history)?;
        let (prepared_rest, prepared_rest_offset) = slash_input::prepared_args(&prepared_text)?;
        let mut args_elements =
            slash_input::args_elements(prepared_rest, prepared_rest_offset, &prepared_elements);
        let trimmed_rest = prepared_rest.trim();
        args_elements = Self::trim_text_elements(prepared_rest, trimmed_rest, args_elements);
        self.draft.textarea.enter_vim_insert_mode();
        Some((trimmed_rest.to_string(), args_elements))
    }

    fn reject_slash_command_if_unavailable(&self, command: &SlashCommandItem) -> bool {
        if !self.is_task_running
            || command.available_during_task()
            || matches!(command, SlashCommandItem::Builtin(cmd) if cmd.requires_dispatch_validation())
        {
            return false;
        }
        let message = format!(
            "'/{}' is disabled while a task is in progress.",
            command.command()
        );
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_error_event(message),
        )));
        true
    }

    /// Stage the current slash-command text for later local recall.
    ///
    /// Staging snapshots the rich composer state before the textarea is cleared. `ChatWidget`
    /// commits the staged entry after dispatch so command recall follows the submitted text, not
    /// the command outcome.
    fn stage_slash_command_history(&mut self, command: &SlashCommandItem) {
        if matches!(command, SlashCommandItem::Builtin(SlashCommand::Clear)) {
            return;
        }
        self.stage_slash_command_history_text(self.draft.textarea.text().trim().to_string());
    }

    /// Stage a popup-selected command using its canonical command text.
    ///
    /// Popup filtering text can be partial, so recording the selected command avoids recalling
    /// `/di` after the user actually accepted `/diff`.
    pub(super) fn stage_selected_slash_command_history(&mut self, command: &CommandItem) {
        if matches!(command, CommandItem::Builtin(SlashCommand::Clear)) {
            return;
        }
        self.stage_slash_command_history_text(format!("/{}", command.command()));
    }

    /// Store the provided command text and the current composer adornments in the pending slot.
    ///
    /// The pending entry intentionally has the same shape as other local history entries so recall
    /// can rehydrate attachments, mention bindings, and pending paste placeholders if command
    /// workflows start carrying those through in the future.
    fn stage_slash_command_history_text(&mut self, text: String) {
        self.pending_slash_command_history = Some(HistoryEntry {
            text,
            text_elements: self.draft.textarea.text_elements(),
            local_image_paths: self.attachments.local_image_paths(),
            remote_image_urls: self.attachments.remote_image_urls(),
            mention_bindings: self.snapshot_mention_bindings(),
            pending_pastes: self.draft.pending_pastes.clone(),
        });
    }
}
