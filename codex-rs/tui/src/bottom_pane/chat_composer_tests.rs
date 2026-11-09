use super::*;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use image::ImageBuffer;
use image::Rgba;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use tempfile::tempdir;

use crate::app_event::AppEvent;

use crate::bottom_pane::AppEventSender;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::chat_composer::LARGE_PASTE_CHAR_THRESHOLD;
use crate::bottom_pane::textarea::TextArea;
use codex_protocol::models::local_image_label_text;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

pub(super) fn new_test_composer() -> (ChatComposer, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    (
        ChatComposer::new(
            /*has_input_focus*/ true,
            sender,
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        ),
        rx,
    )
}

#[test]
fn parent_owned_thread_placeholder_snapshot() {
    snapshot_composer_state(
        "parent_owned_thread_placeholder",
        /*enhanced_keys_supported*/ false,
        ChatComposer::set_parent_owned_thread,
    );
}

#[test]
fn light_terminal_palette_renders_light_composer_snapshot() {
    let colors = crate::terminal_probe::DefaultColors {
        fg: (0x55, 0x57, 0x53),
        bg: (0xff, 0xff, 0xff),
    };

    crate::terminal_palette::with_test_default_colors(colors, || {
        let (composer, _rx) = new_test_composer();
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 48, /*height*/ 6,
        );
        let mut buffer = Buffer::empty(area);
        composer.render(area, &mut buffer);

        assert_eq!(
            buffer[(0, 1)].bg,
            crate::terminal_palette::rgb_color((244, 244, 244))
        );
        insta::assert_snapshot!("light_terminal_palette_composer", format!("{buffer:?}"));
    });
}

#[test]
fn footer_hint_row_is_separated_from_composer() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let row_to_string = |y: u16| {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        row
    };

    let mut hint_row: Option<(u16, String)> = None;
    for y in 0..area.height {
        let row = row_to_string(y);
        if row.contains("? for shortcuts") {
            hint_row = Some((y, row));
            break;
        }
    }

    let (hint_row_idx, hint_row_contents) =
        hint_row.expect("expected footer hint row to be rendered");
    assert_eq!(
        hint_row_idx,
        area.height - 1,
        "hint row should occupy the bottom line: {hint_row_contents:?}",
    );

    assert!(
        hint_row_idx > 0,
        "expected a spacing row above the footer hints",
    );

    let spacing_row = row_to_string(hint_row_idx - 1);
    assert_eq!(
        spacing_row.trim(),
        "",
        "expected blank spacing row above hints but saw: {spacing_row:?}",
    );
}

#[test]
fn footer_flash_overrides_footer_hint_override() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_footer_hint_override(Some(vec![("K".to_string(), "label".to_string())]));
    composer.show_footer_flash(Line::from("FLASH"), Duration::from_secs(10));

    let area = Rect::new(0, 0, 60, 6);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let mut bottom_row = String::new();
    for x in 0..area.width {
        bottom_row.push(
            buf[(x, area.height - 1)]
                .symbol()
                .chars()
                .next()
                .unwrap_or(' '),
        );
    }
    assert!(
        bottom_row.contains("FLASH"),
        "expected flash content to render in footer row, saw: {bottom_row:?}",
    );
    assert!(
        !bottom_row.contains("K label"),
        "expected flash to override hint override, saw: {bottom_row:?}",
    );
}

#[test]
fn footer_flash_expires_and_falls_back_to_hint_override() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_footer_hint_override(Some(vec![("K".to_string(), "label".to_string())]));
    composer.show_footer_flash(Line::from("FLASH"), Duration::from_secs(10));
    composer.footer.flash.as_mut().unwrap().expires_at = Instant::now() - Duration::from_secs(1);

    let area = Rect::new(0, 0, 60, 6);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let mut bottom_row = String::new();
    for x in 0..area.width {
        bottom_row.push(
            buf[(x, area.height - 1)]
                .symbol()
                .chars()
                .next()
                .unwrap_or(' '),
        );
    }
    assert!(
        bottom_row.contains("K label"),
        "expected hint override to render after flash expired, saw: {bottom_row:?}",
    );
    assert!(
        !bottom_row.contains("FLASH"),
        "expected expired flash to be hidden, saw: {bottom_row:?}",
    );
}

pub(super) fn snapshot_composer_state_with_width<F>(
    name: &str,
    width: u16,
    enhanced_keys_supported: bool,
    setup: F,
) where
    F: FnOnce(&mut ChatComposer),
{
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        enhanced_keys_supported,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    setup(&mut composer);
    let footer_props = composer.footer_props();
    let footer_lines = footer_height(&footer_props, width);
    let height = footer_lines + 8;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| composer.render(f.area(), f.buffer_mut()))
        .unwrap();
    insta::assert_snapshot!(name, terminal.backend());
}

pub(super) fn snapshot_composer_state<F>(name: &str, enhanced_keys_supported: bool, setup: F)
where
    F: FnOnce(&mut ChatComposer),
{
    snapshot_composer_state_with_width(name, /*width*/ 100, enhanced_keys_supported, setup);
}

#[test]
fn footer_mode_snapshots() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    snapshot_composer_state(
        "footer_mode_shortcut_overlay",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_esc_backtrack_hint(/*show*/ true);
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        },
    );

    snapshot_composer_state(
        "footer_mode_shortcut_overlay_queue_submissions",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_queue_submissions(/*queue_submissions*/ true);
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        },
    );

    snapshot_composer_state(
        "footer_mode_ctrl_c_quit",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.show_quit_shortcut_hint(
                key_hint::ctrl(KeyCode::Char('c')),
                /*has_focus*/ true,
            );
        },
    );

    snapshot_composer_state(
        "footer_mode_ctrl_c_then_esc_hint",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.show_quit_shortcut_hint(
                key_hint::ctrl(KeyCode::Char('c')),
                /*has_focus*/ true,
            );
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        },
    );

    snapshot_composer_state(
        "footer_mode_esc_hint_from_overlay",
        /*enhanced_keys_supported*/ true,
        |composer| {
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        },
    );

    snapshot_composer_state(
        "footer_mode_esc_hint_backtrack",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_esc_backtrack_hint(/*show*/ true);
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        },
    );

    snapshot_composer_state(
        "footer_mode_overlay_then_external_esc_hint",
        /*enhanced_keys_supported*/ true,
        |composer| {
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
            composer.set_esc_backtrack_hint(/*show*/ true);
        },
    );

    snapshot_composer_state(
        "footer_mode_hidden_while_typing",
        /*enhanced_keys_supported*/ true,
        |composer| {
            type_chars_humanlike(composer, &['h']);
        },
    );

    snapshot_composer_state(
        "footer_mode_history_search",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer
                .history
                .record_local_submission(HistoryEntry::new("cargo test".to_string()));
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
            let _ =
                composer.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        },
    );

    snapshot_composer_state(
        "footer_mode_history_search_unavailable",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_text_content("draft".to_string(), Vec::new(), Vec::new());
            composer.begin_history_search();
            for ch in ['g', 'i', 't'] {
                let _ =
                    composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
            composer.apply_history_search_result(HistorySearchResult::Pending);
            composer.apply_history_search_result(HistorySearchResult::Unavailable);
        },
    );

    snapshot_composer_state(
        "footer_mode_shell_command_absorbs_bang",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_status_line_enabled(/*enabled*/ true);
            composer.set_status_line(Some(
                Line::from("gpt-5.4 high fast · ~/code/codex-1 · Context 0% used").into(),
            ));
            composer.set_text_content("!git status".to_string(), Vec::new(), Vec::new());
        },
    );

    snapshot_composer_state(
        "footer_mode_shell_command_escape_exits_empty_mode",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_status_line_enabled(/*enabled*/ true);
            composer.set_status_line(Some(
                Line::from("gpt-5.4 high fast · ~/code/codex-1 · Context 0% used").into(),
            ));
            composer.set_text_content("!".to_string(), Vec::new(), Vec::new());
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        },
    );
}

#[test]
fn shell_command_cursor_uses_absorbed_prefix() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 40, 5);

    composer.set_text_content("!git".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    assert_eq!(composer.cursor_pos(area), Some((5, 1)));

    composer.set_text_content("! git".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    assert_eq!(composer.cursor_pos(area), Some((6, 1)));
}

#[test]
fn shell_command_uses_shell_accent_style() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_status_line_enabled(/*enabled*/ true);
    composer.set_status_line(Some(
        Line::from("gpt-5.4 high fast · ~/code/codex-1 · Context 0% used").into(),
    ));
    composer.set_text_content("!git status".to_string(), Vec::new(), Vec::new());

    let area = Rect::new(0, 0, 100, 9);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let prompt_cell = &buf[(0, 1)];
    assert_eq!(prompt_cell.symbol(), "!");
    assert_eq!(prompt_cell.style().fg, Some(Color::LightRed));

    let footer_y = area.height - 1;
    let footer_text = (0..area.width)
        .map(|x| buf[(x, footer_y)].symbol().chars().next().unwrap_or(' '))
        .collect::<String>();
    let shell_label_x = footer_text
        .find("Shell mode")
        .expect("expected shell mode footer label");
    assert_eq!(
        buf[(shell_label_x as u16, footer_y)].style().fg,
        Some(Color::LightRed)
    );
}

fn plugin_mention_foreground_color(composer: &ChatComposer) -> Option<Color> {
    let area = Rect::new(0, 0, 40, 5);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let textarea_row = 1;
    let row_text = (0..area.width)
        .map(|x| {
            buf[(x, textarea_row)]
                .symbol()
                .chars()
                .next()
                .unwrap_or(' ')
        })
        .collect::<String>();
    let mention_x = row_text
        .find("@sample")
        .expect("expected plugin mention in composer row");
    buf[(mention_x as u16, textarea_row)].style().fg
}

#[test]
fn plugin_at_mentions_use_plugin_accent_style() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content_with_mention_bindings(
        "@sample plugin".to_string(),
        Vec::new(),
        Vec::new(),
        vec![MentionBinding {
            sigil: '@',
            mention: "sample".to_string(),
            path: "plugin://sample@test".to_string(),
        }],
    );

    assert_eq!(
        plugin_mention_foreground_color(&composer),
        Some(Color::Magenta)
    );
}

#[test]
fn plugin_at_mentions_render_with_plugin_accent_snapshot() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content_with_mention_bindings(
        "@sample plugin".to_string(),
        Vec::new(),
        Vec::new(),
        vec![MentionBinding {
            sigil: '@',
            mention: "sample".to_string(),
            path: "plugin://sample@test".to_string(),
        }],
    );

    let area = Rect::new(0, 0, 40, 5);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let textarea_row = 1;
    let mut text = String::new();
    let mut magenta = String::new();
    for x in 0..area.width {
        let cell = &buf[(x, textarea_row)];
        text.push(cell.symbol().chars().next().unwrap_or(' '));
        magenta.push(if cell.style().fg == Some(Color::Magenta) {
            '^'
        } else {
            ' '
        });
    }
    while text.ends_with(' ') {
        text.pop();
    }
    while magenta.ends_with(' ') {
        magenta.pop();
    }

    insta::assert_snapshot!(
        "plugin_at_mentions_render_with_plugin_accent",
        format!("text:    {text}\nmagenta: {magenta}")
    );
}

#[test]
fn recalled_plugin_at_mentions_keep_plugin_accent_style() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content_with_mention_bindings(
        "@sample plugin".to_string(),
        Vec::new(),
        Vec::new(),
        vec![MentionBinding {
            sigil: '@',
            mention: "sample".to_string(),
            path: "plugin://sample@test".to_string(),
        }],
    );
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    composer.set_text_content(String::new(), Vec::new(), Vec::new());
    let (_, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(needs_redraw);

    assert_eq!(
        plugin_mention_foreground_color(&composer),
        Some(Color::Magenta)
    );
}

#[test]
fn status_line_hyperlink_marks_pr_number_cells() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let url = "https://github.com/openai/codex/pull/20252";
    composer.set_status_line_enabled(/*enabled*/ true);
    composer.set_status_line(
        crate::bottom_pane::status_line_from_segments(
            [(
                crate::bottom_pane::status_line_setup::StatusLineItem::PullRequestNumber,
                "PR #20252".to_string(),
            )],
            /*use_theme_colors*/ false,
            /*thread_id*/ None,
            Some(url),
        )
        .map(Into::into),
    );

    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let marked_cells = (area.top()..area.bottom())
        .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| buf[(x, y)].symbol().contains(url))
        .count();
    assert_eq!(marked_cells, "PR #20252".chars().count());
}

#[test]
fn deleting_prefix_keeps_bang_literal_on_submission() {
    for disable_paste_burst in [false, true] {
        for should_queue in [false, true] {
            let (mut composer, _rx) = new_test_composer();
            composer.set_disable_paste_burst(disable_paste_burst);
            composer.set_task_running(should_queue);
            type_chars_humanlike(&mut composer, &"Yes! I agree.".chars().collect::<Vec<_>>());
            composer.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
            for _ in 0..3 {
                composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
            }
            assert!(!composer.draft.is_bash_mode);
            assert_eq!(composer.current_text(), "! I agree.");
            // Moving and inserting elsewhere must not reinterpret the exposed bang either.
            composer.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
            type_chars_humanlike(&mut composer, &['!']);
            assert!(!composer.draft.is_bash_mode);
            assert_eq!(
                composer
                    .handle_key_event(KeyEvent::new(
                        if should_queue {
                            KeyCode::Tab
                        } else {
                            KeyCode::Enter
                        },
                        KeyModifiers::NONE,
                    ))
                    .0,
                InputResult::Queued {
                    text: "! I agree.!".to_owned(),
                    text_elements: Vec::new(),
                    action: QueuedInputAction::Literal,
                    pending_pastes: Vec::new(),
                }
            );
        }
    }
}

#[test]
fn deleting_prefix_keeps_bang_editable_snapshot() {
    snapshot_composer_state_with_width(
        "deleting_prefix_keeps_bang_literal",
        /*width*/ 60,
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_disable_paste_burst(/*disabled*/ true);
            composer.handle_paste("Yes! I agree.".to_owned());
            composer.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
            for _ in 0..3 {
                composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
            }
        },
    );
    let (mut composer, _rx) = new_test_composer();
    composer.handle_paste("Yes! I agree.".to_owned());
    composer.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    for _ in 0..4 {
        composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    }
    assert_eq!(composer.current_text(), " I agree.");
    assert!(!composer.draft.is_bash_mode);
}

#[test]
fn esc_exits_empty_shell_mode() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['!']);
    assert!(composer.draft.is_bash_mode);
    assert_eq!(composer.current_text(), "!");

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert!(needs_redraw);
    assert!(!composer.draft.is_bash_mode);
    assert_eq!(composer.current_text(), "");
}

#[test]
fn esc_keeps_shell_mode_when_paste_burst_flushes_pending_text() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['!']);
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    assert!(composer.is_in_paste_burst());
    assert_eq!(composer.current_text(), "!");

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert!(needs_redraw);
    assert!(composer.draft.is_bash_mode);
    assert_eq!(composer.current_text(), "!g");
}

#[test]
fn footer_collapse_snapshots() {
    fn setup_collab_footer(
        composer: &mut ChatComposer,
        context_percent: i64,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        composer.set_collaboration_modes_enabled(/*enabled*/ true);
        composer.set_collaboration_mode_indicator(indicator);
        composer.set_context_window(Some(context_percent), /*used_tokens*/ None);
    }

    // Empty textarea, agent idle: shortcuts hint can show, and cycle hint is hidden.
    snapshot_composer_state_with_width(
        "footer_collapse_empty_full",
        /*width*/ 120,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 100, /*indicator*/ None,
            );
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_empty_mode_cycle_with_context",
        /*width*/ 60,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 100, /*indicator*/ None,
            );
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_empty_mode_cycle_without_context",
        /*width*/ 44,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 100, /*indicator*/ None,
            );
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_empty_mode_only",
        /*width*/ 26,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 100, /*indicator*/ None,
            );
        },
    );

    // Empty textarea, plan mode idle: shortcuts hint and cycle hint are available.
    snapshot_composer_state_with_width(
        "footer_collapse_plan_empty_full",
        /*width*/ 120,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 100,
                Some(CollaborationModeIndicator::Plan),
            );
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_empty_mode_cycle_with_context",
        /*width*/ 60,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 100,
                Some(CollaborationModeIndicator::Plan),
            );
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_empty_mode_cycle_without_context",
        /*width*/ 44,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 100,
                Some(CollaborationModeIndicator::Plan),
            );
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_empty_mode_only",
        /*width*/ 26,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 100,
                Some(CollaborationModeIndicator::Plan),
            );
        },
    );

    // Textarea has content, agent running: queue hint is shown.
    snapshot_composer_state_with_width(
        "footer_collapse_queue_full",
        /*width*/ 120,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 98, /*indicator*/ None,
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_queue_short_with_context",
        /*width*/ 50,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 98, /*indicator*/ None,
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_queue_message_without_context",
        /*width*/ 40,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 98, /*indicator*/ None,
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_queue_short_without_context",
        /*width*/ 30,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 98, /*indicator*/ None,
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_queue_mode_only",
        /*width*/ 20,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer, /*context_percent*/ 98, /*indicator*/ None,
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );

    // Textarea has content, plan mode active, agent running: queue hint + mode.
    snapshot_composer_state_with_width(
        "footer_collapse_plan_queue_full",
        /*width*/ 120,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 98,
                Some(CollaborationModeIndicator::Plan),
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_queue_short_with_context",
        /*width*/ 50,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 98,
                Some(CollaborationModeIndicator::Plan),
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_queue_message_without_context",
        /*width*/ 40,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 98,
                Some(CollaborationModeIndicator::Plan),
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_queue_short_without_context",
        /*width*/ 30,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 98,
                Some(CollaborationModeIndicator::Plan),
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
    snapshot_composer_state_with_width(
        "footer_collapse_plan_queue_mode_only",
        /*width*/ 20,
        /*enhanced_keys_supported*/ true,
        |composer| {
            setup_collab_footer(
                composer,
                /*context_percent*/ 98,
                Some(CollaborationModeIndicator::Plan),
            );
            composer.set_task_running(/*running*/ true);
            composer.set_text_content("Test".to_string(), Vec::new(), Vec::new());
        },
    );
}

#[test]
fn esc_hint_stays_hidden_with_draft_content() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['d']);

    assert!(!composer.is_empty());
    assert_eq!(composer.current_text(), "d");
    assert_eq!(composer.footer.mode, FooterMode::ComposerEmpty);
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(composer.footer.mode, FooterMode::ComposerEmpty);
    assert!(!composer.footer.esc_backtrack_hint);
}

#[test]
fn empty_vim_insert_escape_enters_normal_without_esc_hint() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_vim_enabled(/*enabled*/ true);
    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

    assert!(composer.is_empty());
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert!(needs_redraw);
    assert!(composer.is_empty());
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Normal".magenta())
    );
    assert_eq!(composer.footer.mode, FooterMode::ComposerEmpty);
    assert!(!composer.footer.esc_backtrack_hint);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Replace".cyan())
    );
    snapshot_composer_state(
        "vim_replace_mode",
        /*enhanced_keys_supported*/ true,
        |composer| {
            composer.set_vim_enabled(/*enabled*/ true);
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
            composer.handle_paste("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1));
        },
    );
    composer.insert_str("ok");
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));
    assert_eq!(composer.current_text(), "");
    composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Normal".magenta())
    );
}

#[test]
fn slash_opens_command_popup_in_vim_insert_mode() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ true,
    );
    composer.set_vim_enabled(/*enabled*/ true);
    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert!(needs_redraw);
    assert_eq!(composer.draft.textarea.text(), "/");
    assert_eq!(composer.draft.textarea.cursor(), "/".len());
    assert!(matches!(composer.popups.active, ActivePopup::Command(_)));
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
}

