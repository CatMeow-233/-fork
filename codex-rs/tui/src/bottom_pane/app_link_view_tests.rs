use super::*;
use crate::app::app_server_requests::ResolvedAppServerRequest;
use crate::app_event::AppEvent;
use crate::render::renderable::Renderable;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::unbounded_channel;

fn suggestion_target() -> AppLinkElicitationTarget {
    AppLinkElicitationTarget {
        thread_id: ThreadId::try_from("00000000-0000-0000-0000-000000000001")
            .expect("valid thread id"),
        server_name: "codex_apps".to_string(),
        request_id: AppServerRequestId::String("request-1".to_string()),
    }
}

fn generic_url_target() -> AppLinkElicitationTarget {
    AppLinkElicitationTarget {
        thread_id: ThreadId::try_from("00000000-0000-0000-0000-000000000002")
            .expect("valid thread id"),
        server_name: "payments".to_string(),
        request_id: AppServerRequestId::String("request-2".to_string()),
    }
}

fn auth_url_request(url: &str) -> codex_app_server_protocol::McpServerElicitationRequest {
    codex_app_server_protocol::McpServerElicitationRequest::Url {
        meta: Some(serde_json::json!({
            "_codex_apps": {
                "connector_auth_failure": {
                    "is_auth_failure": true,
                    "connector_id": "connector_calendar",
                    "connector_name": "Google Calendar",
                },
            },
        })),
        message: "Reconnect Google Calendar on ChatGPT.".to_string(),
        url: url.to_string(),
        elicitation_id: "codex_apps_auth_call_123".to_string(),
    }
}

#[test]
fn codex_apps_auth_url_elicitation_builds_auth_app_link_params() {
    let target = suggestion_target();
    let request = auth_url_request("https://chatgpt.com/apps/google-calendar/connector_calendar");

    let params = AppLinkViewParams::from_url_app_server_request(
        target.thread_id,
        &target.server_name,
        target.request_id.clone(),
        &request,
    )
    .expect("expected auth app link params");

    assert_eq!(params.app_id, "connector_calendar");
    assert_eq!(params.title, "Google Calendar");
    assert_eq!(
        params.url,
        "https://chatgpt.com/apps/google-calendar/connector_calendar"
    );
    assert_eq!(params.suggestion_type, Some(AppLinkSuggestionType::Auth));
    assert_eq!(params.elicitation_target, Some(target));
}

#[test]
fn non_codex_apps_url_elicitation_builds_generic_app_link_params() {
    let target = generic_url_target();
    let request = codex_app_server_protocol::McpServerElicitationRequest::Url {
        meta: None,
        message: "Review the payment details to continue.".to_string(),
        url: "https://payments.example/checkout/123".to_string(),
        elicitation_id: "payment-123".to_string(),
    };

    let params = AppLinkViewParams::from_url_app_server_request(
        target.thread_id,
        &target.server_name,
        target.request_id.clone(),
        &request,
    )
    .expect("expected generic URL app link params");

    assert_eq!(
        params,
        AppLinkViewParams {
            app_id: "payment-123".to_string(),
            title: "Action required".to_string(),
            description: Some("Server: payments".to_string()),
            instructions: "Complete the requested action in your browser, then return here."
                .to_string(),
            url: "https://payments.example/checkout/123".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: Some("Review the payment details to continue.".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::ExternalAction),
            elicitation_target: Some(target),
        }
    );
}

#[test]
fn codex_apps_auth_url_elicitation_rejects_untrusted_urls() {
    let target = suggestion_target();
    for url in [
        "http://chatgpt.com/apps/google-calendar/connector_calendar",
        "https://user:pass@chatgpt.com/apps/google-calendar/connector_calendar",
        "https://chatgpt.com.evil.example/apps/google-calendar/connector_calendar",
        "https://evilchatgpt.com/apps/google-calendar/connector_calendar",
    ] {
        let request = auth_url_request(url);
        let params = AppLinkViewParams::from_url_app_server_request(
            target.thread_id,
            &target.server_name,
            target.request_id.clone(),
            &request,
        );
        assert!(params.is_none(), "expected {url} to be rejected");
    }
}

