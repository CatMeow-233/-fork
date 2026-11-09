//! Renders the session list and its comfortable and dense row layouts.

use super::*;

pub fn render_list(frame: &mut crate::custom_terminal::Frame, area: Rect, state: &PickerState) {
    if area.height == 0 {
        return;
    }
    Clear.render(area, frame.buffer);

    let rows = &state.filtered_rows;
    if rows.is_empty() {
        let message = render_empty_state_line(state);
        frame.render_widget_ref(&message, area);
        return;
    }

    let show_more_above = state.has_more_above();
    let show_more_below = state.has_more_below(area.height as usize);
    let content_area = Rect::new(
        area.x,
        area.y.saturating_add(u16::from(show_more_above)),
        area.width,
        area.height
            .saturating_sub(u16::from(show_more_above))
            .saturating_sub(u16::from(show_more_below)),
    );
    if show_more_above {
        frame.render_widget_ref(
            &more_line("↑ more"),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let start = state.scroll_top.min(rows.len().saturating_sub(1));
    let mut y = content_area.y;
    for (idx, row) in rows[start..].iter().enumerate() {
        if y >= content_area.y.saturating_add(content_area.height) {
            break;
        }
        let row_idx = start + idx;
        let is_selected = row_idx == state.selected;
        let is_expanded =
            is_selected && row.thread_id.is_some() && state.expanded_thread_id == row.thread_id;
        let is_zebra = row_idx.is_multiple_of(2);
        for line in render_session_lines(row, state, is_selected, is_expanded, is_zebra, area.width)
        {
            if y >= content_area.y.saturating_add(content_area.height) {
                break;
            }
            frame.render_widget_ref(&line, Rect::new(area.x, y, area.width, 1));
            y = y.saturating_add(1);
        }
        if state.density == SessionListDensity::Comfortable
            && y < content_area.y.saturating_add(content_area.height)
            && start + idx + 1 < rows.len()
        {
            y = y.saturating_add(1);
        }
    }

    if state.pagination.is_loading() && y < content_area.y.saturating_add(content_area.height) {
        let loading_line: Line = vec![
            "  ".into(),
            "Loading older sessions…"
                .set_style(secondary_text_style())
                .italic(),
        ]
        .into();
        let rect = Rect::new(area.x, y, area.width, 1);
        frame.render_widget_ref(&loading_line, rect);
    }
    if show_more_below {
        let label = if state.pagination.is_loading() {
            "↓ loading more"
        } else {
            "↓ more"
        };
        frame.render_widget_ref(
            &more_line(label),
            Rect::new(
                area.x,
                area.y.saturating_add(area.height.saturating_sub(1)),
                area.width,
                1,
            ),
        );
    }
}

fn more_line(label: &'static str) -> Line<'static> {
    vec![
        label
            .fg(crate::style::accent_color_on(/*background*/ None))
            .not_dim(),
    ]
    .into()
}

pub(super) fn render_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    match state.density {
        SessionListDensity::Comfortable => {
            render_comfortable_session_lines(row, state, is_selected, is_expanded, is_zebra, width)
        }
        SessionListDensity::Dense => {
            render_dense_session_lines(row, state, is_selected, is_expanded, is_zebra, width)
        }
    }
}

pub(super) fn render_comfortable_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let row_style = if is_selected {
        dense_selected_style()
    } else if is_zebra {
        dense_zebra_style()
    } else {
        Style::default()
    };
    let marker = selection_marker(is_selected, is_expanded);
    let unread = state.unread_revision(row).is_some();
    let title = truncate_text(
        row.display_preview(),
        (width as usize).saturating_sub(marker.width() + 2 * usize::from(unread)),
    );
    let title = session_title_span(title, row.thread_id, state.use_theme_colors, is_selected);
    let mut lines = vec![session_summary_line(
        vec![marker],
        title,
        unread,
        row_style,
        width,
    )];
    if is_expanded {
        lines.extend(render_transcript_preview_lines(row, state, width));
        return lines;
    }

    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let created = format_relative_time(reference, row.created_at);
    let updated = format_relative_time(reference, row.updated_at.or(row.created_at));
    let branch = row.git_branch.as_deref();
    let cwd = row
        .cwd
        .as_ref()
        .map(|path| format_directory_display(path, /*max_width*/ None));
    let show_cwd = state.filter_mode == SessionFilterMode::All
        || row
            .cwd
            .as_deref()
            .zip(state.local_filter_cwd.as_deref())
            .is_some_and(|(row_cwd, filter_cwd)| !paths_match(row_cwd, filter_cwd));
    let footer_lines = render_footer_lines(
        state.sort_key,
        &created,
        &updated,
        branch,
        cwd.as_deref(),
        show_cwd,
        width,
    );
    lines.extend(apply_session_row_background(footer_lines, row_style, width));
    lines
}

fn apply_session_row_background(
    lines: Vec<Line<'static>>,
    style: Style,
    width: u16,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| apply_line_background(line, style, width))
        .collect()
}

