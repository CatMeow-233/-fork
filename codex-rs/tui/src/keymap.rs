//! Runtime keymap resolution for the TUI.
//!
//! This module converts deserialized config (`TuiKeymap`) into a concrete
//! `RuntimeKeymap` used by input handlers at runtime.
//!
//! Key responsibilities:
//!
//! 1. Apply deterministic precedence (`context -> global fallback -> defaults`).
//! 2. Parse canonical key spec strings into `KeyBinding` values.
//! 3. Enforce uniqueness across runtime surfaces so one key cannot trigger
//!    multiple actions on the same focused input path.
//! 4. Return actionable, user-facing error messages with config paths and next
//!    steps.
//!
//! Non-responsibilities:
//!
//! 1. This module does not decide which action should run in a given screen.
//!    Callers resolve actions by checking the relevant action binding set.
//! 2. This module does not persist configuration; it only resolves loaded config.

use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::key_hint::ShortcutHint;
use codex_config::types::KeybindingsSpec;
use codex_config::types::MAX_FUNCTION_KEY;
use codex_config::types::TuiKeymap;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

mod bindings;
mod chords;
mod leader;
mod vim_search;
pub(crate) use vim_search::VimSearchKeymap;

#[cfg(test)]
#[path = "keymap/conflict_tests.rs"]
mod conflict_tests;

#[cfg(test)]
#[path = "keymap/voice_tests.rs"]
mod voice_tests;

#[cfg(test)]
#[path = "keymap/global_find_tests.rs"]
mod global_find_tests;

#[cfg(test)]
#[path = "keymap/select_transcript_tests.rs"]
mod select_transcript_tests;

pub(crate) use bindings::KeymapActionId;
pub(crate) use bindings::KeymapContext;
pub(crate) use bindings::bindings_for_action;
pub(crate) use bindings::configured_binding_for_action;
pub(crate) use bindings::keymap_action_id;
pub(crate) use bindings::keymap_action_ids;
use bindings::runtime_action_bindings;
pub(crate) use chords::KEY_CHORD_TIMEOUT;
pub(crate) use chords::KeyChordMatch;
pub(crate) use chords::KeyChordMatcher;
pub(crate) use chords::KeymapContextSet;
pub(crate) use chords::RuntimeChordKeymap;
pub(crate) use chords::chord_stroke_matches;
pub(crate) use chords::dispatch_event;
pub(crate) use chords::is_dispatch_token;
pub(crate) use chords::is_dispatch_token_event;

/// Runtime keymap used by TUI input handlers.
///
/// Resolution precedence is:
///
/// 1. Context-specific binding (`tui.keymap.<context>`).
/// 2. `tui.keymap.global` for actions that support global fallback.
/// 3. Built-in defaults.
///
/// This is the only shape UI code should use for dispatch. It represents a
/// fully resolved snapshot with parsing, fallback, explicit unbinding, and
/// duplicate-key validation already applied. If a caller keeps using an older
/// snapshot after config changes, visible hints and active handlers can drift.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeKeymap {
    pub(crate) app: AppKeymap,
    pub(crate) chords: Arc<RuntimeChordKeymap>,
    pub(crate) chat: ChatKeymap,
    pub(crate) composer: ComposerKeymap,
    pub(crate) editor: Arc<EditorKeymap>,
    pub(crate) vim_normal: VimNormalKeymap,
    pub(crate) vim_operator: VimOperatorKeymap,
    pub(crate) vim_search: VimSearchKeymap,
    pub(crate) vim_text_object: VimTextObjectKeymap,
    pub(crate) pager: PagerKeymap,
    pub(crate) list: ListKeymap,
    pub(crate) agents: AgentsKeymap,
    pub(crate) approval: ApprovalKeymap,
}

#[derive(Clone, Debug)]
pub(crate) struct AppKeymap {
    /// Change where a new conversation runs.
    pub(crate) change_host: Vec<KeyBinding>,
    pub(crate) focus_main_composer: Vec<KeyBinding>,
    pub(crate) focus_sidebar_content: Vec<KeyBinding>,
    pub(crate) focus_sidebar_tabs: Vec<KeyBinding>,
    pub(crate) hide_pane: Vec<KeyBinding>,
    pub(crate) close_sidebar_tab: Vec<KeyBinding>,
    pub(crate) sidebar_up: Vec<KeyBinding>,
    pub(crate) sidebar_down: Vec<KeyBinding>,
    pub(crate) focus_pane_left: Vec<KeyBinding>,
    pub(crate) focus_pane_right: Vec<KeyBinding>,
    pub(crate) minimize_pane_width: Vec<KeyBinding>,
    pub(crate) maximize_pane_width: Vec<KeyBinding>,
    pub(crate) equalize_pane_widths: Vec<KeyBinding>,
    pub(crate) resize_panes: Vec<KeyBinding>,
    pub(crate) move_divider_left: Vec<KeyBinding>,
    pub(crate) move_divider_right: Vec<KeyBinding>,
    pub(crate) toggle_pane_zoom: Vec<KeyBinding>,
    /// Open the agent command center.
    pub(crate) open_agents: Vec<KeyBinding>,
    /// Open transcript overlay.
    pub(crate) open_transcript: Vec<KeyBinding>,
    /// Find text in the full transcript.
    pub(crate) find_transcript: Vec<KeyBinding>,
    /// Enter keyboard selection mode for the transcript.
    pub(crate) select_transcript: Vec<KeyBinding>,
    /// Focus activity groups in the owned transcript to inspect their details.
    pub(crate) focus_activity: Vec<KeyBinding>,
    /// Open retained warnings without replacing the composer.
    pub(crate) open_warnings: Vec<KeyBinding>,
    /// Open external editor for the current draft.
    pub(crate) open_external_editor: Vec<KeyBinding>,
    /// Copy the last agent response to the clipboard.
    pub(crate) copy: Vec<KeyBinding>,
    /// Clear the terminal UI.
    pub(crate) clear_terminal: Vec<KeyBinding>,
    /// Toggle Vim mode for the composer input.
    pub(crate) toggle_vim_mode: Vec<KeyBinding>,
    /// Toggle Fast mode.
    pub(crate) toggle_fast_mode: Vec<KeyBinding>,
    /// Toggle raw scrollback mode for copy-friendly transcript selection.
    pub(crate) toggle_raw_output: Vec<KeyBinding>,
    /// Switch between a side conversation and its parent without closing either.
    pub(crate) toggle_side_conversation: Vec<KeyBinding>,
}

/// Chat-level keybindings evaluated at the app event layer.
///
/// These participate in the first app-scope conflict validation pass alongside
/// `AppKeymap` actions because both are checked before input reaches the
/// composer. Dispatch gating (empty-composer guard for backtrack) happens in
/// handler code, not here.
#[derive(Clone, Debug)]
pub(crate) struct ChatKeymap {
    /// Open the preset submenu.
    pub(crate) open_presets: Vec<KeyBinding>,
    /// Start or stop a voice conversation.
    pub(crate) toggle_voice: Vec<KeyBinding>,
    /// Toggle capture in the active voice session.
    pub(crate) toggle_voice_mute: Vec<KeyBinding>,
    pub(crate) chord_hints: Arc<RuntimeChordKeymap>,
    /// Interrupt the active turn.
    pub(crate) interrupt_turn: Vec<KeyBinding>,
    /// Decrease the active reasoning effort.
    pub(crate) decrease_reasoning_effort: Vec<KeyBinding>,
    /// Increase the active reasoning effort.
    pub(crate) increase_reasoning_effort: Vec<KeyBinding>,
    /// Switch to the previous available permission mode.
    pub(crate) previous_permission_mode: Vec<KeyBinding>,
    /// Switch to the next available permission mode.
    pub(crate) next_permission_mode: Vec<KeyBinding>,
    /// Move forward through async questions, then edit the most recently queued message.
    pub(crate) edit_queued_message: Vec<KeyBinding>,
    /// Move back through async questions toward the composer.
    pub(crate) prompt_stack_back: Vec<KeyBinding>,
    /// Skip the focused question.
    pub(crate) skip_question: Vec<KeyBinding>,
}

impl ChatKeymap {
    pub(crate) fn voice_mute_hint(&self) -> Option<ShortcutHint> {
        let action = keymap_action_id("chat", "toggle_voice_mute")?;
        self.chord_hints
            .primary_hint(action, &self.toggle_voice_mute)
    }
}

/// Composer-level keybindings validated in the second app-scope conflict pass.
///
/// App-level handlers execute before the composer receives input, so any key
/// bound here that also appears in `AppKeymap` would be silently intercepted.
/// The conflict validator prevents this by checking app + composer uniqueness.
#[derive(Clone, Debug)]
pub(crate) struct ComposerKeymap {
    /// Submit current draft.
    pub(crate) submit: Vec<KeyBinding>,
    /// Queue current draft while a task is running.
    pub(crate) queue: Vec<KeyBinding>,
    /// Toggle composer shortcut overlay.
    pub(crate) toggle_shortcuts: Vec<KeyBinding>,
    /// Open reverse history search or move to the previous match.
    pub(crate) history_search_previous: Vec<KeyBinding>,
    /// Move to the next match in reverse history search.
    pub(crate) history_search_next: Vec<KeyBinding>,
}

/// Editor-specific keybindings used by the composer textarea.
///
/// These bindings are interpreted only by text-editing widgets and do not
/// participate in global/chat fallback resolution.
#[derive(Clone, Debug)]
pub(crate) struct EditorKeymap {
    pub(crate) insert_newline: Vec<KeyBinding>,
    pub(crate) move_left: Vec<KeyBinding>,
    pub(crate) move_right: Vec<KeyBinding>,
    pub(crate) move_up: Vec<KeyBinding>,
    pub(crate) move_down: Vec<KeyBinding>,
    pub(crate) move_word_left: Vec<KeyBinding>,
    pub(crate) move_word_right: Vec<KeyBinding>,
    pub(crate) move_line_start: Vec<KeyBinding>,
    pub(crate) move_line_end: Vec<KeyBinding>,
    pub(crate) delete_backward: Vec<KeyBinding>,
    pub(crate) delete_forward: Vec<KeyBinding>,
    pub(crate) delete_backward_word: Vec<KeyBinding>,
    pub(crate) delete_forward_word: Vec<KeyBinding>,
    pub(crate) kill_line_start: Vec<KeyBinding>,
    pub(crate) kill_whole_line: Vec<KeyBinding>,
    pub(crate) kill_line_end: Vec<KeyBinding>,
    pub(crate) yank: Vec<KeyBinding>,
}

/// Vim normal-mode keybindings for modal editing in the composer textarea.
///
/// Normal mode is the resting state when Vim is enabled. Pressing a movement
/// or editing key here either moves the cursor, triggers an operator-pending
/// state (via `start_delete_operator`, `start_yank_operator`, or `start_change_operator`), or transitions
/// to insert mode. Default bindings include both `shift(letter)` and
/// `plain(UPPERCASE)` variants for uppercase commands like `A`, `I`, `O` to
/// handle cross-terminal shift-reporting inconsistencies.
#[derive(Clone, Debug, Default)]
pub(crate) struct VimNormalKeymap {
    pub(crate) enter_insert: Vec<KeyBinding>,
    pub(crate) append_after_cursor: Vec<KeyBinding>,
    pub(crate) append_line_end: Vec<KeyBinding>,
    pub(crate) insert_line_start: Vec<KeyBinding>,
    pub(crate) open_line_below: Vec<KeyBinding>,
    pub(crate) open_line_above: Vec<KeyBinding>,
    pub(crate) enter_replace_mode: Vec<KeyBinding>,
    pub(crate) move_left: Vec<KeyBinding>,
    pub(crate) move_right: Vec<KeyBinding>,
    pub(crate) move_up: Vec<KeyBinding>,
    pub(crate) move_down: Vec<KeyBinding>,
    pub(crate) move_word_forward: Vec<KeyBinding>,
    pub(crate) move_word_backward: Vec<KeyBinding>,
    pub(crate) move_word_end: Vec<KeyBinding>,
    pub(crate) move_line_start: Vec<KeyBinding>,
    pub(crate) move_line_end: Vec<KeyBinding>,
    pub(crate) find_forward: Vec<KeyBinding>,
    pub(crate) find_backward: Vec<KeyBinding>,
    pub(crate) till_forward: Vec<KeyBinding>,
    pub(crate) till_backward: Vec<KeyBinding>,
    pub(crate) jump_top: Vec<KeyBinding>,
    pub(crate) jump_bottom: Vec<KeyBinding>,
    pub(crate) delete_char: Vec<KeyBinding>,
    pub(crate) replace_char: Vec<KeyBinding>,
    pub(crate) repeat_last_change: Vec<KeyBinding>,
    pub(crate) substitute_char: Vec<KeyBinding>,
    pub(crate) delete_to_line_end: Vec<KeyBinding>,
    pub(crate) change_to_line_end: Vec<KeyBinding>,
    pub(crate) yank_line: Vec<KeyBinding>,
    pub(crate) paste_after: Vec<KeyBinding>,
    pub(crate) start_delete_operator: Vec<KeyBinding>,
    pub(crate) start_yank_operator: Vec<KeyBinding>,
    pub(crate) start_change_operator: Vec<KeyBinding>,
    pub(crate) undo: Vec<KeyBinding>,
    pub(crate) redo: Vec<KeyBinding>,
    pub(crate) cancel_operator: Vec<KeyBinding>,
}

/// Vim operator-pending keybindings active after `d`, `y`, or `c` in normal mode.
///
/// When a delete, yank, or change operator is
/// pressed, the next keypress is matched against this context to determine the
/// motion range. Repeating the operator key (`dd`, `yy`) acts on the whole
/// line. `Esc` cancels the pending operator and returns to normal mode.
#[derive(Clone, Debug, Default)]
pub(crate) struct VimOperatorKeymap {
    pub(crate) delete_line: Vec<KeyBinding>,
    pub(crate) yank_line: Vec<KeyBinding>,
    pub(crate) motion_left: Vec<KeyBinding>,
    pub(crate) motion_right: Vec<KeyBinding>,
    pub(crate) motion_up: Vec<KeyBinding>,
    pub(crate) motion_down: Vec<KeyBinding>,
    pub(crate) motion_word_forward: Vec<KeyBinding>,
    pub(crate) motion_word_backward: Vec<KeyBinding>,
    pub(crate) motion_word_end: Vec<KeyBinding>,
    pub(crate) motion_line_start: Vec<KeyBinding>,
    pub(crate) motion_line_end: Vec<KeyBinding>,
    pub(crate) motion_find_forward: Vec<KeyBinding>,
    pub(crate) motion_find_backward: Vec<KeyBinding>,
    pub(crate) motion_till_forward: Vec<KeyBinding>,
    pub(crate) motion_till_backward: Vec<KeyBinding>,
    pub(crate) motion_jump_top: Vec<KeyBinding>,
    pub(crate) motion_jump_bottom: Vec<KeyBinding>,
    pub(crate) select_inner_text_object: Vec<KeyBinding>,
    pub(crate) select_around_text_object: Vec<KeyBinding>,
    pub(crate) cancel: Vec<KeyBinding>,
}

