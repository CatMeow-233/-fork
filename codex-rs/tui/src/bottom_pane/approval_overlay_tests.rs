use super::*;
use crate::app_event::AppEvent;
use crate::keymap::RuntimeKeymap;
use codex_app_server_protocol::AdditionalFileSystemPermissions;
use codex_app_server_protocol::AdditionalNetworkPermissions;
use codex_app_server_protocol::ExecPolicyAmendment;
use codex_app_server_protocol::NetworkApprovalProtocol;
use codex_app_server_protocol::NetworkPolicyAmendment;
use codex_config::types::KeybindingSpec;
use codex_config::types::KeybindingsSpec;
use codex_config::types::TuiKeymap;
use codex_protocol::models::FileSystemPermissions;
use codex_protocol::models::NetworkPermissions;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::test_support::PathExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::unbounded_channel;

fn absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
}

fn render_overlay_lines(view: &ApprovalOverlay, width: u16) -> String {
    let height = view.desired_height(width);
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    view.render(Rect::new(0, 0, width, height), &mut buf);
    (0..buf.area.height)
        .map(|row| {
            (0..buf.area.width)
                .map(|col| buf[(col, row)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_history_cell_lines(
    cell: &dyn crate::history_cell::HistoryCell,
    width: u16,
) -> Vec<String> {
    cell.display_lines(width)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn normalize_snapshot_paths(rendered: String) -> String {
    [
        (absolute_path("/tmp/readme.txt"), "/tmp/readme.txt"),
        (absolute_path("/tmp/out.txt"), "/tmp/out.txt"),
    ]
    .into_iter()
    .fold(rendered, |rendered, (path, normalized)| {
        rendered.replace(&path.display().to_string(), normalized)
    })
}

fn make_overlay(
    request: ApprovalRequest,
    app_event_tx: AppEventSender,
    features: Features,
) -> ApprovalOverlay {
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    make_overlay_with_keymap(
        request,
        app_event_tx,
        features,
        keymap.approval,
        keymap.list,
    )
}

fn make_overlay_with_keymap(
    request: ApprovalRequest,
    app_event_tx: AppEventSender,
    features: Features,
    approval_keymap: ApprovalKeymap,
    list_keymap: ListKeymap,
) -> ApprovalOverlay {
    ApprovalOverlay::new(
        request,
        app_event_tx,
        features,
        approval_keymap,
        list_keymap,
    )
}

fn make_exec_request() -> ApprovalRequest {
    ApprovalRequest::Exec(ExecApprovalRequest {
        kind: Default::default(),
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "test".to_string(),
        environment_id: None,
        command: vec!["echo".to_string(), "hi".to_string()],
        reason: Some("reason".to_string()),
        available_decisions: vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::Cancel,
        ],
        network_approval_context: None,
        additional_permissions: None,
    })
}

fn make_permissions_request() -> ApprovalRequest {
    ApprovalRequest::Permissions(PermissionsApprovalRequest {
        thread_id: ThreadId::new(),
        thread_label: None,
        call_id: "test".to_string(),
        environment_id: None,
        reason: Some("need workspace access".to_string()),
        permissions: RequestPermissionProfile {
            network: Some(NetworkPermissions {
                enabled: Some(true),
            }),
            file_system: Some(FileSystemPermissions::from_read_write_roots(
                Some(vec![absolute_path("/tmp/readme.txt")]),
                Some(vec![absolute_path("/tmp/out.txt")]),
            )),
        },
    })
}

fn make_elicitation_request() -> ApprovalRequest {
    ApprovalRequest::McpElicitation(McpElicitationApprovalRequest {
        thread_id: ThreadId::new(),
        thread_label: None,
        server_name: "test-server".to_string(),
        request_id: RequestId::String("request-1".to_string()),
        message: "Need more information".to_string(),
    })
}

#[test]
fn approval_shortcuts_display_chords_in_configured_order() {
    let chord = crate::key_hint::ShortcutHint::Chord {
        prefix: key_hint::ctrl(KeyCode::Char('x')),
        completion: key_hint::plain(KeyCode::Char('y')),
    };
    let single = crate::key_hint::ShortcutHint::Single(key_hint::plain(KeyCode::Char('y')));
    let binding = |spec: &str| KeybindingSpec(spec.to_string());
    for (specs, expected_shortcut) in [
        (KeybindingsSpec::One(binding("ctrl-x y")), chord),
        (
            KeybindingsSpec::Many(vec![binding("y"), binding("ctrl-x y")]),
            single,
        ),
        (
            KeybindingsSpec::Many(vec![binding("ctrl-x y"), binding("y")]),
            chord,
        ),
    ] {
        let mut config = TuiKeymap::default();
        config.approval.approve = Some(specs);
        let keymap = RuntimeKeymap::from_config(&config).expect("valid approval keymap");
        let request = make_exec_request();
        let (_, params) = ApprovalOverlay::build_options(
            &request,
            build_header(&request),
            &Features::with_defaults(),
            &keymap.approval,
            &keymap.list,
        );
        let approval = params
            .items
            .iter()
            .find(|item| item.name == "Yes, proceed")
            .expect("approval selection");

        assert_eq!(approval.display_shortcut, Some(expected_shortcut));
        if matches!(
            expected_shortcut,
            crate::key_hint::ShortcutHint::Chord { .. }
        ) {
            let dispatch_token = keymap.approval.approve.last().expect("chord token");
            assert_eq!(
                keymap
                    .approval
                    .hint_for_bindings(std::slice::from_ref(dispatch_token)),
                Some(expected_shortcut)
            );
        }
    }
}

#[test]
fn ctrl_c_aborts_and_clears_queue() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(make_exec_request(), tx, Features::with_defaults());
    view.enqueue_request(make_exec_request());
    assert_eq!(CancellationEvent::Handled, view.on_ctrl_c());
    assert!(view.queue.is_empty());
    assert!(view.is_complete());
}

#[test]
fn configured_list_cancel_aborts_exec_approval() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.list.cancel = vec![key_hint::plain(KeyCode::Char('q'))];
    let mut view = make_overlay_with_keymap(
        make_exec_request(),
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

    assert!(view.is_complete());
    let mut decision = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ExecApproval { decision: d, .. },
            ..
        } = ev
        {
            decision = Some(d);
            break;
        }
    }
    assert_eq!(decision, Some(CommandExecutionApprovalDecision::Cancel));
}

#[test]
fn configured_list_cancel_cancels_mcp_elicitation() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.list.cancel = vec![key_hint::plain(KeyCode::Char('q'))];
    let mut view = make_overlay_with_keymap(
        make_elicitation_request(),
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

    assert!(view.is_complete());
    let mut decision = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ResolveElicitation { decision: d, .. },
            ..
        } = ev
        {
            decision = Some(d);
            break;
        }
    }
    assert_eq!(decision, Some(McpServerElicitationAction::Cancel));
}

