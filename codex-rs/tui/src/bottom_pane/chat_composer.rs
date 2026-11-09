//! The chat composer is the bottom-pane text input state machine.
//! Rich registers retain original payloads across draft clears and composer replacement;
//! only the live register transfers, not undo or repeat history. Copies receive fresh IDs and labels
//! before ordinary attachment pruning, including after each semantic repeat action.
//!
//! It edits [`TextArea`] and attachments, routes popup keys, makes completed slash commands atomic,
//! and handles Enter/newlines. It shows Luna Reserve's yellow arrow and detects unbracketed paste
//! bursts, especially on Windows. Paste timing uses Tokio's clock so asynchronous flush deadlines
//! and input classification share a clock, including in paused-time tests. Copy shortcuts and right
//! clicks preserve selected draft text.
//! Local recall retains full draft metadata for composer submissions and accepted command-line
//! initial prompts; transcript replay seeds earlier prompts separately.
//! When enabled, fullscreen right-click paste requires an editable composer without a selection,
//! search, or blocking view. The app reads clipboard text asynchronously and delivers a normal
//! paste only while the same thread, draft, and cursor remain eligible. Intervening input or focus
//! loss cancels the pending paste; a late clipboard result cannot overwrite newer input.
//! The live voice strip renders after effort ignition, followed by the Astra sparkle when eligible.
//! Owned transcripts keep persistent status below the composer and hints on a separate final row.
//! Shortcut help expands above the composer, with its close hint replacing the final shortcuts row
//! so input and persistent status stay anchored when help opens or closes.
//! Escape dismisses visible shortcut help before editing, transcript backtracking, or interruption.
//! Fullscreen limits the composer height; wheel browsing preserves the caret until editor input.
//! Transcript interactions borrow the hint footer through `ComposerRenderOptions`;
//! its resolved presentation drives height, painting, and the focused cursor together.
//!
//! Preset submenu hints borrow slots and reflow with the composer.
//!
//! Shell mode starts when input inserts a leading `!`, never when deletion exposes one.
//! Literal leading bangs remain ordinary messages on submission and during same-draft restoration.
//! Vim Replace preserves the overwritten leading grapheme until Backspace removes an absorbed `!`.
//! The plain-text preset keeps command prefixes literal, including `!`, so Enter and Tab
//! submit ordinary text without enabling shell mode.
//!
//! # Mention Menus
//!
//! By default, `@` lists plugins, filesystem entries, and skills. Skills are hidden when their
//! owning plugin is listed. `$` lists individual skills and apps, but not plugins.
//! Disabling `mentions_v2` restores file-only `@` search and adds plugins back to `$`.
//! Confirmed `$` picker selections inform future ordering within each match tier.
//!
//! # Key Event Routing
//!
//! Plain Left opens agents from an empty, active composer connected to a local daemon or remote App Server.
//! Explicit editor remaps take precedence.
//! Most key handling goes through [`ChatComposer::handle_key_event`], which dispatches to a
//! popup-specific handler if a popup is visible and otherwise to
//! [`ChatComposer::handle_key_event_without_popup`]. After every handled key, we call
//! [`ChatComposer::sync_popups`] so UI state follows the latest buffer/cursor.
//! A pending backend handoff uses the basic editor and paste detection while holding submission.
//! Redraws preserve the paste detector's window so trailing keys stay with the outgoing composer.
//! Fresh Vim drafts start in Insert; Normal `/` and `?` search the composer.
//! On an empty draft, a standalone Normal `/` enters Insert and opens command completion.
//! Backspace on an empty Vim search query cancels search and any pending operator.
//!
//! # Completion and Popup Dismissal
//!
//! All completion suggestions render above the composer and preserve its footer.
//! Owned transcript frames overlay suggestions with blank rows above and below, without reserving
//! layout space. Those rows show scroll arrows when suggestions extend beyond the visible menu.
//! Warning and transcript views hide suggestions without losing the draft, query, or selection.
//! Measurement, painting, and cursor placement share that layout, including clipped views.
//! Unified mention tabs retain their position across filters; Left/Right also works at a bare `@`.
//!
//! Popup targeting resolves an editable token range around the cursor and treats atomic text
//! elements as hard boundaries. When that range begins immediately after an atomic element, the
//! insertion path first adds a horizontal separator and shifts the range so the completion cannot
//! merge with the atomic element. After replacing the range, completion leaves the cursor after one
//! horizontal separator. It advances across an existing separator when no suffix would be joined;
//! otherwise it inserts a space, preserving the existing separator before a non-whitespace suffix.
//! It also inserts a space rather than crossing a line break.
//! After selecting a skill, common closing punctuation, including full-width sentence marks and
//! the Chinese closing book-title bracket (`》`), takes the place of a newly inserted separator.
//! Existing whitespace stays part of the draft.
//!
//! `Esc` records the active token as dismissed. A completed value that begins with `@` or `$` is
//! also re-dismissed before popup synchronization, because separator affinity can still identify
//! the completed token to the left of the cursor. Synchronization keeps the popup hidden only while
//! the query, complete token text, and ordinal among matching occurrences remain the same.
//! Whitespace and atomic-element edges delimit occurrences, while nested sigils do not. This
//! preserves dismissal across offset-only edits without suppressing a later identical token.
//!
//! Slash-command dismissal is tracked separately. Pressing `Esc` while the command popup is active
//! records the first-line `/name` token and keeps the popup closed while that token is unchanged.
//! Editing the command token clears the dismissal and allows the popup to reopen.
//!
//! # History Navigation (↑/↓)
//!
//! The Up/Down history path is managed by [`ChatComposerHistory`]. It merges:
//!
//! - Persistent cross-session history (text-only storage; task links recover element ranges and
//!   mention bindings, but attachments cannot be restored).
//! - Local in-session history (full text + text elements + local/remote image attachments).
//!
//! Plain-text history recall strips images and their placeholders.
//! When recalling a persistent entry, encoded task links restore atomic elements and bindings.
//! Recall moves the cursor to the end. Question editors copy primary history on recall/search;
//! draft capture cancels previews, and restoration resets traversal.
//! Ctrl+R searches history in the footer and previews matches in the composer.
//! Typing and pasting edit the active search query, including large pastes and image paths.
//! Background recovery updates the saved prompt without changing the query or visible preview.
//! Esc or Ctrl+C restores that prompt, including recovered answers and interrupted input.
//! Enter accepts a matching preview and discards the saved prompt; without a match, search stays open.
//! Vim undo/redo snapshots complete drafts and groups direct edits with active Vim transactions.
//! An active edit keeps one separately capped snapshot; canceling does not evict committed history.
//! Canceled history previews restore history and active commands; accepting another prompt resets them.
//! Normal-mode Ctrl+R redoes an edit, or does nothing when redo is empty. Insert-mode Ctrl+R
//! keeps prompt-history search; explicitly configured keybindings retain precedence.
//! Vim queries stay draft-local.
//!
//! Slash commands are staged for local history instead of being recorded immediately. Command
//! recall is a two-phase handoff: stage the submitted slash text here, then record it after
//! `ChatWidget` dispatches the command.
//!
//! # Question Draft Recovery
//!
//! Live terminal turns recover typed, unsubmitted question answers into the main composer.
//! Each recovered answer follows its original question as a Markdown blockquote, with blank lines
//! separating questions, answers, and existing text. The append flushes buffered input and dismisses
//! unused sparkle eligibility. The separator and recovered answer form one Vim edit; large answers
//! use atomic paste placeholders backed by their original text.
//! Recovery escapes a shell or slash command prefix before appending answers, keeping the
//! combined draft editable and its later submission literal. Shell mode's separate `!` becomes
//! text in the composer; slash-prefixed paths remain unchanged because they submit as prompts.
//!
//! # Startup Draft Handoff
//!
//! Startup uses a provisional plain-text composer: editing remains available while its owner
//! records one submit intent. Popups, attachments, and direct dispatch stay disabled. After the
//! protected startup gates finish, `prepare_startup_submission` applies normal submission parsing
//! once, including expansion of paste placeholders. [`ComposerDraftSnapshot`] transfers its
//! text, cursor, pending paste placeholders, local history, and recent activity to the fully
//! initialized composer.
//! `ChatWidget` merges the draft with any existing initial prompt and attachments, rebasing cursor
//! and placeholder positions while preserving both composers' contents. The draft remains deferred
//! until protected views close, input is enabled, and required sandbox setup completes.
//!
//! # Submission and Prompt Expansion
//!
//! Multiline pastes continue blockquote prefixes.
//! `Enter` submits immediately. `Tab` requests queuing while a task is running; if no task is
//! running, `Tab` submits just like Enter so input is never dropped.
//! Submit and queue bindings act only on fresh key presses; repeated events are consumed so a held
//! key cannot dispatch a preserved draft after another input layer closes.
//! Vim Replace shares Insert's composer actions; only textarea editing differs.
//! Literal completion/paste edits discard stale Replace recovery; subsequent typing records anew.
//! Token markers added during Replace are removed when restoring overwritten characters.
//! `Tab` does not submit when entering a `!` shell command.
//!
//! On submit/queue paths, the composer:
//!
//! - Expands pending paste placeholders so element ranges align with the final text.
//! - Trims whitespace and rebases elements when `trim_submission` is enabled (the default).
//!   Otherwise, preserves both.
//! - Treats a leading `!` revealed only by paste expansion as literal model input, not shell input.
//! - Prunes local attached images so only placeholders that survive expansion are sent.
//! - Preserves remote image URLs as separate attachments even when text is empty.
//!
//! When these paths clear the visible textarea after a successful submit or slash-command
//! dispatch, they intentionally preserve the textarea kill buffer. That lets users `Ctrl+K` part
//! of a draft, perform a composer action such as changing reasoning level, and then `Ctrl+Y` the
//! killed text back into the now-empty draft. Replacing the chat widget carries that buffer into
//! the fresh composer so Vim yanks survive `/new`, `/cd`, and thread switches. Rich objects retain
//! their original image paths, mention targets, and collapsed-paste contents. Pasted objects receive
//! fresh identities and labels, including in Replace mode, where Backspace removes them atomically.
//!
//! The numeric auto-submit path used by the slash popup performs the same pending-paste expansion
//! and attachment pruning, and clears pending paste state on success.
//! Slash commands with arguments (like `/plan` and `/review`) reuse the same preparation path so
//! pasted content and text elements are preserved when extracting args.
//! Commands requiring dispatch validation retain their draft and defer busy-state checks to
//! `ChatWidget`, which has the thread state needed to accept or reject the command.
//!
//! # Parent-Owned Thread Mode
//!
//! Parent-owned subagent threads keep the draft editable while blocking agent-directed submission.
//! On the `Enter` and `Tab` submission paths, normal prompts, disallowed slash commands, and `!`
//! shell commands return `ParentOwnedInputBlocked` without clearing the draft. Bare local and
//! navigation slash commands remain available so users can leave or manage the view.
//! Exports and local display commands (`/title` and `/statusline`) also accept arguments.
//!
//! During reconnection, `handle_restricted_key` edits the draft directly without popup dispatch,
//! composer shortcuts, or submission; `?` becomes literal input. Enter and Tab leave the draft
//! intact until reconnection succeeds, except for `/warnings`, `/config`, `/quit`, and `/exit`.
//! When connected but the thread is unavailable, configured
//! submit keys may dispatch explicitly allowed recovery and local commands; other drafts stay put.
//! Empty-prompt agents navigation remains available with its usual editor and draft guards.
//! Collapsed pastes expand into editable text so the full draft can be copied before quitting.
//!
//! # Reasoning Effort Animations
//!
//! The composer observes the effective reasoning tier whenever model-dependent surfaces refresh.
//! The first observation, session configuration, and restored threads establish a baseline;
//! genuine changes to Max/Ultra can then queue composer and status-line transitions when motion
//! and color support allow them.
//! Repeated selections do not restart either effect, dropping below Max clears them, and
//! restoration clears any transition queued while replaying the saved session. Rendering advances
//! active transitions through the frame requester until they finish.
//!
//! # Astra Sparkle Eligibility
//!
//! A confirmed new task can offer one flourish when it starts with Astra or the user switches to
//! Astra through `/model` before entering ordinary prompt content. The draft tracks whether it is
//! untouched, contains a recognized slash command, or is permanently dismissed. An eligible slash
//! command keeps the opportunity while being typed, dispatched, or erased; a fresh Astra response
//! can arm the effect while the command is still pending. Stars stay hidden until the composer is
//! empty. Commands that insert draft content, ordinary typed, pasted, or restored drafts,
//! attachments, and real work dismiss the opportunity even if that content is later cleared.
//! Ordinary disconnected edits also dismiss it immediately, even when erased before rendering;
//! a recognized command typed and erased offline still preserves the opportunity.
//! Opening history search, typing or pasting its query, and canceling it with `Esc` or `Ctrl+C`
//! temporarily hide an active field without restarting its deadline. Any buffered keys are
//! integrated into the original draft without being classified again. A preview does not count
//! as a draft until the user accepts a nonempty match.
//! Toggling shortcut help on an empty composer also hides an active field until help closes,
//! without restarting its deadline, and leaves an unused opportunity intact. A literal `?` is
//! ordinary prompt content.
//! Startup draft snapshots carry this distinction so erased ordinary input does not become
//! untouched on handoff.
//!
//! Sparkle key and paste handling runs only for input routed to the composer; ordinary characters
//! are observed before the paste-burst buffer can hide the first edit. An edit that flushes and
//! erases a held slash also clears the tracked command. An empty main-composer `Esc`
//! can stop the active flourish. Once shown, it fades out by 15 seconds; command entry, popups, and
//! focus loss hide the field without restarting that deadline. Rendering runs after the textarea,
//! placeholder, effort ignition, and voice strip, and draws only in eligible blank cells without
//! overwriting the placeholder or normal cursor. Hidden frames do not schedule animation redraws;
//! motion settings, the starfield preference, and true-color support also gate the effect.
//!
//! # Large Paste Placeholders
//!
//! Large pastes insert an element placeholder in the buffer and store the full text in
//! `pending_pastes`. The placeholder label is derived from the pasted character count:
//!
//! - First paste of a given size uses `[Pasted Content N chars]`.
//! - Additional pending pastes of the same size add a numeric suffix (`#2`, `#3`, ...), where the
//!   next suffix is computed from the placeholders that still exist in `pending_pastes`.
//! - When all placeholders for a size are cleared or deleted, the next paste of that size reuses
//!   the base label without a suffix.
//!
//! On supported terminals the local draft references also have a strip of thumbnails.
//! It shows the first attachments that fit and a count of those hidden. Removing a
//! reference removes its preview through the same attachment bookkeeping used for submission.
//!
//! # Remote Image Rows (Up/Down/Delete)
//!
//! Remote image URLs are rendered as non-editable `[Image #N]` rows above the textarea (inside the
//! same composer block). These rows represent image attachments rehydrated from app-server/backtrack
//! history; TUI users can remove them, but cannot type into that row region.
//!
//! Keyboard behavior:
//!
//! - `Up` at textarea cursor `0` enters remote-row selection at the last remote image.
//! - `Up`/`Down` move selection between remote rows.
//! - `Down` on the last row clears selection and returns control to the textarea.
//! - `Delete`/`Backspace` remove the selected remote image row.
//!
//! Placeholder numbering is unified across remote and local images:
//!
//! - Remote rows occupy `[Image #1]..[Image #M]`.
//! - Local placeholders are offset after that range (`[Image #M+1]..`).
//! - Deleting a remote row relabels local placeholders to keep numbering contiguous.
//!
//! # Non-bracketed Paste Bursts
//!
//! Some terminals, especially Windows, deliver pastes as rapid `KeyCode::Char`/`Enter`/`Tab` events.
//! [`PasteBurst`](super::paste_burst::PasteBurst) buffers text-producing keys (plain, Shift, or
//! Windows AltGr) and integrates buffered text through the composer, avoiding transient shortcut UI.
//! It briefly holds the first ASCII char or an eligible skill-completion punctuation mark to detect
//! a burst. Other non-ASCII chars appear immediately for IME responsiveness, but still participate
//! in burst detection.
//! Parent views must keep flushing editors that lose focus while input is buffered; a hidden
//! editor's pending burst can otherwise keep the shared draw loop waiting indefinitely.
//!
//! Submission flushes expired characters and buffers before classifying Enter, independent of
//! UI flush ticks. `disable_paste_burst` bypasses detection; setting it flushes and clears in-flight state.
//! Mouse edits flush pending typing; selection and copy behavior lives in [`mouse`]. Confirmed
//! explicit copies clear the selection while preserving the draft and cursor. Footer text uses a separate
//! read-only selection owner over its final rendered cells.
//!
//! See `codex-rs/tui/src/bottom_pane/paste_burst.rs` for the detailed state machine.
//!
//! # PasteBurst Integration Points
//!
//! The burst detector is consulted in a few specific places:
//!
//! - [`ChatComposer::handle_input_basic`]: flushes any due burst first, then intercepts plain char
//!   input to either buffer it or insert normally.
//! - [`ChatComposer::handle_non_ascii_char`]: handles the non-ASCII/IME path without holding the
//!   first char, while still allowing paste detection via retro-capture.
//! - Unmodified Tab joins detected bursts, including short Unicode prefixes, before popup dispatch.
//!   Expired bursts are flushed first so manual Tab keeps its normal shortcut behavior.
//! - [`ChatComposer::flush_paste_burst_if_due`]/[`ChatComposer::handle_paste_burst_flush`]: called
//!   from UI ticks to integrate a pending burst via [`ChatComposer::apply_paste`] or insert a
//!   held single character. Explicit paste events (terminal or clipboard) use
//!   [`ChatComposer::handle_paste`], which first commits buffered typing before classifying the
//!   pasted text. This preserves the order when a paste arrives before the next burst-flush tick;
//!   buffered keys have already been classified and go directly through `apply_paste`.
//!   Image detection probes only regular files. Pasted device and FIFO paths stay literal.
//! - Accepted clipboard images during Cloud handoff settle buffered typing before inserting their
//!   placeholder. This edit keeps a newly settled leading bang literal if navigation fails.
//! - Startup recovery previews held keys with the live Vim Replace policy without consuming them.
//!   Buffered pastes remain literal in recovery so large-paste and image classification stays with
//!   the live paste path.
//!
//! # Input Disabled Mode
//!
//! The composer can be temporarily read-only (`input_enabled = false`). In that mode it ignores
//! edits and renders a placeholder prompt instead of the editable textarea. This is part of the
//! overall state machine, since it affects which transitions are even possible from a given UI
//! state.
//!
//! # Module Layout
//!
//! This file defines the composer types and construction; the behavior above lives in child
//! modules. `key_dispatch` holds [`ChatComposer::handle_key_event`], the popup-specific key
//! handlers, and [`ChatComposer::handle_key_event_without_popup`]. `basic_input` applies textarea
//! edits, including the paste-burst and non-ASCII/IME paths, and `submission` prepares
//! Enter/`Tab` submissions and dispatches slash commands. `popup_sync` holds
//! [`ChatComposer::sync_popups`], while `mention_completion` resolves completion tokens and
//! mention bindings. `draft_api` exposes text, draft, attachment, and history accessors;
//! `config_setters` holds feature, keymap, and footer setters; and `rendering` draws the composer.
//!
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::ShortcutHint;
use crate::key_hint::has_ctrl_or_alt;
use codex_message_history::HistoryBatchCursor;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;