#[test]
fn slash_command_dispatches_after_vim_insert_slash() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ true,
    );
    composer.set_vim_enabled(/*enabled*/ true);

    for ch in ['i', '/', 'd', 'i', 'f', 'f'] {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert_eq!(composer.draft.textarea.text(), "/diff");
    assert!(matches!(composer.popups.active, ActivePopup::Command(_)));

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(needs_redraw);
    assert!(composer.is_empty());
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
    assert!(matches!(result, InputResult::Command(SlashCommand::Diff)));
}

#[test]
fn inline_slash_command_dispatch_resets_vim_mode_to_insert() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ true,
    );
    composer.set_collaboration_modes_enabled(/*enabled*/ true);
    composer.set_vim_enabled(/*enabled*/ true);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.set_text_content("/plan investigate this".to_string(), Vec::new(), Vec::new());
    composer.popups.active = ActivePopup::None;
    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(needs_redraw);
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
    match result {
        InputResult::CommandWithArgs(cmd, args, text_elements) => {
            assert_eq!(cmd, SlashCommand::Plan);
            assert_eq!(args, "investigate this");
            assert!(text_elements.is_empty());
        }
        _ => panic!("expected CommandWithArgs"),
    }
    composer
        .prepare_inline_args_submission(/*record_history*/ false)
        .unwrap();
    for code in [KeyCode::Char('x'), KeyCode::Esc, KeyCode::Char('.')] {
        composer.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }
    assert_eq!(composer.current_text(), "xx");
}

#[test]
fn bang_enters_shell_mode_in_vim_normal_mode() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ true,
    );
    composer.set_vim_enabled(/*enabled*/ true);

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert!(needs_redraw);
    assert!(composer.draft.is_bash_mode);
    assert_eq!(composer.current_text(), "!");
    assert_eq!(composer.draft.textarea.text(), "");
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
}

#[test]
fn shell_command_can_be_typed_after_vim_normal_bang() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ true,
    );
    composer.set_vim_enabled(/*enabled*/ true);

    for ch in ['!', 'e', 'c', 'h', 'o'] {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }

    assert!(composer.draft.is_bash_mode);
    assert_eq!(composer.current_text(), "!echo");
    assert_eq!(composer.draft.textarea.text(), "echo");
    assert!(matches!(composer.popups.active, ActivePopup::None));
}

#[test]
fn base_footer_mode_tracks_empty_state_after_quit_hint_expires() {
    use crossterm::event::KeyCode;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['d']);
    composer.show_quit_shortcut_hint(key_hint::ctrl(KeyCode::Char('c')), /*has_focus*/ true);
    composer.footer.quit_shortcut_expires_at =
        Some(Instant::now() - std::time::Duration::from_secs(1));

    assert_eq!(composer.footer_mode(), FooterMode::ComposerHasDraft);

    composer.set_text_content(String::new(), Vec::new(), Vec::new());
    assert_eq!(composer.footer_mode(), FooterMode::ComposerEmpty);
}

#[test]
fn clear_for_ctrl_c_records_cleared_draft() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_text_content("draft text".to_string(), Vec::new(), Vec::new());
    assert_eq!(composer.clear_for_ctrl_c(), Some("draft text".to_string()));
    assert!(composer.is_empty());

    assert_eq!(
        composer.history.navigate_up(&composer.app_event_tx),
        Some(HistoryEntry::new("draft text".to_string()))
    );
}

#[test]
fn clear_for_ctrl_c_preserves_pending_paste_history_entry() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let large = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
    composer.handle_paste(large.clone());
    let char_count = large.chars().count();
    let placeholder = format!("[Pasted Content {char_count} chars]");
    assert_eq!(composer.draft.textarea.text(), placeholder);
    assert_eq!(
        composer.draft.pending_pastes,
        vec![(placeholder.clone(), large.clone())]
    );

    composer.clear_for_ctrl_c();
    assert!(composer.is_empty());

    let history_entry = composer
        .history
        .navigate_up(&composer.app_event_tx)
        .expect("expected history entry");
    let text_elements = vec![TextElement::new(
        (0..placeholder.len()).into(),
        Some(placeholder.clone()),
    )];
    assert_eq!(
        history_entry,
        HistoryEntry::with_pending(
            placeholder.clone(),
            text_elements,
            Vec::new(),
            vec![(placeholder.clone(), large.clone())]
        )
    );

    composer.apply_history_entry(history_entry);
    assert_eq!(composer.draft.textarea.text(), placeholder);
    assert_eq!(
        composer.draft.pending_pastes,
        vec![(placeholder.clone(), large)]
    );
    assert_eq!(
        composer.draft.textarea.element_payloads(),
        vec![placeholder]
    );

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            assert_eq!(text, "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5));
            assert!(text_elements.is_empty());
        }
        _ => panic!("expected Submitted"),
    }
}

#[test]
fn large_paste_numbering_reuses_after_ctrl_c_clear() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let base = format!("[Pasted Content {} chars]", paste.chars().count());

    composer.handle_paste(paste.clone());
    assert_eq!(composer.draft.textarea.text(), base);
    assert_eq!(composer.draft.pending_pastes.len(), 1);

    assert_eq!(composer.clear_for_ctrl_c(), Some(base.clone()));
    assert!(composer.draft.textarea.text().is_empty());
    assert!(composer.draft.pending_pastes.is_empty());

    composer.handle_paste(paste);
    assert_eq!(composer.draft.textarea.text(), base);
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(composer.draft.pending_pastes[0].0, base);
}

#[test]
fn vim_mode_resets_to_insert_after_submission() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(/*enabled*/ true);
    composer.set_vim_enabled(/*enabled*/ true);

    assert!(composer.draft.textarea.is_vim_enabled());
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Normal".magenta())
    );

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.set_text_content("h".to_string(), Vec::new(), Vec::new());
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(composer.draft.textarea.is_vim_enabled());
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
    assert!(composer.is_empty());
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, "h"),
        _ => panic!("expected Submitted"),
    }
}

#[test]
fn vim_mode_resets_to_insert_after_queued_submission() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(/*enabled*/ true);
    composer.set_task_running(/*running*/ true);
    composer.set_vim_enabled(/*enabled*/ true);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.set_text_content("queued".to_string(), Vec::new(), Vec::new());
    let (result, _) = composer.handle_submission(/*should_queue*/ true);

    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
    assert!(composer.is_empty());
    match result {
        InputResult::Queued { text, .. } => assert_eq!(text, "queued"),
        _ => panic!("expected Queued"),
    }
}

#[test]
fn vim_mode_stays_insert_after_suppressed_submission() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(/*enabled*/ true);
    composer.set_vim_enabled(/*enabled*/ true);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.set_text_content("/not-a-command".to_string(), Vec::new(), Vec::new());
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert_eq!(composer.draft.textarea.text(), "/not-a-command");
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
}

#[test]
fn esc_switches_vim_insert_to_normal() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_vim_enabled(/*enabled*/ true);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.set_text_content("hey".to_string(), Vec::new(), Vec::new());
    composer
        .draft
        .textarea
        .set_cursor(composer.draft.textarea.text().len());
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Insert".green())
    );
    assert_eq!(composer.draft.textarea.cursor(), "hey".len());

    composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Normal".magenta())
    );
    assert_eq!(composer.draft.textarea.cursor(), "he".len());
}

#[test]
fn vim_insert_uses_bar_cursor_style() {
    use crate::render::renderable::Renderable;
    use crossterm::cursor::SetCursorStyle;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use crossterm::queue;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ true,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 80, 10);
    let style_output = |style| {
        let mut output = Vec::new();
        queue!(output, style).expect("queue cursor style");
        output
    };
    let default = style_output(SetCursorStyle::DefaultUserShape);
    let steady_bar = style_output(SetCursorStyle::SteadyBar);

    assert_eq!(style_output(composer.cursor_style(area)), default,);

    composer.set_vim_enabled(/*enabled*/ true);
    assert_eq!(style_output(composer.cursor_style(area)), default,);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    composer.set_text_content("hey".to_string(), Vec::new(), Vec::new());
    assert_eq!(style_output(composer.cursor_style(area)), steady_bar);

    composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(style_output(composer.cursor_style(area)), default,);

    composer.handle_key_event(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE));
    assert_eq!(style_output(composer.cursor_style(area)), default,);
}

#[test]
fn clear_for_ctrl_c_preserves_image_draft_state() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let path = PathBuf::from("example.png");
    composer.attach_image(path.clone());
    let placeholder = local_image_label_text(/*label_number*/ 1);

    composer.clear_for_ctrl_c();
    assert!(composer.is_empty());

    let history_entry = composer
        .history
        .navigate_up(&composer.app_event_tx)
        .expect("expected history entry");
    let text_elements = vec![TextElement::new(
        (0..placeholder.len()).into(),
        Some(placeholder.clone()),
    )];
    assert_eq!(
        history_entry,
        HistoryEntry::with_pending(
            placeholder.clone(),
            text_elements,
            vec![path.clone()],
            Vec::new()
        )
    );

    composer.apply_history_entry(history_entry);
    assert_eq!(composer.draft.textarea.text(), placeholder);
    assert_eq!(composer.local_image_paths(), vec![path]);
    assert_eq!(
        composer.draft.textarea.element_payloads(),
        vec![placeholder]
    );
}

#[test]
fn clear_for_ctrl_c_preserves_remote_offset_image_labels() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let remote_image_url = "https://example.com/one.png".to_string();
    composer.set_remote_image_urls(vec![remote_image_url.clone()]);
    let text = "[Image #2] draft".to_string();
    let text_elements = vec![TextElement::new(
        (0.."[Image #2]".len()).into(),
        Some("[Image #2]".to_string()),
    )];
    let local_image_path = PathBuf::from("/tmp/local-draft.png");
    composer.set_text_content(text, text_elements, vec![local_image_path.clone()]);
    let expected_text = composer.current_text();
    let expected_elements = composer.text_elements();
    assert_eq!(expected_text, "[Image #2] draft");
    assert_eq!(
        expected_elements[0].placeholder(&expected_text),
        Some("[Image #2]")
    );

    assert_eq!(composer.clear_for_ctrl_c(), Some(expected_text.clone()));

    assert_eq!(
        composer.history.navigate_up(&composer.app_event_tx),
        Some(HistoryEntry::with_pending_and_remote(
            expected_text,
            expected_elements,
            vec![local_image_path],
            Vec::new(),
            vec![remote_image_url],
        ))
    );
}

#[test]
fn apply_history_entry_preserves_local_placeholders_after_remote_prefix() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let remote_image_url = "https://example.com/one.png".to_string();
    let local_image_path = PathBuf::from("/tmp/local-draft.png");
    composer.apply_history_entry(HistoryEntry::with_pending_and_remote(
        "[Image #2] draft".to_string(),
        vec![TextElement::new(
            (0.."[Image #2]".len()).into(),
            Some("[Image #2]".to_string()),
        )],
        vec![local_image_path.clone()],
        Vec::new(),
        vec![remote_image_url.clone()],
    ));

    let restored_text = composer.current_text();
    assert_eq!(restored_text, "[Image #2] draft");
    let restored_elements = composer.text_elements();
    assert_eq!(restored_elements.len(), 1);
    assert_eq!(
        restored_elements[0].placeholder(&restored_text),
        Some("[Image #2]")
    );
    assert_eq!(composer.local_image_paths(), vec![local_image_path]);
    assert_eq!(composer.remote_image_urls(), vec![remote_image_url]);
}

/// Behavior: `?` toggles the shortcut overlay only when the composer is otherwise empty. After
/// any typing has occurred, `?` should be inserted as a literal character.
#[test]
fn question_mark_only_toggles_on_first_char() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    assert_eq!(result, InputResult::None);
    assert!(needs_redraw, "toggling overlay should request redraw");
    assert_eq!(composer.footer.mode, FooterMode::ShortcutOverlay);

    // Toggle back to prompt mode so subsequent typing captures characters.
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    assert_eq!(composer.footer.mode, FooterMode::ComposerEmpty);

    type_chars_humanlike(&mut composer, &['h']);
    assert_eq!(composer.draft.textarea.text(), "h");
    assert_eq!(composer.footer_mode(), FooterMode::ComposerHasDraft);

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    assert_eq!(result, InputResult::None);
    assert!(needs_redraw, "typing should still mark the view dirty");
    let _ = flush_after_paste_burst(&mut composer);
    assert_eq!(composer.draft.textarea.text(), "h?");
    assert_eq!(composer.footer.mode, FooterMode::ComposerEmpty);
    assert_eq!(composer.footer_mode(), FooterMode::ComposerHasDraft);
}

#[test]
fn shift_question_mark_toggles_shortcut_overlay_when_empty() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(true);

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
    assert_eq!(result, InputResult::None);
    assert!(needs_redraw, "toggling overlay should request redraw");
    assert_eq!(composer.footer.mode, FooterMode::ShortcutOverlay);
}

/// Behavior: while a paste-like burst is being captured, `?` must not toggle the shortcut
/// overlay; it should be treated as part of the pasted content.
#[test]
fn question_mark_does_not_toggle_during_paste_burst() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Force an active paste burst so this test doesn't depend on tight timing.
    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed(String::new(), Instant::now());

    for ch in ['h', 'i', '?', 't', 'h', 'e', 'r', 'e'] {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert!(composer.is_in_paste_burst());
    assert_eq!(composer.draft.textarea.text(), "");

    let _ = flush_after_paste_burst(&mut composer);

    assert_eq!(composer.draft.textarea.text(), "hi?there");
    assert_ne!(composer.footer.mode, FooterMode::ShortcutOverlay);
}

#[test]
fn set_connector_mentions_refreshes_open_mention_popup() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_connectors_enabled(/*enabled*/ true);
    composer.set_text_content("$".to_string(), Vec::new(), Vec::new());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let connectors = vec![AppInfo {
        id: "connector_1".to_string(),
        name: "Notion".to_string(),
        description: Some("Workspace docs".to_string()),
        logo_url: None,
        logo_url_dark: None,
        icon_assets: None,
        icon_dark_assets: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: Some("https://example.test/notion".to_string()),
        is_accessible: true,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }];
    composer.set_connector_mentions(Some(ConnectorsSnapshot { connectors }));

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected mention popup to open after connectors update");
    };
    let mention = popup
        .selected_mention()
        .expect("expected connector mention to be selected");
    assert_eq!(mention.insert_text, "$notion".to_string());
    assert_eq!(mention.path, Some("app://connector_1".to_string()));
}

#[test]
fn set_connector_mentions_skips_disabled_connectors() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_connectors_enabled(/*enabled*/ true);
    composer.set_text_content("$".to_string(), Vec::new(), Vec::new());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let connectors = vec![AppInfo {
        id: "connector_1".to_string(),
        name: "Notion".to_string(),
        description: Some("Workspace docs".to_string()),
        logo_url: None,
        logo_url_dark: None,
        icon_assets: None,
        icon_dark_assets: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: Some("https://example.test/notion".to_string()),
        is_accessible: true,
        is_enabled: false,
        plugin_display_names: Vec::new(),
    }];
    composer.set_connector_mentions(Some(ConnectorsSnapshot { connectors }));

    assert!(
        matches!(composer.popups.active, ActivePopup::None),
        "disabled connectors should not appear in the mention popup"
    );
}

#[test]
fn set_plugin_mentions_refreshes_open_mention_popup() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("$".to_string(), Vec::new(), Vec::new());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
        config_name: "sample@test".to_string(),
        display_name: "Sample Plugin".to_string(),
        plugin_namespace: None,
        description: None,
        has_skills: true,
        mcp_server_names: vec!["sample".to_string()],
        app_connector_ids: Vec::new(),
    }]));

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected mention popup to open after plugin update");
    };
    let mention = popup
        .selected_mention()
        .expect("expected plugin mention to be selected");
    assert_eq!(mention.insert_text, "$sample".to_string());
    assert_eq!(mention.path, Some("plugin://sample@test".to_string()));
}

pub(super) fn test_skill_metadata(name: &str) -> SkillMetadata {
    SkillMetadata {
        name: name.to_string(),
        description: "Example skill used in tests.".to_string(),
        short_description: None,
        interface: None,
        dependencies: None,
        path: test_path_buf(&format!("/tmp/{name}/SKILL.md")).abs().into(),
        scope: crate::test_support::skill_scope_user(),
        enabled: true,
        plugin_id: None,
    }
}

pub(super) fn test_skill_binding(name: &str) -> MentionBinding {
    MentionBinding {
        sigil: '$',
        mention: name.to_string(),
        path: test_path_buf(&format!("/tmp/{name}/SKILL.md"))
            .abs()
            .display()
            .to_string(),
    }
}

fn test_plugin_binding(name: &str) -> MentionBinding {
    MentionBinding {
        sigil: '@',
        mention: name.to_string(),
        path: format!("plugin://{name}@test"),
    }
}

pub(super) fn test_plugin_summary(name: &str, description: &str) -> PluginCapabilitySummary {
    PluginCapabilitySummary {
        config_name: format!("{name}@test"),
        display_name: name.to_string(),
        plugin_namespace: None,
        description: Some(description.to_string()),
        has_skills: false,
        mcp_server_names: vec![name.to_string()],
        app_connector_ids: Vec::new(),
    }
}

fn configure_partially_bound_skill_mentions(composer: &mut ChatComposer) {
    composer.set_skill_mentions(Some(vec![test_skill_metadata("unbound-skill")]));

    composer.set_text_content_with_mention_bindings(
        "$unbound-skill  $bound-skill continue".to_string(),
        Vec::new(),
        Vec::new(),
        vec![test_skill_binding("bound-skill")],
    );
    composer.draft.textarea.set_cursor("$unbound-skill  ".len());
    composer.sync_popups();
}

fn configure_bound_skill_left_of_unbound_skill(composer: &mut ChatComposer) {
    composer.set_skill_mentions(Some(vec![test_skill_metadata("unbound-skill")]));
    composer.set_text_content_with_mention_bindings(
        "$bound-skill$unbound-skill".to_string(),
        Vec::new(),
        Vec::new(),
        vec![test_skill_binding("bound-skill")],
    );
    composer.draft.textarea.set_cursor("$bound-skill".len());
    composer.sync_popups();
}

fn configure_skill_target_between_bound_mentions(composer: &mut ChatComposer) {
    composer.set_skill_mentions(Some(vec![test_skill_metadata("other")]));
    composer.set_text_content_with_mention_bindings(
        "$bound1 $oth $bound2".to_string(),
        Vec::new(),
        Vec::new(),
        vec![test_skill_binding("bound1"), test_skill_binding("bound2")],
    );
    composer
        .draft
        .textarea
        .replace_range("$bound1".len().."$bound1 ".len(), "");
    composer
        .draft
        .textarea
        .replace_range("$bound1$oth".len().."$bound1$oth ".len(), "");
    composer.draft.textarea.set_cursor("$bound1$o".len());
    composer.sync_popups();
}