#[test]
fn shortcut_triggers_selection() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(make_exec_request(), tx, Features::with_defaults());
    assert!(!view.is_complete());
    view.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    // We expect at least one thread-scoped approval op message in the queue.
    let mut saw_op = false;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, AppEvent::SubmitThreadOp { .. }) {
            saw_op = true;
            break;
        }
    }
    assert!(saw_op, "expected approval decision to emit an op");
}

#[test]
fn deny_shortcut_submits_denied_exec_decision() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id: ThreadId::new(),
            thread_label: None,
            id: "test".to_string(),
            environment_id: None,
            command: vec!["echo".to_string(), "hi".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::Decline,
            ],
            network_approval_context: None,
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    let mut saw_denied = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ExecApproval { decision, .. },
            ..
        } = ev
        {
            assert_eq!(decision, CommandExecutionApprovalDecision::Decline);
            saw_denied = true;
            break;
        }
    }
    assert!(saw_denied, "expected deny shortcut to emit denied decision");
}

#[test]
fn approval_headers_preserve_wrapped_url_destinations() {
    let url = "https://github.com/openai/codex/pull/12345?diff=split";
    for mut request in [
        make_exec_request(),
        make_permissions_request(),
        make_elicitation_request(),
        ApprovalRequest::ApplyPatch(ApplyPatchApprovalRequest {
            thread_id: ThreadId::new(),
            thread_label: None,
            id: "patch".into(),
            reason: None,
            cwd: absolute_path("/tmp"),
            changes: HashMap::new(),
        }),
    ] {
        match &mut request {
            ApprovalRequest::Exec(request) => request.reason = Some(url.into()),
            ApprovalRequest::Permissions(request) => request.reason = Some(url.into()),
            ApprovalRequest::McpElicitation(request) => request.message = url.into(),
            ApprovalRequest::ApplyPatch(request) => request.reason = Some(url.into()),
        }
        let header = build_header(&request);
        let area = Rect::new(0, 0, 32, header.desired_height(/*width*/ 32));
        let mut buf = Buffer::empty(area);
        header.render(area, &mut buf);
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
        if matches!(request, ApprovalRequest::Exec(_)) {
            let visible = buf
                .content
                .chunks(32)
                .map(|row| {
                    row.iter()
                        .map(|cell| crate::terminal_hyperlinks::strip_osc8(cell.symbol()))
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n");
            insta::assert_snapshot!("approval_header_wrapped_url", visible);
        }
    }
}

#[test]
fn network_deny_shortcut_submits_policy_deny_decision() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let amendment = NetworkPolicyAmendment {
        host: "example.com".to_string(),
        action: NetworkPolicyRuleAction::Deny,
    };
    let mut view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id: ThreadId::new(),
            thread_label: None,
            id: "test".to_string(),
            environment_id: None,
            command: vec!["curl".to_string(), "https://example.com".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                    network_policy_amendment: amendment.clone(),
                },
            ],
            network_approval_context: Some(NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol: NetworkApprovalProtocol::Https,
            }),
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    let mut saw_deny = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ExecApproval { decision, .. },
            ..
        } = ev
        {
            assert_eq!(
                decision,
                CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                    network_policy_amendment: amendment
                }
            );
            saw_deny = true;
            break;
        }
    }
    assert!(
        saw_deny,
        "expected deny shortcut to emit network policy deny decision"
    );
}