#[test]
fn generic_url_elicitation_rejects_untrusted_urls() {
    let target = generic_url_target();
    for url in [
        "http://payments.example/checkout/123",
        "https://user:pass@payments.example/checkout/123",
    ] {
        let request = codex_app_server_protocol::McpServerElicitationRequest::Url {
            meta: None,
            message: "Review the payment details to continue.".to_string(),
            url: url.to_string(),
            elicitation_id: "payment-123".to_string(),
        };
        let params = AppLinkViewParams::from_url_app_server_request(
            target.thread_id,
            &target.server_name,
            target.request_id.clone(),
            &request,
        );
        assert!(params.is_none(), "expected {url} to be rejected");
    }
}

fn render_snapshot(view: &AppLinkView, area: Rect) -> String {
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| {
                    let symbol = buf[(x, y)].symbol();
                    if symbol.is_empty() {
                        ' '
                    } else {
                        crate::terminal_hyperlinks::strip_osc8(symbol)
                            .chars()
                            .next()
                            .unwrap_or(' ')
                    }
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn installed_app_has_toggle_action() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: "https://example.test/notion".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );

    assert_eq!(
        view.action_labels(),
        vec!["Manage on ChatGPT", "Disable app", "Back"]
    );
}

#[test]
fn regular_app_link_does_not_require_terminal_title_action() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: "https://example.test/notion".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );

    assert!(!view.terminal_title_requires_action());
}

#[test]
fn tool_suggestion_requires_terminal_title_action() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert!(view.terminal_title_requires_action());
}

#[test]
fn horizontal_list_keys_move_action_selection() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: "https://example.test/notion".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );

    assert_eq!(view.selected_action, 0);
    view.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(view.selected_action, 1);
    view.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(view.selected_action, 0);
}

#[test]
fn remapped_horizontal_list_keys_control_action_selection() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut list_keymap = crate::keymap::RuntimeKeymap::defaults().list;
    list_keymap.move_left = vec![key_hint::plain(KeyCode::Char('x'))];
    list_keymap.move_right = vec![key_hint::plain(KeyCode::Char('z'))];
    let mut view = AppLinkView::new_with_keymap(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: "https://example.test/notion".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
        list_keymap,
    );

    assert_eq!(view.selected_action, 0);
    view.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    assert_eq!(view.selected_action, 0);
    view.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(view.selected_action, 0);

    view.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert_eq!(view.selected_action, 1);
    view.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
    assert_eq!(view.selected_action, 1);
    view.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(view.selected_action, 1);

    view.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(view.selected_action, 0);
}

#[test]
fn toggle_action_sends_set_app_enabled_and_updates_label() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: "https://example.test/notion".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SetAppEnabled { id, enabled }) => {
            assert_eq!(id, "connector_1");
            assert!(!enabled);
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }

    assert_eq!(
        view.action_labels(),
        vec!["Manage on ChatGPT", "Enable app", "Back"]
    );
}