fn configure_skill_mention_with_trailing_space(composer: &mut ChatComposer) {
    composer.set_skill_mentions(Some(vec![test_skill_metadata("figma")]));
    let text = "$figma ";
    composer.set_text_content(text.to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor(text.len());
    composer.sync_popups();
}

fn configure_bound_plugin_left_of_unbound_plugin(composer: &mut ChatComposer) {
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_plugin_mentions(Some(vec![test_plugin_summary(
        "other",
        "Plugin used to test adjacent mention targeting.",
    )]));
    composer.set_text_content_with_mention_bindings(
        "@sample@other".to_string(),
        Vec::new(),
        Vec::new(),
        vec![test_plugin_binding("sample")],
    );
    composer.draft.textarea.set_cursor("@sample".len());
    composer.sync_popups();
}

fn configure_unbound_plugin_left_of_bound_plugin(composer: &mut ChatComposer) {
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_plugin_mentions(Some(vec![test_plugin_summary(
        "left",
        "Plugin used to test bound mention fallback.",
    )]));
    composer.set_text_content_with_mention_bindings(
        "@left  @bound".to_string(),
        Vec::new(),
        Vec::new(),
        vec![test_plugin_binding("bound")],
    );
    composer.draft.textarea.set_cursor("@left ".len());
    composer.sync_popups();
}

fn configure_plugin_target_between_bound_mentions(composer: &mut ChatComposer) {
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_plugin_mentions(Some(vec![test_plugin_summary(
        "other",
        "Plugin used to test middle mention targeting.",
    )]));
    composer.set_text_content_with_mention_bindings(
        "@bound1 @oth @bound2".to_string(),
        Vec::new(),
        Vec::new(),
        vec![test_plugin_binding("bound1"), test_plugin_binding("bound2")],
    );
    composer
        .draft
        .textarea
        .replace_range("@bound1".len().."@bound1 ".len(), "");
    composer
        .draft
        .textarea
        .replace_range("@bound1@oth".len().."@bound1@oth ".len(), "");
    composer.draft.textarea.set_cursor("@bound1@o".len());
    composer.sync_popups();
}

#[test]
fn skill_popup_targets_unbound_mention_left_of_bound_mention() {
    let (mut composer, _rx) = new_test_composer();
    configure_partially_bound_skill_mentions(&mut composer);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        composer.current_text(),
        "$unbound-skill  $bound-skill continue"
    );
    assert_eq!(
        composer.mention_bindings(),
        vec![
            test_skill_binding("unbound-skill"),
            test_skill_binding("bound-skill"),
        ]
    );

    composer.insert_str("foo");
    assert_eq!(
        composer.current_text(),
        "$unbound-skill foo $bound-skill continue"
    );
}

#[test]
fn skill_popup_targets_unbound_mention_left_of_bound_mention_snapshot() {
    snapshot_composer_state(
        "skill_popup_targets_unbound_mention_left_of_bound_mention",
        /*enhanced_keys_supported*/ false,
        configure_partially_bound_skill_mentions,
    );
}

#[test]
fn skill_popup_prefers_confirmed_selections_within_match_tier() {
    snapshot_composer_state(
        "skill_popup_prefers_confirmed_selections_within_match_tier",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![
                test_skill_metadata("statsig"),
                test_skill_metadata("status"),
                test_skill_metadata("hello-statsig"),
            ]));
            for name in ["hello-statsig", "hello-statsig", "status"] {
                let text = format!("${name}");
                composer.set_text_content(text.clone(), Vec::new(), Vec::new());
                composer.set_current_cursor(text.len());
                composer.sync_popups();
                let _ =
                    composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            composer.set_text_content("$stat".to_owned(), Vec::new(), Vec::new());
            composer.set_current_cursor("$stat".len());
            composer.sync_popups();
            let ActivePopup::Skill(popup) = &composer.popups.active else {
                panic!("expected skill popup");
            };
            assert_eq!(
                popup
                    .selected_mention()
                    .map(|item| item.insert_text.as_str()),
                Some("$status")
            );
        },
    );
}

#[test]
fn skill_popup_targets_unbound_mention_right_of_adjacent_bound_mention_snapshot() {
    snapshot_composer_state(
        "skill_popup_targets_unbound_mention_right_of_adjacent_bound_mention",
        /*enhanced_keys_supported*/ false,
        configure_bound_skill_left_of_unbound_skill,
    );
}

#[test]
fn skill_popup_falls_back_from_bound_skill_with_path_suffix_snapshot() {
    snapshot_composer_state(
        "skill_popup_falls_back_from_bound_skill_with_path_suffix",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("left")]));
            composer.set_text_content_with_mention_bindings(
                "$left  $bound/path".to_string(),
                Vec::new(),
                Vec::new(),
                vec![test_skill_binding("bound")],
            );
            composer.draft.textarea.set_cursor("$left  ".len());
            composer.sync_popups();
        },
    );
}

#[test]
fn skill_popup_closes_after_trailing_space_snapshot() {
    snapshot_composer_state(
        "skill_popup_closes_after_trailing_space",
        /*enhanced_keys_supported*/ false,
        configure_skill_mention_with_trailing_space,
    );
}

#[test]
fn unified_mention_popup_falls_back_from_bound_plugin_on_right_snapshot() {
    snapshot_composer_state(
        "unified_mention_popup_falls_back_from_bound_plugin_on_right",
        /*enhanced_keys_supported*/ false,
        configure_unbound_plugin_left_of_bound_plugin,
    );
}

#[test]
fn adjacent_plugin_completion_inserts_separator_snapshot() {
    snapshot_composer_state(
        "adjacent_plugin_completion_inserts_separator",
        /*enhanced_keys_supported*/ false,
        |composer| {
            configure_bound_plugin_left_of_unbound_plugin(composer);
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        },
    );
}

#[test]
fn skill_completion_replaces_only_middle_adjacent_target() {
    let (mut composer, _rx) = new_test_composer();
    configure_skill_target_between_bound_mentions(&mut composer);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(composer.current_text(), "$bound1 $other $bound2");
    assert_eq!(
        composer.mention_bindings(),
        vec![
            test_skill_binding("bound1"),
            test_skill_binding("other"),
            test_skill_binding("bound2"),
        ]
    );
}

#[test]
fn unified_completion_replaces_only_middle_adjacent_target() {
    let (mut composer, _rx) = new_test_composer();
    configure_plugin_target_between_bound_mentions(&mut composer);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(composer.current_text(), "@bound1 @other @bound2");
    assert_eq!(
        composer.mention_bindings(),
        vec![
            test_plugin_binding("bound1"),
            test_plugin_binding("other"),
            test_plugin_binding("bound2"),
        ]
    );
}

#[test]
fn skill_popup_falls_back_from_shell_variable_to_skill_on_right() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("rustdoc")]));
    composer.set_text_content("$HOME  $rustdoc".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$HOME ".len());
    composer.sync_popups();

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected skill popup to fall back to the right token");
    };
    assert_eq!(
        popup
            .selected_mention()
            .expect("expected rustdoc selection")
            .insert_text,
        "$rustdoc"
    );
}

#[test]
fn file_popup_ignores_shell_positional_parameter_snapshot() {
    snapshot_composer_state(
        "file_popup_ignores_shell_positional_parameter",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("available")]));
            composer.set_text_content("@src  $1_suffix".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("@src ".len());
            composer.sync_popups();

            assert!(matches!(composer.popups.active, ActivePopup::File(_)));
        },
    );
}

#[test]
fn file_popup_ignores_bare_shell_parameter_with_matching_skill_snapshot() {
    snapshot_composer_state(
        "file_popup_ignores_bare_shell_parameter_with_matching_skill",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("_tool")]));
            composer.set_text_content("@src  $_".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("@src ".len());
            composer.sync_popups();

            assert!(matches!(composer.popups.active, ActivePopup::File(_)));
        },
    );
}

#[test]
fn file_popup_ignores_definite_shell_parameters_with_matching_skills() {
    for (query, skill_name) in [("12", "12factor"), ("-", "-tool")] {
        let (mut composer, _rx) = new_test_composer();
        composer.set_skill_mentions(Some(vec![test_skill_metadata(skill_name)]));
        composer.set_text_content(format!("@src  ${query}"), Vec::new(), Vec::new());
        composer.draft.textarea.set_cursor("@src ".len());
        composer.sync_popups();

        assert!(matches!(composer.popups.active, ActivePopup::File(_)));
    }
}

#[test]
fn skill_popup_accepts_loaded_hyphen_leading_skill_name() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("-tool")]));
    composer.set_text_content("$-t".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$-t".len());
    composer.sync_popups();

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected skill popup for -tool");
    };
    assert_eq!(
        popup
            .selected_mention()
            .expect("expected -tool selection")
            .insert_text,
        "$-tool"
    );
}

#[test]
fn file_popup_ignores_unbindable_qualified_skill_snapshot() {
    snapshot_composer_state(
        "file_popup_ignores_unbindable_qualified_skill",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("1plugin:deploy")]));
            composer.set_text_content("@src  $1plugin:d".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("@src ".len());
            composer.sync_popups();

            assert!(matches!(composer.popups.active, ActivePopup::File(_)));
        },
    );
}

#[test]
fn skill_popup_preserves_normal_target_after_ambiguous_probe_snapshot() {
    snapshot_composer_state(
        "skill_popup_preserves_normal_target_after_ambiguous_probe",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("plugin:deploy")]));
            composer.set_text_content("$1x  $plugin:d".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("$1x ".len());
            composer.sync_popups();

            let ActivePopup::Skill(popup) = &composer.popups.active else {
                panic!("expected the right qualified skill popup");
            };
            assert_eq!(
                popup
                    .selected_mention()
                    .expect("expected qualified skill selection")
                    .insert_text,
                "$plugin:deploy"
            );
        },
    );
}

#[test]
fn mention_target_prebuilds_catalog_only_for_ambiguous_query() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("1password")]));

    composer.set_text_content("$normal".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$normal".len());
    let normal_target = composer
        .current_mention_target()
        .expect("expected normal mention target");
    assert!(normal_target.prebuilt_mentions.is_none());

    composer.set_text_content("$1p".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$1p".len());
    let ambiguous_target = composer
        .current_mention_target()
        .expect("expected ambiguous mention target");
    assert_eq!(
        ambiguous_target.prebuilt_mentions.map(|mentions| {
            mentions
                .into_iter()
                .map(|mention| mention.display_name)
                .collect::<Vec<_>>()
        }),
        Some(vec!["1password".to_string()])
    );
}

#[test]
fn skill_popup_accepts_digit_leading_skill_snapshot() {
    snapshot_composer_state(
        "skill_popup_accepts_digit_leading_skill",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("1password")]));
            composer.set_text_content("$1p".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("$1p".len());
            composer.sync_popups();

            assert!(matches!(composer.popups.active, ActivePopup::Skill(_)));
        },
    );
}

#[test]
fn skill_popup_does_not_fuzzy_match_shell_variable_snapshot() {
    snapshot_composer_state(
        "skill_popup_does_not_fuzzy_match_shell_variable",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("home")]));
            composer.set_text_content("$HOME".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("$HOME".len());
            composer.sync_popups();

            assert!(matches!(composer.popups.active, ActivePopup::None));
        },
    );
}

#[test]
fn skill_popup_preserves_loaded_shell_like_skill_at_separator() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![
        test_skill_metadata("1password"),
        test_skill_metadata("rustdoc"),
    ]));
    composer.set_text_content("$1p  $rustdoc".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$1p ".len());
    composer.sync_popups();

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected the left shell-like skill popup");
    };
    assert_eq!(
        popup
            .selected_mention()
            .expect("expected 1password selection")
            .insert_text,
        "$1password"
    );
}

#[test]
fn skill_popup_accepts_lowercase_skill_named_like_env_var() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("home")]));
    composer.set_text_content("$home".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$home".len());
    composer.sync_popups();

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected skill popup for lowercase home skill");
    };
    assert_eq!(
        popup
            .selected_mention()
            .expect("expected home skill selection")
            .insert_text,
        "$home"
    );
}

#[test]
fn legacy_file_popup_falls_back_when_right_skill_is_unavailable() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_text_content("@src  $missing".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@src ".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::File(_)));
}

#[test]
fn unified_popup_falls_back_when_right_skill_is_unavailable() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_text_content("@src  $missing".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@src ".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::MentionV2(_)));
}

#[test]
fn skill_popup_falls_back_when_right_at_mention_is_bound() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("old")]));
    composer.set_text_content_with_mention_bindings(
        "$old  @bound".to_string(),
        Vec::new(),
        Vec::new(),
        vec![MentionBinding {
            sigil: '@',
            mention: "bound".to_string(),
            path: "plugin://bound@test".to_string(),
        }],
    );
    composer.draft.textarea.set_cursor("$old ".len());
    composer.sync_popups();

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected the left skill popup");
    };
    assert_eq!(
        popup
            .selected_mention()
            .expect("expected old skill selection")
            .insert_text,
        "$old"
    );
}

#[test]
fn legacy_popup_prefers_right_at_token_over_left_skill() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("old")]));
    composer.set_text_content("$old  @new".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$old  ".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::File(_)));
}

#[test]
fn unified_popup_prefers_right_skill_over_left_at_token() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_skill_mentions(Some(vec![test_skill_metadata("new")]));
    composer.set_text_content("@old  $new".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@old  ".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::Skill(_)));
}

#[test]
fn skill_popup_closes_at_plain_text_after_whitespace() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("old")]));
    composer.set_text_content("$old word".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("$old ".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::None));
}

#[test]
fn skill_popup_closes_between_spaces_before_plain_text_snapshot() {
    snapshot_composer_state(
        "skill_popup_closes_between_spaces_before_plain_text",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_skill_mentions(Some(vec![test_skill_metadata("old")]));
            composer.set_text_content("$old  word".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor("$old ".len());
            composer.sync_popups();

            assert!(matches!(composer.popups.active, ActivePopup::None));
        },
    );
}

fn assert_typed_skill_prefix_starts_new_mention(inserted: &str) {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("rustdoc")]));
    composer.set_text_content("$simplify-code".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor(/*pos*/ 0);

    composer.insert_str(inserted);
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(composer.current_text(), "$rustdoc $simplify-code");
    assert_eq!(
        composer.mention_bindings(),
        vec![test_skill_binding("rustdoc")]
    );
}

#[test]
fn typing_empty_skill_prefix_before_existing_skill_starts_new_mention() {
    assert_typed_skill_prefix_starts_new_mention("$");
}

#[test]
fn typing_partial_skill_prefix_before_existing_skill_starts_new_mention() {
    assert_typed_skill_prefix_starts_new_mention("$r");
}

#[test]
fn set_skill_mentions_refreshes_open_mention_popup() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("$".to_string(), Vec::new(), Vec::new());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let skill_path = test_path_buf("/tmp/skill/SKILL.md").abs();
    composer.set_skill_mentions(Some(vec![SkillMetadata {
        name: "codex".to_string(),
        description: "Primary personal Codex repo skill.".to_string(),
        short_description: None,
        interface: None,
        dependencies: None,
        path: skill_path.clone().into(),
        scope: crate::test_support::skill_scope_user(),
        enabled: true,
        plugin_id: None,
    }]));

    let ActivePopup::Skill(popup) = &composer.popups.active else {
        panic!("expected mention popup to open after skills update");
    };
    let mention = popup
        .selected_mention()
        .expect("expected skill mention to be selected");
    assert_eq!(mention.insert_text, "$codex".to_string());
    assert_eq!(mention.path, Some(skill_path.display().to_string()));
}

#[test]
fn mention_items_keep_direct_skills_and_apps_with_unified_mentions() {
    let skill_path = test_path_buf("/tmp/repo/google-calendar/SKILL.md").abs();
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_connectors_enabled(/*enabled*/ true);
    composer.set_text_content("$goog".to_string(), Vec::new(), Vec::new());
    composer.set_skill_mentions(Some(vec![SkillMetadata {
        name: "google-calendar:availability".to_string(),
        description: "Find availability and plan event changes".to_string(),
        short_description: None,
        interface: Some(SkillInterface {
            display_name: Some("Google Calendar".to_string()),
            short_description: None,
            icon_small: None,
            icon_large: None,
            icon_small_url: None,
            icon_large_url: None,
            brand_color: None,
            default_prompt: None,
        }),
        dependencies: None,
        path: skill_path.clone().into(),
        scope: crate::test_support::skill_scope_repo(),
        enabled: true,
        plugin_id: Some("google-calendar@debug".to_string()),
    }]));
    composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
        config_name: "google-calendar@debug".to_string(),
        display_name: "Google Calendar".to_string(),
        plugin_namespace: None,
        description: Some(
            "Connect Google Calendar for scheduling, availability, and event management."
                .to_string(),
        ),
        has_skills: true,
        mcp_server_names: vec!["google-calendar".to_string()],
        app_connector_ids: vec![AppConnectorId("google_calendar".to_string())],
    }]));
    composer.set_connector_mentions(Some(ConnectorsSnapshot {
        connectors: vec![AppInfo {
            id: "google_calendar".to_string(),
            name: "Google Calendar".to_string(),
            description: Some("Look up events and availability".to_string()),
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://example.test/google-calendar".to_string()),
            is_accessible: true,
            is_enabled: true,
            plugin_display_names: vec!["Google Calendar".to_string()],
        }],
    }));

    let mentions = composer.mention_items();
    assert_eq!(mentions.len(), 3);
    assert_eq!(mentions[0].category_tag, Some("[Skill]".to_string()));
    assert_eq!(mentions[0].path, Some(skill_path.display().to_string()));
    assert_eq!(mentions[0].display_name, "Google Calendar".to_string());
    assert_eq!(mentions[1].category_tag, Some("[Plugin]".to_string()));
    assert_eq!(
        mentions[1].path,
        Some("plugin://google-calendar@debug".to_string())
    );
    assert_eq!(mentions[2].category_tag, Some("[App]".to_string()));
    assert_eq!(mentions[2].path, Some("app://google_calendar".to_string()));

    composer.set_mentions_v2_enabled(/*enabled*/ true);
    assert_eq!(
        composer
            .mention_items()
            .into_iter()
            .map(|mention| (mention.insert_text, mention.path))
            .collect::<Vec<_>>(),
        vec![
            (
                "$google-calendar:availability".to_string(),
                Some(skill_path.display().to_string()),
            ),
            (
                "$google-calendar".to_string(),
                Some("app://google_calendar".to_string()),
            ),
        ],
    );
}

#[test]
fn plugin_mention_popup_snapshot() {
    snapshot_composer_state(
        "plugin_mention_popup",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_text_content("$sa".to_string(), Vec::new(), Vec::new());
            composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
                config_name: "sample@test".to_string(),
                display_name: "Sample Plugin".to_string(),
                plugin_namespace: None,
                description: Some(
                    "Plugin that includes the Figma MCP server and Skills for common workflows"
                        .to_string(),
                ),
                has_skills: true,
                mcp_server_names: vec!["sample".to_string()],
                app_connector_ids: vec![AppConnectorId("calendar".to_string())],
            }]));
        },
    );
}

#[test]
fn task_mention_popup_and_selected_composer_snapshots() {
    for selected in [false, true] {
        snapshot_composer_state(
            if selected {
                "task_mention_selected_composer"
            } else {
                "task_mention_popup"
            },
            /*enhanced_keys_supported*/ false,
            |composer| {
                composer.set_mentions_v2_enabled(/*enabled*/ true);
                composer.set_task_mentions_enabled(/*enabled*/ true);
                composer.set_text_content("@migr".to_string(), Vec::new(), Vec::new());
                composer.on_task_search_result(
                    "migr",
                    vec![
                        crate::task_mentions::TaskMention {
                            thread_id: "task-123".to_string(),
                            title: "Review database migration".to_string(),
                            cwd: "/workspace/project".to_string(),
                            snippet: "database migration".to_string(),
                        },
                        crate::task_mentions::TaskMention {
                            thread_id: "task-456".to_string(),
                            title: "migration".to_string(),
                            cwd: "/another/project".to_string(),
                            snippet: "migration".to_string(),
                        },
                    ],
                );
                if selected {
                    composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    assert!(matches!(composer.mention_bindings().as_slice(),
                            [MentionBinding { mention, path, .. }]
                                if mention == "Review database migration"
                                    && path == "thread://task-123"));
                    assert!(composer.task_mentions.as_ref().is_some_and(Vec::is_empty));
                    let area = Rect::new(0, 0, 40, 5);
                    let mut buf = Buffer::empty(area);
                    composer.render(area, &mut buf);
                    assert_eq!(buf[(2, 1)].style().fg, Some(Color::Cyan));
                }
            },
        );
    }
}

