//! The bottom-pane footer renders transient hints and context indicators.
//!
//! The footer is pure rendering: it measures and paints selected content without mutating state.
//! It intentionally does not decide *which* footer content should be shown; that is owned by the
//! `ChatComposer` (which selects a `FooterMode`) and by higher-level state machines like
//! `ChatWidget` (which decides when quit/interrupt is allowed).
//!
//! The shortcut reference lives in `shortcut_overlay`; its responsive layout is shared by
//! footer measurement and rendering so resizing does not clip a differently measured layout.
//!
//! Some footer content is time-based rather than event-based, such as the "press again to quit"
//! hint. The owning widgets schedule redraws so time-based hints can expire even if the UI is
//! otherwise idle.
//!
//! Terminology used in this module:
//! - "status line" means the configurable contextual row built from `/statusline` items such as
//!   model, git branch, and context usage.
//! - "instructional footer" means a row that tells the user what to do next, such as quit
//!   confirmation, shortcut help, or queue hints.
//! - "contextual footer" means the footer is free to show ambient context instead of an
//!   instruction. In that state, the footer may render the configured status line, the active
//!   agent label, side-conversation state, or some combination of those.
//!
//! Single-line collapse overview:
//! 1. The composer decides the current `FooterMode` and hint flags, then calls
//!    `single_line_footer_layout` for the base single-line modes.
//! 2. `single_line_footer_layout` applies the width-based fallback rules:
//!    (If this description is hard to follow, just try it out by resizing
//!    your terminal width; these rules were built out of trial and error.)
//!    - Start with the fullest left-side hint plus the right-side context.
//!    - When the queue hint is active, prefer keeping that queue hint visible,
//!      even if it means dropping the right-side context earlier; the queue
//!      hint may also be shortened before it is removed.
//!    - When the queue hint is not active but the mode cycle hint is applicable,
//!      drop "? for shortcuts" before dropping "(shift+tab to cycle)".
//!    - If "(shift+tab to cycle)" cannot fit, also hide the right-side
//!      context to avoid too many state transitions in quick succession.
//!    - Finally, try a mode-only line (with and without context), and fall
//!      back to no left-side footer if nothing can fit.
//! 3. The composer renders the chosen line, the default `FooterProps` mapping, or no left content.
pub(crate) mod footer_content;

use self::footer_content::FooterContent;
use super::shortcut_overlay;
use super::status_line_style::STATUS_LINE_SEPARATOR;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::ShortcutHint;
use crate::render::line_utils::prefix_lines;
use crate::status::format_tokens_compact;
use crate::style::secondary_text_style;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

