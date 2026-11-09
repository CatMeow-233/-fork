//! Renders the picker chrome around the list: search line, toolbar, footer, and loading overlay.

use super::*;

/// Returns the human-readable column header for the given sort key.
fn sort_key_label(sort_key: ThreadSortKey) -> &'static str {
    match sort_key {
        ThreadSortKey::CreatedAt => "Created",
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt | ThreadSortKey::SectionPosition => {
            "Updated"
        }
    }
}

pub(super) fn list_viewport_width(width: u16) -> u16 {
    width.saturating_sub(PICKER_LIST_HORIZONTAL_INSET)
}

pub(super) fn search_line(state: &PickerState, width: u16) -> Line<'_> {
    if let Some(error) = state.inline_error.as_deref() {
        return Line::from(truncate_text(error, usize::from(width)).red());
    }
    if state.query.is_empty() {
        "Type to search".set_style(secondary_text_style()).into()
    } else {
        truncate_text(&format!("Search: {}", state.query), usize::from(width)).into()
    }
}

pub(super) fn toolbar_for_width(state: &PickerState, width: u16) -> Line<'static> {
    let toolbar = toolbar_line(state, /*compact*/ false);
    if toolbar.width() <= usize::from(width) {
        return toolbar;
    }
    let toolbar = toolbar_line(state, /*compact*/ true);
    if toolbar.width() <= usize::from(width) {
        return toolbar;
    }
    // Keep the control affected by the arrow keys visible when the compact strip cannot fit.
    let spans = match state.toolbar_focus {
        ToolbarControl::Filter => filter_control_spans(state, /*compact*/ true),
        ToolbarControl::Sort => sort_control_spans(state, /*compact*/ true),
        ToolbarControl::Status => vec![
            "Status:".set_style(secondary_text_style()),
            toolbar_value(
                match state.status {
                    SessionStatus::Active => "Active",
                    SessionStatus::Archived => "Archived",
                },
                /*active*/ true,
                /*focused*/ true,
            ),
        ],
    };
    spans.into()
}

pub(super) fn toolbar_line(state: &PickerState, compact: bool) -> Line<'static> {
    let mut spans = Vec::new();
    let separator = if compact && matches!(state.action, SessionPickerAction::Resume) {
        " "
    } else {
        "   "
    };
    spans.extend(filter_control_spans(state, compact));
    spans.push(separator.set_style(secondary_text_style()));
    if matches!(state.action, SessionPickerAction::Resume) {
        let status_focused = state.toolbar_focus == ToolbarControl::Status;
        if compact {
            let active_status = match state.status {
                SessionStatus::Active => "Active",
                SessionStatus::Archived => "Archived",
            };
            spans.push(toolbar_value(
                active_status,
                /*active*/ true,
                status_focused,
            ));
        } else {
            spans.push("Status: ".set_style(secondary_text_style()));
            spans.push(toolbar_value(
                "Active",
                state.status == SessionStatus::Active,
                status_focused,
            ));
            spans.push(toolbar_value(
                "Archived",
                state.status == SessionStatus::Archived,
                status_focused,
            ));
        }
        spans.push(separator.set_style(secondary_text_style()));
    }
    spans.extend(sort_control_spans(state, compact));
    spans.into()
}

fn sort_control_spans(state: &PickerState, compact: bool) -> Vec<Span<'static>> {
    let sort_focused = state.toolbar_focus == ToolbarControl::Sort;
    if compact {
        return vec![
            "Sort:".set_style(secondary_text_style()),
            toolbar_value(
                sort_key_label(state.sort_key),
                /*active*/ true,
                sort_focused,
            ),
        ];
    }
    vec![
        "Sort: ".set_style(secondary_text_style()),
        toolbar_value(
            sort_key_label(ThreadSortKey::UpdatedAt),
            state.sort_key == ThreadSortKey::UpdatedAt,
            sort_focused,
        ),
        toolbar_value(
            sort_key_label(ThreadSortKey::CreatedAt),
            state.sort_key == ThreadSortKey::CreatedAt,
            sort_focused,
        ),
    ]
}