#[test]
fn task_mention_titles_are_normalized_and_bounded_before_insertion() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_task_mentions_enabled(/*enabled*/ true);
    composer.set_text_content("@task".to_string(), Vec::new(), Vec::new());
    let task = crate::task_mentions::TaskMention {
        thread_id: "task-123".to_string(),
        title: format!("task\n\t{}é{}", "a".repeat(154), "é".repeat(200)),
        cwd: "/workspace/project".to_string(),
        snippet: String::new(),
    };
    composer.on_task_search_result("task", vec![task.clone()]);
    composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        composer.mention_bindings(),
        vec![MentionBinding {
            sigil: '@',
            mention: format!("task {}é", "a".repeat(154)),
            path: "thread://task-123".to_string(),
        }]
    );
    composer.draft.textarea.set_text_clearing_elements("@task");
    composer.sync_popups();
    composer.on_task_search_result("task", vec![task]);
    assert!(matches!(&composer.popups.active,
            ActivePopup::MentionV2(popup) if popup.selected().is_some()));

    let history = HistoryEntry::new(
        "[@sample](plugin://sample@test) @sample [@sample](thread://task-123)x".to_string(),
    );
    composer.set_text_content_with_mention_bindings(
        history.text,
        history.text_elements,
        Vec::new(),
        history.mention_bindings,
    );
    assert_eq!(
        composer.mention_bindings(),
        ["plugin://sample@test", "thread://task-123"]
            .into_iter()
            .map(|path| MentionBinding {
                sigil: '@',
                mention: "sample".to_string(),
                path: path.to_string(),
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        composer.draft.textarea.text_element_snapshots()[1]
            .range
            .start,
        "@sample @sample ".len()
    );
}

#[test]
fn bare_unified_mention_resets_an_inherited_file_search() {
    let (mut composer, mut rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);

    composer.insert_str("@");
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::StartFileSearch(query)) if query.is_empty()
    ));

    composer.insert_str("s");
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::StartFileSearch(query)) if query == "s"
    ));
}

#[test]
fn reopening_identical_unified_mention_restarts_file_search() {
    let (mut composer, mut rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_text_content("@foo @foo".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@f".len());
    composer.sync_popups();
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::StartFileSearch(query)) if query.is_empty()
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::StartFileSearch(query)) if query == "foo"
    ));

    composer.on_file_search_result("foo".to_string(), Vec::new());
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(composer.popups.active, ActivePopup::None));

    composer.draft.textarea.set_cursor("@foo @foo".len());
    composer.sync_popups();
    assert!(matches!(composer.popups.active, ActivePopup::MentionV2(_)));
    let queries = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::StartFileSearch(query) => Some(query),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(queries, vec![String::new(), "foo".to_string()]);
}

#[test]
fn restored_unified_mention_restarts_file_search() {
    let (mut composer, mut rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);

    composer.set_text_content("@foo".to_string(), Vec::new(), Vec::new());

    assert!(matches!(composer.popups.active, ActivePopup::MentionV2(_)));
    let queries = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::StartFileSearch(query) => Some(query),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(queries, vec![String::new(), "foo".to_string()]);
}

#[test]
fn mention_popup_type_prefixes_snapshot() {
    snapshot_composer_state_with_width(
        "mention_popup_type_prefixes",
        /*width*/ 72,
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_mentions_v2_enabled(/*enabled*/ true);
            composer.set_connectors_enabled(/*enabled*/ true);
            composer.set_text_content("$goog".to_string(), Vec::new(), Vec::new());
            composer.set_skill_mentions(Some(vec![SkillMetadata {
                name: "google-calendar-skill".to_string(),
                description: "Find availability and plan event changes".to_string(),
                short_description: None,
                interface: Some(SkillInterface {
                    display_name: Some("Google Calendar".to_string()),
                    short_description: None,
                    icon_small: None,
                    icon_large: None,
                    icon_small_url: None,
                    icon_large_url: None,
                    brand_color: None,
                    default_prompt: None,
                }),
                dependencies: None,
                path: test_path_buf("/tmp/repo/google-calendar/SKILL.md")
                    .abs()
                    .into(),
                scope: crate::test_support::skill_scope_repo(),
                enabled: true,
                plugin_id: None,
            }]));
            composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
                config_name: "google-calendar@debug".to_string(),
                display_name: "Google Calendar".to_string(),
                plugin_namespace: None,
                description: Some(
                    "Connect Google Calendar for scheduling, availability, and event management."
                        .to_string(),
                ),
                has_skills: false,
                mcp_server_names: vec!["google-calendar".to_string()],
                app_connector_ids: Vec::new(),
            }]));
            composer.set_connector_mentions(Some(ConnectorsSnapshot {
                connectors: vec![AppInfo {
                    id: "google_calendar".to_string(),
                    name: "Google Calendar".to_string(),
                    description: Some("Look up events and availability".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/google-calendar".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                }],
            }));
        },
    );
}

#[test]
fn set_connector_mentions_excludes_disabled_apps_from_mention_popup() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_connectors_enabled(/*enabled*/ true);
    composer.set_text_content("$".to_string(), Vec::new(), Vec::new());

    let connectors = vec![AppInfo {
        id: "connector_1".to_string(),
        name: "Notion".to_string(),
        description: Some("Workspace docs".to_string()),
        logo_url: None,
        logo_url_dark: None,
        icon_assets: None,
        icon_dark_assets: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: Some("https://example.test/notion".to_string()),
        is_accessible: true,
        is_enabled: false,
        plugin_display_names: Vec::new(),
    }];
    composer.set_connector_mentions(Some(ConnectorsSnapshot { connectors }));

    assert!(matches!(composer.popups.active, ActivePopup::None));
}

#[test]
fn shortcut_overlay_persists_while_task_running() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    assert_eq!(composer.footer.mode, FooterMode::ShortcutOverlay);

    composer.set_task_running(/*running*/ true);

    assert_eq!(composer.footer.mode, FooterMode::ShortcutOverlay);
    assert_eq!(composer.footer_mode(), FooterMode::ShortcutOverlay);
}

#[test]
fn test_current_at_token_basic_cases() {
    let test_cases = vec![
        // Valid @ tokens
        ("@hello", 3, Some("hello".to_string()), "Basic ASCII token"),
        (
            "@file.txt",
            4,
            Some("file.txt".to_string()),
            "ASCII with extension",
        ),
        (
            "hello @world test",
            8,
            Some("world".to_string()),
            "ASCII token in middle",
        ),
        (
            "@test123",
            5,
            Some("test123".to_string()),
            "ASCII with numbers",
        ),
        // Unicode examples
        ("@İstanbul", 3, Some("İstanbul".to_string()), "Turkish text"),
        (
            "@testЙЦУ.rs",
            8,
            Some("testЙЦУ.rs".to_string()),
            "Mixed ASCII and Cyrillic",
        ),
        ("@诶", 2, Some("诶".to_string()), "Chinese character"),
        ("@👍", 2, Some("👍".to_string()), "Emoji token"),
        // Invalid cases (should return None)
        ("hello", 2, None, "No @ symbol"),
        (
            "@",
            1,
            Some("".to_string()),
            "Only @ symbol triggers empty query",
        ),
        ("@ hello", 2, None, "@ followed by space"),
        ("test @ world", 6, None, "@ with spaces around"),
    ];

    for (input, cursor_pos, expected, description) in test_cases {
        let mut textarea = TextArea::new();
        textarea.insert_str(input);
        textarea.set_cursor(cursor_pos);

        let result = ChatComposer::current_at_token(&textarea);
        assert_eq!(
            result, expected,
            "Failed for case: {description} - input: '{input}', cursor: {cursor_pos}"
        );
    }
}

#[test]
fn test_current_at_token_cursor_positions() {
    let test_cases = vec![
        // Different cursor positions within a token
        ("@test", 0, Some("test".to_string()), "Cursor at @"),
        ("@test", 1, Some("test".to_string()), "Cursor after @"),
        ("@test", 5, Some("test".to_string()), "Cursor at end"),
        // Multiple tokens - cursor determines which token
        ("@file1 @file2", 0, Some("file1".to_string()), "First token"),
        (
            "@file1 @file2",
            8,
            Some("file2".to_string()),
            "Second token",
        ),
        // Edge cases
        ("@", 0, Some("".to_string()), "Only @ symbol"),
        ("@a", 2, Some("a".to_string()), "Single character after @"),
        ("", 0, None, "Empty input"),
    ];

    for (input, cursor_pos, expected, description) in test_cases {
        let mut textarea = TextArea::new();
        textarea.insert_str(input);
        textarea.set_cursor(cursor_pos);

        let result = ChatComposer::current_at_token(&textarea);
        assert_eq!(
            result, expected,
            "Failed for cursor position case: {description} - input: '{input}', cursor: {cursor_pos}",
        );
    }
}

#[test]
fn test_current_at_token_whitespace_boundaries() {
    let test_cases = vec![
        // Space boundaries
        (
            "aaa@aaa",
            4,
            None,
            "Connected @ token - no completion by design",
        ),
        (
            "aaa @aaa",
            5,
            Some("aaa".to_string()),
            "@ token after space",
        ),
        (
            "test @file.txt",
            7,
            Some("file.txt".to_string()),
            "@ token after space",
        ),
        // Full-width space boundaries
        (
            "test　@İstanbul",
            8,
            Some("İstanbul".to_string()),
            "@ token after full-width space",
        ),
        (
            "@ЙЦУ　@诶",
            10,
            Some("诶".to_string()),
            "Full-width space between Unicode tokens",
        ),
        // Tab and newline boundaries
        (
            "test\t@file",
            6,
            Some("file".to_string()),
            "@ token after tab",
        ),
    ];

    for (input, cursor_pos, expected, description) in test_cases {
        let mut textarea = TextArea::new();
        textarea.insert_str(input);
        textarea.set_cursor(cursor_pos);

        let result = ChatComposer::current_at_token(&textarea);
        assert_eq!(
            result, expected,
            "Failed for whitespace boundary case: {description} - input: '{input}', cursor: {cursor_pos}",
        );
    }
}

#[test]
fn test_current_at_token_tracks_tokens_with_second_at() {
    let input = "npx -y @kaeawc/auto-mobile@latest";
    let token_start = input.find("@kaeawc").expect("scoped npm package present");
    let version_at = input
        .rfind("@latest")
        .expect("version suffix present in scoped npm package");
    let test_cases = vec![
        (token_start, "Cursor at leading @"),
        (token_start + 8, "Cursor inside scoped package name"),
        (version_at, "Cursor at version @"),
        (input.len(), "Cursor at end of token"),
    ];

    for (cursor_pos, description) in test_cases {
        let mut textarea = TextArea::new();
        textarea.insert_str(input);
        textarea.set_cursor(cursor_pos);

        let result = ChatComposer::current_at_token(&textarea);
        assert_eq!(
            result,
            Some("kaeawc/auto-mobile@latest".to_string()),
            "Failed for case: {description} - input: '{input}', cursor: {cursor_pos}"
        );
    }
}

#[test]
fn test_current_at_token_allows_file_queries_with_second_at() {
    let input = "@icons/icon@2x.png";
    let version_at = input
        .rfind("@2x")
        .expect("second @ in file token should be present");
    let test_cases = vec![
        (0, "Cursor at leading @"),
        (8, "Cursor before second @"),
        (version_at, "Cursor at second @"),
        (input.len(), "Cursor at end of token"),
    ];

    for (cursor_pos, description) in test_cases {
        let mut textarea = TextArea::new();
        textarea.insert_str(input);
        textarea.set_cursor(cursor_pos);

        let result = ChatComposer::current_at_token(&textarea);
        assert!(
            result.is_some(),
            "Failed for case: {description} - input: '{input}', cursor: {cursor_pos}"
        );
    }
}

#[test]
fn test_current_at_token_ignores_mid_word_at() {
    let input = "foo@bar";
    let at_pos = input.find('@').expect("@ present");
    let test_cases = vec![
        (at_pos, "Cursor at mid-word @"),
        (input.len(), "Cursor at end of word containing @"),
    ];

    for (cursor_pos, description) in test_cases {
        let mut textarea = TextArea::new();
        textarea.insert_str(input);
        textarea.set_cursor(cursor_pos);

        let result = ChatComposer::current_at_token(&textarea);
        assert_eq!(
            result, None,
            "Failed for case: {description} - input: '{input}', cursor: {cursor_pos}"
        );
    }
}

#[test]
fn set_text_content_rebinds_at_sigiled_mentions() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let mention_bindings = vec![MentionBinding {
        sigil: '@',
        mention: "figma".to_string(),
        path: "/tmp/user/figma/SKILL.md".to_string(),
    }];
    composer.set_text_content_with_mention_bindings(
        "@figma please".to_string(),
        Vec::new(),
        Vec::new(),
        mention_bindings.clone(),
    );

    assert_eq!(composer.mention_bindings(), mention_bindings);
}

#[test]
fn set_text_content_rebinds_matching_sigil_only() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let mention_bindings = vec![MentionBinding {
        sigil: '$',
        mention: "figma".to_string(),
        path: "app://figma".to_string(),
    }];
    composer.set_text_content_with_mention_bindings(
        "@figma then $figma".to_string(),
        Vec::new(),
        Vec::new(),
        mention_bindings.clone(),
    );

    let bound_tokens = composer
        .current_mention_elements()
        .into_iter()
        .map(|(_, sigil, mention)| (sigil, mention))
        .collect::<Vec<_>>();
    assert_eq!(bound_tokens, vec![('$', "figma".to_string())]);
    assert_eq!(composer.mention_bindings(), mention_bindings);
}

#[test]
fn set_text_content_rebinds_both_sigil_forms() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let mention_bindings = vec![
        MentionBinding {
            sigil: '@',
            mention: "figma".to_string(),
            path: "plugin://figma@test".to_string(),
        },
        MentionBinding {
            sigil: '$',
            mention: "figma".to_string(),
            path: "app://figma".to_string(),
        },
    ];
    composer.set_text_content_with_mention_bindings(
        "@figma then $figma".to_string(),
        Vec::new(),
        Vec::new(),
        mention_bindings.clone(),
    );

    let bound_tokens = composer
        .current_mention_elements()
        .into_iter()
        .map(|(_, sigil, mention)| (sigil, mention))
        .collect::<Vec<_>>();
    assert_eq!(
        bound_tokens,
        vec![('@', "figma".to_string()), ('$', "figma".to_string())]
    );
    assert_eq!(composer.mention_bindings(), mention_bindings);
}

#[test]
fn set_text_content_rebinds_at_mentions_after_email_substrings() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let text = "foo@sample.com then @sample".to_string();
    let mention_start = text.rfind("@sample").expect("expected bound mention token");
    let mention_range = mention_start..mention_start + "@sample".len();
    let mention_bindings = vec![MentionBinding {
        sigil: '@',
        mention: "sample".to_string(),
        path: "plugin://sample@test".to_string(),
    }];
    composer.set_text_content_with_mention_bindings(
        text,
        Vec::new(),
        Vec::new(),
        mention_bindings.clone(),
    );

    // Fix coverage: only the plaintext `@sample` should be atomic; the email substring stays editable.
    assert_eq!(
        composer
            .draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .map(|snapshot| snapshot.range)
            .collect::<Vec<_>>(),
        vec![mention_range]
    );
    assert_eq!(composer.mention_bindings(), mention_bindings);
}

#[test]
fn set_text_content_rebinds_at_mentions_after_punctuation() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let text = "Please ask (@sample)".to_string();
    let mention_start = text.find("@sample").expect("expected bound mention token");
    let mention_range = mention_start..mention_start + "@sample".len();
    let mention_bindings = vec![MentionBinding {
        sigil: '@',
        mention: "sample".to_string(),
        path: "plugin://sample@test".to_string(),
    }];
    composer.set_text_content_with_mention_bindings(
        text,
        Vec::new(),
        Vec::new(),
        mention_bindings.clone(),
    );

    assert_eq!(
        composer
            .draft
            .textarea
            .text_element_snapshots()
            .into_iter()
            .map(|snapshot| snapshot.range)
            .collect::<Vec<_>>(),
        vec![mention_range]
    );
    assert_eq!(composer.mention_bindings(), mention_bindings);
}

#[test]
fn bound_at_mentions_do_not_block_arrow_navigation() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_text_content_with_mention_bindings(
        "go @figma now".to_string(),
        Vec::new(),
        Vec::new(),
        vec![MentionBinding {
            sigil: '@',
            mention: "figma".to_string(),
            path: "plugin://figma@debug".to_string(),
        }],
    );
    composer.draft.textarea.set_cursor("go".len());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.cursor(), "go".len() + 1);
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.cursor(), "go @figma".len());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.cursor(), "go ".len());
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.cursor(), "go".len());
    assert!(matches!(composer.popups.active, ActivePopup::None));
}

#[test]
fn restored_bound_at_mentions_do_not_open_mention_popup() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    for (text, move_cursor_to_end) in [
        ("@sample".to_string(), false),
        ("Please ask @sample.".to_string(), true),
    ] {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            sender,
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        );
        composer.set_plugin_mentions(Some(vec![PluginCapabilitySummary {
            config_name: "sample@test".to_string(),
            display_name: "sample".to_string(),
            plugin_namespace: None,
            description: None,
            has_skills: true,
            mcp_server_names: vec!["sample".to_string()],
            app_connector_ids: Vec::new(),
        }]));

        composer.set_text_content_with_mention_bindings(
            text.clone(),
            Vec::new(),
            Vec::new(),
            vec![MentionBinding {
                sigil: '@',
                mention: "sample".to_string(),
                path: "plugin://sample@test".to_string(),
            }],
        );
        if move_cursor_to_end {
            composer.move_cursor_to_end();
        }

        assert!(matches!(composer.popups.active, ActivePopup::None));

        let (result, consumed) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(consumed);
        match result {
            InputResult::Submitted {
                text: submitted, ..
            } => assert_eq!(submitted, text),
            _ => panic!("expected restored bound mention to submit"),
        }
    }
}

#[test]
fn enter_submits_when_file_popup_has_no_selection() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let input = "npx -y @kaeawc/auto-mobile@latest";
    composer.draft.textarea.insert_str(input);
    composer.draft.textarea.set_cursor(input.len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::File(_)));

    let (result, consumed) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(consumed);
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, input),
        _ => panic!("expected Submitted"),
    }
}

#[test]
fn enter_submits_when_unified_mention_popup_has_no_selection() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_mentions_v2_enabled(/*enabled*/ true);

    let input = "npx -y @kaeawc/auto-mobile@latest";
    composer.draft.textarea.insert_str(input);
    composer.draft.textarea.set_cursor(input.len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::MentionV2(_)));

    let (result, consumed) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(consumed);
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, input),
        _ => panic!("expected Submitted"),
    }
}

/// Behavior: if the ASCII path has a pending first char (flicker suppression) and a non-ASCII
/// char arrives next, the pending ASCII char should still be preserved and the overall input
/// should submit normally (i.e. we should not misclassify this as a paste burst).
#[test]
fn ascii_prefix_survives_non_ascii_followup() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert!(composer.is_in_paste_burst());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, "1あ"),
        _ => panic!("expected Submitted"),
    }
}

/// Behavior: a single non-ASCII char should be inserted immediately (IME-friendly) and should
/// not create any paste-burst state.
#[test]
fn non_ascii_char_inserts_immediately_without_burst_state() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('あ'), KeyModifiers::NONE));

    assert_eq!(composer.draft.textarea.text(), "あ");
    assert!(!composer.is_in_paste_burst());
}

/// Behavior: while we're capturing a paste-like burst, Enter should be treated as a newline
/// within the burst (not as "submit"), and the whole payload should flush as one paste.
#[test]
fn non_ascii_burst_buffers_enter_and_flushes_multiline() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed(String::new(), Instant::now());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

    assert!(composer.draft.textarea.text().is_empty());
    let _ = flush_after_paste_burst(&mut composer);
    assert_eq!(composer.draft.textarea.text(), "你好\nhi");
}