#[test]
fn resolved_request_dismisses_overlay_without_emitting_abort() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let request = make_exec_request();
    let thread_id = request.thread_id();
    let mut view = make_overlay(request, tx, Features::with_defaults());

    assert!(
        !view.dismiss_app_server_request(&ResolvedAppServerRequest::ExecApproval {
            thread_id: ThreadId::new().to_string(),
            id: "test".to_string(),
        })
    );
    assert!(!view.is_complete());
    assert!(
        view.dismiss_app_server_request(&ResolvedAppServerRequest::ExecApproval {
            thread_id: thread_id.to_string(),
            id: "test".to_string(),
        })
    );
    assert!(
        view.is_complete(),
        "resolved request should close the overlay"
    );
    assert!(
        rx.try_recv().is_err(),
        "dismissing a stale request should not emit an approval op"
    );
}

#[test]
fn o_opens_source_thread_for_cross_thread_approval() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let thread_id = ThreadId::new();
    let mut view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id,
            thread_label: Some("Robie [explorer]".to_string()),
            id: "test".to_string(),
            environment_id: None,
            command: vec!["echo".to_string(), "hi".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::Cancel,
            ],
            network_approval_context: None,
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));

    let event = rx.try_recv().expect("expected select-agent-thread event");
    assert_eq!(
        matches!(event, AppEvent::SelectAgentThread(id) if id == thread_id),
        true
    );
}

#[test]
fn configured_open_thread_shortcut_opens_source_thread() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let thread_id = ThreadId::new();
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.approval.open_thread = vec![key_hint::plain(KeyCode::Char('x'))];
    let mut view = make_overlay_with_keymap(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id,
            thread_label: Some("Robie [explorer]".to_string()),
            id: "test".to_string(),
            environment_id: None,
            command: vec!["echo".to_string(), "hi".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::Cancel,
            ],
            network_approval_context: None,
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );

    view.handle_key_event(KeyEvent::new(
        KeyCode::Char('o'),
        /*modifiers*/ KeyModifiers::NONE,
    ));
    assert!(rx.try_recv().is_err());

    view.handle_key_event(KeyEvent::new(
        KeyCode::Char('x'),
        /*modifiers*/ KeyModifiers::NONE,
    ));
    let event = rx.try_recv().expect("expected select-agent-thread event");
    assert!(matches!(event, AppEvent::SelectAgentThread(id) if id == thread_id));
}

#[test]
fn cross_thread_footer_hint_mentions_o_shortcut() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id: ThreadId::new(),
            thread_label: Some("Robie [explorer]".to_string()),
            id: "test".to_string(),
            environment_id: None,
            command: vec!["echo".to_string(), "hi".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::Cancel,
            ],
            network_approval_context: None,
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );

    assert_snapshot!(
        "approval_overlay_cross_thread_prompt",
        render_overlay_lines(&view, /*width*/ 80)
    );
}