#[test]
fn generic_url_elicitation_resolves_without_connector_refresh() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let target = generic_url_target();
    let request = codex_app_server_protocol::McpServerElicitationRequest::Url {
        meta: None,
        message: "Review the payment details to continue.".to_string(),
        url: "https://payments.example/checkout/123".to_string(),
        elicitation_id: "payment-123".to_string(),
    };
    let params = AppLinkViewParams::from_url_app_server_request(
        target.thread_id,
        &target.server_name,
        target.request_id.clone(),
        &request,
    )
    .expect("expected generic URL app link params");
    let mut view = AppLinkView::new(params, tx);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match rx.try_recv() {
        Ok(AppEvent::OpenUrlInBrowser { url }) => {
            assert_eq!(url, "https://payments.example/checkout/123");
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert_eq!(view.screen, AppLinkScreen::InstallConfirmation);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match rx.try_recv() {
        Ok(AppEvent::SubmitThreadOp { thread_id, op }) => {
            assert_eq!(thread_id, target.thread_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "payments".to_string(),
                    request_id: AppServerRequestId::String("request-2".to_string()),
                    decision: McpServerElicitationAction::Accept,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(rx.try_recv().is_err());
    assert!(view.is_complete());
}

#[test]
fn install_confirmation_does_not_split_long_url_like_token_without_scheme() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: url_like.to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );
    view.screen = AppLinkScreen::InstallConfirmation;

    let rendered: Vec<String> = view
        .content_lines(/*width*/ 40)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect();

    assert_eq!(
        rendered
            .iter()
            .filter(|line| line.contains(url_like))
            .count(),
        1,
        "expected full URL-like token in one rendered line, got: {rendered:?}"
    );
}

#[test]
fn install_confirmation_render_keeps_url_tail_visible_when_narrow() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let url = "https://example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890/artifacts/reports/performance/summary/detail/with/a/very/long/path/tail42";
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_1".to_string(),
            title: "Notion".to_string(),
            description: None,
            instructions: "Manage app".to_string(),
            url: url.to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: None,
            suggestion_type: None,
            elicitation_target: None,
        },
        tx,
    );
    view.screen = AppLinkScreen::InstallConfirmation;

    let width: u16 = 36;
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    let rendered_blob = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| {
                    let symbol = buf[(x, y)].symbol();
                    if symbol.is_empty() {
                        ' '
                    } else {
                        crate::terminal_hyperlinks::strip_osc8(symbol)
                            .chars()
                            .next()
                            .unwrap_or(' ')
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        rendered_blob.contains("tail42"),
        "expected wrapped setup URL tail to remain visible in narrow pane, got:\n{rendered_blob}"
    );
}

#[test]
fn install_tool_suggestion_resolves_elicitation_after_confirmation() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Install this app in your browser, then return here.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: false,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Install),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match rx.try_recv() {
        Ok(AppEvent::OpenUrlInBrowser { url }) => {
            assert_eq!(url, "https://example.test/google-calendar".to_string());
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert_eq!(view.screen, AppLinkScreen::InstallConfirmation);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match rx.try_recv() {
        Ok(AppEvent::RefreshConnectors { force_refetch }) => {
            assert!(force_refetch);
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::SubmitThreadOp { thread_id, op }) => {
            assert_eq!(thread_id, suggestion_target().thread_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "codex_apps".to_string(),
                    request_id: AppServerRequestId::String("request-1".to_string()),
                    decision: McpServerElicitationAction::Accept,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(view.is_complete());
}

#[test]
fn declined_tool_suggestion_resolves_elicitation_decline() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: None,
            instructions: "Install this app in your browser, then return here.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: false,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Install),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SubmitThreadOp { thread_id, op }) => {
            assert_eq!(thread_id, suggestion_target().thread_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "codex_apps".to_string(),
                    request_id: AppServerRequestId::String("request-1".to_string()),
                    decision: McpServerElicitationAction::Decline,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(view.is_complete());
}

#[test]
fn enable_tool_suggestion_resolves_elicitation_after_enable() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SetAppEnabled { id, enabled }) => {
            assert_eq!(id, "connector_google_calendar");
            assert!(enabled);
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::SubmitThreadOp { thread_id, op }) => {
            assert_eq!(thread_id, suggestion_target().thread_id);
            assert_eq!(
                op,
                Op::ResolveElicitation {
                    server_name: "codex_apps".to_string(),
                    request_id: AppServerRequestId::String("request-1".to_string()),
                    decision: McpServerElicitationAction::Accept,
                    content: None,
                    meta: None,
                }
            );
        }
        Ok(other) => panic!("unexpected app event: {other:?}"),
        Err(err) => panic!("missing app event: {err}"),
    }
    assert!(view.is_complete());
}

#[test]
fn resolved_tool_suggestion_dismisses_matching_view() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert!(
        view.dismiss_app_server_request(&ResolvedAppServerRequest::McpElicitation {
            server_name: "codex_apps".to_string(),
            request_id: AppServerRequestId::String("request-1".to_string()),
        })
    );
    assert!(view.is_complete());
}

