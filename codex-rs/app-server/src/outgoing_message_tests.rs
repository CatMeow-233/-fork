use std::time::Duration;

use codex_app_server_protocol::AccountLoginCompletedNotification;
use codex_app_server_protocol::AccountRateLimitsUpdatedNotification;
use codex_app_server_protocol::AccountUpdatedNotification;
use codex_app_server_protocol::ApplyPatchApprovalParams;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::DynamicToolCallParams;
use codex_app_server_protocol::FileChangeRequestApprovalParams;
use codex_app_server_protocol::GuardianWarningNotification;
use codex_app_server_protocol::ModelRerouteReason;
use codex_app_server_protocol::ModelReroutedNotification;
use codex_app_server_protocol::ModelVerification;
use codex_app_server_protocol::ModelVerificationNotification;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use codex_app_server_protocol::ServerResponse;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::TurnModerationMetadataNotification;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use tokio::time::timeout;
use uuid::Uuid;

use super::*;

#[test]
fn verify_server_notification_serialization() {
    let notification =
        ServerNotification::AccountLoginCompleted(AccountLoginCompletedNotification {
            login_id: Some(Uuid::nil().to_string()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        });

    let jsonrpc_notification = OutgoingMessage::AppServerNotification(ServerNotificationEnvelope {
        notification,
        emitted_at_ms: Some(1_234),
    });
    assert_eq!(
        json!({
            "method": "account/login/completed",
            "params": {
                "loginId": Uuid::nil().to_string(),
                "success": true,
                "error": null,
                "onboardingEntrypoint": null,
            },
            "emittedAtMs": 1_234,
        }),
        serde_json::to_value(jsonrpc_notification)
            .expect("ensure the strum macros serialize the method field correctly"),
        "ensure the strum macros serialize the method field correctly"
    );
}

#[test]
fn verify_account_login_completed_notification_serialization() {
    let notification =
        ServerNotification::AccountLoginCompleted(AccountLoginCompletedNotification {
            login_id: Some(Uuid::nil().to_string()),
            success: true,
            error: None,
            onboarding_entrypoint: None,
        });

    assert_eq!(
        json!({
            "method": "account/login/completed",
            "params": {
                "loginId": Uuid::nil().to_string(),
                "success": true,
                "error": null,
                "onboardingEntrypoint": null,
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_account_rate_limits_notification_serialization() {
    let notification =
        ServerNotification::AccountRateLimitsUpdated(AccountRateLimitsUpdatedNotification {
            rate_limits: RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                normal_model_slug: None,
                primary: Some(RateLimitWindow {
                    used_percent: 25,
                    window_duration_mins: Some(15),
                    resets_at: Some(123),
                }),
                secondary: None,
                credits: None,
                individual_limit: None,
                spend_control_reached: None,
                plan_type: Some(PlanType::SelfServeBusinessProLite),
                rate_limit_reached_type: None,
            },
        });

    assert_eq!(
        json!({
            "method": "account/rateLimits/updated",
            "params": {
                    "rateLimits": {
                    "limitId": "codex",
                    "limitName": null,
                    "normalModelSlug": null,
                    "primary": {
                        "usedPercent": 25,
                        "windowDurationMins": 15,
                        "resetsAt": 123
                    },
                    "secondary": null,
                    "credits": null,
                    "individualLimit": null,
                    "spendControlReached": null,
                    "planType": "self_serve_business_prolite",
                    "rateLimitReachedType": null
                }
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_account_updated_notification_serialization() {
    let notification = ServerNotification::AccountUpdated(AccountUpdatedNotification {
        auth_mode: Some(AuthMode::Chatgpt),
        plan_type: Some(PlanType::SelfServeBusinessProLite),
    });

    assert_eq!(
        json!({
            "method": "account/updated",
            "params": {
                "authMode": "chatgpt",
                "planType": "self_serve_business_prolite"
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_config_warning_notification_serialization() {
    let notification = ServerNotification::ConfigWarning(ConfigWarningNotification {
        summary: "Config error: using defaults".to_string(),
        details: Some("error loading config: bad config".to_string()),
        path: None,
        range: None,
    });

    assert_eq!(
        json!( {
            "method": "configWarning",
            "params": {
                "summary": "Config error: using defaults",
                "details": "error loading config: bad config",
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_guardian_warning_notification_serialization() {
    let notification = ServerNotification::GuardianWarning(GuardianWarningNotification {
        thread_id: "thread-1".to_string(),
        message: "Automatic approval review denied the requested action.".to_string(),
    });

    assert_eq!(
        json!({
            "method": "guardianWarning",
            "params": {
                "threadId": "thread-1",
                "message": "Automatic approval review denied the requested action.",
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_model_rerouted_notification_serialization() {
    let notification = ServerNotification::ModelRerouted(ModelReroutedNotification {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        from_model: "gpt-5.3-codex".to_string(),
        to_model: "gpt-5.2".to_string(),
        reason: ModelRerouteReason::HighRiskCyberActivity,
    });

    assert_eq!(
        json!({
            "method": "model/rerouted",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "fromModel": "gpt-5.3-codex",
                "toModel": "gpt-5.2",
                "reason": "highRiskCyberActivity",
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_model_verification_notification_serialization() {
    let notification = ServerNotification::ModelVerification(ModelVerificationNotification {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        verifications: vec![ModelVerification::TrustedAccessForCyber],
    });

    assert_eq!(
        json!({
            "method": "model/verification",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "verifications": ["trustedAccessForCyber"],
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn verify_turn_moderation_metadata_notification_serialization() {
    let notification =
        ServerNotification::TurnModerationMetadata(TurnModerationMetadataNotification {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            metadata: json!({"presentation": "inline"}),
        });

    assert_eq!(
        json!({
            "method": "turn/moderationMetadata",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "metadata": {"presentation": "inline"},
            },
        }),
        serde_json::to_value(notification).expect("ensure the notification serializes correctly"),
        "ensure the notification serializes correctly"
    );
}

#[test]
fn server_request_response_from_result_decodes_typed_response() {
    let request = ServerRequest::CommandExecutionRequestApproval {
        request_id: RequestId::Integer(7),
        params: CommandExecutionRequestApprovalParams {
            kind: Default::default(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            item_id: "item-1".to_string(),
            started_at_ms: 0,
            approval_id: None,
            environment_id: None,
            reason: None,
            network_approval_context: None,
            command: Some("echo hi".to_string()),
            cwd: None,
            command_actions: None,
            additional_permissions: None,
            proposed_execpolicy_amendment: None,
            proposed_network_policy_amendments: None,
            available_decisions: None,
        },
    };

    let response = request
        .response_from_result(json!({
            "decision": "acceptForSession",
        }))
        .expect("decode typed server response");

    let ServerResponse::CommandExecutionRequestApproval {
        request_id,
        response,
    } = response
    else {
        panic!("expected command execution approval response");
    };
    assert_eq!(request_id, RequestId::Integer(7));
    assert_eq!(
        response.decision,
        CommandExecutionApprovalDecision::AcceptForSession
    );
}
#[tokio::test]
async fn send_response_routes_to_target_connection() {
    let (tx, mut rx) = mpsc::channel::<OutgoingEnvelope>(4);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    let request_id = ConnectionRequestId {
        connection_id: ConnectionId(42),
        request_id: RequestId::Integer(7),
    };

    outgoing
        .send_response(
            request_id.clone(),
            ClientResponsePayload::ThreadArchive(
                codex_app_server_protocol::ThreadArchiveResponse {},
            ),
        )
        .await;

    let envelope = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("should receive envelope before timeout")
        .expect("channel should contain one message");

    match envelope {
        OutgoingEnvelope::ToConnection {
            connection_id,
            message,
            ..
        } => {
            assert_eq!(connection_id, ConnectionId(42));
            let OutgoingMessage::Response(response) = message else {
                panic!("expected response message");
            };
            assert_eq!(response.id, request_id.request_id);
            assert_eq!(
                serde_json::to_value(response.result).expect("result should serialize"),
                json!({})
            );
        }
        other => panic!("expected targeted response envelope, got: {other:?}"),
    }
}

#[tokio::test]
async fn send_response_clears_registered_request_context() {
    let (tx, _rx) = mpsc::channel::<OutgoingEnvelope>(4);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    let request_id = ConnectionRequestId {
        connection_id: ConnectionId(42),
        request_id: RequestId::Integer(7),
    };

    outgoing
        .register_request_context(RequestContext::new(
            request_id.clone(),
            "thread/start",
            tracing::info_span!("app_server.request", rpc.method = "thread/start"),
            /*parent_trace*/ None,
        ))
        .await;
    assert_eq!(outgoing.request_context_count().await, 1);

    outgoing
        .send_response(
            request_id,
            ClientResponsePayload::ThreadArchive(
                codex_app_server_protocol::ThreadArchiveResponse {},
            ),
        )
        .await;

    assert_eq!(outgoing.request_context_count().await, 0);
}

#[tokio::test]
async fn send_error_routes_to_target_connection() {
    let (tx, mut rx) = mpsc::channel::<OutgoingEnvelope>(4);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    let request_id = ConnectionRequestId {
        connection_id: ConnectionId(9),
        request_id: RequestId::Integer(3),
    };
    let error = internal_error("boom");

    outgoing.send_error(request_id.clone(), error.clone()).await;

    let envelope = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("should receive envelope before timeout")
        .expect("channel should contain one message");

    match envelope {
        OutgoingEnvelope::ToConnection {
            connection_id,
            message,
            ..
        } => {
            assert_eq!(connection_id, ConnectionId(9));
            let OutgoingMessage::Error(outgoing_error) = message else {
                panic!("expected error message");
            };
            assert_eq!(outgoing_error.id, RequestId::Integer(3));
            assert_eq!(outgoing_error.error, error);
        }
        other => panic!("expected targeted error envelope, got: {other:?}"),
    }
}

/// Empty target lists broadcast; explicit targets preserve order, payload, and one timestamp.
#[tokio::test]
async fn send_server_notification_to_connections_reuses_timestamp() {
    for connection_ids in [
        vec![],
        vec![ConnectionId(1)],
        vec![ConnectionId(1), ConnectionId(2)],
    ] {
        let (tx, mut rx) = mpsc::channel::<OutgoingEnvelope>(2);
        let outgoing =
            OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
        let notification = ServerNotification::ConfigWarning(ConfigWarningNotification {
            summary: "test".to_string(),
            details: None,
            path: None,
            range: None,
        });
        let expected = serde_json::to_value(&notification).expect("serialize notification");

        outgoing
            .send_server_notification_to_connections(&connection_ids, notification)
            .await;

        let mut timestamps = Vec::new();
        for index in 0..connection_ids.len().max(1) {
            let message = match rx.try_recv().expect("notification should be queued") {
                OutgoingEnvelope::Broadcast { message } => {
                    assert!(connection_ids.is_empty());
                    message
                }
                OutgoingEnvelope::ToConnection {
                    connection_id,
                    message,
                    write_complete_tx,
                } => {
                    assert_eq!(connection_id, connection_ids[index]);
                    assert!(write_complete_tx.is_none());
                    message
                }
            };
            let OutgoingMessage::AppServerNotification(envelope) = message else {
                panic!("expected server notification");
            };
            assert_eq!(
                serde_json::to_value(&envelope.notification).unwrap(),
                expected
            );
            timestamps.push(
                envelope
                    .emitted_at_ms
                    .expect("notification has a timestamp"),
            );
        }
        assert!(
            timestamps
                .iter()
                .all(|timestamp| *timestamp == timestamps[0])
        );
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn send_server_notification_to_connection_and_wait_tracks_write_completion() {
    let (tx, mut rx) = mpsc::channel::<OutgoingEnvelope>(4);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    let send_task = tokio::spawn(async move {
        outgoing
            .send_server_notification_to_connection_and_wait(
                ConnectionId(42),
                ServerNotification::ModelRerouted(ModelReroutedNotification {
                    thread_id: "thread-1".to_string(),
                    turn_id: "turn-1".to_string(),
                    from_model: "gpt-5.3-codex".to_string(),
                    to_model: "gpt-5.2".to_string(),
                    reason: ModelRerouteReason::HighRiskCyberActivity,
                }),
            )
            .await
    });

    let envelope = timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("should receive envelope before timeout")
        .expect("channel should contain one message");
    let OutgoingEnvelope::ToConnection {
        connection_id,
        message,
        write_complete_tx,
    } = envelope
    else {
        panic!("expected targeted server notification envelope");
    };
    assert_eq!(connection_id, ConnectionId(42));
    let OutgoingMessage::AppServerNotification(envelope) = message else {
        panic!("expected app-server notification");
    };
    assert!(
        envelope
            .emitted_at_ms
            .is_some_and(|emitted_at_ms| emitted_at_ms > 0)
    );
    write_complete_tx
        .expect("write completion sender should be attached")
        .send(())
        .expect("receiver should still be waiting");

    timeout(Duration::from_secs(1), send_task)
        .await
        .expect("send task should finish after write completion is signaled")
        .expect("send task should not panic");
}

#[tokio::test]
async fn connection_closed_clears_registered_request_contexts() {
    let (tx, _rx) = mpsc::channel::<OutgoingEnvelope>(4);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());
    let closed_connection_request = ConnectionRequestId {
        connection_id: ConnectionId(9),
        request_id: RequestId::Integer(3),
    };
    let open_connection_request = ConnectionRequestId {
        connection_id: ConnectionId(10),
        request_id: RequestId::Integer(4),
    };

    outgoing
        .register_request_context(RequestContext::new(
            closed_connection_request,
            "turn/interrupt",
            tracing::info_span!("app_server.request", rpc.method = "turn/interrupt"),
            /*parent_trace*/ None,
        ))
        .await;
    outgoing
        .register_request_context(RequestContext::new(
            open_connection_request,
            "turn/start",
            tracing::info_span!("app_server.request", rpc.method = "turn/start"),
            /*parent_trace*/ None,
        ))
        .await;
    assert_eq!(outgoing.request_context_count().await, 2);

    outgoing.connection_closed(ConnectionId(9)).await;

    assert_eq!(outgoing.request_context_count().await, 1);
}

#[tokio::test]
async fn notify_client_error_forwards_error_to_waiter() {
    let (tx, _rx) = mpsc::channel::<OutgoingEnvelope>(4);
    let outgoing =
        OutgoingMessageSender::new(tx, codex_analytics::AnalyticsEventsClient::disabled());

    let (request_id, wait_for_result) = outgoing
        .send_request(ServerRequestPayload::ApplyPatchApproval(
            ApplyPatchApprovalParams {
                conversation_id: ThreadId::new(),
                call_id: "call-id".to_string(),
                file_changes: HashMap::new(),
                reason: None,
                grant_root: None,
            },
        ))
        .await;

    let error = internal_error("refresh failed");

    outgoing
        .notify_client_error(ConnectionId(1), request_id, error.clone())
        .await;

    let result = timeout(Duration::from_secs(1), wait_for_result)
        .await
        .expect("wait should not time out")
        .expect("waiter should receive a callback");
    assert_eq!(result, Err(error));
}

#[tokio::test]
async fn pending_requests_for_thread_returns_thread_requests_in_request_id_order() {
    let (tx, _rx) = mpsc::channel::<OutgoingEnvelope>(8);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let thread_id = ThreadId::new();
    let thread_outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![ConnectionId(1)], thread_id);

    let (dynamic_tool_request_id, _dynamic_tool_waiter) = thread_outgoing
        .send_request(ServerRequestPayload::DynamicToolCall(
            DynamicToolCallParams {
                thread_id: thread_id.to_string(),
                turn_id: "turn-1".to_string(),
                call_id: "call-0".to_string(),
                namespace: None,
                tool: "tool".to_string(),
                arguments: json!({}),
            },
        ))
        .await;
    let (first_request_id, _first_waiter) = thread_outgoing
        .send_request(ServerRequestPayload::ToolRequestUserInput(
            ToolRequestUserInputParams {
                thread_id: thread_id.to_string(),
                turn_id: "turn-1".to_string(),
                item_id: "call-1".to_string(),
                questions: vec![],
                is_blocking: true,
                auto_resolution_ms: None,
            },
        ))
        .await;
    let (second_request_id, _second_waiter) = thread_outgoing
        .send_request(ServerRequestPayload::FileChangeRequestApproval(
            FileChangeRequestApprovalParams {
                thread_id: thread_id.to_string(),
                turn_id: "turn-1".to_string(),
                item_id: "call-2".to_string(),
                started_at_ms: 0,
                reason: None,
                grant_root: None,
            },
        ))
        .await;
    let pending_requests = outgoing.pending_requests_for_thread(thread_id).await;
    assert_eq!(
        pending_requests
            .iter()
            .map(ServerRequest::id)
            .collect::<Vec<_>>(),
        vec![
            &dynamic_tool_request_id,
            &first_request_id,
            &second_request_id
        ]
    );
}

#[tokio::test]
async fn cancel_requests_for_thread_cancels_all_thread_requests() {
    let (tx, _rx) = mpsc::channel::<OutgoingEnvelope>(8);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let thread_id = ThreadId::new();
    let thread_outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing.clone(), vec![ConnectionId(1)], thread_id);

    let (_dynamic_tool_request_id, dynamic_tool_waiter) = thread_outgoing
        .send_request(ServerRequestPayload::DynamicToolCall(
            DynamicToolCallParams {
                thread_id: thread_id.to_string(),
                turn_id: "turn-1".to_string(),
                call_id: "call-0".to_string(),
                namespace: None,
                tool: "tool".to_string(),
                arguments: json!({}),
            },
        ))
        .await;
    let (_request_id, user_input_waiter) = thread_outgoing
        .send_request(ServerRequestPayload::ToolRequestUserInput(
            ToolRequestUserInputParams {
                thread_id: thread_id.to_string(),
                turn_id: "turn-1".to_string(),
                item_id: "call-1".to_string(),
                questions: vec![],
                is_blocking: true,
                auto_resolution_ms: None,
            },
        ))
        .await;
    let error = internal_error("tracked request cancelled");

    outgoing
        .cancel_requests_for_thread(thread_id, Some(error.clone()))
        .await;

    let dynamic_tool_result = timeout(Duration::from_secs(1), dynamic_tool_waiter)
        .await
        .expect("dynamic tool waiter should resolve")
        .expect("dynamic tool waiter should receive a callback");
    let user_input_result = timeout(Duration::from_secs(1), user_input_waiter)
        .await
        .expect("user input waiter should resolve")
        .expect("user input waiter should receive a callback");
    assert_eq!(dynamic_tool_result, Err(error.clone()));
    assert_eq!(user_input_result, Err(error));
    assert!(
        outgoing
            .pending_requests_for_thread(thread_id)
            .await
            .is_empty()
    );
}