#[test]
fn exec_prefix_option_emits_execpolicy_amendment() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id: ThreadId::new(),
            thread_label: None,
            id: "test".to_string(),
            environment_id: None,
            command: vec!["echo".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                    execpolicy_amendment: ExecPolicyAmendment {
                        command: vec!["echo".to_string()],
                    },
                },
                CommandExecutionApprovalDecision::Cancel,
            ],
            network_approval_context: None,
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    let mut saw_op = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ExecApproval { decision, .. },
            ..
        } = ev
        {
            assert_eq!(
                decision,
                CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                    execpolicy_amendment: ExecPolicyAmendment {
                        command: vec!["echo".to_string()],
                    }
                }
            );
            saw_op = true;
            break;
        }
    }
    assert!(
        saw_op,
        "expected approval decision to emit an op with command prefix"
    );
}

#[test]
fn network_deny_forever_shortcut_is_not_bound() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id: ThreadId::new(),
            thread_label: None,
            id: "test".to_string(),
            environment_id: None,
            command: vec!["curl".to_string(), "https://example.com".to_string()],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::AcceptForSession,
                CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                    network_policy_amendment: NetworkPolicyAmendment {
                        host: "example.com".to_string(),
                        action: NetworkPolicyRuleAction::Allow,
                    },
                },
                CommandExecutionApprovalDecision::Cancel,
            ],
            network_approval_context: Some(NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol: NetworkApprovalProtocol::Https,
            }),
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

    assert!(
        rx.try_recv().is_err(),
        "unexpected approval event emitted for hidden network deny shortcut"
    );
}

#[test]
fn header_includes_command_snippet() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let command = vec!["echo".into(), "hello".into(), "world".into()];
    let exec_request = ApprovalRequest::Exec(ExecApprovalRequest {
        kind: Default::default(),
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "test".into(),
        environment_id: None,
        command,
        reason: None,
        available_decisions: vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::Cancel,
        ],
        network_approval_context: None,
        additional_permissions: None,
    });

    let view = make_overlay(exec_request, tx, Features::with_defaults());
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, view.desired_height(/*width*/ 80)));
    view.render(
        Rect::new(0, 0, 80, view.desired_height(/*width*/ 80)),
        &mut buf,
    );

    let rendered: Vec<String> = (0..buf.area.height)
        .map(|row| {
            (0..buf.area.width)
                .map(|col| buf[(col, row)].symbol().to_string())
                .collect()
        })
        .collect();
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("echo hello world")),
        "expected header to include command snippet, got {rendered:?}"
    );
}

#[test]
fn network_exec_options_use_expected_labels_and_hide_execpolicy_amendment() {
    let network_context = NetworkApprovalContext {
        host: "example.com".to_string(),
        protocol: NetworkApprovalProtocol::Https,
    };
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    let options = exec_options(
        &[
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::AcceptForSession,
            CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                network_policy_amendment: NetworkPolicyAmendment {
                    host: "example.com".to_string(),
                    action: NetworkPolicyRuleAction::Allow,
                },
            },
            CommandExecutionApprovalDecision::Cancel,
        ],
        Some(&network_context),
        /*additional_permissions*/ None,
        &keymap.approval,
    );

    let labels: Vec<String> = options.into_iter().map(|option| option.label).collect();
    assert_eq!(
        labels,
        vec![
            "Yes, just this once".to_string(),
            "Yes, and allow this host for this conversation".to_string(),
            "Yes, and allow this host in the future".to_string(),
            "No, and tell Codex what to do differently".to_string(),
        ]
    );
}

#[test]
fn generic_exec_options_can_offer_allow_for_session() {
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    let options = exec_options(
        &[
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::AcceptForSession,
            CommandExecutionApprovalDecision::Cancel,
        ],
        /*network_approval_context*/ None,
        /*additional_permissions*/ None,
        &keymap.approval,
    );

    let labels: Vec<String> = options.into_iter().map(|option| option.label).collect();
    assert_eq!(
        labels,
        vec![
            "Yes, proceed".to_string(),
            "Yes, and don't ask again for this command in this session".to_string(),
            "No, and tell Codex what to do differently".to_string(),
        ]
    );
}

