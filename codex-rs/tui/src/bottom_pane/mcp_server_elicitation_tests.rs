use super::*;
use crate::app_event::AppEvent;
use crate::render::renderable::Renderable;
use codex_config::types::KeybindingSpec;
use codex_config::types::KeybindingsSpec;
use codex_config::types::TuiKeymap;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

fn test_sender() -> (AppEventSender, UnboundedReceiver<AppEvent>) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    (AppEventSender::new(tx_raw), rx)
}

fn form_request(
    message: &str,
    requested_schema: Value,
    meta: Option<Value>,
) -> McpServerElicitationRequestParams {
    McpServerElicitationRequestParams {
        thread_id: "thread-1".to_string(),
        turn_id: Some("turn-1".to_string()),
        server_name: "server-1".to_string(),
        request: McpServerElicitationRequest::Form {
            meta,
            message: message.to_string(),
            requested_schema: serde_json::from_value(requested_schema)
                .expect("test schema should deserialize"),
        },
    }
}

fn request_id(value: &str) -> AppServerRequestId {
    AppServerRequestId::String(value.to_string())
}

fn from_form_request(
    thread_id: ThreadId,
    request: McpServerElicitationRequestParams,
) -> Option<McpServerElicitationFormRequest> {
    McpServerElicitationFormRequest::from_app_server_request(
        thread_id,
        request_id("request-1"),
        &request,
    )
}

fn empty_object_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {},
    })
}

fn tool_approval_meta(
    persist_modes: &[&str],
    tool_params: Option<Value>,
    tool_params_display: Option<Vec<(&str, Value, &str)>>,
) -> Option<Value> {
    let mut meta = serde_json::Map::from_iter([(
        APPROVAL_META_KIND_KEY.to_string(),
        Value::String(APPROVAL_META_KIND_MCP_TOOL_CALL.to_string()),
    )]);
    if !persist_modes.is_empty() {
        meta.insert(
            APPROVAL_PERSIST_KEY.to_string(),
            Value::Array(
                persist_modes
                    .iter()
                    .map(|mode| Value::String((*mode).to_string()))
                    .collect(),
            ),
        );
    }
    if let Some(tool_params) = tool_params {
        meta.insert(APPROVAL_TOOL_PARAMS_KEY.to_string(), tool_params);
    }
    if let Some(tool_params_display) = tool_params_display {
        meta.insert(
            APPROVAL_TOOL_PARAMS_DISPLAY_KEY.to_string(),
            Value::Array(
                tool_params_display
                    .into_iter()
                    .map(|(name, value, display_name)| {
                        serde_json::json!({
                            "name": name,
                            "value": value,
                            "display_name": display_name,
                        })
                    })
                    .collect(),
            ),
        );
    }
    Some(Value::Object(meta))
}

fn snapshot_buffer(buf: &Buffer) -> String {
    let mut lines = Vec::new();
    for y in 0..buf.area().height {
        let mut row = String::new();
        for x in 0..buf.area().width {
            row.push(
                crate::terminal_hyperlinks::strip_osc8(buf[(x, y)].symbol())
                    .chars()
                    .next()
                    .unwrap_or(' '),
            );
        }
        lines.push(row);
    }
    lines.join("\n")
}

fn render_snapshot(overlay: &McpServerElicitationOverlay, area: Rect) -> String {
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    snapshot_buffer(&buf)
}

#[test]
fn parses_boolean_form_request() {
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                        "description": "Approve the pending action.",
                    }
                },
                "required": ["confirmed"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");

    assert_eq!(
        request,
        McpServerElicitationFormRequest {
            thread_id,
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            message: "Allow this request?".to_string(),
            approval_display_params: Vec::new(),
            response_mode: McpServerElicitationResponseMode::FormContent,
            is_tool_approval: false,
            fields: vec![McpServerElicitationField {
                id: "confirmed".to_string(),
                label: "Confirm".to_string(),
                prompt: "Approve the pending action.".to_string(),
                required: true,
                input: McpServerElicitationFieldInput::Select {
                    options: vec![
                        McpServerElicitationOption {
                            label: "True".to_string(),
                            description: None,
                            value: Value::Bool(true),
                        },
                        McpServerElicitationOption {
                            label: "False".to_string(),
                            description: None,
                            value: Value::Bool(false),
                        },
                    ],
                    default_idx: None,
                },
            }],
            tool_suggestion: None,
        }
    );
}

