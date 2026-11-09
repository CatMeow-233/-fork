use super::*;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::render::renderable::Renderable;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crate::test_support::test_path_display;
use codex_app_server_protocol::HookErrorInfo;
use codex_app_server_protocol::HookEventName;
use codex_app_server_protocol::HookHandlerMetadata;
use codex_app_server_protocol::HookMetadata;
use codex_app_server_protocol::HookSource;
use codex_app_server_protocol::HookTrustStatus;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use tokio::sync::mpsc::unbounded_channel;

pub(super) fn render_lines(view: &HooksBrowserView, width: u16) -> String {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    (0..area.height)
        .map(|row| {
            let rendered = (0..area.width)
                .map(|col| {
                    let symbol = buf[(area.x + col, area.y + row)].symbol();
                    if symbol.is_empty() {
                        " ".to_string()
                    } else {
                        crate::terminal_hyperlinks::strip_osc8(symbol)
                    }
                })
                .collect::<String>();
            let normalized = rendered
                .replace(&test_path_display("/tmp/hooks.json"), "/tmp/hooks.json")
                .replace(&test_path_display("/tmp/h.json"), "/tmp/h.json");
            format!("{normalized:width$}", width = area.width as usize)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn render_buffer(view: &HooksBrowserView, width: u16) -> Buffer {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    buf
}

#[allow(clippy::too_many_arguments)]
pub(super) fn hook(
    key: &str,
    event_name: HookEventName,
    source: HookSource,
    plugin_id: Option<&str>,
    command: &str,
    enabled: bool,
    is_managed: bool,
    display_order: i64,
) -> HookMetadata {
    let current_hash = "sha256:current".to_string();
    HookMetadata {
        key: key.to_string(),
        event_name,
        handler: HookHandlerMetadata::Command {
            command: command.to_string(),
            r#async: false,
        },
        is_managed,
        matcher: Some("Bash".to_string()),
        timeout_sec: 30,
        status_message: None,
        additional_context_limit: None,
        source_path: test_path_buf("/tmp/hooks.json").abs(),
        source,
        plugin_id: plugin_id.map(str::to_string),
        environment_id: None,
        display_order,
        enabled,
        current_hash,
        trust_status: if is_managed {
            HookTrustStatus::Managed
        } else {
            HookTrustStatus::Trusted
        },
    }
}

fn view() -> HooksBrowserView {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    HooksBrowserView::new(
        vec![
            hook(
                "plugin:superpowers",
                HookEventName::PreToolUse,
                HookSource::Plugin,
                Some("superpowers@openai-curated"),
                "${CODEX_PLUGIN_ROOT}/hooks/pre-tool-use-check.sh",
                /*enabled*/ true,
                /*is_managed*/ false,
                /*display_order*/ 0,
            ),
            hook(
                "path:user-config",
                HookEventName::PreToolUse,
                HookSource::User,
                /*plugin_id*/ None,
                "~/bin/check-shell-with-a-command-that-is-way-too-long-for-the-summary-column.sh",
                /*enabled*/ false,
                /*is_managed*/ false,
                /*display_order*/ 1,
            ),
            hook(
                "path:managed",
                HookEventName::PermissionRequest,
                HookSource::System,
                /*plugin_id*/ None,
                "/enterprise/hooks/permission-check.sh",
                /*enabled*/ true,
                /*is_managed*/ true,
                /*display_order*/ 2,
            ),
        ],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    )
}

#[test]
fn renders_event_browser() {
    let mut view = view();
    assert_snapshot!("hooks_browser_events", render_lines(&view, /*width*/ 112));
    view.state.selected_idx = Some(view.event_rows().len() - 1);
    assert_snapshot!(
        "hooks_browser_model_requests",
        render_lines(&view, /*width*/ 112),
    );
    view.keymap.accept.clear();
    assert!(!render_lines(&view, /*width*/ 112).contains("enter details"));
}

#[test]
fn compact_hooks_keep_the_last_event_and_selection_visible() {
    let mut view = view();
    view.state.selected_idx = Some(view.event_rows().len() - 1);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 16,
    );
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    let text = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(text);
    let selected_y = text
        .lines()
        .position(|line| line.starts_with("› "))
        .unwrap() as u16;
    assert_eq!(buf[(0, selected_y)].bg, selection_style().bg.unwrap());
    assert_eq!(buf[(39, selected_y)].bg, selection_style().bg.unwrap());
    assert!(text.contains("Interrupt"));
    assert!(text.contains("Active/Installed"));
}

#[test]
fn selected_event_rows_use_the_shared_selection_style() {
    let view = view();
    let buf = render_buffer(&view, /*width*/ 112);
    let expected = selection_style();

    let selected_cell = buf
        .content
        .iter()
        .find(|cell| {
            let style = cell.style();
            cell.symbol() == "P"
                && style.fg == expected.fg
                && style.add_modifier.contains(Modifier::BOLD)
        })
        .expect("selected event row should use the shared accent style");

    assert_eq!(selected_cell.style().fg, expected.fg);
}

#[test]
fn renders_event_browser_with_review_column_when_needed() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let view = HooksBrowserView::new(
        vec![untrusted_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    assert_snapshot!(
        "hooks_browser_events_with_review_column",
        render_lines(&view, /*width*/ 112)
    );
    assert_eq!(
        view.event_table_lines()[1].spans[3].style.fg,
        selection_style().fg
    );
    assert!(
        view.event_table_lines()[1].spans[3]
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
}

#[test]
fn renders_event_browser_with_issues() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let view = HooksBrowserView::new(
        Vec::new(),
        vec!["skipped invalid matcher for PreToolUse".to_string()],
        vec![HookErrorInfo {
            path: test_path_buf("/tmp/hooks.json"),
            message: "failed to parse hooks config".to_string(),
        }],
        AppEventSender::new(tx_raw),
    );

    assert_snapshot!(
        "hooks_browser_events_with_issues",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn renders_handler_browser_with_details() {
    let mut view = view();
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_snapshot!("hooks_browser_handlers", render_lines(&view, /*width*/ 112));
}

#[test]
fn renders_handler_status_messages_with_blank_fallback() {
    let mut view = view();
    view.entry.hooks[0].status_message = Some("  检查 🦀 shell commands  ".to_string());
    view.entry.hooks[1].status_message = Some(" \t\n ".to_string());
    view.entry.hooks[2].event_name = HookEventName::PreToolUse;
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_snapshot!(
        "hooks_browser_status_messages",
        render_lines(&view, /*width*/ 112)
    );

    for (fg, bg) in [
        ((230, 230, 230), (18, 20, 30)),
        ((32, 32, 32), (255, 255, 255)),
    ] {
        crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors { fg, bg },
            || {
                let buf = render_buffer(&view, /*width*/ 112);
                let selected_y = (0..buf.area.height)
                    .find(|&y| buf[(0, y)].symbol() == "›")
                    .expect("selected hook row");
                for y in [selected_y, selected_y + 1] {
                    assert!(buf[(7, y)].modifier.contains(Modifier::DIM));
                    assert!(!buf[(10, y)].modifier.contains(Modifier::DIM));
                }
                assert_eq!(buf[(10, selected_y)].symbol(), "检");
                assert_eq!(buf[(10, selected_y)].fg, selection_style().fg.unwrap());
                assert_eq!(
                    (0..buf.area.width)
                        .map(|x| buf[(x, selected_y)].bg)
                        .collect::<Vec<_>>(),
                    vec![selection_style().bg.unwrap(); usize::from(buf.area.width)]
                );
            },
        );
    }
}

#[test]
fn renders_mcp_tool_handler_with_server_and_tool_details() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut configured_hook = hook(
        "plugin:security-scanner",
        HookEventName::PreToolUse,
        HookSource::Plugin,
        Some("security-tools@openai-curated"),
        "",
        /*enabled*/ true,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    configured_hook.handler = HookHandlerMetadata::McpTool {
        server: "scanner".to_string(),
        tool: "scan_file".to_string(),
    };
    let mut view = HooksBrowserView::new(
        vec![configured_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_snapshot!(
        "hooks_browser_mcp_tool_handler",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn renders_handler_additional_context_limit() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut configured_hook = hook(
        "path:context-limit",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use.sh",
        /*enabled*/ true,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    configured_hook.additional_context_limit = Some(0);
    let mut view = HooksBrowserView::new(
        vec![configured_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_snapshot!(
        "hooks_browser_additional_context_limit",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn renders_untrusted_enabled_handler_as_inactive() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "~/bin/untrusted.sh",
        /*enabled*/ true,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let HookHandlerMetadata::Command { r#async, .. } = &mut untrusted_hook.handler else {
        panic!("expected command hook");
    };
    *r#async = true;
    let mut view = HooksBrowserView::new(
        vec![untrusted_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_snapshot!(
        "hooks_browser_untrusted_enabled_handler",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn review_needed_handler_rows_use_warning_color() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "~/bin/untrusted.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 1,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let mut view = HooksBrowserView::new(
        vec![
            hook(
                "path:trusted",
                HookEventName::PreToolUse,
                HookSource::User,
                /*plugin_id*/ None,
                "~/bin/trusted.sh",
                /*enabled*/ true,
                /*is_managed*/ false,
                /*display_order*/ 0,
            ),
            untrusted_hook,
        ],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(
        view.handler_row_lines(HookEventName::PreToolUse, /*width*/ 112)[1]
            .style
            .fg,
        Some(Color::Yellow)
    );
}

#[test]
fn review_needed_handler_header_uses_warning_color() {
    assert_eq!(
        HooksBrowserView::handler_header_lines(
            HookEventName::PreToolUse,
            /*review_needed_count*/ 1,
        )[1]
        .spans[0]
            .style
            .fg,
        Some(Color::Yellow)
    );
}

#[test]
fn renders_managed_handler_without_toggle_hint() {
    let mut view = view();
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_snapshot!(
        "hooks_browser_managed_handler",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn renders_selected_managed_handler() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut view = HooksBrowserView::new(
        vec![
            hook(
                "path:managed-1",
                HookEventName::PreToolUse,
                HookSource::System,
                /*plugin_id*/ None,
                "/enterprise/hooks/pre-tool-use-1.sh",
                /*enabled*/ true,
                /*is_managed*/ true,
                /*display_order*/ 0,
            ),
            hook(
                "path:managed-2",
                HookEventName::PreToolUse,
                HookSource::System,
                /*plugin_id*/ None,
                "/enterprise/hooks/pre-tool-use-2.sh",
                /*enabled*/ true,
                /*is_managed*/ true,
                /*display_order*/ 1,
            ),
        ],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    assert_snapshot!(
        "hooks_browser_selected_managed_handler",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn renders_scrolled_handler_window() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let hooks = (0..MAX_POPUP_ROWS + 3)
        .map(|idx| {
            let mut hook = hook(
                &format!("path:hook-{idx}"),
                HookEventName::PreToolUse,
                HookSource::User,
                /*plugin_id*/ None,
                &format!("/tmp/hook-{idx}.sh"),
                /*enabled*/ true,
                /*is_managed*/ false,
                idx as i64,
            );
            if idx % 2 == 0 {
                hook.status_message = Some(format!("Check tool {}", idx + 1));
            }
            hook
        })
        .collect();
    let mut view =
        HooksBrowserView::new(hooks, Vec::new(), Vec::new(), AppEventSender::new(tx_raw));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    for _ in 0..MAX_POPUP_ROWS + 1 {
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    assert_snapshot!(
        "hooks_browser_scrolled_handlers",
        render_lines(&view, /*width*/ 112)
    );
    assert_eq!(
        view.selected_hook(HookEventName::PreToolUse)
            .map(|hook| hook.key.as_str()),
        Some("path:hook-9")
    );
}

#[test]
fn renders_command_details_with_three_line_cap() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut capped_command_hook = hook(
        "path:long-command",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty",
        /*enabled*/ true,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    capped_command_hook.source_path = test_path_buf("/tmp/h.json").abs();
    capped_command_hook.status_message =
        Some("Check shell commands before allowing them to execute".to_string());
    let mut view = HooksBrowserView::new(
        vec![capped_command_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_snapshot!(
        "hooks_browser_capped_command_details",
        render_lines(&view, /*width*/ 44)
    );
}

#[test]
fn renders_empty_handler_browser_message() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut view = HooksBrowserView::new(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_snapshot!(
        "hooks_browser_empty_handlers",
        render_lines(&view, /*width*/ 112)
    );
}

#[test]
fn managed_hooks_count_as_active() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let view = HooksBrowserView::new(
        vec![hook(
            "path:managed",
            HookEventName::PreToolUse,
            HookSource::System,
            /*plugin_id*/ None,
            "/enterprise/hooks/pre-tool-use-check.sh",
            /*enabled*/ true,
            /*is_managed*/ true,
            /*display_order*/ 0,
        )],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    let rows = view.event_rows();
    let pre_tool_use = rows
        .into_iter()
        .find(|row| row.event_name == HookEventName::PreToolUse)
        .expect("pre tool use row");

    assert_eq!(pre_tool_use.installed, 1);
    assert_eq!(pre_tool_use.active, 1);
}

#[test]
fn review_needed_hooks_are_not_active() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use-check.sh",
        /*enabled*/ true,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let view = HooksBrowserView::new(
        vec![untrusted_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    let rows = view.event_rows();
    let pre_tool_use = rows
        .into_iter()
        .find(|row| row.event_name == HookEventName::PreToolUse)
        .expect("pre tool use row");

    assert_eq!(pre_tool_use.installed, 1);
    assert_eq!(pre_tool_use.active, 0);
    assert_eq!(pre_tool_use.needs_review, 1);
}

#[test]
fn review_needed_event_is_selected_by_default() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PermissionRequest,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/permission-request-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let view = HooksBrowserView::new(
        vec![untrusted_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    assert_eq!(
        view.selected_event(),
        Some(HookEventName::PermissionRequest)
    );
}

#[test]
fn renders_review_needed_handler() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let mut view = HooksBrowserView::new(
        vec![untrusted_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_snapshot!(
        "hooks_browser_review_needed_handler",
        render_lines(&view, /*width*/ 112)
    );
}

fn assert_unmanaged_toggle_key(key_code: KeyCode) {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let mut view = HooksBrowserView::new(
        vec![hook(
            "plugin:superpowers",
            HookEventName::PreToolUse,
            HookSource::Plugin,
            Some("superpowers@openai-curated"),
            "hooks/pre-tool-use-check.sh",
            /*enabled*/ true,
            /*is_managed*/ false,
            /*display_order*/ 0,
        )],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(key_code));

    match rx.try_recv().expect("toggle event") {
        AppEvent::SetHookEnabled { key, enabled } => {
            assert_eq!(key, "plugin:superpowers");
            assert!(!enabled);
        }
        other => panic!("expected hook toggle event, got {other:?}"),
    }
}

#[test]
fn toggle_keys_toggle_unmanaged_handler() {
    for key_code in [KeyCode::Char(' '), KeyCode::Enter] {
        assert_unmanaged_toggle_key(key_code);
    }
}

#[test]
fn space_does_not_toggle_managed_handler() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let mut view = HooksBrowserView::new(
        vec![hook(
            "path:managed",
            HookEventName::PreToolUse,
            HookSource::System,
            /*plugin_id*/ None,
            "/enterprise/hooks/pre-tool-use-check.sh",
            /*enabled*/ true,
            /*is_managed*/ true,
            /*display_order*/ 0,
        )],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));

    assert!(rx.try_recv().is_err());
}

#[test]
fn trust_key_trusts_review_needed_handler_without_changing_enablement() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let current_hash = untrusted_hook.current_hash.clone();
    let mut view = HooksBrowserView::new(
        vec![untrusted_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Char('t')));

    match rx.try_recv().expect("trust event") {
        AppEvent::TrustHook {
            key,
            current_hash: hash_to_trust,
        } => {
            assert_eq!(key, "path:untrusted");
            assert_eq!(hash_to_trust, current_hash);
        }
        other => panic!("expected hook trust event, got {other:?}"),
    }
}

#[test]
fn trust_key_preserves_disabled_modified_handler() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let mut modified_hook = hook(
        "path:modified",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    modified_hook.trust_status = HookTrustStatus::Modified;
    let current_hash = modified_hook.current_hash.clone();
    let mut view = HooksBrowserView::new(
        vec![modified_hook],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Char('t')));

    let hook = view.entry.hooks.first().expect("trusted hook");
    assert!(!hook.enabled);
    assert_eq!(hook.trust_status, HookTrustStatus::Trusted);
    match rx.try_recv().expect("trust event") {
        AppEvent::TrustHook {
            key,
            current_hash: hash_to_trust,
        } => {
            assert_eq!(key, "path:modified");
            assert_eq!(hash_to_trust, current_hash);
        }
        other => panic!("expected hook trust event, got {other:?}"),
    }
}

#[test]
fn trust_key_on_event_page_trusts_all_review_needed_hooks() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let mut untrusted_hook = hook(
        "path:untrusted",
        HookEventName::PreToolUse,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/pre-tool-use-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 0,
    );
    untrusted_hook.trust_status = HookTrustStatus::Untrusted;
    let mut modified_hook = hook(
        "path:modified",
        HookEventName::Stop,
        HookSource::User,
        /*plugin_id*/ None,
        "/tmp/stop-check.sh",
        /*enabled*/ false,
        /*is_managed*/ false,
        /*display_order*/ 1,
    );
    modified_hook.trust_status = HookTrustStatus::Modified;
    let mut view = HooksBrowserView::new(
        vec![
            untrusted_hook,
            modified_hook,
            hook(
                "path:trusted",
                HookEventName::PreToolUse,
                HookSource::User,
                /*plugin_id*/ None,
                "/tmp/trusted.sh",
                /*enabled*/ true,
                /*is_managed*/ false,
                /*display_order*/ 2,
            ),
        ],
        Vec::new(),
        Vec::new(),
        AppEventSender::new(tx_raw),
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Char('t')));

    assert_eq!(
        view.entry
            .hooks
            .iter()
            .map(|hook| hook.trust_status)
            .collect::<Vec<_>>(),
        vec![
            HookTrustStatus::Trusted,
            HookTrustStatus::Trusted,
            HookTrustStatus::Trusted,
        ]
    );
    match rx.try_recv().expect("trust event") {
        AppEvent::TrustHooks { updates } => assert_eq!(
            updates,
            vec![
                HookTrustUpdate {
                    key: "path:untrusted".to_string(),
                    current_hash: "sha256:current".to_string(),
                },
                HookTrustUpdate {
                    key: "path:modified".to_string(),
                    current_hash: "sha256:current".to_string(),
                },
            ]
        ),
        other => panic!("expected hook trust event, got {other:?}"),
    }
    assert!(rx.try_recv().is_err());
}

#[test]
fn escape_returns_to_the_selected_event() {
    let mut view = view();
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    view.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_eq!(view.page, HooksBrowserPage::Events);
    assert_eq!(
        view.selected_event(),
        Some(HookEventName::PermissionRequest)
    );
}

#[test]
fn esc_routes_through_the_view() {
    assert!(view().prefer_esc_to_handle_key_event());
}