#[test]
fn additional_permissions_exec_options_hide_execpolicy_amendment() {
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    let additional_permissions = AdditionalPermissionProfile {
        network: None,
        file_system: Some(
            FileSystemPermissions::from_read_write_roots(
                Some(vec![absolute_path("/tmp/readme.txt")]),
                Some(vec![absolute_path("/tmp/out.txt")]),
            )
            .into(),
        ),
    };
    let options = exec_options(
        &[
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::Cancel,
        ],
        /*network_approval_context*/ None,
        Some(&additional_permissions),
        &keymap.approval,
    );

    let labels: Vec<String> = options.into_iter().map(|option| option.label).collect();
    assert_eq!(
        labels,
        vec![
            "Yes, proceed".to_string(),
            "No, and tell Codex what to do differently".to_string(),
        ]
    );
}

#[test]
fn permissions_options_use_expected_labels() {
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    let labels: Vec<String> = permissions_options(&keymap.approval)
        .into_iter()
        .map(|option| option.label)
        .collect();
    assert_eq!(
        labels,
        vec![
            "Yes, grant these permissions for this turn".to_string(),
            "Yes, grant for this turn with strict auto review".to_string(),
            "Yes, grant these permissions for this session".to_string(),
            "No, continue without permissions".to_string(),
        ]
    );
}

#[test]
fn additional_permissions_rule_shows_non_path_file_system_entries() {
    let additional_permissions = AdditionalPermissionProfile {
        network: None,
        file_system: Some(AdditionalFileSystemPermissions {
            read: None,
            write: None,
            entries: Some(vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Write,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::GlobPattern {
                        pattern: "**/*.env".to_string(),
                    },
                    access: FileSystemAccessMode::Deny,
                },
            ]),
            glob_scan_max_depth: None,
        }),
    };

    assert_eq!(
        format_additional_permissions_rule(&additional_permissions),
        Some("write `:root`; deny read glob `**/*.env`".to_string())
    );
}

#[test]
fn additional_permissions_rule_uses_workspace_roots_label() {
    let additional_permissions = AdditionalPermissionProfile {
        network: None,
        file_system: Some(AdditionalFileSystemPermissions {
            read: None,
            write: None,
            entries: Some(vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::ProjectRoots {
                        subpath: Some(LegacyAppPathString::from_path(std::path::Path::new(".git"))),
                    },
                },
                access: FileSystemAccessMode::Read,
            }]),
            glob_scan_max_depth: None,
        }),
    };

    assert_eq!(
        format_additional_permissions_rule(&additional_permissions),
        Some("read `:workspace_roots/.git`".to_string())
    );
}

#[test]
fn permissions_session_shortcut_submits_session_scope() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(make_permissions_request(), tx, Features::with_defaults());

    view.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

    let mut saw_op = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::RequestPermissionsResponse { response, .. },
            ..
        } = ev
        {
            assert_eq!(response.scope, PermissionGrantScope::Session);
            saw_op = true;
            break;
        }
    }
    assert!(
        saw_op,
        "expected permission approval decision to emit a session-scoped response"
    );
}

#[test]
fn permissions_deny_shortcut_uses_deny_keymap() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.approval.deny = vec![key_hint::plain(KeyCode::Char('x'))];
    keymap.approval.decline = Vec::new();
    let mut view = make_overlay_with_keymap(
        make_permissions_request(),
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));

    let mut saw_op = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::RequestPermissionsResponse { response, .. },
            ..
        } = ev
        {
            assert!(response.permissions.is_empty());
            assert_eq!(response.scope, PermissionGrantScope::Turn);
            assert!(!response.strict_auto_review);
            saw_op = true;
            break;
        }
    }
    assert!(
        saw_op,
        "expected permission deny shortcut to emit an empty permission response"
    );
}

#[test]
fn permissions_strict_auto_review_shortcut_submits_turn_scope_with_strict_review() {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut view = make_overlay(make_permissions_request(), tx, Features::with_defaults());

    view.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

    let mut saw_op = false;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::RequestPermissionsResponse { response, .. },
            ..
        } = ev
        {
            assert_eq!(response.scope, PermissionGrantScope::Turn);
            assert!(response.strict_auto_review);
            saw_op = true;
            break;
        }
    }
    assert!(
        saw_op,
        "expected permission approval decision to emit a strict auto review response"
    );
}