/// Vim text-object keybindings active after an operator plus inner/around prefix.
#[derive(Clone, Debug, Default)]
pub(crate) struct VimTextObjectKeymap {
    pub(crate) word: Vec<KeyBinding>,
    pub(crate) big_word: Vec<KeyBinding>,
    pub(crate) parentheses: Vec<KeyBinding>,
    pub(crate) brackets: Vec<KeyBinding>,
    pub(crate) braces: Vec<KeyBinding>,
    pub(crate) double_quote: Vec<KeyBinding>,
    pub(crate) single_quote: Vec<KeyBinding>,
    pub(crate) backtick: Vec<KeyBinding>,
    pub(crate) cancel: Vec<KeyBinding>,
}

/// Pager/overlay keybindings for transcript and static help views.
#[derive(Clone, Debug)]
pub(crate) struct PagerKeymap {
    pub(crate) scroll_up: Vec<KeyBinding>,
    pub(crate) scroll_down: Vec<KeyBinding>,
    pub(crate) page_up: Vec<KeyBinding>,
    pub(crate) page_down: Vec<KeyBinding>,
    pub(crate) half_page_up: Vec<KeyBinding>,
    pub(crate) half_page_down: Vec<KeyBinding>,
    pub(crate) jump_top: Vec<KeyBinding>,
    pub(crate) jump_bottom: Vec<KeyBinding>,
    pub(crate) close: Vec<KeyBinding>,
    pub(crate) close_transcript: Vec<KeyBinding>,
    pub(crate) find: Vec<KeyBinding>,
    chord_hints: Arc<RuntimeChordKeymap>,
}

impl PagerKeymap {
    pub(crate) fn primary_hint(
        &self,
        action: &'static str,
        bindings: &[KeyBinding],
    ) -> Option<ShortcutHint> {
        let action = keymap_action_id(KeymapContext::Pager.config_name(), action)?;
        self.chord_hints.primary_hint(action, bindings)
    }
}

/// Semantic navigation and confirmation actions shared by list-like views.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListAction {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    PageUp,
    PageDown,
    JumpTop,
    JumpBottom,
    Accept,
    Cancel,
}

impl ListAction {
    fn config_name(self) -> &'static str {
        match self {
            Self::MoveUp => "move_up",
            Self::MoveDown => "move_down",
            Self::MoveLeft => "move_left",
            Self::MoveRight => "move_right",
            Self::PageUp => "page_up",
            Self::PageDown => "page_down",
            Self::JumpTop => "jump_top",
            Self::JumpBottom => "jump_bottom",
            Self::Accept => "accept",
            Self::Cancel => "cancel",
        }
    }
}

/// Generic list picker keybindings shared across popup list views.
///
/// These actions describe list intent rather than a specific widget layout.
/// Vertical actions move the highlighted row, page and jump actions move within
/// the current filtered row set, and horizontal actions are available to views
/// that expose adjacent choices such as tabs, toolbar values, or ordered item
/// movement. Views that also accept search text are responsible for checking
/// `is_plain_text_key_event` before dispatching plain-character bindings so a
/// configured `j`, `k`, `h`, or `l` does not steal query input.
#[derive(Clone, Debug)]
pub(crate) struct ListKeymap {
    pub(crate) move_up: Vec<KeyBinding>,
    pub(crate) move_down: Vec<KeyBinding>,
    pub(crate) move_left: Vec<KeyBinding>,
    pub(crate) move_right: Vec<KeyBinding>,
    pub(crate) page_up: Vec<KeyBinding>,
    pub(crate) page_down: Vec<KeyBinding>,
    pub(crate) jump_top: Vec<KeyBinding>,
    pub(crate) jump_bottom: Vec<KeyBinding>,
    pub(crate) accept: Vec<KeyBinding>,
    pub(crate) cancel: Vec<KeyBinding>,
    chord_hints: Arc<RuntimeChordKeymap>,
}

impl ListKeymap {
    pub(crate) fn bindings_for(&self, action: ListAction) -> &[KeyBinding] {
        match action {
            ListAction::MoveUp => &self.move_up,
            ListAction::MoveDown => &self.move_down,
            ListAction::MoveLeft => &self.move_left,
            ListAction::MoveRight => &self.move_right,
            ListAction::PageUp => &self.page_up,
            ListAction::PageDown => &self.page_down,
            ListAction::JumpTop => &self.jump_top,
            ListAction::JumpBottom => &self.jump_bottom,
            ListAction::Accept => &self.accept,
            ListAction::Cancel => &self.cancel,
        }
    }

    pub(crate) fn action_for(&self, event: KeyEvent) -> Option<ListAction> {
        [
            ListAction::MoveUp,
            ListAction::MoveDown,
            ListAction::MoveLeft,
            ListAction::MoveRight,
            ListAction::PageUp,
            ListAction::PageDown,
            ListAction::JumpTop,
            ListAction::JumpBottom,
            ListAction::Accept,
            ListAction::Cancel,
        ]
        .into_iter()
        .find(|action| self.bindings_for(*action).is_pressed(event))
    }

    /// Resolve only vertical navigation, preserving its historical priority.
    pub(crate) fn vertical_action_for(&self, event: KeyEvent) -> Option<ListAction> {
        [
            ListAction::MoveUp,
            ListAction::MoveDown,
            ListAction::PageUp,
            ListAction::PageDown,
            ListAction::JumpTop,
            ListAction::JumpBottom,
        ]
        .into_iter()
        .find(|action| self.bindings_for(*action).is_pressed(event))
    }

    pub(crate) fn has_chord_prefix(&self, event: KeyEvent) -> bool {
        self.chord_hints.bindings.iter().any(|binding| {
            binding.action.context.overlaps(KeymapContext::List)
                && binding.chord.prefix.is_press(event)
        })
    }

    pub(crate) fn primary_hint(&self, action: ListAction) -> Option<ShortcutHint> {
        let action_id = keymap_action_id(KeymapContext::List.config_name(), action.config_name())?;
        self.chord_hints
            .primary_hint(action_id, self.bindings_for(action))
    }
}

/// Task-management shortcuts specific to the agents dashboard.
#[derive(Clone, Debug)]
pub(crate) struct AgentsKeymap {
    pub(crate) resume: Vec<KeyBinding>,
    pub(crate) search: Vec<KeyBinding>,
    pub(crate) new_task: Vec<KeyBinding>,
    pub(crate) new_worktree: Vec<KeyBinding>,
    pub(crate) fork: Vec<KeyBinding>,
    pub(crate) rename: Vec<KeyBinding>,
    pub(crate) stop: Vec<KeyBinding>,
    pub(crate) archive: Vec<KeyBinding>,
    pub(crate) delete: Vec<KeyBinding>,
    pub(crate) hide: Vec<KeyBinding>,
    pub(crate) toggle_pin: Vec<KeyBinding>,
    pub(crate) toggle_grouping: Vec<KeyBinding>,
    chord_hints: Arc<RuntimeChordKeymap>,
}

impl AgentsKeymap {
    pub(crate) fn primary_hint(
        &self,
        action: &'static str,
        bindings: &[KeyBinding],
    ) -> Option<ShortcutHint> {
        let action_id = keymap_action_id(KeymapContext::Agents.config_name(), action)?;
        self.chord_hints.primary_hint(action_id, bindings)
    }
}

/// Approval modal keybindings.
///
/// This covers both selection actions and the "open details fullscreen" escape
/// hatch for large approval payloads.
#[derive(Clone, Debug)]
pub(crate) struct ApprovalKeymap {
    pub(crate) open_fullscreen: Vec<KeyBinding>,
    pub(crate) open_thread: Vec<KeyBinding>,
    pub(crate) approve: Vec<KeyBinding>,
    pub(crate) approve_for_session: Vec<KeyBinding>,
    pub(crate) approve_for_prefix: Vec<KeyBinding>,
    pub(crate) deny: Vec<KeyBinding>,
    pub(crate) decline: Vec<KeyBinding>,
    pub(crate) cancel: Vec<KeyBinding>,
    chord_hints: Arc<RuntimeChordKeymap>,
}

impl ApprovalKeymap {
    pub(crate) fn primary_hint(
        &self,
        action: &'static str,
        bindings: &[KeyBinding],
    ) -> Option<ShortcutHint> {
        let action = keymap_action_id(KeymapContext::Approval.config_name(), action)?;
        self.chord_hints.primary_hint(action, bindings)
    }

    pub(crate) fn hint_for_bindings(&self, bindings: &[KeyBinding]) -> Option<ShortcutHint> {
        [
            ("open_fullscreen", self.open_fullscreen.as_slice()),
            ("open_thread", self.open_thread.as_slice()),
            ("approve", self.approve.as_slice()),
            ("approve_for_session", self.approve_for_session.as_slice()),
            ("approve_for_prefix", self.approve_for_prefix.as_slice()),
            ("deny", self.deny.as_slice()),
            ("decline", self.decline.as_slice()),
            ("cancel", self.cancel.as_slice()),
        ]
        .into_iter()
        .find_map(|(action, configured)| {
            (!bindings.is_empty() && bindings.iter().all(|binding| configured.contains(binding)))
                .then(|| self.primary_hint(action, configured))
                .flatten()
        })
        .or_else(|| primary_binding(bindings).map(ShortcutHint::from))
    }
}

/// Returns the first binding, used as the primary UI hint for an action.
///
/// Rendering code should prefer this for concise hints while preserving all
/// bindings for actual input matching.
pub(crate) fn primary_binding(bindings: &[KeyBinding]) -> Option<KeyBinding> {
    user_bindings(bindings).first().copied()
}

/// Remove the trailing internal dispatch token, when present.
pub(crate) fn user_bindings(bindings: &[KeyBinding]) -> &[KeyBinding] {
    if bindings
        .last()
        .is_some_and(|binding| chords::is_dispatch_token(*binding))
    {
        &bindings[..bindings.len() - 1]
    } else {
        bindings
    }
}

/// Resolve one context-local action binding from config.
///
/// Expands to `resolve_bindings(...)` with:
/// - configured source: `tui.keymap.<context>.<action>`
/// - fallback source: the same action from built-in defaults
/// - error path: a stable string path for user-facing diagnostics
///
/// This keeps the resolution table concise while guaranteeing path strings
/// stay in sync with field names.
macro_rules! resolve_local {
    ($keymap:expr, $defaults:expr, $context:ident, $action:ident) => {
        resolve_bindings(
            ($keymap).$context.$action.as_ref(),
            &($defaults).$context.$action,
            concat!(
                "tui.keymap.",
                stringify!($context),
                ".",
                stringify!($action)
            ),
        )?
    };
}

/// Resolve one action binding with global fallback.
///
/// Expands to `resolve_bindings_with_global_fallback(...)` with precedence:
/// 1. `tui.keymap.<context>.<action>`
/// 2. `tui.keymap.global.<action>`
/// 3. built-in defaults for `<context>.<action>`
///
/// Used only for actions that intentionally support global reuse.
/// Context-local empty lists still count as configured values, so they unbind
/// the action instead of falling back to `global`.
macro_rules! resolve_with_global {
    ($keymap:expr, $defaults:expr, $context:ident, $action:ident) => {
        resolve_bindings_with_global_fallback(
            ($keymap).$context.$action.as_ref(),
            ($keymap).global.$action.as_ref(),
            &($defaults).$context.$action,
            concat!(
                "tui.keymap.",
                stringify!($context),
                ".",
                stringify!($action)
            ),
        )?
    };
}

/// Expand one default-table binding entry into a [`KeyBinding`].
///
/// This is a small declarative layer over `key_hint::{plain, ctrl, alt, shift}`
/// used by `default_bindings!` so `built_in_defaults` stays readable.
///
/// Supported forms:
/// - `plain(<KeyCode>)`
/// - `ctrl(<KeyCode>)`
/// - `alt(<KeyCode>)`
/// - `shift(<KeyCode>)`
/// - `raw(<KeyBinding expression>)` for bindings that do not match the helpers
///   (for example combined modifiers like Ctrl+Shift).
macro_rules! default_binding {
    (plain($key:expr)) => {
        key_hint::plain($key)
    };
    (ctrl($key:expr)) => {
        key_hint::ctrl($key)
    };
    (alt($key:expr)) => {
        key_hint::alt($key)
    };
    (shift($key:expr)) => {
        key_hint::shift($key)
    };
    (raw($binding:expr)) => {
        $binding
    };
}

/// Build a `Vec<KeyBinding>` for built-in defaults.
///
/// This macro is intentionally scoped to built-in keymaps. Runtime
/// config parsing still goes through `parse_bindings(...)` so user errors can
/// be reported with config-path-aware diagnostics.
macro_rules! default_bindings {
    ($($kind:ident($($arg:tt)*)),* $(,)?) => {
        vec![$(default_binding!($kind($($arg)*))),*]
    };
}

impl RuntimeKeymap {
    /// Return built-in defaults.
    ///
    /// This is a convenience for tests and bootstrapping UI state before user
    /// config has been loaded. It should not be used as a fallback after
    /// parsing `TuiKeymap`, because doing so would ignore explicit user
    /// unbindings and conflict diagnostics.
    pub(crate) fn defaults() -> Self {
        static DEFAULTS: std::sync::OnceLock<RuntimeKeymap> = std::sync::OnceLock::new();

        DEFAULTS
            .get_or_init(|| {
                Self::resolve(&TuiKeymap::default(), /*workspace*/ false).unwrap_or_else(|error| {
                    panic!("built-in keymap defaults must be valid: {error}")
                })
            })
            .clone()
    }

    /// Resolve a runtime keymap from config, applying precedence and validation.
    ///
    /// Returns an error when:
    ///
    /// 1. A keybinding spec cannot be parsed.
    /// 2. A context has ambiguous bindings (same key assigned to multiple actions).
    ///
    /// The error text includes the relevant config path and a concrete next step.
    /// Calling code should not merge bindings across unrelated contexts before
    /// dispatch, or conflict guarantees from this resolver no longer hold.
    pub(crate) fn from_config(keymap: &TuiKeymap) -> Result<Self, String> {
        if keymap == &TuiKeymap::default() {
            return Ok(Self::defaults());
        }
        Self::resolve(keymap, /*workspace*/ false)
    }

    pub(crate) fn for_session(keymap: &TuiKeymap, workspace: bool) -> Result<Self, String> {
        if workspace {
            Self::resolve(keymap, workspace)
        } else {
            Self::from_config(keymap)
        }
    }

