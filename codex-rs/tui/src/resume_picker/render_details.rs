//! Renders expanded session details, transcript previews, and relative timestamps.

use super::*;

pub(super) fn render_transcript_preview_lines(
    row: &Row,
    state: &PickerState,
    width: u16,
) -> Vec<Line<'static>> {
    let mut details = render_expanded_session_details(row, state, width);
    let Some(thread_id) = row.thread_id else {
        return details;
    };
    let preview_lines = match state.transcript_previews.get(&thread_id) {
        Some(TranscriptPreviewState::Loading) => {
            vec![
                vec![
                    "  │ ".set_style(secondary_text_style()),
                    "Loading recent transcript..."
                        .set_style(secondary_text_style())
                        .italic(),
                ]
                .into(),
            ]
        }
        Some(TranscriptPreviewState::Failed) => vec![
            vec![
                "  │ ".set_style(secondary_text_style()),
                "Could not load transcript preview"
                    .set_style(crate::style::status_style(
                        crate::style::StatusTone::Failure,
                    ))
                    .italic(),
            ]
            .into(),
        ],
        Some(TranscriptPreviewState::Loaded(lines)) => {
            render_conversation_preview_lines(lines, width, row.cwd.as_deref())
        }
        None => Vec::new(),
    };
    details.extend(preview_lines);
    details
}

pub(super) fn render_expanded_session_details(
    row: &Row,
    state: &PickerState,
    width: u16,
) -> Vec<Line<'static>> {
    let reference = state.relative_time_reference.unwrap_or_else(Utc::now);
    let session = match (row.thread_name.as_deref(), row.thread_id) {
        (Some(thread_name), Some(thread_id)) => format!("{thread_name} ({thread_id})"),
        (Some(thread_name), None) => thread_name.to_string(),
        (None, Some(thread_id)) => thread_id.to_string(),
        (None, None) => "-".to_string(),
    };
    let directory = row
        .cwd
        .as_ref()
        .map(|path| format_directory_display(path, /*max_width*/ None))
        .unwrap_or_else(|| "-".to_string());
    let branch = format!(
        "{SESSION_META_BRANCH_ICON} {}",
        row.git_branch.as_deref().unwrap_or("no branch")
    );

    vec![
        expanded_detail_line("Session:", &session, width),
        expanded_time_detail_line(
            "Created:",
            reference,
            row.created_at,
            width,
            state.clock_format,
        ),
        expanded_time_detail_line(
            "Updated:",
            reference,
            row.updated_at.or(row.created_at),
            width,
            state.clock_format,
        ),
        expanded_detail_line("Directory:", &directory, width),
        expanded_detail_line("Branch:", &branch, width),
        vec!["  │".set_style(secondary_text_style())].into(),
        vec![
            "  │ ".set_style(secondary_text_style()),
            "Conversation:".set_style(secondary_text_style()),
        ]
        .into(),
    ]
}

fn render_conversation_preview_lines(
    lines: &[TranscriptPreviewLine],
    width: u16,
    cwd: Option<&Path>,
) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![
            vec![
                "  └ ".set_style(secondary_text_style()),
                "No transcript preview available"
                    .set_style(secondary_text_style())
                    .italic(),
            ]
            .into(),
        ];
    }

    let mut rendered = Vec::new();
    for line in lines {
        rendered.extend(render_transcript_content_lines(line, width, cwd));
    }
    let rendered_len = rendered.len();
    rendered
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix = if idx + 1 == rendered_len {
                "  └ "
            } else {
                "  │ "
            };
            prefix_transcript_line(prefix, line)
        })
        .collect()
}