#[test]
fn additional_permissions_prompt_shows_permission_rule_line() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let exec_request = ApprovalRequest::Exec(ExecApprovalRequest {
        kind: Default::default(),
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "test".into(),
        environment_id: None,
        command: vec!["cat".into(), "/tmp/readme.txt".into()],
        reason: None,
        available_decisions: vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::Cancel,
        ],
        network_approval_context: None,
        additional_permissions: Some(AdditionalPermissionProfile {
            network: Some(AdditionalNetworkPermissions {
                enabled: Some(true),
            }),
            file_system: Some(
                FileSystemPermissions::from_read_write_roots(
                    Some(vec![absolute_path("/tmp/readme.txt")]),
                    Some(vec![absolute_path("/tmp/out.txt")]),
                )
                .into(),
            ),
        }),
    });

    let view = make_overlay(exec_request, tx, Features::with_defaults());
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, view.desired_height(/*width*/ 100)));
    view.render(
        Rect::new(0, 0, 100, view.desired_height(/*width*/ 100)),
        &mut buf,
    );

    let rendered: Vec<String> = (0..buf.area.height)
        .map(|row| {
            (0..buf.area.width)
                .map(|col| buf[(col, row)].symbol().to_string())
                .collect()
        })
        .collect();

    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Permission rule:")),
        "expected permission-rule line, got {rendered:?}"
    );
    assert!(
        rendered.iter().any(|line| line.contains("network;")),
        "expected network permission text, got {rendered:?}"
    );
}

#[test]
fn additional_permissions_prompt_snapshot() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let exec_request = ApprovalRequest::Exec(ExecApprovalRequest {
        kind: Default::default(),
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "test".into(),
        environment_id: None,
        command: vec!["cat".into(), "/tmp/readme.txt".into()],
        reason: Some("need filesystem access".into()),
        available_decisions: vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::Cancel,
        ],
        network_approval_context: None,
        additional_permissions: Some(AdditionalPermissionProfile {
            network: Some(AdditionalNetworkPermissions {
                enabled: Some(true),
            }),
            file_system: Some(
                FileSystemPermissions::from_read_write_roots(
                    Some(vec![absolute_path("/tmp/readme.txt")]),
                    Some(vec![absolute_path("/tmp/out.txt")]),
                )
                .into(),
            ),
        }),
    });

    let view = make_overlay(exec_request, tx, Features::with_defaults());
    assert_snapshot!(
        "approval_overlay_additional_permissions_prompt",
        normalize_snapshot_paths(render_overlay_lines(&view, /*width*/ 120))
    );
}

#[test]
fn permissions_prompt_snapshot() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let view = make_overlay(make_permissions_request(), tx, Features::with_defaults());
    assert_snapshot!(
        "approval_overlay_permissions_prompt",
        normalize_snapshot_paths(render_overlay_lines(&view, /*width*/ 120))
    );
}

#[test]
fn apply_patch_prompt_with_thread_label_omits_command_line() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let mut changes = HashMap::new();
    changes.insert(
        PathBuf::from("bug1.txt"),
        FileChange::Add {
            content: "one\ntwo\nthree\n".to_string(),
        },
    );
    changes.insert(
        PathBuf::from("old.txt"),
        FileChange::Update {
            unified_diff: String::new(),
            move_path: Some(PathBuf::from("new.txt")),
        },
    );
    #[cfg(windows)]
    let (foreign_path, foreign_move_path) = ("/remote/old.txt", "/remote/new.txt");
    #[cfg(not(windows))]
    let (foreign_path, foreign_move_path) = (r"C:\workspace\old.txt", r"C:\workspace\new.txt");
    changes.insert(
        PathBuf::from(foreign_path),
        FileChange::Update {
            unified_diff: String::new(),
            move_path: Some(PathBuf::from(foreign_move_path)),
        },
    );
    let request = ApprovalRequest::ApplyPatch(ApplyPatchApprovalRequest {
        thread_id: ThreadId::new(),
        thread_label: Some("Banach [worker]".to_string()),
        id: "test".to_string(),
        reason: None,
        cwd: test_path_buf("/tmp").abs(),
        changes,
    });
    let keymap = crate::keymap::RuntimeKeymap::defaults();
    let view = ApprovalOverlay::new(
        request,
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );
    let rendered = render_overlay_lines(&view, /*width*/ 120);
    assert!(rendered.contains("Thread: Banach [worker]"));
    assert!(rendered.contains("Description: Apply proposed file edits"));
    for path in ["/tmp/bug1.txt", "/tmp/new.txt", "/tmp/old.txt"] {
        assert!(rendered.contains(&format!("Destination: {}", test_path_buf(path).display())));
    }
    for path in [foreign_path, foreign_move_path] {
        assert!(rendered.contains(&format!("Destination: {path}")));
    }
    assert!(rendered.contains("o to open thread"));
    assert!(!rendered.contains("$ apply_patch"));
}