fn filter_control_spans(state: &PickerState, compact: bool) -> Vec<Span<'static>> {
    let filter_focused = state.toolbar_focus == ToolbarControl::Filter;
    if compact || state.filter_cwd.is_none() {
        return vec![
            "Filter:".set_style(secondary_text_style()),
            toolbar_value(
                filter_mode_label(state.filter_mode),
                /*active*/ true,
                filter_focused,
            ),
        ];
    }
    let mut values = vec![
        "Filter: ".set_style(secondary_text_style()),
        toolbar_value(
            filter_mode_label(SessionFilterMode::Cwd),
            state.filter_mode == SessionFilterMode::Cwd,
            filter_focused,
        ),
    ];
    if state.repo_filter_available {
        values.push(toolbar_value(
            filter_mode_label(SessionFilterMode::Repo),
            state.filter_mode == SessionFilterMode::Repo,
            filter_focused,
        ));
    }
    values.push(toolbar_value(
        filter_mode_label(SessionFilterMode::All),
        state.filter_mode == SessionFilterMode::All,
        filter_focused,
    ));
    values
}

fn toolbar_value(label: &'static str, active: bool, focused: bool) -> Span<'static> {
    let value = format!(" {label} ");
    if active {
        value.set_style(if focused {
            crate::bottom_pane::active_tab_style()
        } else {
            let style = crate::style::user_message_style().bold().not_dim();
            style.fg(readable_color_on(Color::Reset, style.bg))
        })
    } else {
        value.set_style(secondary_text_style())
    }
}

fn filter_mode_label(filter_mode: SessionFilterMode) -> &'static str {
    match filter_mode {
        SessionFilterMode::Cwd => "Cwd",
        SessionFilterMode::Repo => "Repo",
        SessionFilterMode::All => "All",
    }
}

pub(super) fn render_picker_footer(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    state: &PickerState,
    list_height: u16,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let separator = Rect::new(area.x, area.y, area.width, 1);
    render_picker_footer_separator(
        frame,
        separator,
        picker_footer_progress_label(state, list_height, area.width),
    );

    let lines = footer_hint_lines(state, area.width);
    for (idx, line) in lines.into_iter().enumerate() {
        let y = area.y.saturating_add(1 + idx as u16);
        if y >= area.bottom() {
            break;
        }
        frame.render_widget_ref(&line, Rect::new(area.x, y, area.width, 1));
    }
}

fn render_picker_footer_separator(
    frame: &mut crate::custom_terminal::Frame,
    area: Rect,
    progress_label: String,
) {
    if area.width == 0 {
        return;
    }

    let separator = "─".repeat(area.width as usize);
    frame.render_widget_ref(&Line::from(separator.dim()), area);

    let progress_width = UnicodeWidthStr::width(progress_label.as_str()) as u16;
    if progress_width < area.width {
        let percent_area = Rect::new(
            area.x + area.width - progress_width - 1,
            area.y,
            progress_width,
            1,
        );
        frame.render_widget_ref(
            &Line::from(progress_label.set_style(secondary_text_style())),
            percent_area,
        );
    }
}

pub fn picker_footer_progress_label(state: &PickerState, list_height: u16, width: u16) -> String {
    let position = if state.filtered_rows.is_empty() {
        0
    } else {
        state.selected.saturating_add(1)
    };
    let total = if state.pagination.is_loading() {
        format!("{}…", state.filtered_rows.len())
    } else {
        state.filtered_rows.len().to_string()
    };
    let percent = picker_footer_percent(state, list_height);
    let labels = [
        format!(" {position} / {total} · {percent}% "),
        format!(" {position}/{total} · {percent}% "),
        format!(" {percent}% "),
    ];
    labels
        .into_iter()
        .find(|label| UnicodeWidthStr::width(label.as_str()) < width as usize)
        .unwrap_or_default()
}

