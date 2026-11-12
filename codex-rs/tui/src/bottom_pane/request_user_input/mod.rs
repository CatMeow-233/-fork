//! Request-user-input overlay state machine.
//!
//! Core behaviors:
//! - Each question can be answered by selecting one option and/or providing notes.
//! - Notes are stored per question and appended as extra answers.
//! - Typing while focused on options jumps into notes to keep freeform input fast.
//! - The composer submit binding advances to the next question; the last question submits all answers.
//! - Freeform-only questions submit an empty answer list when empty.
use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crate::app::app_server_requests::ResolvedAppServerRequest;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::style::Stylize;
use ratatui::text::Line;
mod layout;
pub(super) mod render;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::ChatComposerConfig;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::scroll_state::ScrollState;
use crate::bottom_pane::selection_popup_common::GenericDisplayRow;
use crate::bottom_pane::selection_popup_common::measure_rows_height;
use crate::footer_hint::shortcut;
use crate::history_cell;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::ShortcutHint;
use crate::keymap::KeymapContext;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::keymap::RuntimeKeymap;
use crate::render::renderable::Renderable;

#[cfg(test)]
use crate::app_command::AppCommand as Op;
use codex_app_server_protocol::ToolRequestUserInputAnswer;
#[cfg(test)]
use codex_app_server_protocol::ToolRequestUserInputOption;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::ToolRequestUserInputQuestion;
use codex_app_server_protocol::ToolRequestUserInputResponse;
use codex_protocol::ThreadId;
use codex_protocol::user_input::TextElement;
use unicode_width::UnicodeWidthStr;

const NOTES_PLACEHOLDER: &str = "Add notes";
const ANSWER_PLACEHOLDER: &str = "Type your answer (optional)";
// Keep in sync with ChatComposer's minimum composer height.
const MIN_COMPOSER_HEIGHT: u16 = 3;
const SELECT_OPTION_PLACEHOLDER: &str = "Select an option to add notes";
pub(super) const TIP_SEPARATOR: &str = " | ";
pub(super) const DESIRED_SPACERS_BETWEEN_SECTIONS: u16 = 2;
const OTHER_OPTION_LABEL: &str = "None of the above";
const OTHER_OPTION_DESCRIPTION: &str = "Optionally, add details in notes (tab)";
const UNANSWERED_CONFIRM_TITLE: &str = "Submit with unanswered questions?";
const UNANSWERED_CONFIRM_GO_BACK: &str = "Go back";
const UNANSWERED_CONFIRM_GO_BACK_DESC: &str = "Return to the first unanswered question";
const UNANSWERED_CONFIRM_SUBMIT: &str = "Proceed";
const UNANSWERED_CONFIRM_SUBMIT_DESC_SINGULAR: &str = "question";
const UNANSWERED_CONFIRM_SUBMIT_DESC_PLURAL: &str = "questions";
const AUTO_RESOLUTION_HIDDEN_GRACE: Duration = Duration::from_secs(/*secs*/ 60);
const AUTO_RESOLUTION_VISIBLE_COUNTDOWN: Duration = Duration::from_secs(/*secs*/ 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Options,
    Notes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoResolutionTiming {
    Disabled,
    HiddenGrace { remaining: Duration },
    VisibleCountdown { remaining: Duration },
    Due,
}

fn format_auto_resolution_remaining(remaining: Duration) -> String {
    let mut seconds = remaining.as_secs();
    if remaining.subsec_nanos() > 0 {
        seconds = seconds.saturating_add(1);
    }
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes}m {seconds:02}s")
}

/// Composer draft saved per question: request_user_input notes and freeform
/// answers, and MCP elicitation text-field answers.
#[derive(Default, Clone, PartialEq)]
pub(super) struct ComposerDraft {
    pub(super) text: String,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) local_image_paths: Vec<PathBuf>,
    pub(super) pending_pastes: Vec<(String, String)>,
}

impl ComposerDraft {
    pub(super) fn from_composer(composer: &ChatComposer) -> Self {
        Self {
            text: composer.current_text(),
            text_elements: composer.text_elements(),
            local_image_paths: composer.local_image_paths(),
            pending_pastes: composer.pending_pastes(),
        }
    }

    pub(super) fn text_with_pending(&self) -> String {
        if self.pending_pastes.is_empty() {
            return self.text.clone();
        }
        debug_assert!(
            !self.text_elements.is_empty(),
            "pending pastes should always have matching text elements"
        );
        let (expanded, _) = ChatComposer::expand_pending_pastes(
            &self.text,
            self.text_elements.clone(),
            &self.pending_pastes,
        );
        expanded
    }
}

/// Wrap each `\n`-separated line of `text` at `width`, keeping web URLs linked.
pub(super) fn wrap_plain_text_with_links(
    text: &str,
    width: u16,
) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
    text.split('\n')
        .flat_map(|text| {
            let source =
                crate::terminal_hyperlinks::annotate_web_urls_in_line(text.to_owned().into());
            let wrapped = crate::wrapping::wrap_ranges_trim(text, usize::from(width.max(1)))
                .into_iter()
                .map(|range| crate::wrapping::WrappedLine {
                    line: (&text[range.clone()]).into(),
                    range,
                    prefix_bytes: 0,
                })
                .collect();
            crate::terminal_hyperlinks::remap_source_wrapped_line(&source, wrapped)
        })
        .collect()
}

struct AnswerState {
    // Scrollable cursor state for option navigation/highlight.
    options_state: ScrollState,
    // Per-question notes draft.
    draft: ComposerDraft,
    // Whether the answer for this question has been explicitly submitted.
    answer_committed: bool,
    // Whether the notes UI has been explicitly opened for this question.
    notes_visible: bool,
}