#[test]
fn apply_patch_prompt_without_changes_shows_unavailable_destination() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let request = ApprovalRequest::ApplyPatch(ApplyPatchApprovalRequest {
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "test".to_string(),
        reason: None,
        cwd: absolute_path("/tmp"),
        changes: HashMap::new(),
    });
    let view = make_overlay(request, tx, Features::with_defaults());
    assert_snapshot!(
        "approval_overlay_patch_destination_unavailable",
        normalize_snapshot_paths(render_overlay_lines(&view, /*width*/ 120))
    );
}

#[test]
fn network_exec_prompt_title_includes_host() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx);
    let exec_request = ApprovalRequest::Exec(ExecApprovalRequest {
        kind: Default::default(),
        thread_id: ThreadId::new(),
        thread_label: None,
        id: "test".into(),
        environment_id: None,
        command: vec!["curl".into(), "https://example.com".into()],
        reason: Some("network request blocked".into()),
        available_decisions: vec![
            CommandExecutionApprovalDecision::Accept,
            CommandExecutionApprovalDecision::AcceptForSession,
            CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                network_policy_amendment: NetworkPolicyAmendment {
                    host: "example.com".to_string(),
                    action: NetworkPolicyRuleAction::Allow,
                },
            },
            CommandExecutionApprovalDecision::Cancel,
        ],
        network_approval_context: Some(NetworkApprovalContext {
            host: "example.com".to_string(),
            protocol: NetworkApprovalProtocol::Https,
        }),
        additional_permissions: None,
    });

    let view = make_overlay(exec_request, tx, Features::with_defaults());
    let mut buf = Buffer::empty(Rect::new(0, 0, 100, view.desired_height(/*width*/ 100)));
    view.render(
        Rect::new(0, 0, 100, view.desired_height(/*width*/ 100)),
        &mut buf,
    );
    assert_snapshot!("network_exec_prompt", format!("{buf:?}"));

    let rendered: Vec<String> = (0..buf.area.height)
        .map(|row| {
            (0..buf.area.width)
                .map(|col| buf[(col, row)].symbol().to_string())
                .collect()
        })
        .collect();

    assert!(
        rendered.iter().any(|line| {
            line.contains("Do you want to approve network access to \"example.com\"?")
        }),
        "expected network title to include host, got {rendered:?}"
    );
    assert!(
        !rendered.iter().any(|line| line.contains("$ curl")),
        "network prompt should not show command line, got {rendered:?}"
    );
    assert!(
        !rendered.iter().any(|line| line.contains("don't ask again")),
        "network prompt should not show execpolicy option, got {rendered:?}"
    );
}

#[test]
fn ctrl_shift_a_opens_fullscreen() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = make_overlay(make_exec_request(), tx, Features::with_defaults());

    view.handle_key_event(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));

    let mut saw_fullscreen = false;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, AppEvent::FullScreenApprovalRequest(_)) {
            saw_fullscreen = true;
            break;
        }
    }
    assert!(saw_fullscreen, "expected ctrl+shift+a to open fullscreen");
}

#[test]
fn exec_history_cell_wraps_with_two_space_indent() {
    let command = vec![
        "/bin/zsh".into(),
        "-lc".into(),
        "git add tui/src/render/mod.rs tui/src/render/renderable.rs".into(),
    ];
    let cell = history_cell::new_approval_decision_cell(
        history_cell::ApprovalDecisionSubject::Command(command),
        ReviewDecision::Approved,
        history_cell::ApprovalDecisionActor::User,
    );
    let lines = cell.display_lines(/*width*/ 28);
    let rendered: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    let expected = vec![
        "✔ You approved codex to run".to_string(),
        "  git add tui/src/render/".to_string(),
        "  mod.rs tui/src/render/".to_string(),
        "  renderable.rs this time".to_string(),
    ];
    assert_eq!(rendered, expected);
}