#[test]
fn resolved_tool_suggestion_ignores_non_matching_request() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert!(
        !view.dismiss_app_server_request(&ResolvedAppServerRequest::McpElicitation {
            server_name: "other_server".to_string(),
            request_id: AppServerRequestId::String("request-1".to_string()),
        })
    );
    assert!(!view.is_complete());
}

#[test]
fn install_suggestion_with_reason_snapshot() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Install this app in your browser, then return here.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: false,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Install),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert_snapshot!(
        "app_link_view_install_suggestion_with_reason",
        render_snapshot(
            &view,
            Rect::new(0, 0, 72, view.desired_height(/*width*/ 72))
        )
    );
}

#[test]
fn enable_suggestion_with_reason_snapshot() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: Some("Plan events and schedules.".to_string()),
            instructions: "Enable this app to use it for the current request.".to_string(),
            url: "https://example.test/google-calendar".to_string(),
            is_installed: true,
            is_enabled: false,
            suggest_reason: Some("Plan and reference events from your calendar".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Enable),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert_snapshot!(
        "app_link_view_enable_suggestion_with_reason",
        render_snapshot(
            &view,
            Rect::new(0, 0, 72, view.desired_height(/*width*/ 72))
        )
    );
}

#[test]
fn auth_suggestion_with_reason_snapshot() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = AppLinkView::new(
        AppLinkViewParams {
            app_id: "connector_google_calendar".to_string(),
            title: "Google Calendar".to_string(),
            description: None,
            instructions: "Sign in to this app in your browser, then return here.".to_string(),
            url: "https://chatgpt.com/apps/google-calendar/connector_google_calendar".to_string(),
            is_installed: true,
            is_enabled: true,
            suggest_reason: Some("Reconnect Google Calendar on ChatGPT.".to_string()),
            suggestion_type: Some(AppLinkSuggestionType::Auth),
            elicitation_target: Some(suggestion_target()),
        },
        tx,
    );

    assert_snapshot!(
        "app_link_view_auth_suggestion_with_reason",
        render_snapshot(
            &view,
            Rect::new(0, 0, 72, view.desired_height(/*width*/ 72))
        )
    );
}

#[test]
fn generic_url_elicitation_snapshot() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let target = generic_url_target();
    let request = codex_app_server_protocol::McpServerElicitationRequest::Url {
        meta: None,
        message: "Review the payment details to continue.".to_string(),
        url: "https://payments.example/checkout/123".to_string(),
        elicitation_id: "payment-123".to_string(),
    };
    let params = AppLinkViewParams::from_url_app_server_request(
        target.thread_id,
        &target.server_name,
        target.request_id.clone(),
        &request,
    )
    .expect("expected generic URL app link params");
    let view = AppLinkView::new(params, tx);

    assert_snapshot!(
        "app_link_view_generic_url_elicitation",
        render_snapshot(
            &view,
            Rect::new(0, 0, 72, view.desired_height(/*width*/ 72))
        )
    );
}

#[test]
fn generic_url_elicitation_confirmation_snapshot() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let target = generic_url_target();
    let request = codex_app_server_protocol::McpServerElicitationRequest::Url {
        meta: None,
        message: "Review the payment details to continue.".to_string(),
        url: "https://payments.example/checkout/123".to_string(),
        elicitation_id: "payment-123".to_string(),
    };
    let params = AppLinkViewParams::from_url_app_server_request(
        target.thread_id,
        &target.server_name,
        target.request_id.clone(),
        &request,
    )
    .expect("expected generic URL app link params");
    let mut view = AppLinkView::new(params, tx);

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_snapshot!(
        "app_link_view_generic_url_elicitation_confirmation",
        render_snapshot(
            &view,
            Rect::new(0, 0, 72, view.desired_height(/*width*/ 72))
        )
    );
}