/// Behavior: a paste-like burst may include a full-width/ideographic space (U+3000). It should
/// still be captured as a single paste payload and preserve the exact Unicode content.
#[test]
fn non_ascii_burst_preserves_ideographic_space_and_ascii() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed(String::new(), Instant::now());

    for ch in ['你', '　', '好'] {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    for ch in ['h', 'i'] {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }

    assert!(composer.draft.textarea.text().is_empty());
    let _ = flush_after_paste_burst(&mut composer);
    assert_eq!(composer.draft.textarea.text(), "你　好\nhi");
}

/// Behavior: a large multi-line payload containing both non-ASCII and ASCII (e.g. "UTF-8",
/// "Unicode") should be captured as a single paste-like burst, and Enter key events should
/// become `\n` within the buffered content.
#[test]
fn non_ascii_burst_buffers_large_multiline_mixed_ascii_and_unicode() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    const LARGE_MIXED_PAYLOAD: &str = "天地玄黄 宇宙洪荒\n\
日月盈昃 辰宿列张\n\
寒来暑往 秋收冬藏\n\
\n\
你好世界 编码测试\n\
汉字处理 UTF-8\n\
终端显示 正确无误\n\
\n\
风吹竹林 月照大江\n\
白云千载 青山依旧\n\
程序员 与 Unicode 同行";

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Force an active burst so the test doesn't depend on timing heuristics.
    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed(String::new(), Instant::now());

    for ch in LARGE_MIXED_PAYLOAD.chars() {
        let code = if ch == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(ch)
        };
        let _ = composer.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
    }

    assert!(composer.draft.textarea.text().is_empty());
    let _ = flush_after_paste_burst(&mut composer);
    assert_eq!(composer.draft.textarea.text(), LARGE_MIXED_PAYLOAD);
}

/// Behavior: while a paste-like burst is active, Enter should not submit; it should insert a
/// newline into the buffered payload and flush as a single paste later.
#[test]
fn ascii_burst_treats_enter_as_newline_even_when_parent_owned() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    for parent_owned in [false, true] {
        let (mut composer, _rx) = new_test_composer();
        if parent_owned {
            composer.set_parent_owned_thread();
        }
        let mut now = Instant::now();
        let step = Duration::from_millis(1);

        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            now,
        );
        now += step;
        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            now,
        );
        now += step;

        let (result, _) = composer.handle_submission_with_time(/*should_queue*/ false, now);
        assert!(
            matches!(result, InputResult::None),
            "Enter during a burst should insert newline, not submit"
        );

        for ch in ['t', 'h', 'e', 'r', 'e'] {
            now += step;
            let _ = composer.handle_input_basic_with_time(
                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
                now,
            );
        }

        assert!(composer.draft.textarea.text().is_empty());
        let flush_time = now + PasteBurst::recommended_active_flush_delay() + step;
        let flushed = composer.handle_paste_burst_flush(flush_time);
        assert!(flushed, "expected paste burst to flush");
        assert_eq!(composer.draft.textarea.text(), "hi\nthere");
    }
}

/// Behavior: startup-pending submissions are queued immediately, so Enter should flush any
/// buffered burst text into that queued message instead of turning into a draft newline.
#[test]
fn queued_submission_flushes_ascii_burst_instead_of_inserting_newline() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let mut now = Instant::now();
    let step = Duration::from_millis(1);
    for ch in ['h', 'i'] {
        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            now,
        );
        now += step;
    }
    assert!(composer.is_in_paste_burst());

    let (result, _) = composer.handle_submission_with_time(/*should_queue*/ true, now);

    assert_eq!(
        result,
        InputResult::Queued {
            text: "hi".to_string(),
            text_elements: Vec::new(),
            action: QueuedInputAction::Plain,
            pending_pastes: Vec::new(),
        }
    );
    assert!(composer.draft.textarea.text().is_empty());
    assert!(!composer.is_in_paste_burst());
}

/// Behavior: even if Enter suppression would normally be active for a burst, Enter should
/// still dispatch a built-in slash command when the first line begins with `/`.
#[test]
fn slash_context_enter_ignores_paste_burst_enter_suppression() {
    use crate::slash_command::SlashCommand;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.draft.textarea.set_text_clearing_elements("/diff");
    composer.draft.textarea.set_cursor("/diff".len());
    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed(String::new(), Instant::now());

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Command(SlashCommand::Diff)));
}

/// Behavior: if a burst is buffering text and the user presses a non-char key, flush the
/// buffered burst *before* applying that key so the buffer cannot get stuck.
#[test]
fn non_char_key_flushes_active_burst_before_input() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Force an active burst so we can deterministically buffer characters without relying on
    // timing.
    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed(String::new(), Instant::now());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    assert!(composer.draft.textarea.text().is_empty());
    assert!(composer.is_in_paste_burst());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "hi");
    assert_eq!(composer.draft.textarea.cursor(), 1);
    assert!(!composer.is_in_paste_burst());
}

/// Behavior: enabling `disable_paste_burst` flushes any held first character (flicker
/// suppression) and then inserts subsequent chars immediately without creating burst state.
#[test]
fn disable_paste_burst_flushes_pending_first_char_and_inserts_immediately() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // First ASCII char is normally held briefly. Flip the config mid-stream and ensure the
    // held char is not dropped.
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(composer.is_in_paste_burst());
    assert!(composer.draft.textarea.text().is_empty());

    composer.set_disable_paste_burst(/*disabled*/ true);
    assert_eq!(composer.draft.textarea.text(), "a");
    assert!(!composer.is_in_paste_burst());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "ab");
    assert!(!composer.is_in_paste_burst());
}

/// Behavior: a small explicit paste inserts text directly (no placeholder), and the submitted
/// text matches what is visible in the textarea.
#[test]
fn handle_paste_small_inserts_text() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let sanitized = "_count_rows\tindent\ntwo";
    let needs_redraw =
        composer.handle_paste("_count_r\x1b[13;2:3uows\tindent\n\0two\u{7f}".to_string());
    assert!(needs_redraw);
    assert_eq!(composer.draft.textarea.text(), sanitized);
    assert!(composer.draft.pending_pastes.is_empty());

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, sanitized),
        _ => panic!("expected Submitted"),
    }
}

#[test]
fn empty_enter_returns_none() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Ensure composer is empty and press Enter.
    assert!(composer.draft.textarea.text().is_empty());
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match result {
        InputResult::None => {}
        other => panic!("expected None for empty enter, got: {other:?}"),
    }
}

/// Behavior: a large explicit paste inserts a placeholder into the textarea, stores the full
/// content in `pending_pastes`, and expands the placeholder to the full content on submit.
#[test]
fn handle_paste_large_uses_placeholder_and_replaces_on_submit() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let large = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 10);
    let needs_redraw = composer.handle_paste(large.clone());
    assert!(needs_redraw);
    let placeholder = format!("[Pasted Content {} chars]", large.chars().count());
    assert_eq!(composer.draft.textarea.text(), placeholder);
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(composer.draft.pending_pastes[0].0, placeholder);
    assert_eq!(composer.draft.pending_pastes[0].1, large);

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, large),
        _ => panic!("expected Submitted"),
    }
    assert!(composer.draft.pending_pastes.is_empty());
}

#[test]
fn submit_at_character_limit_succeeds() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(true);
    let input = "x".repeat(MAX_USER_INPUT_TEXT_CHARS);
    composer.draft.textarea.set_text_clearing_elements(&input);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        result,
        InputResult::Submitted { text, .. } if text == input
    ));
}

#[test]
fn oversized_submit_reports_error_and_restores_draft() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(true);
    let input = "x".repeat(MAX_USER_INPUT_TEXT_CHARS + 1);
    composer.draft.textarea.set_text_clearing_elements(&input);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(InputResult::None, result);
    assert_eq!(composer.draft.textarea.text(), input);

    let mut found_error = false;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            let message = cell
                .display_lines(/*width*/ 80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(message.contains(&user_input_too_large_message(input.chars().count())));
            found_error = true;
            break;
        }
    }
    assert!(found_error, "expected oversized-input error history cell");
}

#[test]
fn oversized_queued_submission_reports_error_and_restores_draft() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(false);
    let input = "x".repeat(MAX_USER_INPUT_TEXT_CHARS + 1);
    composer.draft.textarea.set_text_clearing_elements(&input);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(InputResult::None, result);
    assert_eq!(composer.draft.textarea.text(), input);

    let mut found_error = false;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            let message = cell
                .display_lines(/*width*/ 80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(message.contains(&user_input_too_large_message(input.chars().count())));
            found_error = true;
            break;
        }
    }
    assert!(found_error, "expected oversized-input error history cell");
}

/// Behavior: editing that removes a paste placeholder should also clear the associated
/// `pending_pastes` entry so it cannot be submitted accidentally.
#[test]
fn edit_clears_pending_paste() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let large = "y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1);
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.handle_paste(large);
    assert_eq!(composer.draft.pending_pastes.len(), 1);

    // Any edit that removes the placeholder should clear pending_paste
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(composer.draft.pending_pastes.is_empty());
}

#[test]
fn ui_snapshots() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut terminal = match Terminal::new(TestBackend::new(100, 10)) {
        Ok(t) => t,
        Err(e) => panic!("Failed to create terminal: {e}"),
    };

    let test_cases = vec![
        ("empty", None),
        ("small", Some("short".to_string())),
        ("large", Some("z".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5))),
        ("multiple_pastes", None),
        ("backspace_after_pastes", None),
    ];

    for (name, input) in test_cases {
        // Create a fresh composer for each test case
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            sender.clone(),
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        );

        if let Some(text) = input {
            composer.handle_paste(text);
        } else if name == "multiple_pastes" {
            // First large paste
            composer.handle_paste("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3));
            // Second large paste
            composer.handle_paste("y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7));
            // Small paste
            composer.handle_paste(" another short paste".to_string());
        } else if name == "backspace_after_pastes" {
            // Three large pastes
            composer.handle_paste("a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 2));
            composer.handle_paste("b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4));
            composer.handle_paste("c".repeat(LARGE_PASTE_CHAR_THRESHOLD + 6));
            // Move cursor to end and press backspace
            composer
                .draft
                .textarea
                .set_cursor(composer.draft.textarea.text().len());
            composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }

        terminal
            .draw(|f| composer.render(f.area(), f.buffer_mut()))
            .unwrap_or_else(|e| panic!("Failed to draw {name} composer: {e}"));

        insta::assert_snapshot!(name, terminal.backend());
    }
}

#[test]
fn image_placeholder_snapshots() {
    snapshot_composer_state(
        "image_placeholder_single",
        /*enhanced_keys_supported*/ false,
        |composer| {
            let (mut previous, _rx) = new_test_composer();
            previous.attach_image(PathBuf::from("/tmp/image1.png"));
            previous.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
            composer.restore_kill_buffer_snapshot(previous.take_kill_buffer_snapshot());
            composer.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        },
    );

    snapshot_composer_state(
        "image_placeholder_multiple",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.attach_image(PathBuf::from("/tmp/image1.png"));
            composer.attach_image(PathBuf::from("/tmp/image2.png"));
        },
    );
}

#[test]
fn remote_image_rows_snapshots() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    snapshot_composer_state(
        "remote_image_rows",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_remote_image_urls(vec![
                "https://example.com/one.png".to_string(),
                "https://example.com/two.png".to_string(),
            ]);
            composer.set_text_content("describe these".to_string(), Vec::new(), Vec::new());
        },
    );

    snapshot_composer_state(
        "remote_image_rows_after_delete_first",
        /*enhanced_keys_supported*/ false,
        |composer| {
            composer.set_remote_image_urls(vec![
                "https://example.com/one.png".to_string(),
                "https://example.com/two.png".to_string(),
            ]);
            composer.set_text_content("describe these".to_string(), Vec::new(), Vec::new());
            composer.draft.textarea.set_cursor(/*pos*/ 0);
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        },
    );
}

#[test]
fn slash_popup_model_clipping_snapshot() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);

    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Type "/mo" humanlike so paste-burst doesn’t interfere.
    type_chars_humanlike(&mut composer, &['/', 'm', 'o']);

    let mut terminal =
        Terminal::new(TestBackend::new(/*width*/ 60, /*height*/ 3)).expect("create terminal");
    terminal
        .draw(|f| {
            composer.render(f.area(), f.buffer_mut());
            assert_eq!(composer.cursor_pos(f.area()), Some((5, 1)));
        })
        .expect("draw composer");

    // The picker sits above the draft; clipping must keep the draft and cursor visible.
    insta::assert_snapshot!("slash_popup_mo_clipped", terminal.backend());
}

#[test]
fn slash_popup_selects_first_matching_command() {
    use super::super::command_popup::CommandItem;
    for (input, expected) in [
        ("/mo", "model"),
        ("/res", "resume"),
        ("/ar", "archive"),
        ("/pet", "pets"),
        ("/bt", "btw"),
        ("/si", "side"),
    ] {
        let (tx, _rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            sender,
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        );
        type_chars_humanlike(&mut composer, &input.chars().collect::<Vec<_>>());

        match &composer.popups.active {
            ActivePopup::Command(popup) => match popup.selected_item() {
                Some(CommandItem::Builtin(cmd)) => assert_eq!(cmd.command(), expected),
                Some(CommandItem::ServiceTier(command)) => {
                    panic!("expected {expected} command, got service tier {command:?}")
                }
                None => panic!("no selected command for '{input}'"),
            },
            _ => panic!("slash popup not active after typing '{input}'"),
        }
    }
}

#[test]
fn service_tier_slash_command_dispatches_from_catalog_name() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_service_tier_commands_enabled(/*enabled*/ true);
    composer.set_service_tier_commands(vec![ServiceTierCommand {
        id: "priority".to_string(),
        name: "fast".to_string(),
        description: "Fastest inference with increased plan usage".to_string(),
    }]);
    type_chars_humanlike(&mut composer, &['/', 'f', 'a', 's', 't']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        result,
        InputResult::ServiceTierCommand(ServiceTierCommand {
            id: "priority".to_string(),
            name: "fast".to_string(),
            description: "Fastest inference with increased plan usage".to_string(),
        })
    );
}

#[test]
fn dynamic_slash_command_becomes_editable_and_is_not_exported() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_voice_command_enabled(/*enabled*/ true);
    type_chars_humanlike(&mut composer, &['/', 'v', 'o', 'i', 'c', 'e', ' ']);
    assert_eq!(composer.draft.textarea.text_elements().len(), 1);

    composer.set_voice_command_enabled(/*enabled*/ false);

    assert_eq!(composer.current_text(), "/voice ");
    assert!(composer.draft.textarea.text_elements().is_empty());
    assert!(composer.text_elements().is_empty());
}

fn flush_after_paste_burst(composer: &mut ChatComposer) -> bool {
    std::thread::sleep(PasteBurst::recommended_active_flush_delay());
    composer.flush_paste_burst_if_due()
}

// Test helper: simulate human typing with a brief delay and flush the paste-burst buffer
pub(super) fn type_chars_humanlike(composer: &mut ChatComposer, chars: &[char]) {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyModifiers;
    for &ch in chars {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        std::thread::sleep(ChatComposer::recommended_paste_flush_delay());
        let _ = composer.flush_paste_burst_if_due();
        if ch == ' ' {
            let _ = composer.handle_key_event(KeyEvent::new_with_kind(
                KeyCode::Char(' '),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ));
        }
    }
}

#[test]
fn slash_init_dispatches_command_and_does_not_submit_literal_text() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Type the slash command.
    type_chars_humanlike(&mut composer, &['/', 'i', 'n', 'i', 't']);

    // Press Enter to dispatch the selected command.
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // When a slash command is dispatched, the composer should return a
    // Command result (not submit literal text) and clear its textarea.
    match result {
        InputResult::Command(cmd) => {
            assert_eq!(cmd.command(), "init");
        }
        InputResult::CommandWithArgs(_, _, _) => {
            panic!("expected command dispatch without args for '/init'")
        }
        InputResult::ServiceTierCommand(command) => {
            panic!("expected init command, got service tier {command:?}")
        }
        InputResult::Submitted { text, .. } => {
            panic!("expected command dispatch, but composer submitted literal text: {text}")
        }
        InputResult::Queued { .. } => {
            panic!("expected command dispatch, but composer queued literal text")
        }
        InputResult::ParentOwnedInputBlocked => {
            panic!("expected command dispatch, but parent-owned input was blocked")
        }
        InputResult::None => panic!("expected Command result for '/init'"),
    }
    assert!(
        composer.draft.textarea.is_empty(),
        "composer should be cleared"
    );
}

#[test]
fn kill_buffer_preserves_image_after_submit() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_steer_enabled(true);
    composer.draft.textarea.insert_str("restore me ");
    let image_path = PathBuf::from("/tmp/killed-image.png");
    composer.attach_image(image_path.clone());

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(composer.draft.textarea.is_empty());

    composer.draft.textarea.insert_str("hello");
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));
    assert!(composer.draft.textarea.is_empty());

    let other_image_path = PathBuf::from("/tmp/other-image.png");
    composer.attach_image(other_image_path.clone());
    composer.handle_paste(" ".into());
    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        result,
        InputResult::Submitted {
            text: "[Image #1] restore me [Image #2]".into(),
            text_elements: vec![
                TextElement::new((0..10).into(), Some("[Image #1]".into())),
                TextElement::new((22..32).into(), Some("[Image #2]".into())),
            ],
        }
    );
    assert_eq!(
        composer.take_recent_submission_images(),
        vec![other_image_path, image_path]
    );
}

#[test]
fn kill_buffer_persists_after_slash_command_dispatch() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.draft.textarea.insert_str("restore me");
    composer.draft.textarea.set_cursor(/*pos*/ 0);

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert!(composer.draft.textarea.is_empty());

    composer.draft.textarea.insert_str("/diff");
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Command(cmd) => {
            assert_eq!(cmd.command(), "diff");
        }
        _ => panic!("expected Command result for '/diff'"),
    }
    assert!(composer.draft.textarea.is_empty());

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(composer.draft.textarea.text(), "restore me");
}

#[test]
fn slash_command_disabled_while_task_running_keeps_text() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_task_running(/*running*/ true);
    composer
        .draft
        .textarea
        .set_text_clearing_elements("/compact");

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(InputResult::None, result);
    assert_eq!("/compact", composer.draft.textarea.text());

    let mut found_error = false;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            let message = cell
                .display_lines(/*width*/ 80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(message.contains("disabled while a task is in progress"));
            found_error = true;
            break;
        }
    }
    assert!(found_error, "expected error history cell to be sent");
}

#[test]
fn enter_queues_when_queue_submissions_is_enabled() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_queue_submissions(/*queue_submissions*/ true);
    composer
        .draft
        .textarea
        .set_text_clearing_elements("queued before session");

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(
        result,
        InputResult::Queued {
            text: "queued before session".to_string(),
            text_elements: Vec::new(),
            action: QueuedInputAction::Plain,
            pending_pastes: Vec::new(),
        }
    );
}

#[test]
fn tab_queues_slash_led_prompts_while_task_running_without_validation() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    fn assert_queued_slash(input: &str) {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            sender,
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        );
        composer.set_task_running(/*running*/ true);
        composer.draft.textarea.set_text_clearing_elements(input);

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        match result {
            InputResult::Queued {
                text,
                text_elements,
                action,
                ..
            } => {
                assert_eq!(text, input);
                assert!(text_elements.is_empty());
                assert_eq!(action, QueuedInputAction::ParseSlash);
            }
            other => panic!("expected slash-led input to queue, got {other:?}"),
        }
        assert!(composer.draft.textarea.is_empty());
        assert!(
            rx.try_recv().is_err(),
            "queueing should not report slash errors"
        );
    }

    assert_queued_slash("/compact");
    assert_queued_slash("/review check regressions");
    assert_queued_slash("/fast");
    assert_queued_slash("/does-not-exist");
}