pub(super) fn picker_footer_percent(state: &PickerState, list_height: u16) -> u8 {
    if state.pagination.is_loading() {
        return state.frozen_footer_percent.unwrap_or_else(|| {
            if state.filtered_rows.is_empty() {
                0
            } else {
                picker_footer_scroll_percent(state, list_height)
            }
        });
    }

    picker_footer_scroll_percent(state, list_height)
}

pub(super) fn picker_footer_scroll_percent(state: &PickerState, list_height: u16) -> u8 {
    if state.filtered_rows.is_empty() {
        return 100;
    }

    let content_rows = state.available_content_rows(list_height as usize);
    let total_height =
        state.rendered_height_between(/*start*/ 0, state.filtered_rows.len() - 1);
    let max_scroll = total_height.saturating_sub(content_rows);
    if max_scroll == 0 {
        return 100;
    }
    let remaining_height =
        state.rendered_height_between(state.scroll_top, state.filtered_rows.len() - 1);
    if remaining_height <= content_rows {
        return 100;
    }

    let skipped_height = if state.scroll_top == 0 {
        0
    } else {
        state.rendered_height_between(/*start*/ 0, state.scroll_top - 1)
    };
    (((skipped_height.min(max_scroll)) as f32 / max_scroll as f32) * 100.0).round() as u8
}

