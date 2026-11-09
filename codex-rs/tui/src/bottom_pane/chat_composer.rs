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
mod completion_target;
mod composer_layout;
mod draft_state;
mod footer_state;
mod history_search;
mod inline_input;
mod mouse;
pub(crate) use mouse::ClipboardPasteTarget;
mod paste_input;
mod popup_state;
mod reconnect;
pub(crate) use reconnect::RestrictedInputMode;
mod slash_input;
mod sparkle;
mod status_surface;
mod vim_history;
mod vim_register;
mod vim_search;
mod warning_notice;

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

    pub(crate) fn set_frame_requester(&mut self, frame_requester: FrameRequester) {
        self.frame_requester = Some(frame_requester);
    }

    /// Records the effective reasoning tier, captures the outgoing status
    /// line, and queues the one-shot effects for a genuine Max/Ultra change
    /// after the initial baseline.
    pub(crate) fn set_active_reasoning_effort(
        &mut self,
        effort: Option<&ReasoningEffort>,
        animations_enabled: bool,
    ) -> bool {
        let tier = EffortTier::from_effort(effort);
        let is_baseline = !self.effort_observed;
        self.effort_observed = true;
        if self.effort_tier == tier {
            return false;
        }
        self.effort_tier = tier;
        self.effort_ignition = None;
        self.effort_status_line_transition = None;
        if let Some(tier) = tier
            && !is_baseline
            && animations_enabled
        {
            let style = IgnitionStyle::random(self.effort_animation_style);
            self.effort_ignition = Some(EffortIgnition::new(tier, style));
            self.effort_animation_style = Some(style);
            if self.footer.status_line_enabled
                && let Some(FooterContent::HyperlinkLine(previous)) =
                    passive_footer_status_line(&self.footer_props())
            {
                self.effort_status_line_transition =
                    Some(EffortStatusLineTransition::new(tier, previous));
            }
            if let Some(frame_requester) = &self.frame_requester {
                frame_requester.schedule_frame();
            }
        }
        true
    }

    /// Establishes the current tier without retaining or starting a one-shot effect.
    pub(crate) fn set_active_reasoning_effort_baseline(
        &mut self,
        effort: Option<&ReasoningEffort>,
    ) {
        self.effort_tier = EffortTier::from_effort(effort);
        self.effort_observed = true;
        self.effort_ignition = None;
        self.effort_status_line_transition = None;
    }

    pub fn set_skill_mentions(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.skills = skills;
        self.refresh_mentions_v2_popup_candidates();
        self.sync_popups();
    }

    pub fn set_plugin_mentions(&mut self, plugins: Option<Vec<PluginCapabilitySummary>>) {
        self.plugins = plugins;
        self.refresh_mentions_v2_popup_candidates();
        self.sync_popups();
    }

    pub(crate) fn set_task_mentions_enabled(&mut self, enabled: bool) {
        self.task_mentions = enabled.then(Vec::new);
        self.refresh_mentions_v2_popup_candidates();
        if enabled
            && matches!(self.popups.active, ActivePopup::MentionV2(_))
            && let Some(query) = self.current_mentions_v2_token()
            && !query.is_empty()
        {
            self.app_event_tx.send(AppEvent::StartFileSearch(query));
        }
    }

    pub(crate) fn task_mentions_enabled(&self) -> bool {
        self.task_mentions.is_some()
    }

    pub(crate) fn on_task_search_result(
        &mut self,
        query: &str,
        mut matches: Vec<crate::task_mentions::TaskMention>,
    ) {
        if self.current_mentions_v2_token().as_deref() != Some(query) {
            return;
        }
        let mentioned_ids = self
            .current_mention_elements()
            .into_iter()
            .filter_map(|(id, _, _)| self.draft.mention_bindings.get(&id))
            .filter_map(|binding| crate::task_mentions::valid_thread_path(&binding.path))
            .collect::<std::collections::HashSet<_>>();
        matches.retain(|task| {
            mentioned_ids.len() < crate::task_mentions::MAX_REFERENCED_TASKS
                && !mentioned_ids.contains(task.thread_id.as_str())
        });
        let Some(task_mentions) = &mut self.task_mentions else {
            return;
        };
        *task_mentions = matches;
        self.refresh_mentions_v2_popup_candidates();
    }

    /// Refreshes an open mention catalog when skill or plugin metadata changes.
    fn refresh_mentions_v2_popup_candidates(&mut self) {
        let ActivePopup::MentionV2(popup) = &mut self.popups.active else {
            return;
        };
        popup.set_candidates(super::mentions_v2::build_search_catalog(
            self.skills.as_deref(),
            self.plugins.as_deref(),
            self.task_mentions.as_deref().unwrap_or_default(),
        ));
    }

    pub fn set_plugins_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.plugins_command_enabled = enabled);
    }

    pub fn set_token_activity_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.token_activity_command_enabled = enabled
        });
    }

    pub fn set_mentions_v2_enabled(&mut self, enabled: bool) {
        self.mentions_v2_enabled = enabled;
        self.history.set_at_mention_restore_enabled(enabled);
        self.sync_popups();
    }

    /// Toggle composer-side image paste handling.
    ///
    /// This only affects whether image-like paste content is converted into attachments; the
    /// `ChatWidget` layer still performs capability checks before images are submitted.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.config.image_paste_enabled = enabled;
    }

    pub fn set_connector_mentions(&mut self, connectors_snapshot: Option<ConnectorsSnapshot>) {
        self.connectors_snapshot = connectors_snapshot;
        self.sync_popups();
    }

    pub(crate) fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        let elements = self.current_mention_elements();
        let mut ordered = Vec::new();
        for (id, sigil, mention) in elements {
            if let Some(binding) = self.draft.mention_bindings.remove(&id)
                && binding.sigil == sigil
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    sigil: binding.sigil,
                    mention: binding.mention,
                    path: binding.path,
                });
            }
        }
        self.draft.mention_bindings.clear();
        ordered
    }

    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.collaboration_modes_enabled = enabled
        });
    }

    pub fn set_connectors_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.connectors_enabled = enabled);
    }

    pub fn set_service_tier_commands_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.service_tier_commands_enabled = enabled
        });
    }

    pub fn set_service_tier_commands(&mut self, commands: Vec<ServiceTierCommand>) {
        self.update_slash_command_catalog(|composer| composer.service_tier_commands = commands);
    }

    pub fn set_goal_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.goal_command_enabled = enabled);
    }

    pub fn set_voice_command_enabled(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| composer.voice_command_enabled = enabled);
    }

    fn update_slash_command_catalog(&mut self, update: impl FnOnce(&mut Self)) {
        update(self);
        self.sync_popups();
    }

    /// Refresh the event destination when a retained editor survives reconnection.
    #[allow(dead_code, reason = "Used by later layers of the TUI refresh stack.")]
    pub(crate) fn set_app_event_sender(&mut self, app_event_tx: AppEventSender) {
        self.app_event_tx = app_event_tx;
    }

    /// Replace composer, editor, and footer-hint key bindings from one runtime snapshot.
    ///
    /// Submit and queue bindings are cached here because composer dispatch must
    /// check them before generic textarea editing. The embedded textarea receives
    /// the same snapshot's editor bindings so a live remap cannot leave submit
    /// keys updated while cursor/editing keys still use old defaults.
    pub(crate) fn set_keymap_bindings(&mut self, keymap: &RuntimeKeymap) {
        self.submit_keys = keymap.composer.submit.clone();
        self.queue_keys = keymap.composer.queue.clone();
        self.toggle_shortcuts_keys = keymap.composer.toggle_shortcuts.clone();
        self.history_search_previous_keys = keymap.composer.history_search_previous.clone();
        self.history_search_next_keys = keymap.composer.history_search_next.clone();
        self.editor_keymap = keymap.editor.clone();
        self.vim_normal_keymap = keymap.vim_normal.clone();
        self.draft.textarea.set_keymap_bindings(keymap);
        self.footer.external_editor_key =
            keymap.primary_hint(KeymapContext::Global, "open_external_editor");
        self.footer.show_warnings_key = keymap.primary_hint(KeymapContext::Global, "open_warnings");
        self.footer.show_transcript_key =
            keymap.primary_hint(KeymapContext::Global, "open_transcript");
        self.footer.find_transcript_key =
            keymap.primary_hint(KeymapContext::Global, "find_transcript");
        self.footer.focus_activity_key =
            keymap.primary_hint(KeymapContext::Global, "focus_activity");
        self.footer.insert_newline_key =
            match keymap.primary_hint(KeymapContext::Editor, "insert_newline") {
                hint @ Some(ShortcutHint::Chord { .. }) => hint,
                _ => footer_insert_newline_key(
                    &keymap.editor.insert_newline,
                    self.footer.use_shift_enter_hint,
                )
                .map(ShortcutHint::from),
            };
        self.footer.queue_key = keymap.primary_hint(KeymapContext::Composer, "queue");
        self.footer.toggle_shortcuts_key =
            keymap.primary_hint(KeymapContext::Composer, "toggle_shortcuts");
        self.footer.history_search_key =
            keymap.primary_hint(KeymapContext::Composer, "history_search_previous");
        self.footer.reasoning_down_key =
            keymap.primary_hint(KeymapContext::Chat, "decrease_reasoning_effort");
        self.footer.reasoning_up_key =
            keymap.primary_hint(KeymapContext::Chat, "increase_reasoning_effort");
        self.footer.toggle_voice_key = keymap.primary_hint(KeymapContext::Chat, "toggle_voice");
    }

    /// Return the contexts whose handlers can consume the next composer key.
    pub(crate) fn keymap_contexts(&self) -> KeymapContextSet {
        if self.draft.textarea.vim_query().is_some() {
            return KeymapContextSet::new(KeymapContext::Editor);
        }
        if self.history_search.is_some() {
            return KeymapContextSet::new(KeymapContext::Composer);
        }
        if !self.draft.input_enabled || self.popups.active() {
            return KeymapContextSet::default();
        }
        let contexts = self.draft.textarea.keymap_contexts();
        contexts.with(KeymapContext::Composer)
    }

    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.footer.collaboration_mode_indicator = indicator;
    }

    pub fn set_goal_status_indicator(&mut self, indicator: Option<GoalStatusIndicator>) {
        self.footer.goal_status_indicator = indicator;
    }

    pub fn set_ide_context_active(&mut self, active: bool) {
        self.footer.ide_context_active = active;
    }

    pub fn set_side_conversation_active(&mut self, active: bool) {
        self.update_slash_command_catalog(|composer| composer.side_conversation_active = active);
    }

    /// Compatibility shim for tests that still toggle the removed steer mode flag.
    #[cfg(test)]
    pub fn set_steer_enabled(&mut self, _enabled: bool) {}
    /// Centralized feature gating keeps config checks out of call sites.
    fn popups_enabled(&self) -> bool {
        self.config.popups_enabled
    }

    fn slash_commands_enabled(&self) -> bool {
        self.config.slash_commands_enabled
    }

    fn image_paste_enabled(&self) -> bool {
        self.config.image_paste_enabled
    }
    #[cfg(target_os = "windows")]
    pub fn set_windows_degraded_sandbox_active(&mut self, enabled: bool) {
        self.update_slash_command_catalog(|composer| {
            composer.windows_degraded_sandbox_active = enabled
        });
    }

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

    fn mode_indicator_line(&self, show_cycle_hint: bool) -> Option<Line<'static>> {
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

    fn right_footer_line_with_context(&self) -> Line<'static> {
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

    pub(super) fn set_text_content_with_shell_prefix(
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

    fn history_navigation_cursor(&self) -> usize {
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

    fn current_text_elements(&self) -> Vec<TextElement> {
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

    pub(super) fn snapshot_draft(&self) -> ComposerDraft {
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

    pub(super) fn restore_draft(&mut self, draft: ComposerDraft) {
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
    fn apply_history_entry(&mut self, entry: HistoryEntry) {
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

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
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
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
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
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }
        let elements_before = self.draft.textarea.element_payloads();
        self.draft.textarea.input(input);
        self.reconcile_deleted_elements(elements_before);
        (InputResult::None, true)
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

    fn is_image_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".gif")
            || lower.ends_with(".webp")
    }

    /// Leaves the cursor after one horizontal separator following a completion.
    ///
    /// Another separator is preserved before any non-whitespace suffix so subsequent typing does
    /// not merge into it. Line breaks are never reused as separators; a space is inserted before
    /// them so subsequent typing stays on the completed token's line.
    ///
    /// Returns the position of the space when completion inserted one.
    fn advance_past_completion_separator(&mut self) -> Option<usize> {
        let cursor = self.draft.textarea.cursor();
        let existing_separator_len = self.draft.textarea.text()[cursor..]
            .chars()
            .next()
            .filter(|c| {
                c.is_whitespace()
                    && !matches!(
                        *c,
                        '\n' | '\r'
                            | '\u{000B}'
                            | '\u{000C}'
                            | '\u{0085}'
                            | '\u{2028}'
                            | '\u{2029}'
                    )
            })
            .map(char::len_utf8);
        if let Some(separator_len) = existing_separator_len {
            let after_separator = cursor + separator_len;
            let separator_precedes_suffix = self.draft.textarea.text()[after_separator..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
            if !separator_precedes_suffix {
                self.draft.textarea.set_cursor(after_separator);
                return None;
            }
        }
        self.draft.textarea.insert_str_at(cursor, " ");
        Some(cursor)
    }

    /// Dismisses popup synchronization only for the exact token occurrence just inserted.
    ///
    /// Matching both range and text prevents an identical token later in the draft from inheriting
    /// the completed token's dismissal state.
    fn dismiss_completed_prefixed_token(
        &mut self,
        prefix: char,
        inserted_range: Range<usize>,
        inserted_text: &str,
    ) {
        let Some(completed_token) = inserted_text.strip_prefix(prefix) else {
            return;
        };
        // Completion leaves the cursor on separator whitespace, where normal token affinity would
        // otherwise immediately reopen the popup for sigil-prefixed inserted text.
        let Some((current_range, current_token)) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            /*allow_empty*/ true,
        ) else {
            return;
        };
        if current_range != inserted_range || current_token != completed_token {
            return;
        }

        if prefix == '@' && !self.mentions_v2_enabled {
            self.popups.dismissed_file_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        } else {
            self.popups.dismissed_mention_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        }
    }

    fn insert_selected_file_path(&mut self, token_range: Range<usize>, selected_path: &str) {
        let started_vim_edit = self.begin_direct_vim_edit();
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        if Self::is_image_path(selected_path) {
            let path_buf = PathBuf::from(selected_path);
            match image::image_dimensions(&path_buf) {
                Ok((width, height)) => {
                    tracing::debug!("selected image dimensions={}x{}", width, height);
                    let start_idx = token_range.start;
                    self.draft.textarea.replace_range(token_range, "");
                    self.draft.textarea.set_cursor(start_idx);
                    self.attach_image(path_buf);
                    self.advance_past_completion_separator();
                }
                Err(err) => {
                    tracing::trace!("image dimensions lookup failed: {err}");
                    self.insert_selected_path(token_range, selected_path);
                }
            }
        } else {
            self.insert_selected_path(token_range, selected_path);
        }
        if started_vim_edit {
            self.finish_vim_edit();
        }
    }

    fn trim_text_elements(
        original: &str,
        trimmed: &str,
        elements: Vec<TextElement>,
    ) -> Vec<TextElement> {
        if trimmed.is_empty() || elements.is_empty() {
            return Vec::new();
        }
        let trimmed_start = original.len().saturating_sub(original.trim_start().len());
        let trimmed_end = trimmed_start.saturating_add(trimmed.len());

        elements
            .into_iter()
            .filter_map(|elem| {
                let start = elem.byte_range.start;
                let end = elem.byte_range.end;
                if end <= trimmed_start || start >= trimmed_end {
                    return None;
                }
                let new_start = start.saturating_sub(trimmed_start);
                let new_end = end.saturating_sub(trimmed_start).min(trimmed.len());
                if new_start >= new_end {
                    return None;
                }
                let placeholder = trimmed.get(new_start..new_end).map(str::to_string);
                Some(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ))
            })
            .collect()
    }

    /// Expand large-paste placeholders using element ranges and rebuild other element spans.
    pub(crate) fn expand_pending_pastes(
        text: &str,
        mut elements: Vec<TextElement>,
        pending_pastes: &[(String, String)],
    ) -> (String, Vec<TextElement>) {
        if pending_pastes.is_empty() || elements.is_empty() {
            return (text.to_string(), elements);
        }

        // Stage 1: index pending paste payloads by placeholder for deterministic replacements.
        let mut pending_by_placeholder: HashMap<&str, VecDeque<&str>> = HashMap::new();
        for (placeholder, actual) in pending_pastes {
            pending_by_placeholder
                .entry(placeholder.as_str())
                .or_default()
                .push_back(actual.as_str());
        }

        // Stage 2: walk elements in order and rebuild text/spans in a single pass.
        elements.sort_by_key(|elem| elem.byte_range.start);

        let mut rebuilt = String::with_capacity(text.len());
        let mut rebuilt_elements = Vec::with_capacity(elements.len());
        let mut cursor = 0usize;

        for elem in elements {
            let start = elem.byte_range.start.min(text.len());
            let end = elem.byte_range.end.min(text.len());
            if start > end {
                continue;
            }
            if start > cursor {
                rebuilt.push_str(&text[cursor..start]);
            }
            let elem_text = &text[start..end];
            let placeholder = elem.placeholder(text).map(str::to_string);
            let replacement = placeholder
                .as_deref()
                .and_then(|ph| pending_by_placeholder.get_mut(ph))
                .and_then(VecDeque::pop_front);
            if let Some(actual) = replacement {
                // Stage 3: inline actual paste payloads and drop their placeholder elements.
                rebuilt.push_str(actual);
            } else {
                // Stage 4: keep non-paste elements, updating their byte ranges for the new text.
                let new_start = rebuilt.len();
                rebuilt.push_str(elem_text);
                let new_end = rebuilt.len();
                let placeholder = placeholder.or_else(|| Some(elem_text.to_string()));
                rebuilt_elements.push(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ));
            }
            cursor = end;
        }

        // Stage 5: append any trailing text that followed the last element.
        if cursor < text.len() {
            rebuilt.push_str(&text[cursor..]);
        }

        (rebuilt, rebuilt_elements)
    }

    pub fn skills(&self) -> Option<&Vec<SkillMetadata>> {
        self.skills.as_ref()
    }

    pub fn plugins(&self) -> Option<&Vec<PluginCapabilitySummary>> {
        self.plugins.as_ref()
    }

    fn mentions_enabled(&self) -> bool {
        let skills_ready = self
            .skills
            .as_ref()
            .is_some_and(|skills| !skills.is_empty());
        let plugins_ready = self
            .plugins
            .as_ref()
            .is_some_and(|plugins| !plugins.is_empty());
        let connectors_ready = self.connectors_enabled
            && self
                .connectors_snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.connectors.is_empty());
        skills_ready || plugins_ready || connectors_ready
    }

    fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<String> {
        completion_target::current_prefixed_token_range(textarea, prefix, allow_empty)
            .map(|(_, token)| token)
    }

    /// Extract the `@token` that the cursor is currently positioned on, if any.
    ///
    /// The returned string **does not** include the leading `@`.
    fn current_at_token(textarea: &TextArea) -> Option<String> {
        Self::current_prefixed_token(textarea, '@', /*allow_empty*/ false)
    }

    /// Returns the active prefixed token only when its sigil and name remain editable plaintext.
    ///
    /// Atomic elements and tokens whose mention prefix is already atomic are excluded so bound
    /// mentions are not offered for completion again.
    fn current_editable_prefixed_token_range(
        &self,
        prefix: char,
        allow_empty: bool,
    ) -> Option<(Range<usize>, String)> {
        let (range, token) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            allow_empty,
        )?;
        completion_target::prefixed_token_range_is_editable(
            &self.draft.textarea,
            prefix,
            &range,
            &token,
        )
        .then_some((range, token))
    }

    fn current_editable_at_token_range_with_options(
        &self,
        allow_empty: bool,
    ) -> Option<(Range<usize>, String)> {
        self.current_editable_prefixed_token_range('@', allow_empty)
    }

    fn current_mentions_v2_token_range(&self) -> Option<(Range<usize>, String)> {
        if !self.mentions_v2_enabled {
            return None;
        }
        self.current_editable_at_token_range_with_options(/*allow_empty*/ true)
    }

    fn current_mentions_v2_token(&self) -> Option<String> {
        self.current_mentions_v2_token_range()
            .map(|(_, token)| token)
    }

    /// Resolves the active legacy `$` target without eagerly cloning the mention catalog.
    ///
    /// Definite shell syntax is rejected. Ambiguous shell-like queries are accepted only when a
    /// bindable mention matches. The query-independent catalog built during that check is carried
    /// forward only when the selected target is also ambiguous.
    fn current_mention_target(&self) -> Option<MentionCompletionTarget> {
        if !self.mentions_enabled() {
            return None;
        }
        let mentions: OnceCell<Vec<MentionItem>> = OnceCell::new();
        let (range, query) = {
            let dollar_query_is_completable =
                |query: &str| match completion_target::dollar_query_kind(query) {
                    completion_target::DollarQueryKind::Completable => true,
                    completion_target::DollarQueryKind::AmbiguousShellParameter => mentions
                        .get_or_init(|| {
                            self.mention_items()
                                .into_iter()
                                .filter(|mention| {
                                    mention.path.is_some()
                                        && Self::mention_token_from_insert_text(
                                            &mention.insert_text,
                                        )
                                        .is_some()
                                })
                                .collect()
                        })
                        .iter()
                        .any(|mention| mention.fuzzy_match_query(query).is_some()),
                    completion_target::DollarQueryKind::ShellVariable
                    | completion_target::DollarQueryKind::DefiniteShellParameter
                    | completion_target::DollarQueryKind::Invalid => false,
                };
            let (range, query) =
                completion_target::current_prefixed_token_range_with_dollar_predicate(
                    &self.draft.textarea,
                    '$',
                    /*allow_empty*/ true,
                    dollar_query_is_completable,
                )?;
            if !completion_target::prefixed_token_range_is_editable(
                &self.draft.textarea,
                '$',
                &range,
                &query,
            ) || !dollar_query_is_completable(&query)
            {
                return None;
            }
            (range, query)
        };
        let prebuilt_mentions = if matches!(
            completion_target::dollar_query_kind(&query),
            completion_target::DollarQueryKind::AmbiguousShellParameter
        ) {
            mentions.into_inner()
        } else {
            None
        };

        Some(MentionCompletionTarget {
            range,
            query,
            prebuilt_mentions,
        })
    }

    /// Replace the active `@token` (the one under the cursor) with `path`.
    fn insert_selected_path(&mut self, token_range: Range<usize>, path: &str) {
        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid adding
        // quotes when the path already contains one to keep behavior simple.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace just the active `@token` so unrelated text elements, such as
        // large-paste placeholders, remain atomic and can still expand on submit.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, &inserted);
        let inserted_range = start_idx..start_idx.saturating_add(inserted.len());
        self.draft.textarea.set_cursor(inserted_range.end);
        self.advance_past_completion_separator();
        self.dismiss_completed_prefixed_token('@', inserted_range, &inserted);
    }

    fn insert_selected_mention(
        &mut self,
        token_range: Range<usize>,
        insert_text: &str,
        path: Option<&str>,
    ) {
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        // Remove the active token and insert the selected mention as an atomic element.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, "");
        self.draft.textarea.set_cursor(start_idx);
        let id = self.draft.textarea.insert_element(insert_text);
        let inserted_range = start_idx..start_idx.saturating_add(insert_text.len());

        let mention = if path
            .and_then(crate::task_mentions::valid_thread_path)
            .is_some()
        {
            insert_text
                .strip_prefix('@')
                .filter(|name| !name.is_empty())
                .map(|name| ('@', name.to_string()))
        } else {
            Self::mention_token_from_insert_text(insert_text)
        };
        if let (Some(path), Some((sigil, mention))) = (path, mention) {
            self.draft.mention_bindings.insert(
                id,
                ComposerMentionBinding {
                    sigil,
                    mention,
                    path: path.to_string(),
                },
            );
        }
        if path
            .and_then(crate::task_mentions::valid_thread_path)
            .is_some()
            && let Some(task_mentions) = &mut self.task_mentions
        {
            task_mentions.clear();
        }

        let inserted_space = self.advance_past_completion_separator();
        let is_skill = path.is_some_and(|path| {
            self.skills
                .as_deref()
                .is_some_and(|skills| skills.iter().any(|skill| skill.path.as_str() == path))
        });
        self.draft.skill_completion_space = inserted_space.filter(|_| is_skill);
        if let Some(sigil) = insert_text.chars().next()
            && matches!(sigil, '$' | '@')
        {
            self.dismiss_completed_prefixed_token(sigil, inserted_range, insert_text);
        }
    }

    /// Inserts a leading separator when a completion starts directly after an atomic element.
    ///
    /// The returned range is shifted to keep replacing the same editable token after insertion.
    fn separate_completion_from_adjacent_element(
        &mut self,
        mut token_range: Range<usize>,
    ) -> Range<usize> {
        let starts_after_element = self
            .draft
            .textarea
            .text_element_ranges()
            .any(|range| range.end == token_range.start);
        if starts_after_element {
            self.draft
                .textarea
                .replace_range(token_range.start..token_range.start, " ");
            token_range.start += 1;
            token_range.end += 1;
        }
        token_range
    }

    fn mention_token_from_insert_text(insert_text: &str) -> Option<(char, String)> {
        let sigil = insert_text.chars().next()?;
        if !matches!(sigil, '$' | '@') {
            return None;
        }
        let name = &insert_text[sigil.len_utf8()..];
        if name.is_empty() {
            return None;
        }
        if name
            .as_bytes()
            .iter()
            .all(|byte| is_mention_name_char(*byte))
        {
            Some((sigil, name.to_string()))
        } else {
            None
        }
    }

    fn current_mention_elements(&self) -> Vec<(u64, char, String)> {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                if let Some(binding) = self.draft.mention_bindings.get(&snapshot.id)
                    && crate::task_mentions::valid_thread_path(&binding.path).is_some()
                    && snapshot.text == format!("{}{}", binding.sigil, binding.mention)
                {
                    return Some((snapshot.id, binding.sigil, binding.mention.clone()));
                }
                Self::mention_token_from_insert_text(snapshot.text.as_str())
                    .map(|(sigil, mention)| (snapshot.id, sigil, mention))
            })
            .collect()
    }

    fn snapshot_mention_bindings(&self) -> Vec<MentionBinding> {
        self.snapshot_mention_bindings_with_ranges().0
    }

    fn snapshot_mention_bindings_with_ranges(&self) -> (Vec<MentionBinding>, Vec<Range<usize>>) {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|element| {
                let binding = self.draft.mention_bindings.get(&element.id)?;
                if element.text != format!("{}{}", binding.sigil, binding.mention)
                    || (crate::task_mentions::valid_thread_path(&binding.path).is_none()
                        && Self::mention_token_from_insert_text(&element.text).is_none())
                {
                    return None;
                }
                Some((
                    MentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention.clone(),
                        path: binding.path.clone(),
                    },
                    element.range,
                ))
            })
            .unzip()
    }

    fn bind_mentions_from_snapshot(&mut self, mention_bindings: Vec<MentionBinding>) {
        self.draft.mention_bindings.clear();
        if mention_bindings.is_empty() {
            return;
        }

        let text = self.draft.textarea.text().to_string();
        let mut scan_from = 0usize;
        for binding in mention_bindings {
            let token = format!("{}{}", binding.sigil, binding.mention);
            let range = if crate::task_mentions::valid_thread_path(&binding.path).is_some() {
                self.draft
                    .textarea
                    .text_element_snapshots()
                    .into_iter()
                    .find(|element| element.range.start >= scan_from && element.text == token)
                    .map(|element| element.range)
            } else {
                find_next_mention_token_range(text.as_str(), token.as_str(), scan_from)
            };
            let Some(range) = range else {
                continue;
            };

            let id = if let Some(id) = self.draft.textarea.add_element_range(range.clone()) {
                Some(id)
            } else {
                self.draft
                    .textarea
                    .element_id_for_exact_range(range.clone())
            };

            if let Some(id) = id {
                self.draft.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
                scan_from = range.end;
            }
        }
    }

    fn plugin_at_mention_highlights(&self) -> Vec<(Range<usize>, Style)> {
        self.draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                let binding = self.draft.mention_bindings.get(&snapshot.id)?;
                if !binding.path.starts_with("plugin://") || !snapshot.text.starts_with('@') {
                    return None;
                }
                Some((snapshot.range, Style::default().fg(Color::Magenta)))
            })
            .collect()
    }

    /// Prepare text for submission/queuing. Returns None if submission should be suppressed.
    /// On success, clears pending paste payloads because placeholders have been expanded.
    ///
    /// When `record_history` is true, the final submission is stored for ↑/↓ recall.
    fn prepare_submission_text(
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
    fn handle_submission(&mut self, should_queue: bool) -> (InputResult, bool) {
        let result =
            self.handle_submission_with_time(should_queue, tokio::time::Instant::now().into_std());
        self.reset_vim_mode_after_successful_dispatch(&result.0);
        result
    }

    /// Prepare a startup-confirmed draft through the normal queue parser, independent of keymaps.
    pub(super) fn prepare_startup_submission(&mut self) -> InputResult {
        // Startup's plain-text editor cannot show shell mode; require confirmation here.
        if self.is_bang_shell_command() {
            return InputResult::None;
        }
        self.handle_submission(/*should_queue*/ true).0
    }

    fn reset_vim_mode_after_successful_dispatch(&mut self, result: &InputResult) {
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

    fn handle_submission_with_time(
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
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
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
    fn try_dispatch_bare_slash_command(&mut self) -> Option<InputResult> {
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
    fn try_dispatch_slash_command_with_args(&mut self) -> Option<InputResult> {
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
    fn stage_selected_slash_command_history(&mut self, command: &CommandItem) {
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
    fn handle_key_event_without_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
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
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
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

    fn is_bang_shell_command(&self) -> bool {
        self.config.shell_commands_enabled && self.draft.is_bash_mode
    }

    fn shell_mode_footer_line(&self) -> Option<Line<'static>> {
        self.is_bang_shell_command()
            .then_some(())
            .map(|_| Line::from(vec![Span::from("Shell mode").light_red()]))
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
    fn handle_input_basic(&mut self, input: KeyEvent) -> (InputResult, bool) {
        // Ignore key releases here to avoid treating them as additional input
        // (e.g., appending the same character twice via paste-burst logic).
        if !matches!(input.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return (InputResult::None, false);
        }

        self.handle_input_basic_with_time(input, tokio::time::Instant::now().into_std())
    }

    fn handle_input_basic_with_time(
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
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
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

    fn sync_bash_mode_from_text(&mut self) {
        if self.config.shell_commands_enabled
            && !self.draft.is_bash_mode
            && self.draft.textarea.text().starts_with('!')
        {
            self.draft.textarea.absorb_prompt_prefix(/*prefix_len*/ 1);
            self.draft.is_bash_mode = true;
        }
    }

    fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
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
    fn handle_empty_prompt_shortcut(&mut self, key_event: &KeyEvent) -> bool {
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

    fn empty_prompt_shortcut_toggle(&self, key_event: &KeyEvent) -> Option<FooterMode> {
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

    pub(crate) fn sync_popups(&mut self) {
        self.sync_slash_command_elements();
        if self.history_search.is_some() || self.draft.textarea.vim_query().is_some() {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;
            self.popups.dismissed_file_token = None;
            self.popups.dismissed_mention_token = None;
            return;
        }
        if !self.popups_enabled() || self.draft.textarea.mouse_selection_range().is_some() {
            self.popups.active = ActivePopup::None;
            return;
        }
        let mut mentions_v2_token = self.current_mentions_v2_token_range();
        let mut file_token = if self.mentions_v2_enabled {
            None
        } else {
            self.current_editable_at_token_range_with_options(/*allow_empty*/ false)
        };
        let browsing_history = self
            .history
            .should_handle_navigation(&self.current_text(), self.history_navigation_cursor());
        // When browsing input history (shell-style Up/Down recall), skip all popup
        // synchronization so nothing steals focus from continued history navigation.
        if browsing_history {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;
            return;
        }
        let mut mention_target = self.current_mention_target();
        let at_token_start = mentions_v2_token
            .as_ref()
            .or(file_token.as_ref())
            .map(|(range, _)| range.start);
        let mention_token_start = mention_target.as_ref().map(|target| target.range.start);
        if let (Some(at_token_start), Some(mention_token_start)) =
            (at_token_start, mention_token_start)
        {
            if at_token_start > mention_token_start {
                mention_target = None;
            } else if mention_token_start > at_token_start {
                mentions_v2_token = None;
                file_token = None;
            }
        }

        let allow_command_popup = self.slash_commands_enabled()
            && !self.draft.is_bash_mode
            && file_token.is_none()
            && mentions_v2_token.is_none()
            && mention_target.is_none();
        self.sync_command_popup(allow_command_popup);

        if matches!(self.popups.active, ActivePopup::Command(_)) {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.popups.dismissed_file_token = None;
            self.popups.dismissed_mention_token = None;
            return;
        }

        if let Some((range, token)) = mentions_v2_token {
            self.sync_mentions_v2_popup(range, token);
            return;
        }

        if let Some(target) = mention_target {
            if self.popups.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.popups.current_file_query = None;
            }
            self.sync_mention_popup(target);
            return;
        }
        self.popups.dismissed_mention_token = None;

        if let Some((range, token)) = file_token {
            self.sync_file_search_popup(range, token);
            return;
        }

        if self.popups.current_file_query.is_some() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        }
        self.popups.dismissed_file_token = None;
        if matches!(
            self.popups.active,
            ActivePopup::File(_) | ActivePopup::Skill(_) | ActivePopup::MentionV2(_)
        ) {
            self.popups.active = ActivePopup::None;
        }
    }

    /// Synchronize `self.command_popup` with the current text in the
    /// textarea. This must be called after every modification that can change
    /// the text so the popup is shown/updated/hidden as appropriate.
    fn sync_command_popup(&mut self, allow: bool) {
        let text = self.draft.textarea.text();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        // Keep an explicitly dismissed popup closed until the command token changes.
        let command_token = slash_input::command_popup_filter_text(first_line, /*cursor*/ 0);
        if let Some(command_token) = command_token.as_deref()
            && self.popups.dismissed_command_token.as_deref() == Some(command_token)
        {
            return;
        }
        self.popups.dismissed_command_token = None;

        if !allow {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        // Determine whether the caret is inside the initial '/name' token on the first line.
        let cursor = self.draft.textarea.cursor();
        let caret_on_first_line = cursor <= first_line_end;

        let is_editing_slash_command_name = caret_on_first_line
            && self
                .slash_input()
                .is_editing_command_name(first_line, cursor);
        let command_filter_text = caret_on_first_line
            .then(|| slash_input::command_popup_filter_text(first_line, cursor))
            .flatten();

        // If the cursor is currently positioned within an `@token`, prefer the
        // file-search popup over the slash popup so users can insert a file path
        // as an argument to the command (e.g., "/review @docs/...").
        if Self::current_at_token(&self.draft.textarea).is_some() {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        match &mut self.popups.active {
            ActivePopup::Command(popup) => {
                if is_editing_slash_command_name {
                    if let Some(command_filter_text) = command_filter_text.as_deref() {
                        popup.on_composer_text_change(command_filter_text.to_string());
                    }
                } else {
                    self.popups.active = ActivePopup::None;
                }
            }
            _ => {
                if is_editing_slash_command_name
                    && let Some(command_filter_text) = command_filter_text.as_deref()
                {
                    let command_popup = self.slash_input().command_popup(command_filter_text);
                    self.popups.active = ActivePopup::Command(command_popup);
                }
            }
        }
    }

    /// Synchronize the legacy file-search popup with the current `@` token.
    fn sync_file_search_popup(&mut self, range: Range<usize>, query: String) {
        if self
            .popups
            .dismissed_file_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        if query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
        } else {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(query.clone()));
        }

        match &mut self.popups.active {
            ActivePopup::File(popup) => {
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
            }
            _ => {
                let mut popup = FileSearchPopup::new();
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
                self.popups.active = ActivePopup::File(popup);
            }
        }

        if query.is_empty() {
            self.popups.current_file_query = None;
        } else {
            self.popups.current_file_query = Some(query);
        }
        self.popups.dismissed_file_token = None;
    }

    fn sync_mention_popup(&mut self, target: MentionCompletionTarget) {
        let MentionCompletionTarget {
            range,
            query,
            prebuilt_mentions,
        } = target;
        if self
            .popups
            .dismissed_mention_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        let mentions = prebuilt_mentions.unwrap_or_else(|| self.mention_items());
        if mentions.is_empty() {
            self.popups.active = ActivePopup::None;
            return;
        }

        match &mut self.popups.active {
            ActivePopup::Skill(popup) => {
                popup.set_query(&query);
                popup.set_mentions(mentions);
            }
            _ => {
                let mut popup =
                    SkillPopup::new(mentions).with_frequencies(self.mention_usage.frequencies());
                popup.set_query(&query);
                self.popups.active = ActivePopup::Skill(popup);
            }
        }
    }

    fn sync_mentions_v2_popup(&mut self, range: Range<usize>, query: String) {
        if self
            .popups
            .dismissed_mention_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        if query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.popups.current_file_query = None;
        } else {
            let new_popup = !matches!(self.popups.active, ActivePopup::MentionV2(_));
            if new_popup {
                // A fresh popup has no cached matches, and the app-owned file-search manager can
                // retain an identical query. Reset it before issuing the query so results arrive.
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
            }
            if new_popup || self.popups.current_file_query.as_deref() != Some(query.as_str()) {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(query.clone()));
                self.popups.current_file_query = Some(query.clone());
            }
        }

        match &mut self.popups.active {
            ActivePopup::MentionV2(popup) => {
                popup.set_query(&query);
            }
            _ => {
                let candidates = super::mentions_v2::build_search_catalog(
                    self.skills.as_deref(),
                    self.plugins.as_deref(),
                    self.task_mentions.as_deref().unwrap_or_default(),
                );
                self.popups.active =
                    ActivePopup::MentionV2(MentionV2Popup::new(candidates, &query));
            }
        }

        self.popups.dismissed_mention_token = None;
    }

    fn mention_items(&self) -> Vec<MentionItem> {
        let mut mentions = Vec::new();
        if let Some(skills) = self.skills.as_ref() {
            for skill in skills {
                let display_name = skill_display_name(skill);
                let description = skill_description(skill);
                let skill_name = skill.name.clone();
                let search_terms = if display_name == skill.name {
                    vec![skill_name.clone()]
                } else {
                    vec![skill_name.clone(), display_name.clone()]
                };
                mentions.push(MentionItem {
                    display_name,
                    description,
                    insert_text: format!("${skill_name}"),
                    search_terms,
                    path: Some(skill.path.as_str().to_owned()),
                    category_tag: Some("[Skill]".to_string()),
                    sort_rank: 1,
                });
            }
        }

        if !self.mentions_v2_enabled
            && let Some(plugins) = self.plugins.as_ref()
        {
            for plugin in plugins {
                let (plugin_name, marketplace_name) = plugin
                    .config_name
                    .split_once('@')
                    .unwrap_or((plugin.config_name.as_str(), ""));
                let mut capability_labels = Vec::new();
                if plugin.has_skills {
                    capability_labels.push("skills".to_string());
                }
                if !plugin.mcp_server_names.is_empty() {
                    let mcp_server_count = plugin.mcp_server_names.len();
                    capability_labels.push(if mcp_server_count == 1 {
                        "1 MCP server".to_string()
                    } else {
                        format!("{mcp_server_count} MCP servers")
                    });
                }
                if !plugin.app_connector_ids.is_empty() {
                    let app_count = plugin.app_connector_ids.len();
                    capability_labels.push(if app_count == 1 {
                        "1 app".to_string()
                    } else {
                        format!("{app_count} apps")
                    });
                }
                let description = plugin.description.clone().or_else(|| {
                    Some(if capability_labels.is_empty() {
                        "Plugin".to_string()
                    } else {
                        format!("Plugin · {}", capability_labels.join(" · "))
                    })
                });
                let mut search_terms = vec![plugin_name.to_string(), plugin.config_name.clone()];
                if plugin.display_name != plugin_name {
                    search_terms.push(plugin.display_name.clone());
                }
                if !marketplace_name.is_empty() {
                    search_terms.push(marketplace_name.to_string());
                }
                mentions.push(MentionItem {
                    display_name: plugin.display_name.clone(),
                    description,
                    insert_text: format!("${plugin_name}"),
                    search_terms,
                    path: Some(format!("plugin://{}", plugin.config_name)),
                    category_tag: Some("[Plugin]".to_string()),
                    sort_rank: 0,
                });
            }
        }

        if self.connectors_enabled
            && let Some(snapshot) = self.connectors_snapshot.as_ref()
        {
            for connector in &snapshot.connectors {
                if !connector.is_accessible || !connector.is_enabled {
                    continue;
                }
                let display_name = codex_connectors::metadata::connector_display_label(connector);
                let description = Some(Self::connector_brief_description(connector));
                let slug = codex_connectors::metadata::connector_mention_slug(connector);
                let search_terms = vec![display_name.clone(), connector.id.clone(), slug.clone()];
                let connector_id = connector.id.as_str();
                mentions.push(MentionItem {
                    display_name: display_name.clone(),
                    description,
                    insert_text: format!("${slug}"),
                    search_terms,
                    path: Some(format!("app://{connector_id}")),
                    category_tag: Some("[App]".to_string()),
                    sort_rank: 1,
                });
            }
        }

        mentions
    }

    fn connector_brief_description(connector: &AppInfo) -> String {
        Self::connector_description(connector).unwrap_or_default()
    }

    fn connector_description(connector: &AppInfo) -> Option<String> {
        connector
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(str::to_string)
    }

    fn set_has_focus(&mut self, has_focus: bool) {
        self.has_focus = has_focus;
    }

    #[allow(dead_code)]
    pub(crate) fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.draft.input_enabled = enabled;
        self.draft.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && self.popups.active() {
            self.popups.active = ActivePopup::None;
        }
    }

    pub(crate) fn show_shutdown_in_progress(&mut self) {
        self.set_input_enabled(/*enabled*/ false, Some("Shutting down...".to_string()));
        self.footer.quit_shortcut_expires_at = None;
        self.footer.mode = FooterMode::ComposerEmpty;
        self.footer.hint_override = Some(Vec::new());
        self.footer.flash = None;
    }

    pub fn set_task_running(&mut self, running: bool) {
        self.is_task_running = running;
    }

    pub(crate) fn set_queue_submissions(&mut self, queue_submissions: bool) {
        self.queue_submissions = queue_submissions;
    }

    pub(crate) fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.footer.context_window_percent == percent
            && self.footer.context_window_used_tokens == used_tokens
        {
            return;
        }
        self.footer.context_window_percent = percent;
        self.footer.context_window_used_tokens = used_tokens;
    }

    pub(crate) fn set_context_window_pending(&mut self, pending: bool) {
        self.footer.context_window_pending = pending;
    }

    pub(crate) fn set_esc_backtrack_hint(&mut self, show: bool) {
        self.footer.esc_backtrack_hint = show;
        if show {
            self.footer.mode = esc_hint_mode(self.footer.mode, self.is_task_running);
        } else {
            self.footer.mode = reset_mode_after_activity(self.footer.mode);
        }
    }

    /// Replaces the contextual footer label for the currently viewed agent.
    ///
    /// Returning `false` means the value was unchanged, so callers can skip redraw work. This
    /// field is intentionally just cached presentation state; `ChatComposer` does not infer which
    /// thread is active on its own.
    pub(crate) fn set_active_agent_label(&mut self, active_agent_label: Option<String>) -> bool {
        if self.footer.active_agent_label == active_agent_label {
            return false;
        }
        self.footer.active_agent_label = active_agent_label;
        true
    }
}

fn footer_insert_newline_key(
    bindings: &[KeyBinding],
    enhanced_keys_supported: bool,
) -> Option<KeyBinding> {
    let bindings = user_bindings(bindings);
    let shift_enter = key_hint::shift(KeyCode::Enter);
    if enhanced_keys_supported && bindings.contains(&shift_enter) {
        return Some(shift_enter);
    }

    let plain_enter = key_hint::plain(KeyCode::Enter);
    bindings
        .iter()
        .copied()
        .find(|binding| *binding != plain_enter)
        .or_else(|| bindings.first().copied())
}

fn skill_description(skill: &SkillMetadata) -> Option<String> {
    let description = skill
        .interface
        .as_ref()
        .and_then(|interface| interface.short_description.as_deref())
        .or(skill.short_description.as_deref())
        .unwrap_or(&skill.description);
    let trimmed = description.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

fn ends_plaintext_at_mention(bytes: &[u8], index: usize) -> bool {
    bytes.get(index).is_none_or(|byte| {
        byte.is_ascii_whitespace()
            || *byte == b'.'
                && bytes.get(index + 1).is_none_or(|next| {
                    next.is_ascii_whitespace()
                        || !next.is_ascii_alphanumeric() && *next != b'_' && *next != b'-'
                })
            || !matches!(*byte, b'.' | b'/' | b'\\')
                && !byte.is_ascii_alphanumeric()
                && *byte != b'_'
                && *byte != b'-'
    })
}

fn ends_plaintext_at_dollar_mention(bytes: &[u8], index: usize) -> bool {
    bytes
        .get(index)
        .is_none_or(|byte| !is_mention_name_char(*byte))
}

fn starts_plaintext_at_mention(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }

    text.get(..index)
        .and_then(|prefix| prefix.chars().next_back())
        .is_some_and(|ch| ch.is_whitespace() || !is_mention_name_char_char(ch))
}

fn is_mention_name_char_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn find_next_mention_token_range(text: &str, token: &str, from: usize) -> Option<Range<usize>> {
    if token.is_empty() || from >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let token_bytes = token.as_bytes();
    let sigil = *token_bytes.first()?;
    let mut index = from;

    while index < bytes.len() {
        if bytes[index] != sigil {
            index += 1;
            continue;
        }

        let end = index.saturating_add(token_bytes.len());
        if end > bytes.len() {
            return None;
        }
        if &bytes[index..end] != token_bytes {
            index += 1;
            continue;
        }

        // Fix for restored `@` mentions: rebinding must not attach to embedded substrings such
        // as email addresses, while preserving the existing `$` mention matching behavior.
        let starts_plaintext_mention = if sigil == b'@' {
            starts_plaintext_at_mention(text, index)
        } else {
            true
        };
        // Fix for restored `@` mentions: mirror history encoding's trailing boundary so path-like
        // text such as `@sample/pkg` is not rebound as the plain `@sample` mention.
        let ends_plaintext_mention = if sigil == b'@' {
            ends_plaintext_at_mention(bytes, end)
        } else {
            ends_plaintext_at_dollar_mention(bytes, end)
        };

        if starts_plaintext_mention && ends_plaintext_mention {
            return Some(index..end);
        }

        index = end;
    }

    None
}

impl Renderable for ChatComposer {
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.cursor_pos_with_options(area, ComposerRenderOptions::default())
    }

    fn cursor_style(&self, _area: Rect) -> crossterm::cursor::SetCursorStyle {
        if self.draft.textarea.uses_vim_insert_cursor() {
            crossterm::cursor::SetCursorStyle::SteadyBar
        } else {
            crossterm::cursor::SetCursorStyle::DefaultUserShape
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.desired_height_with_options(width, ComposerRenderOptions::default())
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_mask(area, buf, /*mask_char*/ None);
    }
}

impl ChatComposer {
    pub(crate) fn set_luna_reserve_active(&mut self, active: bool) -> bool {
        if self.luna_reserve_active == active {
            return false;
        }
        self.luna_reserve_active = active;
        true
    }
}

impl ChatComposer {
    pub(crate) fn render_with_mask(&self, area: Rect, buf: &mut Buffer, mask_char: Option<char>) {
        self.render_with_options(area, buf, mask_char, ComposerRenderOptions::default());
    }

    pub(crate) fn render_with_options(
        &self,
        area: Rect,
        buf: &mut Buffer,
        mask_char: Option<char>,
        options: ComposerRenderOptions<'_>,
    ) {
        let ComposerLayout {
            status,
            composer: composer_rect,
            remote_images: remote_images_rect,
            thumbnails,
            textarea: textarea_rect,
            popup: popup_rect,
            footer: footer_rect,
        } = self.layout_with_options(area, options);
        self.render_status_surface(status, buf, options);
        if self.popups.active.is_above_composer()
            && options.command_popup_placement != CommandPopupPlacement::Hidden
        {
            let popup_rect = if options.command_popup_placement == CommandPopupPlacement::Overlay {
                let rows_above_composer = composer_rect
                    .y
                    .saturating_sub(options.overlay_top.max(buf.area.y));
                let height = self
                    .popups
                    .active
                    .required_height(composer_rect.width, /*footer_total_height*/ 0)
                    .min(rows_above_composer);
                // Command and unified mention menus can render in a single row.
                let height = if matches!(
                    self.popups.active,
                    ActivePopup::Command(_) | ActivePopup::MentionV2(_)
                ) || height > 2
                {
                    height
                } else {
                    0
                };
                Rect::new(
                    composer_rect.x,
                    composer_rect.y - height,
                    composer_rect.width,
                    height,
                )
            } else {
                popup_rect
            };
            // Overlay rows must erase both glyphs and styles from the transcript underneath.
            ratatui::widgets::Clear.render(popup_rect, buf);
            match &self.popups.active {
                ActivePopup::Command(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::MentionV2(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::File(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::Skill(popup) => popup.render_ref(popup_rect, buf),
                ActivePopup::None => {
                    unreachable!("only suggestion menus use above-composer placement")
                }
            }
        }
        let transcript_hint_area = inset_footer_hint_area(footer_rect);
        let warning_notice = self
            .warning_notice_layout(transcript_hint_area, options)
            .filter(|_| !options.unfocused);
        let warning_area = warning_notice.as_ref().map(|(area, _)| *area);
        match options.footer {
            _ if options.unfocused => {}
            Some(footer) => {
                self.render_transcript_footer(transcript_hint_area, buf, footer, warning_area);
            }
            None if self.shortcuts_above_composer(options) => {
                let props = self.hint_footer_props(options);
                super::shortcut_overlay::render(&props, inset_footer_hint_area(popup_rect), buf);
                super::shortcut_overlay::close_hint(&props, transcript_hint_area.width)
                    .render(transcript_hint_area, buf);
            }
            None => {
                let hint_rect = warning_notice::before_warning(footer_rect, warning_area);
                if let Some(input) = self.draft.textarea.vim_query() {
                    input.render(inset_footer_hint_area(hint_rect), buf);
                } else if let Some(line) = self.history_search_footer_line() {
                    render_footer_line(hint_rect, buf, line);
                } else if self.shows_shared_status(options) {
                    self.render_status_surface(hint_rect, buf, options);
                } else {
                    let footer_props = self.hint_footer_props(options);
                    let show_cycle_hint = !self.has_separate_status(options)
                        && !footer_props.is_task_running
                        && self.footer.collaboration_mode_indicator.is_some();
                    let show_shortcuts_hint =
                        footer_props.mode == FooterMode::ComposerEmpty && !self.is_in_paste_burst();
                    let show_queue_hint = footer_props.mode == FooterMode::ComposerHasDraft
                        && footer_props.is_task_running;
                    let status_line_active = uses_passive_footer_status_layout(&footer_props);
                    let left_mode_indicator =
                        if status_line_active || self.has_separate_status(options) {
                            None
                        } else {
                            self.footer.collaboration_mode_indicator
                        };
                    let active_footer_hint_override = self.footer.hint_override.as_ref();
                    let left_width = if self.footer.flash_visible() {
                        self.footer
                            .flash
                            .as_ref()
                            .map(|flash| flash.line.width() as u16)
                            .unwrap_or(0)
                    } else if let Some(items) = active_footer_hint_override {
                        footer_hint_items_width(items)
                    } else {
                        footer_line_width(
                            &footer_props,
                            left_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        )
                    };
                    let right_line =
                        if let Some(label) = self.footer.side_conversation_context_label.as_ref() {
                            Some(side_conversation_context_line(label))
                        } else if let Some(line) = self.shell_mode_footer_line() {
                            Some(line)
                        } else if self.has_separate_status(options) {
                            None
                        } else if status_line_active {
                            let full = self.mode_indicator_line(show_cycle_hint);
                            let compact = self.mode_indicator_line(/*show_cycle_hint*/ false);
                            let full_width = full.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                            if can_show_left_with_context(hint_rect, left_width, full_width) {
                                full
                            } else {
                                compact
                            }
                        } else {
                            Some(self.right_footer_line_with_context())
                        };
                    let right_width = right_line.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                    let can_show_left_and_context =
                        can_show_left_with_context(hint_rect, left_width, right_width);
                    let has_override =
                        self.footer.flash_visible() || active_footer_hint_override.is_some();
                    let single_line_layout = if has_override || status_line_active {
                        None
                    } else {
                        match footer_props.mode {
                            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                                // Both of these modes render the single-line footer style (with
                                // either the shortcuts hint or the optional queue hint). We still
                                // want the single-line collapse rules so the mode label can win over
                                // the context indicator on narrow widths.
                                Some(single_line_footer_layout(
                                    hint_rect,
                                    right_width,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                    footer_props.key_hints,
                                ))
                            }
                            FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay => None,
                        }
                    };
                    let show_right = if matches!(
                        footer_props.mode,
                        FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay
                    ) {
                        false
                    } else {
                        single_line_layout
                            .as_ref()
                            .map(|(_, show_context)| *show_context)
                            .unwrap_or(can_show_left_and_context)
                    };

                    if let Some((summary_left, _)) = single_line_layout {
                        match summary_left {
                            SummaryLeft::Default => {
                                render_footer_from_props(
                                    hint_rect,
                                    buf,
                                    &footer_props,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                );
                            }
                            SummaryLeft::Custom(line) => {
                                render_footer_line(hint_rect, buf, line);
                            }
                            SummaryLeft::None => {}
                        }
                    } else if self.footer.flash_visible() {
                        if let Some(flash) = self.footer.flash.as_ref() {
                            Widget::render(&flash.line, inset_footer_hint_area(hint_rect), buf);
                        }
                    } else if let Some(items) = active_footer_hint_override {
                        render_footer_hint_items(hint_rect, buf, items);
                    } else {
                        render_footer_from_props(
                            hint_rect,
                            buf,
                            &footer_props,
                            self.footer.collaboration_mode_indicator,
                            show_cycle_hint,
                            show_shortcuts_hint,
                            show_queue_hint,
                        );
                    }
                    if show_right && let Some(line) = &right_line {
                        render_context_right(hint_rect, buf, line);
                    }
                }
            }
        }
        self.footer.warning_notice_area.set(warning_area);
        if let Some((warning_area, line)) = warning_notice {
            line.render(warning_area, buf);
        }
        if let Some(selection) = options.rendered_selection
            && !self.popup_active()
            && options.footer.is_none_or(|footer| !footer.is_interactive)
        {
            let revision = (options.warning_count, self.footer_props().is_task_running);
            let changed = self.footer.selection_revision.replace(revision) != revision;
            let mut selection = selection.borrow_mut();
            selection.register(status.intersection(buf.area), changed);
            selection.register(footer_rect.intersection(buf.area), changed);
        }
        let style = user_message_style();
        Block::default().style(style).render(composer_rect, buf);
        if let Some(previews) = options.image_previews {
            previews.render(
                thumbnails,
                buf,
                &self.attachments.local_images,
                &self.draft.textarea,
            );
        }
        if !remote_images_rect.is_empty() {
            let first = self
                .attachments
                .selected_remote_image_index
                .unwrap_or_default()
                .saturating_sub(usize::from(remote_images_rect.height) - 1);
            Paragraph::new(
                self.attachments
                    .remote_image_lines()
                    .into_iter()
                    .skip(first)
                    .collect::<Vec<_>>(),
            )
            .style(style)
            .render(remote_images_rect, buf);
        }
        if !textarea_rect.is_empty() {
            let prompt = if self.draft.input_enabled {
                if self.draft.is_bash_mode {
                    Span::from("!").light_red().bold()
                } else if self.luna_reserve_active {
                    // Reserve keeps one arrow at every reasoning effort; only its foreground changes.
                    "›"
                        .fg(crate::terminal_palette::best_color((246, 197, 67)))
                        .bold()
                } else if let Some(tier) = self.effort_tier {
                    let charge = self
                        .effort_ignition
                        .as_ref()
                        .map(EffortIgnition::charge_alpha)
                        .unwrap_or(1.0);
                    tier.prompt(charge)
                } else {
                    "›".bold()
                }
            } else {
                "›".dim()
            };
            buf.set_span(
                textarea_rect.x - LIVE_PREFIX_COLS,
                textarea_rect.y,
                &prompt,
                textarea_rect.width,
            );
        }

        let mut state = self.draft.textarea_state.borrow_mut();
        if options.max_height.is_none() {
            state.follow_cursor();
        }
        let textarea_is_empty = self.draft.textarea.text().is_empty() && !self.draft.is_bash_mode;
        if self.draft.input_enabled {
            if let Some(mask_char) = mask_char {
                self.draft
                    .textarea
                    .render_ref_masked(textarea_rect, buf, &mut state, mask_char);
            } else {
                let mut highlights = self.plugin_at_mention_highlights();
                let search_highlight_style =
                    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD);
                highlights.extend(
                    self.history_search_highlight_ranges()
                        .into_iter()
                        .chain(self.draft.textarea.vim_search_highlights())
                        .map(|range| (range, search_highlight_style)),
                );
                if highlights.is_empty() {
                    StatefulWidgetRef::render_ref(
                        &(&self.draft.textarea),
                        textarea_rect,
                        buf,
                        &mut state,
                    );
                } else {
                    self.draft.textarea.render_ref_styled_with_highlights(
                        textarea_rect,
                        buf,
                        &mut state,
                        Style::default(),
                        &highlights,
                    );
                }
            }
        }
        if !options.unfocused && (!self.draft.input_enabled || textarea_is_empty) {
            let text = if self.draft.input_enabled {
                self.placeholder_text.as_str().to_string()
            } else {
                self.draft
                    .input_disabled_placeholder
                    .as_deref()
                    .unwrap_or("Input disabled.")
                    .to_string()
            };
            if !textarea_rect.is_empty() {
                let placeholder = Span::from(text).dim();
                Line::from(vec![placeholder]).render(textarea_rect.inner(Margin::new(0, 0)), buf);
            }
        }
        // The reserved right margin indicates hidden rows without changing wrapping.
        if options.max_height.is_some()
            && self.draft.input_enabled
            && !textarea_rect.is_empty()
            && textarea_rect.right() < composer_rect.right()
        {
            if state.scroll > 0 {
                buf.set_span(textarea_rect.right(), textarea_rect.y, &"↑".dim(), 1);
            }
            if state.scroll.saturating_add(textarea_rect.height)
                < self.draft.textarea.desired_height(textarea_rect.width)
            {
                buf.set_span(
                    textarea_rect.right(),
                    textarea_rect.bottom() - 1,
                    &"↓".dim(),
                    1,
                );
            }
        }
        if matches!(self.popups.active, ActivePopup::None)
            && let Some(ignition) = &self.effort_ignition
            && !ignition.is_finished()
        {
            let protected_top = if remote_images_rect.is_empty() {
                textarea_rect.y
            } else {
                remote_images_rect.y
            };
            let protected = Rect::new(
                composer_rect.x,
                protected_top,
                composer_rect.width,
                textarea_rect.bottom().saturating_sub(protected_top),
            );
            if ignition.render(composer_rect, protected, buf)
                && let Some(frame_requester) = &self.frame_requester
            {
                frame_requester.schedule_frame_in(IGNITION_FRAME_TICK);
            }
        }
        drop(state);
        self.render_voice_strip(composer_rect, buf);
        self.render_sparkle(
            composer_rect,
            textarea_rect,
            self.cursor_pos_with_options(area, options),
            buf,
        );
        if options.unfocused {
            buf.set_style(composer_rect, crate::style::quiet_workspace_style());
        } else if options.footer.is_some_and(|footer| footer.is_interactive) {
            buf.set_style(composer_rect, Style::default().dim());
        }
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