#[test]
fn remapped_submit_does_not_fall_back_to_enter() {
    use crate::key_hint;
    use crate::keymap::RuntimeKeymap;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer
        .draft
        .textarea
        .set_text_clearing_elements("explain the change");
    composer
        .draft
        .textarea
        .set_cursor(composer.draft.textarea.text().len());
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.submit = vec![key_hint::ctrl(KeyCode::Char('j'))];
    composer.set_keymap_bindings(&keymap);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(InputResult::None, result);
    assert_eq!("explain the change\n", composer.draft.textarea.text());
}

#[test]
fn remapped_queue_does_not_fall_back_to_tab() {
    use crate::key_hint;
    use crate::keymap::RuntimeKeymap;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_task_running(/*running*/ true);
    composer
        .draft
        .textarea
        .set_text_clearing_elements("queue me");
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.queue = vec![key_hint::ctrl(KeyCode::Char('q'))];
    composer.set_keymap_bindings(&keymap);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(InputResult::None, result);
    assert_eq!("queue me", composer.draft.textarea.text());
}

#[test]
fn remapped_history_search_does_not_fall_back_to_ctrl_r() {
    use crate::key_hint;
    use crate::keymap::RuntimeKeymap;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.history_search_previous = vec![key_hint::plain(KeyCode::F(2))];
    composer.set_keymap_bindings(&keymap);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(!composer.history_search_active());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert!(composer.history_search_active());
}

#[test]
fn tab_queues_leading_space_slash_as_plain_text_while_task_running() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_task_running(/*running*/ true);
    composer
        .draft
        .textarea
        .set_text_clearing_elements(" /does-not-exist");

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    match result {
        InputResult::Queued { text, action, .. } => {
            assert_eq!(text, "/does-not-exist");
            assert_eq!(action, QueuedInputAction::Plain);
        }
        other => panic!("expected leading-space slash input to queue, got {other:?}"),
    }
}

#[test]
fn tab_queues_bang_shell_prompts_while_task_running_without_execution() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    fn assert_queued_bang_input(
        input: &str,
        expected_text: &str,
        expected_action: QueuedInputAction,
    ) {
        let (tx, mut rx) = unbounded_channel::<AppEvent>();
        let sender = AppEventSender::new(tx);
        let mut composer = ChatComposer::new(
            /*has_input_focus*/ true,
            sender,
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        );
        composer.set_task_running(/*running*/ true);
        composer.handle_paste(input.to_owned());

        let (result, _needs_redraw) =
            composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        match result {
            InputResult::Queued {
                text,
                text_elements,
                action,
                ..
            } => {
                assert_eq!(text, expected_text);
                assert!(text_elements.is_empty());
                assert_eq!(action, expected_action);
            }
            other => panic!("expected bang shell input to queue, got {other:?}"),
        }
        assert!(composer.draft.textarea.is_empty());
        assert!(
            rx.try_recv().is_err(),
            "queueing should not show shell help immediately"
        );
    }

    assert_queued_bang_input("!echo hi", "!echo hi", QueuedInputAction::RunShell);
    assert_queued_bang_input("!", "!", QueuedInputAction::RunShell);
    assert_queued_bang_input(" !echo hi", "!echo hi", QueuedInputAction::Literal);
}

#[test]
fn slash_tab_completion_moves_cursor_to_end() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['/', 'c']);

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(composer.draft.textarea.text(), "/compact ");
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );
}

#[test]
fn slash_tab_completion_wins_over_queueing_while_task_running() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_task_running(/*running*/ true);

    type_chars_humanlike(&mut composer, &['/', 'm', 'o']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert_eq!(result, InputResult::None);
    assert_eq!(composer.draft.textarea.text(), "/model ");
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );
}

#[test]
fn slash_key_completes_selected_slash_command_as_text() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['/', 'm']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));

    assert_eq!(result, InputResult::None);
    assert_eq!(composer.draft.textarea.text(), "/model ");
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );
}

#[test]
fn slash_tab_then_enter_dispatches_builtin_command() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Type a prefix and complete with Tab, which inserts a trailing space
    // and moves the cursor beyond the '/name' token (hides the popup).
    type_chars_humanlike(&mut composer, &['/', 'd', 'i']);
    let (_res, _redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "/diff ");

    // Press Enter: should dispatch the command, not submit literal text.
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Command(cmd) => assert_eq!(cmd.command(), "diff"),
        InputResult::CommandWithArgs(_, _, _) => {
            panic!("expected command dispatch without args for '/diff'")
        }
        InputResult::ServiceTierCommand(command) => {
            panic!("expected diff command, got service tier {command:?}")
        }
        InputResult::Submitted { text, .. } => {
            panic!("expected command dispatch after Tab completion, got literal submit: {text}")
        }
        InputResult::Queued { .. } => {
            panic!("expected command dispatch after Tab completion, got literal queue")
        }
        InputResult::ParentOwnedInputBlocked => {
            panic!("expected command dispatch, but parent-owned input was blocked")
        }
        InputResult::None => panic!("expected Command result for '/diff'"),
    }
    assert!(composer.draft.textarea.is_empty());
}

#[test]
fn slash_command_elementizes_on_space() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_collaboration_modes_enabled(/*enabled*/ true);

    type_chars_humanlike(&mut composer, &['/', 'p', 'l', 'a', 'n', ' ']);

    let text = composer.draft.textarea.text().to_string();
    let elements = composer.draft.textarea.text_elements();
    assert_eq!(text, "/plan ");
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].placeholder(&text), Some("/plan"));
}

#[test]
fn slash_command_elementizes_only_known_commands() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_collaboration_modes_enabled(/*enabled*/ true);

    type_chars_humanlike(&mut composer, &['/', 'U', 's', 'e', 'r', 's', ' ']);

    let text = composer.draft.textarea.text().to_string();
    let elements = composer.draft.textarea.text_elements();
    assert_eq!(text, "/Users ");
    assert!(elements.is_empty());
}

#[test]
fn slash_command_element_removed_when_not_at_start() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['/', 'r', 'e', 'v', 'i', 'e', 'w', ' ']);

    let text = composer.draft.textarea.text().to_string();
    let elements = composer.draft.textarea.text_elements();
    assert_eq!(text, "/review ");
    assert_eq!(elements.len(), 1);

    composer.draft.textarea.set_cursor(/*pos*/ 0);
    type_chars_humanlike(&mut composer, &['x']);

    let text = composer.draft.textarea.text().to_string();
    let elements = composer.draft.textarea.text_elements();
    assert_eq!(text, "x/review ");
    assert!(elements.is_empty());
}

#[test]
fn tab_submits_when_no_task_running() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['h', 'i']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert!(matches!(
        result,
        InputResult::Submitted { ref text, .. } if text == "hi"
    ));
    assert!(composer.draft.textarea.is_empty());
}

#[test]
fn tab_does_not_submit_for_bang_shell_command() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_task_running(/*running*/ false);

    type_chars_humanlike(&mut composer, &['!', 'l', 's']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert!(
        composer.current_text().starts_with("!ls"),
        "expected Tab not to submit or clear a `!` command"
    );
}

#[test]
fn bang_prefixed_slash_text_submits_literal_shell_command() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['!', '/', 'd', 'i', 'f', 'f']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(
        result,
        InputResult::Submitted { ref text, .. } if text == "!/diff"
    ));
}

#[test]
fn slash_mention_dispatches_command_and_inserts_at() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['/', 'm', 'e', 'n', 't', 'i', 'o', 'n']);

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match result {
        InputResult::Command(cmd) => {
            assert_eq!(cmd.command(), "mention");
        }
        InputResult::CommandWithArgs(_, _, _) => {
            panic!("expected command dispatch without args for '/mention'")
        }
        InputResult::ServiceTierCommand(command) => {
            panic!("expected mention command, got service tier {command:?}")
        }
        InputResult::Submitted { text, .. } => {
            panic!("expected command dispatch, but composer submitted literal text: {text}")
        }
        InputResult::Queued { .. } => {
            panic!("expected command dispatch, but composer queued literal text")
        }
        InputResult::ParentOwnedInputBlocked => {
            panic!("expected command dispatch, but parent-owned input was blocked")
        }
        InputResult::None => panic!("expected Command result for '/mention'"),
    }
    assert!(
        composer.draft.textarea.is_empty(),
        "composer should be cleared"
    );
    composer.insert_str("@");
    assert_eq!(composer.draft.textarea.text(), "@");
}

#[test]
fn slash_plan_args_preserve_text_elements() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_collaboration_modes_enabled(/*enabled*/ true);

    type_chars_humanlike(&mut composer, &['/', 'p', 'l', 'a', 'n', ' ']);
    let placeholder = local_image_label_text(/*label_number*/ 1);
    composer.attach_image(PathBuf::from("/tmp/plan.png"));

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match result {
        InputResult::CommandWithArgs(cmd, args, text_elements) => {
            assert_eq!(cmd.command(), "plan");
            assert_eq!(args, placeholder);
            assert_eq!(text_elements.len(), 1);
            assert_eq!(
                text_elements[0].placeholder(&args),
                Some(placeholder.as_str())
            );
        }
        _ => panic!("expected CommandWithArgs for /plan with args"),
    }
}

#[test]
fn file_completion_preserves_large_paste_placeholder_elements() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let large = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
    let placeholder = format!("[Pasted Content {} chars]", large.chars().count());

    composer.handle_paste(large.clone());
    composer.insert_str(" @ma");
    composer.on_file_search_result(
        "ma".to_string(),
        vec![FileMatch {
            score: 1,
            path: PathBuf::from("src/main.rs"),
            match_type: codex_file_search::MatchType::File,
            root: PathBuf::from("/tmp"),
            indices: None,
        }],
    );

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

    let text = composer.draft.textarea.text().to_string();
    assert_eq!(text, format!("{placeholder} src/main.rs "));
    let elements = composer.draft.textarea.text_elements();
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].placeholder(&text), Some(placeholder.as_str()));

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            assert_eq!(text, format!("{large} src/main.rs"));
            assert!(text_elements.is_empty());
        }
        _ => panic!("expected Submitted"),
    }
}

fn complete_file(
    composer: &mut ChatComposer,
    text: &str,
    cursor: usize,
    query: &str,
    selected_path: PathBuf,
) {
    composer.set_text_content(text.to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor(cursor);
    composer.sync_popups();
    let root = selected_path
        .parent()
        .filter(|_| selected_path.is_absolute())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    composer.on_file_search_result(
        query.to_string(),
        vec![FileMatch {
            score: 1,
            path: selected_path,
            match_type: codex_file_search::MatchType::File,
            root,
            indices: None,
        }],
    );
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
}

#[test]
fn legacy_file_completion_ignores_shell_variable_to_the_right() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_skill_mentions(Some(vec![test_skill_metadata("available")]));
    complete_file(
        &mut composer,
        "@src  $HOME",
        /*cursor*/ "@src ".len(),
        "src",
        PathBuf::from("src/main.rs"),
    );

    assert_eq!(composer.current_text(), "src/main.rs  $HOME");
}

#[test]
fn unified_popup_preserves_empty_at_before_existing_text() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_mentions_v2_enabled(/*enabled*/ true);
    composer.set_text_content("@ word".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::MentionV2(_)));
}

#[test]
fn adjacent_file_completion_inserts_leading_separator() {
    let (mut composer, _rx) = new_test_composer();
    configure_bound_plugin_left_of_unbound_plugin(&mut composer);
    composer.insert_selected_file_path("@sample".len().."@sample@other".len(), "src/main.rs");

    assert_eq!(composer.current_text(), "@sample src/main.rs ");
}

#[test]
fn adjacent_image_completion_inserts_leading_separator() {
    let tmp = tempdir().expect("create TempDir");
    let image_path = tmp.path().join("image.png");
    let image: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(3, 2, |_x, _y| Rgba([1, 2, 3, 255]));
    image.save(&image_path).expect("write temp png");

    let (mut composer, _rx) = new_test_composer();
    configure_bound_plugin_left_of_unbound_plugin(&mut composer);
    composer.set_vim_enabled(/*enabled*/ true);
    let before = composer.draft_snapshot();
    composer.insert_selected_file_path(
        "@sample".len().."@sample@other".len(),
        image_path.to_str().expect("UTF-8 path"),
    );

    assert_eq!(composer.current_text(), "@sample [Image #1] ");
    assert_eq!(composer.local_image_paths(), vec![image_path]);
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
    assert_eq!(composer.draft_snapshot(), before);
}

#[test]
fn file_completion_preserves_separator_before_existing_suffix() {
    let (mut composer, _rx) = new_test_composer();
    complete_file(
        &mut composer,
        "@ma next",
        /*cursor*/ "@ma".len(),
        "ma",
        PathBuf::from("src/main.rs"),
    );
    composer.insert_str("foo");

    assert_eq!(composer.current_text(), "src/main.rs foo next");
}

#[test]
fn file_completion_preserves_separator_before_sigiled_suffix() {
    for suffix in ["@next", "$next"] {
        let (mut composer, _rx) = new_test_composer();
        composer.set_text_content(format!("@ma {suffix}"), Vec::new(), Vec::new());
        composer.insert_selected_path(0.."@ma".len(), "src/main.rs");
        composer.insert_str("foo");

        assert_eq!(composer.current_text(), format!("src/main.rs foo {suffix}"));
    }
}

#[test]
fn file_completion_inserts_separator_before_line_break() {
    let (mut composer, _rx) = new_test_composer();
    complete_file(
        &mut composer,
        "@ma\nnext",
        /*cursor*/ "@ma".len(),
        "ma",
        PathBuf::from("src/main.rs"),
    );
    composer.insert_str("foo");

    assert_eq!(composer.current_text(), "src/main.rs foo\nnext");
}

#[test]
fn file_completion_for_sigil_path_does_not_reopen_popup() {
    let (mut composer, _rx) = new_test_composer();
    complete_file(
        &mut composer,
        "@ma\nnext",
        /*cursor*/ "@ma".len(),
        "ma",
        PathBuf::from("@scope/main.rs"),
    );

    assert_eq!(composer.current_text(), "@scope/main.rs \nnext");
    assert!(matches!(composer.popups.active, ActivePopup::None));

    let (result, consumed) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(consumed);
    match result {
        InputResult::Submitted { text, .. } => assert_eq!(text, "@scope/main.rs \nnext"),
        _ => panic!("expected completed path to submit"),
    }
}

#[test]
fn file_completion_does_not_dismiss_identical_next_token() {
    let (mut composer, _rx) = new_test_composer();
    complete_file(
        &mut composer,
        "@ma  @scope/main.rs",
        /*cursor*/ "@ma".len(),
        "ma",
        PathBuf::from("@scope/main.rs"),
    );
    composer.draft.textarea.set_cursor("@scope/main.rs  ".len());
    composer.sync_popups();

    assert!(matches!(composer.popups.active, ActivePopup::File(_)));
}

#[test]
fn dismissed_file_popup_tracks_token_across_leading_whitespace_edits() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_text_content("@ma".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@ma".len());
    composer.sync_popups();
    assert!(matches!(composer.popups.active, ActivePopup::File(_)));

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(composer.popups.active, ActivePopup::None));

    composer.draft.textarea.set_cursor(/*pos*/ 0);
    composer.insert_str("   ");
    assert!(matches!(composer.popups.active, ActivePopup::None));

    for _ in 0..3 {
        let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(matches!(composer.popups.active, ActivePopup::None));
    }
}

#[test]
fn dismissed_file_popup_ignores_token_substrings_in_leading_paste() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_text_content("@ma".to_string(), Vec::new(), Vec::new());
    composer.draft.textarea.set_cursor("@ma".len());
    composer.sync_popups();

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(composer.popups.active, ActivePopup::None));

    composer.draft.textarea.set_cursor(/*pos*/ 0);
    composer.handle_paste("email@ma.com ".to_string());

    assert_eq!(composer.current_text(), "email@ma.com @ma");
    assert!(matches!(composer.popups.active, ActivePopup::None));
}

/// Behavior: multiple paste operations can coexist; placeholders should be expanded to their
/// original content on submission.
#[test]
fn test_multiple_pastes_submission() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Define test cases: (paste content, is_large)
    let test_cases = [
        ("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 3), true),
        (" and ".to_string(), false),
        ("y".repeat(LARGE_PASTE_CHAR_THRESHOLD + 7), true),
    ];

    // Expected states after each paste
    let mut expected_text = String::new();
    let mut expected_pending_count = 0;

    // Apply all pastes and build expected state
    let states: Vec<_> = test_cases
        .iter()
        .map(|(content, is_large)| {
            composer.handle_paste(content.clone());
            if *is_large {
                let placeholder = format!("[Pasted Content {} chars]", content.chars().count());
                expected_text.push_str(&placeholder);
                expected_pending_count += 1;
            } else {
                expected_text.push_str(content);
            }
            (expected_text.clone(), expected_pending_count)
        })
        .collect();

    // Verify all intermediate states were correct
    assert_eq!(
        states,
        vec![
            (
                format!("[Pasted Content {} chars]", test_cases[0].0.chars().count()),
                1
            ),
            (
                format!(
                    "[Pasted Content {} chars] and ",
                    test_cases[0].0.chars().count()
                ),
                1
            ),
            (
                format!(
                    "[Pasted Content {} chars] and [Pasted Content {} chars]",
                    test_cases[0].0.chars().count(),
                    test_cases[2].0.chars().count()
                ),
                2
            ),
        ]
    );

    // Submit and verify final expansion
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    if let InputResult::Submitted { text, .. } = result {
        assert_eq!(text, format!("{} and {}", test_cases[0].0, test_cases[2].0));
    } else {
        panic!("expected Submitted");
    }
}

#[test]
fn test_placeholder_deletion() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Define test cases: (content, is_large)
    let test_cases = [
        ("a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5), true),
        (" and ".to_string(), false),
        ("b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 6), true),
    ];

    // Apply all pastes
    let mut current_pos = 0;
    let states: Vec<_> = test_cases
        .iter()
        .map(|(content, is_large)| {
            composer.handle_paste(content.clone());
            if *is_large {
                let placeholder = format!("[Pasted Content {} chars]", content.chars().count());
                current_pos += placeholder.len();
            } else {
                current_pos += content.len();
            }
            (
                composer.draft.textarea.text().to_string(),
                composer.draft.pending_pastes.len(),
                current_pos,
            )
        })
        .collect();

    // Delete placeholders one by one and collect states
    let mut deletion_states = vec![];

    // First deletion
    composer.draft.textarea.set_cursor(states[0].2);
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    deletion_states.push((
        composer.draft.textarea.text().to_string(),
        composer.draft.pending_pastes.len(),
    ));

    // Second deletion
    composer
        .draft
        .textarea
        .set_cursor(composer.draft.textarea.text().len());
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    deletion_states.push((
        composer.draft.textarea.text().to_string(),
        composer.draft.pending_pastes.len(),
    ));

    // Verify all states
    assert_eq!(
        deletion_states,
        vec![
            (" and [Pasted Content 1006 chars]".to_string(), 1),
            (" and ".to_string(), 0),
        ]
    );
}

/// Behavior: if multiple large pastes share the same placeholder label (same char count),
/// deleting one placeholder removes only its corresponding `pending_pastes` entry.
#[test]
fn deleting_duplicate_length_pastes_removes_only_target() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let placeholder_base = format!("[Pasted Content {} chars]", paste.chars().count());
    let placeholder_second = format!("{placeholder_base} #2");

    composer.handle_paste(paste.clone());
    composer.handle_paste(paste.clone());
    assert_eq!(
        composer.draft.textarea.text(),
        format!("{placeholder_base}{placeholder_second}")
    );
    assert_eq!(composer.draft.pending_pastes.len(), 2);

    composer
        .draft
        .textarea
        .set_cursor(composer.draft.textarea.text().len());
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

    assert_eq!(composer.draft.textarea.text(), placeholder_base);
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(composer.draft.pending_pastes[0].0, placeholder_base);
    assert_eq!(composer.draft.pending_pastes[0].1, paste);
}