#[test]
fn unsupported_numeric_form_falls_back() {
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Pick a number",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "count": {
                        "type": "integer",
                        "title": "Count",
                    }
                },
            }),
            /*meta*/ None,
        ),
    );

    assert_eq!(request, None);
}

#[test]
fn empty_object_schema_uses_approval_actions() {
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            empty_object_schema(),
            /*meta*/ None,
        ),
    )
    .expect("expected approval fallback");

    assert_eq!(
        request,
        McpServerElicitationFormRequest {
            thread_id,
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            message: "Allow this request?".to_string(),
            approval_display_params: Vec::new(),
            response_mode: McpServerElicitationResponseMode::ApprovalAction,
            is_tool_approval: false,
            fields: vec![McpServerElicitationField {
                id: APPROVAL_FIELD_ID.to_string(),
                label: String::new(),
                prompt: String::new(),
                required: true,
                input: McpServerElicitationFieldInput::Select {
                    options: vec![
                        McpServerElicitationOption {
                            label: "Allow".to_string(),
                            description: Some("Allow this request and continue".to_string()),
                            value: Value::String(APPROVAL_ACCEPT_ONCE_VALUE.to_string()),
                        },
                        McpServerElicitationOption {
                            label: "Deny".to_string(),
                            description: Some("Decline this request and continue".to_string()),
                            value: Value::String(APPROVAL_DECLINE_VALUE.to_string()),
                        },
                        McpServerElicitationOption {
                            label: "Cancel".to_string(),
                            description: Some("Cancel this request".to_string()),
                            value: Value::String(APPROVAL_CANCEL_VALUE.to_string()),
                        },
                    ],
                    default_idx: Some(0),
                },
            }],
            tool_suggestion: None,
        }
    );
}

#[test]
fn empty_tool_approval_schema_uses_approval_actions() {
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            empty_object_schema(),
            tool_approval_meta(
                &[],
                /*tool_params*/ None,
                /*tool_params_display*/ None,
            ),
        ),
    )
    .expect("expected approval fallback");

    assert_eq!(
        request,
        McpServerElicitationFormRequest {
            thread_id,
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            message: "Allow this request?".to_string(),
            approval_display_params: Vec::new(),
            response_mode: McpServerElicitationResponseMode::ApprovalAction,
            is_tool_approval: true,
            fields: vec![McpServerElicitationField {
                id: APPROVAL_FIELD_ID.to_string(),
                label: String::new(),
                prompt: String::new(),
                required: true,
                input: McpServerElicitationFieldInput::Select {
                    options: vec![
                        McpServerElicitationOption {
                            label: "Allow".to_string(),
                            description: Some("Run the tool and continue".to_string()),
                            value: Value::String(APPROVAL_ACCEPT_ONCE_VALUE.to_string()),
                        },
                        McpServerElicitationOption {
                            label: "Cancel".to_string(),
                            description: Some("Cancel this tool call".to_string()),
                            value: Value::String(APPROVAL_CANCEL_VALUE.to_string()),
                        },
                    ],
                    default_idx: Some(0),
                },
            }],
            tool_suggestion: None,
        }
    );
}

#[test]
fn tool_suggestion_meta_is_parsed_into_request_payload() {
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Suggest Google Calendar",
            empty_object_schema(),
            Some(serde_json::json!({
                "codex_approval_kind": "tool_suggestion",
                "tool_type": "connector",
                "suggest_type": "install",
                "suggest_reason": "Plan and reference events from your calendar",
                "tool_id": "connector_2128aebfecb84f64a069897515042a44",
                "tool_name": "Google Calendar",
                "install_url": "https://example.test/google-calendar",
            })),
        ),
    )
    .expect("expected tool suggestion form");

    assert_eq!(
        request.tool_suggestion(),
        Some(&ToolSuggestionRequest {
            tool_type: ToolSuggestionToolType::Connector,
            suggest_type: ToolSuggestionType::Install,
            suggest_reason: "Plan and reference events from your calendar".to_string(),
            tool_id: "connector_2128aebfecb84f64a069897515042a44".to_string(),
            tool_name: "Google Calendar".to_string(),
            install_url: Some("https://example.test/google-calendar".to_string()),
        })
    );
}