#[test]
fn exec_history_cell_does_not_render_blank_action_for_empty_command() {
    let approved = history_cell::new_approval_decision_cell(
        history_cell::ApprovalDecisionSubject::Command(Vec::new()),
        ReviewDecision::Approved,
        history_cell::ApprovalDecisionActor::User,
    );
    assert_eq!(
        render_history_cell_lines(approved.as_ref(), /*width*/ 80),
        vec!["✔ You approved this request this time".to_string()]
    );

    let approved_for_session = history_cell::new_approval_decision_cell(
        history_cell::ApprovalDecisionSubject::Command(Vec::new()),
        ReviewDecision::ApprovedForSession,
        history_cell::ApprovalDecisionActor::User,
    );
    assert_eq!(
        render_history_cell_lines(approved_for_session.as_ref(), /*width*/ 80),
        vec!["✔ You approved this request every time this session".to_string()]
    );
}

#[test]
fn network_access_command_history_uses_target_without_structured_context() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = make_overlay(
        ApprovalRequest::Exec(ExecApprovalRequest {
            kind: Default::default(),
            thread_id: ThreadId::new(),
            thread_label: None,
            id: "test".into(),
            environment_id: None,
            command: vec![
                "network-access".to_string(),
                "https://example.com:8443".to_string(),
            ],
            reason: None,
            available_decisions: vec![
                CommandExecutionApprovalDecision::Accept,
                CommandExecutionApprovalDecision::Cancel,
            ],
            network_approval_context: None,
            additional_permissions: None,
        }),
        tx,
        Features::with_defaults(),
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

    let mut decision = None;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            decision = Some(cell);
            break;
        }
    }
    let decision = decision.expect("expected decision cell in history");
    assert_eq!(
        render_history_cell_lines(decision.as_ref(), /*width*/ 80),
        vec![
            "✔ You approved codex network access to https://example.com:8443 this time".to_string(),
        ]
    );
}

#[test]
fn esc_cancels_mcp_elicitation() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = make_overlay(make_elicitation_request(), tx, Features::with_defaults());

    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    let mut decision = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ResolveElicitation { decision: d, .. },
            ..
        } = ev
        {
            decision = Some(d);
            break;
        }
    }
    assert_eq!(decision, Some(McpServerElicitationAction::Cancel));
}

#[test]
fn esc_still_cancels_elicitation_with_custom_overlap() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.approval.decline = vec![
        key_hint::plain(KeyCode::Esc),
        key_hint::plain(KeyCode::Char('n')),
    ];
    keymap.approval.cancel = vec![key_hint::plain(KeyCode::Char('x'))];

    let mut view = make_overlay_with_keymap(
        make_elicitation_request(),
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let mut esc_decision = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ResolveElicitation { decision, .. },
            ..
        } = ev
        {
            esc_decision = Some(decision);
            break;
        }
    }
    assert_eq!(esc_decision, Some(McpServerElicitationAction::Cancel));

    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.approval.decline = vec![
        key_hint::plain(KeyCode::Esc),
        key_hint::plain(KeyCode::Char('n')),
    ];
    keymap.approval.cancel = vec![key_hint::plain(KeyCode::Char('x'))];

    let mut view = make_overlay_with_keymap(
        make_elicitation_request(),
        tx,
        Features::with_defaults(),
        keymap.approval,
        keymap.list,
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let mut n_decision = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ResolveElicitation { decision, .. },
            ..
        } = ev
        {
            n_decision = Some(decision);
            break;
        }
    }
    assert_eq!(n_decision, Some(McpServerElicitationAction::Decline));
}

#[test]
fn enter_sets_last_selected_index_without_dismissing() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = make_overlay(make_exec_request(), tx, Features::with_defaults());
    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        view.is_complete(),
        "exec approval should complete without queued requests"
    );

    let mut decision = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            op: Op::ExecApproval { decision: d, .. },
            ..
        } = ev
        {
            decision = Some(d);
            break;
        }
    }
    assert_eq!(decision, Some(CommandExecutionApprovalDecision::Accept));
}