    fn resolve(keymap: &TuiKeymap, workspace: bool) -> Result<Self, String> {
        // Inactive pane shortcuts must not claim keys or chord prefixes in classic mode.
        let mut classic_keymap;
        let keymap = if workspace {
            keymap
        } else {
            classic_keymap = keymap.clone();
            classic_keymap.global.focus_main_composer = None;
            classic_keymap.global.focus_sidebar_content = None;
            classic_keymap.global.focus_sidebar_tabs = None;
            classic_keymap.global.hide_pane = None;
            classic_keymap.global.close_sidebar_tab = None;
            classic_keymap.global.sidebar_up = None;
            classic_keymap.global.sidebar_down = None;
            classic_keymap.global.focus_pane_left = None;
            classic_keymap.global.focus_pane_right = None;
            &classic_keymap
        };
        let defaults = Self::built_in_defaults();
        let chords = Arc::new(RuntimeChordKeymap::from_config(keymap)?);
        let side_toggle_default_is_shadowed = keymap.global.toggle_side_conversation.is_none()
            && ["ctrl-/", "ctrl-7"].into_iter().any(|alias| {
                configured_main_surface_alias_is_used(keymap, alias)
                    || configured_context_alias_is_used(&keymap.list, alias)
                    || configured_context_alias_is_used(&keymap.approval, alias)
            });

        // Voice mute yields to existing shortcuts and chord prefixes.
        let voice_mute_default_is_shadowed = keymap.chat.toggle_voice_mute.is_none()
            && (configured_main_surface_alias_is_used(keymap, "f9")
                || configured_context_alias_is_used(&keymap.vim_search, "f9")
                || chords.bindings.iter().any(|binding| {
                    binding.action.context.overlaps(KeymapContext::Voice)
                        && binding.chord.prefix.parts() == key_hint::plain(KeyCode::F(9)).parts()
                }));

        // New activity and warnings defaults must not invalidate an existing custom binding or
        // chord prefix.
        let unclaimed_defaults = |bindings: &[KeyBinding]| -> Vec<KeyBinding> {
            bindings
                .iter()
                .copied()
                .filter(|binding| {
                    !configured_context_binding_is_used(keymap, *binding)
                        && !chords.bindings.iter().any(|chord| {
                            chord.chord.prefix.normalized_parts() == binding.normalized_parts()
                        })
                })
                .collect()
        };
        let focus_activity_defaults = unclaimed_defaults(&defaults.app.focus_activity);
        let open_warnings_defaults = unclaimed_defaults(&defaults.app.open_warnings);

        let app = AppKeymap {
            change_host: resolve_bindings(
                keymap.global.change_host.as_ref(),
                &defaults.app.change_host,
                "tui.keymap.global.change_host",
            )?,
            focus_main_composer: resolve_bindings(
                keymap.global.focus_main_composer.as_ref(),
                &[],
                "tui.keymap.global.focus_main_composer",
            )?,
            focus_sidebar_content: resolve_bindings(
                keymap.global.focus_sidebar_content.as_ref(),
                &[],
                "tui.keymap.global.focus_sidebar_content",
            )?,
            focus_sidebar_tabs: resolve_bindings(
                keymap.global.focus_sidebar_tabs.as_ref(),
                &[],
                "tui.keymap.global.focus_sidebar_tabs",
            )?,
            hide_pane: resolve_bindings(
                keymap.global.hide_pane.as_ref(),
                &[],
                "tui.keymap.global.hide_pane",
            )?,
            close_sidebar_tab: resolve_bindings(
                keymap.global.close_sidebar_tab.as_ref(),
                &[],
                "tui.keymap.global.close_sidebar_tab",
            )?,
            sidebar_up: resolve_bindings(
                keymap.global.sidebar_up.as_ref(),
                &[],
                "tui.keymap.global.sidebar_up",
            )?,
            sidebar_down: resolve_bindings(
                keymap.global.sidebar_down.as_ref(),
                &[],
                "tui.keymap.global.sidebar_down",
            )?,
            focus_pane_left: resolve_bindings(
                keymap.global.focus_pane_left.as_ref(),
                &[],
                "tui.keymap.global.focus_pane_left",
            )?,
            focus_pane_right: resolve_bindings(
                keymap.global.focus_pane_right.as_ref(),
                &[],
                "tui.keymap.global.focus_pane_right",
            )?,
            minimize_pane_width: resolve_bindings(
                keymap.global.minimize_pane_width.as_ref(),
                &[],
                "tui.keymap.global.minimize_pane_width",
            )?,
            maximize_pane_width: resolve_bindings(
                keymap.global.maximize_pane_width.as_ref(),
                &[],
                "tui.keymap.global.maximize_pane_width",
            )?,
            equalize_pane_widths: resolve_bindings(
                keymap.global.equalize_pane_widths.as_ref(),
                &[],
                "tui.keymap.global.equalize_pane_widths",
            )?,
            resize_panes: resolve_bindings(
                keymap.global.resize_panes.as_ref(),
                &[],
                "tui.keymap.global.resize_panes",
            )?,
            move_divider_left: resolve_bindings(
                keymap.global.move_divider_left.as_ref(),
                &[],
                "tui.keymap.global.move_divider_left",
            )?,
            move_divider_right: resolve_bindings(
                keymap.global.move_divider_right.as_ref(),
                &[],
                "tui.keymap.global.move_divider_right",
            )?,
            toggle_pane_zoom: resolve_bindings(
                keymap.global.toggle_pane_zoom.as_ref(),
                &[],
                "tui.keymap.global.toggle_pane_zoom",
            )?,
            open_agents: resolve_bindings(
                keymap.global.open_agents.as_ref(),
                &defaults.app.open_agents,
                "tui.keymap.global.open_agents",
            )?,
            open_transcript: resolve_bindings(
                keymap.global.open_transcript.as_ref(),
                &defaults.app.open_transcript,
                "tui.keymap.global.open_transcript",
            )?,
            find_transcript: resolve_bindings(
                keymap.global.find_transcript.as_ref(),
                &defaults.app.find_transcript,
                "tui.keymap.global.find_transcript",
            )?,
            select_transcript: resolve_bindings(
                keymap.global.select_transcript.as_ref(),
                &defaults.app.select_transcript,
                "tui.keymap.global.select_transcript",
            )?,
            focus_activity: resolve_bindings(
                keymap.global.focus_activity.as_ref(),
                &focus_activity_defaults,
                "tui.keymap.global.focus_activity",
            )?,
            open_warnings: open_warnings_defaults,
            open_external_editor: resolve_bindings(
                keymap.global.open_external_editor.as_ref(),
                &defaults.app.open_external_editor,
                "tui.keymap.global.open_external_editor",
            )?,
            copy: resolve_bindings(
                keymap.global.copy.as_ref(),
                &defaults.app.copy,
                "tui.keymap.global.copy",
            )?,
            clear_terminal: resolve_bindings(
                keymap.global.clear_terminal.as_ref(),
                &defaults.app.clear_terminal,
                "tui.keymap.global.clear_terminal",
            )?,
            toggle_vim_mode: resolve_bindings(
                keymap.global.toggle_vim_mode.as_ref(),
                &defaults.app.toggle_vim_mode,
                "tui.keymap.global.toggle_vim_mode",
            )?,
            toggle_fast_mode: resolve_bindings(
                keymap.global.toggle_fast_mode.as_ref(),
                &defaults.app.toggle_fast_mode,
                "tui.keymap.global.toggle_fast_mode",
            )?,
            toggle_raw_output: resolve_bindings(
                keymap.global.toggle_raw_output.as_ref(),
                &defaults.app.toggle_raw_output,
                "tui.keymap.global.toggle_raw_output",
            )?,
            toggle_side_conversation: if side_toggle_default_is_shadowed {
                Vec::new()
            } else {
                resolve_bindings(
                    keymap.global.toggle_side_conversation.as_ref(),
                    &defaults.app.toggle_side_conversation,
                    "tui.keymap.global.toggle_side_conversation",
                )?
            },
        };

        // Voice yields to explicitly configured shortcuts and chord prefixes.
        let voice_toggle_default_is_shadowed = keymap.chat.toggle_voice.is_none()
            && (configured_main_surface_alias_is_used(keymap, "f8")
                || configured_context_alias_is_used(&keymap.vim_search, "f8")
                || chords.bindings.iter().any(|binding| {
                    binding.action.context.overlaps(KeymapContext::Chat)
                        && binding.chord.prefix.parts() == key_hint::plain(KeyCode::F(8)).parts()
                }));

        let mut chat = ChatKeymap {
            open_presets: resolve_local!(keymap, defaults, chat, open_presets),
            toggle_voice: if voice_toggle_default_is_shadowed {
                Vec::new()
            } else {
                resolve_local!(keymap, defaults, chat, toggle_voice)
            },
            toggle_voice_mute: if voice_mute_default_is_shadowed {
                Vec::new()
            } else {
                resolve_local!(keymap, defaults, chat, toggle_voice_mute)
            },
            chord_hints: Arc::clone(&chords),
            interrupt_turn: resolve_local!(keymap, defaults, chat, interrupt_turn),
            decrease_reasoning_effort: resolve_bindings(
                keymap.chat.decrease_reasoning_effort.as_ref(),
                &defaults.chat.decrease_reasoning_effort,
                "tui.keymap.chat.decrease_reasoning_effort",
            )?,
            increase_reasoning_effort: resolve_bindings(
                keymap.chat.increase_reasoning_effort.as_ref(),
                &defaults.chat.increase_reasoning_effort,
                "tui.keymap.chat.increase_reasoning_effort",
            )?,
            previous_permission_mode: resolve_local!(
                keymap,
                defaults,
                chat,
                previous_permission_mode
            ),
            next_permission_mode: resolve_local!(keymap, defaults, chat, next_permission_mode),
            edit_queued_message: resolve_local!(keymap, defaults, chat, edit_queued_message),
            prompt_stack_back: resolve_local!(keymap, defaults, chat, prompt_stack_back),
            skip_question: resolve_local!(keymap, defaults, chat, skip_question),
        };

        let composer = ComposerKeymap {
            submit: resolve_with_global!(keymap, defaults, composer, submit),
            queue: resolve_with_global!(keymap, defaults, composer, queue),
            toggle_shortcuts: resolve_with_global!(keymap, defaults, composer, toggle_shortcuts),
            history_search_previous: resolve_local!(
                keymap,
                defaults,
                composer,
                history_search_previous
            ),
            history_search_next: resolve_local!(keymap, defaults, composer, history_search_next),
        };

        let editor = Arc::new(EditorKeymap {
            insert_newline: resolve_local!(keymap, defaults, editor, insert_newline),
            move_left: resolve_local!(keymap, defaults, editor, move_left),
            move_right: resolve_local!(keymap, defaults, editor, move_right),
            move_up: resolve_local!(keymap, defaults, editor, move_up),
            move_down: resolve_local!(keymap, defaults, editor, move_down),
            move_word_left: resolve_local!(keymap, defaults, editor, move_word_left),
            move_word_right: resolve_local!(keymap, defaults, editor, move_word_right),
            move_line_start: resolve_local!(keymap, defaults, editor, move_line_start),
            move_line_end: resolve_local!(keymap, defaults, editor, move_line_end),
            delete_backward: resolve_local!(keymap, defaults, editor, delete_backward),
            delete_forward: resolve_local!(keymap, defaults, editor, delete_forward),
            delete_backward_word: resolve_local!(keymap, defaults, editor, delete_backward_word),
            delete_forward_word: resolve_local!(keymap, defaults, editor, delete_forward_word),
            kill_line_start: resolve_local!(keymap, defaults, editor, kill_line_start),
            kill_whole_line: resolve_local!(keymap, defaults, editor, kill_whole_line),
            kill_line_end: resolve_local!(keymap, defaults, editor, kill_line_end),
            yank: resolve_local!(keymap, defaults, editor, yank),
        });

        let mut vim_normal = VimNormalKeymap {
            enter_insert: resolve_local!(keymap, defaults, vim_normal, enter_insert),
            append_after_cursor: resolve_local!(keymap, defaults, vim_normal, append_after_cursor),
            append_line_end: resolve_local!(keymap, defaults, vim_normal, append_line_end),
            insert_line_start: resolve_local!(keymap, defaults, vim_normal, insert_line_start),
            open_line_below: resolve_local!(keymap, defaults, vim_normal, open_line_below),
            open_line_above: resolve_local!(keymap, defaults, vim_normal, open_line_above),
            enter_replace_mode: resolve_local!(keymap, defaults, vim_normal, enter_replace_mode),
            move_left: resolve_local!(keymap, defaults, vim_normal, move_left),
            move_right: resolve_local!(keymap, defaults, vim_normal, move_right),
            move_up: resolve_local!(keymap, defaults, vim_normal, move_up),
            move_down: resolve_local!(keymap, defaults, vim_normal, move_down),
            move_word_forward: resolve_local!(keymap, defaults, vim_normal, move_word_forward),
            move_word_backward: resolve_local!(keymap, defaults, vim_normal, move_word_backward),
            move_word_end: resolve_local!(keymap, defaults, vim_normal, move_word_end),
            move_line_start: resolve_local!(keymap, defaults, vim_normal, move_line_start),
            move_line_end: resolve_local!(keymap, defaults, vim_normal, move_line_end),
            find_forward: resolve_local!(keymap, defaults, vim_normal, find_forward),
            find_backward: resolve_local!(keymap, defaults, vim_normal, find_backward),
            till_forward: resolve_local!(keymap, defaults, vim_normal, till_forward),
            till_backward: resolve_local!(keymap, defaults, vim_normal, till_backward),
            jump_top: resolve_local!(keymap, defaults, vim_normal, jump_top),
            jump_bottom: resolve_local!(keymap, defaults, vim_normal, jump_bottom),
            delete_char: resolve_local!(keymap, defaults, vim_normal, delete_char),
            replace_char: resolve_local!(keymap, defaults, vim_normal, replace_char),
            repeat_last_change: resolve_local!(keymap, defaults, vim_normal, repeat_last_change),
            substitute_char: resolve_local!(keymap, defaults, vim_normal, substitute_char),
            delete_to_line_end: resolve_local!(keymap, defaults, vim_normal, delete_to_line_end),
            change_to_line_end: resolve_local!(keymap, defaults, vim_normal, change_to_line_end),
            yank_line: resolve_local!(keymap, defaults, vim_normal, yank_line),
            paste_after: resolve_local!(keymap, defaults, vim_normal, paste_after),
            start_delete_operator: resolve_local!(
                keymap,
                defaults,
                vim_normal,
                start_delete_operator
            ),
            start_yank_operator: resolve_local!(keymap, defaults, vim_normal, start_yank_operator),
            start_change_operator: resolve_local!(
                keymap,
                defaults,
                vim_normal,
                start_change_operator
            ),
            undo: resolve_local!(keymap, defaults, vim_normal, undo),
            redo: resolve_local!(keymap, defaults, vim_normal, redo),
            cancel_operator: resolve_local!(keymap, defaults, vim_normal, cancel_operator),
        };

        let configured_vim_normal_bindings_to_preserve = configured_bindings_to_preserve([
            (
                keymap.vim_normal.enter_insert.as_ref(),
                vim_normal.enter_insert.as_slice(),
            ),
            (
                keymap.vim_normal.append_after_cursor.as_ref(),
                vim_normal.append_after_cursor.as_slice(),
            ),
            (
                keymap.vim_normal.append_line_end.as_ref(),
                vim_normal.append_line_end.as_slice(),
            ),
            (
                keymap.vim_normal.insert_line_start.as_ref(),
                vim_normal.insert_line_start.as_slice(),
            ),
            (
                keymap.vim_normal.open_line_below.as_ref(),
                vim_normal.open_line_below.as_slice(),
            ),
            (
                keymap.vim_normal.open_line_above.as_ref(),
                vim_normal.open_line_above.as_slice(),
            ),
            (
                keymap.vim_normal.enter_replace_mode.as_ref(),
                vim_normal.enter_replace_mode.as_slice(),
            ),
            (
                keymap.vim_normal.move_left.as_ref(),
                vim_normal.move_left.as_slice(),
            ),
            (
                keymap.vim_normal.move_right.as_ref(),
                vim_normal.move_right.as_slice(),
            ),
            (
                keymap.vim_normal.move_up.as_ref(),
                vim_normal.move_up.as_slice(),
            ),
            (
                keymap.vim_normal.move_down.as_ref(),
                vim_normal.move_down.as_slice(),
            ),
            (
                keymap.vim_normal.move_word_forward.as_ref(),
                vim_normal.move_word_forward.as_slice(),
            ),
            (
                keymap.vim_normal.move_word_backward.as_ref(),
                vim_normal.move_word_backward.as_slice(),
            ),
            (
                keymap.vim_normal.move_word_end.as_ref(),
                vim_normal.move_word_end.as_slice(),
            ),
            (
                keymap.vim_normal.move_line_start.as_ref(),
                vim_normal.move_line_start.as_slice(),
            ),
            (
                keymap.vim_normal.move_line_end.as_ref(),
                vim_normal.move_line_end.as_slice(),
            ),
            (
                keymap.vim_normal.find_forward.as_ref(),
                vim_normal.find_forward.as_slice(),
            ),
            (
                keymap.vim_normal.find_backward.as_ref(),
                vim_normal.find_backward.as_slice(),
            ),
            (
                keymap.vim_normal.till_forward.as_ref(),
                vim_normal.till_forward.as_slice(),
            ),
            (
                keymap.vim_normal.till_backward.as_ref(),
                vim_normal.till_backward.as_slice(),
            ),
            (
                keymap.vim_normal.jump_top.as_ref(),
                vim_normal.jump_top.as_slice(),
            ),
            (
                keymap.vim_normal.jump_bottom.as_ref(),
                vim_normal.jump_bottom.as_slice(),
            ),
            (
                keymap.vim_normal.delete_char.as_ref(),
                vim_normal.delete_char.as_slice(),
            ),
            (
                keymap.vim_normal.replace_char.as_ref(),
                vim_normal.replace_char.as_slice(),
            ),
            (
                keymap.vim_normal.substitute_char.as_ref(),
                vim_normal.substitute_char.as_slice(),
            ),
            (
                keymap.vim_normal.repeat_last_change.as_ref(),
                vim_normal.repeat_last_change.as_slice(),
            ),
            (
                keymap.vim_normal.change_to_line_end.as_ref(),
                vim_normal.change_to_line_end.as_slice(),
            ),
            (
                keymap.vim_normal.delete_to_line_end.as_ref(),
                vim_normal.delete_to_line_end.as_slice(),
            ),
            (
                keymap.vim_normal.yank_line.as_ref(),
                vim_normal.yank_line.as_slice(),
            ),
            (
                keymap.vim_normal.paste_after.as_ref(),
                vim_normal.paste_after.as_slice(),
            ),
            (
                keymap.vim_normal.start_delete_operator.as_ref(),
                vim_normal.start_delete_operator.as_slice(),
            ),
            (
                keymap.vim_normal.start_yank_operator.as_ref(),
                vim_normal.start_yank_operator.as_slice(),
            ),
            (
                keymap.vim_normal.start_change_operator.as_ref(),
                vim_normal.start_change_operator.as_slice(),
            ),
            (keymap.vim_normal.undo.as_ref(), vim_normal.undo.as_slice()),
            (keymap.vim_normal.redo.as_ref(), vim_normal.redo.as_slice()),
            (
                keymap.vim_normal.cancel_operator.as_ref(),
                vim_normal.cancel_operator.as_slice(),
            ),
        ]);

        if keymap.vim_normal.start_change_operator.is_none() {
            vim_normal
                .start_change_operator
                .retain(|binding| !configured_vim_normal_bindings_to_preserve.contains(binding));
        }
        if keymap.vim_normal.substitute_char.is_none() {
            vim_normal
                .substitute_char
                .retain(|binding| !configured_vim_normal_bindings_to_preserve.contains(binding));
        }
        for (configured, bindings) in [
            (
                keymap.vim_normal.replace_char.as_ref(),
                &mut vim_normal.replace_char,
            ),
            (
                keymap.vim_normal.repeat_last_change.as_ref(),
                &mut vim_normal.repeat_last_change,
            ),
            (
                keymap.vim_normal.find_forward.as_ref(),
                &mut vim_normal.find_forward,
            ),
            (
                keymap.vim_normal.find_backward.as_ref(),
                &mut vim_normal.find_backward,
            ),
            (
                keymap.vim_normal.till_forward.as_ref(),
                &mut vim_normal.till_forward,
            ),
            (
                keymap.vim_normal.till_backward.as_ref(),
                &mut vim_normal.till_backward,
            ),
            (
                keymap.vim_normal.jump_top.as_ref(),
                &mut vim_normal.jump_top,
            ),
            (
                keymap.vim_normal.jump_bottom.as_ref(),
                &mut vim_normal.jump_bottom,
            ),
        ] {
            if configured.is_none() {
                bindings.retain(|binding| {
                    !configured_vim_normal_bindings_to_preserve.contains(binding)
                        && !chords.bindings.iter().any(|chord| {
                            chord.action.context == KeymapContext::VimNormal
                                && chord.chord.prefix.parts() == binding.parts()
                        })
                });
            }
        }
        let mut vim_operator = VimOperatorKeymap {
            delete_line: resolve_local!(keymap, defaults, vim_operator, delete_line),
            yank_line: resolve_local!(keymap, defaults, vim_operator, yank_line),
            motion_left: resolve_local!(keymap, defaults, vim_operator, motion_left),
            motion_right: resolve_local!(keymap, defaults, vim_operator, motion_right),
            motion_up: resolve_local!(keymap, defaults, vim_operator, motion_up),
            motion_down: resolve_local!(keymap, defaults, vim_operator, motion_down),
            motion_word_forward: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                motion_word_forward
            ),
            motion_word_backward: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                motion_word_backward
            ),
            motion_word_end: resolve_local!(keymap, defaults, vim_operator, motion_word_end),
            motion_line_start: resolve_local!(keymap, defaults, vim_operator, motion_line_start),
            motion_line_end: resolve_local!(keymap, defaults, vim_operator, motion_line_end),
            motion_find_forward: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                motion_find_forward
            ),
            motion_find_backward: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                motion_find_backward
            ),
            motion_till_forward: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                motion_till_forward
            ),
            motion_till_backward: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                motion_till_backward
            ),
            motion_jump_top: resolve_local!(keymap, defaults, vim_operator, motion_jump_top),
            motion_jump_bottom: resolve_local!(keymap, defaults, vim_operator, motion_jump_bottom),
            select_inner_text_object: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                select_inner_text_object
            ),
            select_around_text_object: resolve_local!(
                keymap,
                defaults,
                vim_operator,
                select_around_text_object
            ),
            cancel: resolve_local!(keymap, defaults, vim_operator, cancel),
        };

        let configured_vim_operator_bindings_to_preserve = configured_bindings_to_preserve([
            (
                keymap.vim_operator.delete_line.as_ref(),
                vim_operator.delete_line.as_slice(),
            ),
            (
                keymap.vim_operator.yank_line.as_ref(),
                vim_operator.yank_line.as_slice(),
            ),
            (
                keymap.vim_operator.motion_left.as_ref(),
                vim_operator.motion_left.as_slice(),
            ),
            (
                keymap.vim_operator.motion_right.as_ref(),
                vim_operator.motion_right.as_slice(),
            ),
            (
                keymap.vim_operator.motion_up.as_ref(),
                vim_operator.motion_up.as_slice(),
            ),
            (
                keymap.vim_operator.motion_down.as_ref(),
                vim_operator.motion_down.as_slice(),
            ),
            (
                keymap.vim_operator.motion_word_forward.as_ref(),
                vim_operator.motion_word_forward.as_slice(),
            ),
            (
                keymap.vim_operator.motion_word_backward.as_ref(),
                vim_operator.motion_word_backward.as_slice(),
            ),
            (
                keymap.vim_operator.motion_word_end.as_ref(),
                vim_operator.motion_word_end.as_slice(),
            ),
            (
                keymap.vim_operator.motion_line_start.as_ref(),
                vim_operator.motion_line_start.as_slice(),
            ),
            (
                keymap.vim_operator.motion_line_end.as_ref(),
                vim_operator.motion_line_end.as_slice(),
            ),
            (
                keymap.vim_operator.motion_find_forward.as_ref(),
                vim_operator.motion_find_forward.as_slice(),
            ),
            (
                keymap.vim_operator.motion_find_backward.as_ref(),
                vim_operator.motion_find_backward.as_slice(),
            ),
            (
                keymap.vim_operator.motion_till_forward.as_ref(),
                vim_operator.motion_till_forward.as_slice(),
            ),
            (
                keymap.vim_operator.motion_till_backward.as_ref(),
                vim_operator.motion_till_backward.as_slice(),
            ),
            (
                keymap.vim_operator.motion_jump_top.as_ref(),
                vim_operator.motion_jump_top.as_slice(),
            ),
            (
                keymap.vim_operator.motion_jump_bottom.as_ref(),
                vim_operator.motion_jump_bottom.as_slice(),
            ),
            (
                keymap.vim_operator.cancel.as_ref(),
                vim_operator.cancel.as_slice(),
            ),
        ]);

        if keymap.vim_operator.select_inner_text_object.is_none() {
            vim_operator
                .select_inner_text_object
                .retain(|binding| !configured_vim_operator_bindings_to_preserve.contains(binding));
        }
        if keymap.vim_operator.select_around_text_object.is_none() {
            vim_operator
                .select_around_text_object
                .retain(|binding| !configured_vim_operator_bindings_to_preserve.contains(binding));
        }
        for (configured, bindings) in [
            (
                keymap.vim_operator.motion_find_forward.as_ref(),
                &mut vim_operator.motion_find_forward,
            ),
            (
                keymap.vim_operator.motion_find_backward.as_ref(),
                &mut vim_operator.motion_find_backward,
            ),
            (
                keymap.vim_operator.motion_till_forward.as_ref(),
                &mut vim_operator.motion_till_forward,
            ),
            (
                keymap.vim_operator.motion_till_backward.as_ref(),
                &mut vim_operator.motion_till_backward,
            ),
            (
                keymap.vim_operator.motion_jump_top.as_ref(),
                &mut vim_operator.motion_jump_top,
            ),
            (
                keymap.vim_operator.motion_jump_bottom.as_ref(),
                &mut vim_operator.motion_jump_bottom,
            ),
        ] {
            if configured.is_none() {
                bindings.retain(|binding| {
                    !configured_vim_operator_bindings_to_preserve.contains(binding)
                        && !chords.bindings.iter().any(|chord| {
                            chord.action.context == KeymapContext::VimOperator
                                && chord.chord.prefix.parts() == binding.parts()
                        })
                });
            }
        }

        let vim_text_object = VimTextObjectKeymap {
            word: resolve_local!(keymap, defaults, vim_text_object, word),
            big_word: resolve_local!(keymap, defaults, vim_text_object, big_word),
            parentheses: resolve_local!(keymap, defaults, vim_text_object, parentheses),
            brackets: resolve_local!(keymap, defaults, vim_text_object, brackets),
            braces: resolve_local!(keymap, defaults, vim_text_object, braces),
            double_quote: resolve_local!(keymap, defaults, vim_text_object, double_quote),
            single_quote: resolve_local!(keymap, defaults, vim_text_object, single_quote),
            backtick: resolve_local!(keymap, defaults, vim_text_object, backtick),
            cancel: resolve_local!(keymap, defaults, vim_text_object, cancel),
        };

        // New question shortcuts yield individually to existing explicit bindings.
        for (configured, bindings, aliases) in [
            (
                keymap.chat.prompt_stack_back.as_ref(),
                &mut chat.prompt_stack_back,
                vec![
                    ("alt-down", key_hint::alt(KeyCode::Down)),
                    ("shift-right", key_hint::shift(KeyCode::Right)),
                ],
            ),
            (
                keymap.chat.skip_question.as_ref(),
                &mut chat.skip_question,
                vec![("ctrl-]", key_hint::ctrl(KeyCode::Char(']')))],
            ),
        ] {
            if configured.is_none() {
                bindings.retain(|binding| {
                    !aliases.iter().any(|(alias, candidate)| {
                        binding == candidate
                            && (configured_main_surface_alias_is_used(keymap, alias)
                                || configured_context_alias_is_used(&keymap.list, alias)
                                || configured_context_alias_is_used(&keymap.vim_search, alias))
                    }) && !chords.bindings.iter().any(|chord| {
                        chord.action.context.overlaps(KeymapContext::Chat)
                            && binding.normalized_parts() == chord.chord.prefix.normalized_parts()
                    })
                });
            }
        }

        // Reasoning arrow aliases are fallback defaults: existing explicit
        // bindings on the same input path keep the keys, while explicit
        // reasoning bindings remain authoritative.
        if keymap.chat.decrease_reasoning_effort.is_none()
            && configured_main_surface_alias_is_used(keymap, "shift-down")
        {
            chat.decrease_reasoning_effort
                .retain(|binding| *binding != key_hint::shift(KeyCode::Down));
        }
        if keymap.chat.increase_reasoning_effort.is_none()
            && configured_main_surface_alias_is_used(keymap, "shift-up")
        {
            chat.increase_reasoning_effort
                .retain(|binding| *binding != key_hint::shift(KeyCode::Up));
        }

        let pager = PagerKeymap {
            scroll_up: resolve_local!(keymap, defaults, pager, scroll_up),
            scroll_down: resolve_local!(keymap, defaults, pager, scroll_down),
            page_up: resolve_local!(keymap, defaults, pager, page_up),
            page_down: resolve_local!(keymap, defaults, pager, page_down),
            half_page_up: resolve_local!(keymap, defaults, pager, half_page_up),
            half_page_down: resolve_local!(keymap, defaults, pager, half_page_down),
            jump_top: resolve_local!(keymap, defaults, pager, jump_top),
            jump_bottom: resolve_local!(keymap, defaults, pager, jump_bottom),
            close: resolve_local!(keymap, defaults, pager, close),
            close_transcript: resolve_local!(keymap, defaults, pager, close_transcript),
            find: resolve_local!(keymap, defaults, pager, find),
            chord_hints: Arc::clone(&chords),
        };

        let mut agents = AgentsKeymap {
            resume: resolve_local!(keymap, defaults, agents, resume),
            search: resolve_local!(keymap, defaults, agents, search),
            new_task: resolve_local!(keymap, defaults, agents, new_task),
            new_worktree: resolve_local!(keymap, defaults, agents, new_worktree),
            fork: resolve_local!(keymap, defaults, agents, fork),
            rename: resolve_local!(keymap, defaults, agents, rename),
            stop: resolve_local!(keymap, defaults, agents, stop),
            archive: resolve_local!(keymap, defaults, agents, archive),
            delete: resolve_local!(keymap, defaults, agents, delete),
            hide: resolve_local!(keymap, defaults, agents, hide),
            toggle_pin: resolve_local!(keymap, defaults, agents, toggle_pin),
            toggle_grouping: resolve_local!(keymap, defaults, agents, toggle_grouping),
            chord_hints: Arc::clone(&chords),
        };

        // New defaults yield to explicit bindings, including existing list shortcuts.
        for (configured, bindings) in [
            (keymap.agents.resume.as_ref(), &mut agents.resume),
            (keymap.agents.search.as_ref(), &mut agents.search),
            (keymap.agents.new_task.as_ref(), &mut agents.new_task),
            (
                keymap.agents.new_worktree.as_ref(),
                &mut agents.new_worktree,
            ),
            (keymap.agents.fork.as_ref(), &mut agents.fork),
            (keymap.agents.rename.as_ref(), &mut agents.rename),
            (keymap.agents.stop.as_ref(), &mut agents.stop),
            (keymap.agents.archive.as_ref(), &mut agents.archive),
            (keymap.agents.delete.as_ref(), &mut agents.delete),
            (keymap.agents.hide.as_ref(), &mut agents.hide),
            (keymap.agents.toggle_pin.as_ref(), &mut agents.toggle_pin),
            (
                keymap.agents.toggle_grouping.as_ref(),
                &mut agents.toggle_grouping,
            ),
        ] {
            if configured.is_none() {
                bindings.retain(|binding| {
                    !configured_context_binding_is_used(&keymap.agents, *binding)
                        && !configured_context_binding_is_used(&keymap.list, *binding)
                        && !configured_context_binding_is_used(&keymap.global, *binding)
                        && !chords.bindings.iter().any(|chord| {
                            chord.action.context.overlaps(KeymapContext::Agents)
                                && chord.chord.prefix == *binding
                        })
                });
            }
        }

        let approval = ApprovalKeymap {
            open_fullscreen: resolve_local!(keymap, defaults, approval, open_fullscreen),
            open_thread: resolve_local!(keymap, defaults, approval, open_thread),
            approve: resolve_local!(keymap, defaults, approval, approve),
            approve_for_session: resolve_local!(keymap, defaults, approval, approve_for_session),
            approve_for_prefix: resolve_local!(keymap, defaults, approval, approve_for_prefix),
            deny: resolve_local!(keymap, defaults, approval, deny),
            decline: resolve_local!(keymap, defaults, approval, decline),
            cancel: resolve_local!(keymap, defaults, approval, cancel),
            chord_hints: Arc::clone(&chords),
        };

        let list_move_up = resolve_local!(keymap, defaults, list, move_up);
        let list_move_down = resolve_local!(keymap, defaults, list, move_down);
        let list_accept = resolve_local!(keymap, defaults, list, accept);
        let list_cancel = resolve_local!(keymap, defaults, list, cancel);
        let configured_bindings_to_preserve = configured_bindings_to_preserve([
            (
                keymap.global.open_agents.as_ref(),
                app.open_agents.as_slice(),
            ),
            (
                keymap.global.open_transcript.as_ref(),
                app.open_transcript.as_slice(),
            ),
            (
                keymap.global.find_transcript.as_ref(),
                app.find_transcript.as_slice(),
            ),
            (
                keymap.global.select_transcript.as_ref(),
                app.select_transcript.as_slice(),
            ),
            (
                keymap.global.focus_activity.as_ref(),
                app.focus_activity.as_slice(),
            ),
            (
                keymap.global.open_external_editor.as_ref(),
                app.open_external_editor.as_slice(),
            ),
            (keymap.global.copy.as_ref(), app.copy.as_slice()),
            (
                keymap.global.clear_terminal.as_ref(),
                app.clear_terminal.as_slice(),
            ),
            (
                keymap.global.toggle_vim_mode.as_ref(),
                app.toggle_vim_mode.as_slice(),
            ),
            (
                keymap.global.toggle_fast_mode.as_ref(),
                app.toggle_fast_mode.as_slice(),
            ),
            (
                keymap.global.toggle_raw_output.as_ref(),
                app.toggle_raw_output.as_slice(),
            ),
            (
                keymap.global.toggle_side_conversation.as_ref(),
                app.toggle_side_conversation.as_slice(),
            ),
            (keymap.list.move_up.as_ref(), list_move_up.as_slice()),
            (keymap.list.move_down.as_ref(), list_move_down.as_slice()),
            (keymap.list.accept.as_ref(), list_accept.as_slice()),
            (keymap.list.cancel.as_ref(), list_cancel.as_slice()),
            (
                keymap.approval.open_fullscreen.as_ref(),
                approval.open_fullscreen.as_slice(),
            ),
            (
                keymap.approval.open_thread.as_ref(),
                approval.open_thread.as_slice(),
            ),
            (
                keymap.approval.approve.as_ref(),
                approval.approve.as_slice(),
            ),
            (
                keymap.approval.approve_for_session.as_ref(),
                approval.approve_for_session.as_slice(),
            ),
            (
                keymap.approval.approve_for_prefix.as_ref(),
                approval.approve_for_prefix.as_slice(),
            ),
            (keymap.approval.deny.as_ref(), approval.deny.as_slice()),
            (
                keymap.approval.decline.as_ref(),
                approval.decline.as_slice(),
            ),
            (keymap.approval.cancel.as_ref(), approval.cancel.as_slice()),
        ]);

        let list = ListKeymap {
            move_up: list_move_up,
            move_down: list_move_down,
            move_left: resolve_new_default_bindings(
                keymap.list.move_left.as_ref(),
                &defaults.list.move_left,
                &configured_bindings_to_preserve,
                "tui.keymap.list.move_left",
            )?,
            move_right: resolve_new_default_bindings(
                keymap.list.move_right.as_ref(),
                &defaults.list.move_right,
                &configured_bindings_to_preserve,
                "tui.keymap.list.move_right",
            )?,
            page_up: resolve_new_default_bindings(
                keymap.list.page_up.as_ref(),
                &defaults.list.page_up,
                &configured_bindings_to_preserve,
                "tui.keymap.list.page_up",
            )?,
            page_down: resolve_new_default_bindings(
                keymap.list.page_down.as_ref(),
                &defaults.list.page_down,
                &configured_bindings_to_preserve,
                "tui.keymap.list.page_down",
            )?,
            jump_top: resolve_new_default_bindings(
                keymap.list.jump_top.as_ref(),
                &defaults.list.jump_top,
                &configured_bindings_to_preserve,
                "tui.keymap.list.jump_top",
            )?,
            jump_bottom: resolve_new_default_bindings(
                keymap.list.jump_bottom.as_ref(),
                &defaults.list.jump_bottom,
                &configured_bindings_to_preserve,
                "tui.keymap.list.jump_bottom",
            )?,
            accept: list_accept,
            cancel: list_cancel,
            chord_hints: Arc::clone(&chords),
        };

        let mut resolved = Self {
            app,
            chords,
            chat,
            composer,
            editor,
            vim_normal,
            vim_operator,
            vim_search: VimSearchKeymap {
                forward: resolve_local!(keymap, defaults, vim_search, forward),
                backward: resolve_local!(keymap, defaults, vim_search, backward),
                next: resolve_local!(keymap, defaults, vim_search, next),
                previous: resolve_local!(keymap, defaults, vim_search, previous),
            },
            vim_text_object,
            pager,
            list,
            agents,
            approval,
        };

        let configured: Vec<_> = runtime_action_bindings(&resolved)
            .filter(|action| {
                action.id.context.overlaps(KeymapContext::VimNormal)
                    && bindings::configured_binding_for_action(keymap, action.id)
                        .is_some_and(Option::is_some)
            })
            .flat_map(|action| action.bindings.iter().copied())
            .collect();
        for (setting, bindings) in [
            (
                keymap.vim_normal.undo.as_ref(),
                &mut resolved.vim_normal.undo,
            ),
            (
                keymap.vim_normal.redo.as_ref(),
                &mut resolved.vim_normal.redo,
            ),
            (
                keymap.vim_normal.enter_replace_mode.as_ref(),
                &mut resolved.vim_normal.enter_replace_mode,
            ),
        ] {
            if setting.is_none() {
                bindings.retain(|binding| {
                    let (code, modifiers) = binding.parts();
                    let event = KeyEvent::new(code, modifiers);
                    !configured.is_pressed(event)
                        && !resolved.chords.bindings.iter().any(|chord| {
                            chord.action.context.overlaps(KeymapContext::VimNormal)
                                && chord.chord.prefix.is_press(event)
                        })
                });
            }
        }
        resolved.configure_search(keymap)?;
        chords::leader_defaults::register(keymap, &mut resolved, workspace);
        resolved.validate_conflicts()?;
        chords::validate_chord_conflicts(&resolved)?;
        chords::install_dispatch_bindings(&mut resolved)?;
        Ok(resolved)
    }

    /// Resolve the visible primary shortcut from configured declaration order.
    pub(crate) fn primary_hint(
        &self,
        context: KeymapContext,
        action: &'static str,
    ) -> Option<ShortcutHint> {
        let action_id = keymap_action_id(context.config_name(), action)?;
        let bindings = bindings_for_action(self, context.config_name(), action)?;
        self.chords.primary_hint(action_id, bindings)
    }

    /// Built-in keymap defaults.
    ///
    /// Some actions intentionally include compatibility variants (for example
    /// both `?` and `shift-?`) because terminals disagree on whether SHIFT is
    /// preserved for certain printable/control chords.
    fn built_in_defaults() -> Self {
        Self {
            app: AppKeymap {
                change_host: Vec::new(),
                focus_main_composer: Vec::new(),
                focus_sidebar_content: Vec::new(),
                focus_sidebar_tabs: Vec::new(),
                hide_pane: Vec::new(),
                close_sidebar_tab: Vec::new(),
                sidebar_up: Vec::new(),
                sidebar_down: Vec::new(),
                focus_pane_left: Vec::new(),
                focus_pane_right: Vec::new(),
                minimize_pane_width: Vec::new(),
                maximize_pane_width: Vec::new(),
                equalize_pane_widths: Vec::new(),
                resize_panes: Vec::new(),
                move_divider_left: Vec::new(),
                move_divider_right: Vec::new(),
                toggle_pane_zoom: Vec::new(),
                open_agents: default_bindings![],
                open_transcript: default_bindings![ctrl(KeyCode::Char('t'))],
                find_transcript: default_bindings![plain(KeyCode::F(3))],
                select_transcript: default_bindings![],
                focus_activity: default_bindings![plain(KeyCode::F(4))],
                open_warnings: default_bindings![plain(KeyCode::F(2))],
                open_external_editor: default_bindings![ctrl(KeyCode::Char('g'))],
                copy: default_bindings![ctrl(KeyCode::Char('o'))],
                clear_terminal: default_bindings![ctrl(KeyCode::Char('l'))],
                toggle_vim_mode: default_bindings![],
                toggle_fast_mode: default_bindings![],
                toggle_raw_output: default_bindings![alt(KeyCode::Char('r'))],
                toggle_side_conversation: default_bindings![ctrl(KeyCode::Char('/'))],
            },
            chords: Arc::default(),
            chat: ChatKeymap {
                open_presets: Vec::new(),
                toggle_voice: default_bindings![plain(KeyCode::F(8))],
                toggle_voice_mute: default_bindings![plain(KeyCode::F(9))],
                chord_hints: Arc::default(),
                interrupt_turn: default_bindings![plain(KeyCode::Esc)],
                decrease_reasoning_effort: default_bindings![
                    alt(KeyCode::Char(',')),
                    shift(KeyCode::Down)
                ],
                increase_reasoning_effort: default_bindings![
                    alt(KeyCode::Char('.')),
                    shift(KeyCode::Up)
                ],
                previous_permission_mode: default_bindings![],
                next_permission_mode: default_bindings![],
                edit_queued_message: default_bindings![shift(KeyCode::Left), alt(KeyCode::Up)],
                prompt_stack_back: default_bindings![shift(KeyCode::Right), alt(KeyCode::Down)],
                skip_question: default_bindings![ctrl(KeyCode::Char(']'))],
            },
            composer: ComposerKeymap {
                submit: default_bindings![plain(KeyCode::Enter)],
                queue: default_bindings![plain(KeyCode::Tab)],
                toggle_shortcuts: default_bindings![
                    plain(KeyCode::Char('?')),
                    shift(KeyCode::Char('?'))
                ],
                history_search_previous: default_bindings![ctrl(KeyCode::Char('r'))],
                history_search_next: default_bindings![ctrl(KeyCode::Char('s'))],
            },
            editor: Arc::new(EditorKeymap {
                insert_newline: default_bindings![
                    ctrl(KeyCode::Char('j')),
                    ctrl(KeyCode::Char('m')),
                    plain(KeyCode::Enter),
                    shift(KeyCode::Enter),
                    alt(KeyCode::Enter)
                ],
                move_left: default_bindings![plain(KeyCode::Left), ctrl(KeyCode::Char('b'))],
                move_right: default_bindings![plain(KeyCode::Right), ctrl(KeyCode::Char('f'))],
                move_up: default_bindings![plain(KeyCode::Up), ctrl(KeyCode::Char('p'))],
                move_down: default_bindings![plain(KeyCode::Down), ctrl(KeyCode::Char('n'))],
                move_word_left: default_bindings![
                    alt(KeyCode::Char('b')),
                    raw(KeyBinding::new(KeyCode::Left, KeyModifiers::ALT)),
                    raw(KeyBinding::new(KeyCode::Left, KeyModifiers::CONTROL))
                ],
                move_word_right: default_bindings![
                    alt(KeyCode::Char('f')),
                    raw(KeyBinding::new(KeyCode::Right, KeyModifiers::ALT)),
                    raw(KeyBinding::new(KeyCode::Right, KeyModifiers::CONTROL))
                ],
                move_line_start: default_bindings![
                    plain(KeyCode::Home),
                    ctrl(KeyCode::Char('a')),
                    raw(KeyBinding::new(KeyCode::Left, KeyModifiers::SUPER))
                ],
                move_line_end: default_bindings![
                    plain(KeyCode::End),
                    ctrl(KeyCode::Char('e')),
                    raw(KeyBinding::new(KeyCode::Right, KeyModifiers::SUPER))
                ],
                delete_backward: default_bindings![
                    plain(KeyCode::Backspace),
                    shift(KeyCode::Backspace),
                    ctrl(KeyCode::Char('h'))
                ],
                delete_forward: default_bindings![
                    plain(KeyCode::Delete),
                    shift(KeyCode::Delete),
                    ctrl(KeyCode::Char('d'))
                ],
                delete_backward_word: default_bindings![
                    alt(KeyCode::Backspace),
                    ctrl(KeyCode::Backspace),
                    raw(KeyBinding::new(
                        KeyCode::Backspace,
                        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                    )),
                    ctrl(KeyCode::Char('w')),
                    raw(KeyBinding::new(
                        KeyCode::Char('h'),
                        KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ))
                ],
                delete_forward_word: default_bindings![
                    alt(KeyCode::Delete),
                    ctrl(KeyCode::Delete),
                    raw(KeyBinding::new(
                        KeyCode::Delete,
                        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                    )),
                    alt(KeyCode::Char('d'))
                ],
                kill_line_start: default_bindings![
                    ctrl(KeyCode::Char('u')),
                    raw(KeyBinding::new(KeyCode::Backspace, KeyModifiers::SUPER))
                ],
                kill_whole_line: default_bindings![],
                kill_line_end: default_bindings![ctrl(KeyCode::Char('k'))],
                yank: default_bindings![ctrl(KeyCode::Char('y'))],
            }),
            vim_normal: VimNormalKeymap {
                enter_insert: default_bindings![plain(KeyCode::Char('i')), plain(KeyCode::Insert)],
                append_after_cursor: default_bindings![plain(KeyCode::Char('a'))],
                append_line_end: default_bindings![
                    shift(KeyCode::Char('a')),
                    plain(KeyCode::Char('A'))
                ],
                insert_line_start: default_bindings![
                    shift(KeyCode::Char('i')),
                    plain(KeyCode::Char('I'))
                ],
                open_line_below: default_bindings![plain(KeyCode::Char('o'))],
                open_line_above: default_bindings![
                    shift(KeyCode::Char('o')),
                    plain(KeyCode::Char('O'))
                ],
                enter_replace_mode: default_bindings![
                    shift(KeyCode::Char('r')),
                    plain(KeyCode::Char('R'))
                ],
                move_left: default_bindings![plain(KeyCode::Char('h')), plain(KeyCode::Left)],
                move_right: default_bindings![plain(KeyCode::Char('l')), plain(KeyCode::Right)],
                move_up: default_bindings![plain(KeyCode::Char('k')), plain(KeyCode::Up)],
                move_down: default_bindings![plain(KeyCode::Char('j')), plain(KeyCode::Down)],
                move_word_forward: default_bindings![plain(KeyCode::Char('w'))],
                move_word_backward: default_bindings![plain(KeyCode::Char('b'))],
                move_word_end: default_bindings![plain(KeyCode::Char('e'))],
                move_line_start: default_bindings![plain(KeyCode::Char('0'))],
                move_line_end: default_bindings![
                    plain(KeyCode::Char('$')),
                    shift(KeyCode::Char('$'))
                ],
                find_forward: default_bindings![plain(KeyCode::Char('f'))],
                find_backward: default_bindings![shift(KeyCode::Char('f'))],
                till_forward: default_bindings![plain(KeyCode::Char('t'))],
                till_backward: default_bindings![shift(KeyCode::Char('t'))],
                jump_top: default_bindings![],
                jump_bottom: default_bindings![
                    shift(KeyCode::Char('g')),
                    plain(KeyCode::Char('G'))
                ],
                delete_char: default_bindings![plain(KeyCode::Char('x'))],
                replace_char: default_bindings![plain(KeyCode::Char('r'))],
                repeat_last_change: default_bindings![plain(KeyCode::Char('.'))],
                substitute_char: default_bindings![plain(KeyCode::Char('s'))],
                delete_to_line_end: default_bindings![
                    shift(KeyCode::Char('d')),
                    plain(KeyCode::Char('D'))
                ],
                change_to_line_end: default_bindings![
                    shift(KeyCode::Char('c')),
                    plain(KeyCode::Char('C'))
                ],
                yank_line: default_bindings![shift(KeyCode::Char('y')), plain(KeyCode::Char('Y'))],
                paste_after: default_bindings![plain(KeyCode::Char('p'))],
                start_delete_operator: default_bindings![plain(KeyCode::Char('d'))],
                start_yank_operator: default_bindings![plain(KeyCode::Char('y'))],
                start_change_operator: default_bindings![plain(KeyCode::Char('c'))],
                undo: default_bindings![plain(KeyCode::Char('u'))],
                redo: default_bindings![ctrl(KeyCode::Char('r'))],
                cancel_operator: default_bindings![plain(KeyCode::Esc)],
            },
            vim_search: VimSearchKeymap::default(),
            vim_operator: VimOperatorKeymap {
                delete_line: default_bindings![plain(KeyCode::Char('d'))],
                yank_line: default_bindings![plain(KeyCode::Char('y'))],
                motion_left: default_bindings![plain(KeyCode::Char('h'))],
                motion_right: default_bindings![plain(KeyCode::Char('l'))],
                motion_up: default_bindings![plain(KeyCode::Char('k'))],
                motion_down: default_bindings![plain(KeyCode::Char('j'))],
                motion_word_forward: default_bindings![plain(KeyCode::Char('w'))],
                motion_word_backward: default_bindings![plain(KeyCode::Char('b'))],
                motion_word_end: default_bindings![plain(KeyCode::Char('e'))],
                motion_line_start: default_bindings![plain(KeyCode::Char('0'))],
                motion_line_end: default_bindings![
                    plain(KeyCode::Char('$')),
                    shift(KeyCode::Char('$'))
                ],
                motion_find_forward: default_bindings![plain(KeyCode::Char('f'))],
                motion_find_backward: default_bindings![shift(KeyCode::Char('f'))],
                motion_till_forward: default_bindings![plain(KeyCode::Char('t'))],
                motion_till_backward: default_bindings![shift(KeyCode::Char('t'))],
                motion_jump_top: default_bindings![],
                motion_jump_bottom: default_bindings![
                    shift(KeyCode::Char('g')),
                    plain(KeyCode::Char('G'))
                ],
                select_inner_text_object: default_bindings![plain(KeyCode::Char('i'))],
                select_around_text_object: default_bindings![plain(KeyCode::Char('a'))],
                cancel: default_bindings![plain(KeyCode::Esc)],
            },
            vim_text_object: VimTextObjectKeymap {
                word: default_bindings![plain(KeyCode::Char('w'))],
                big_word: default_bindings![shift(KeyCode::Char('w')), plain(KeyCode::Char('W'))],
                parentheses: default_bindings![
                    plain(KeyCode::Char('(')),
                    shift(KeyCode::Char('(')),
                    plain(KeyCode::Char(')')),
                    shift(KeyCode::Char(')')),
                    plain(KeyCode::Char('b'))
                ],
                brackets: default_bindings![plain(KeyCode::Char('[')), plain(KeyCode::Char(']'))],
                braces: default_bindings![
                    plain(KeyCode::Char('{')),
                    shift(KeyCode::Char('{')),
                    plain(KeyCode::Char('}')),
                    shift(KeyCode::Char('}')),
                    shift(KeyCode::Char('b')),
                    plain(KeyCode::Char('B'))
                ],
                double_quote: default_bindings![
                    plain(KeyCode::Char('"')),
                    shift(KeyCode::Char('"'))
                ],
                single_quote: default_bindings![plain(KeyCode::Char('\''))],
                backtick: default_bindings![plain(KeyCode::Char('`'))],
                cancel: default_bindings![plain(KeyCode::Esc)],
            },
            pager: PagerKeymap {
                scroll_up: default_bindings![plain(KeyCode::Up), plain(KeyCode::Char('k'))],
                scroll_down: default_bindings![plain(KeyCode::Down), plain(KeyCode::Char('j'))],
                page_up: default_bindings![
                    plain(KeyCode::PageUp),
                    shift(KeyCode::Char(' ')),
                    ctrl(KeyCode::Char('b'))
                ],
                page_down: default_bindings![
                    plain(KeyCode::PageDown),
                    plain(KeyCode::Char(' ')),
                    ctrl(KeyCode::Char('f'))
                ],
                half_page_up: default_bindings![ctrl(KeyCode::Char('u'))],
                half_page_down: default_bindings![ctrl(KeyCode::Char('d'))],
                jump_top: default_bindings![plain(KeyCode::Home)],
                jump_bottom: default_bindings![plain(KeyCode::End)],
                close: default_bindings![plain(KeyCode::Char('q')), ctrl(KeyCode::Char('c'))],
                close_transcript: default_bindings![ctrl(KeyCode::Char('t'))],
                find: default_bindings![plain(KeyCode::F(3)), plain(KeyCode::Char('/'))],
                chord_hints: Arc::default(),
            },
            list: ListKeymap {
                move_up: default_bindings![
                    plain(KeyCode::Up),
                    ctrl(KeyCode::Char('p')),
                    ctrl(KeyCode::Char('k')),
                    plain(KeyCode::Char('k'))
                ],
                move_down: default_bindings![
                    plain(KeyCode::Down),
                    ctrl(KeyCode::Char('n')),
                    ctrl(KeyCode::Char('j')),
                    plain(KeyCode::Char('j'))
                ],
                move_left: default_bindings![plain(KeyCode::Left), ctrl(KeyCode::Char('h'))],
                move_right: default_bindings![plain(KeyCode::Right), ctrl(KeyCode::Char('l'))],
                page_up: default_bindings![plain(KeyCode::PageUp), ctrl(KeyCode::Char('b'))],
                page_down: default_bindings![plain(KeyCode::PageDown), ctrl(KeyCode::Char('f'))],
                jump_top: default_bindings![plain(KeyCode::Home)],
                jump_bottom: default_bindings![plain(KeyCode::End)],
                accept: default_bindings![plain(KeyCode::Enter)],
                cancel: default_bindings![plain(KeyCode::Esc)],
                chord_hints: Arc::default(),
            },
            agents: AgentsKeymap {
                resume: default_bindings![plain(KeyCode::Char('o'))],
                search: default_bindings![plain(KeyCode::F(3)), plain(KeyCode::Char('/'))],
                new_task: default_bindings![plain(KeyCode::Char('n'))],
                new_worktree: default_bindings![plain(KeyCode::Char('w'))],
                fork: default_bindings![plain(KeyCode::Char('f'))],
                rename: default_bindings![plain(KeyCode::Char('r'))],
                stop: default_bindings![plain(KeyCode::Char('x'))],
                archive: default_bindings![plain(KeyCode::Char('a'))],
                delete: default_bindings![plain(KeyCode::Backspace)],
                hide: default_bindings![plain(KeyCode::Char('h'))],
                toggle_pin: default_bindings![plain(KeyCode::Char('p'))],
                toggle_grouping: default_bindings![plain(KeyCode::Char('g'))],
                chord_hints: Arc::default(),
            },
            approval: ApprovalKeymap {
                open_fullscreen: default_bindings![
                    ctrl(KeyCode::Char('a')),
                    raw(KeyBinding::new(
                        KeyCode::Char('a'),
                        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                    ))
                ],
                open_thread: default_bindings![plain(KeyCode::Char('o'))],
                approve: default_bindings![plain(KeyCode::Char('y'))],
                approve_for_session: default_bindings![plain(KeyCode::Char('a'))],
                approve_for_prefix: default_bindings![plain(KeyCode::Char('p'))],
                deny: default_bindings![plain(KeyCode::Char('d'))],
                decline: default_bindings![plain(KeyCode::Esc), plain(KeyCode::Char('n'))],
                cancel: default_bindings![plain(KeyCode::Char('c'))],
                chord_hints: Arc::default(),
            },
        }
    }

    /// Reject ambiguous bindings in scopes that are evaluated together.
    ///
    /// We validate in multiple passes because runtime handling has mixed
    /// precedence:
    ///
    /// 1. `app` actions can shadow composer actions because app checks run
    ///    before forwarding to the composer.
    /// 2. Contexts with hard-coded sequence behavior, such as edit-previous
    ///    backtracking, intentionally stay outside this configurable keymap.
    fn validate_conflicts(&self) -> Result<(), String> {
        for (action, bindings) in [
            ("global.change_host", &self.app.change_host),
            ("chat.open_presets", &self.chat.open_presets),
            ("chat.toggle_voice", &self.chat.toggle_voice),
            (
                "chat.previous_permission_mode",
                &self.chat.previous_permission_mode,
            ),
            ("chat.next_permission_mode", &self.chat.next_permission_mode),
            ("chat.prompt_stack_back", &self.chat.prompt_stack_back),
            ("chat.skip_question", &self.chat.skip_question),
        ] {
            if bindings.iter().any(|binding| {
                let (code, modifiers) = binding.parts();
                crate::key_hint::is_plain_text_key_event(KeyEvent::new(code, modifiers))
                    || matches!(code, KeyCode::Char(_)) && crate::key_hint::is_altgr(modifiers)
            }) {
                return Err(format!(
                    "tui.keymap.{action}: printable keys are reserved for text input"
                ));
            }
        }
        let minimize = &self.app.minimize_pane_width;
        let maximize = &self.app.maximize_pane_width;
        let equalize = &self.app.equalize_pane_widths;
        let resize = &self.app.resize_panes;
        #[cfg(unix)]
        for (action, bindings) in [
            ("global.change_host", &self.app.change_host),
            ("global.open_agents", &self.app.open_agents),
            ("global.focus_main_composer", &self.app.focus_main_composer),
            (
                "global.focus_sidebar_content",
                &self.app.focus_sidebar_content,
            ),
            ("global.focus_sidebar_tabs", &self.app.focus_sidebar_tabs),
            ("global.focus_pane_left", &self.app.focus_pane_left),
            ("global.focus_pane_right", &self.app.focus_pane_right),
            ("global.minimize_pane_width", minimize),
            ("global.maximize_pane_width", maximize),
            ("global.equalize_pane_widths", equalize),
            ("global.resize_panes", resize),
            ("global.move_divider_left", &self.app.move_divider_left),
            ("global.move_divider_right", &self.app.move_divider_right),
            ("global.toggle_pane_zoom", &self.app.toggle_pane_zoom),
            ("global.hide_pane", &self.app.hide_pane),
            ("global.close_sidebar_tab", &self.app.close_sidebar_tab),
            ("global.sidebar_up", &self.app.sidebar_up),
            ("global.sidebar_down", &self.app.sidebar_down),
            ("chat.open_presets", &self.chat.open_presets),
            ("chat.toggle_voice", &self.chat.toggle_voice),
        ] {
            if bindings.contains(&key_hint::ctrl(KeyCode::Char('z'))) {
                return Err(format!(
                    "tui.keymap.{action}: ctrl-z is reserved for suspend"
                ));
            }
        }
        if self.app.open_agents.iter().any(|binding| {
            matches!(binding.parts(), (KeyCode::Char(_), modifiers)
                if crate::key_hint::is_altgr(modifiers))
        }) {
            return Err(
                "tui.keymap.global.open_agents: AltGr characters are reserved for text input"
                    .to_string(),
            );
        }
        let mut side_toggle_bindings = self.app.toggle_side_conversation.clone();
        let slash_binding = key_hint::ctrl(KeyCode::Char('/'));
        let legacy_slash_binding = key_hint::ctrl(KeyCode::Char('7'));
        if side_toggle_bindings.contains(&slash_binding)
            && !side_toggle_bindings.contains(&legacy_slash_binding)
        {
            side_toggle_bindings.push(legacy_slash_binding);
        }

        let divider_left = self.app.move_divider_left.as_slice();
        let divider_right = self.app.move_divider_right.as_slice();
        let main_bindings = [
            ("change_host", self.app.change_host.as_slice()),
            (
                "focus_main_composer",
                self.app.focus_main_composer.as_slice(),
            ),
            (
                "focus_sidebar_content",
                self.app.focus_sidebar_content.as_slice(),
            ),
            ("focus_sidebar_tabs", self.app.focus_sidebar_tabs.as_slice()),
            ("focus_pane_left", self.app.focus_pane_left.as_slice()),
            ("focus_pane_right", self.app.focus_pane_right.as_slice()),
            ("minimize_pane_width", minimize),
            ("maximize_pane_width", maximize),
            ("equalize_pane_widths", equalize),
            ("resize_panes", resize),
            ("move_divider_left", divider_left),
            ("move_divider_right", divider_right),
            ("toggle_pane_zoom", self.app.toggle_pane_zoom.as_slice()),
            ("hide_pane", self.app.hide_pane.as_slice()),
            ("close_sidebar_tab", self.app.close_sidebar_tab.as_slice()),
            ("sidebar_up", self.app.sidebar_up.as_slice()),
            ("sidebar_down", self.app.sidebar_down.as_slice()),
            ("open_agents", self.app.open_agents.as_slice()),
            ("open_transcript", self.app.open_transcript.as_slice()),
            ("find_transcript", self.app.find_transcript.as_slice()),
            ("select_transcript", self.app.select_transcript.as_slice()),
            ("focus_activity", self.app.focus_activity.as_slice()),
            ("open_warnings", self.app.open_warnings.as_slice()),
            (
                "open_external_editor",
                self.app.open_external_editor.as_slice(),
            ),
            ("copy", self.app.copy.as_slice()),
            ("clear_terminal", self.app.clear_terminal.as_slice()),
            ("toggle_vim_mode", self.app.toggle_vim_mode.as_slice()),
            ("toggle_fast_mode", self.app.toggle_fast_mode.as_slice()),
            ("toggle_raw_output", self.app.toggle_raw_output.as_slice()),
            ("toggle_side_conversation", side_toggle_bindings.as_slice()),
            ("chat.open_presets", self.chat.open_presets.as_slice()),
            ("chat.toggle_voice", self.chat.toggle_voice.as_slice()),
            (
                "chat.toggle_voice_mute",
                self.chat.toggle_voice_mute.as_slice(),
            ),
            ("chat.interrupt_turn", self.chat.interrupt_turn.as_slice()),
            (
                "chat.decrease_reasoning_effort",
                self.chat.decrease_reasoning_effort.as_slice(),
            ),
            (
                "chat.increase_reasoning_effort",
                self.chat.increase_reasoning_effort.as_slice(),
            ),
            (
                "chat.previous_permission_mode",
                self.chat.previous_permission_mode.as_slice(),
            ),
            (
                "chat.next_permission_mode",
                self.chat.next_permission_mode.as_slice(),
            ),
            (
                "chat.edit_queued_message",
                self.chat.edit_queued_message.as_slice(),
            ),
            (
                "chat.prompt_stack_back",
                self.chat.prompt_stack_back.as_slice(),
            ),
            ("chat.skip_question", self.chat.skip_question.as_slice()),
            ("composer.submit", self.composer.submit.as_slice()),
            ("composer.queue", self.composer.queue.as_slice()),
            (
                "composer.toggle_shortcuts",
                self.composer.toggle_shortcuts.as_slice(),
            ),
            (
                "composer.history_search_previous",
                self.composer.history_search_previous.as_slice(),
            ),
            (
                "composer.history_search_next",
                self.composer.history_search_next.as_slice(),
            ),
        ];
        validate_unique("app", main_bindings)?;

        validate_no_reserved(
            "main",
            main_bindings,
            MAIN_RESERVED_BINDINGS,
            [(
                "chat.interrupt_turn",
                "fixed.backtrack",
                key_hint::plain(KeyCode::Esc),
            )],
        )?;

        let approval_overlay_bindings = [
            ("list.move_up", self.list.move_up.as_slice()),
            ("list.move_down", self.list.move_down.as_slice()),
            ("list.move_left", self.list.move_left.as_slice()),
            ("list.move_right", self.list.move_right.as_slice()),
            ("list.page_up", self.list.page_up.as_slice()),
            ("list.page_down", self.list.page_down.as_slice()),
            ("list.jump_top", self.list.jump_top.as_slice()),
            ("list.jump_bottom", self.list.jump_bottom.as_slice()),
            ("list.accept", self.list.accept.as_slice()),
            ("list.cancel", self.list.cancel.as_slice()),
            (
                "approval.open_fullscreen",
                self.approval.open_fullscreen.as_slice(),
            ),
            ("approval.open_thread", self.approval.open_thread.as_slice()),
            ("approval.approve", self.approval.approve.as_slice()),
            (
                "approval.approve_for_session",
                self.approval.approve_for_session.as_slice(),
            ),
            (
                "approval.approve_for_prefix",
                self.approval.approve_for_prefix.as_slice(),
            ),
            ("approval.deny", self.approval.deny.as_slice()),
            ("approval.decline", self.approval.decline.as_slice()),
            ("approval.cancel", self.approval.cancel.as_slice()),
        ];
        validate_no_shadow_with_allowed_overlaps(
            "app",
            [
                ("open_agents", self.app.open_agents.as_slice()),
                ("open_transcript", self.app.open_transcript.as_slice()),
                ("find_transcript", self.app.find_transcript.as_slice()),
                ("select_transcript", self.app.select_transcript.as_slice()),
                ("focus_activity", self.app.focus_activity.as_slice()),
                ("open_warnings", self.app.open_warnings.as_slice()),
                (
                    "open_external_editor",
                    self.app.open_external_editor.as_slice(),
                ),
                ("copy", self.app.copy.as_slice()),
                ("clear_terminal", self.app.clear_terminal.as_slice()),
                ("toggle_vim_mode", self.app.toggle_vim_mode.as_slice()),
                ("toggle_fast_mode", self.app.toggle_fast_mode.as_slice()),
                ("toggle_raw_output", self.app.toggle_raw_output.as_slice()),
                ("toggle_side_conversation", side_toggle_bindings.as_slice()),
            ],
            approval_overlay_bindings,
            [(
                "clear_terminal",
                "list.move_right",
                key_hint::ctrl(KeyCode::Char('l')),
            )],
        )?;

        // The request-user-input overlay consumes turn interruption before
        // configurable question navigation reaches its list handler.
        validate_no_shadow_with_allowed_overlaps(
            "request_user_input",
            [("chat.interrupt_turn", self.chat.interrupt_turn.as_slice())],
            [
                ("list.move_left", self.list.move_left.as_slice()),
                ("list.move_right", self.list.move_right.as_slice()),
            ],
            [],
        )?;

        // Async question shortcuts run before option selection and text editing.
        validate_no_shadow_with_allowed_overlaps(
            "async_questions",
            [
                (
                    "chat.prompt_stack_back",
                    self.chat.prompt_stack_back.as_slice(),
                ),
                ("chat.skip_question", self.chat.skip_question.as_slice()),
            ],
            [
                ("list.move_up", self.list.move_up.as_slice()),
                ("list.move_down", self.list.move_down.as_slice()),
                ("list.accept", self.list.accept.as_slice()),
            ],
            [],
        )?;

        // While the composer is focused, these main-surface handlers always
        // consume matching keys before the event reaches the textarea editor.
        validate_no_shadow_with_allowed_overlaps(
            "main",
            [
                (
                    "focus_main_composer",
                    self.app.focus_main_composer.as_slice(),
                ),
                (
                    "focus_sidebar_content",
                    self.app.focus_sidebar_content.as_slice(),
                ),
                ("focus_sidebar_tabs", self.app.focus_sidebar_tabs.as_slice()),
                ("focus_pane_left", self.app.focus_pane_left.as_slice()),
                ("focus_pane_right", self.app.focus_pane_right.as_slice()),
                ("minimize_pane_width", minimize),
                ("maximize_pane_width", maximize),
                ("equalize_pane_widths", equalize),
                ("resize_panes", resize),
                ("move_divider_left", divider_left),
                ("move_divider_right", divider_right),
                ("toggle_pane_zoom", self.app.toggle_pane_zoom.as_slice()),
                ("hide_pane", self.app.hide_pane.as_slice()),
                ("close_sidebar_tab", self.app.close_sidebar_tab.as_slice()),
                ("sidebar_up", self.app.sidebar_up.as_slice()),
                ("sidebar_down", self.app.sidebar_down.as_slice()),
                ("open_agents", self.app.open_agents.as_slice()),
                ("open_transcript", self.app.open_transcript.as_slice()),
                ("find_transcript", self.app.find_transcript.as_slice()),
                ("select_transcript", self.app.select_transcript.as_slice()),
                ("focus_activity", self.app.focus_activity.as_slice()),
                ("open_warnings", self.app.open_warnings.as_slice()),
                (
                    "open_external_editor",
                    self.app.open_external_editor.as_slice(),
                ),
                ("copy", self.app.copy.as_slice()),
                ("clear_terminal", self.app.clear_terminal.as_slice()),
                ("chat.open_presets", self.chat.open_presets.as_slice()),
                ("chat.toggle_voice", self.chat.toggle_voice.as_slice()),
                (
                    "chat.toggle_voice_mute",
                    self.chat.toggle_voice_mute.as_slice(),
                ),
                ("chat.interrupt_turn", self.chat.interrupt_turn.as_slice()),
                (
                    "chat.decrease_reasoning_effort",
                    self.chat.decrease_reasoning_effort.as_slice(),
                ),
                (
                    "chat.increase_reasoning_effort",
                    self.chat.increase_reasoning_effort.as_slice(),
                ),
                (
                    "chat.previous_permission_mode",
                    self.chat.previous_permission_mode.as_slice(),
                ),
                (
                    "chat.next_permission_mode",
                    self.chat.next_permission_mode.as_slice(),
                ),
                (
                    "chat.prompt_stack_back",
                    self.chat.prompt_stack_back.as_slice(),
                ),
                ("chat.skip_question", self.chat.skip_question.as_slice()),
                ("composer.submit", self.composer.submit.as_slice()),
                ("toggle_vim_mode", self.app.toggle_vim_mode.as_slice()),
                ("toggle_fast_mode", self.app.toggle_fast_mode.as_slice()),
                ("toggle_raw_output", self.app.toggle_raw_output.as_slice()),
                ("toggle_side_conversation", side_toggle_bindings.as_slice()),
                (
                    "composer.history_search_previous",
                    self.composer.history_search_previous.as_slice(),
                ),
            ],
            [
                (
                    "editor.insert_newline",
                    self.editor.insert_newline.as_slice(),
                ),
                ("editor.move_left", self.editor.move_left.as_slice()),
                ("editor.move_right", self.editor.move_right.as_slice()),
                ("editor.move_up", self.editor.move_up.as_slice()),
                ("editor.move_down", self.editor.move_down.as_slice()),
                (
                    "editor.move_word_left",
                    self.editor.move_word_left.as_slice(),
                ),
                (
                    "editor.move_word_right",
                    self.editor.move_word_right.as_slice(),
                ),
                (
                    "editor.move_line_start",
                    self.editor.move_line_start.as_slice(),
                ),
                ("editor.move_line_end", self.editor.move_line_end.as_slice()),
                (
                    "editor.delete_backward",
                    self.editor.delete_backward.as_slice(),
                ),
                (
                    "editor.delete_forward",
                    self.editor.delete_forward.as_slice(),
                ),
                (
                    "editor.delete_backward_word",
                    self.editor.delete_backward_word.as_slice(),
                ),
                (
                    "editor.delete_forward_word",
                    self.editor.delete_forward_word.as_slice(),
                ),
                (
                    "editor.kill_line_start",
                    self.editor.kill_line_start.as_slice(),
                ),
                (
                    "editor.kill_whole_line",
                    self.editor.kill_whole_line.as_slice(),
                ),
                ("editor.kill_line_end", self.editor.kill_line_end.as_slice()),
                ("editor.yank", self.editor.yank.as_slice()),
            ],
            [(
                "composer.submit",
                "editor.insert_newline",
                key_hint::plain(KeyCode::Enter),
            )],
        )?;

        let context_bindings = |context| {
            runtime_action_bindings(self)
                .filter(move |binding| binding.id.context == context)
                .map(|binding| (binding.id.action, binding.bindings))
        };
        for context in [
            KeymapContext::Editor,
            KeymapContext::VimNormal,
            KeymapContext::VimOperator,
            KeymapContext::VimTextObject,
        ] {
            validate_unique(context.config_name(), context_bindings(context))?;
        }

        validate_unique("pager", context_bindings(KeymapContext::Pager))?;

        validate_unique(
            "transcript",
            [
                (
                    "global.focus_main_composer",
                    self.app.focus_main_composer.as_slice(),
                ),
                (
                    "global.focus_sidebar_content",
                    self.app.focus_sidebar_content.as_slice(),
                ),
                (
                    "global.focus_sidebar_tabs",
                    self.app.focus_sidebar_tabs.as_slice(),
                ),
                (
                    "global.focus_pane_left",
                    self.app.focus_pane_left.as_slice(),
                ),
                (
                    "global.focus_pane_right",
                    self.app.focus_pane_right.as_slice(),
                ),
                ("global.minimize_pane_width", minimize),
                ("global.maximize_pane_width", maximize),
                ("global.equalize_pane_widths", equalize),
                ("global.resize_panes", resize),
                ("global.move_divider_left", divider_left),
                ("global.move_divider_right", divider_right),
                (
                    "global.toggle_pane_zoom",
                    self.app.toggle_pane_zoom.as_slice(),
                ),
                ("global.hide_pane", self.app.hide_pane.as_slice()),
                (
                    "global.close_sidebar_tab",
                    self.app.close_sidebar_tab.as_slice(),
                ),
                ("global.sidebar_up", self.app.sidebar_up.as_slice()),
                ("global.sidebar_down", self.app.sidebar_down.as_slice()),
                (
                    "global.select_transcript",
                    self.app.select_transcript.as_slice(),
                ),
                ("pager.scroll_up", self.pager.scroll_up.as_slice()),
                ("pager.scroll_down", self.pager.scroll_down.as_slice()),
                ("pager.page_up", self.pager.page_up.as_slice()),
                ("pager.page_down", self.pager.page_down.as_slice()),
                ("pager.half_page_up", self.pager.half_page_up.as_slice()),
                ("pager.half_page_down", self.pager.half_page_down.as_slice()),
                ("pager.jump_top", self.pager.jump_top.as_slice()),
                ("pager.jump_bottom", self.pager.jump_bottom.as_slice()),
                ("pager.close", self.pager.close.as_slice()),
                (
                    "pager.close_transcript",
                    self.pager.close_transcript.as_slice(),
                ),
                ("pager.find", self.pager.find.as_slice()),
            ],
        )?;

        validate_no_reserved(
            "pager",
            context_bindings(KeymapContext::Pager),
            TRANSCRIPT_BACKTRACK_RESERVED_BINDINGS,
            [],
        )?;

        validate_unique("list", context_bindings(KeymapContext::List))?;
        validate_unique(
            "activity",
            context_bindings(KeymapContext::List).chain([
                (
                    "global.focus_main_composer",
                    self.app.focus_main_composer.as_slice(),
                ),
                (
                    "global.focus_sidebar_content",
                    self.app.focus_sidebar_content.as_slice(),
                ),
                (
                    "global.focus_sidebar_tabs",
                    self.app.focus_sidebar_tabs.as_slice(),
                ),
                (
                    "global.focus_pane_left",
                    self.app.focus_pane_left.as_slice(),
                ),
                (
                    "global.focus_pane_right",
                    self.app.focus_pane_right.as_slice(),
                ),
                ("global.minimize_pane_width", minimize),
                ("global.maximize_pane_width", maximize),
                ("global.equalize_pane_widths", equalize),
                ("global.resize_panes", resize),
                ("global.move_divider_left", divider_left),
                ("global.move_divider_right", divider_right),
                (
                    "global.toggle_pane_zoom",
                    self.app.toggle_pane_zoom.as_slice(),
                ),
                ("global.hide_pane", self.app.hide_pane.as_slice()),
                (
                    "global.close_sidebar_tab",
                    self.app.close_sidebar_tab.as_slice(),
                ),
                ("global.sidebar_up", self.app.sidebar_up.as_slice()),
                ("global.sidebar_down", self.app.sidebar_down.as_slice()),
                ("global.focus_activity", self.app.focus_activity.as_slice()),
                (
                    "global.find_transcript",
                    self.app.find_transcript.as_slice(),
                ),
                (
                    "global.select_transcript",
                    self.app.select_transcript.as_slice(),
                ),
            ]),
        )?;

        validate_unique("agents", context_bindings(KeymapContext::Agents))?;
        validate_no_reserved(
            "agents",
            context_bindings(KeymapContext::Agents),
            MAIN_RESERVED_BINDINGS,
            [(
                "search",
                "fixed.slash_command",
                key_hint::plain(KeyCode::Char('/')),
            )],
        )?;
        for (action, bindings) in context_bindings(KeymapContext::Agents) {
            #[cfg(unix)]
            if bindings.contains(&key_hint::ctrl(KeyCode::Char('z'))) {
                return Err(format!(
                    "tui.keymap.agents.{action}: ctrl-z is reserved for suspend"
                ));
            }
            if bindings.iter().any(|binding| {
                let (code, modifiers) = binding.normalized_parts();
                (action != "delete"
                    && code == KeyCode::Backspace
                    && modifiers == KeyModifiers::NONE)
                    || (matches!(code, KeyCode::Char(_)) && crate::key_hint::is_altgr(modifiers))
            }) {
                return Err(format!(
                    "tui.keymap.agents.{action}: AltGr and backspace are reserved for editing"
                ));
            }
        }

        validate_unique("approval", context_bindings(KeymapContext::Approval))?;

        let mut seen: HashMap<(KeyCode, KeyModifiers), &'static str> = HashMap::new();
        for (action, bindings) in approval_overlay_bindings {
            for binding in bindings {
                let key = binding.normalized_parts();
                if let Some(previous) = seen.insert(key, action) {
                    // Approval overlays intentionally reserve Esc as a stable
                    // cancellation path even though decline options may also
                    // display it in contexts where that is safe.
                    if previous == "list.cancel"
                        && action == "approval.decline"
                        && key == (KeyCode::Esc, KeyModifiers::NONE)
                    {
                        continue;
                    }
                    return Err(format!(
                        "Ambiguous approval overlay keymap bindings: `{previous}` and `{action}` use the same key. \
Set unique keys in `~/.codex/config.toml` and retry. \
See the Codex keymap documentation for supported actions and examples."
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Reject duplicate keys inside one effective context map.
///
/// This intentionally allows the same key across different contexts; handlers
/// only evaluate one context at a time.
fn validate_unique<'a>(
    context: &str,
    pairs: impl IntoIterator<Item = (&'static str, &'a [KeyBinding])>,
) -> Result<(), String> {
    let mut seen: HashMap<(KeyCode, KeyModifiers), &'static str> = HashMap::new();
    for (action, bindings) in pairs {
        for binding in bindings {
            let key = binding.normalized_parts();
            if let Some(previous) = seen.insert(key, action) {
                return Err(format!(
                    "Ambiguous `tui.keymap.{context}` bindings: `{previous}` and `{action}` use the same key. \
Set unique keys in `~/.codex/config.toml` and retry. \
See the Codex keymap documentation for supported actions and examples."
                ));
            }
        }
    }
    Ok(())
}

fn validate_no_shadow_with_allowed_overlaps<const N: usize, const M: usize, const A: usize>(
    context: &str,
    primary: [(&'static str, &[KeyBinding]); N],
    shadowed: [(&'static str, &[KeyBinding]); M],
    allowed_overlaps: [(&'static str, &'static str, KeyBinding); A],
) -> Result<(), String> {
    let mut seen: HashMap<(KeyCode, KeyModifiers), &'static str> = HashMap::new();
    for (action, bindings) in primary {
        for binding in bindings {
            seen.insert(binding.normalized_parts(), action);
        }
    }
    for (action, bindings) in shadowed {
        for binding in bindings {
            let key = binding.normalized_parts();
            if let Some(previous) = seen.get(&key) {
                if allowed_overlaps.iter().any(
                    |(allowed_primary, allowed_shadowed, allowed_binding)| {
                        *allowed_primary == *previous
                            && *allowed_shadowed == action
                            && allowed_binding.normalized_parts() == key
                    },
                ) {
                    continue;
                }
                return Err(format!(
                    "Ambiguous `tui.keymap.{context}` bindings: `{previous}` shadows `{action}` with the same key. \
Set unique keys in `~/.codex/config.toml` and retry. \
See the Codex keymap documentation for supported actions and examples."
                ));
            }
        }
    }
    Ok(())
}

fn validate_no_reserved<'a, const A: usize>(
    context: &str,
    pairs: impl IntoIterator<Item = (&'static str, &'a [KeyBinding])>,
    reserved: &[(&'static str, KeyBinding)],
    allowed_overlaps: [(&'static str, &'static str, KeyBinding); A],
) -> Result<(), String> {
    for (action, bindings) in pairs {
        for binding in bindings {
            let key = binding.parts();
            if let Some((reserved_action, _)) = reserved
                .iter()
                .find(|(_, reserved_binding)| reserved_binding.parts() == key)
            {
                if allowed_overlaps.iter().any(
                    |(allowed_action, allowed_reserved_action, allowed_binding)| {
                        *allowed_action == action
                            && *allowed_reserved_action == *reserved_action
                            && allowed_binding.parts() == key
                    },
                ) {
                    continue;
                }
                return Err(format!(
                    "Ambiguous `tui.keymap.{context}` bindings: `{action}` uses a key reserved by `{reserved_action}`. \
Set a different key in `~/.codex/config.toml` and retry. \
See the Codex keymap documentation for supported actions and examples."
                ));
            }
        }
    }
    Ok(())
}

const MAIN_RESERVED_BINDINGS: &[(&str, KeyBinding)] = &[
    (
        "fixed.interrupt_or_quit",
        key_hint::ctrl(KeyCode::Char('c')),
    ),
    ("fixed.quit", key_hint::ctrl(KeyCode::Char('d'))),
    ("fixed.paste_image", key_hint::ctrl(KeyCode::Char('v'))),
    ("fixed.paste_image", key_hint::ctrl_alt(KeyCode::Char('v'))),
    (
        "fixed.cycle_collaboration_mode",
        key_hint::shift(KeyCode::Tab),
    ),
    ("fixed.backtrack", key_hint::plain(KeyCode::Esc)),
    ("fixed.previous_agent", key_hint::alt(KeyCode::Left)),
    ("fixed.next_agent", key_hint::alt(KeyCode::Right)),
    ("fixed.slash_command", key_hint::plain(KeyCode::Char('/'))),
    ("fixed.shell_command", key_hint::plain(KeyCode::Char('!'))),
    ("fixed.file_paths", key_hint::plain(KeyCode::Char('@'))),
    (
        "fixed.connector_mentions",
        key_hint::plain(KeyCode::Char('$')),
    ),
];

const TRANSCRIPT_BACKTRACK_RESERVED_BINDINGS: &[(&str, KeyBinding)] = &[
    (
        "fixed.transcript_edit_previous",
        key_hint::plain(KeyCode::Esc),
    ),
    (
        "fixed.transcript_edit_previous",
        key_hint::plain(KeyCode::Left),
    ),
    (
        "fixed.transcript_edit_next",
        key_hint::plain(KeyCode::Right),
    ),
    (
        "fixed.transcript_confirm_edit",
        key_hint::plain(KeyCode::Enter),
    ),
];

/// Resolve one action with context -> global -> default precedence.
///
/// `path` should be the context-specific config path so parser errors point
/// users at the override they attempted to set.
///
/// A configured empty list is authoritative: it returns an empty binding set
/// and does not continue to the global or built-in fallback. This is what makes
/// explicit unbinding work for globally reusable actions like composer submit.
fn resolve_bindings_with_global_fallback(
    configured: Option<&KeybindingsSpec>,
    global: Option<&KeybindingsSpec>,
    fallback: &[KeyBinding],
    path: &str,
) -> Result<Vec<KeyBinding>, String> {
    if let Some(configured) = configured {
        return parse_bindings(configured, path);
    }
    if let Some(global) = global {
        return parse_bindings(global, path);
    }
    Ok(fallback.to_vec())
}

/// Resolve one action binding in a context without global fallback.
///
/// Missing values inherit from the built-in fallback; configured values, including
/// empty lists, replace that fallback for the action.
fn resolve_bindings(
    configured: Option<&KeybindingsSpec>,
    fallback: &[KeyBinding],
    path: &str,
) -> Result<Vec<KeyBinding>, String> {
    let Some(spec) = configured else {
        return Ok(fallback.to_vec());
    };
    parse_bindings(spec, path)
}

fn configured_bindings_to_preserve<const N: usize>(
    pairs: [(Option<&KeybindingsSpec>, &[KeyBinding]); N],
) -> Vec<KeyBinding> {
    let mut configured_bindings = Vec::new();
    for (configured, resolved) in pairs {
        if configured.is_none() {
            continue;
        }
        for binding in resolved {
            if !configured_bindings.contains(binding) {
                configured_bindings.push(*binding);
            }
        }
    }
    configured_bindings
}

fn configured_main_surface_alias_is_used(keymap: &TuiKeymap, alias: &str) -> bool {
    let mut global = keymap.global.clone();
    if keymap.composer.submit.is_some() {
        global.submit = None;
    }
    if keymap.composer.queue.is_some() {
        global.queue = None;
    }
    if keymap.composer.toggle_shortcuts.is_some() {
        global.toggle_shortcuts = None;
    }

    // Reasoning shortcuts run before composer/editor key handling, so fallback
    // aliases must yield to any explicit binding on the same main-surface input
    // path.
    configured_context_alias_is_used(&global, alias)
        || configured_context_alias_is_used(&keymap.chat, alias)
        || configured_context_alias_is_used(&keymap.composer, alias)
        || configured_context_alias_is_used(&keymap.editor, alias)
        || configured_context_alias_is_used(&keymap.vim_normal, alias)
        || configured_context_alias_is_used(&keymap.vim_operator, alias)
        || configured_context_alias_is_used(&keymap.vim_text_object, alias)
}

fn configured_context_alias_is_used(context: &impl Serialize, alias: &str) -> bool {
    parse_keybinding(alias)
        .is_some_and(|binding| configured_context_binding_is_used(context, binding))
}

fn configured_context_binding_is_used(context: &impl Serialize, binding: KeyBinding) -> bool {
    let Ok(value) = serde_json::to_value(context) else {
        return false;
    };
    keymap_value_contains_binding(&value, binding)
}

fn keymap_value_contains_binding(value: &serde_json::Value, binding: KeyBinding) -> bool {
    match value {
        serde_json::Value::String(value) => parse_keybinding(value)
            .is_some_and(|configured| configured.normalized_parts() == binding.normalized_parts()),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| keymap_value_contains_binding(value, binding)),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| keymap_value_contains_binding(value, binding)),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) | serde_json::Value::Null => {
            false
        }
    }
}

fn resolve_new_default_bindings(
    configured: Option<&KeybindingsSpec>,
    fallback: &[KeyBinding],
    configured_bindings_to_preserve: &[KeyBinding],
    path: &str,
) -> Result<Vec<KeyBinding>, String> {
    let Some(spec) = configured else {
        return Ok(fallback
            .iter()
            .copied()
            .filter(|binding| !configured_bindings_to_preserve.contains(binding))
            .collect());
    };
    parse_bindings(spec, path)
}

/// Parse one keybinding value (`string` or `list[string]`) into concrete bindings.
///
/// Duplicate entries are de-duplicated while preserving first-seen order so the
/// first key can remain the primary UI hint.
fn parse_bindings(spec: &KeybindingsSpec, path: &str) -> Result<Vec<KeyBinding>, String> {
    let mut parsed = Vec::new();
    for raw in spec.specs() {
        if raw.as_str().contains(' ') {
            continue;
        }
        let binding = parse_keybinding(raw.as_str()).ok_or_else(|| {
            format!(
                "Invalid `{path}` = `{}`. Use values like `ctrl-a`, `shift-enter`, or `page-down`. \
See the Codex keymap documentation for supported actions and examples.",
                raw.as_str()
            )
        })?;

        if !parsed
            .iter()
            .any(|previous: &KeyBinding| previous.normalized_parts() == binding.normalized_parts())
        {
            parsed.push(binding);
        }
    }
    Ok(parsed)
}

/// Parse one normalized keybinding spec such as `ctrl-a` or `shift-enter`.
///
/// Specs are expected to be normalized by config deserialization, but this
/// parser remains strict to keep runtime error messages precise.
fn parse_keybinding(spec: &str) -> Option<KeyBinding> {
    let mut parts = spec.split('-');
    let mut modifiers = KeyModifiers::NONE;
    let mut key_name = None;

    for part in parts.by_ref() {
        match part {
            "ctrl" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "super" => modifiers |= KeyModifiers::SUPER,
            other => {
                key_name = Some(other.to_string());
                break;
            }
        }
    }

    let mut key_name = key_name?;
    for trailing in parts {
        key_name.push('-');
        key_name.push_str(trailing);
    }

    let key = match key_name.as_str() {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "esc" => KeyCode::Esc,
        "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "page-up" => KeyCode::PageUp,
        "page-down" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        "minus" => KeyCode::Char('-'),
        other if other.len() == 1 => KeyCode::Char(char::from(other.as_bytes()[0])),
        other if other.starts_with('f') => {
            let number = other[1..].parse::<u8>().ok()?;
            if (1..=MAX_FUNCTION_KEY).contains(&number) {
                KeyCode::F(number)
            } else {
                return None;
            }
        }
        _ => return None,
    };

    Some(KeyBinding::new(key, modifiers))
}

#[cfg(test)]
#[path = "keymap_tests.rs"]
mod tests;