/// The rendering inputs for the footer area under the composer.
///
/// Callers are expected to construct `FooterProps` from higher-level state (`ChatComposer`,
/// `BottomPane`, and `ChatWidget`) and pass it to the footer content and layout helpers. The
/// footer treats these values as authoritative and does not attempt to infer missing state
/// (for example, it does not query whether a task is running).
#[derive(Clone, Debug)]
pub(crate) struct FooterProps {
    pub(crate) mode: FooterMode,
    pub(crate) esc_backtrack_hint: bool,
    pub(crate) is_task_running: bool,
    pub(crate) queue_submissions: bool,
    pub(crate) collaboration_modes_enabled: bool,
    pub(crate) is_wsl: bool,
    /// Which key the user must press again to quit.
    ///
    /// This is rendered when `mode` is `FooterMode::QuitShortcutReminder`.
    pub(crate) quit_shortcut_key: KeyBinding,
    pub(crate) status_line_value: Option<FooterContent>,
    pub(crate) status_line_enabled: bool,
    pub(crate) key_hints: FooterKeyHints,
    /// Active thread label shown when the footer is rendering contextual information instead of an
    /// instructional hint.
    ///
    /// When both this label and the configured status line are available, they are rendered on the
    /// same row separated by ` · `.
    pub(crate) active_agent_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CollaborationModeIndicator {
    Plan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GoalStatusIndicator {
    Active { usage: Option<String> },
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited { usage: Option<String> },
    Complete { usage: Option<String> },
}

const FOOTER_CONTEXT_GAP_COLS: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FooterKeyHints {
    pub(crate) agents: Option<ShortcutHint>,
    pub(crate) toggle_shortcuts: Option<ShortcutHint>,
    pub(crate) queue: Option<ShortcutHint>,
    pub(crate) insert_newline: Option<ShortcutHint>,
    pub(crate) external_editor: Option<ShortcutHint>,
    pub(crate) edit_previous: Option<ShortcutHint>,
    pub(crate) show_transcript: Option<ShortcutHint>,
    pub(crate) find_transcript: Option<ShortcutHint>,
    pub(crate) focus_activity: Option<ShortcutHint>,
    pub(crate) history_search: Option<ShortcutHint>,
    pub(crate) reasoning_down: Option<ShortcutHint>,
    pub(crate) reasoning_up: Option<ShortcutHint>,
    pub(crate) toggle_voice: Option<ShortcutHint>,
}

impl FooterKeyHints {
    #[cfg(test)]
    pub(crate) fn default_bindings() -> Self {
        Self {
            agents: None,
            toggle_shortcuts: Some(key_hint::plain(KeyCode::Char('?')).into()),
            queue: Some(key_hint::plain(KeyCode::Tab).into()),
            insert_newline: Some(key_hint::ctrl(KeyCode::Char('j')).into()),
            external_editor: Some(key_hint::ctrl(KeyCode::Char('g')).into()),
            edit_previous: Some(key_hint::plain(KeyCode::Esc).into()),
            show_transcript: Some(key_hint::ctrl(KeyCode::Char('t')).into()),
            find_transcript: Some(key_hint::plain(KeyCode::F(3)).into()),
            focus_activity: Some(key_hint::plain(KeyCode::F(4)).into()),
            history_search: Some(key_hint::ctrl(KeyCode::Char('r')).into()),
            reasoning_down: Some(key_hint::alt(KeyCode::Char(',')).into()),
            reasoning_up: Some(key_hint::alt(KeyCode::Char('.')).into()),
            toggle_voice: Some(key_hint::plain(KeyCode::F(8)).into()),
        }
    }
}

impl CollaborationModeIndicator {
    fn label(self, show_cycle_hint: bool) -> String {
        let suffix = if show_cycle_hint {
            format!(
                " ({} to cycle)",
                key_hint::shift(KeyCode::Tab).display_label()
            )
        } else {
            String::new()
        };
        match self {
            CollaborationModeIndicator::Plan => format!("Plan mode{suffix}"),
        }
    }

    fn styled_line(self, show_cycle_hint: bool) -> Line<'static> {
        let mut line = Line::from(self.label(/*show_cycle_hint*/ false).magenta());
        if show_cycle_hint {
            line.push_span(" (".set_style(secondary_text_style()));
            line.extend(key_hint::shift(KeyCode::Tab).spans());
            line.push_span(" to cycle)".set_style(secondary_text_style()));
        }
        line
    }
}

/// Selects which footer content is rendered.
///
/// The current mode is owned by `ChatComposer`, which may override it based on transient state
/// (for example, showing `QuitShortcutReminder` only while its timer is active).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FooterMode {
    /// Single-line query prompt for history recall or Vim buffer search.
    HistorySearch,
    /// Transient "press again to quit" reminder (Ctrl+C/Ctrl+D).
    QuitShortcutReminder,
    /// Multi-line shortcut overlay shown after pressing `?`.
    ShortcutOverlay,
    /// Transient "press Esc again" hint shown after the first Esc while idle.
    EscHint,
    /// Base single-line footer when the composer is empty.
    ComposerEmpty,
    /// Base single-line footer when the composer contains a draft.
    ///
    /// The shortcuts hint is suppressed here; when a task is running, this
    /// mode can show the queue hint instead.
    ComposerHasDraft,
}

