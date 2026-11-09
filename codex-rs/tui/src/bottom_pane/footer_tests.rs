use super::*;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::test_backend::VT100Backend;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::backend::TestBackend;

#[test]
fn voice_live_microphone_indicator_is_red() {
    let line = footer_hint_items_line(&[("voice".into(), "● listen".into())]);
    assert_eq!(line.spans[1].style.fg, Some(ratatui::style::Color::Red));
}

#[test]
fn voice_footer_rendering_preserves_text_and_styles() {
    let items = [
        ("voice".into(), "● listen".into()),
        ("ctrl+m".into(), "mute".into()),
    ];
    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 40, /*height*/ 1)).expect("create terminal");
    terminal
        .draw(|frame| render_footer_hint_items(frame.area(), frame.buffer_mut(), &items))
        .expect("render voice footer");
    let backend = terminal.backend();
    let mut previous_style = None;
    let style_runs = (0..40)
        .filter_map(|x| {
            let cell = backend.buffer().cell((x, 0))?;
            let style = cell.style();
            if previous_style == Some(style) {
                None
            } else {
                previous_style = Some(style);
                Some((x, style))
            }
        })
        .collect::<Vec<_>>();
    insta::assert_debug_snapshot!(
        "voice_footer_rendered_styles",
        (backend.to_string(), style_runs)
    );
}

fn snapshot_footer(name: &str, props: FooterProps) {
    snapshot_footer_with_mode_indicator(
        name, /*width*/ 80, &props, /*collaboration_mode_indicator*/ None,
    );
}

fn snapshot_footer_with_context(
    name: &str,
    props: FooterProps,
    percent: Option<i64>,
    used_tokens: Option<i64>,
) {
    snapshot_footer_with_mode_indicator_and_context(
        name,
        /*width*/ 80,
        &props,
        /*collaboration_mode_indicator*/ None,
        context_window_line(percent, used_tokens),
    );
}

fn draw_footer_frame<B: Backend>(
    terminal: &mut Terminal<B>,
    height: u16,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ide_context_active: bool,
    context_line: Line<'static>,
) {
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, f.area().width, height);
            let show_cycle_hint = !props.is_task_running;
            let show_shortcuts_hint = match props.mode {
                FooterMode::ComposerEmpty => true,
                FooterMode::ComposerHasDraft => false,
                FooterMode::HistorySearch
                | FooterMode::QuitShortcutReminder
                | FooterMode::ShortcutOverlay
                | FooterMode::EscHint => false,
            };
            let show_queue_hint = match props.mode {
                FooterMode::ComposerHasDraft => props.is_task_running,
                FooterMode::HistorySearch
                | FooterMode::QuitShortcutReminder
                | FooterMode::ComposerEmpty
                | FooterMode::ShortcutOverlay
                | FooterMode::EscHint => false,
            };
            let status_line_active = uses_passive_footer_status_layout(props);
            let passive_status_line = if status_line_active {
                passive_footer_status_line(props).map(|content| content.into_hyperlink_line().line)
            } else {
                None
            };
            let left_mode_indicator = if status_line_active {
                None
            } else {
                collaboration_mode_indicator
            };
            let available_width = area.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
            let mut truncated_status_line = if status_line_active
                && matches!(
                    props.mode,
                    FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                ) {
                passive_status_line.as_ref().map(|line| {
                    truncate_line_with_ellipsis_if_overflow(line.clone(), available_width)
                })
            } else {
                None
            };
            let mut left_width = if status_line_active {
                truncated_status_line
                    .as_ref()
                    .map(|line| line.width() as u16)
                    .unwrap_or(0)
            } else {
                footer_line_width(
                    props,
                    left_mode_indicator,
                    show_cycle_hint,
                    show_shortcuts_hint,
                    show_queue_hint,
                )
            };
            let right_line = if status_line_active {
                let full = status_line_right_indicator_line(
                    collaboration_mode_indicator,
                    /*goal_status_indicator*/ None,
                    ide_context_active,
                    show_cycle_hint,
                );
                let compact = status_line_right_indicator_line(
                    collaboration_mode_indicator,
                    /*goal_status_indicator*/ None,
                    ide_context_active,
                    /*show_cycle_hint*/ false,
                );
                let full_width = full.as_ref().map(|line| line.width() as u16).unwrap_or(0);
                if can_show_left_with_context(area, left_width, full_width) {
                    full
                } else {
                    compact
                }
            } else {
                Some(context_line.clone())
            };
            let right_width = right_line
                .as_ref()
                .map(|line| line.width() as u16)
                .unwrap_or(0);
            if status_line_active
                && let Some(max_left) = max_left_width_for_right(area, right_width)
                && left_width > max_left
                && let Some(line) = passive_status_line.as_ref().map(|line| {
                    truncate_line_with_ellipsis_if_overflow(line.clone(), max_left as usize)
                })
            {
                left_width = line.width() as u16;
                truncated_status_line = Some(line);
            }
            let can_show_left_and_context =
                can_show_left_with_context(area, left_width, right_width);
            if matches!(
                props.mode,
                FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
            ) {
                if status_line_active {
                    if let Some(line) = truncated_status_line.clone() {
                        render_footer_line(area, f.buffer_mut(), line);
                    }
                    if can_show_left_and_context && let Some(line) = &right_line {
                        render_context_right(area, f.buffer_mut(), line);
                    }
                } else {
                    let (summary_left, show_context) = single_line_footer_layout(
                        area,
                        right_width,
                        left_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                        props.key_hints,
                    );
                    match summary_left {
                        SummaryLeft::Default => {
                            render_footer_from_props(
                                area,
                                f.buffer_mut(),
                                props,
                                left_mode_indicator,
                                show_cycle_hint,
                                show_shortcuts_hint,
                                show_queue_hint,
                            );
                        }
                        SummaryLeft::Custom(line) => {
                            render_footer_line(area, f.buffer_mut(), line);
                        }
                        SummaryLeft::None => {}
                    }
                    if show_context && let Some(line) = &right_line {
                        render_context_right(area, f.buffer_mut(), line);
                    }
                }
            } else {
                render_footer_from_props(
                    area,
                    f.buffer_mut(),
                    props,
                    left_mode_indicator,
                    show_cycle_hint,
                    show_shortcuts_hint,
                    show_queue_hint,
                );
                let show_context = can_show_left_and_context
                    && !matches!(
                        props.mode,
                        FooterMode::EscHint
                            | FooterMode::HistorySearch
                            | FooterMode::QuitShortcutReminder
                            | FooterMode::ShortcutOverlay
                    );
                if show_context && let Some(line) = &right_line {
                    render_context_right(area, f.buffer_mut(), line);
                }
            }
        })
        .unwrap();
}