pub(crate) struct RequestUserInputOverlay {
    app_event_tx: AppEventSender,
    request: ToolRequestUserInputParams,
    // Queue of incoming requests to process after the current one.
    queue: VecDeque<ToolRequestUserInputParams>,
    // Reuse the shared chat composer so notes/freeform answers match the
    // primary input styling and behavior.
    composer: ChatComposer,
    // One entry per question: selection state plus a stored notes draft.
    answers: Vec<AnswerState>,
    current_idx: usize,
    focus: Focus,
    done: bool,
    pending_submission_draft: Option<ComposerDraft>,
    confirm_unanswered: Option<ScrollState>,
    request_started_at: Instant,
    auto_resolution_snoozed: bool,
    composer_submit_keys: Vec<KeyBinding>,
    composer_submit_hint: Option<ShortcutHint>,
    interrupt_turn_keys: Vec<KeyBinding>,
    interrupt_turn_hint: Option<ShortcutHint>,
    list_keymap: ListKeymap,
}

impl RequestUserInputOverlay {
    #[cfg(test)]
    pub(crate) fn new(
        request: ToolRequestUserInputParams,
        app_event_tx: AppEventSender,
        has_input_focus: bool,
        enhanced_keys_supported: bool,
        disable_paste_burst: bool,
    ) -> Self {
        Self::new_with_keymap(
            request,
            app_event_tx,
            has_input_focus,
            enhanced_keys_supported,
            disable_paste_burst,
            RuntimeKeymap::defaults(),
        )
    }

    pub(crate) fn new_with_keymap(
        request: ToolRequestUserInputParams,
        app_event_tx: AppEventSender,
        has_input_focus: bool,
        enhanced_keys_supported: bool,
        disable_paste_burst: bool,
        keymap: RuntimeKeymap,
    ) -> Self {
        // Use the same composer widget, but disable popups/slash-commands and
        // image-path attachment so it behaves like a focused notes field.
        let mut composer = ChatComposer::new_with_config(
            has_input_focus,
            app_event_tx.clone(),
            enhanced_keys_supported,
            ANSWER_PLACEHOLDER.to_string(),
            disable_paste_burst,
            ChatComposerConfig::plain_text(),
        );
        composer.set_keymap_bindings(&keymap);
        // The overlay renders its own footer hints, so keep the composer footer empty.
        composer.set_footer_hint_override(Some(Vec::new()));
        let mut overlay = Self {
            app_event_tx,
            request,
            queue: VecDeque::new(),
            composer,
            answers: Vec::new(),
            current_idx: 0,
            focus: Focus::Options,
            done: false,
            pending_submission_draft: None,
            confirm_unanswered: None,
            request_started_at: Instant::now(),
            auto_resolution_snoozed: false,
            composer_submit_keys: keymap.composer.submit.clone(),
            composer_submit_hint: keymap.primary_hint(KeymapContext::Composer, "submit"),
            interrupt_turn_keys: keymap.chat.interrupt_turn.clone(),
            interrupt_turn_hint: keymap.primary_hint(KeymapContext::Chat, "interrupt_turn"),
            list_keymap: keymap.list,
        };
        overlay.reset_for_request();
        overlay.ensure_focus_available();
        overlay.restore_current_draft();
        overlay
    }

    fn current_index(&self) -> usize {
        self.current_idx
    }

    fn current_question(&self) -> Option<&ToolRequestUserInputQuestion> {
        self.request.questions.get(self.current_index())
    }

    fn current_answer_mut(&mut self) -> Option<&mut AnswerState> {
        let idx = self.current_index();
        self.answers.get_mut(idx)
    }

    fn current_answer(&self) -> Option<&AnswerState> {
        let idx = self.current_index();
        self.answers.get(idx)
    }

    fn question_count(&self) -> usize {
        self.request.questions.len()
    }

    fn advance_queue_or_complete_at(&mut self, now: Instant) {
        if let Some(next) = self.queue.pop_front() {
            self.request = next;
            self.request_started_at = now;
            self.auto_resolution_snoozed = false;
            self.reset_for_request();
            self.ensure_focus_available();
            self.restore_current_draft();
        } else {
            self.done = true;
        }
    }

    fn snooze_auto_resolution(&mut self) {
        if !self.request.is_blocking {
            self.auto_resolution_snoozed = true;
        }
    }

    fn auto_resolution_timing_at(&self, now: Instant) -> AutoResolutionTiming {
        // autoResolutionMs is deprecated; isBlocking now controls whether the
        // request can auto-resolve using the TUI's fixed grace/countdown policy.
        if self.request.is_blocking || self.auto_resolution_snoozed {
            return AutoResolutionTiming::Disabled;
        }

        let elapsed = now.saturating_duration_since(self.request_started_at);
        if elapsed < AUTO_RESOLUTION_HIDDEN_GRACE {
            return AutoResolutionTiming::HiddenGrace {
                remaining: AUTO_RESOLUTION_HIDDEN_GRACE.saturating_sub(elapsed),
            };
        }
        let visible_elapsed = elapsed.saturating_sub(AUTO_RESOLUTION_HIDDEN_GRACE);
        if visible_elapsed < AUTO_RESOLUTION_VISIBLE_COUNTDOWN {
            return AutoResolutionTiming::VisibleCountdown {
                remaining: AUTO_RESOLUTION_VISIBLE_COUNTDOWN.saturating_sub(visible_elapsed),
            };
        }
        AutoResolutionTiming::Due
    }

