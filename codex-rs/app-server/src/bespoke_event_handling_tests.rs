use super::*;
use crate::CHANNEL_CAPACITY;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use codex_app_server_protocol::AutoReviewDecisionSource;
use codex_app_server_protocol::GuardianApprovalReviewStatus;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadStatus;
use codex_app_server_protocol::TurnPlanStepStatus;
use codex_login::CodexAuth;
use codex_protocol::AgentPath;
use codex_protocol::items::AgentMessageContent as CoreAgentMessageContent;
use codex_protocol::items::AgentMessageItem as CoreAgentMessageItem;
use codex_protocol::items::DynamicToolCallItem;
use codex_protocol::items::DynamicToolCallStatus as CoreDynamicToolCallStatus;
use codex_protocol::items::SubAgentActivityItem;
use codex_protocol::items::TurnItem as CoreTurnItem;
use codex_protocol::models::FileSystemPermissions as CoreFileSystemPermissions;
use codex_protocol::models::NetworkPermissions as CoreNetworkPermissions;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use codex_protocol::protocol::AuthRecoveryEvent;
use codex_protocol::protocol::CreditsSnapshot;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::GuardianAssessmentEvent;
use codex_protocol::protocol::GuardianAssessmentStatus;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use core_test_support::load_default_config_for_test;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::sync::mpsc;

fn new_thread_state() -> Arc<Mutex<ThreadState>> {
    Arc::new(Mutex::new(ThreadState::default()))
}

const TEST_TURN_COMPLETED_AT: i64 = 1_716_000_456;
const TEST_TURN_DURATION_MS: i64 = 1_234;

async fn recv_broadcast_message(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
) -> Result<OutgoingMessage> {
    let envelope = rx
        .recv()
        .await
        .ok_or_else(|| anyhow!("should send one message"))?;
    match envelope {
        OutgoingEnvelope::Broadcast { message } => Ok(message),
        OutgoingEnvelope::ToConnection { message, .. } => Ok(message),
    }
}

async fn recv_broadcast_notification(
    rx: &mut mpsc::Receiver<OutgoingEnvelope>,
) -> Result<ServerNotification> {
    let message = recv_broadcast_message(rx).await?;
    let OutgoingMessage::AppServerNotification(envelope) = message else {
        bail!("unexpected message: {message:?}");
    };
    Ok(envelope.notification)
}

fn turn_complete_event(turn_id: &str) -> TurnCompleteEvent {
    TurnCompleteEvent {
        root_turn_id: None,
        turn_id: turn_id.to_string(),
        started_at: None,
        last_agent_message: None,
        error: None,
        completed_at: Some(TEST_TURN_COMPLETED_AT),
        duration_ms: Some(TEST_TURN_DURATION_MS),
        time_to_first_token_ms: None,
    }
}

fn turn_aborted_event(turn_id: &str) -> TurnAbortedEvent {
    TurnAbortedEvent {
        root_turn_id: None,
        turn_id: Some(turn_id.to_string()),
        started_at: None,
        reason: codex_protocol::protocol::TurnAbortReason::Interrupted,
        error: None,
        completed_at: Some(TEST_TURN_COMPLETED_AT),
        duration_ms: Some(TEST_TURN_DURATION_MS),
    }
}

fn command_execution_completion_item(command: &str) -> CommandExecutionCompletionItem {
    CommandExecutionCompletionItem {
        model_context: None,
        plugin_id: Some("sample@openai-curated".to_string()),
        script_path: Some("scripts/run.py".to_string()),
        command: command.to_string(),
        cwd: test_path_buf("/tmp").abs().into(),
        command_actions: vec![V2ParsedCommand::Unknown {
            command: command.to_string(),
        }],
    }
}

fn guardian_command_assessment(
    id: &str,
    turn_id: &str,
    status: GuardianAssessmentStatus,
) -> GuardianAssessmentEvent {
    let (risk_level, user_authorization, rationale) = match status {
        GuardianAssessmentStatus::InProgress => (None, None, None),
        GuardianAssessmentStatus::Approved => (
            Some(codex_protocol::protocol::GuardianRiskLevel::Low),
            Some(codex_protocol::protocol::GuardianUserAuthorization::High),
            Some("looks safe".to_string()),
        ),
        GuardianAssessmentStatus::Denied => (
            Some(codex_protocol::protocol::GuardianRiskLevel::High),
            Some(codex_protocol::protocol::GuardianUserAuthorization::Low),
            Some("too risky".to_string()),
        ),
        GuardianAssessmentStatus::TimedOut => (None, None, Some("review timed out".to_string())),
        GuardianAssessmentStatus::Aborted => (None, None, None),
    };
    GuardianAssessmentEvent {
        review_reason: None,
        model_context: None,
        id: format!("review-{id}"),
        target_item_id: Some(id.to_string()),
        plugin_id: Some("sample@openai-curated".to_string()),
        script_path: Some("scripts/run.py".to_string()),
        turn_id: turn_id.to_string(),
        started_at_ms: 1_000,
        completed_at_ms: (!matches!(status, GuardianAssessmentStatus::InProgress)).then_some(1_042),
        status,
        risk_level,
        user_authorization,
        rationale,
        decision_source: if matches!(status, GuardianAssessmentStatus::InProgress) {
            None
        } else {
            Some(codex_protocol::protocol::GuardianAssessmentDecisionSource::Agent)
        },
        action: serde_json::from_value(json!({
            "type": "command",
            "source": "shell",
            "command": format!("rm -f /tmp/{id}.sqlite"),
            "cwd": test_path_buf("/tmp"),
        }))
        .expect("guardian action"),
    }
}

struct GuardianAssessmentTestContext {
    conversation_id: ThreadId,
    conversation: Arc<CodexThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    thread_state: Arc<Mutex<ThreadState>>,
    thread_watch_manager: ThreadWatchManager,
}

impl GuardianAssessmentTestContext {
    async fn apply_guardian_assessment_event(&self, assessment: GuardianAssessmentEvent) {
        let event_turn_id = assessment.turn_id.clone();
        apply_bespoke_event_handling(
            Event {
                id: event_turn_id,
                msg: EventMsg::GuardianAssessment(assessment),
            },
            self.conversation_id,
            self.conversation.clone(),
            self.thread_manager.clone(),
            self.outgoing.clone(),
            self.thread_state.clone(),
            self.thread_watch_manager.clone(),
        )
        .await;
    }
}