fn snapshot_footer_with_mode_indicator(
    name: &str,
    width: u16,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
) {
    snapshot_footer_with_mode_indicator_and_context(
        name,
        width,
        props,
        collaboration_mode_indicator,
        context_window_line(/*percent*/ None, /*used_tokens*/ None),
    );
}

fn snapshot_footer_with_mode_indicator_and_context(
    name: &str,
    width: u16,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    context_line: Line<'static>,
) {
    let height = footer_height(props, width).max(/*other*/ 1);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    draw_footer_frame(
        &mut terminal,
        height,
        props,
        collaboration_mode_indicator,
        /*ide_context_active*/ false,
        context_line,
    );
    assert_snapshot!(name, terminal.backend());
}

fn render_footer_with_mode_indicator_and_context(
    width: u16,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    context_line: Line<'static>,
) -> String {
    let height = footer_height(props, width).max(/*other*/ 1);
    let mut terminal = Terminal::new(VT100Backend::new(width, height)).expect("terminal");
    draw_footer_frame(
        &mut terminal,
        height,
        props,
        collaboration_mode_indicator,
        /*ide_context_active*/ false,
        context_line,
    );
    terminal.backend().vt100().screen().contents()
}

fn snapshot_footer_with_indicators(
    name: &str,
    width: u16,
    props: &FooterProps,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    ide_context_active: bool,
) {
    let height = footer_height(props, width).max(/*other*/ 1);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    draw_footer_frame(
        &mut terminal,
        height,
        props,
        collaboration_mode_indicator,
        ide_context_active,
        context_window_line(/*percent*/ None, /*used_tokens*/ None),
    );
    assert_snapshot!(name, terminal.backend());
}