use codex_protocol::openai_models::ReasoningEffort;

use super::chat_composer_history::ChatComposerHistory;
use super::chat_composer_history::HistoryEntry;
use super::chat_composer_history::HistoryEntryResponse;
use super::chat_composer_history::HistorySearchResult;
use super::command_popup::CommandItem;
use super::effort_ignition::EffortIgnition;
use super::effort_ignition::EffortTier;
use super::effort_ignition::IGNITION_FRAME_TICK;
use super::effort_ignition::IgnitionStyle;
use super::effort_status_line::EFFORT_STATUS_LINE_FRAME_TICK;
use super::effort_status_line::EffortStatusLineTransition;
use super::file_search_popup::FileSearchPopup;
use super::footer::CollaborationModeIndicator;
use super::footer::FooterMode;
use super::footer::FooterProps;
use super::footer::GoalStatusIndicator;
use super::footer::SummaryLeft;
use super::footer::can_show_left_with_context;
use super::footer::context_window_line;
use super::footer::esc_hint_mode;
use super::footer::footer_height;
use super::footer::footer_hint_items_width;
use super::footer::footer_line_width;
use super::footer::inset_footer_hint_area;
use super::footer::max_left_width_for_right;
use super::footer::passive_footer_status_line;
use super::footer::render_context_right;
use super::footer::render_footer_from_props;
use super::footer::render_footer_hint_items;
use super::footer::render_footer_line;
use super::footer::reset_mode_after_activity;
use super::footer::side_conversation_context_line;
use super::footer::single_line_footer_layout;
use super::footer::status_line_right_indicator_line;
use super::footer::toggle_shortcut_mode;
use super::footer::uses_passive_footer_status_layout;
use super::mention_usage::MentionUsage;
use super::mentions_v2::MentionV2Popup;
use super::mentions_v2::MentionV2Selection;
use super::paste_burst::CharDecision;
use super::paste_burst::PasteBurst;
use super::prompt_args::parse_slash_name;
use super::skill_popup::MentionItem;
use super::skill_popup::SkillPopup;
use super::slash_commands::ServiceTierCommand;
use super::slash_commands::SlashCommandItem;
use super::voice_strip::VoiceStrip;
use crate::bottom_pane::footer_content::FooterContent;
use crate::history_cell::sanitize_user_text;
use crate::key_hint::KeyBindingListExt;
use crate::keymap::EditorKeymap;
use crate::keymap::KeymapContext;
use crate::keymap::KeymapContextSet;
use crate::keymap::RuntimeKeymap;
use crate::keymap::VimNormalKeymap;
use crate::keymap::user_bindings;
use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use crate::style::user_message_style;
use crate::ui_consts::FOOTER_INDENT_COLS;
use codex_protocol::ThreadId;
use codex_protocol::user_input::ByteRange;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use codex_protocol::user_input::TextElement;