#[test]
fn guardian_assessment_started_uses_event_turn_id_fallback() {
    let conversation_id = ThreadId::new();
    let action = codex_protocol::protocol::GuardianAssessmentAction::Command {
        source: codex_protocol::protocol::GuardianCommandSource::Shell,
        command: "rm -rf /tmp/example.sqlite".to_string(),
        cwd: test_path_buf("/tmp").abs().into(),
    };
    let notification = guardian_auto_approval_review_notification(
        &conversation_id,
        "turn-from-event",
        &GuardianAssessmentEvent {
            review_reason: None,
            model_context: None,
            id: "review-1".to_string(),
            target_item_id: Some("item-1".to_string()),
            plugin_id: None,
            script_path: None,
            turn_id: String::new(),
            started_at_ms: 1_000,
            completed_at_ms: None,
            status: codex_protocol::protocol::GuardianAssessmentStatus::InProgress,
            risk_level: None,
            user_authorization: None,
            rationale: None,
            decision_source: None,
            action: action.clone(),
        },
    );

    match notification {
        ServerNotification::ItemGuardianApprovalReviewStarted(payload) => {
            assert_eq!(payload.thread_id, conversation_id.to_string());
            assert_eq!(payload.turn_id, "turn-from-event");
            assert_eq!(payload.started_at_ms, 1_000);
            assert_eq!(payload.review_id, "review-1");
            assert_eq!(payload.target_item_id.as_deref(), Some("item-1"));
            assert_eq!(
                payload.review.status,
                GuardianApprovalReviewStatus::InProgress
            );
            assert_eq!(payload.review.risk_level, None);
            assert_eq!(payload.review.user_authorization, None);
            assert_eq!(payload.review.rationale, None);
            assert_eq!(payload.action, action.into());
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

#[test]
fn guardian_assessment_completed_emits_review_payload() {
    let conversation_id = ThreadId::new();
    let action = codex_protocol::protocol::GuardianAssessmentAction::Command {
        source: codex_protocol::protocol::GuardianCommandSource::Shell,
        command: "rm -rf /tmp/example.sqlite".to_string(),
        cwd: test_path_buf("/tmp").abs().into(),
    };
    let notification = guardian_auto_approval_review_notification(
        &conversation_id,
        "turn-from-event",
        &GuardianAssessmentEvent {
            review_reason: None,
            model_context: None,
            id: "review-2".to_string(),
            target_item_id: Some("item-2".to_string()),
            plugin_id: None,
            script_path: None,
            turn_id: "turn-from-assessment".to_string(),
            started_at_ms: 1_000,
            completed_at_ms: Some(1_042),
            status: codex_protocol::protocol::GuardianAssessmentStatus::Denied,
            risk_level: Some(codex_protocol::protocol::GuardianRiskLevel::High),
            user_authorization: Some(codex_protocol::protocol::GuardianUserAuthorization::Low),
            rationale: Some("too risky".to_string()),
            decision_source: Some(
                codex_protocol::protocol::GuardianAssessmentDecisionSource::Agent,
            ),
            action: action.clone(),
        },
    );

    match notification {
        ServerNotification::ItemGuardianApprovalReviewCompleted(payload) => {
            assert_eq!(payload.thread_id, conversation_id.to_string());
            assert_eq!(payload.turn_id, "turn-from-assessment");
            assert_eq!(payload.started_at_ms, 1_000);
            assert_eq!(payload.completed_at_ms, 1_042);
            assert_eq!(payload.review_id, "review-2");
            assert_eq!(payload.target_item_id.as_deref(), Some("item-2"));
            assert_eq!(payload.decision_source, AutoReviewDecisionSource::Agent);
            assert_eq!(payload.review.status, GuardianApprovalReviewStatus::Denied);
            assert_eq!(
                payload.review.risk_level,
                Some(codex_app_server_protocol::GuardianRiskLevel::High)
            );
            assert_eq!(
                payload.review.user_authorization,
                Some(codex_app_server_protocol::GuardianUserAuthorization::Low)
            );
            assert_eq!(payload.review.rationale.as_deref(), Some("too risky"));
            assert_eq!(payload.action, action.into());
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

#[test]
fn guardian_assessment_aborted_emits_completed_review_payload() {
    let conversation_id = ThreadId::new();
    let action = codex_protocol::protocol::GuardianAssessmentAction::NetworkAccess {
        target: "api.openai.com:443".to_string(),
        host: "api.openai.com".to_string(),
        protocol: codex_protocol::protocol::NetworkApprovalProtocol::Https,
        port: 443,
    };
    let notification = guardian_auto_approval_review_notification(
        &conversation_id,
        "turn-from-event",
        &GuardianAssessmentEvent {
            review_reason: None,
            model_context: None,
            id: "review-3".to_string(),
            target_item_id: None,
            plugin_id: None,
            script_path: None,
            turn_id: "turn-from-assessment".to_string(),
            started_at_ms: 1_000,
            completed_at_ms: Some(1_042),
            status: codex_protocol::protocol::GuardianAssessmentStatus::Aborted,
            risk_level: None,
            user_authorization: None,
            rationale: None,
            decision_source: Some(
                codex_protocol::protocol::GuardianAssessmentDecisionSource::Agent,
            ),
            action: action.clone(),
        },
    );

    match notification {
        ServerNotification::ItemGuardianApprovalReviewCompleted(payload) => {
            assert_eq!(payload.thread_id, conversation_id.to_string());
            assert_eq!(payload.turn_id, "turn-from-assessment");
            assert_eq!(payload.review_id, "review-3");
            assert_eq!(payload.target_item_id, None);
            assert_eq!(payload.decision_source, AutoReviewDecisionSource::Agent);
            assert_eq!(payload.review.status, GuardianApprovalReviewStatus::Aborted);
            assert_eq!(payload.review.risk_level, None);
            assert_eq!(payload.review.user_authorization, None);
            assert_eq!(payload.review.rationale, None);
            assert_eq!(payload.action, action.into());
        }
        other => panic!("unexpected notification: {other:?}"),
    }
}

#[tokio::test]
async fn command_execution_started_helper_emits_once() -> Result<()> {
    let conversation_id = ThreadId::new();
    let thread_state = new_thread_state();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());
    let completion_item = command_execution_completion_item("printf hi");

    let first_start = start_command_execution_item(
        &conversation_id,
        "turn-1".to_string(),
        "cmd-1".to_string(),
        /*started_at_ms*/ 1_000,
        &completion_item,
        &outgoing,
        &thread_state,
    )
    .await;
    assert!(first_start);

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::ItemStarted(payload) => {
            assert_eq!(payload.item.started_at_ms, Some(1_000));
            assert_eq!(payload.thread_id, conversation_id.to_string());
            assert_eq!(payload.turn_id, "turn-1");
            assert_eq!(
                payload.item.payload,
                ThreadItemPayload::CommandExecution {
                    model_context: None,
                    sandbox_type: None,
                    id: "cmd-1".to_string(),
                    plugin_id: completion_item.plugin_id.clone(),
                    script_path: completion_item.script_path.clone(),
                    command: completion_item.command.clone(),
                    cwd: completion_item.cwd.clone(),
                    process_id: None,
                    source: CommandExecutionSource::Agent,
                    status: CommandExecutionStatus::InProgress,
                    command_actions: completion_item.command_actions.clone(),
                    aggregated_output: None,
                    exit_code: None,
                    duration_ms: None,
                }
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }

    let second_start = start_command_execution_item(
        &conversation_id,
        "turn-1".to_string(),
        "cmd-1".to_string(),
        /*started_at_ms*/ 1_100,
        &completion_item,
        &outgoing,
        &thread_state,
    )
    .await;
    assert!(!second_start);
    assert!(rx.try_recv().is_err(), "duplicate start should not emit");
    Ok(())
}

#[tokio::test]
async fn complete_command_execution_item_emits_declined_once_for_pending_command() -> Result<()> {
    let conversation_id = ThreadId::new();
    let thread_state = new_thread_state();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());
    let completion_item = command_execution_completion_item("printf hi");

    start_command_execution_item(
        &conversation_id,
        "turn-1".to_string(),
        "cmd-1".to_string(),
        /*started_at_ms*/ 1_000,
        &completion_item,
        &outgoing,
        &thread_state,
    )
    .await;
    let _started = recv_broadcast_notification(&mut rx).await?;

    complete_command_execution_item(
        &conversation_id,
        "turn-1".to_string(),
        "cmd-1".to_string(),
        completion_item,
        CommandExecutionStatus::Declined,
        /*completed_at_ms*/ None,
        &outgoing,
        &thread_state,
    )
    .await;

    let completed = recv_broadcast_notification(&mut rx).await?;
    match completed {
        ServerNotification::ItemCompleted(payload) => {
            let ThreadItemPayload::CommandExecution {
                id,
                plugin_id,
                script_path,
                status,
                ..
            } = payload.item.payload
            else {
                bail!("expected command execution completion");
            };
            assert_eq!(id, "cmd-1");
            assert_eq!(plugin_id.as_deref(), Some("sample@openai-curated"));
            assert_eq!(script_path.as_deref(), Some("scripts/run.py"));
            assert_eq!(status, CommandExecutionStatus::Declined);
        }
        other => bail!("unexpected message: {other:?}"),
    }

    complete_command_execution_item(
        &conversation_id,
        "turn-1".to_string(),
        "cmd-1".to_string(),
        command_execution_completion_item("printf hi"),
        CommandExecutionStatus::Declined,
        /*completed_at_ms*/ None,
        &outgoing,
        &thread_state,
    )
    .await;
    assert!(
        rx.try_recv().is_err(),
        "completion should not emit after the pending item is cleared"
    );
    Ok(())
}

#[tokio::test]
async fn guardian_command_execution_notifications_wrap_review_lifecycle() -> Result<()> {
    let codex_home = TempDir::new()?;
    let config = load_default_config_for_test(&codex_home).await;
    let thread_manager = Arc::new(
        codex_core::test_support::thread_manager_with_models_provider_and_home(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            config.model_provider.clone(),
            config.codex_home.to_path_buf(),
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        ),
    );
    let codex_core::NewThread {
        thread_id: conversation_id,
        thread: conversation,
        ..
    } = thread_manager
        .start_thread(codex_core::StartThreadOptions::new(config.clone()))
        .await?;
    let thread_state = new_thread_state();
    let thread_watch_manager = ThreadWatchManager::new();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], conversation_id);
    let guardian_context = GuardianAssessmentTestContext {
        conversation_id,
        conversation: conversation.clone(),
        thread_manager: thread_manager.clone(),
        outgoing: outgoing.clone(),
        thread_state: thread_state.clone(),
        thread_watch_manager: thread_watch_manager.clone(),
    };

    guardian_context
        .apply_guardian_assessment_event(guardian_command_assessment(
            "cmd-guardian-approved",
            "turn-guardian-approved",
            GuardianAssessmentStatus::InProgress,
        ))
        .await;
    let first = recv_broadcast_notification(&mut rx).await?;
    match first {
        ServerNotification::ItemStarted(payload) => {
            assert_eq!(payload.turn_id, "turn-guardian-approved");
            let ThreadItemPayload::CommandExecution {
                id,
                plugin_id,
                script_path,
                status,
                ..
            } = payload.item.payload
            else {
                bail!("expected command execution item");
            };
            assert_eq!(id, "cmd-guardian-approved");
            assert_eq!(plugin_id.as_deref(), Some("sample@openai-curated"));
            assert_eq!(script_path.as_deref(), Some("scripts/run.py"));
            assert_eq!(status, CommandExecutionStatus::InProgress);
        }
        other => bail!("unexpected message: {other:?}"),
    }
    let second = recv_broadcast_notification(&mut rx).await?;
    match second {
        ServerNotification::ItemGuardianApprovalReviewStarted(payload) => {
            assert_eq!(payload.review_id, "review-cmd-guardian-approved");
            assert_eq!(
                payload.target_item_id.as_deref(),
                Some("cmd-guardian-approved")
            );
            assert_eq!(
                payload.review.status,
                GuardianApprovalReviewStatus::InProgress
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }

    guardian_context
        .apply_guardian_assessment_event(guardian_command_assessment(
            "cmd-guardian-approved",
            "turn-guardian-approved",
            GuardianAssessmentStatus::Approved,
        ))
        .await;
    let third = recv_broadcast_notification(&mut rx).await?;
    match third {
        ServerNotification::ItemGuardianApprovalReviewCompleted(payload) => {
            assert_eq!(payload.review_id, "review-cmd-guardian-approved");
            assert_eq!(
                payload.target_item_id.as_deref(),
                Some("cmd-guardian-approved")
            );
            assert_eq!(payload.decision_source, AutoReviewDecisionSource::Agent);
            assert_eq!(
                payload.review.status,
                GuardianApprovalReviewStatus::Approved
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }
    assert!(
        rx.try_recv().is_err(),
        "approved review should not complete the command item"
    );

    guardian_context
        .apply_guardian_assessment_event(guardian_command_assessment(
            "cmd-guardian-denied",
            "turn-guardian-denied",
            GuardianAssessmentStatus::InProgress,
        ))
        .await;
    let fourth = recv_broadcast_notification(&mut rx).await?;
    match fourth {
        ServerNotification::ItemStarted(payload) => {
            assert_eq!(payload.turn_id, "turn-guardian-denied");
            let ThreadItemPayload::CommandExecution {
                id,
                plugin_id,
                script_path,
                status,
                ..
            } = payload.item.payload
            else {
                bail!("expected command execution item");
            };
            assert_eq!(id, "cmd-guardian-denied");
            assert_eq!(plugin_id.as_deref(), Some("sample@openai-curated"));
            assert_eq!(script_path.as_deref(), Some("scripts/run.py"));
            assert_eq!(status, CommandExecutionStatus::InProgress);
        }
        other => bail!("unexpected message: {other:?}"),
    }
    let fifth = recv_broadcast_notification(&mut rx).await?;
    match fifth {
        ServerNotification::ItemGuardianApprovalReviewStarted(payload) => {
            assert_eq!(payload.review_id, "review-cmd-guardian-denied");
            assert_eq!(
                payload.target_item_id.as_deref(),
                Some("cmd-guardian-denied")
            );
            assert_eq!(
                payload.review.status,
                GuardianApprovalReviewStatus::InProgress
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }

    guardian_context
        .apply_guardian_assessment_event(guardian_command_assessment(
            "cmd-guardian-denied",
            "turn-guardian-denied",
            GuardianAssessmentStatus::Denied,
        ))
        .await;
    let sixth = recv_broadcast_notification(&mut rx).await?;
    match sixth {
        ServerNotification::ItemGuardianApprovalReviewCompleted(payload) => {
            assert_eq!(payload.review_id, "review-cmd-guardian-denied");
            assert_eq!(
                payload.target_item_id.as_deref(),
                Some("cmd-guardian-denied")
            );
            assert_eq!(payload.decision_source, AutoReviewDecisionSource::Agent);
            assert_eq!(payload.review.status, GuardianApprovalReviewStatus::Denied);
        }
        other => bail!("unexpected message: {other:?}"),
    }
    let seventh = recv_broadcast_notification(&mut rx).await?;
    match seventh {
        ServerNotification::ItemCompleted(payload) => {
            assert_eq!(payload.item.completed_at_ms, Some(1_042));
            let ThreadItemPayload::CommandExecution {
                id,
                plugin_id,
                script_path,
                status,
                ..
            } = payload.item.payload
            else {
                bail!("expected command execution completion");
            };
            assert_eq!(id, "cmd-guardian-denied");
            assert_eq!(plugin_id.as_deref(), Some("sample@openai-curated"));
            assert_eq!(script_path.as_deref(), Some("scripts/run.py"));
            assert_eq!(status, CommandExecutionStatus::Declined);
        }
        other => bail!("unexpected message: {other:?}"),
    }

    let mut missing_target = guardian_command_assessment(
        "cmd-guardian-missing-target",
        "turn-guardian-missing-target",
        GuardianAssessmentStatus::InProgress,
    );
    missing_target.target_item_id = None;
    guardian_context
        .apply_guardian_assessment_event(missing_target)
        .await;
    let eighth = recv_broadcast_notification(&mut rx).await?;
    match eighth {
        ServerNotification::ItemGuardianApprovalReviewStarted(payload) => {
            assert_eq!(payload.review_id, "review-cmd-guardian-missing-target");
            assert_eq!(payload.target_item_id, None);
            assert_eq!(
                payload.review.status,
                GuardianApprovalReviewStatus::InProgress
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }

    assert!(rx.try_recv().is_err(), "no extra messages expected");
    conversation.shutdown_and_wait().await?;
    Ok(())
}

#[test]
fn file_change_accept_for_session_maps_to_approved_for_session() {
    let decision = map_file_change_approval_decision(FileChangeApprovalDecision::AcceptForSession);
    assert_eq!(decision, ReviewDecision::ApprovedForSession);
}

#[test]
fn mcp_server_elicitation_turn_transition_error_maps_to_cancel() {
    let error = JSONRPCErrorError {
        code: -1,
        message: "client request resolved because the turn state was changed".to_string(),
        data: Some(serde_json::json!({ "reason": "turnTransition" })),
    };

    let response = mcp_server_elicitation_response_from_client_result(Ok(Err(error)));

    assert_eq!(
        response,
        McpServerElicitationRequestResponse {
            action: McpServerElicitationAction::Cancel,
            content: None,
            meta: None,
        }
    );
}

#[test]
fn request_permissions_turn_transition_error_is_ignored() {
    let error = JSONRPCErrorError {
        code: -1,
        message: "client request resolved because the turn state was changed".to_string(),
        data: Some(serde_json::json!({ "reason": "turnTransition" })),
    };

    let response = request_permissions_response_from_client_result(Ok(Err(error)))
        .expect("paths should localize");

    assert_eq!(response, None);
}

#[test]
fn request_permissions_response_deserializes_network_and_file_system_grants() {
    let input_path = if cfg!(target_os = "windows") {
        r"C:\tmp\input"
    } else {
        "/tmp/input"
    };
    let output_path = if cfg!(target_os = "windows") {
        r"C:\tmp\output"
    } else {
        "/tmp/output"
    };
    let extra_path = if cfg!(target_os = "windows") {
        r"C:\tmp\extra"
    } else {
        "/tmp/extra"
    };
    let absolute_path = |path: &str| {
        AbsolutePathBuf::try_from(std::path::PathBuf::from(path)).expect("absolute path")
    };
    let cases = vec![
        (
            serde_json::json!({}),
            CoreRequestPermissionProfile::default(),
        ),
        (
            serde_json::json!({
                "network": {
                    "enabled": true,
                },
            }),
            CoreRequestPermissionProfile {
                network: Some(CoreNetworkPermissions {
                    enabled: Some(true),
                }),
                ..CoreRequestPermissionProfile::default()
            },
        ),
        (
            serde_json::json!({
                "fileSystem": {
                    "write": [output_path],
                },
            }),
            CoreRequestPermissionProfile {
                file_system: Some(CoreFileSystemPermissions::from_read_write_roots(
                    /*read*/ None,
                    Some(vec![absolute_path(output_path)]),
                )),
                ..CoreRequestPermissionProfile::default()
            },
        ),
        (
            serde_json::json!({
                "fileSystem": {
                    "read": [input_path],
                    "write": [output_path, extra_path],
                },
                "macos": {
                    "calendar": true,
                },
            }),
            CoreRequestPermissionProfile {
                file_system: Some(CoreFileSystemPermissions::from_read_write_roots(
                    Some(vec![absolute_path(input_path)]),
                    Some(vec![absolute_path(output_path), absolute_path(extra_path)]),
                )),
                ..CoreRequestPermissionProfile::default()
            },
        ),
    ];

    for (granted_permissions, expected_permissions) in cases {
        let response = request_permissions_response_from_client_result(Ok(Ok(serde_json::json!({
            "permissions": granted_permissions,
        }))))
        .expect("paths should localize")
        .expect("response should be accepted");

        assert_eq!(
            response,
            CoreRequestPermissionsResponse {
                permissions: expected_permissions,
                scope: CorePermissionGrantScope::Turn,
                strict_auto_review: false,
            }
        );
    }
}

#[test]
fn request_permissions_response_preserves_session_scope() {
    let response = request_permissions_response_from_client_result(Ok(Ok(serde_json::json!({
        "scope": "session",
        "permissions": {},
    }))))
    .expect("paths should localize")
    .expect("response should be accepted");

    assert_eq!(
        response,
        CoreRequestPermissionsResponse {
            permissions: CoreRequestPermissionProfile::default(),
            scope: CorePermissionGrantScope::Session,
            strict_auto_review: false,
        }
    );
}

#[test]
fn request_permissions_response_rejects_session_scoped_strict_auto_review() {
    let response = request_permissions_response_from_client_result(Ok(Ok(serde_json::json!({
        "scope": "session",
        "strictAutoReview": true,
        "permissions": {
            "network": {
                "enabled": true,
            },
        },
    }))))
    .expect("paths should localize")
    .expect("response should be accepted");

    assert_eq!(
        response,
        CoreRequestPermissionsResponse {
            permissions: CoreRequestPermissionProfile::default(),
            scope: CorePermissionGrantScope::Turn,
            strict_auto_review: false,
        }
    );
}

#[test]
fn request_permissions_response_preserves_turn_scoped_strict_auto_review() {
    let response = request_permissions_response_from_client_result(Ok(Ok(serde_json::json!({
        "strictAutoReview": true,
        "permissions": {
            "network": {
                "enabled": true,
            },
        },
    }))))
    .expect("paths should localize")
    .expect("response should be accepted");

    assert_eq!(response.scope, CorePermissionGrantScope::Turn);
    assert!(response.strict_auto_review);
}

#[tokio::test]
async fn test_handle_error_records_message() -> Result<()> {
    let conversation_id = ThreadId::new();
    let thread_state = new_thread_state();

    handle_error(
        conversation_id,
        TurnError {
            misalignment: None,
            message: "boom".to_string(),
            codex_error_info: Some(V2CodexErrorInfo::InternalServerError),
            additional_details: None,
        },
        &thread_state,
    )
    .await;

    let turn_summary = find_and_remove_turn_summary(conversation_id, &thread_state).await;
    assert_eq!(
        turn_summary.last_error,
        Some(TurnError {
            misalignment: None,
            message: "boom".to_string(),
            codex_error_info: Some(V2CodexErrorInfo::InternalServerError),
            additional_details: None,
        })
    );
    Ok(())
}

#[tokio::test]
async fn turn_started_omits_active_snapshot_items() -> Result<()> {
    let codex_home = TempDir::new()?;
    let config = load_default_config_for_test(&codex_home).await;
    let thread_manager = Arc::new(
        codex_core::test_support::thread_manager_with_models_provider_and_home(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            config.model_provider.clone(),
            config.codex_home.to_path_buf(),
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        ),
    );
    let codex_core::NewThread {
        thread_id: conversation_id,
        thread: conversation,
        ..
    } = thread_manager
        .start_thread(codex_core::StartThreadOptions::new(config.clone()))
        .await?;
    let thread_state = new_thread_state();
    {
        let mut state = thread_state.lock().await;
        state.track_current_turn_event(
            "turn-1",
            &EventMsg::TurnStarted(codex_protocol::protocol::TurnStartedEvent {
                turn_attribution: None,
                turn_id: "turn-1".to_string(),
                root_turn_id: None,
                trace_id: None,
                started_at: Some(42),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        );
        state.track_current_turn_event(
            "turn-1",
            &EventMsg::UserMessage(codex_protocol::protocol::UserMessageEvent {
                client_id: None,
                message: "already tracked".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
                ..Default::default()
            }),
        );
    }
    let thread_watch_manager = ThreadWatchManager::new();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], conversation_id);

    apply_bespoke_event_handling(
        Event {
            id: "turn-1".to_string(),
            msg: EventMsg::TurnStarted(codex_protocol::protocol::TurnStartedEvent {
                turn_attribution: None,
                turn_id: "turn-1".to_string(),
                root_turn_id: None,
                trace_id: None,
                started_at: Some(42),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        },
        conversation_id,
        Arc::clone(&conversation),
        Arc::clone(&thread_manager),
        outgoing.clone(),
        Arc::clone(&thread_state),
        thread_watch_manager.clone(),
    )
    .await;

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnStarted(n) => {
            assert_eq!(n.turn.id, "turn-1");
            assert_eq!(n.turn.items_view, TurnItemsView::NotLoaded);
            assert!(n.turn.items.is_empty());
        }
        other => bail!("unexpected message: {other:?}"),
    }

    for (phase, method, event) in [
        (
            "started",
            "modelProvider/authRecoveryStarted",
            EventMsg::AuthRecoveryStarted as fn(AuthRecoveryEvent) -> EventMsg,
        ),
        (
            "completed",
            "modelProvider/authRecoveryCompleted",
            EventMsg::AuthRecoveryCompleted,
        ),
    ] {
        let message = format!("Authentication recovery {phase}.");
        apply_bespoke_event_handling(
            Event {
                id: "turn-1".to_string(),
                msg: event(AuthRecoveryEvent {
                    provider: "test-provider".to_string(),
                    message: message.clone(),
                }),
            },
            conversation_id,
            Arc::clone(&conversation),
            Arc::clone(&thread_manager),
            outgoing.clone(),
            Arc::clone(&thread_state),
            thread_watch_manager.clone(),
        )
        .await;

        assert_eq!(
            serde_json::to_value(recv_broadcast_notification(&mut rx).await?)?,
            json!({
                "method": method,
                "params": {
                    "threadId": conversation_id.to_string(),
                    "turnId": "turn-1",
                    "provider": "test-provider",
                    "message": message,
                }
            })
        );
    }

    Ok(())
}

#[tokio::test]
async fn interrupted_subagent_activity_removes_missing_thread_watch() -> Result<()> {
    let codex_home = TempDir::new()?;
    let config = load_default_config_for_test(&codex_home).await;
    let thread_manager = Arc::new(
        codex_core::test_support::thread_manager_with_models_provider_and_home(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            config.model_provider.clone(),
            config.codex_home.to_path_buf(),
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        ),
    );
    let codex_core::NewThread {
        thread_id: conversation_id,
        thread: conversation,
        ..
    } = thread_manager
        .start_thread(codex_core::StartThreadOptions::new(config))
        .await?;
    let child_thread_id = ThreadId::new();
    let child_thread_id_string = child_thread_id.to_string();
    let thread_watch_manager = ThreadWatchManager::new();
    thread_watch_manager
        .note_turn_started(&child_thread_id_string)
        .await;
    assert_eq!(thread_watch_manager.running_turn_count().await, 1);
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], conversation_id);

    apply_bespoke_event_handling(
        Event {
            id: "turn-1".to_string(),
            msg: EventMsg::ItemCompleted(ItemCompletedEvent {
                content_token_count: None,
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: CoreTurnItem::SubAgentActivity(SubAgentActivityItem {
                    model: None,
                    reasoning_effort: None,
                    id: "activity-1".to_string(),
                    kind: SubAgentActivityKind::Interrupted,
                    agent_thread_id: child_thread_id,
                    agent_path: AgentPath::try_from("/root/worker")
                        .expect("agent path should parse"),
                }),
                started_at_ms: Some(42),
                completed_at_ms: 42,
            }),
        },
        conversation_id,
        conversation,
        thread_manager,
        outgoing,
        new_thread_state(),
        thread_watch_manager.clone(),
    )
    .await;

    assert_eq!(
        thread_watch_manager
            .loaded_status_for_thread(&child_thread_id_string)
            .await,
        ThreadStatus::NotLoaded
    );
    assert_eq!(thread_watch_manager.running_turn_count().await, 0);
    let message = recv_broadcast_notification(&mut rx).await?;
    let ServerNotification::ItemCompleted(payload) = message else {
        bail!("unexpected message: {message:?}");
    };
    assert_eq!(
        payload,
        ItemCompletedNotification::new(
            conversation_id.to_string(),
            "turn-1".to_string(),
            ThreadItemPayload::SubAgentActivity {
                model: None,
                reasoning_effort: None,
                id: "activity-1".to_string(),
                kind: codex_app_server_protocol::SubAgentActivityKind::Interrupted,
                agent_thread_id: child_thread_id_string,
                agent_path: "/root/worker".to_string(),
            },
            Some(42),
            /*completed_at_ms*/ 42
        )
    );
    Ok(())
}

#[tokio::test]
async fn canonical_dynamic_tool_start_emits_item_and_requests_client() -> Result<()> {
    let codex_home = TempDir::new()?;
    let config = load_default_config_for_test(&codex_home).await;
    let thread_manager = Arc::new(
        codex_core::test_support::thread_manager_with_models_provider_and_home(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            config.model_provider.clone(),
            config.codex_home.to_path_buf(),
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
        ),
    );
    let codex_core::NewThread {
        thread_id: conversation_id,
        thread: conversation,
        ..
    } = thread_manager
        .start_thread(codex_core::StartThreadOptions::new(config))
        .await?;
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], conversation_id);

    apply_bespoke_event_handling(
        Event {
            id: "turn-1".to_string(),
            msg: EventMsg::ItemStarted(ItemStartedEvent {
                thread_id: conversation_id,
                turn_id: "turn-1".to_string(),
                item: CoreTurnItem::DynamicToolCall(DynamicToolCallItem {
                    id: "dynamic-1".to_string(),
                    namespace: Some("apps".to_string()),
                    tool: "lookup".to_string(),
                    arguments: json!({"id": "123"}),
                    status: CoreDynamicToolCallStatus::InProgress,
                    content_items: None,
                    success: None,
                    error: None,
                    duration: None,
                }),
                started_at_ms: 42,
            }),
        },
        conversation_id,
        conversation,
        thread_manager,
        outgoing,
        new_thread_state(),
        ThreadWatchManager::new(),
    )
    .await;

    let item_started = recv_broadcast_notification(&mut rx).await?;
    let ServerNotification::ItemStarted(payload) = item_started else {
        bail!("unexpected message: {item_started:?}");
    };
    assert_eq!(payload.item.id(), "dynamic-1");

    let request = recv_broadcast_message(&mut rx).await?;
    let OutgoingMessage::Request(ServerRequest::DynamicToolCall { params, .. }) = request else {
        bail!("unexpected message: {request:?}");
    };
    assert_eq!(
        params,
        DynamicToolCallParams {
            thread_id: conversation_id.to_string(),
            turn_id: "turn-1".to_string(),
            call_id: "dynamic-1".to_string(),
            namespace: Some("apps".to_string()),
            tool: "lookup".to_string(),
            arguments: json!({"id": "123"}),
        }
    );
    Ok(())
}

#[tokio::test]
async fn test_handle_turn_complete_emits_completed_without_error() -> Result<()> {
    let conversation_id = ThreadId::new();
    let event_turn_id = "complete1".to_string();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());
    let thread_state = new_thread_state();
    let event = turn_complete_event(&event_turn_id);
    {
        let mut state = thread_state.lock().await;
        state.track_current_turn_event(
            &event_turn_id,
            &EventMsg::TurnStarted(codex_protocol::protocol::TurnStartedEvent {
                turn_attribution: None,
                turn_id: event_turn_id.clone(),
                root_turn_id: None,
                trace_id: None,
                started_at: Some(42),
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            }),
        );
        state.track_current_turn_event(
            &event_turn_id,
            &EventMsg::ItemCompleted(ItemCompletedEvent {
                content_token_count: None,
                thread_id: conversation_id,
                turn_id: event_turn_id.clone(),
                item: CoreTurnItem::AgentMessage(CoreAgentMessageItem {
                    moderation: None,
                    id: "msg-1".to_string(),
                    content: vec![
                        CoreAgentMessageContent::Text {
                            text: "complete ".to_string(),
                        },
                        CoreAgentMessageContent::Text {
                            text: "response".to_string(),
                        },
                    ],
                    phase: None,
                    memory_citation: None,
                    delivery: None,
                    questions: None,
                }),
                started_at_ms: Some(42_123),
                completed_at_ms: 42_200,
            }),
        );
        state.track_current_turn_event(
            &event_turn_id,
            &EventMsg::ItemCompleted(ItemCompletedEvent {
                content_token_count: None,
                thread_id: conversation_id,
                turn_id: event_turn_id.clone(),
                item: CoreTurnItem::AgentMessage(CoreAgentMessageItem {
                    moderation: None,
                    id: "msg-2".to_string(),
                    content: vec![CoreAgentMessageContent::Text {
                        text: "  ".to_string(),
                    }],
                    phase: None,
                    memory_citation: None,
                    delivery: None,
                    questions: None,
                }),
                started_at_ms: Some(42_321),
                completed_at_ms: 42_400,
            }),
        );
        state.track_current_turn_event(&event_turn_id, &EventMsg::TurnComplete(event.clone()));
    }

    handle_turn_complete(
        conversation_id,
        event_turn_id.clone(),
        event,
        &outgoing,
        &thread_state,
    )
    .await;

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnCompleted(n) => {
            assert_eq!(n.turn.id, event_turn_id);
            assert_eq!(n.turn.status, TurnStatus::Completed);
            assert_eq!(n.turn.items_view, TurnItemsView::Summary);
            assert!(matches!(
                &n.turn.items[..],
                [ThreadItem { payload: ThreadItemPayload::AgentMessage { id, text, .. }, started_at_ms: Some(42_123), completed_at_ms: Some(42_200) }]
                    if id == "msg-1" && text == "complete response"
            ));
            assert_eq!(n.turn.error, None);
            assert_eq!(n.turn.started_at, Some(42));
            assert_eq!(n.turn.completed_at, Some(TEST_TURN_COMPLETED_AT));
            assert_eq!(n.turn.duration_ms, Some(TEST_TURN_DURATION_MS));
        }
        other => bail!("unexpected message: {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "no extra messages expected");
    Ok(())
}

#[tokio::test]
async fn test_handle_turn_interrupted_emits_interrupted_without_error() -> Result<()> {
    let conversation_id = ThreadId::new();
    let event_turn_id = "interrupt1".to_string();
    let thread_state = new_thread_state();
    handle_error(
        conversation_id,
        TurnError {
            misalignment: None,
            message: "oops".to_string(),
            codex_error_info: None,
            additional_details: None,
        },
        &thread_state,
    )
    .await;
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());

    handle_turn_interrupted(
        conversation_id,
        event_turn_id.clone(),
        TurnAbortedEvent {
            root_turn_id: Some("root-a".to_string()),
            ..turn_aborted_event(&event_turn_id)
        },
        &outgoing,
        &thread_state,
    )
    .await;

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnCompleted(n) => {
            assert_eq!(n.turn.id, event_turn_id);
            assert_eq!(n.turn.root_turn_id.as_deref(), Some("root-a"));
            assert_eq!(n.turn.status, TurnStatus::Interrupted);
            assert_eq!(n.turn.error, None);
            assert_eq!(n.turn.completed_at, Some(TEST_TURN_COMPLETED_AT));
            assert_eq!(n.turn.duration_ms, Some(TEST_TURN_DURATION_MS));
        }
        other => bail!("unexpected message: {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "no extra messages expected");
    Ok(())
}

#[tokio::test]
async fn test_handle_turn_complete_emits_failed_with_error() -> Result<()> {
    let conversation_id = ThreadId::new();
    let event_turn_id = "complete_err1".to_string();
    let thread_state = new_thread_state();
    handle_error(
        conversation_id,
        TurnError {
            misalignment: None,
            message: "bad".to_string(),
            codex_error_info: Some(V2CodexErrorInfo::Other),
            additional_details: None,
        },
        &thread_state,
    )
    .await;
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());

    handle_turn_complete(
        conversation_id,
        event_turn_id.clone(),
        turn_complete_event(&event_turn_id),
        &outgoing,
        &thread_state,
    )
    .await;

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnCompleted(n) => {
            assert_eq!(n.turn.id, event_turn_id);
            assert_eq!(n.turn.status, TurnStatus::Failed);
            assert_eq!(
                n.turn.error,
                Some(TurnError {
                    misalignment: None,
                    message: "bad".to_string(),
                    codex_error_info: Some(V2CodexErrorInfo::Other),
                    additional_details: None,
                })
            );
            assert_eq!(n.turn.completed_at, Some(TEST_TURN_COMPLETED_AT));
            assert_eq!(n.turn.duration_ms, Some(TEST_TURN_DURATION_MS));
        }
        other => bail!("unexpected message: {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "no extra messages expected");
    Ok(())
}

#[tokio::test]
async fn test_handle_turn_plan_update_emits_notification_for_v2() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());
    let update = UpdatePlanArgs {
        explanation: Some("need plan".to_string()),
        plan: vec![
            PlanItemArg {
                step: "first".to_string(),
                status: StepStatus::Pending,
            },
            PlanItemArg {
                step: "second".to_string(),
                status: StepStatus::Completed,
            },
        ],
    };

    let conversation_id = ThreadId::new();

    handle_turn_plan_update(conversation_id, "turn-123", update, &outgoing).await;

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnPlanUpdated(n) => {
            assert_eq!(n.thread_id, conversation_id.to_string());
            assert_eq!(n.turn_id, "turn-123");
            assert_eq!(n.explanation.as_deref(), Some("need plan"));
            assert_eq!(n.plan.len(), 2);
            assert_eq!(n.plan[0].step, "first");
            assert_eq!(n.plan[0].status, TurnPlanStepStatus::Pending);
            assert_eq!(n.plan[1].step, "second");
            assert_eq!(n.plan[1].status, TurnPlanStepStatus::Completed);
        }
        other => bail!("unexpected message: {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "no extra messages expected");
    Ok(())
}

#[tokio::test]
async fn test_handle_token_count_event_emits_usage_and_rate_limits() -> Result<()> {
    let conversation_id = ThreadId::new();
    let turn_id = "turn-123".to_string();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());

    let info = TokenUsageInfo {
        total_token_usage: TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 25,
            cache_write_input_tokens: 0,
            output_tokens: 50,
            reasoning_output_tokens: 9,
            total_tokens: 200,
            codex_rollout_budget_units: None,
        },
        last_usage_includes_reasoning: None,
        last_token_usage: TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 5,
            cache_write_input_tokens: 0,
            output_tokens: 7,
            reasoning_output_tokens: 1,
            total_tokens: 23,
            codex_rollout_budget_units: None,
        },
        model_context_window: Some(4096),
    };
    let rate_limits = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(RateLimitWindow {
            used_percent: 42.5,
            window_minutes: Some(15),
            resets_at: Some(1700000000),
        }),
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("5".to_string()),
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };

    handle_token_count_event(
        conversation_id,
        turn_id.clone(),
        TokenCountEvent {
            info: Some(info),
            rate_limits: Some(rate_limits),
        },
        &outgoing,
    )
    .await;

    let first = recv_broadcast_notification(&mut rx).await?;
    match first {
        ServerNotification::ThreadTokenUsageUpdated(payload) => {
            assert_eq!(payload.thread_id, conversation_id.to_string());
            assert_eq!(payload.turn_id, turn_id);
            let usage = payload.token_usage;
            assert_eq!(usage.total.total_tokens, 200);
            assert_eq!(usage.total.cached_input_tokens, 25);
            assert_eq!(usage.last.output_tokens, 7);
            assert_eq!(usage.model_context_window, Some(4096));
        }
        other => bail!("unexpected notification: {other:?}"),
    }

    let second = recv_broadcast_notification(&mut rx).await?;
    match second {
        ServerNotification::AccountRateLimitsUpdated(payload) => {
            assert_eq!(payload.rate_limits.limit_id.as_deref(), Some("codex"));
            assert_eq!(payload.rate_limits.limit_name, None);
            assert!(payload.rate_limits.primary.is_some());
            assert!(payload.rate_limits.credits.is_some());
        }
        other => bail!("unexpected notification: {other:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn test_handle_token_count_event_without_usage_info() -> Result<()> {
    let conversation_id = ThreadId::new();
    let turn_id = "turn-456".to_string();
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());

    handle_token_count_event(
        conversation_id,
        turn_id.clone(),
        TokenCountEvent {
            info: None,
            rate_limits: None,
        },
        &outgoing,
    )
    .await;

    assert!(
        rx.try_recv().is_err(),
        "no notifications should be emitted when token usage info is absent"
    );
    Ok(())
}

#[tokio::test]
async fn test_handle_turn_complete_emits_error_multiple_turns() -> Result<()> {
    // Conversation A will have two turns; Conversation B will have one turn.
    let conversation_a = ThreadId::new();
    let conversation_b = ThreadId::new();
    let thread_state = new_thread_state();

    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());

    // Turn 1 on conversation A
    let a_turn1 = "a_turn1".to_string();
    handle_error(
        conversation_a,
        TurnError {
            misalignment: None,
            message: "a1".to_string(),
            codex_error_info: Some(V2CodexErrorInfo::BadRequest),
            additional_details: None,
        },
        &thread_state,
    )
    .await;
    handle_turn_complete(
        conversation_a,
        a_turn1.clone(),
        turn_complete_event(&a_turn1),
        &outgoing,
        &thread_state,
    )
    .await;

    // Turn 1 on conversation B
    let b_turn1 = "b_turn1".to_string();
    handle_error(
        conversation_b,
        TurnError {
            misalignment: None,
            message: "b1".to_string(),
            codex_error_info: None,
            additional_details: None,
        },
        &thread_state,
    )
    .await;
    handle_turn_complete(
        conversation_b,
        b_turn1.clone(),
        turn_complete_event(&b_turn1),
        &outgoing,
        &thread_state,
    )
    .await;

    // Turn 2 on conversation A
    let a_turn2 = "a_turn2".to_string();
    handle_turn_complete(
        conversation_a,
        a_turn2.clone(),
        turn_complete_event(&a_turn2),
        &outgoing,
        &thread_state,
    )
    .await;

    // Verify: A turn 1
    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnCompleted(n) => {
            assert_eq!(n.turn.id, a_turn1);
            assert_eq!(n.turn.status, TurnStatus::Failed);
            assert_eq!(
                n.turn.error,
                Some(TurnError {
                    misalignment: None,
                    message: "a1".to_string(),
                    codex_error_info: Some(V2CodexErrorInfo::BadRequest),
                    additional_details: None,
                })
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }

    // Verify: B turn 1
    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnCompleted(n) => {
            assert_eq!(n.turn.id, b_turn1);
            assert_eq!(n.turn.status, TurnStatus::Failed);
            assert_eq!(
                n.turn.error,
                Some(TurnError {
                    misalignment: None,
                    message: "b1".to_string(),
                    codex_error_info: None,
                    additional_details: None,
                })
            );
        }
        other => bail!("unexpected message: {other:?}"),
    }

    // Verify: A turn 2
    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnCompleted(n) => {
            assert_eq!(n.turn.id, a_turn2);
            assert_eq!(n.turn.status, TurnStatus::Completed);
            assert_eq!(n.turn.error, None);
        }
        other => bail!("unexpected message: {other:?}"),
    }

    assert!(rx.try_recv().is_err(), "no extra messages expected");
    Ok(())
}

#[tokio::test]
async fn test_handle_turn_diff_emits_v2_notification() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let outgoing =
        ThreadScopedOutgoingMessageSender::new(outgoing, vec![ConnectionId(1)], ThreadId::new());
    let unified_diff = "--- a\n+++ b\n".to_string();
    let conversation_id = ThreadId::new();

    handle_turn_diff(
        conversation_id,
        "turn-1",
        TurnDiffEvent {
            unified_diff: unified_diff.clone(),
        },
        &outgoing,
    )
    .await;

    let msg = recv_broadcast_notification(&mut rx).await?;
    match msg {
        ServerNotification::TurnDiffUpdated(notification) => {
            assert_eq!(notification.thread_id, conversation_id.to_string());
            assert_eq!(notification.turn_id, "turn-1");
            assert_eq!(notification.diff, unified_diff);
        }
        other => bail!("unexpected message: {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "no extra messages expected");
    Ok(())
}