pub(crate) fn toggle_shortcut_mode(
    current: FooterMode,
    ctrl_c_hint: bool,
    is_empty: bool,
) -> FooterMode {
    if ctrl_c_hint && matches!(current, FooterMode::QuitShortcutReminder) {
        return current;
    }

    let base_mode = if is_empty {
        FooterMode::ComposerEmpty
    } else {
        FooterMode::ComposerHasDraft
    };

    match current {
        FooterMode::ShortcutOverlay | FooterMode::QuitShortcutReminder => base_mode,
        _ => FooterMode::ShortcutOverlay,
    }
}

pub(crate) fn esc_hint_mode(current: FooterMode, is_task_running: bool) -> FooterMode {
    if is_task_running {
        current
    } else {
        FooterMode::EscHint
    }
}

pub(crate) fn reset_mode_after_activity(current: FooterMode) -> FooterMode {
    match current {
        FooterMode::EscHint
        | FooterMode::ShortcutOverlay
        | FooterMode::QuitShortcutReminder
        | FooterMode::HistorySearch
        | FooterMode::ComposerHasDraft => FooterMode::ComposerEmpty,
        other => other,
    }
}

pub(crate) fn footer_height(props: &FooterProps, width: u16) -> u16 {
    match props.mode {
        FooterMode::ShortcutOverlay => {
            shortcut_overlay::lines(props, width.saturating_sub(FOOTER_INDENT_COLS as u16)).len()
                as u16
        }
        FooterMode::ComposerEmpty
        | FooterMode::ComposerHasDraft
        | FooterMode::HistorySearch
        | FooterMode::QuitShortcutReminder
        | FooterMode::EscHint => 1,
    }
}