/// Behavior: large-paste placeholder numbering continues when another placeholder of the
/// same length still exists, so a new paste gets a new unique placeholder label.
#[test]
fn large_paste_numbering_continues_with_same_length_placeholder() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let base = format!("[Pasted Content {} chars]", paste.chars().count());
    let second = format!("{base} #2");
    let third = format!("{base} #3");

    composer.handle_paste(paste.clone());
    composer.handle_paste(paste.clone());
    assert_eq!(composer.draft.textarea.text(), format!("{base}{second}"));

    composer.draft.textarea.set_cursor(base.len());
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), second);
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(composer.draft.pending_pastes[0].0, second);

    composer
        .draft
        .textarea
        .set_cursor(composer.draft.textarea.text().len());
    composer.handle_paste(paste);

    assert_eq!(composer.draft.textarea.text(), format!("{second}{third}"));
    assert_eq!(composer.draft.pending_pastes.len(), 2);
    assert_eq!(composer.draft.pending_pastes[0].0, second);
    assert_eq!(composer.draft.pending_pastes[1].0, third);
}

/// Behavior: if all placeholders of a given length are removed, numbering resets to the
/// base placeholder on the next paste.
#[test]
fn large_paste_numbering_reuses_after_all_deleted() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let base = format!("[Pasted Content {} chars]", paste.chars().count());

    composer.handle_paste(paste.clone());
    assert_eq!(composer.draft.textarea.text(), base);
    assert_eq!(composer.draft.pending_pastes.len(), 1);

    composer
        .draft
        .textarea
        .set_cursor(composer.draft.textarea.text().len());
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(composer.draft.textarea.text().is_empty());
    assert!(composer.draft.pending_pastes.is_empty());

    composer.handle_paste(paste);
    assert_eq!(composer.draft.textarea.text(), base);
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(composer.draft.pending_pastes[0].0, base);
}

#[test]
fn test_partial_placeholder_deletion() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Define test cases: (cursor_position_from_end, expected_pending_count)
    let test_cases = [
        5, // Delete from middle - should clear tracking
        0, // Delete from end - should clear tracking
    ];

    let paste = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let placeholder = format!("[Pasted Content {} chars]", paste.chars().count());

    let states: Vec<_> = test_cases
        .into_iter()
        .map(|pos_from_end| {
            composer.handle_paste(paste.clone());
            composer
                .draft
                .textarea
                .set_cursor(placeholder.len() - pos_from_end);
            composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            let result = (
                composer.draft.textarea.text().contains(&placeholder),
                composer.draft.pending_pastes.len(),
            );
            composer.draft.textarea.set_text_clearing_elements("");
            result
        })
        .collect();

    assert_eq!(
        states,
        vec![
            (false, 0), // After deleting from middle
            (false, 0), // After deleting from end
        ]
    );
}

// --- Image attachment tests ---
#[test]
fn attach_image_and_submit_includes_local_image_paths() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let path = PathBuf::from("/tmp/image1.png");
    composer.attach_image(path.clone());
    composer.handle_paste(" hi".into());
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            assert_eq!(text, "[Image #1] hi");
            assert_eq!(text_elements.len(), 1);
            assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
            assert_eq!(
                text_elements[0].byte_range,
                ByteRange {
                    start: 0,
                    end: "[Image #1]".len()
                }
            );
        }
        _ => panic!("expected Submitted"),
    }
    let imgs = composer.take_recent_submission_images();
    assert_eq!(vec![path], imgs);
}

#[test]
fn submit_captures_recent_mention_bindings_before_clearing_textarea() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let mention_bindings = vec![MentionBinding {
        sigil: '$',
        mention: "figma".to_string(),
        path: "/tmp/user/figma/SKILL.md".to_string(),
    }];
    composer.set_text_content_with_mention_bindings(
        "$figma please".to_string(),
        Vec::new(),
        Vec::new(),
        mention_bindings.clone(),
    );

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));
    assert_eq!(
        composer.take_recent_submission_mention_bindings(),
        mention_bindings
    );
    assert!(composer.take_mention_bindings().is_empty());
}

#[test]
fn history_navigation_restores_remote_and_local_image_attachments() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let remote_image_url = "https://example.com/remote.png".to_string();
    composer.set_remote_image_urls(vec![remote_image_url.clone()]);
    let path = PathBuf::from("/tmp/image1.png");
    composer.attach_image(path.clone());

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    let _ = composer.take_remote_image_urls();
    composer.set_text_content(String::new(), Vec::new(), Vec::new());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));

    let text = composer.current_text();
    assert_eq!(text, "[Image #2]");
    let text_elements = composer.text_elements();
    assert_eq!(text_elements.len(), 1);
    assert_eq!(text_elements[0].placeholder(&text), Some("[Image #2]"));
    assert_eq!(composer.local_image_paths(), vec![path]);
    assert_eq!(composer.remote_image_urls(), vec![remote_image_url]);
}

#[test]
fn history_navigation_restores_remote_only_submissions() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let remote_image_urls = vec![
        "https://example.com/one.png".to_string(),
        "https://example.com/two.png".to_string(),
    ];
    composer.set_remote_image_urls(remote_image_urls.clone());

    let (submitted_text, submitted_elements) = composer
        .prepare_submission_text(/*record_history*/ true)
        .expect("remote-only submission should be prepared");
    assert_eq!(submitted_text, "");
    assert!(submitted_elements.is_empty());

    let _ = composer.take_remote_image_urls();
    composer.set_text_content(String::new(), Vec::new(), Vec::new());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.current_text(), "");
    assert!(composer.text_elements().is_empty());
    assert_eq!(composer.remote_image_urls(), remote_image_urls);
}

#[test]
fn history_navigation_leaves_cursor_at_end_of_line() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['f', 'i', 'r', 's', 't']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    type_chars_humanlike(&mut composer, &['s', 'e', 'c', 'o', 'n', 'd']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "second");
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "first");
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "second");
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(composer.draft.textarea.is_empty());
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.draft.textarea.text().len()
    );
}

#[test]
fn vim_normal_operator_pending_consumes_submit_key() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("hello".to_string(), Vec::new(), Vec::new());
    composer.set_vim_enabled(/*enabled*/ true);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    assert!(composer.draft.textarea.is_vim_operator_pending());

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(matches!(result, InputResult::None));
    assert_eq!(composer.draft.textarea.text(), "hello");
    assert_eq!(
        composer.vim_mode_indicator_span(),
        Some("Vim: Normal".magenta())
    );
    assert!(!composer.draft.textarea.is_vim_operator_pending());
}

#[test]
fn remapped_editor_history_navigation_does_not_fall_back_to_up() {
    use crate::key_hint;
    use crate::keymap::RuntimeKeymap;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['f', 'i', 'r', 's', 't']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    let mut keymap = RuntimeKeymap::defaults();
    Arc::make_mut(&mut keymap.editor).move_up = vec![key_hint::plain(KeyCode::F(2))];
    composer.set_keymap_bindings(&keymap);
    assert!(Arc::ptr_eq(&composer.editor_keymap, &keymap.editor));
    assert_eq!(Arc::strong_count(&keymap.editor), 3);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert!(composer.draft.textarea.is_empty());

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));
    assert_eq!(composer.draft.textarea.text(), "first");
}

#[test]
fn history_navigation_from_start_of_bang_command_recalls_older_entry() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['f', 'i', 'r', 's', 't']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    type_chars_humanlike(&mut composer, &['!', 'g', 'i', 't']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.current_text(), "!git");

    composer.draft.textarea.set_cursor(/*pos*/ 0);
    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(composer.current_text(), "first");
}

#[test]
fn vim_normal_history_navigation_from_start_of_bang_command_recalls_older_entry() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    type_chars_humanlike(&mut composer, &['f', 'i', 'r', 's', 't']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    type_chars_humanlike(&mut composer, &['!', 'g', 'i', 't']);
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));

    composer.set_vim_enabled(/*enabled*/ true);

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    assert_eq!(composer.current_text(), "!git");
    assert_eq!(composer.draft.textarea.cursor(), "git".len() - 1);

    let (_result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    assert_eq!(composer.current_text(), "first");
    assert_eq!(composer.draft.textarea.cursor(), "first".len() - 1);
}

#[test]
fn set_text_content_reattaches_images_without_placeholder_metadata() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let placeholder = local_image_label_text(/*label_number*/ 1);
    let text = format!("{placeholder} restored");
    let text_elements = vec![TextElement::new(
        (0..placeholder.len()).into(),
        /*placeholder*/ None,
    )];
    let path = PathBuf::from("/tmp/image1.png");

    composer.set_text_content(text, text_elements, vec![path.clone()]);

    assert_eq!(composer.local_image_paths(), vec![path]);
}

#[test]
fn large_paste_preserves_image_text_elements_on_submit() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
    composer.handle_paste(large_content.clone());
    composer.handle_paste(" ".into());
    let path = PathBuf::from("/tmp/image_with_paste.png");
    composer.attach_image(path.clone());

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            let expected = format!("{large_content} [Image #1]");
            assert_eq!(text, expected);
            assert_eq!(text_elements.len(), 1);
            assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
            assert_eq!(
                text_elements[0].byte_range,
                ByteRange {
                    start: large_content.len() + 1,
                    end: large_content.len() + 1 + "[Image #1]".len(),
                }
            );
        }
        _ => panic!("expected Submitted"),
    }
    let imgs = composer.take_recent_submission_images();
    assert_eq!(vec![path], imgs);
}

#[test]
fn large_paste_with_leading_whitespace_trims_and_shifts_elements() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let large_content = format!("  {}", "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5));
    composer.handle_paste(large_content.clone());
    composer.handle_paste(" ".into());
    let path = PathBuf::from("/tmp/image_with_trim.png");
    composer.attach_image(path.clone());

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            let trimmed = large_content.trim().to_string();
            assert_eq!(text, format!("{trimmed} [Image #1]"));
            assert_eq!(text_elements.len(), 1);
            assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
            assert_eq!(
                text_elements[0].byte_range,
                ByteRange {
                    start: trimmed.len() + 1,
                    end: trimmed.len() + 1 + "[Image #1]".len(),
                }
            );
        }
        _ => panic!("expected Submitted"),
    }
    let imgs = composer.take_recent_submission_images();
    assert_eq!(vec![path], imgs);
}

#[test]
fn pasted_crlf_normalizes_newlines_for_elements() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let pasted = "line1\r\nline2\r\n".to_string();
    composer.handle_paste(pasted);
    composer.handle_paste(" ".into());
    let path = PathBuf::from("/tmp/image_crlf.png");
    composer.attach_image(path.clone());

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            assert_eq!(text, "line1\nline2\n [Image #1]");
            assert!(!text.contains('\r'));
            assert_eq!(text_elements.len(), 1);
            assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
            assert_eq!(
                text_elements[0].byte_range,
                ByteRange {
                    start: "line1\nline2\n ".len(),
                    end: "line1\nline2\n [Image #1]".len(),
                }
            );
        }
        _ => panic!("expected Submitted"),
    }
    let imgs = composer.take_recent_submission_images();
    assert_eq!(vec![path], imgs);
}

#[test]
fn suppressed_submission_restores_pending_paste_payload() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer
        .draft
        .textarea
        .set_text_clearing_elements("/unknown ");
    composer.draft.textarea.set_cursor("/unknown ".len());
    let large_content = "x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 5);
    composer.handle_paste(large_content.clone());
    let placeholder = composer
        .draft
        .pending_pastes
        .first()
        .expect("expected pending paste")
        .0
        .clone();

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::None));
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(
        composer.draft.textarea.text(),
        format!("/unknown {placeholder}")
    );

    composer.draft.textarea.set_cursor(/*pos*/ 0);
    composer.draft.textarea.insert_str(" ");
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            assert_eq!(text, format!("/unknown {large_content}"));
            assert!(text_elements.is_empty());
        }
        _ => panic!("expected Submitted"),
    }
    assert!(composer.draft.pending_pastes.is_empty());
}

#[test]
fn attach_image_without_text_submits_empty_text_and_images() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let path = PathBuf::from("/tmp/image2.png");
    composer.attach_image(path.clone());
    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match result {
        InputResult::Submitted {
            text,
            text_elements,
        } => {
            assert_eq!(text, "[Image #1]");
            assert_eq!(text_elements.len(), 1);
            assert_eq!(text_elements[0].placeholder(&text), Some("[Image #1]"));
            assert_eq!(
                text_elements[0].byte_range,
                ByteRange {
                    start: 0,
                    end: "[Image #1]".len()
                }
            );
        }
        _ => panic!("expected Submitted"),
    }
    let imgs = composer.take_recent_submission_images();
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0], path);
    assert!(composer.attachments.local_images.is_empty());
}

#[test]
fn duplicate_image_placeholders_get_suffix() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let path = PathBuf::from("/tmp/image_dup.png");
    composer.attach_image(path.clone());
    composer.handle_paste(" ".into());
    composer.attach_image(path);

    let text = composer.draft.textarea.text().to_string();
    assert!(text.contains("[Image #1]"));
    assert!(text.contains("[Image #2]"));
    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        "[Image #1]"
    );
    assert_eq!(
        composer.attachments.local_images[1].placeholder,
        "[Image #2]"
    );
}

#[test]
fn image_placeholder_backspace_behaves_like_text_placeholder() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    let path = PathBuf::from("/tmp/image3.png");
    composer.attach_image(path.clone());
    let placeholder = composer.attachments.local_images[0].placeholder.clone();

    // Case 1: backspace at end
    composer
        .draft
        .textarea
        .move_cursor_to_end_of_line(/*move_down_at_eol*/ false);
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(!composer.draft.textarea.text().contains(&placeholder));
    assert!(composer.attachments.local_images.is_empty());

    // Re-add and ensure backspace at element start does not delete the placeholder.
    composer.attach_image(path);
    let placeholder2 = composer.attachments.local_images[0].placeholder.clone();
    // Move cursor to roughly middle of placeholder
    if let Some(start_pos) = composer.draft.textarea.text().find(&placeholder2) {
        let mid_pos = start_pos + (placeholder2.len() / 2);
        composer.draft.textarea.set_cursor(mid_pos);
        composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(composer.draft.textarea.text().contains(&placeholder2));
        assert_eq!(composer.attachments.local_images.len(), 1);
    } else {
        panic!("Placeholder not found in textarea");
    }
}

#[test]
fn backspace_with_multibyte_text_before_placeholder_does_not_panic() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Insert an image placeholder at the start
    let path = PathBuf::from("/tmp/image_multibyte.png");
    composer.attach_image(path);
    // Add multibyte text after the placeholder
    composer.draft.textarea.insert_str("日本語");

    // Cursor is at end; pressing backspace should delete the last character
    // without panicking and leave the placeholder intact.
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

    assert_eq!(composer.attachments.local_images.len(), 1);
    assert!(composer.draft.textarea.text().starts_with("[Image #1]"));
}

#[test]
fn deleting_one_of_duplicate_image_placeholders_removes_one_entry() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let path1 = PathBuf::from("/tmp/image_dup1.png");
    let path2 = PathBuf::from("/tmp/image_dup2.png");

    composer.attach_image(path1);
    // separate placeholders with a space for clarity
    composer.handle_paste(" ".into());
    composer.attach_image(path2.clone());

    let placeholder1 = composer.attachments.local_images[0].placeholder.clone();
    let placeholder2 = composer.attachments.local_images[1].placeholder.clone();
    let text = composer.draft.textarea.text().to_string();
    let start1 = text.find(&placeholder1).expect("first placeholder present");
    let end1 = start1 + placeholder1.len();
    composer.draft.textarea.set_cursor(end1);

    // Backspace should delete the first placeholder and its mapping.
    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

    let new_text = composer.draft.textarea.text().to_string();
    assert_eq!(
        1,
        new_text.matches(&placeholder1).count(),
        "one placeholder remains after deletion"
    );
    assert_eq!(
        0,
        new_text.matches(&placeholder2).count(),
        "second placeholder was relabeled"
    );
    assert_eq!(
        1,
        new_text.matches("[Image #1]").count(),
        "remaining placeholder relabeled to #1"
    );
    assert_eq!(
        vec![LocalImageAttachment {
            path: path2,
            placeholder: "[Image #1]".to_string()
        }],
        composer.attachments.local_images,
        "one image mapping remains"
    );
}

#[test]
fn deleting_reordered_image_one_renumbers_text_in_place() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let path1 = PathBuf::from("/tmp/image_first.png");
    let path2 = PathBuf::from("/tmp/image_second.png");
    let placeholder1 = local_image_label_text(/*label_number*/ 1);
    let placeholder2 = local_image_label_text(/*label_number*/ 2);

    // Placeholders can be reordered in the text buffer; deleting image #1 should renumber
    // image #2 wherever it appears, not just after the cursor.
    let text = format!("Test {placeholder2} test {placeholder1}");
    let start2 = text.find(&placeholder2).expect("placeholder2 present");
    let start1 = text.find(&placeholder1).expect("placeholder1 present");
    let text_elements = vec![
        TextElement::new(
            ByteRange {
                start: start2,
                end: start2 + placeholder2.len(),
            },
            Some(placeholder2),
        ),
        TextElement::new(
            ByteRange {
                start: start1,
                end: start1 + placeholder1.len(),
            },
            Some(placeholder1.clone()),
        ),
    ];
    composer.set_text_content(text, text_elements, vec![path1, path2.clone()]);

    let end1 = start1 + placeholder1.len();
    composer.draft.textarea.set_cursor(end1);

    composer.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

    assert_eq!(
        composer.draft.textarea.text(),
        format!("Test {placeholder1} test ")
    );
    assert_eq!(
        vec![LocalImageAttachment {
            path: path2,
            placeholder: placeholder1
        }],
        composer.attachments.local_images,
        "attachment renumbered after deletion"
    );
}

#[test]
fn deleting_first_text_element_renumbers_following_text_element() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let path1 = PathBuf::from("/tmp/image_first.png");
    let path2 = PathBuf::from("/tmp/image_second.png");

    // Insert two adjacent atomic elements.
    composer.attach_image(path1);
    composer.attach_image(path2.clone());
    assert_eq!(composer.draft.textarea.text(), "[Image #1][Image #2]");
    assert_eq!(composer.attachments.local_images.len(), 2);

    // Delete the first element using normal textarea editing (forward Delete at cursor start).
    composer.draft.textarea.set_cursor(/*pos*/ 0);
    composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

    // Remaining image should be renumbered and the textarea element updated.
    assert_eq!(composer.attachments.local_images.len(), 1);
    assert_eq!(composer.attachments.local_images[0].path, path2);
    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        "[Image #1]"
    );
    assert_eq!(composer.draft.textarea.text(), "[Image #1]");
}

#[test]
fn pasting_filepath_attaches_image() {
    let tmp = tempdir().expect("create TempDir");
    let tmp_path: PathBuf = tmp.path().join("codex_tui_test_paste_image.png");
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_fn(3, 2, |_x, _y| Rgba([1, 2, 3, 255]));
    img.save(&tmp_path).expect("failed to write temp png");

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let needs_redraw = composer.handle_paste(tmp_path.to_string_lossy().to_string());
    assert!(needs_redraw);
    assert!(composer.draft.textarea.text().starts_with("[Image #1] "));

    let imgs = composer.take_recent_submission_images();
    assert_eq!(imgs, vec![tmp_path]);
}

#[test]
fn slash_path_input_submits_without_command_error() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer
        .draft
        .textarea
        .set_text_clearing_elements("/Users/example/project/src/main.rs");

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    if let InputResult::Submitted { text, .. } = result {
        assert_eq!(text, "/Users/example/project/src/main.rs");
    } else {
        panic!("expected Submitted");
    }
    assert!(composer.draft.textarea.is_empty());
    match rx.try_recv() {
        Ok(event) => panic!("unexpected event: {event:?}"),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        Err(err) => panic!("unexpected channel state: {err:?}"),
    }
}