#[test]
fn plugin_tool_suggestion_meta_without_install_url_is_parsed_into_request_payload() {
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Suggest Slack",
            empty_object_schema(),
            Some(serde_json::json!({
                "codex_approval_kind": "tool_suggestion",
                "tool_type": "plugin",
                "suggest_type": "install",
                "suggest_reason": "Install the Slack plugin to search messages",
                "tool_id": "slack@openai-curated",
                "tool_name": "Slack",
            })),
        ),
    )
    .expect("expected tool suggestion form");

    assert_eq!(
        request.tool_suggestion(),
        Some(&ToolSuggestionRequest {
            tool_type: ToolSuggestionToolType::Plugin,
            suggest_type: ToolSuggestionType::Install,
            suggest_reason: "Install the Slack plugin to search messages".to_string(),
            tool_id: "slack@openai-curated".to_string(),
            tool_name: "Slack".to_string(),
            install_url: None,
        })
    );
}

#[test]
fn tool_approval_display_params_prefer_explicit_display_order() {
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Allow Calendar to create an event",
            empty_object_schema(),
            tool_approval_meta(
                &[],
                Some(serde_json::json!({
                    "zeta": 3,
                    "alpha": 1,
                })),
                Some(vec![
                    (
                        "calendar_id",
                        Value::String("primary".to_string()),
                        "Calendar",
                    ),
                    (
                        "title",
                        Value::String("Roadmap review".to_string()),
                        "Title",
                    ),
                ]),
            ),
        ),
    )
    .expect("expected approval fallback");

    assert_eq!(
        request.approval_display_params,
        vec![
            McpToolApprovalDisplayParam {
                name: "calendar_id".to_string(),
                value: Value::String("primary".to_string()),
                display_name: "Calendar".to_string(),
            },
            McpToolApprovalDisplayParam {
                name: "title".to_string(),
                value: Value::String("Roadmap review".to_string()),
                display_name: "Title".to_string(),
            },
        ]
    );
}

#[test]
fn submit_sends_accept_with_typed_content() {
    let (tx, mut rx) = test_sender();
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                        "description": "Approve the pending action.",
                    }
                },
                "required": ["confirmed"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let mut overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.select_current_option(/*committed*/ true);
    overlay.submit_answers();

    let event = rx.try_recv().expect("expected resolution");
    let AppEvent::SubmitThreadOp {
        thread_id: resolved_thread_id,
        op,
    } = event
    else {
        panic!("expected SubmitThreadOp");
    };
    assert_eq!(resolved_thread_id, thread_id);
    assert_eq!(
        op,
        Op::ResolveElicitation {
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            decision: McpServerElicitationAction::Accept,
            content: Some(serde_json::json!({
                "confirmed": true,
            })),
            meta: None,
        }
    );
}

#[test]
fn horizontal_list_keys_move_between_select_fields() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Choose values",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "first": {
                        "type": "boolean",
                        "title": "First",
                    },
                    "second": {
                        "type": "boolean",
                        "title": "Second",
                    }
                },
                "required": ["first", "second"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let mut overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.current_idx, 0);
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(overlay.current_idx, 1);
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(overlay.current_idx, 0);
    overlay.list_keymap.accept.clear();
    assert!(
        overlay
            .footer_tips()
            .iter()
            .all(|tip| !tip.to_string().contains("submit"))
    );
}