/// Render a single precomputed footer line.
pub(crate) fn render_footer_line(area: Rect, buf: &mut Buffer, line: Line<'static>) {
    Paragraph::new(prefix_lines(
        vec![line],
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

/// Render footer content directly from `FooterProps`.
///
/// This is intentionally not part of the width-based collapse/fallback logic.
/// Transient instructional states (shortcut overlay, Esc hint, quit reminder)
/// prioritize "what to do next" instructions and currently suppress the
/// collaboration mode label entirely. When collapse logic has already chosen a
/// specific single line, prefer `render_footer_line`.
pub(crate) fn render_footer_from_props(
    area: Rect,
    buf: &mut Buffer,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) {
    if props.mode == FooterMode::ShortcutOverlay {
        shortcut_overlay::render(props, inset_footer_hint_area(area), buf);
        return;
    }
    Paragraph::new(prefix_lines(
        footer_from_props_lines(
            props,
            collaboration_mode_indicator,
            show_cycle_hint,
            show_shortcuts_hint,
            show_queue_hint,
        ),
        " ".repeat(FOOTER_INDENT_COLS).into(),
        " ".repeat(FOOTER_INDENT_COLS).into(),
    ))
    .render(area, buf);
}

pub(crate) fn left_fits(area: Rect, left_width: u16) -> bool {
    let max_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16);
    left_width <= max_width
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryHintKind {
    None,
    Shortcuts,
    QueueMessage,
    QueueShort,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeftSideState {
    hint: SummaryHintKind,
    show_cycle_hint: bool,
}

fn left_side_line(
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    state: LeftSideState,
    key_hints: FooterKeyHints,
) -> Line<'static> {
    let mut line = Line::from("");
    match state.hint {
        SummaryHintKind::None => {}
        SummaryHintKind::Shortcuts => {
            if let Some(key) = key_hints.agents {
                line.extend(key.spans());
                line.push_span(" for agents".set_style(secondary_text_style()));
                if key_hints.toggle_shortcuts.is_some() {
                    line.push_span(" · ".set_style(secondary_text_style()));
                }
            }
            if let Some(key) = key_hints.toggle_shortcuts {
                line.extend(key.spans());
                line.push_span(" for shortcuts".set_style(secondary_text_style()));
            }
        }
        SummaryHintKind::QueueMessage => {
            if let Some(key) = key_hints.queue {
                line.extend(key.spans());
                line.push_span(" to queue message".set_style(secondary_text_style()));
            }
        }
        SummaryHintKind::QueueShort => {
            if let Some(key) = key_hints.queue {
                line.extend(key.spans());
                line.push_span(" to queue".set_style(secondary_text_style()));
            }
        }
    };

    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        if !matches!(state.hint, SummaryHintKind::None) {
            line.push_span(" · ".set_style(secondary_text_style()));
        }
        line.extend(
            collaboration_mode_indicator
                .styled_line(state.show_cycle_hint)
                .spans,
        );
    }

    line
}

pub(crate) enum SummaryLeft {
    Default,
    Custom(Line<'static>),
    None,
}

/// Compute the single-line footer layout and whether the right-side context
/// indicator can be shown alongside it.
pub(crate) fn single_line_footer_layout(
    area: Rect,
    context_width: u16,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
    key_hints: FooterKeyHints,
) -> (SummaryLeft, bool) {
    let hint_kind = if show_queue_hint {
        SummaryHintKind::QueueMessage
    } else if show_shortcuts_hint {
        SummaryHintKind::Shortcuts
    } else {
        SummaryHintKind::None
    };
    let default_state = LeftSideState {
        hint: hint_kind,
        show_cycle_hint,
    };
    let default_line = left_side_line(collaboration_mode_indicator, default_state, key_hints);
    let default_width = default_line.width() as u16;
    if default_width > 0 && can_show_left_with_context(area, default_width, context_width) {
        return (SummaryLeft::Default, true);
    }

    let state_line = |state: LeftSideState| -> Line<'static> {
        if state == default_state {
            default_line.clone()
        } else {
            left_side_line(collaboration_mode_indicator, state, key_hints)
        }
    };
    let state_width = |state: LeftSideState| -> u16 { state_line(state).width() as u16 };
    if show_shortcuts_hint && key_hints.agents.is_some() {
        let compact_line = left_side_line(
            collaboration_mode_indicator,
            default_state,
            FooterKeyHints {
                toggle_shortcuts: None,
                ..key_hints
            },
        );
        if can_show_left_with_context(area, compact_line.width() as u16, context_width) {
            return (SummaryLeft::Custom(compact_line), true);
        }
    }
    // When the mode cycle hint is applicable (idle, non-queue mode), only show
    // the right-side context indicator if the "(shift+tab to cycle)" variant
    // can also fit.
    let context_requires_cycle_hint = show_cycle_hint && !show_queue_hint;

    if show_queue_hint {
        // In queue mode, prefer dropping context before dropping the queue hint.
        let queue_states = [
            default_state,
            LeftSideState {
                hint: SummaryHintKind::QueueMessage,
                show_cycle_hint: false,
            },
            LeftSideState {
                hint: SummaryHintKind::QueueShort,
                show_cycle_hint: false,
            },
        ];

        // Pass 1: keep the right-side context indicator if any queue variant
        // can fit alongside it. We skip adjacent duplicates because
        // `default_state` can already be the no-cycle queue variant.
        let mut previous_state: Option<LeftSideState> = None;
        for state in queue_states {
            if previous_state == Some(state) {
                continue;
            }
            previous_state = Some(state);
            let width = state_width(state);
            if width > 0 && can_show_left_with_context(area, width, context_width) {
                if state == default_state {
                    return (SummaryLeft::Default, true);
                }
                return (SummaryLeft::Custom(state_line(state)), true);
            }
        }

        // Pass 2: if context cannot fit, drop it before dropping the queue
        // hint. Reuse the same dedupe so we do not try equivalent states twice.
        let mut previous_state: Option<LeftSideState> = None;
        for state in queue_states {
            if previous_state == Some(state) {
                continue;
            }
            previous_state = Some(state);
            let width = state_width(state);
            if width > 0 && left_fits(area, width) {
                if state == default_state {
                    return (SummaryLeft::Default, false);
                }
                return (SummaryLeft::Custom(state_line(state)), false);
            }
        }
    } else if collaboration_mode_indicator.is_some() {
        if show_cycle_hint {
            // First fallback: drop shortcut hint but keep the cycle
            // hint on the mode label if it can fit.
            let cycle_state = LeftSideState {
                hint: SummaryHintKind::None,
                show_cycle_hint: true,
            };
            let cycle_width = state_width(cycle_state);
            if cycle_width > 0 && can_show_left_with_context(area, cycle_width, context_width) {
                return (SummaryLeft::Custom(state_line(cycle_state)), true);
            }
            if cycle_width > 0 && left_fits(area, cycle_width) {
                return (SummaryLeft::Custom(state_line(cycle_state)), false);
            }
        }

        // Next fallback: mode label only. If the cycle hint is applicable but
        // cannot fit, we also suppress context so the right side does not
        // outlive "(shift+tab to cycle)" on the left.
        let mode_only_state = LeftSideState {
            hint: SummaryHintKind::None,
            show_cycle_hint: false,
        };
        let mode_only_width = state_width(mode_only_state);
        if !context_requires_cycle_hint
            && mode_only_width > 0
            && can_show_left_with_context(area, mode_only_width, context_width)
        {
            return (
                SummaryLeft::Custom(state_line(mode_only_state)),
                true, // show_context
            );
        }
        if mode_only_width > 0 && left_fits(area, mode_only_width) {
            return (
                SummaryLeft::Custom(state_line(mode_only_state)),
                false, // show_context
            );
        }
    }

    // Final fallback: if queue variants (or other earlier states) could not fit
    // at all, drop every hint and try to show just the mode label.
    if let Some(collaboration_mode_indicator) = collaboration_mode_indicator {
        let mode_only_state = LeftSideState {
            hint: SummaryHintKind::None,
            show_cycle_hint: false,
        };
        // Compute the width without going through `state_line` so we do not
        // depend on `default_state` (which may still be a queue variant).
        let mode_only_width = left_side_line(
            Some(collaboration_mode_indicator),
            mode_only_state,
            key_hints,
        )
        .width() as u16;
        if !context_requires_cycle_hint
            && can_show_left_with_context(area, mode_only_width, context_width)
        {
            return (
                SummaryLeft::Custom(left_side_line(
                    Some(collaboration_mode_indicator),
                    mode_only_state,
                    key_hints,
                )),
                true, // show_context
            );
        }
        if left_fits(area, mode_only_width) {
            return (
                SummaryLeft::Custom(left_side_line(
                    Some(collaboration_mode_indicator),
                    mode_only_state,
                    key_hints,
                )),
                false, // show_context
            );
        }
    }

    (SummaryLeft::None, true)
}

pub(crate) fn mode_indicator_line(
    indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    indicator.map(|indicator| indicator.styled_line(show_cycle_hint))
}

pub(crate) fn goal_status_indicator_line(
    indicator: Option<&GoalStatusIndicator>,
) -> Option<Line<'static>> {
    let indicator = indicator?;
    let label = match indicator {
        GoalStatusIndicator::Active { usage } => {
            if let Some(usage) = usage {
                format!("Pursuing goal ({usage})")
            } else {
                "Pursuing goal".to_string()
            }
        }
        GoalStatusIndicator::Paused => "Goal paused (/goal resume)".to_string(),
        GoalStatusIndicator::Blocked => "Goal stalled (/goal resume)".to_string(),
        GoalStatusIndicator::UsageLimited => "Goal hit usage limits (/goal resume)".to_string(),
        GoalStatusIndicator::BudgetLimited { usage } => {
            if let Some(usage) = usage {
                format!("Goal unmet ({usage})")
            } else {
                "Goal abandoned".to_string()
            }
        }
        GoalStatusIndicator::Complete { usage } => {
            if let Some(usage) = usage {
                format!("Goal achieved ({usage})")
            } else {
                "Goal achieved".to_string()
            }
        }
    };

    Some(Line::from(vec![Span::from(label).magenta()]))
}

pub(crate) fn status_line_right_indicator_line(
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    goal_status_indicator: Option<&GoalStatusIndicator>,
    ide_context_active: bool,
    show_cycle_hint: bool,
) -> Option<Line<'static>> {
    let primary_indicator = mode_indicator_line(collaboration_mode_indicator, show_cycle_hint)
        .or_else(|| goal_status_indicator_line(goal_status_indicator));
    let ide_context_indicator = ide_context_active.then(|| Line::from(vec!["IDE context".cyan()]));
    let mut line: Option<Line<'static>> = None;

    for indicator in [primary_indicator, ide_context_indicator]
        .into_iter()
        .flatten()
    {
        if let Some(line) = line.as_mut() {
            line.push_span(" · ".set_style(secondary_text_style()));
            for span in indicator.spans {
                line.push_span(span);
            }
        } else {
            line = Some(indicator);
        }
    }

    line
}