    fn auto_resolution_next_frame_delay_at(&self, now: Instant) -> Option<Duration> {
        match self.auto_resolution_timing_at(now) {
            AutoResolutionTiming::Disabled => None,
            AutoResolutionTiming::HiddenGrace { remaining } => Some(remaining),
            AutoResolutionTiming::VisibleCountdown { remaining } => {
                Some(remaining.min(Duration::from_secs(/*secs*/ 1)))
            }
            AutoResolutionTiming::Due => Some(Duration::ZERO),
        }
    }

    fn maybe_auto_resolve_at(&mut self, now: Instant) -> bool {
        if !matches!(
            self.auto_resolution_timing_at(now),
            AutoResolutionTiming::Due
        ) {
            return false;
        }
        self.submit_empty_auto_resolution(now);
        true
    }

    fn auto_resolution_countdown_text_at(&self, now: Instant) -> Option<String> {
        match self.auto_resolution_timing_at(now) {
            AutoResolutionTiming::VisibleCountdown { remaining } => Some(format!(
                "auto-resolves in {}",
                format_auto_resolution_remaining(remaining)
            )),
            AutoResolutionTiming::Disabled
            | AutoResolutionTiming::HiddenGrace { .. }
            | AutoResolutionTiming::Due => None,
        }
    }

    pub(super) fn progress_prefix_text(&self) -> String {
        if self.question_count() > 0 {
            let idx = self.current_index() + 1;
            let total = self.question_count();
            let base = format!("Question {idx}/{total}");
            let unanswered = self.unanswered_count();
            if unanswered > 0 {
                format!("{base} ({unanswered} unanswered)")
            } else {
                base
            }
        } else {
            "No questions".to_string()
        }
    }

    fn has_options(&self) -> bool {
        self.current_question()
            .and_then(|question| question.options.as_ref())
            .is_some_and(|options| !options.is_empty())
    }

    fn options_len(&self) -> usize {
        self.current_question()
            .map(Self::options_len_for_question)
            .unwrap_or(0)
    }

    fn option_index_for_digit(&self, ch: char) -> Option<usize> {
        if !self.has_options() {
            return None;
        }
        let digit = ch.to_digit(10)?;
        if digit == 0 {
            return None;
        }
        let idx = (digit - 1) as usize;
        (idx < self.options_len()).then_some(idx)
    }

    fn selected_option_index(&self) -> Option<usize> {
        if !self.has_options() {
            return None;
        }
        self.current_answer()
            .and_then(|answer| answer.options_state.selected_idx)
    }

    fn notes_has_content(&self, idx: usize) -> bool {
        if idx == self.current_index() {
            !self.composer.current_text_with_pending().trim().is_empty()
        } else {
            !self.answers[idx].draft.text.trim().is_empty()
        }
    }

    pub(super) fn notes_ui_visible(&self) -> bool {
        if !self.has_options() {
            return true;
        }
        let idx = self.current_index();
        self.current_answer()
            .is_some_and(|answer| answer.notes_visible || self.notes_has_content(idx))
    }