fn apply_line_background(mut line: Line<'static>, style: Style, width: u16) -> Line<'static> {
    let padding = (width as usize).saturating_sub(line.width());
    if padding > 0 {
        line.spans.push(" ".repeat(padding).set_style(style));
    }
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        // Preserve each role's hue and weight, then resolve it against the painted row.
        span.style = style.patch(span.style);
        if style
            .add_modifier
            .contains(ratatui::style::Modifier::REVERSED)
        {
            span.style.fg = style.fg;
            span.style = span.style.reversed();
        } else {
            span.style.fg = Some(readable_color_on(
                span.style.fg.unwrap_or(Color::Reset),
                span.style.bg,
            ));
            span.style = span.style.not_reversed();
        }
    }
    line
}

pub(super) fn render_dense_session_lines(
    row: &Row,
    state: &PickerState,
    is_selected: bool,
    is_expanded: bool,
    is_zebra: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let marker = selection_marker(is_selected, is_expanded);
    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let created = format_relative_time(reference, row.created_at);
    let updated = format_relative_time(reference, row.updated_at.or(row.created_at));
    let date = match state.sort_key {
        ThreadSortKey::CreatedAt => created,
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt | ThreadSortKey::SectionPosition => {
            updated
        }
    };
    let mut lines = vec![dense_summary_line(DenseSummaryInput {
        marker,
        date: &date,
        title: row.display_preview(),
        unread: state.unread_revision(row).is_some(),
        thread_id: row.thread_id,
        use_theme_colors: state.use_theme_colors,
        is_selected,
        is_zebra,
        width,
    })];
    if is_expanded {
        lines.extend(render_transcript_preview_lines(row, state, width));
    }
    lines
}

pub(super) fn dense_summary_line(input: DenseSummaryInput<'_>) -> Line<'static> {
    let row_style = if input.is_selected {
        dense_selected_style()
    } else if input.is_zebra {
        dense_zebra_style()
    } else {
        Style::default()
    };
    let marker_width = input.marker.width();
    let available = (input.width as usize).saturating_sub(marker_width);
    let columns = dense_columns(available);
    let title = session_title_span(
        dense_column_text(
            input.title,
            columns
                .title_width
                .saturating_sub(2 * usize::from(input.unread)),
        ),
        input.thread_id,
        input.use_theme_colors,
        input.is_selected,
    );

    let spans = vec![
        input.marker,
        dense_column_text(input.date, columns.date_width).set_style(secondary_text_style()),
    ];
    session_summary_line(spans, title, input.unread, row_style, input.width)
}

/// Paint the title and metadata first so contrast adjustment preserves the fixed blue dot.
pub(super) fn session_summary_line(
    mut prefix: Vec<Span<'static>>,
    title: Span<'static>,
    unread: bool,
    row_style: Style,
    width: u16,
) -> Line<'static> {
    let title_index = prefix.len();
    prefix.push(title);
    let mut line = apply_line_background(
        prefix.into(),
        row_style,
        width.saturating_sub(2 * u16::from(unread)),
    );
    if unread {
        line.spans.splice(
            title_index..title_index,
            [crate::style::task_marker(unread, row_style), " ".into()],
        );
    }
    line
}

fn dense_columns(width: usize) -> DenseColumns {
    let date_width = SESSION_META_DATE_WIDTH;
    DenseColumns {
        date_width,
        title_width: width.saturating_sub(date_width),
    }
}

pub(super) fn dense_zebra_style() -> Style {
    dense_row_background_style(/*selected*/ false)
}

fn dense_selected_style() -> Style {
    selected_session_style()
}