pub(super) fn footer_hint_lines(state: &PickerState, width: u16) -> Vec<Line<'static>> {
    if state.is_transcript_loading() {
        let hints = [
            PickerFooterHint {
                key: "loading".to_string(),
                wide_label: String::from("transcript"),
                compact_label: String::from("transcript"),
                priority: 0,
            },
            PickerFooterHint {
                key: crate::key_hint::ctrl(KeyCode::Char('c')).display_label(),
                wide_label: String::from("quit"),
                compact_label: String::from("quit"),
                priority: 1,
            },
        ];
        let line = fit_footer_hints(&hints, FooterHintLabelMode::Wide, width)
            .or_else(|| fit_footer_hints(&hints, FooterHintLabelMode::Compact, width))
            .or_else(|| fit_footer_hints(&hints, FooterHintLabelMode::KeyOnly, width))
            .unwrap_or_default();
        return vec![line, Line::default()];
    }

    let action_label = if state.status == SessionStatus::Archived {
        "restore"
    } else {
        state.action.action_label()
    };
    let (esc_label, esc_compact_label) = if state.query.is_empty() {
        match state.launch_context {
            SessionPickerLaunchContext::Startup => ("start new", "new"),
            SessionPickerLaunchContext::ExistingSession { .. } => ("exit", "exit"),
        }
    } else {
        ("clear search", "clear")
    };
    let ctrl_c_label = match state.launch_context {
        SessionPickerLaunchContext::Startup => "quit",
        SessionPickerLaunchContext::ExistingSession { .. } => "exit",
    };
    let density_label = match state.density {
        SessionListDensity::Comfortable => "dense view",
        SessionListDensity::Dense => "comfortable view",
    };
    let density_compact_label = match state.density {
        SessionListDensity::Comfortable => "dense",
        SessionListDensity::Dense => "comfy",
    };
    let mut first_row_hints = Vec::new();
    if let Some(accept) = state.keymap.list.primary_hint(ListAction::Accept) {
        first_row_hints.push(PickerFooterHint {
            key: accept.display_label(),
            wide_label: action_label.to_string(),
            compact_label: action_label.to_string(),
            priority: 0,
        });
    }
    if !state.filtered_rows.is_empty() && state.archive_shortcut_available() {
        first_row_hints.push(PickerFooterHint {
            key: crate::key_hint::ctrl(KeyCode::Char('a')).display_label(),
            wide_label: String::from("archive"),
            compact_label: String::from("archive"),
            priority: 2,
        });
    }
    if let Some(cancel) = state.keymap.list.primary_hint(ListAction::Cancel) {
        first_row_hints.push(PickerFooterHint {
            key: cancel.display_label(),
            wide_label: esc_label.to_string(),
            compact_label: esc_compact_label.to_string(),
            priority: 1,
        });
    }
    first_row_hints.extend([
        PickerFooterHint {
            key: crate::key_hint::ctrl(KeyCode::Char('c')).display_label(),
            wide_label: ctrl_c_label.to_string(),
            compact_label: ctrl_c_label.to_string(),
            priority: 2,
        },
        PickerFooterHint {
            key: "tab".to_string(),
            wide_label: String::from("focus sort/filter"),
            compact_label: String::from("focus"),
            priority: 7,
        },
    ]);
    let option_keys = [ListAction::MoveLeft, ListAction::MoveRight]
        .into_iter()
        .filter_map(|action| state.keymap.list.primary_hint(action))
        .map(super::key_hint::ShortcutHint::display_label)
        .collect::<Vec<_>>()
        .join("/");
    if !option_keys.is_empty() {
        first_row_hints.push(PickerFooterHint {
            key: option_keys,
            wide_label: String::from("change option"),
            compact_label: String::from("option"),
            priority: 8,
        });
    }
    let mut second_row_hints = vec![
        PickerFooterHint {
            key: crate::key_hint::ctrl(KeyCode::Char('o')).display_label(),
            wide_label: density_label.to_string(),
            compact_label: density_compact_label.to_string(),
            priority: 3,
        },
        PickerFooterHint {
            key: crate::key_hint::ctrl(KeyCode::Char('t')).display_label(),
            wide_label: String::from("transcript"),
            compact_label: String::from("preview"),
            priority: 4,
        },
        PickerFooterHint {
            key: crate::key_hint::ctrl(KeyCode::Char('e')).display_label(),
            wide_label: String::from("expand"),
            compact_label: String::from("exp"),
            priority: 6,
        },
    ];
    let browse_keys = [ListAction::MoveUp, ListAction::MoveDown]
        .into_iter()
        .filter_map(|action| state.keymap.list.primary_hint(action))
        .map(super::key_hint::ShortcutHint::display_label)
        .collect::<Vec<_>>()
        .join("/");
    if !browse_keys.is_empty() {
        second_row_hints.push(PickerFooterHint {
            key: browse_keys,
            wide_label: String::from("browse"),
            compact_label: String::from("browse"),
            priority: 5,
        });
    }

    vec![
        hint_line_for_row(&first_row_hints, width),
        hint_line_for_row(&second_row_hints, width),
    ]
}

fn hint_line_for_row(hints: &[PickerFooterHint], width: u16) -> Line<'static> {
    if width >= FOOTER_COMPACT_BREAKPOINT
        && let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::Wide, width)
    {
        return line;
    }
    if let Some(line) = fit_footer_hints(hints, FooterHintLabelMode::Compact, width) {
        return line;
    }
    let mut retained = (0..hints.len()).collect::<Vec<_>>();
    retained.sort_by_key(|idx| hints[*idx].priority);
    for mode in [FooterHintLabelMode::Compact, FooterHintLabelMode::KeyOnly] {
        for retain_count in (1..=retained.len()).rev() {
            let mut candidate_indices = retained[..retain_count].to_vec();
            candidate_indices.sort_unstable();
            let candidate = candidate_indices
                .iter()
                .map(|idx| &hints[*idx])
                .collect::<Vec<_>>();
            if let Some(line) = fit_footer_hint_refs(&candidate, mode, width) {
                return line;
            }
        }
    }
    Line::default()
}