pub(crate) fn side_conversation_context_line(label: &str) -> Line<'static> {
    let mut line = Line::default();
    let rest = if let Some(rest) = label.strip_prefix("Side ") {
        line.extend(["Side".magenta().bold(), " ".into()]);
        rest
    } else {
        label
    };
    for (index, part) in rest.split(" · ").enumerate() {
        if index > 0 {
            line.push_span(" · ".set_style(secondary_text_style()));
        }
        if let Some((keys, action)) = [" to switch", " to close", " to archive", " for side"]
            .into_iter()
            .find_map(|action| part.strip_suffix(action).map(|keys| (keys, action)))
        {
            line.extend(key_hint::key_label_spans(keys));
            line.push_span(action.set_style(secondary_text_style()));
        } else {
            line.push_span(part.to_owned().magenta());
        }
    }
    line
}

fn right_aligned_x(area: Rect, content_width: u16) -> Option<u16> {
    if area.is_empty() {
        return None;
    }

    let right_padding = FOOTER_INDENT_COLS as u16;
    let max_width = area.width.saturating_sub(right_padding);
    if content_width == 0 || max_width == 0 {
        return None;
    }

    if content_width >= max_width {
        return Some(area.x.saturating_add(right_padding));
    }

    Some(
        area.x
            .saturating_add(area.width)
            .saturating_sub(content_width)
            .saturating_sub(right_padding),
    )
}