#[test]
fn switching_fields_clears_length_validation_flash() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Two fields",
            serde_json::json!({
                "type": "object",
                "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
                "required": ["a", "b"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("supported form");
    let mut overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ true,
    );
    overlay.handle_paste("x".repeat(codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS + 1));
    overlay.handle_key_event(KeyCode::Enter.into());
    assert!(overlay.next_frame_delay().is_some());
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(overlay.next_frame_delay(), None);
    assert!(overlay.composer.current_text().is_empty());
}

#[test]
fn text_fields_inherit_composer_chord_context_and_submit_binding() {
    let (tx, _rx) = test_sender();
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Enter a name",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "title": "Name",
                    }
                },
                "required": ["name"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("supported text field");
    let mut config = TuiKeymap::default();
    config.composer.submit = Some(KeybindingsSpec::One(KeybindingSpec(
        "ctrl-x enter".to_string(),
    )));
    let keymap = RuntimeKeymap::from_config(&config).expect("valid MCP composer chord");
    let mut overlay = McpServerElicitationOverlay::new_with_keymap(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ true, keymap,
    );

    let snapshot = render_snapshot(
        &overlay,
        Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 10,
        ),
    )
    .lines()
    .map(str::trim_end)
    .collect::<Vec<_>>()
    .join("\n");
    insta::assert_snapshot!(
        "mcp_server_elicitation_text_configured_key_chords",
        snapshot
    );
    assert!(
        overlay
            .keymap_contexts()
            .contains(crate::keymap::KeymapContext::Composer)
    );
    overlay.composer_submit_hint = None;
    assert!(
        overlay
            .footer_tips()
            .iter()
            .all(|tip| !tip.to_string().contains("submit"))
    );
}

#[test]
fn empty_tool_approval_schema_session_choice_sets_persist_meta() {
    let (tx, mut rx) = test_sender();
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            empty_object_schema(),
            tool_approval_meta(
                &[
                    APPROVAL_PERSIST_SESSION_VALUE,
                    APPROVAL_PERSIST_ALWAYS_VALUE,
                ],
                /*tool_params*/ None,
                /*tool_params_display*/ None,
            ),
        ),
    )
    .expect("expected approval fallback");
    let mut overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    if let Some(answer) = overlay.current_answer_mut() {
        answer.selection.selected_idx = Some(1);
    }
    overlay.select_current_option(/*committed*/ true);
    overlay.submit_answers();

    let event = rx.try_recv().expect("expected resolution");
    let AppEvent::SubmitThreadOp {
        thread_id: resolved_thread_id,
        op,
    } = event
    else {
        panic!("expected SubmitThreadOp");
    };
    assert_eq!(resolved_thread_id, thread_id);
    assert_eq!(
        op,
        Op::ResolveElicitation {
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            decision: McpServerElicitationAction::Accept,
            content: None,
            meta: Some(serde_json::json!({
                APPROVAL_PERSIST_KEY: APPROVAL_PERSIST_SESSION_VALUE,
            })),
        }
    );
}

#[test]
fn empty_tool_approval_schema_always_allow_sets_persist_meta() {
    let (tx, mut rx) = test_sender();
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            empty_object_schema(),
            tool_approval_meta(
                &[
                    APPROVAL_PERSIST_SESSION_VALUE,
                    APPROVAL_PERSIST_ALWAYS_VALUE,
                ],
                /*tool_params*/ None,
                /*tool_params_display*/ None,
            ),
        ),
    )
    .expect("expected approval fallback");
    let mut overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    if let Some(answer) = overlay.current_answer_mut() {
        answer.selection.selected_idx = Some(2);
    }
    overlay.select_current_option(/*committed*/ true);
    overlay.submit_answers();

    let event = rx.try_recv().expect("expected resolution");
    let AppEvent::SubmitThreadOp {
        thread_id: resolved_thread_id,
        op,
    } = event
    else {
        panic!("expected SubmitThreadOp");
    };
    assert_eq!(resolved_thread_id, thread_id);
    assert_eq!(
        op,
        Op::ResolveElicitation {
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            decision: McpServerElicitationAction::Accept,
            content: None,
            meta: Some(serde_json::json!({
                APPROVAL_PERSIST_KEY: APPROVAL_PERSIST_ALWAYS_VALUE,
            })),
        }
    );
}