pub fn render_transcript_loading_overlay(frame: &mut crate::custom_terminal::Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let message = "Loading transcript…";
    let message_width = UnicodeWidthStr::width(message) as u16;
    let overlay_width = if area.width >= message_width.saturating_add(10) {
        message_width + 10
    } else {
        area.width
    };
    let overlay_height = if area.height >= 3 { 3 } else { 1 };
    let overlay = Rect::new(
        area.x + area.width.saturating_sub(overlay_width) / 2,
        area.y + area.height.saturating_sub(overlay_height) / 2,
        overlay_width,
        overlay_height,
    );
    let style = transcript_loading_overlay_style();
    for y in overlay.y..overlay.bottom() {
        for x in overlay.x..overlay.right() {
            frame.buffer[(x, y)].set_symbol(" ").set_style(style);
        }
    }

    let message = truncate_text(message, overlay.width as usize);
    let message_width = UnicodeWidthStr::width(message.as_str()) as u16;
    let line = Rect::new(
        overlay.x + overlay.width.saturating_sub(message_width) / 2,
        overlay.y + overlay.height / 2,
        message_width.min(overlay.width),
        1,
    );
    frame.render_widget_ref(&Line::from(message.bold()), line);
}

pub(super) fn transcript_loading_overlay_style() -> Style {
    let Some(bg) = default_bg() else {
        return Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .not_dim()
            .not_reversed();
    };
    let (overlay, alpha) = if is_light(bg) {
        ((0, 0, 0), 0.08)
    } else {
        ((255, 255, 255), 0.14)
    };
    let fill = best_color(blend(overlay, bg, alpha));
    Style::default()
        .bg(fill)
        .fg(readable_color_on(Color::Reset, Some(fill)))
        .not_dim()
        .not_reversed()
}

fn fit_footer_hints(
    hints: &[PickerFooterHint],
    mode: FooterHintLabelMode,
    width: u16,
) -> Option<Line<'static>> {
    let hint_refs = hints.iter().collect::<Vec<_>>();
    fit_footer_hint_refs(&hint_refs, mode, width)
}

fn fit_footer_hint_refs(
    hints: &[&PickerFooterHint],
    mode: FooterHintLabelMode,
    width: u16,
) -> Option<Line<'static>> {
    let gap_width = FOOTER_HINT_GAP;
    if footer_hints_width(hints, mode, gap_width) > width as usize {
        return None;
    }

    let mut spans = vec![
        " ".repeat(FOOTER_HINT_LEFT_PADDING)
            .set_style(footer_hint_label_style()),
    ];
    for (idx, hint) in hints.iter().enumerate() {
        if idx > 0 {
            spans.push(" ".repeat(gap_width).set_style(footer_hint_label_style()));
        }
        spans.extend(crate::key_hint::key_label_spans(&hint.key));
        let label = match mode {
            FooterHintLabelMode::Wide => Some(hint.wide_label.as_str()),
            FooterHintLabelMode::Compact => Some(hint.compact_label.as_str()),
            FooterHintLabelMode::KeyOnly => None,
        };
        if let Some(label) = label {
            spans.push(" ".set_style(footer_hint_label_style()));
            spans.push(label.to_string().set_style(footer_hint_label_style()));
        }
    }
    Some(spans.into())
}

fn footer_hints_width(
    hints: &[&PickerFooterHint],
    mode: FooterHintLabelMode,
    gap_width: usize,
) -> usize {
    FOOTER_HINT_LEFT_PADDING
        + hints
            .iter()
            .enumerate()
            .map(|(idx, hint)| {
                let label_width = match mode {
                    FooterHintLabelMode::Wide => {
                        1 + UnicodeWidthStr::width(hint.wide_label.as_str())
                    }
                    FooterHintLabelMode::Compact => {
                        1 + UnicodeWidthStr::width(hint.compact_label.as_str())
                    }
                    FooterHintLabelMode::KeyOnly => 0,
                };
                let hint_width = UnicodeWidthStr::width(hint.key.as_str()) + label_width;
                if idx == 0 {
                    hint_width
                } else {
                    hint_width + gap_width
                }
            })
            .sum::<usize>()
}