pub(crate) fn max_left_width_for_right(area: Rect, right_width: u16) -> Option<u16> {
    let context_x = right_aligned_x(area, right_width)?;
    let left_start = area.x + FOOTER_INDENT_COLS as u16;

    // minimal one column gap between left and right
    let gap = FOOTER_CONTEXT_GAP_COLS;

    if context_x <= left_start + gap {
        return Some(0);
    }

    Some(context_x.saturating_sub(left_start + gap))
}

pub(crate) fn can_show_left_with_context(area: Rect, left_width: u16, context_width: u16) -> bool {
    let Some(context_x) = right_aligned_x(area, context_width) else {
        return true;
    };
    if left_width == 0 {
        return true;
    }
    left_width
        <= context_x
            .saturating_sub(area.x)
            .saturating_sub(FOOTER_INDENT_COLS as u16 + FOOTER_CONTEXT_GAP_COLS)
}

pub(crate) fn render_context_right(area: Rect, buf: &mut Buffer, line: &Line<'static>) {
    if area.is_empty() {
        return;
    }

    let context_width = line.width() as u16;
    let Some(mut x) = right_aligned_x(area, context_width) else {
        return;
    };
    let y = area.y + area.height.saturating_sub(1);
    let max_x = area.x.saturating_add(area.width);

    for span in &line.spans {
        if x >= max_x {
            break;
        }
        let span_width = span.width() as u16;
        if span_width == 0 {
            continue;
        }
        let remaining = max_x.saturating_sub(x);
        let draw_width = span_width.min(remaining);
        buf.set_span(x, y, span, draw_width);
        x = x.saturating_add(span_width);
    }
}

pub(crate) fn render_unfocused_status(text: String, area: Rect, buf: &mut Buffer) {
    let area = inset_footer_hint_area(area);
    crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
        text.dim().into(),
        usize::from(area.width),
    )
    .render(area, buf);
}