fn dense_row_background_style(selected: bool) -> Style {
    let Some(bg) = default_bg() else {
        return Style::default();
    };
    let (overlay, alpha) = if is_light(bg) {
        ((0, 0, 0), if selected { 0.12 } else { 0.04 })
    } else {
        ((255, 255, 255), if selected { 0.12 } else { 0.055 })
    };
    Style::default().bg(best_color(blend(overlay, bg, alpha)))
}

fn dense_column_text(text: &str, width: usize) -> String {
    let text = truncate_text(text, width.saturating_sub(1));
    let padding = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
    format!("{text}{}", " ".repeat(padding))
}

pub(super) fn selection_marker(is_selected: bool, is_expanded: bool) -> Span<'static> {
    match (is_selected, is_expanded) {
        (true, true) => "⌄ ".set_style(selected_session_style().bold()),
        (true, false) => "› ".set_style(selected_session_style()),
        (false, _) => "  ".into(),
    }
}

pub(super) fn selected_session_style() -> Style {
    crate::style::selection_style()
}

fn session_title_span(
    title: String,
    thread_id: Option<ThreadId>,
    use_theme_colors: bool,
    is_selected: bool,
) -> Span<'static> {
    let title = if use_theme_colors && let Some(thread_id) = thread_id {
        title.fg(crate::thread_color::thread_color(thread_id))
    } else {
        title.into()
    };
    if is_selected { title.bold() } else { title }
}

pub(super) fn render_footer_lines(
    sort_key: ThreadSortKey,
    created: &str,
    updated: &str,
    branch: Option<&str>,
    cwd: Option<&str>,
    show_cwd: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let date = match sort_key {
        ThreadSortKey::CreatedAt => created,
        ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt | ThreadSortKey::SectionPosition => {
            updated
        }
    };
    let mut parts = vec![FooterPart::Date(date.to_string())];
    if show_cwd {
        parts.push(FooterPart::Cwd(cwd.map(str::to_string)));
    }
    parts.push(FooterPart::Branch(branch.map(str::to_string)));
    pack_footer_parts(parts, width)
}

fn pack_footer_parts(parts: Vec<FooterPart>, width: u16) -> Vec<Line<'static>> {
    let available_width = width as usize;
    if available_width <= SESSION_META_INDENT_WIDTH {
        return Vec::new();
    }
    let cwd_width = cwd_column_width(available_width);
    let all_parts_width = footer_parts_width(&parts, cwd_width);
    if all_parts_width <= available_width {
        return vec![footer_line(parts, available_width, cwd_width)];
    }

    let mut lines = Vec::with_capacity(parts.len());
    let mut current_parts = Vec::new();
    for part in parts {
        let mut candidate_parts = std::mem::take(&mut current_parts);
        candidate_parts.push(part);
        if candidate_parts.len() > 1
            && footer_parts_width(&candidate_parts, cwd_width) > available_width
        {
            let previous_parts = candidate_parts
                .drain(..candidate_parts.len().saturating_sub(1))
                .collect();
            lines.push(footer_line(previous_parts, available_width, cwd_width));
        }
        current_parts = candidate_parts;
    }
    if !current_parts.is_empty() {
        lines.push(footer_line(current_parts, available_width, cwd_width));
    }
    lines
}

fn cwd_column_width(width: usize) -> usize {
    let available = width.saturating_sub(
        SESSION_META_INDENT_WIDTH + SESSION_META_DATE_WIDTH + 2 * SESSION_META_FIELD_GAP_WIDTH,
    );
    (available / 2).clamp(SESSION_META_MIN_CWD_WIDTH, SESSION_META_MAX_CWD_WIDTH)
}

fn footer_parts_width(parts: &[FooterPart], cwd_width: usize) -> usize {
    let content_width: usize = parts
        .iter()
        .enumerate()
        .map(|(idx, part)| footer_part_width(part, idx + 1 < parts.len(), cwd_width))
        .sum();
    SESSION_META_INDENT_WIDTH + content_width
}

fn footer_part_width(part: &FooterPart, padded: bool, cwd_width: usize) -> usize {
    let prefix_width = part.prefix().map_or(0, UnicodeWidthStr::width);
    let prefix_gap_width = usize::from(part.prefix().is_some() && !part.text().is_empty());
    let text_width = UnicodeWidthStr::width(part.text());
    let actual_width = prefix_width + prefix_gap_width + text_width;
    match part {
        FooterPart::Date(_) if padded => SESSION_META_DATE_WIDTH.max(actual_width),
        FooterPart::Cwd(_) if padded => cwd_width,
        _ => actual_width,
    }
}