    pub(super) fn wrapped_question_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        self.current_question()
            .map(|q| wrap_plain_text_with_links(&q.question, width))
            .unwrap_or_default()
    }

    fn focus_is_notes(&self) -> bool {
        matches!(self.focus, Focus::Notes)
    }

    fn confirm_unanswered_active(&self) -> bool {
        self.confirm_unanswered.is_some()
    }

    pub(super) fn option_rows(&self) -> Vec<GenericDisplayRow> {
        self.current_question()
            .and_then(|question| question.options.as_ref().map(|options| (question, options)))
            .map(|(question, options)| {
                let selected_idx = self
                    .current_answer()
                    .and_then(|answer| answer.options_state.selected_idx);
                let mut rows = options
                    .iter()
                    .enumerate()
                    .map(|(idx, opt)| {
                        let selected = selected_idx.is_some_and(|sel| sel == idx);
                        let prefix = if selected { '›' } else { ' ' };
                        let label = opt.label.as_str();
                        let number = idx + 1;
                        let prefix_label = format!("{prefix} {number}. ");
                        let wrap_indent = UnicodeWidthStr::width(prefix_label.as_str());
                        GenericDisplayRow {
                            selection_style: Some(crate::bottom_pane::selection_style()),
                            name: format!("{prefix_label}{label}"),
                            description: Some(opt.description.clone()),
                            wrap_indent: Some(wrap_indent),
                            ..Default::default()
                        }
                    })
                    .collect::<Vec<_>>();

                if Self::other_option_enabled_for_question(question) {
                    let idx = options.len();
                    let selected = selected_idx.is_some_and(|sel| sel == idx);
                    let prefix = if selected { '›' } else { ' ' };
                    let number = idx + 1;
                    let prefix_label = format!("{prefix} {number}. ");
                    let wrap_indent = UnicodeWidthStr::width(prefix_label.as_str());
                    rows.push(GenericDisplayRow {
                        selection_style: Some(crate::bottom_pane::selection_style()),
                        name: format!("{prefix_label}{OTHER_OPTION_LABEL}"),
                        description: Some(OTHER_OPTION_DESCRIPTION.to_string()),
                        wrap_indent: Some(wrap_indent),
                        ..Default::default()
                    });
                }

                rows
            })
            .unwrap_or_default()
    }

    pub(super) fn options_required_height(&self, width: u16) -> u16 {
        if !self.has_options() {
            return 0;
        }

        let rows = self.option_rows();
        if rows.is_empty() {
            return 1;
        }

        let mut state = self
            .current_answer()
            .map(|answer| answer.options_state)
            .unwrap_or_default();
        if state.selected_idx.is_none() {
            state.selected_idx = Some(0);
        }

        measure_rows_height(&rows, &state, rows.len(), width.max(1))
    }

    /// Options prefer their full wrapped height, which `layout_sections` relies on.
    pub(super) fn options_preferred_height(&self, width: u16) -> u16 {
        self.options_required_height(width)
    }

    fn capture_composer_draft(&self) -> ComposerDraft {
        ComposerDraft::from_composer(&self.composer)
    }

    fn save_current_draft(&mut self) {
        let draft = self.capture_composer_draft();
        let notes_empty = draft.text.trim().is_empty();
        if let Some(answer) = self.current_answer_mut() {
            if answer.answer_committed && answer.draft != draft {
                answer.answer_committed = false;
            }
            answer.draft = draft;
            if !notes_empty {
                answer.notes_visible = true;
            }
        }
    }

    fn restore_current_draft(&mut self) {
        self.composer
            .set_placeholder_text(self.notes_placeholder().to_string());
        self.composer.set_footer_hint_override(Some(Vec::new()));
        let Some(answer) = self.current_answer() else {
            self.composer
                .set_text_content(String::new(), Vec::new(), Vec::new());
            self.composer.move_cursor_to_end();
            return;
        };
        let draft = answer.draft.clone();
        self.composer
            .set_text_content(draft.text, draft.text_elements, draft.local_image_paths);
        self.composer.set_pending_pastes(draft.pending_pastes);
        self.composer.move_cursor_to_end();
    }

    fn notes_placeholder(&self) -> &'static str {
        if self.has_options() && self.selected_option_index().is_none() {
            SELECT_OPTION_PLACEHOLDER
        } else if self.has_options() {
            NOTES_PLACEHOLDER
        } else {
            ANSWER_PLACEHOLDER
        }
    }

    fn sync_composer_placeholder(&mut self) {
        self.composer
            .set_placeholder_text(self.notes_placeholder().to_string());
    }

    /// Move the option selection with wrap-around, un-committing the answer.
    fn move_option_selection(&mut self, down: bool) {
        let options_len = self.options_len();
        let Some(answer) = self.current_answer_mut() else {
            return;
        };
        if down {
            answer.options_state.move_down_wrap(options_len);
        } else {
            answer.options_state.move_up_wrap(options_len);
        }
        answer.answer_committed = false;
        self.sync_composer_placeholder();
    }

    /// Discard the current answer's notes draft and empty the composer.
    fn reset_notes_draft(&mut self, notes_visible: bool) {
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = ComposerDraft::default();
            answer.answer_committed = false;
            answer.notes_visible = notes_visible;
        }
        self.pending_submission_draft = None;
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.composer.move_cursor_to_end();
    }

    fn clear_notes_draft(&mut self) {
        self.reset_notes_draft(/*notes_visible*/ true);
        self.sync_composer_placeholder();
    }

    fn footer_tips(&self) -> Vec<Line<'static>> {
        let mut tips = Vec::new();
        let notes_visible = self.notes_ui_visible();
        if self.has_options() {
            if self.selected_option_index().is_some() && !notes_visible {
                tips.push(shortcut("tab", "to add notes"));
            }
            if self.selected_option_index().is_some() && notes_visible {
                tips.push(Line::from(
                    [
                        crate::key_hint::key_label_spans("tab"),
                        vec![" or ".dim()],
                        crate::key_hint::key_label_spans("esc"),
                        vec![" to clear notes".dim()],
                    ]
                    .concat(),
                ));
            }
        }

        let question_count = self.question_count();
        let is_last_question = self.current_index().saturating_add(1) >= question_count;
        let submit_key = if self.focus_is_notes() || !self.has_options() {
            self.composer_submit_hint.map(ShortcutHint::display_label)
        } else {
            self.list_keymap
                .primary_hint(ListAction::Accept)
                .map(ShortcutHint::display_label)
        };
        if let Some(submit_key) = submit_key {
            let submit_tip = if question_count == 1 {
                shortcut(&submit_key, "to submit answer")
            } else if is_last_question {
                shortcut(&submit_key, "to submit all")
            } else {
                shortcut(&submit_key, "to submit answer")
            };
            tips.push(submit_tip);
        }
        if question_count > 1 {
            if self.has_options() && !self.focus_is_notes() {
                tips.push(shortcut("←/→", "to navigate questions"));
            } else if !self.has_options() {
                tips.push(shortcut(
                    &format!(
                        "{} / {}",
                        crate::key_hint::ctrl(KeyCode::Char('p')).display_label(),
                        crate::key_hint::ctrl(KeyCode::Char('n')).display_label()
                    ),
                    "change question",
                ));
            }
        }
        if let Some(interrupt_key) = self.interrupt_turn_hint
            && !(self.has_options()
                && notes_visible
                && interrupt_key == ShortcutHint::Single(crate::key_hint::plain(KeyCode::Esc)))
        {
            tips.push(shortcut(&interrupt_key.display_label(), "to interrupt"));
        }
        tips
    }

    pub(super) fn footer_tip_lines(&self, width: u16) -> Vec<Vec<Line<'static>>> {
        self.wrap_footer_tips(width, self.footer_tips())
    }

    pub(super) fn footer_tip_lines_with_prefix(
        &self,
        width: u16,
        prefix: Option<Line<'static>>,
    ) -> Vec<Vec<Line<'static>>> {
        let mut tips = Vec::new();
        if let Some(prefix) = prefix {
            tips.push(prefix);
        }
        tips.extend(self.footer_tips());
        self.wrap_footer_tips(width, tips)
    }

    fn wrap_footer_tips(&self, width: u16, tips: Vec<Line<'static>>) -> Vec<Vec<Line<'static>>> {
        crate::footer_hint::wrap_hint_rows(
            tips,
            width,
            UnicodeWidthStr::width(TIP_SEPARATOR),
            Line::width,
        )
    }

    pub(super) fn footer_required_height(&self, width: u16) -> u16 {
        self.footer_tip_lines(width).len() as u16
    }

    /// Ensure the focus mode is valid for the current question.
    fn ensure_focus_available(&mut self) {
        if self.question_count() == 0 {
            return;
        }
        if !self.has_options() {
            self.focus = Focus::Notes;
            if let Some(answer) = self.current_answer_mut() {
                answer.notes_visible = true;
            }
            return;
        }
        if matches!(self.focus, Focus::Notes) && !self.notes_ui_visible() {
            self.focus = Focus::Options;
            self.sync_composer_placeholder();
        }
    }

    /// Rebuild local answer state from the current request.
    fn reset_for_request(&mut self) {
        self.answers = self
            .request
            .questions
            .iter()
            .map(|question| {
                let has_options = question
                    .options
                    .as_ref()
                    .is_some_and(|options| !options.is_empty());
                let mut options_state = ScrollState::new();
                if has_options {
                    options_state.selected_idx = Some(0);
                }
                AnswerState {
                    options_state,
                    draft: ComposerDraft::default(),
                    answer_committed: false,
                    notes_visible: !has_options,
                }
            })
            .collect();

        self.current_idx = 0;
        self.focus = Focus::Options;
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.confirm_unanswered = None;
        self.pending_submission_draft = None;
    }

    fn options_len_for_question(question: &ToolRequestUserInputQuestion) -> usize {
        let options_len = question
            .options
            .as_ref()
            .map(std::vec::Vec::len)
            .unwrap_or(0);
        if Self::other_option_enabled_for_question(question) {
            options_len + 1
        } else {
            options_len
        }
    }

    fn other_option_enabled_for_question(question: &ToolRequestUserInputQuestion) -> bool {
        question.is_other
            && question
                .options
                .as_ref()
                .is_some_and(|options| !options.is_empty())
    }

    fn option_label_for_index(
        question: &ToolRequestUserInputQuestion,
        idx: usize,
    ) -> Option<String> {
        let options = question.options.as_ref()?;
        if idx < options.len() {
            return options.get(idx).map(|opt| opt.label.clone());
        }
        if idx == options.len() && Self::other_option_enabled_for_question(question) {
            return Some(OTHER_OPTION_LABEL.to_string());
        }
        None
    }

    /// Move to the next/previous question, wrapping in either direction.
    fn move_question(&mut self, next: bool) {
        let len = self.question_count();
        if len == 0 {
            return;
        }
        self.save_current_draft();
        let offset = if next { 1 } else { len.saturating_sub(1) };
        self.current_idx = (self.current_idx + offset) % len;
        self.restore_current_draft();
        self.ensure_focus_available();
    }

    fn jump_to_question(&mut self, idx: usize) {
        if idx >= self.question_count() {
            return;
        }
        self.save_current_draft();
        self.current_idx = idx;
        self.restore_current_draft();
        self.ensure_focus_available();
    }

    /// Synchronize selection state to the currently focused option.
    fn select_current_option(&mut self, committed: bool) {
        if !self.has_options() {
            return;
        }
        let options_len = self.options_len();
        let updated = if let Some(answer) = self.current_answer_mut() {
            answer.options_state.clamp_selection(options_len);
            answer.answer_committed = committed;
            true
        } else {
            false
        };
        if updated {
            self.sync_composer_placeholder();
        }
    }

    /// Clear the current option selection and hide notes when empty.
    fn clear_selection(&mut self) {
        if !self.has_options() {
            return;
        }
        if let Some(answer) = self.current_answer_mut() {
            answer.options_state.reset();
        }
        self.reset_notes_draft(/*notes_visible*/ false);
        self.sync_composer_placeholder();
    }

    fn clear_notes_and_focus_options(&mut self) {
        if !self.has_options() {
            return;
        }
        self.reset_notes_draft(/*notes_visible*/ false);
        self.focus = Focus::Options;
        self.sync_composer_placeholder();
    }

    /// Ensure there is a selection before allowing notes entry.
    fn ensure_selected_for_notes(&mut self) {
        if let Some(answer) = self.current_answer_mut() {
            answer.notes_visible = true;
        }
        self.sync_composer_placeholder();
    }

    /// Advance to next question, or submit when on the last one.
    fn go_next_or_submit(&mut self) {
        if self.current_index() + 1 >= self.question_count() {
            self.save_current_draft();
            if self.unanswered_count() > 0 {
                self.open_unanswered_confirmation();
            } else {
                self.submit_answers();
            }
        } else {
            self.move_question(/*next*/ true);
        }
    }

    /// Build the response payload and dispatch it to the app.
    fn submit_answers(&mut self) {
        self.confirm_unanswered = None;
        self.save_current_draft();
        let mut answers = HashMap::new();
        for (idx, question) in self.request.questions.iter().enumerate() {
            let answer_state = &self.answers[idx];
            let options = question.options.as_ref();
            // For option questions we may still produce no selection.
            let selected_idx =
                if options.is_some_and(|opts| !opts.is_empty()) && answer_state.answer_committed {
                    answer_state.options_state.selected_idx
                } else {
                    None
                };
            // Notes are appended as extra answers. For freeform questions, only submit when
            // the user explicitly committed the draft.
            let notes = if answer_state.answer_committed {
                answer_state.draft.text_with_pending().trim().to_string()
            } else {
                String::new()
            };
            let selected_label = selected_idx
                .and_then(|selected_idx| Self::option_label_for_index(question, selected_idx));
            let mut answer_list = selected_label.into_iter().collect::<Vec<_>>();
            if !notes.is_empty() {
                answer_list.push(format!("user_note: {notes}"));
            }
            answers.insert(
                question.id.clone(),
                ToolRequestUserInputAnswer {
                    answers: answer_list,
                },
            );
        }
        if self.send_answers(answers) {
            self.advance_queue_or_complete_at(Instant::now());
        }
    }

    fn submit_empty_auto_resolution(&mut self, now: Instant) {
        self.confirm_unanswered = None;
        if self.send_answers(HashMap::new()) {
            self.advance_queue_or_complete_at(now);
        }
    }

    /// Send `answers` to the app and record them in history; returns false
    /// without sending anything when the request's thread ID is invalid.
    fn send_answers(&self, answers: HashMap<String, ToolRequestUserInputAnswer>) -> bool {
        let Ok(thread_id) = ThreadId::from_string(&self.request.thread_id) else {
            tracing::error!("user input request had an invalid thread ID");
            return false;
        };
        self.app_event_tx.user_input_answer(
            thread_id,
            self.request.turn_id.clone(),
            ToolRequestUserInputResponse {
                answers: answers.clone(),
            },
        );
        self.app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::RequestUserInputResultCell {
                questions: self.request.questions.clone(),
                answers,
                interrupted: false,
            },
        )));
        true
    }

    fn dismiss_resolved_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        let ResolvedAppServerRequest::UserInput { call_id } = request else {
            return false;
        };

        let queue_len = self.queue.len();
        self.queue
            .retain(|queued_request| queued_request.item_id != *call_id);
        if self.request.item_id == *call_id {
            self.advance_queue_or_complete_at(Instant::now());
            return true;
        }

        self.queue.len() != queue_len
    }

    fn open_unanswered_confirmation(&mut self) {
        let mut state = ScrollState::new();
        state.selected_idx = Some(0);
        self.confirm_unanswered = Some(state);
    }

    fn close_unanswered_confirmation(&mut self) {
        self.confirm_unanswered = None;
    }

    fn unanswered_question_count(&self) -> usize {
        self.unanswered_count()
    }

    fn unanswered_submit_description(&self) -> String {
        let count = self.unanswered_question_count();
        let suffix = if count == 1 {
            UNANSWERED_CONFIRM_SUBMIT_DESC_SINGULAR
        } else {
            UNANSWERED_CONFIRM_SUBMIT_DESC_PLURAL
        };
        format!("Submit with {count} unanswered {suffix}")
    }

    fn first_unanswered_index(&self) -> Option<usize> {
        let current_text = self.composer.current_text();
        self.request
            .questions
            .iter()
            .enumerate()
            .find(|(idx, _)| !self.is_question_answered(*idx, &current_text))
            .map(|(idx, _)| idx)
    }

    fn unanswered_confirmation_rows(&self) -> Vec<GenericDisplayRow> {
        let selected = self
            .confirm_unanswered
            .as_ref()
            .and_then(|state| state.selected_idx)
            .unwrap_or(0);
        let entries = [
            (
                UNANSWERED_CONFIRM_SUBMIT,
                self.unanswered_submit_description(),
            ),
            (
                UNANSWERED_CONFIRM_GO_BACK,
                UNANSWERED_CONFIRM_GO_BACK_DESC.to_string(),
            ),
        ];
        entries
            .iter()
            .enumerate()
            .map(|(idx, (label, description))| {
                let prefix = if idx == selected { '›' } else { ' ' };
                let number = idx + 1;
                GenericDisplayRow {
                    selection_style: Some(crate::bottom_pane::selection_style()),
                    name: format!("{prefix} {number}. {label}"),
                    description: Some(description.clone()),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn is_question_answered(&self, idx: usize, _current_text: &str) -> bool {
        let Some(question) = self.request.questions.get(idx) else {
            return false;
        };
        let Some(answer) = self.answers.get(idx) else {
            return false;
        };
        let has_options = question
            .options
            .as_ref()
            .is_some_and(|options| !options.is_empty());
        if has_options {
            answer.options_state.selected_idx.is_some() && answer.answer_committed
        } else {
            answer.answer_committed
        }
    }

    /// Count questions that would submit an empty answer list.
    fn unanswered_count(&self) -> usize {
        let current_text = self.composer.current_text();
        self.request
            .questions
            .iter()
            .enumerate()
            .filter(|(idx, _question)| !self.is_question_answered(*idx, &current_text))
            .count()
    }

    /// Compute the preferred notes input height for the current question.
    fn notes_input_height(&self, width: u16) -> u16 {
        let min_height = MIN_COMPOSER_HEIGHT;
        self.composer
            .desired_height(width.max(1))
            .clamp(min_height, min_height.saturating_add(5))
    }

    fn apply_submission_to_draft(&mut self, text: String, text_elements: Vec<TextElement>) {
        let local_image_paths = self.composer.local_image_paths();
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = ComposerDraft {
                text: text.clone(),
                text_elements: text_elements.clone(),
                local_image_paths: local_image_paths.clone(),
                pending_pastes: Vec::new(),
            };
        }
        self.composer
            .set_text_content(text, text_elements, local_image_paths);
        self.composer.move_cursor_to_end();
        self.composer.set_footer_hint_override(Some(Vec::new()));
    }

    fn apply_submission_draft(&mut self, draft: ComposerDraft) {
        if let Some(answer) = self.current_answer_mut() {
            answer.draft = draft.clone();
        }
        self.composer
            .set_text_content(draft.text, draft.text_elements, draft.local_image_paths);
        self.composer.set_pending_pastes(draft.pending_pastes);
        self.composer.move_cursor_to_end();
        self.composer.set_footer_hint_override(Some(Vec::new()));
    }

    fn handle_composer_input_result(&mut self, result: InputResult) -> bool {
        match result {
            InputResult::Submitted {
                text,
                text_elements,
            }
            | InputResult::Queued {
                text,
                text_elements,
                ..
            } => {
                if self.has_options()
                    && matches!(self.focus, Focus::Notes)
                    && !text.trim().is_empty()
                {
                    let options_len = self.options_len();
                    if let Some(answer) = self.current_answer_mut() {
                        answer.options_state.clamp_selection(options_len);
                    }
                }
                if self.has_options() {
                    if let Some(answer) = self.current_answer_mut() {
                        answer.answer_committed = true;
                    }
                } else if let Some(answer) = self.current_answer_mut() {
                    answer.answer_committed = !text.trim().is_empty();
                }
                let draft_override = self.pending_submission_draft.take();
                if let Some(draft) = draft_override {
                    self.apply_submission_draft(draft);
                } else {
                    self.apply_submission_to_draft(text, text_elements);
                }
                self.go_next_or_submit();
                true
            }
            _ => false,
        }
    }

    fn handle_confirm_unanswered_key_event(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }
        let Some(state) = self.confirm_unanswered.as_mut() else {
            return;
        };

        match key_event.code {
            KeyCode::Esc | KeyCode::Backspace => {
                self.close_unanswered_confirmation();
                if let Some(idx) = self.first_unanswered_index() {
                    self.jump_to_question(idx);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                state.move_up_wrap(/*len*/ 2);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.move_down_wrap(/*len*/ 2);
            }
            KeyCode::Enter => {
                let selected = state.selected_idx.unwrap_or(0);
                self.close_unanswered_confirmation();
                if selected == 0 {
                    self.submit_answers();
                } else if let Some(idx) = self.first_unanswered_index() {
                    self.jump_to_question(idx);
                }
            }
            KeyCode::Char('1') | KeyCode::Char('2') => {
                let idx = if matches!(key_event.code, KeyCode::Char('1')) {
                    0
                } else {
                    1
                };
                state.selected_idx = Some(idx);
            }
            _ => {}
        }
    }
}

impl BottomPaneView for RequestUserInputOverlay {
    fn keymap_contexts(&self) -> crate::keymap::KeymapContextSet {
        if self.confirm_unanswered_active() {
            return crate::keymap::KeymapContextSet::default();
        }
        if matches!(self.focus, Focus::Options) {
            return crate::keymap::KeymapContextSet::new(crate::keymap::KeymapContext::List)
                .with(crate::keymap::KeymapContext::Chat);
        }
        self.composer
            .keymap_contexts()
            .with(crate::keymap::KeymapContext::Chat)
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn will_interrupt_turn_on_key_event(&self, key_event: KeyEvent) -> bool {
        if KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL).is_press(key_event) {
            return self.confirm_unanswered_active()
                || !self.focus_is_notes()
                || self.composer.current_text_with_pending().is_empty();
        }

        key_event.kind != KeyEventKind::Release
            && !self.confirm_unanswered_active()
            && !(matches!(key_event.code, KeyCode::Esc)
                && self.has_options()
                && self.notes_ui_visible())
            && self.interrupt_turn_keys.is_pressed(key_event)
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if key_event.kind == KeyEventKind::Release {
            return;
        }

        self.snooze_auto_resolution();

        if self.confirm_unanswered_active() {
            self.handle_confirm_unanswered_key_event(key_event);
            return;
        }

        if matches!(key_event.code, KeyCode::Esc) && self.has_options() && self.notes_ui_visible() {
            self.clear_notes_and_focus_options();
            return;
        }

        if self.interrupt_turn_keys.is_pressed(key_event) {
            // TODO: Emit interrupted request_user_input results (including committed answers)
            // once core supports persisting them reliably without follow-up turn issues.
            self.app_event_tx.interrupt();
            self.done = true;
            return;
        }

        if self.focus_is_notes() && self.composer_submit_keys.is_pressed(key_event) {
            self.ensure_selected_for_notes();
            self.pending_submission_draft = Some(self.capture_composer_draft());
            let (result, _) = self.composer.handle_key_event(key_event);
            if !self.handle_composer_input_result(result) {
                self.pending_submission_draft = None;
                if self.has_options() {
                    self.select_current_option(/*committed*/ true);
                }
                self.go_next_or_submit();
            }
            return;
        }

        // Question navigation is always available.
        match key_event {
            KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::PageUp,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.move_question(/*next*/ false);
                return;
            }
            KeyEvent {
                code: KeyCode::PageDown,
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.move_question(/*next*/ true);
                return;
            }
            KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Left,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.has_options() && matches!(self.focus, Focus::Options) => {
                self.move_question(/*next*/ false);
                return;
            }
            _ if self.has_options()
                && matches!(self.focus, Focus::Options)
                && self.list_keymap.action_for(key_event) == Some(ListAction::MoveLeft) =>
            {
                self.move_question(/*next*/ false);
                return;
            }
            KeyEvent {
                code: KeyCode::Char('l'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Right,
                modifiers: KeyModifiers::NONE,
                ..
            } if self.has_options() && matches!(self.focus, Focus::Options) => {
                self.move_question(/*next*/ true);
                return;
            }
            _ if self.has_options()
                && matches!(self.focus, Focus::Options)
                && self.list_keymap.action_for(key_event) == Some(ListAction::MoveRight) =>
            {
                self.move_question(/*next*/ true);
                return;
            }
            _ => {}
        }

        match self.focus {
            Focus::Options => {
                // Keep selection synchronized as the user moves.
                match (self.list_keymap.action_for(key_event), key_event.code) {
                    (Some(ListAction::MoveUp), _) | (_, KeyCode::Up | KeyCode::Char('k')) => {
                        self.move_option_selection(/*down*/ false);
                    }
                    (Some(ListAction::MoveDown), _) | (_, KeyCode::Down | KeyCode::Char('j')) => {
                        self.move_option_selection(/*down*/ true);
                    }
                    (_, KeyCode::Char(' ')) => {
                        self.select_current_option(/*committed*/ true);
                    }
                    (_, KeyCode::Backspace | KeyCode::Delete) => {
                        self.clear_selection();
                    }
                    (_, KeyCode::Tab) | (Some(ListAction::Accept), _) | (_, KeyCode::Enter)
                        if self.selected_option_index().is_some()
                            && (key_event.code == KeyCode::Tab
                                || self.current_question().is_some_and(|question| {
                                    Self::other_option_enabled_for_question(question)
                                        && self.selected_option_index()
                                            == question.options.as_ref().map(Vec::len)
                                })) =>
                    {
                        self.focus = Focus::Notes;
                        self.ensure_selected_for_notes();
                    }
                    (Some(ListAction::Accept), _) | (_, KeyCode::Enter) => {
                        let has_selection = self.selected_option_index().is_some();
                        if has_selection {
                            self.select_current_option(/*committed*/ true);
                        }
                        self.go_next_or_submit();
                    }
                    (_, KeyCode::Char(ch)) => {
                        if let Some(option_idx) = self.option_index_for_digit(ch) {
                            if let Some(answer) = self.current_answer_mut() {
                                answer.options_state.selected_idx = Some(option_idx);
                            }
                            self.select_current_option(/*committed*/ true);
                            self.go_next_or_submit();
                        }
                    }
                    _ => {}
                }
            }
            Focus::Notes => {
                let notes_empty = self.composer.current_text_with_pending().trim().is_empty();
                if self.has_options() && matches!(key_event.code, KeyCode::Tab) {
                    self.clear_notes_and_focus_options();
                    return;
                }
                if self.has_options() && matches!(key_event.code, KeyCode::Backspace) && notes_empty
                {
                    self.save_current_draft();
                    if let Some(answer) = self.current_answer_mut() {
                        answer.notes_visible = false;
                    }
                    self.focus = Focus::Options;
                    self.sync_composer_placeholder();
                    return;
                }
                if self.has_options() && matches!(key_event.code, KeyCode::Up | KeyCode::Down) {
                    self.move_option_selection(key_event.code == KeyCode::Down);
                    return;
                }
                self.ensure_selected_for_notes();
                if matches!(
                    key_event.code,
                    KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
                ) && let Some(answer) = self.current_answer_mut()
                {
                    answer.answer_committed = false;
                }
                let before = self.capture_composer_draft();
                let (result, _) = self.composer.handle_key_event(key_event);
                let submitted = self.handle_composer_input_result(result);
                if !submitted {
                    let after = self.capture_composer_draft();
                    if before != after
                        && let Some(answer) = self.current_answer_mut()
                    {
                        answer.answer_committed = false;
                    }
                }
            }
        }
    }

    fn terminal_title_requires_action(&self) -> bool {
        true
    }

    fn program_status_blocked_kind(
        &self,
    ) -> Option<crate::terminal_program_status::ProgramStatusKind> {
        Some(crate::terminal_program_status::ProgramStatusKind::Question)
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if self.confirm_unanswered_active() {
            self.close_unanswered_confirmation();
            // TODO: Emit interrupted request_user_input results (including committed answers)
            // once core supports persisting them reliably without follow-up turn issues.
            self.app_event_tx.interrupt();
            self.done = true;
            return CancellationEvent::Handled;
        }
        if self.focus_is_notes() && !self.composer.current_text_with_pending().is_empty() {
            self.clear_notes_draft();
            return CancellationEvent::Handled;
        }

        // TODO: Emit interrupted request_user_input results (including committed answers)
        // once core supports persisting them reliably without follow-up turn issues.
        self.app_event_tx.interrupt();
        self.done = true;
        CancellationEvent::Handled
    }

    fn is_complete(&self) -> bool {
        self.done
    }

    fn handle_paste(&mut self, pasted: String) -> bool {
        if pasted.is_empty() {
            return false;
        }
        self.snooze_auto_resolution();
        if matches!(self.focus, Focus::Options) {
            // Treat pastes the same as typing: switch into notes.
            self.focus = Focus::Notes;
        }
        self.ensure_selected_for_notes();
        if let Some(answer) = self.current_answer_mut() {
            answer.answer_committed = false;
        }
        self.composer.handle_paste(pasted)
    }

    fn flush_paste_burst_if_due(&mut self) -> bool {
        self.composer.flush_paste_burst_if_due()
    }

    fn is_in_paste_burst(&self) -> bool {
        self.composer.is_in_paste_burst()
    }

    fn pre_draw_tick(&mut self, now: Instant) -> bool {
        self.maybe_auto_resolve_at(now)
    }

    fn next_frame_delay(&self) -> Option<Duration> {
        self.auto_resolution_next_frame_delay_at(Instant::now())
            .into_iter()
            .chain(self.composer.footer_flash_delay())
            .min()
    }

    fn try_consume_user_input_request(
        &mut self,
        request: ToolRequestUserInputParams,
    ) -> Option<ToolRequestUserInputParams> {
        self.queue.push_back(request);
        None
    }

    fn dismiss_app_server_request(&mut self, request: &ResolvedAppServerRequest) -> bool {
        self.dismiss_resolved_request(request)
    }
}

#[cfg(test)]
#[path = "request_user_input_tests.rs"]
mod tests;