pub(crate) fn inset_footer_hint_area(mut area: Rect) -> Rect {
    if area.width > 2 {
        area.x += 2;
        area.width = area.width.saturating_sub(2);
    }
    area
}

pub(crate) fn render_footer_hint_items(area: Rect, buf: &mut Buffer, items: &[(String, String)]) {
    if items.is_empty() {
        return;
    }

    footer_hint_items_line(items).render(inset_footer_hint_area(area), buf);
}

/// Map `FooterProps` to footer lines without width-based collapse.
///
/// This is the canonical FooterMode-to-text mapping. It powers transient,
/// instructional states (shortcut overlay, Esc hint, quit reminder) and also
/// the default rendering for base states when collapse is not applied (or when
/// `single_line_footer_layout` returns `SummaryLeft::Default`). Collapse and
/// fallback decisions live in `single_line_footer_layout`; this function only
/// formats the chosen/default content.
fn footer_from_props_lines(
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> Vec<Line<'static>> {
    let key_hints = props.key_hints;
    // The configurable status line has its own layout. An agent label by itself still belongs
    // to the ordinary footer flow.
    if props.mode.shows_passive_line(props.is_task_running) && props.active_agent_label.is_some() {
        return vec![passive_footer_suffix(props, /*has_status*/ false)];
    }
    match props.mode {
        FooterMode::QuitShortcutReminder => {
            vec![quit_shortcut_reminder_line(props.quit_shortcut_key)]
        }
        FooterMode::HistorySearch => {
            vec![Line::from("reverse-i-search: ").set_style(secondary_text_style())]
        }
        FooterMode::ComposerEmpty => {
            let state = LeftSideState {
                hint: if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(
                collaboration_mode_indicator,
                state,
                key_hints,
            )]
        }
        FooterMode::ShortcutOverlay => shortcut_overlay::lines(props, u16::MAX),
        FooterMode::EscHint => vec![esc_hint_line(props.esc_backtrack_hint)],
        FooterMode::ComposerHasDraft => {
            let state = LeftSideState {
                hint: if show_queue_hint {
                    SummaryHintKind::QueueMessage
                } else if show_shortcuts_hint {
                    SummaryHintKind::Shortcuts
                } else {
                    SummaryHintKind::None
                },
                show_cycle_hint,
            };
            vec![left_side_line(
                collaboration_mode_indicator,
                state,
                key_hints,
            )]
        }
    }
}

/// Returns the contextual footer row when the footer is not busy showing an instructional hint.
///
/// The returned line may contain the configured status line, the currently viewed agent label, or
/// both combined. Active instructional states such as quit reminders, shortcut overlays, and queue
/// prompts deliberately return `None` so those call-to-action hints stay visible.
pub(crate) fn passive_footer_status_line(props: &FooterProps) -> Option<FooterContent> {
    if !props.mode.shows_passive_line(props.is_task_running) {
        return None;
    }

    let status = props
        .status_line_value
        .as_ref()
        .filter(|_| props.status_line_enabled);
    let mut line = match status {
        Some(FooterContent::Line(line)) => HyperlinkLine::from(line.clone()),
        Some(FooterContent::HyperlinkLine(line)) => line.clone(),
        Some(content @ FooterContent::Extension { .. }) => return Some(content.clone()),
        None => HyperlinkLine::default(),
    };
    let suffix = passive_footer_suffix(props, status.is_some());
    if status.is_none() && suffix.spans.is_empty() {
        return None;
    }
    if line.width() > 0 && suffix.width() > 0 {
        line.line
            .push_span(STATUS_LINE_SEPARATOR.set_style(secondary_text_style()));
    }
    line.line.spans.extend(suffix.spans);
    Some(FooterContent::HyperlinkLine(line))
}

fn passive_footer_suffix(props: &FooterProps, has_status: bool) -> Line<'static> {
    let mut line = Line::default();
    if let Some(active_agent_label) = &props.active_agent_label {
        line.push_span(active_agent_label.clone().set_style(secondary_text_style()));
    }
    if props.mode == FooterMode::ComposerEmpty
        && let Some(key) = props.key_hints.agents
        && (has_status || !line.spans.is_empty())
    {
        if !line.spans.is_empty() {
            line.push_span(" · ".set_style(secondary_text_style()));
        }
        line.extend(key.spans());
        line.push_span(" for agents".set_style(secondary_text_style()));
    }
    line
}