#[test]
fn ctrl_c_cancels_elicitation() {
    let (tx, mut rx) = test_sender();
    let thread_id = ThreadId::default();
    let request = from_form_request(
        thread_id,
        form_request(
            "Allow this request?",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                        "description": "Approve the pending action.",
                    }
                },
                "required": ["confirmed"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let mut overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.on_ctrl_c(), CancellationEvent::Handled);

    let event = rx.try_recv().expect("expected resolution");
    let AppEvent::SubmitThreadOp {
        thread_id: resolved_thread_id,
        op,
    } = event
    else {
        panic!("expected SubmitThreadOp");
    };
    assert_eq!(resolved_thread_id, thread_id);
    assert_eq!(
        op,
        Op::ResolveElicitation {
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
            decision: McpServerElicitationAction::Cancel,
            content: None,
            meta: None,
        }
    );
}

#[test]
fn queues_requests_fifo() {
    let (tx, _rx) = test_sender();
    let first = from_form_request(
        ThreadId::default(),
        form_request(
            "First",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                    }
                },
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let second = from_form_request(
        ThreadId::default(),
        form_request(
            "Second",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                    }
                },
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let third = from_form_request(
        ThreadId::default(),
        form_request(
            "Third",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                    }
                },
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let mut overlay = McpServerElicitationOverlay::new(
        first, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.try_consume_mcp_server_elicitation_request(second);
    overlay.try_consume_mcp_server_elicitation_request(third);
    overlay.select_current_option(/*committed*/ true);
    overlay.submit_answers();

    assert_eq!(overlay.request.message, "Second");

    overlay.select_current_option(/*committed*/ true);
    overlay.submit_answers();

    assert_eq!(overlay.request.message, "Third");
}

#[test]
fn resolved_request_dismisses_overlay_without_emitting_events() {
    let (tx, mut rx) = test_sender();
    let thread_id = ThreadId::default();
    let supported_form_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "confirmed": {
                "type": "boolean",
                "title": "Confirm",
            }
        },
    });
    let mut overlay = McpServerElicitationOverlay::new(
        from_form_request(
            thread_id,
            form_request("First", supported_form_schema.clone(), /*meta*/ None),
        )
        .expect("expected supported form"),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.try_consume_mcp_server_elicitation_request(
        McpServerElicitationFormRequest::from_app_server_request(
            thread_id,
            request_id("request-2"),
            &McpServerElicitationRequestParams {
                thread_id: "thread-1".to_string(),
                turn_id: Some("turn-2".to_string()),
                server_name: "server-1".to_string(),
                request: McpServerElicitationRequest::Form {
                    meta: None,
                    message: "Second".to_string(),
                    requested_schema: serde_json::from_value(supported_form_schema)
                        .expect("test schema should deserialize"),
                },
            },
        )
        .expect("expected supported form"),
    );

    assert!(
        overlay.dismiss_app_server_request(&ResolvedAppServerRequest::McpElicitation {
            server_name: "server-1".to_string(),
            request_id: request_id("request-1"),
        })
    );
    assert_eq!(overlay.request.message, "Second");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    assert!(
        overlay.dismiss_app_server_request(&ResolvedAppServerRequest::McpElicitation {
            server_name: "server-1".to_string(),
            request_id: request_id("request-2"),
        })
    );
    assert!(overlay.is_complete());
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn form_prompt_preserves_the_complete_wrapped_url_destination() {
    let url = "https://github.com/openai/codex/pull/12345?diff=split";
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            &format!("Review {url} before choosing."),
            serde_json::json!({
                "type": "object",
                "properties": {"confirmed": {"type": "boolean", "title": "Continue"}},
                "required": ["confirmed"]
            }),
            /*meta*/ None,
        ),
    )
    .expect("supported form");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 40, 20);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    let linked = buf
        .content
        .iter()
        .filter(|cell| cell.symbol().contains("\x1b]8;;"))
        .map(|cell| {
            assert!(cell.symbol().starts_with(&format!("\x1b]8;;{url}\x07")));
            crate::terminal_hyperlinks::strip_osc8(cell.symbol())
        })
        .collect::<String>();
    assert_eq!(linked, url);
    insta::assert_snapshot!("mcp_form_wrapped_url", snapshot_buffer(&buf));
}