#[test]
fn footer_snapshots() {
    snapshot_footer(
        "footer_shortcuts_default",
        FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    snapshot_footer(
        "footer_shortcuts_shift_and_esc",
        FooterProps {
            mode: FooterMode::ShortcutOverlay,
            esc_backtrack_hint: true,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints {
                insert_newline: Some(key_hint::shift(KeyCode::Enter).into()),
                ..FooterKeyHints::default_bindings()
            },
            active_agent_label: None,
        },
    );

    snapshot_footer(
        "footer_shortcuts_collaboration_modes_enabled",
        FooterProps {
            mode: FooterMode::ShortcutOverlay,
            esc_backtrack_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: true,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    snapshot_footer(
        "footer_shortcuts_running",
        FooterProps {
            mode: FooterMode::ShortcutOverlay,
            esc_backtrack_hint: false,
            is_task_running: true,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    snapshot_footer(
        "footer_ctrl_c_quit_idle",
        FooterProps {
            mode: FooterMode::QuitShortcutReminder,
            esc_backtrack_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    snapshot_footer(
        "footer_esc_hint_idle",
        FooterProps {
            mode: FooterMode::EscHint,
            esc_backtrack_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    snapshot_footer(
        "footer_esc_hint_primed",
        FooterProps {
            mode: FooterMode::EscHint,
            esc_backtrack_hint: true,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    snapshot_footer_with_context(
        "footer_shortcuts_context_running",
        FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            is_task_running: true,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
        Some(72),
        /*used_tokens*/ None,
    );

    snapshot_footer_with_context(
        "footer_context_tokens_used",
        FooterProps {
            mode: FooterMode::ComposerEmpty,
            esc_backtrack_hint: false,
            is_task_running: false,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
        /*percent*/ None,
        Some(123_456),
    );

    snapshot_footer(
        "footer_composer_has_draft_queue_hint_enabled",
        FooterProps {
            mode: FooterMode::ComposerHasDraft,
            esc_backtrack_hint: false,
            is_task_running: true,
            queue_submissions: false,
            collaboration_modes_enabled: false,
            is_wsl: false,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            status_line_value: None,
            status_line_enabled: false,
            key_hints: FooterKeyHints::default_bindings(),
            active_agent_label: None,
        },
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: true,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: None,
        status_line_enabled: false,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer_with_mode_indicator(
        "footer_mode_indicator_wide",
        /*width*/ 120,
        &props,
        Some(CollaborationModeIndicator::Plan),
    );

    snapshot_footer_with_mode_indicator(
        "footer_mode_indicator_narrow_overlap_hides",
        /*width*/ 40,
        &props,
        Some(CollaborationModeIndicator::Plan),
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: true,
        queue_submissions: false,
        collaboration_modes_enabled: true,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: None,
        status_line_enabled: false,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer_with_mode_indicator(
        "footer_mode_indicator_running_hides_hint",
        /*width*/ 120,
        &props,
        Some(CollaborationModeIndicator::Plan),
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: false,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: Some(Line::from("Status line content".to_string()).into()),
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer("footer_status_line_overrides_shortcuts", props);

    let props = FooterProps {
        mode: FooterMode::ComposerHasDraft,
        esc_backtrack_hint: false,
        is_task_running: true,
        queue_submissions: false,
        collaboration_modes_enabled: false,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: Some(Line::from("Status line content".to_string()).into()),
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer("footer_status_line_yields_to_queue_hint", props);

    let props = FooterProps {
        mode: FooterMode::ComposerHasDraft,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: false,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: Some(Line::from("Status line content".to_string()).into()),
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer("footer_status_line_overrides_draft_idle", props);

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: true,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: None, // command timed out / empty
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer_with_mode_indicator_and_context(
        "footer_status_line_enabled_mode_right",
        /*width*/ 120,
        &props,
        Some(CollaborationModeIndicator::Plan),
        context_window_line(Some(50), /*used_tokens*/ None),
    );

    snapshot_footer_with_indicators(
        "footer_status_line_enabled_mode_and_ide_context_right",
        /*width*/ 120,
        &props,
        Some(CollaborationModeIndicator::Plan),
        /*ide_context_active*/ true,
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: true,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: None,
        status_line_enabled: false,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer_with_mode_indicator_and_context(
        "footer_status_line_disabled_context_right",
        /*width*/ 120,
        &props,
        Some(CollaborationModeIndicator::Plan),
        context_window_line(Some(50), /*used_tokens*/ None),
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: false,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: None,
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    // has status line and no collaboration mode
    snapshot_footer_with_mode_indicator_and_context(
        "footer_status_line_enabled_no_mode_right",
        /*width*/ 120,
        &props,
        /*collaboration_mode_indicator*/ None,
        context_window_line(Some(50), /*used_tokens*/ None),
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: true,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: Some(
            Line::from(
                "Status line content that should truncate before the mode indicator".to_string(),
            )
            .into(),
        ),
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    snapshot_footer_with_mode_indicator_and_context(
        "footer_status_line_truncated_with_gap",
        /*width*/ 40,
        &props,
        Some(CollaborationModeIndicator::Plan),
        context_window_line(Some(50), /*used_tokens*/ None),
    );

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: false,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: None,
        status_line_enabled: false,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: Some("Robie [explorer]".to_string()),
    };

    snapshot_footer("footer_active_agent_label", props);

    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: false,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: Some(Line::from("Status line content".to_string()).into()),
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: Some("Robie [explorer]".to_string()),
    };

    snapshot_footer("footer_status_line_with_active_agent_label", props);
}

#[test]
fn footer_status_line_truncates_to_keep_mode_indicator() {
    let props = FooterProps {
        mode: FooterMode::ComposerEmpty,
        esc_backtrack_hint: false,
        is_task_running: false,
        queue_submissions: false,
        collaboration_modes_enabled: true,
        is_wsl: false,
        quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
        status_line_value: Some(
            Line::from(
                "Status line content that is definitely too long to fit alongside the mode label"
                    .to_string(),
            )
            .into(),
        ),
        status_line_enabled: true,
        key_hints: FooterKeyHints::default_bindings(),
        active_agent_label: None,
    };

    let screen = render_footer_with_mode_indicator_and_context(
        /*width*/ 80,
        &props,
        Some(CollaborationModeIndicator::Plan),
        context_window_line(Some(50), /*used_tokens*/ None),
    );
    let collapsed = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("Plan mode"),
        "mode indicator should remain visible"
    );
    assert!(
        !collapsed.contains("⇧tab to cycle"),
        "compact mode indicator should be used when space is tight"
    );
    assert!(
        screen.contains('…'),
        "status line should be truncated with ellipsis to keep mode indicator"
    );
}