#[test]
fn slash_with_leading_space_submits_as_text() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer
        .draft
        .textarea
        .set_text_clearing_elements(" /this-looks-like-a-command");

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    if let InputResult::Submitted { text, .. } = result {
        assert_eq!(text, "/this-looks-like-a-command");
    } else {
        panic!("expected Submitted");
    }
    assert!(composer.draft.textarea.is_empty());
    match rx.try_recv() {
        Ok(event) => panic!("unexpected event: {event:?}"),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        Err(err) => panic!("unexpected channel state: {err:?}"),
    }
}

#[test]
fn shortcut_modified_input_does_not_enter_paste_bursts() {
    snapshot_composer_state(
        "shortcut_modified_space_is_not_rendered",
        /*enhanced_keys_supported*/ false,
        |composer| {
            let now = Instant::now();
            for input in [
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SUPER),
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
            ] {
                composer.handle_input_basic_with_time(input, now);
            }

            assert!(
                composer
                    .handle_paste_burst_flush(now + PasteBurst::recommended_active_flush_delay())
            );
        },
    );

    for modifiers in [
        KeyModifiers::SUPER,
        KeyModifiers::SHIFT | KeyModifiers::SUPER,
        KeyModifiers::HYPER,
        KeyModifiers::META,
    ] {
        let (mut composer, _rx) = new_test_composer();
        let now = Instant::now();

        composer.handle_input_basic_with_time(KeyEvent::new(KeyCode::Char(' '), modifiers), now);

        assert!(
            !composer.is_in_paste_burst(),
            "shortcut modifiers {modifiers:?} must not start a paste burst"
        );
        assert!(!composer.handle_paste_burst_flush(now + PasteBurst::recommended_flush_delay()));
        assert_eq!(composer.draft.textarea.text(), "");
    }
}

#[test]
fn plain_and_shift_spaces_insert_normally() {
    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
        let (mut composer, _rx) = new_test_composer();
        let now = Instant::now();

        composer.handle_input_basic_with_time(KeyEvent::new(KeyCode::Char(' '), modifiers), now);

        assert!(composer.is_in_paste_burst());
        assert_eq!(composer.draft.textarea.text(), "");
        assert!(composer.handle_paste_burst_flush(now + PasteBurst::recommended_flush_delay()));
        assert_eq!(composer.draft.textarea.text(), " ");
        assert!(!composer.is_in_paste_burst());
    }

    snapshot_composer_state(
        "plain_space_is_rendered",
        /*enhanced_keys_supported*/ false,
        |composer| {
            let now = Instant::now();
            for input in [
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
            ] {
                composer.handle_input_basic_with_time(input, now);
            }

            assert!(
                composer
                    .handle_paste_burst_flush(now + PasteBurst::recommended_active_flush_delay())
            );
        },
    );
}

#[test]
fn non_text_input_flushes_pending_paste_without_inserting_text() {
    let (mut composer, _rx) = new_test_composer();
    let now = Instant::now();

    composer
        .handle_input_basic_with_time(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), now);
    composer
        .handle_input_basic_with_time(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE), now);

    assert!(composer.is_in_paste_burst());
    assert_eq!(composer.draft.textarea.text(), "");

    composer
        .handle_input_basic_with_time(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::SUPER), now);

    assert_eq!(composer.draft.textarea.text(), "ab");
    assert!(!composer.is_in_paste_burst());
    assert!(!composer.handle_paste_burst_flush(now + PasteBurst::recommended_flush_delay()));
}

#[test]
fn platform_specific_altgr_input_remains_text() {
    for modifiers in [
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT,
    ] {
        let (mut composer, _rx) = new_test_composer();
        let now = Instant::now();

        composer.handle_input_basic_with_time(KeyEvent::new(KeyCode::Char('@'), modifiers), now);

        if cfg!(windows) {
            assert!(composer.is_in_paste_burst());
            assert!(composer.handle_paste_burst_flush(now + PasteBurst::recommended_flush_delay()));
            assert_eq!(composer.draft.textarea.text(), "@");
        } else {
            assert!(!composer.is_in_paste_burst());
            assert!(
                !composer.handle_paste_burst_flush(now + PasteBurst::recommended_flush_delay())
            );
            assert_eq!(composer.draft.textarea.text(), "");
        }
    }
}

/// Behavior: the first fast ASCII character is held briefly to avoid flicker; if no burst
/// follows, it should eventually flush as normal typed input (not as a paste).
#[test]
fn pending_first_ascii_char_flushes_as_typed() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    assert!(composer.is_in_paste_burst());
    assert!(composer.draft.textarea.text().is_empty());

    std::thread::sleep(ChatComposer::recommended_paste_flush_delay());
    let flushed = composer.flush_paste_burst_if_due();
    assert!(flushed, "expected pending first char to flush");
    assert_eq!(composer.draft.textarea.text(), "h");
    assert!(!composer.is_in_paste_burst());
}

/// Behavior: fast "paste-like" ASCII input should buffer and then flush as a single paste. If
/// the payload is small, it should insert directly (no placeholder).
#[test]
fn burst_paste_fast_small_buffers_and_flushes_on_stop() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let count = 32;
    let mut now = Instant::now();
    let step = Duration::from_millis(/*millis*/ 1);
    for _ in 0..count {
        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            now,
        );
        assert!(
            composer.is_in_paste_burst(),
            "expected active paste burst during fast typing"
        );
        assert!(
            composer.draft.textarea.text().is_empty(),
            "text should not appear during burst"
        );
        now += step;
    }

    assert!(
        composer.draft.textarea.text().is_empty(),
        "text should remain empty until flush"
    );
    let flush_time = now + PasteBurst::recommended_active_flush_delay() + step;
    let flushed = composer.handle_paste_burst_flush(flush_time);
    assert!(flushed, "expected buffered text to flush after stop");
    assert_eq!(composer.draft.textarea.text(), "a".repeat(count));
    assert!(
        composer.draft.pending_pastes.is_empty(),
        "no placeholder for small burst"
    );
}

/// Behavior: fast "paste-like" ASCII input should buffer and then flush as a single paste. If
/// the payload is large, it should insert a placeholder and defer the full text until submit.
#[test]
fn burst_paste_fast_large_inserts_placeholder_on_flush() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let count = LARGE_PASTE_CHAR_THRESHOLD + 1; // > threshold to trigger placeholder
    let mut now = Instant::now();
    let step = Duration::from_millis(/*millis*/ 1);
    for _ in 0..count {
        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            now,
        );
        now += step;
    }

    // Nothing should appear until we stop and flush
    assert!(composer.draft.textarea.text().is_empty());
    let flush_time = now + PasteBurst::recommended_active_flush_delay() + step;
    let flushed = composer.handle_paste_burst_flush(flush_time);
    assert!(flushed, "expected flush after stopping fast input");

    let expected_placeholder = format!("[Pasted Content {count} chars]");
    assert_eq!(composer.draft.textarea.text(), expected_placeholder);
    assert_eq!(composer.draft.pending_pastes.len(), 1);
    assert_eq!(composer.draft.pending_pastes[0].0, expected_placeholder);
    assert_eq!(composer.draft.pending_pastes[0].1.len(), count);
    assert!(composer.draft.pending_pastes[0].1.chars().all(|c| c == 'x'));
}

/// Behavior: human-like typing (with delays between chars) should not be classified as a paste
/// burst. Characters should appear immediately and should not trigger a paste placeholder.
#[test]
fn humanlike_typing_1000_chars_appears_live_no_placeholder() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let count = LARGE_PASTE_CHAR_THRESHOLD; // 1000 in current config
    let mut now = Instant::now();
    let step = ChatComposer::recommended_paste_flush_delay();
    for _ in 0..count {
        let _ = composer.handle_input_basic_with_time(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
            now,
        );
        now += step;
        let _ = composer.handle_paste_burst_flush(now);
    }

    assert_eq!(composer.draft.textarea.text(), "z".repeat(count));
    assert!(composer.draft.pending_pastes.is_empty());
}

#[test]
fn slash_popup_not_activated_for_slash_space_text_history_like_input() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use tokio::sync::mpsc::unbounded_channel;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Simulate history-like content: "/ test"
    composer.set_text_content("/ test".to_string(), Vec::new(), Vec::new());

    // After set_text_content -> sync_popups is called; popup should NOT be Command.
    assert!(
        matches!(composer.popups.active, ActivePopup::None),
        "expected no slash popup for '/ test'"
    );

    // Up should be handled by history navigation path, not slash popup handler.
    let (result, _redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(result, InputResult::None);
}

#[test]
fn slash_popup_activated_for_bare_slash_and_valid_prefixes() {
    // use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio::sync::mpsc::unbounded_channel;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    // Case 1: bare "/"
    composer.set_text_content("/".to_string(), Vec::new(), Vec::new());
    assert!(
        matches!(composer.popups.active, ActivePopup::Command(_)),
        "bare '/' should activate slash popup"
    );

    // Case 2: valid prefix "/re" (matches /review, /resume, etc.)
    composer.set_text_content("/re".to_string(), Vec::new(), Vec::new());
    assert!(
        matches!(composer.popups.active, ActivePopup::Command(_)),
        "'/re' should activate slash popup via prefix match"
    );

    // Case 3: fuzzy match "/ac" (subsequence of /compact and /feedback)
    composer.set_text_content("/ac".to_string(), Vec::new(), Vec::new());
    assert!(
        matches!(composer.popups.active, ActivePopup::Command(_)),
        "'/ac' should activate slash popup via fuzzy match"
    );

    // Case 4: invalid prefix "/zzz" – still allowed to open popup if it
    // matches no built-in command; our current logic will not open popup.
    // Verify that explicitly.
    composer.set_text_content("/zzz".to_string(), Vec::new(), Vec::new());
    assert!(
        matches!(composer.popups.active, ActivePopup::None),
        "'/zzz' should not activate slash popup because it is not a prefix of any built-in command"
    );
}

#[test]
fn bare_slash_command_can_be_recalled_after_recording_pending_history() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_text_content("/diff".to_string(), Vec::new(), Vec::new());
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(result, InputResult::Command(SlashCommand::Diff));
    composer.record_pending_slash_command_history();

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(result, InputResult::None);
    assert_eq!(composer.current_text(), "/diff");
}

#[test]
fn popup_selected_slash_command_records_canonical_command_history() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_text_content("/di".to_string(), Vec::new(), Vec::new());
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(result, InputResult::Command(SlashCommand::Diff));
    composer.record_pending_slash_command_history();

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(result, InputResult::None);
    assert_eq!(composer.current_text(), "/diff");
}

#[test]
fn inline_slash_command_can_be_recalled_after_recording_pending_history() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_collaboration_modes_enabled(/*enabled*/ true);

    composer.set_text_content("/plan investigate this".to_string(), Vec::new(), Vec::new());
    composer.popups.active = ActivePopup::None;
    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match result {
        InputResult::CommandWithArgs(cmd, args, text_elements) => {
            assert_eq!(cmd, SlashCommand::Plan);
            assert_eq!(args, "investigate this");
            assert!(text_elements.is_empty());
        }
        other => panic!("expected inline /plan command, got {other:?}"),
    }
    composer.record_pending_slash_command_history();

    let (result, _needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(result, InputResult::None);
    assert_eq!(composer.current_text(), "/plan investigate this");
}

#[test]
fn apply_external_edit_rebuilds_text_and_attachments() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let placeholder = local_image_label_text(/*label_number*/ 1);
    composer.draft.textarea.insert_element(&placeholder);
    composer
        .attachments
        .local_images
        .push(LocalImageAttachment {
            placeholder: placeholder.clone(),
            path: PathBuf::from("img.png"),
        });
    composer
        .draft
        .pending_pastes
        .push(("[Pasted]".to_string(), "data".to_string()));

    composer.apply_external_edit(format!("Edited {placeholder} text"));

    assert_eq!(
        composer.current_text(),
        format!("Edited {placeholder} text")
    );
    assert!(composer.draft.pending_pastes.is_empty());
    assert_eq!(composer.attachments.local_images.len(), 1);
    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        placeholder
    );
    assert_eq!(
        composer.draft.textarea.cursor(),
        composer.current_text().len()
    );
}

#[test]
fn apply_external_edit_absorbs_bash_prefix_without_duplication() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("!git status".to_string(), Vec::new(), Vec::new());

    composer.apply_external_edit("!git status".to_string());

    assert!(composer.draft.is_bash_mode);
    assert_eq!(composer.draft.textarea.text(), "git status");
    assert_eq!(composer.current_text(), "!git status");
}

#[test]
fn apply_external_edit_can_leave_bash_mode() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("!git status".to_string(), Vec::new(), Vec::new());

    composer.apply_external_edit("git status".to_string());

    assert!(!composer.draft.is_bash_mode);
    assert_eq!(composer.draft.textarea.text(), "git status");
    assert_eq!(composer.current_text(), "git status");
}

#[test]
fn apply_external_edit_can_enter_bash_mode() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_text_content("git status".to_string(), Vec::new(), Vec::new());

    composer.apply_external_edit("!git status".to_string());

    assert!(composer.draft.is_bash_mode);
    assert_eq!(composer.draft.textarea.text(), "git status");
    assert_eq!(composer.current_text(), "!git status");
}

#[test]
fn apply_external_edit_drops_missing_attachments() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let placeholder = local_image_label_text(/*label_number*/ 1);
    composer.draft.textarea.insert_element(&placeholder);
    composer
        .attachments
        .local_images
        .push(LocalImageAttachment {
            placeholder: placeholder.clone(),
            path: PathBuf::from("img.png"),
        });

    composer.apply_external_edit("No images here".to_string());

    assert_eq!(composer.current_text(), "No images here".to_string());
    assert!(composer.attachments.local_images.is_empty());
}

#[test]
fn apply_external_edit_renumbers_image_placeholders() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let first_path = PathBuf::from("img1.png");
    let second_path = PathBuf::from("img2.png");
    composer.attach_image(first_path);
    composer.attach_image(second_path.clone());

    let placeholder2 = local_image_label_text(/*label_number*/ 2);
    composer.apply_external_edit(format!("Keep {placeholder2}"));

    let placeholder1 = local_image_label_text(/*label_number*/ 1);
    assert_eq!(composer.current_text(), format!("Keep {placeholder1}"));
    assert_eq!(composer.attachments.local_images.len(), 1);
    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        placeholder1
    );
    assert_eq!(composer.local_image_paths(), vec![second_path]);
    assert_eq!(
        composer.draft.textarea.element_payloads(),
        vec![placeholder1]
    );
}

#[test]
fn current_text_with_pending_expands_placeholders() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let placeholder = "[Pasted Content 5 chars]".to_string();
    composer.draft.textarea.insert_element(&placeholder);
    composer
        .draft
        .pending_pastes
        .push((placeholder.clone(), "hello".to_string()));

    assert_eq!(
        composer.current_text_with_pending(),
        "hello".to_string(),
        "placeholder should expand to actual text"
    );
}

#[test]
fn current_text_with_pending_expands_overlapping_placeholders() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let first_paste = "a".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let second_paste = "b".repeat(LARGE_PASTE_CHAR_THRESHOLD + 4);
    let base = format!("[Pasted Content {} chars]", first_paste.chars().count());
    let second = format!("{base} #2");

    composer.handle_paste(first_paste.clone());
    composer.handle_paste(second_paste.clone());

    assert_eq!(composer.current_text(), format!("{base}{second}"));
    assert_eq!(
        composer.current_text_with_pending(),
        format!("{first_paste}{second_paste}")
    );
}

#[test]
fn apply_external_edit_limits_duplicates_to_occurrences() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let placeholder = local_image_label_text(/*label_number*/ 1);
    composer.draft.textarea.insert_element(&placeholder);
    composer
        .attachments
        .local_images
        .push(LocalImageAttachment {
            placeholder: placeholder.clone(),
            path: PathBuf::from("img.png"),
        });

    composer.apply_external_edit(format!("{placeholder} extra {placeholder}"));

    assert_eq!(
        composer.current_text(),
        format!("{placeholder} extra {placeholder}")
    );
    assert_eq!(composer.attachments.local_images.len(), 1);
}

#[test]
fn remote_images_do_not_modify_textarea_text_or_elements() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_remote_image_urls(vec![
        "https://example.com/one.png".to_string(),
        "https://example.com/two.png".to_string(),
    ]);

    assert_eq!(composer.current_text(), "");
    assert_eq!(composer.text_elements(), Vec::<TextElement>::new());
}

#[test]
fn attach_image_after_remote_prefix_uses_offset_label() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_remote_image_urls(vec![
        "https://example.com/one.png".to_string(),
        "https://example.com/two.png".to_string(),
    ]);
    composer.attach_image(PathBuf::from("/tmp/local.png"));

    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        "[Image #3]"
    );
    assert_eq!(composer.current_text(), "[Image #3]");
}

#[test]
fn prepare_submission_keeps_remote_offset_local_placeholder_numbering() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_remote_image_urls(vec!["https://example.com/one.png".to_string()]);
    let base_text = "[Image #2] hello".to_string();
    let base_elements = vec![TextElement::new(
        (0.."[Image #2]".len()).into(),
        Some("[Image #2]".to_string()),
    )];
    composer.set_text_content(
        base_text,
        base_elements,
        vec![PathBuf::from("/tmp/local.png")],
    );

    let (submitted_text, submitted_elements) = composer
        .prepare_submission_text(/*record_history*/ true)
        .expect("remote+local submission should be generated");
    assert_eq!(submitted_text, "[Image #2] hello");
    assert_eq!(
        submitted_elements,
        vec![TextElement::new(
            (0.."[Image #2]".len()).into(),
            Some("[Image #2]".to_string())
        )]
    );
}

#[test]
fn prepare_submission_with_only_remote_images_returns_empty_text() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_remote_image_urls(vec!["https://example.com/one.png".to_string()]);
    let (submitted_text, submitted_elements) = composer
        .prepare_submission_text(/*record_history*/ true)
        .expect("remote-only submission should be generated");
    assert_eq!(submitted_text, "");
    assert!(submitted_elements.is_empty());
}

#[test]
fn delete_selected_remote_image_relabels_local_placeholders() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_remote_image_urls(vec![
        "https://example.com/one.png".to_string(),
        "https://example.com/two.png".to_string(),
    ]);
    composer.attach_image(PathBuf::from("/tmp/local.png"));
    composer.draft.textarea.set_cursor(/*pos*/ 0);

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(
        composer.remote_image_urls(),
        vec!["https://example.com/one.png".to_string()]
    );
    assert_eq!(composer.current_text(), "[Image #2]");
    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        "[Image #2]"
    );

    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    let _ = composer.handle_key_event(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(composer.remote_image_urls(), Vec::<String>::new());
    assert_eq!(composer.current_text(), "[Image #1]");
    assert_eq!(
        composer.attachments.local_images[0].placeholder,
        "[Image #1]"
    );
}

#[test]
fn input_disabled_ignores_keypresses_and_hides_cursor() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_text_content("hello".to_string(), Vec::new(), Vec::new());
    composer.set_input_enabled(
        /*enabled*/ false,
        Some("Input disabled for test.".to_string()),
    );

    let (result, needs_redraw) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

    assert_eq!(result, InputResult::None);
    assert!(!needs_redraw);
    assert_eq!(composer.current_text(), "hello");

    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    assert_eq!(composer.cursor_pos(area), None);
}

#[test]
fn shutdown_in_progress_disables_input_and_uses_hint_without_footer() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    composer.set_text_content("hello".to_string(), Vec::new(), Vec::new());
    composer.show_shutdown_in_progress();

    assert!(!composer.input_enabled());
    assert_eq!(composer.current_text(), "hello");
    assert_eq!(composer.custom_footer_height(), Some(0));

    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    assert_eq!(composer.cursor_pos(area), None);

    let mut terminal = Terminal::new(TestBackend::new(40, 5)).expect("terminal");
    terminal
        .draw(|f| composer.render(f.area(), f.buffer_mut()))
        .unwrap();
    insta::assert_snapshot!("shutdown_in_progress", terminal.backend());
}