#[test]
fn boolean_form_snapshot() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Allow this request?",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "confirmed": {
                        "type": "boolean",
                        "title": "Confirm",
                        "description": "Approve the pending action.",
                    }
                },
                "required": ["confirmed"],
            }),
            /*meta*/ None,
        ),
    )
    .expect("expected supported form");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(
        overlay.program_status_blocked_kind(),
        Some(ProgramStatusKind::Question)
    );
    insta::assert_snapshot!(
        "mcp_server_elicitation_boolean_form",
        render_snapshot(&overlay, Rect::new(0, 0, 120, 16))
    );
}

#[test]
fn approval_form_tool_approval_snapshot() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Allow this request?",
            empty_object_schema(),
            tool_approval_meta(
                &[],
                /*tool_params*/ None,
                /*tool_params_display*/ None,
            ),
        ),
    )
    .expect("expected approval fallback");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(
        overlay.program_status_blocked_kind(),
        Some(ProgramStatusKind::Permission)
    );
    insta::assert_snapshot!(
        "mcp_server_elicitation_approval_form_without_schema",
        render_snapshot(&overlay, Rect::new(0, 0, 120, 16))
    );
}

#[test]
fn message_only_form_snapshot() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Boolean elicit MCP example: do you confirm?",
            empty_object_schema(),
            /*meta*/ None,
        ),
    )
    .expect("expected message-only form");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    insta::assert_snapshot!(
        "mcp_server_elicitation_message_only_form",
        render_snapshot(&overlay, Rect::new(0, 0, 120, 16))
    );
}

#[test]
fn message_only_form_with_persist_options_snapshot() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Boolean elicit MCP example: do you confirm?",
            empty_object_schema(),
            Some(serde_json::json!({
                APPROVAL_PERSIST_KEY: [
                    APPROVAL_PERSIST_SESSION_VALUE,
                    APPROVAL_PERSIST_ALWAYS_VALUE,
                ],
            })),
        ),
    )
    .expect("expected message-only form");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    insta::assert_snapshot!(
        "mcp_server_elicitation_message_only_form_with_persist_options",
        render_snapshot(&overlay, Rect::new(0, 0, 120, 16))
    );
}

#[test]
fn approval_form_tool_approval_with_persist_options_snapshot() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
        ThreadId::default(),
        form_request(
            "Allow this request?",
            empty_object_schema(),
            tool_approval_meta(
                &[
                    APPROVAL_PERSIST_SESSION_VALUE,
                    APPROVAL_PERSIST_ALWAYS_VALUE,
                ],
                /*tool_params*/ None,
                /*tool_params_display*/ None,
            ),
        ),
    )
    .expect("expected approval fallback");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    insta::assert_snapshot!(
        "mcp_server_elicitation_approval_form_with_session_persist",
        render_snapshot(&overlay, Rect::new(0, 0, 120, 16))
    );
}

#[test]
fn approval_form_tool_approval_with_param_summary_snapshot() {
    let (tx, _rx) = test_sender();
    let request = from_form_request(
            ThreadId::default(),
            form_request(
                "Allow Calendar to create an event",
                empty_object_schema(),
                tool_approval_meta(
                    &[],
                    Some(serde_json::json!({
                        "calendar_id": "primary",
                        "title": "Roadmap review",
                        "notes": "This is a deliberately long note that should truncate before it turns the approval body into a giant wall of text in the TUI overlay.",
                        "ignored_after_limit": "fourth param",
                    })),
                    Some(vec![
                        (
                            "calendar_id",
                            Value::String("primary".to_string()),
                            "Calendar",
                        ),
                        (
                            "title",
                            Value::String("Roadmap review".to_string()),
                            "Title",
                        ),
                        (
                            "notes",
                            Value::String("This is a deliberately long note that should truncate before it turns the approval body into a giant wall of text in the TUI overlay.".to_string()),
                            "Notes",
                        ),
                        (
                            "ignored_after_limit",
                            Value::String("fourth param".to_string()),
                            "Ignored",
                        ),
                    ]),
                ),
            ),
        )
        .expect("expected approval fallback");
    let overlay = McpServerElicitationOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    insta::assert_snapshot!(
        "mcp_server_elicitation_approval_form_with_param_summary",
        render_snapshot(&overlay, Rect::new(0, 0, 120, 16))
    );
}