impl FooterMode {
    /// Whether contextual status can occupy the row normally used for instructions.
    pub(super) fn shows_passive_line(self, is_task_running: bool) -> bool {
        match self {
            Self::ComposerEmpty => true,
            Self::ComposerHasDraft => !is_task_running,
            Self::HistorySearch
            | Self::QuitShortcutReminder
            | Self::ShortcutOverlay
            | Self::EscHint => false,
        }
    }
}

/// Whether callers should reserve the dedicated status-line layout for a contextual footer row.
///
/// The dedicated layout exists for the configurable `/statusline` row. An agent label by itself
/// can be rendered by the standard footer flow, so this only becomes `true` when the status line
/// feature is enabled and the current mode allows contextual footer content.
pub(crate) fn uses_passive_footer_status_layout(props: &FooterProps) -> bool {
    props.status_line_enabled && props.mode.shows_passive_line(props.is_task_running)
}

pub(crate) fn footer_line_width(
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    show_cycle_hint: bool,
    show_shortcuts_hint: bool,
    show_queue_hint: bool,
) -> u16 {
    footer_from_props_lines(
        props,
        collaboration_mode_indicator,
        show_cycle_hint,
        show_shortcuts_hint,
        show_queue_hint,
    )
    .last()
    .map(|line| line.width() as u16)
    .unwrap_or(0)
}

pub(crate) fn footer_hint_items_width(items: &[(String, String)]) -> u16 {
    if items.is_empty() {
        return 0;
    }
    footer_hint_items_line(items).width() as u16
}

pub(crate) fn footer_hint_items_line(items: &[(String, String)]) -> Line<'static> {
    let mut spans = Vec::with_capacity(items.len() * 4);
    for (idx, (key, label)) in items.iter().enumerate() {
        if idx > 0 {
            spans.push(" · ".set_style(secondary_text_style()));
        }
        spans.extend(key_hint::key_label_spans(key));
        if idx == 0
            && key == "voice"
            && let Some(label) = label.strip_prefix("● ")
        {
            spans.push(" ●".red());
            spans.push(format!(" {label}").set_style(secondary_text_style()));
        } else {
            spans.push(format!(" {label}").set_style(secondary_text_style()));
        }
    }
    Line::from(spans)
}

fn quit_shortcut_reminder_line(key: KeyBinding) -> Line<'static> {
    let mut line = Line::from(key.spans());
    line.push_span(" again to quit".set_style(secondary_text_style()));
    line
}

fn esc_hint_line(esc_backtrack_hint: bool) -> Line<'static> {
    let esc = key_hint::plain(KeyCode::Esc);
    if esc_backtrack_hint {
        let mut line = Line::from(esc.spans());
        line.push_span(" again to edit previous message".set_style(secondary_text_style()));
        line
    } else {
        let mut line = Line::from(esc.spans());
        line.push_span(" ".set_style(secondary_text_style()));
        line.extend(esc.spans());
        line.push_span(" to edit previous message".set_style(secondary_text_style()));
        line
    }
}

pub(crate) fn context_window_line(percent: Option<i64>, used_tokens: Option<i64>) -> Line<'static> {
    if let Some(percent) = percent {
        let percent = percent.clamp(0, 100);
        return Line::from(vec![
            Span::from(format!("{percent}% context left")).set_style(secondary_text_style()),
        ]);
    }

    if let Some(tokens) = used_tokens {
        let used_fmt = format_tokens_compact(tokens);
        return Line::from(vec![
            Span::from(format!("{used_fmt} used")).set_style(secondary_text_style()),
        ]);
    }

    Line::from(vec![
        Span::from("100% context left").set_style(secondary_text_style()),
    ])
}

#[cfg(test)]
#[path = "footer_tests.rs"]
mod tests;