fn footer_line(parts: Vec<FooterPart>, width: usize, cwd_width: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec!["  ".into()];
    let mut remaining_width = width.saturating_sub(SESSION_META_INDENT_WIDTH);
    let part_count = parts.len();
    for (idx, part) in parts.into_iter().enumerate() {
        if idx > 0 {
            let gap_width = SESSION_META_FIELD_GAP_WIDTH.min(remaining_width);
            if gap_width > 0 {
                spans.push(" ".repeat(gap_width).set_style(secondary_text_style()));
                remaining_width = remaining_width.saturating_sub(gap_width);
            }
        }
        let padded = idx + 1 < part_count;
        let target_width = match part {
            FooterPart::Date(_) if padded => Some(SESSION_META_DATE_WIDTH),
            FooterPart::Cwd(_) if padded => Some(cwd_width),
            FooterPart::Date(_) | FooterPart::Branch(_) | FooterPart::Cwd(_) => None,
        };
        let used_width = push_footer_part(&mut spans, part, target_width, remaining_width);
        remaining_width = remaining_width.saturating_sub(used_width);
        if let Some(target_width) = target_width {
            let padding = target_width.saturating_sub(used_width);
            if padding > 0 {
                spans.push(" ".repeat(padding).set_style(secondary_text_style()));
                remaining_width = remaining_width.saturating_sub(padding);
            }
        }
    }
    spans.into()
}

fn push_footer_part(
    spans: &mut Vec<Span<'static>>,
    part: FooterPart,
    target_width: Option<usize>,
    available_width: usize,
) -> usize {
    let text = part.text().to_string();
    let Some(prefix) = part.prefix() else {
        let text = truncate_text(&text, available_width);
        let width = UnicodeWidthStr::width(text.as_str());
        spans.push(text.set_style(secondary_text_style()));
        return width;
    };

    let prefix_width = UnicodeWidthStr::width(prefix);
    if available_width <= prefix_width {
        let prefix = truncate_text(prefix, available_width);
        let width = UnicodeWidthStr::width(prefix.as_str());
        spans.push(prefix.set_style(secondary_text_style()));
        return width;
    }

    spans.push(prefix.set_style(secondary_text_style()));
    let mut used_width = prefix_width;
    if !text.is_empty() && used_width < available_width {
        spans.push(" ".set_style(secondary_text_style()));
        used_width += 1;
    }
    let text_width = target_width
        .unwrap_or(available_width)
        .saturating_sub(used_width)
        .min(available_width.saturating_sub(used_width));
    let text = truncate_text(&text, text_width);
    let rendered_text_width = UnicodeWidthStr::width(text.as_str());
    match part {
        FooterPart::Branch(None) | FooterPart::Cwd(None) => {
            spans.push(text.set_style(secondary_text_style()).italic())
        }
        _ => spans.push(text.set_style(secondary_text_style())),
    }
    used_width + rendered_text_width
}

fn render_empty_state_line(state: &PickerState) -> Line<'static> {
    if !state.query.is_empty() {
        if state.search_state.is_active()
            || (state.pagination.is_loading() && state.pagination.next_cursor.is_some())
        {
            return vec!["Searching…".set_style(secondary_text_style()).italic()].into();
        }
        if state.pagination.reached_scan_cap {
            let msg = format!(
                "Search scanned first {} sessions; more may exist",
                state.pagination.num_scanned_files
            );
            return vec![Span::from(msg).set_style(secondary_text_style()).italic()].into();
        }
        return vec![
            "No results for your search"
                .set_style(secondary_text_style())
                .italic(),
        ]
        .into();
    }

    if state.pagination.is_loading() {
        if state.all_rows.is_empty() && state.pagination.num_scanned_files == 0 {
            return vec![
                "Loading sessions…"
                    .set_style(secondary_text_style())
                    .italic(),
            ]
            .into();
        }
        return vec![
            "Loading older sessions…"
                .set_style(secondary_text_style())
                .italic(),
        ]
        .into();
    }

    vec!["No sessions yet".set_style(secondary_text_style()).italic()].into()
}