fn render_transcript_content_lines(
    line: &TranscriptPreviewLine,
    width: u16,
    cwd: Option<&Path>,
) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(4) as usize;
    let lines = match line.speaker {
        TranscriptPreviewSpeaker::User => vec![conversation_content_line(
            Line::from(line.text.clone()),
            conversation_user_style(),
        )],
        TranscriptPreviewSpeaker::Assistant => {
            let mut lines = render_assistant(
                &line.text,
                /*width*/ None,
                cwd,
                &|_| false,
                crate::markdown_render::ListSpacing::AfterMultiline,
            )
            .lines
            .into_iter()
            .map(|line| line.line)
            .collect::<Vec<_>>();
            for line in &mut lines {
                *line = conversation_content_line(line.clone(), conversation_assistant_style());
            }
            lines
        }
    };
    adaptive_wrap_lines(lines, RtOptions::new(content_width.max(/*other*/ 1)))
}

fn conversation_content_line(mut line: Line<'static>, style: Style) -> Line<'static> {
    line.style = line.style.patch(style);
    for span in &mut line.spans {
        span.style = span.style.patch(style);
    }
    line
}

fn prefix_transcript_line(prefix: &'static str, line: Line<'static>) -> Line<'static> {
    let mut spans = vec![prefix.set_style(transcript_prefix_style(&line))];
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

fn transcript_prefix_style(line: &Line<'_>) -> Style {
    let style = line
        .spans
        .iter()
        .find(|span| !span.content.trim().is_empty())
        .map(|span| line.style.patch(span.style))
        .unwrap_or(line.style);
    connector_style_from_content(style)
}

fn connector_style_from_content(style: Style) -> Style {
    Style {
        fg: style.fg,
        bg: style.bg,
        ..Style::default()
    }
}

fn conversation_assistant_style() -> Style {
    secondary_text_style()
}

fn conversation_user_style() -> Style {
    Style::default()
        .fg(readable_color_on(Color::Reset, /*background*/ None))
        .not_dim()
        .italic()
}

fn expanded_detail_line(label: &'static str, value: &str, width: u16) -> Line<'static> {
    const LABEL_WIDTH: usize = 10;
    let prefix_width = 4;
    let gap_width = 2;
    let value_width = (width as usize)
        .saturating_sub(prefix_width + LABEL_WIDTH + gap_width)
        .max(1);
    vec![
        "  │ ".set_style(secondary_text_style()),
        format!("{label:<LABEL_WIDTH$}").set_style(secondary_text_style()),
        "  ".set_style(secondary_text_style()),
        truncate_text(value, value_width).into(),
    ]
    .into()
}

fn expanded_time_detail_line(
    label: &'static str,
    reference: DateTime<Utc>,
    ts: Option<DateTime<Utc>>,
    width: u16,
    clock_format: ClockFormat,
) -> Line<'static> {
    let Some(ts) = ts else {
        return expanded_detail_line(label, "-", width);
    };
    let value = format!(
        "{} · {}",
        format_relative_time_long(reference, ts),
        format_timestamp(ts, clock_format)
    );
    expanded_detail_line(label, &value, width)
}

pub(crate) fn format_relative_time(reference: DateTime<Utc>, ts: Option<DateTime<Utc>>) -> String {
    let Some(ts) = ts else {
        return "-".to_string();
    };
    let seconds = (reference - ts).num_seconds().max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

pub(super) fn format_relative_time_long(reference: DateTime<Utc>, ts: DateTime<Utc>) -> String {
    let seconds = (reference - ts).num_seconds().max(0);
    if seconds == 0 {
        return "now".to_string();
    }
    if seconds < 60 {
        return plural_time(seconds, "second");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return plural_time(minutes, "minute");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return plural_time(hours, "hour");
    }
    plural_time(hours / 24, "day")
}

fn plural_time(value: i64, unit: &str) -> String {
    if value == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{value} {unit}s ago")
    }
}

fn format_timestamp(ts: DateTime<Utc>, clock_format: ClockFormat) -> String {
    ts.format(match clock_format {
        ClockFormat::TwelveHour => "%Y-%m-%d %-I:%M:%S %p",
        ClockFormat::TwentyFourHour => "%Y-%m-%d %H:%M:%S",
    })
    .to_string()
}