mod agents_navigation;
mod attachment_state;
mod basic_input;
mod completion_target;
mod composer_layout;
mod config_setters;
mod draft_api;
mod draft_state;
mod footer_state;
mod history_search;
mod inline_input;
mod key_dispatch;
mod mention_completion;
mod mouse;
pub(crate) use mouse::ClipboardPasteTarget;
mod paste_input;
mod popup_state;
mod popup_sync;
mod reconnect;
pub(crate) use reconnect::RestrictedInputMode;
mod rendering;
mod slash_input;
mod sparkle;
mod status_surface;
mod submission;
mod vim_history;
mod vim_register;
mod vim_search;
mod warning_notice;

use self::config_setters::footer_insert_newline_key;
use self::mention_completion::ends_plaintext_at_dollar_mention;
use self::mention_completion::ends_plaintext_at_mention;
use self::mention_completion::is_mention_name_char;
// Child modules keep their moved `super::mentions_v2` and `super::shortcut_overlay` paths.
use super::mentions_v2;
use super::shortcut_overlay;

use self::attachment_state::AttachmentState;
use self::draft_state::ComposerMentionBinding;
use self::draft_state::DraftState;
pub(crate) use self::footer_state::CommandPopupPlacement;
use self::footer_state::ComposerLayout;
pub(crate) use self::footer_state::ComposerRenderOptions;
use self::footer_state::FooterState;
pub(crate) use self::footer_state::TranscriptFooter;
use self::history_search::HistorySearchSession;
use self::popup_state::ActivePopup;
use self::popup_state::DismissedToken;
use self::popup_state::PopupState;
use self::slash_input::SlashInput;
use self::slash_input::SlashValidation;
use self::slash_input::SubmissionValidation;
use self::vim_history::VimHistory;
use crate::app_event::AppEvent;
use crate::app_event::ConnectorsSnapshot;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::textarea::TextArea;
use crate::clipboard_paste::normalize_pasted_path;
use crate::clipboard_paste::pasted_image_format;
use crate::history_cell;
use crate::skills_helpers::skill_display_name;
use crate::tui::FrameRequester;
use crate::ui_consts::LIVE_PREFIX_COLS;
#[cfg(test)]
use codex_app_server_protocol::SkillInterface;
use codex_app_server_protocol::SkillMetadata;
use codex_connectors::AppInfo;
use codex_file_search::FileMatch;
#[cfg(test)]
use codex_plugin::AppConnectorId;
use codex_plugin::PluginCapabilitySummary;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
pub(crate) use vim_register::KillBufferSnapshot;

use ratatui::style::Color;

/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

fn user_input_too_large_message(actual_chars: usize) -> String {
    format!(
        "Message exceeds the maximum length of {MAX_USER_INPUT_TEXT_CHARS} characters ({actual_chars} provided)."
    )
}

/// Result returned when the user interacts with the text area.
#[derive(Debug, PartialEq)]
pub enum InputResult {
    Submitted {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Queued {
        text: String,
        text_elements: Vec<TextElement>,
        action: QueuedInputAction,
        pending_pastes: Vec<(String, String)>,
    },
    /// A bare slash command parsed by the composer.
    ///
    /// Callers that dispatch this variant are also responsible for resolving any pending local
    /// command-history entry that the composer staged before clearing the visible input.
    Command(SlashCommand),
    /// A bare model service-tier command parsed by the composer.
    ServiceTierCommand(ServiceTierCommand),
    /// An inline slash command and its trimmed argument text.
    ///
    /// The `TextElement` ranges are rebased into the argument string, while any pending local
    /// command-history entry still represents the original command invocation that should be
    /// committed only if dispatch accepts it.
    CommandWithArgs(SlashCommand, String, Vec<TextElement>),
    /// Agent-directed input was attempted while viewing a parent-owned spawned child thread.
    ParentOwnedInputBlocked,
    None,
}

fn parent_owned_command_is_allowed(command: SlashCommand, args: &str) -> bool {
    if matches!(
        command,
        SlashCommand::Export | SlashCommand::Title | SlashCommand::Statusline
    ) {
        return true;
    }

    args.is_empty()
        && matches!(
            command,
            SlashCommand::Feedback
                | SlashCommand::New
                | SlashCommand::Clear
                | SlashCommand::Resume
                | SlashCommand::App
                | SlashCommand::Side
                | SlashCommand::Btw
                | SlashCommand::Agents
                | SlashCommand::MultiAgents
                | SlashCommand::Vim
                | SlashCommand::Keymap
                | SlashCommand::Config
                | SlashCommand::ElevateSandbox
                | SlashCommand::Experimental
                | SlashCommand::Memories
                | SlashCommand::Quit
                | SlashCommand::Exit
                | SlashCommand::Logout
                | SlashCommand::Copy
                | SlashCommand::Raw
                | SlashCommand::Diff
                | SlashCommand::Mention
                | SlashCommand::Skills
                | SlashCommand::Import
                | SlashCommand::Hooks
                | SlashCommand::Status
                | SlashCommand::Warnings
                | SlashCommand::Daemon
                | SlashCommand::Usage
                | SlashCommand::Ide
                | SlashCommand::DebugConfig
                | SlashCommand::Theme
                | SlashCommand::Pets
                | SlashCommand::Ps
                | SlashCommand::Stop
                | SlashCommand::MemoryDrop
                | SlashCommand::MemoryUpdate
                | SlashCommand::Mcp
                | SlashCommand::Apps
                | SlashCommand::Plugins
                | SlashCommand::Rollout
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedInputAction {
    Plain,
    /// Preserve model-input provenance when paste expansion reveals a leading shell sigil.
    Literal,
    ParseSlash,
    RunShell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingPasteHandling {
    Expand,
    Preserve,
}

/// Shared composer behavior; defaults match the main chat input.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatComposerConfig {
    /// Whether command/file/skill popups are allowed to appear.
    pub(crate) popups_enabled: bool,
    /// Whether `/...` input is parsed and dispatched as slash commands.
    pub(crate) slash_commands_enabled: bool,
    /// Whether a leading `!` switches the editor into shell mode.
    pub(crate) shell_commands_enabled: bool,
    /// Whether pasting a file path can attach local images.
    pub(crate) image_paste_enabled: bool,
    pub(crate) blockquote_paste_enabled: bool,
    /// Strip leading and trailing whitespace from submissions.
    pub(crate) trim_submission: bool,
    /// Embedded editors reset Vim only when their owner accepts the answer.
    pub(crate) reset_vim_on_submission: bool,
}

impl Default for ChatComposerConfig {
    fn default() -> Self {
        Self {
            popups_enabled: true,
            slash_commands_enabled: true,
            shell_commands_enabled: true,
            image_paste_enabled: true,
            blockquote_paste_enabled: true,
            trim_submission: true,
            reset_vim_on_submission: true,
        }
    }
}

impl ChatComposerConfig {
    /// Text answers support Markdown, without popups, commands, or image attachments.
    pub(crate) const fn plain_text() -> Self {
        Self {
            popups_enabled: false,
            slash_commands_enabled: false,
            shell_commands_enabled: false,
            image_paste_enabled: false,
            blockquote_paste_enabled: true,
            trim_submission: true,
            reset_vim_on_submission: true,
        }
    }
}

pub(crate) struct ChatComposer {
    draft: DraftState,
    popups: PopupState,
    pub(super) mention_usage: MentionUsage,
    app_event_tx: AppEventSender,
    history: ChatComposerHistory,
    pub(super) command_center_enabled: bool,
    // Keep the footer payload out of the composer's stack frame during session navigation.
    footer: Box<FooterState>,
    has_focus: bool,
    frame_requester: Option<FrameRequester>,
    sparkle: sparkle::Sparkle,
    effort_tier: Option<EffortTier>,
    effort_animation_style: Option<IgnitionStyle>,
    effort_ignition: Option<EffortIgnition>,
    voice_strip: Option<VoiceStrip>,
    effort_status_line_transition: Option<EffortStatusLineTransition>,
    effort_observed: bool,
    luna_reserve_active: bool,
    attachments: AttachmentState,
    placeholder_text: String,
    blocks_direct_input: bool,
    is_task_running: bool,
    queue_submissions: bool,
    /// Slash-command draft staged for local recall after application-level dispatch.
    ///
    /// This slot is intentionally separate from `ChatComposerHistory` so inline slash commands can
    /// prepare their argument text without also double-recording the full command invocation.
    pending_slash_command_history: Option<HistoryEntry>,
    skills: Option<Vec<SkillMetadata>>,
    plugins: Option<Vec<PluginCapabilitySummary>>,
    task_mentions: Option<Vec<crate::task_mentions::TaskMention>>,
    connectors_snapshot: Option<ConnectorsSnapshot>,
    collaboration_modes_enabled: bool,
    config: ChatComposerConfig,
    connectors_enabled: bool,
    plugins_command_enabled: bool,
    token_activity_command_enabled: bool,
    service_tier_commands_enabled: bool,
    service_tier_commands: Vec<ServiceTierCommand>,
    mentions_v2_enabled: bool,
    goal_command_enabled: bool,
    voice_command_enabled: bool,
    worktrees_enabled: bool,
    windows_degraded_sandbox_active: bool,
    side_conversation_active: bool,
    history_search: Option<HistorySearchSession>,
    vim_history: VimHistory,
    vim_register: vim_register::VimRegister,
    submit_keys: Vec<KeyBinding>,
    queue_keys: Vec<KeyBinding>,
    toggle_shortcuts_keys: Vec<KeyBinding>,
    history_search_previous_keys: Vec<KeyBinding>,
    history_search_next_keys: Vec<KeyBinding>,
    editor_keymap: Arc<EditorKeymap>,
    vim_normal_keymap: VimNormalKeymap,
}

/// A resolved legacy `$` target plus any catalog built while disambiguating shell syntax.
struct MentionCompletionTarget {
    range: Range<usize>,
    query: String,
    prebuilt_mentions: Option<Vec<MentionItem>>,
}

#[derive(Clone, Copy)]
pub(crate) enum ShellPrefixHandling {
    Interpret,
    Literal,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ComposerDraft {
    is_bash_mode: bool,
    pub(super) text: String,
    text_elements: Vec<TextElement>,
    local_image_paths: Vec<PathBuf>,
    remote_image_urls: Vec<String>,
    mention_bindings: Vec<MentionBinding>,
    mention_ranges: Vec<Range<usize>>,
    pending_pastes: Vec<(String, String)>,
    cursor: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ComposerDraftSnapshot {
    pub(crate) vim_normal_mode: bool,
    pub(crate) text: String,
    pub(crate) shell_mode: Option<bool>,
    pub(crate) cursor: usize,
    pub(crate) text_elements: Vec<TextElement>,
    pub(crate) local_images: Vec<LocalImageAttachment>,
    pub(crate) remote_image_urls: Vec<String>,
    pub(crate) mention_bindings: Vec<MentionBinding>,
    pub(crate) pending_pastes: Vec<(String, String)>,
    pub(crate) startup_local_history: Vec<HistoryEntry>,
    pub(crate) last_composer_activity_at: Option<Instant>,
    pub(crate) sparkle_draft: sparkle::SparkleDraft,
}

impl ChatComposer {
    fn slash_input(&self) -> SlashInput<'_> {
        SlashInput::new(
            self.slash_commands_enabled(),
            self.draft.is_bash_mode,
            self.builtin_command_flags(),
            &self.service_tier_commands,
        )
    }

    pub fn new(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
    ) -> Self {
        Self::new_with_config(
            has_input_focus,
            app_event_tx,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            ChatComposerConfig::default(),
        )
    }

    /// Construct a composer with explicit feature gating.
    ///
    /// This enables reuse in contexts like request-user-input where we want
    /// the same visuals and editing behavior without slash commands or popups.
    pub(crate) fn new_with_config(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
        config: ChatComposerConfig,
    ) -> Self {
        let use_shift_enter_hint = enhanced_keys_supported;
        let default_keymap = RuntimeKeymap::defaults();
        let default_editor_keymap = default_keymap.editor.clone();
        let default_vim_normal_keymap = default_keymap.vim_normal.clone();

        let mut this = Self {
            draft: DraftState::new(),
            popups: PopupState::default(),
            mention_usage: MentionUsage::default(),
            app_event_tx,
            history: ChatComposerHistory::new(),
            command_center_enabled: false,
            footer: Box::new(FooterState {
                quit_shortcut_expires_at: None,
                quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
                esc_backtrack_hint: false,
                use_shift_enter_hint,
                mode: FooterMode::ComposerEmpty,
                hint_override: None,
                flash: None,
                context_window_percent: None,
                context_window_used_tokens: None,
                context_window_pending: false,
                collaboration_mode_indicator: None,
                goal_status_indicator: None,
                ide_context_active: false,
                status_line_value: None,
                measured_status_line: Default::default(),
                status_line_enabled: false,
                side_conversation_context_label: None,
                active_agent_label: None,
                external_editor_key: default_keymap
                    .primary_hint(KeymapContext::Global, "open_external_editor"),
                warning_notice_area: std::cell::Cell::default(),
                selection_revision: std::cell::Cell::default(),
                show_warnings_key: default_keymap
                    .primary_hint(KeymapContext::Global, "open_warnings"),
                show_transcript_key: default_keymap
                    .primary_hint(KeymapContext::Global, "open_transcript"),
                find_transcript_key: default_keymap
                    .primary_hint(KeymapContext::Global, "find_transcript"),
                focus_activity_key: default_keymap
                    .primary_hint(KeymapContext::Global, "focus_activity"),
                insert_newline_key: footer_insert_newline_key(
                    &default_keymap.editor.insert_newline,
                    use_shift_enter_hint,
                )
                .map(ShortcutHint::from),
                queue_key: default_keymap.primary_hint(KeymapContext::Composer, "queue"),
                toggle_shortcuts_key: default_keymap
                    .primary_hint(KeymapContext::Composer, "toggle_shortcuts"),
                history_search_key: default_keymap
                    .primary_hint(KeymapContext::Composer, "history_search_previous"),
                reasoning_down_key: default_keymap
                    .primary_hint(KeymapContext::Chat, "decrease_reasoning_effort"),
                reasoning_up_key: default_keymap
                    .primary_hint(KeymapContext::Chat, "increase_reasoning_effort"),
                toggle_voice_key: default_keymap.primary_hint(KeymapContext::Chat, "toggle_voice"),
            }),
            has_focus: has_input_focus,
            frame_requester: None,
            sparkle: sparkle::Sparkle::default(),
            effort_tier: None,
            effort_animation_style: None,
            effort_ignition: None,
            voice_strip: None,
            effort_status_line_transition: None,
            effort_observed: false,
            luna_reserve_active: false,
            attachments: AttachmentState::default(),
            placeholder_text,
            blocks_direct_input: false,
            is_task_running: false,
            queue_submissions: false,
            pending_slash_command_history: None,
            skills: None,
            plugins: None,
            task_mentions: None,
            connectors_snapshot: None,
            collaboration_modes_enabled: false,
            config,
            connectors_enabled: false,
            plugins_command_enabled: false,
            token_activity_command_enabled: false,
            service_tier_commands_enabled: false,
            service_tier_commands: Vec::new(),
            mentions_v2_enabled: false,
            goal_command_enabled: false,
            voice_command_enabled: false,
            worktrees_enabled: false,
            windows_degraded_sandbox_active: false,
            side_conversation_active: false,
            history_search: None,
            vim_history: VimHistory::default(),
            vim_register: vim_register::VimRegister::default(),
            submit_keys: vec![key_hint::plain(KeyCode::Enter)],
            queue_keys: vec![key_hint::plain(KeyCode::Tab)],
            toggle_shortcuts_keys: vec![
                key_hint::plain(KeyCode::Char('?')),
                key_hint::shift(KeyCode::Char('?')),
            ],
            history_search_previous_keys: default_keymap.composer.history_search_previous.clone(),
            history_search_next_keys: default_keymap.composer.history_search_next.clone(),
            editor_keymap: default_editor_keymap,
            vim_normal_keymap: default_vim_normal_keymap,
        };
        this.draft.textarea.set_keymap_bindings(&default_keymap);
        // Apply configuration via the setter to keep side-effects centralized.
        this.set_disable_paste_burst(disable_paste_burst);
        this
    }
}

#[cfg(test)]
#[path = "chat_composer/agents_navigation_tests.rs"]
mod agents_navigation_tests;

#[cfg(test)]
#[path = "chat_composer_effort_tests.rs"]
mod effort_tests;

#[cfg(test)]
#[path = "chat_composer_enter_tests.rs"]
mod enter_tests;

#[cfg(test)]
#[path = "chat_composer/embedded_input_tests.rs"]
mod embedded_input_tests;

#[cfg(test)]
#[path = "chat_composer/paste_tests.rs"]
mod paste_tests;

#[cfg(test)]
#[path = "chat_composer/snapshot_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
#[path = "chat_composer/mentions_layout_tests.rs"]
mod mentions_layout_tests;

#[cfg(test)]
#[path = "chat_composer/skill_completion_tests.rs"]
mod skill_completion_tests;

#[cfg(test)]
#[path = "chat_composer_tests.rs"]
mod tests;
