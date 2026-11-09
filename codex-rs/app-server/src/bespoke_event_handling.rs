use crate::notification_media::without_notification_media;
use crate::outgoing_message::ClientRequestResult;
use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use crate::request_processors::thread_settings_from_config_snapshot;
use crate::server_request_error::is_turn_transition_server_request_error;
use crate::thread_state::PendingNetworkApproval;
use crate::thread_state::ThreadState;
use crate::thread_state::TurnSummary;
use crate::thread_state::resolve_server_request_on_thread_listener;
use crate::thread_status::ThreadWatchActiveGuard;
use crate::thread_status::ThreadWatchManager;
use codex_app_server_protocol::AccountRateLimitsUpdatedNotification;
use codex_app_server_protocol::AdditionalPermissionProfile as V2AdditionalPermissionProfile;
use codex_app_server_protocol::AuthRecoveryNotification;
use codex_app_server_protocol::CodexErrorInfo as V2CodexErrorInfo;
use codex_app_server_protocol::CommandAction as V2ParsedCommand;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionPresentation;
use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
use codex_app_server_protocol::CommandExecutionRequestApprovalResponse;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::DeprecationNoticeNotification;
use codex_app_server_protocol::DynamicToolCallParams;
use codex_app_server_protocol::EnvironmentConnectionNotification;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::ExecPolicyAmendment as V2ExecPolicyAmendment;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::FileChangeRequestApprovalParams;
use codex_app_server_protocol::FileChangeRequestApprovalResponse;
use codex_app_server_protocol::GrantedPermissionProfile as V2GrantedPermissionProfile;
use codex_app_server_protocol::GuardianWarningNotification;
use codex_app_server_protocol::HookCompletedNotification;
use codex_app_server_protocol::HookStartedNotification;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::McpServerElicitationAction;
use codex_app_server_protocol::McpServerElicitationRequestParams;
use codex_app_server_protocol::McpServerElicitationRequestResponse;
use codex_app_server_protocol::McpServerStartupState;
use codex_app_server_protocol::McpServerStatusUpdatedNotification;
use codex_app_server_protocol::ModelReroutedNotification;
use codex_app_server_protocol::ModelSafetyBufferingUpdatedNotification;
use codex_app_server_protocol::ModelVerificationNotification;
use codex_app_server_protocol::NetworkApprovalContext as V2NetworkApprovalContext;
use codex_app_server_protocol::NetworkPolicyAmendment as V2NetworkPolicyAmendment;
use codex_app_server_protocol::NetworkPolicyRuleAction as V2NetworkPolicyRuleAction;
use codex_app_server_protocol::PermissionsRequestApprovalParams;
use codex_app_server_protocol::PermissionsRequestApprovalResponse;
use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::RawResponseItemCompletedNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequestPayload;
use codex_app_server_protocol::StrictReviewRequiredNotification;
use codex_app_server_protocol::ThreadGoalUpdatedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadRealtimeClosedNotification;
use codex_app_server_protocol::ThreadRealtimeErrorNotification;
use codex_app_server_protocol::ThreadRealtimeItemAddedNotification;
use codex_app_server_protocol::ThreadRealtimeItemCompletedNotification;
use codex_app_server_protocol::ThreadRealtimeItemStartedNotification;
use codex_app_server_protocol::ThreadRealtimeItemTranscriptDeltaNotification;
use codex_app_server_protocol::ThreadRealtimeOutputAudioDeltaNotification;
use codex_app_server_protocol::ThreadRealtimeSdpNotification;
use codex_app_server_protocol::ThreadRealtimeStartedNotification;
use codex_app_server_protocol::ThreadRealtimeTranscriptDeltaNotification;
use codex_app_server_protocol::ThreadRealtimeTranscriptDoneNotification;
use codex_app_server_protocol::ThreadSettingsUpdatedNotification;
use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::ToolRequestUserInputOption;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::ToolRequestUserInputQuestion;
use codex_app_server_protocol::ToolRequestUserInputResponse;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnDiffUpdatedNotification;
use codex_app_server_protocol::TurnError;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnItemsView;
use codex_app_server_protocol::TurnModerationMetadataNotification;
use codex_app_server_protocol::TurnPlanStep;
use codex_app_server_protocol::TurnPlanUpdatedNotification;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::WarningNotification;
use codex_app_server_protocol::build_item_from_guardian_event;
use codex_app_server_protocol::guardian_auto_approval_review_notification;
use codex_app_server_protocol::item_event_to_server_notification;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::items::CollabAgentTool as CoreCollabAgentTool;
use codex_protocol::items::ModelInvocationContext;
use codex_protocol::items::TurnItem as CoreTurnItem;
use codex_protocol::mcp_approval_meta::APPROVAL_KIND_KEY;
use codex_protocol::mcp_approval_meta::APPROVAL_KIND_MCP_TOOL_CALL;
use codex_protocol::mcp_approval_meta::CANCELLATION_REASON_KEY;
use codex_protocol::mcp_approval_meta::McpApprovalCancellationReason;
use codex_protocol::models::AdditionalPermissionProfile as CoreAdditionalPermissionProfile;
use codex_protocol::plan_tool::UpdatePlanArgs;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::PrivateSessionEventPayload;
use codex_protocol::protocol::RealtimeEvent;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SubAgentActivityKind;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnDiffEvent;
use codex_protocol::request_permissions::PermissionGrantScope as CorePermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile as CoreRequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsResponse as CoreRequestPermissionsResponse;
use codex_protocol::request_user_input::RequestUserInputAnswer as CoreRequestUserInputAnswer;
use codex_protocol::request_user_input::RequestUserInputResponse as CoreRequestUserInputResponse;
use codex_shell_command::parse_command::shlex_join;
use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::PathUri;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::error;

enum CommandExecutionApprovalPresentation {
    Network(V2NetworkApprovalContext),
    Command(CommandExecutionCompletionItem),
}

#[derive(Debug, PartialEq)]
struct CommandExecutionCompletionItem {
    model_context: Option<ModelInvocationContext>,
    plugin_id: Option<String>,
    script_path: Option<String>,
    command: String,
    cwd: LegacyAppPathString,
    command_actions: Vec<V2ParsedCommand>,
}

pub(crate) async fn apply_bespoke_event_handling(
    event: Event,
    conversation_id: ThreadId,
    conversation: Arc<CodexThread>,
    thread_manager: Arc<ThreadManager>,
    outgoing: ThreadScopedOutgoingMessageSender,
    thread_state: Arc<tokio::sync::Mutex<ThreadState>>,
    thread_watch_manager: ThreadWatchManager,
) {
    let Event {
        id: event_turn_id,
        msg,
    } = event;
    let approval_starts_command = msg.command_approval_start().is_some();
    match msg {
        EventMsg::TurnStarted(payload) => {
            // While not technically necessary as it was already done on TurnComplete, be extra cautios and abort any pending server requests.
            outgoing.abort_pending_server_requests().await;
            thread_watch_manager
                .note_turn_started(&conversation_id.to_string())
                .await;
            let turn = {
                let state = thread_state.lock().await;
                let turn = state.active_turn_metadata_snapshot().map(Turn::from);
                turn.unwrap_or_else(|| Turn {
                    id: payload.turn_id.clone(),
                    root_turn_id: payload.root_turn_id.clone(),
                    items: Vec::new(),
                    items_view: TurnItemsView::NotLoaded,
                    error: None,
                    status: TurnStatus::InProgress,
                    started_at: payload.started_at,
                    completed_at: None,
                    duration_ms: None,
                })
            };
            let notification = TurnStartedNotification {
                thread_id: conversation_id.to_string(),
                turn,
            };
            outgoing
                .send_server_notification(ServerNotification::TurnStarted(notification))
                .await;
        }
        EventMsg::TurnComplete(turn_complete_event) => {
            // All per-thread requests are bound to a turn, so abort them.
            outgoing.abort_pending_server_requests().await;
            respond_to_pending_interrupts(&thread_state, &outgoing).await;
            let turn_failed = thread_state.lock().await.turn_summary.last_error.is_some();
            thread_watch_manager
                .note_turn_completed(&conversation_id.to_string(), turn_failed)
                .await;
            handle_turn_complete(
                conversation_id,
                event_turn_id,
                turn_complete_event,
                &outgoing,
                &thread_state,
            )
            .await;
        }
        EventMsg::McpStartupUpdate(update) => {
            let (status, error, failure_reason) = match update.status {
                codex_protocol::protocol::McpStartupStatus::Starting => {
                    (McpServerStartupState::Starting, None, None)
                }
                codex_protocol::protocol::McpStartupStatus::Ready => {
                    (McpServerStartupState::Ready, None, None)
                }
                codex_protocol::protocol::McpStartupStatus::Failed { error, reason } => (
                    McpServerStartupState::Failed,
                    Some(error),
                    reason.map(Into::into),
                ),
                codex_protocol::protocol::McpStartupStatus::Cancelled => {
                    (McpServerStartupState::Cancelled, None, None)
                }
            };
            let notification = McpServerStatusUpdatedNotification {
                thread_id: Some(conversation_id.to_string()),
                name: update.server,
                status,
                error,
                failure_reason,
            };
            outgoing
                .send_server_notification(ServerNotification::McpServerStatusUpdated(notification))
                .await;
        }
        EventMsg::EnvironmentConnected(event) => {
            outgoing
                .send_server_notification(ServerNotification::EnvironmentConnected(
                    EnvironmentConnectionNotification {
                        thread_id: conversation_id.to_string(),
                        environment_id: event.environment_id,
                    },
                ))
                .await;
        }
        EventMsg::EnvironmentDisconnected(event) => {
            outgoing
                .send_server_notification(ServerNotification::EnvironmentDisconnected(
                    EnvironmentConnectionNotification {
                        thread_id: conversation_id.to_string(),
                        environment_id: event.environment_id,
                    },
                ))
                .await;
        }
        EventMsg::AuthRecoveryStarted(event) => {
            outgoing
                .send_server_notification(ServerNotification::AuthRecoveryStarted(
                    AuthRecoveryNotification {
                        thread_id: conversation_id.to_string(),
                        turn_id: event_turn_id,
                        provider: event.provider,
                        message: event.message,
                    },
                ))
                .await;
        }
        EventMsg::AuthRecoveryCompleted(event) => {
            outgoing
                .send_server_notification(ServerNotification::AuthRecoveryCompleted(
                    AuthRecoveryNotification {
                        thread_id: conversation_id.to_string(),
                        turn_id: event_turn_id,
                        provider: event.provider,
                        message: event.message,
                    },
                ))
                .await;
        }
        EventMsg::Warning(warning_event) => {
            let notification = WarningNotification {
                thread_id: Some(conversation_id.to_string()),
                message: warning_event.message,
            };
            outgoing
                .send_server_notification(ServerNotification::Warning(notification))
                .await;
        }
        EventMsg::GuardianWarning(warning_event) => {
            let notification = GuardianWarningNotification {
                thread_id: conversation_id.to_string(),
                message: warning_event.message,
            };
            outgoing
                .send_server_notification(ServerNotification::GuardianWarning(notification))
                .await;
        }
        EventMsg::GuardianAssessment(assessment) => {
            let pending_command_execution = match build_item_from_guardian_event(
                &assessment,
                CommandExecutionStatus::InProgress,
            ) {
                Some(ThreadItemPayload::CommandExecution {
                    id,
                    model_context,
                    plugin_id,
                    script_path,
                    command,
                    cwd,
                    command_actions,
                    ..
                }) => Some((
                    id,
                    CommandExecutionCompletionItem {
                        model_context,
                        plugin_id,
                        script_path,
                        command,
                        cwd,
                        command_actions,
                    },
                )),
                Some(_) | None => None,
            };
            let assessment_turn_id = if assessment.turn_id.is_empty() {
                event_turn_id.clone()
            } else {
                assessment.turn_id.clone()
            };
            if approval_starts_command
                && let Some((target_item_id, completion_item)) = pending_command_execution.as_ref()
            {
                start_command_execution_item(
                    &conversation_id,
                    assessment_turn_id.clone(),
                    target_item_id.clone(),
                    assessment.started_at_ms,
                    completion_item,
                    &outgoing,
                    &thread_state,
                )
                .await;
            }
            let notification = guardian_auto_approval_review_notification(
                &conversation_id,
                &event_turn_id,
                &assessment,
            );
            outgoing.send_server_notification(notification).await;
            if assessment.status == codex_protocol::protocol::GuardianAssessmentStatus::InProgress
                && matches!(
                    assessment.review_reason,
                    Some(
                        codex_protocol::approvals::GuardianReviewReason::ElevatedRisk
                            | codex_protocol::approvals::GuardianReviewReason::StaleScore
                            | codex_protocol::approvals::GuardianReviewReason::IncompatibleCompaction
                            | codex_protocol::approvals::GuardianReviewReason::ScoringFailure
                            | codex_protocol::approvals::GuardianReviewReason::AuthorizationChanged
                    )
                )
            {
                outgoing
                    .send_server_notification(ServerNotification::StrictReviewRequired(
                        StrictReviewRequiredNotification {
                            thread_id: conversation_id.to_string(),
                            turn_id: assessment_turn_id.clone(),
                            started_at_ms: assessment.started_at_ms,
                        },
                    ))
                    .await;
            }
            let completion_status = match assessment.status {
                codex_protocol::protocol::GuardianAssessmentStatus::Denied
                | codex_protocol::protocol::GuardianAssessmentStatus::Aborted => {
                    Some(CommandExecutionStatus::Declined)
                }
                codex_protocol::protocol::GuardianAssessmentStatus::TimedOut => {
                    Some(CommandExecutionStatus::Failed)
                }
                codex_protocol::protocol::GuardianAssessmentStatus::InProgress
                | codex_protocol::protocol::GuardianAssessmentStatus::Approved => None,
            };
            if let Some(completion_status) = completion_status
                && let Some((target_item_id, completion_item)) = pending_command_execution
            {
                complete_command_execution_item(
                    &conversation_id,
                    assessment_turn_id,
                    target_item_id,
                    completion_item,
                    completion_status,
                    assessment.completed_at_ms,
                    &outgoing,
                    &thread_state,
                )
                .await;
            }
        }
        EventMsg::ModelReroute(event) => {
            let notification = ModelReroutedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                from_model: event.from_model,
                to_model: event.to_model,
                reason: event.reason.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::ModelRerouted(notification))
                .await;
        }
        EventMsg::ModelVerification(event) => {
            let notification = ModelVerificationNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                verifications: event.verifications.into_iter().map(Into::into).collect(),
            };
            outgoing
                .send_server_notification(ServerNotification::ModelVerification(notification))
                .await;
        }
        EventMsg::TurnModerationMetadata(event) => {
            let notification = TurnModerationMetadataNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                metadata: event.metadata,
            };
            outgoing
                .send_server_notification(ServerNotification::TurnModerationMetadata(notification))
                .await;
        }
        EventMsg::SafetyBuffering(event) => {
            let notification = ModelSafetyBufferingUpdatedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id.clone(),
                model: event.model,
                use_cases: event.use_cases,
                reasons: event.reasons,
                show_buffering_ui: event.show_buffering_ui,
                faster_model: event.faster_model,
            };
            outgoing
                .send_server_notification(ServerNotification::ModelSafetyBufferingUpdated(
                    notification,
                ))
                .await;
        }
        EventMsg::RealtimeConversationStarted(event) => {
            let notification = ThreadRealtimeStartedNotification {
                thread_id: conversation_id.to_string(),
                realtime_session_id: event.realtime_session_id,
                version: event.version,
            };
            outgoing
                .send_server_notification(ServerNotification::ThreadRealtimeStarted(notification))
                .await;
        }
        EventMsg::RealtimeConversationSdp(event) => {
            let notification = ThreadRealtimeSdpNotification {
                thread_id: conversation_id.to_string(),
                sdp: event.sdp,
            };
            outgoing
                .send_server_notification(ServerNotification::ThreadRealtimeSdp(notification))
                .await;
        }
        EventMsg::RealtimeConversationRealtime(event) => match event.payload {
            RealtimeEvent::HistoryItemStarted(item) => {
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeItemStarted(
                        ThreadRealtimeItemStartedNotification {
                            thread_id: conversation_id.to_string(),
                            item: item.into(),
                        },
                    ))
                    .await;
            }
            RealtimeEvent::HistoryTranscriptDelta { item_id, delta } => {
                outgoing
                    .send_server_notification(
                        ServerNotification::ThreadRealtimeItemTranscriptDelta(
                            ThreadRealtimeItemTranscriptDeltaNotification {
                                thread_id: conversation_id.to_string(),
                                item_id,
                                delta,
                            },
                        ),
                    )
                    .await;
            }
            RealtimeEvent::HistoryItemCompleted(item) => {
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeItemCompleted(
                        ThreadRealtimeItemCompletedNotification {
                            thread_id: conversation_id.to_string(),
                            item: item.into(),
                        },
                    ))
                    .await;
            }
            RealtimeEvent::SessionUpdated {
                realtime_session_id,
                ..
            } => outgoing.track_realtime_session_updated(realtime_session_id),
            RealtimeEvent::InputAudioSpeechStarted(event) => {
                let notification = ThreadRealtimeItemAddedNotification {
                    thread_id: conversation_id.to_string(),
                    item: serde_json::json!({
                        "type": "input_audio_buffer.speech_started",
                        "item_id": event.item_id,
                    }),
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeItemAdded(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::InputTranscriptDelta(event) => {
                let notification = ThreadRealtimeTranscriptDeltaNotification {
                    thread_id: conversation_id.to_string(),
                    role: "user".to_string(),
                    delta: event.delta,
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeTranscriptDelta(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::InputTranscriptDone(event) => {
                let notification = ThreadRealtimeTranscriptDoneNotification {
                    thread_id: conversation_id.to_string(),
                    role: "user".to_string(),
                    text: event.text,
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeTranscriptDone(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::OutputTranscriptDelta(event) => {
                let notification = ThreadRealtimeTranscriptDeltaNotification {
                    thread_id: conversation_id.to_string(),
                    role: "assistant".to_string(),
                    delta: event.delta,
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeTranscriptDelta(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::OutputTranscriptDone(event) => {
                let notification = ThreadRealtimeTranscriptDoneNotification {
                    thread_id: conversation_id.to_string(),
                    role: "assistant".to_string(),
                    text: event.text,
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeTranscriptDone(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::AudioOut(audio) => {
                let notification = ThreadRealtimeOutputAudioDeltaNotification {
                    thread_id: conversation_id.to_string(),
                    audio: audio.into(),
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeOutputAudioDelta(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::ResponseCreated(_) => {}
            RealtimeEvent::ResponseCancelled(event) => {
                let notification = ThreadRealtimeItemAddedNotification {
                    thread_id: conversation_id.to_string(),
                    item: serde_json::json!({
                        "type": "response.cancelled",
                        "response_id": event.response_id,
                    }),
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeItemAdded(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::ResponseDone(_) => {}
            RealtimeEvent::ConversationItemAdded(item) => {
                let notification = ThreadRealtimeItemAddedNotification {
                    thread_id: conversation_id.to_string(),
                    item,
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeItemAdded(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::ConversationItemDone { .. } | RealtimeEvent::NoopRequested(_) => {}
            RealtimeEvent::HandoffRequested(handoff) => {
                let notification = ThreadRealtimeItemAddedNotification {
                    thread_id: conversation_id.to_string(),
                    item: serde_json::json!({
                        "type": "handoff_request",
                        "handoff_id": handoff.handoff_id,
                        "item_id": handoff.item_id,
                        "input_transcript": handoff.input_transcript,
                        "active_transcript": handoff.active_transcript,
                    }),
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeItemAdded(
                        notification,
                    ))
                    .await;
            }
            RealtimeEvent::Error(message) => {
                let notification = ThreadRealtimeErrorNotification {
                    thread_id: conversation_id.to_string(),
                    message,
                };
                outgoing
                    .send_server_notification(ServerNotification::ThreadRealtimeError(notification))
                    .await;
            }
        },
        EventMsg::RealtimeConversationClosed(event) => {
            let notification = ThreadRealtimeClosedNotification {
                thread_id: conversation_id.to_string(),
                reason: event.reason,
            };
            outgoing
                .send_server_notification(ServerNotification::ThreadRealtimeClosed(notification))
                .await;
        }
        EventMsg::ApplyPatchApprovalRequest(event) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let item_id = event.call_id.clone();

            let params = FileChangeRequestApprovalParams {
                thread_id: conversation_id.to_string(),
                turn_id: event.turn_id.clone(),
                item_id: item_id.clone(),
                started_at_ms: event.started_at_ms,
                reason: event.reason.clone(),
                grant_root: event.grant_root.clone(),
            };
            let (pending_request_id, rx) = outgoing
                .send_request(ServerRequestPayload::FileChangeRequestApproval(params))
                .await;
            tokio::spawn(async move {
                on_file_change_request_approval_response(
                    item_id,
                    event.turn_id,
                    pending_request_id,
                    rx,
                    conversation,
                    thread_state.clone(),
                    permission_guard,
                )
                .await;
            });
        }
        EventMsg::ExecApprovalRequest(ev) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let available_decisions = ev
                .effective_available_decisions()
                .into_iter()
                .map(CommandExecutionApprovalDecision::from)
                .collect::<Vec<_>>();
            let ExecApprovalRequestEvent {
                model_context,
                kind,
                call_id,
                plugin_id,
                script_path,
                approval_id,
                turn_id,
                environment_id,
                started_at_ms,
                command,
                cwd,
                reason,
                network_approval_context,
                proposed_execpolicy_amendment,
                proposed_network_policy_amendments,
                additional_permissions,
                parsed_cmd,
                ..
            } = ev;
            let cwd_uri = match PathUri::try_from(cwd.clone()) {
                Ok(cwd) => cwd,
                Err(err) => {
                    error!(%err, "invalid command approval cwd");
                    if let Err(err) = conversation
                        .submit(Op::ExecApproval {
                            id: approval_id.unwrap_or(call_id),
                            turn_id: Some(turn_id),
                            decision: ReviewDecision::denied("invalid command approval cwd"),
                        })
                        .await
                    {
                        error!(%err, "failed to reject invalid command approval");
                    }
                    return;
                }
            };
            let command_presentation =
                CommandExecutionPresentation::from_raw(&command, &parsed_cmd, &cwd_uri);
            // Approval requests retain the exact command; only history is redacted.
            let command_actions = match cwd_uri.to_abs_path() {
                Ok(native_cwd) => parsed_cmd
                    .iter()
                    .cloned()
                    .map(|parsed| V2ParsedCommand::from_core_with_cwd(parsed, &native_cwd))
                    .collect(),
                Err(_) => vec![V2ParsedCommand::Unknown {
                    command: shlex_join(&command),
                }],
            };
            let presentation = if let Some(network_approval_context) =
                network_approval_context.map(V2NetworkApprovalContext::from)
            {
                CommandExecutionApprovalPresentation::Network(network_approval_context)
            } else {
                let completion_item = CommandExecutionCompletionItem {
                    model_context,
                    plugin_id,
                    script_path,
                    command: command_presentation.command,
                    cwd: cwd.clone(),
                    command_actions: command_presentation.command_actions,
                };
                CommandExecutionApprovalPresentation::Command(completion_item)
            };
            let (network_approval_context, command, cwd, command_actions, completion_item) =
                match presentation {
                    CommandExecutionApprovalPresentation::Network(network_approval_context) => {
                        (Some(network_approval_context), None, None, None, None)
                    }
                    CommandExecutionApprovalPresentation::Command(completion_item) => (
                        None,
                        Some(shlex_join(&command)),
                        Some(completion_item.cwd.clone()),
                        Some(command_actions),
                        (kind != codex_protocol::approvals::ExecApprovalKind::HumanCommand)
                            .then_some(completion_item),
                    ),
                };
            if approval_starts_command && let Some(completion_item) = completion_item.as_ref() {
                start_command_execution_item(
                    &conversation_id,
                    event_turn_id.clone(),
                    call_id.clone(),
                    started_at_ms,
                    completion_item,
                    &outgoing,
                    &thread_state,
                )
                .await;
            }
            let proposed_execpolicy_amendment_v2 =
                proposed_execpolicy_amendment.map(V2ExecPolicyAmendment::from);
            let proposed_network_policy_amendments_v2 =
                proposed_network_policy_amendments.map(|amendments| {
                    amendments
                        .into_iter()
                        .map(V2NetworkPolicyAmendment::from)
                        .collect()
                });
            let additional_permissions =
                additional_permissions.map(V2AdditionalPermissionProfile::from);

            let is_network_approval = network_approval_context.is_some();
            let params = CommandExecutionRequestApprovalParams {
                kind: kind.into(),
                thread_id: conversation_id.to_string(),
                turn_id: turn_id.clone(),
                item_id: call_id.clone(),
                started_at_ms,
                approval_id: approval_id.clone(),
                environment_id,
                reason,
                network_approval_context,
                command,
                cwd,
                command_actions,
                additional_permissions,
                proposed_execpolicy_amendment: proposed_execpolicy_amendment_v2,
                proposed_network_policy_amendments: proposed_network_policy_amendments_v2,
                available_decisions: Some(available_decisions),
            };
            let (pending_request_id, rx) = outgoing
                .send_request(ServerRequestPayload::CommandExecutionRequestApproval(
                    params,
                ))
                .await;
            let permission_guard = if is_network_approval {
                thread_state.lock().await.network_approvals.insert(
                    approval_id.as_ref().unwrap_or(&call_id).clone(),
                    PendingNetworkApproval {
                        request_id: pending_request_id.clone(),
                        permission_guard,
                    },
                );
                None
            } else {
                Some(permission_guard)
            };
            tokio::spawn(async move {
                on_command_execution_request_approval_response(
                    event_turn_id,
                    conversation_id,
                    approval_id,
                    call_id,
                    completion_item,
                    pending_request_id,
                    rx,
                    conversation,
                    outgoing,
                    thread_state,
                    permission_guard,
                )
                .await;
            });
        }
        EventMsg::NetworkApprovalResolved { approval_id } => {
            let pending = thread_state
                .lock()
                .await
                .network_approvals
                .remove(&approval_id);
            if let Some(pending) = pending {
                outgoing.resolve_server_request(pending.request_id).await;
                drop(pending.permission_guard);
            }
        }
        EventMsg::RequestUserInput(request) => {
            let user_input_guard = thread_watch_manager
                .note_user_input_requested(&conversation_id.to_string())
                .await;
            let questions = request
                .questions
                .into_iter()
                .map(|question| ToolRequestUserInputQuestion {
                    id: question.id,
                    header: question.header,
                    question: question.question,
                    is_other: question.is_other,
                    is_secret: question.is_secret,
                    options: question.options.map(|options| {
                        options
                            .into_iter()
                            .map(|option| ToolRequestUserInputOption {
                                label: option.label,
                                description: option.description,
                            })
                            .collect()
                    }),
                })
                .collect();
            let params = ToolRequestUserInputParams {
                thread_id: conversation_id.to_string(),
                turn_id: request.turn_id,
                item_id: request.call_id,
                questions,
                is_blocking: request.is_blocking,
                auto_resolution_ms: request.auto_resolution_ms,
            };
            let (pending_request_id, rx) = outgoing
                .send_request(ServerRequestPayload::ToolRequestUserInput(params))
                .await;
            tokio::spawn(async move {
                on_request_user_input_response(
                    event_turn_id,
                    pending_request_id,
                    rx,
                    conversation,
                    thread_state,
                    user_input_guard,
                )
                .await;
            });
        }
        EventMsg::ElicitationRequest(request) => {
            let mcp_tool_approval = matches!(
                &request.request,
                codex_protocol::approvals::ElicitationRequest::Form { meta: Some(meta), .. }
                    if meta.get(APPROVAL_KIND_KEY).and_then(serde_json::Value::as_str) == Some(APPROVAL_KIND_MCP_TOOL_CALL)
            );
            let user_verification = matches!(
                &request.request,
                codex_protocol::approvals::ElicitationRequest::UserVerification { .. }
            );
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let turn_id = match request.turn_id.clone() {
                Some(turn_id) => Some(turn_id),
                None => {
                    let state = thread_state.lock().await;
                    state.active_turn_id().map(str::to_owned)
                }
            };
            let server_name = request.server_name.clone();
            let request_body = match request.request.try_into() {
                Ok(request_body) => request_body,
                Err(err) => {
                    error!(
                        error = %err,
                        server_name,
                        request_id = ?request.id,
                        "failed to parse typed MCP elicitation schema"
                    );
                    if let Err(err) = conversation
                        .submit(Op::ResolveElicitation {
                            server_name: request.server_name,
                            request_id: request.id,
                            decision: codex_protocol::approvals::ElicitationAction::Cancel,
                            content: None,
                            meta: None,
                        })
                        .await
                    {
                        error!("failed to submit ResolveElicitation: {err}");
                    }
                    return;
                }
            };
            let params = McpServerElicitationRequestParams {
                thread_id: conversation_id.to_string(),
                turn_id,
                server_name: request.server_name.clone(),
                request: request_body,
            };
            let (pending_request_id, rx) = outgoing
                .send_request(ServerRequestPayload::McpServerElicitationRequest(params))
                .await;
            tokio::spawn(async move {
                on_mcp_server_elicitation_response(
                    request.server_name,
                    request.id,
                    pending_request_id,
                    PendingMcpElicitationResponse {
                        receiver: rx,
                        user_verification,
                        mcp_tool_approval,
                    },
                    conversation,
                    thread_state,
                    permission_guard,
                )
                .await;
            });
        }
        EventMsg::RequestPermissions(request) => {
            let permission_guard = thread_watch_manager
                .note_permission_requested(&conversation_id.to_string())
                .await;
            let request_cwd = match request.cwd {
                Some(cwd) => cwd,
                None => {
                    LegacyAppPathString::from_abs_path(conversation.config_snapshot().await.cwd())
                }
            };
            let params = PermissionsRequestApprovalParams {
                thread_id: conversation_id.to_string(),
                turn_id: request.turn_id.clone(),
                item_id: request.call_id.clone(),
                environment_id: request.environment_id.clone(),
                started_at_ms: request.started_at_ms,
                cwd: request_cwd,
                reason: request.reason,
                permissions: request.permissions.into(),
            };
            let (pending_request_id, rx) = outgoing
                .send_request(ServerRequestPayload::PermissionsRequestApproval(params))
                .await;
            let pending_response = PendingRequestPermissionsResponse {
                call_id: request.call_id,
                conversation_id,
                turn_id: request.turn_id,
                pending_request_id,
                outgoing,
                receiver: rx,
                request_permissions_guard: permission_guard,
            };
            tokio::spawn(async move {
                on_request_permissions_response(pending_response, conversation, thread_state).await;
            });
        }
        EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_)
        | EventMsg::CollabAgentSpawnBegin(_)
        | EventMsg::CollabAgentSpawnEnd(_)
        | EventMsg::CollabAgentInteractionBegin(_)
        | EventMsg::CollabAgentInteractionEnd(_)
        | EventMsg::CollabWaitingBegin(_)
        | EventMsg::CollabWaitingEnd(_)
        | EventMsg::CollabCloseBegin(_)
        | EventMsg::CollabCloseEnd(_)
        | EventMsg::CollabResumeBegin(_)
        | EventMsg::CollabResumeEnd(_)
        | EventMsg::SubAgentActivity(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_) => {
            // Deprecated item lifecycle events are still fanned out for raw-event and rollout
            // compatibility consumers.
            // App-server v2 receives TurnItem lifecycle instead, and dispatches dynamic tool
            // requests from DynamicToolCall starts.
        }
        EventMsg::McpToolCallBegin(_) | EventMsg::McpToolCallEnd(_) => {
            // Deprecated MCP tool-call events are still fanned out for raw-event and rollout
            // compatibility consumers.
            // App-server v2 receives the canonical TurnItem::McpToolCall lifecycle instead.
        }
        msg @ (EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::AgentReasoningSectionBreak(_)) => {
            let notification = item_event_to_server_notification(
                msg,
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing.send_server_notification(notification).await;
        }
        EventMsg::ContextCompacted(..) => {
            // Core still fans out this deprecated event for raw-event and rollout compatibility
            // consumers;
            // v2 clients receive the canonical ContextCompaction item instead.
        }
        EventMsg::DeprecationNotice(event) => {
            let notification = DeprecationNoticeNotification {
                summary: event.summary,
                details: event.details,
            };
            outgoing
                .send_server_notification(ServerNotification::DeprecationNotice(notification))
                .await;
        }
        EventMsg::TokenCount(token_count_event) => {
            handle_token_count_event(conversation_id, event_turn_id, token_count_event, &outgoing)
                .await;
        }
        EventMsg::Error(ev) => {
            thread_watch_manager
                .note_system_error(&conversation_id.to_string())
                .await;

            if !ev.affects_turn_status() {
                return;
            }

            let turn_error = TurnError {
                misalignment: ev.misalignment.map(Into::into),
                message: ev.message,
                codex_error_info: ev.codex_error_info.map(V2CodexErrorInfo::from),
                additional_details: None,
            };
            handle_error_notification(
                conversation_id,
                &event_turn_id,
                turn_error,
                &outgoing,
                &thread_state,
            )
            .await;
        }
        EventMsg::StreamError(ev) => {
            // We don't need to update the turn summary store for stream errors as they are intermediate error states for retries,
            // but we notify the client.
            let turn_error = TurnError {
                misalignment: None,
                message: ev.message,
                codex_error_info: ev.codex_error_info.map(V2CodexErrorInfo::from),
                additional_details: ev.additional_details,
            };
            outgoing
                .send_server_notification(ServerNotification::Error(ErrorNotification {
                    error: turn_error,
                    will_retry: true,
                    thread_id: conversation_id.to_string(),
                    turn_id: event_turn_id.clone(),
                }))
                .await;
        }
        EventMsg::ViewImageToolCall(_) => {}
        EventMsg::ItemStarted(event) => {
            let should_emit = match &event.item {
                // Approval and guardian flows can emit the command start notification before core
                // emits the canonical item. Keep the first observed start and suppress that duplicate.
                CoreTurnItem::CommandExecution(item) => thread_state
                    .lock()
                    .await
                    .turn_summary
                    .record_command_start(item.id.clone(), event.started_at_ms),
                _ => true,
            };
            let dynamic_tool_call_params = match &event.item {
                CoreTurnItem::DynamicToolCall(item) => Some(DynamicToolCallParams {
                    thread_id: conversation_id.to_string(),
                    turn_id: event.turn_id.clone(),
                    call_id: item.id.clone(),
                    namespace: item.namespace.clone(),
                    tool: item.tool.clone(),
                    arguments: item.arguments.clone(),
                }),
                _ => None,
            };
            if should_emit {
                let mut notification = item_event_to_server_notification(
                    EventMsg::ItemStarted(event),
                    &conversation_id.to_string(),
                    &event_turn_id,
                );
                if conversation.enabled(Feature::OmitAppServerNotificationMedia) {
                    notification = without_notification_media(notification);
                }
                outgoing.send_server_notification(notification).await;
            }
            if let Some(params) = dynamic_tool_call_params {
                let call_id = params.call_id.clone();
                let (_pending_request_id, rx) = outgoing
                    .send_request(ServerRequestPayload::DynamicToolCall(params))
                    .await;
                tokio::spawn(async move {
                    crate::dynamic_tools::on_call_response(call_id, rx, conversation).await;
                });
            }
        }
        EventMsg::ItemCompleted(mut event) => {
            apply_canonical_item_completed_side_effects(
                &thread_manager,
                &thread_watch_manager,
                &thread_state,
                &mut event,
            )
            .await;
            let mut notification = item_event_to_server_notification(
                EventMsg::ItemCompleted(event),
                &conversation_id.to_string(),
                &event_turn_id,
            );
            if conversation.enabled(Feature::OmitAppServerNotificationMedia) {
                notification = without_notification_media(notification);
            }
            outgoing.send_server_notification(notification).await;
        }
        msg @ (EventMsg::PatchApplyUpdated(_) | EventMsg::TerminalInteraction(_)) => {
            let notification = item_event_to_server_notification(
                msg,
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing.send_server_notification(notification).await;
        }
        EventMsg::HookStarted(event) => {
            let notification = HookStartedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event.turn_id,
                run: event.run.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::HookStarted(notification))
                .await;
        }
        EventMsg::HookCompleted(event) => {
            let notification = HookCompletedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event.turn_id,
                run: event.run.into(),
            };
            outgoing
                .send_server_notification(ServerNotification::HookCompleted(notification))
                .await;
        }
        EventMsg::PrivateSessionEvent(private) => match private.event {
            PrivateSessionEventPayload::RecordedItem(mut recorded) => {
                recorded.item.clear_executed_tool_calls();
                let mut notification = ServerNotification::RawResponseItemCompleted(
                    RawResponseItemCompletedNotification {
                        thread_id: private.thread_id.to_string(),
                        turn_id: event_turn_id,
                        item: recorded.item,
                    },
                );
                if conversation.enabled(Feature::OmitAppServerNotificationMedia) {
                    notification = without_notification_media(notification);
                }
                outgoing.send_server_notification(notification).await;
            }
            PrivateSessionEventPayload::TurnAborted(aborted) => {
                // Notify observers without completing or clearing the parent's turn.
                emit_turn_completed_with_status(
                    private.thread_id,
                    event_turn_id,
                    TurnCompletionMetadata {
                        status: TurnStatus::Interrupted,
                        root_turn_id: aborted.root_turn_id,
                        error: aborted.error.map(|error| TurnError {
                            message: error.message,
                            codex_error_info: error.codex_error_info.map(Into::into),
                            misalignment: error.misalignment.map(Into::into),
                            additional_details: None,
                        }),
                        last_agent_message: None,
                        started_at: aborted.started_at,
                        completed_at: aborted.completed_at,
                        duration_ms: aborted.duration_ms,
                    },
                    &outgoing,
                )
                .await;
            }
        },
        EventMsg::RawResponseItem(mut raw_response_item_event) => {
            // Keep warehouse metadata out of app-server notifications.
            raw_response_item_event.item.clear_executed_tool_calls();
            let mut notification = ServerNotification::RawResponseItemCompleted(
                RawResponseItemCompletedNotification {
                    thread_id: conversation_id.to_string(),
                    turn_id: event_turn_id,
                    item: raw_response_item_event.item,
                },
            );
            if conversation.enabled(Feature::OmitAppServerNotificationMedia) {
                notification = without_notification_media(notification);
            }
            outgoing.send_server_notification(notification).await;
        }
        EventMsg::RawResponseCompleted(raw_response_completed_event) => {
            let notification = RawResponseCompletedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: event_turn_id,
                response_id: raw_response_completed_event.response_id,
                usage: raw_response_completed_event.token_usage.map(Into::into),
                usage_metadata: raw_response_completed_event.usage_metadata.map(Into::into),
            };
            outgoing
                .send_server_notification(ServerNotification::RawResponseCompleted(notification))
                .await;
        }
        EventMsg::PatchApplyBegin(_) | EventMsg::PatchApplyEnd(_) => {
            // Core still fans out these deprecated events for raw-event and rollout compatibility
            // consumers;
            // v2 clients receive the canonical FileChange item instead.
        }
        EventMsg::ExecCommandOutputDelta(exec_command_output_delta_event) => {
            let notification = item_event_to_server_notification(
                EventMsg::ExecCommandOutputDelta(exec_command_output_delta_event),
                &conversation_id.to_string(),
                &event_turn_id,
            );
            outgoing.send_server_notification(notification).await;
        }
        // If this is a TurnAborted, reply to any pending interrupt requests.
        EventMsg::TurnAborted(turn_aborted_event) => {
            // All per-thread requests are bound to a turn, so abort them.
            outgoing.abort_pending_server_requests().await;
            respond_to_pending_interrupts(&thread_state, &outgoing).await;

            thread_watch_manager
                .note_turn_interrupted(&conversation_id.to_string())
                .await;
            handle_turn_interrupted(
                conversation_id,
                event_turn_id,
                turn_aborted_event,
                &outgoing,
                &thread_state,
            )
            .await;
        }
        EventMsg::ThreadGoalUpdated(thread_goal_event) => {
            let notification = ThreadGoalUpdatedNotification {
                thread_id: thread_goal_event.thread_id.to_string(),
                turn_id: thread_goal_event.turn_id,
                goal: thread_goal_event.goal.clone().into(),
            };
            outgoing
                .send_global_server_notification(ServerNotification::ThreadGoalUpdated(
                    notification,
                ))
                .await;
        }
        EventMsg::ThreadRolledBack(_) | EventMsg::ThreadQueueChanged(_) => {}
        EventMsg::ThreadSettingsApplied(_) => {
            let (mut config_snapshot, selected_service_tier) =
                conversation.resume_config_snapshot().await;
            config_snapshot.service_tier = selected_service_tier;
            let thread_settings = thread_settings_from_config_snapshot(&config_snapshot);
            let changed = {
                let mut state = thread_state.lock().await;
                state.note_thread_settings(thread_settings.clone())
            };
            if changed {
                outgoing
                    .send_server_notification(ServerNotification::ThreadSettingsUpdated(
                        ThreadSettingsUpdatedNotification {
                            thread_id: conversation_id.to_string(),
                            thread_settings,
                        },
                    ))
                    .await;
            }
        }
        EventMsg::TurnDiff(turn_diff_event) => {
            handle_turn_diff(conversation_id, &event_turn_id, turn_diff_event, &outgoing).await;
        }
        EventMsg::PlanUpdate(plan_update_event) => {
            handle_turn_plan_update(
                conversation_id,
                &event_turn_id,
                plan_update_event,
                &outgoing,
            )
            .await;
        }
        EventMsg::ShutdownComplete => {
            // The proxy waiter may send its resolution after this last core event.
            let approvals = std::mem::take(&mut thread_state.lock().await.network_approvals);
            for pending in approvals.into_values() {
                outgoing.resolve_server_request(pending.request_id).await;
                drop(pending.permission_guard);
            }
            thread_watch_manager
                .note_thread_shutdown(&conversation_id.to_string())
                .await;
        }

        _ => {}
    }
}

async fn handle_turn_diff(
    conversation_id: ThreadId,
    event_turn_id: &str,
    turn_diff_event: TurnDiffEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let notification = TurnDiffUpdatedNotification {
        thread_id: conversation_id.to_string(),
        turn_id: event_turn_id.to_string(),
        diff: turn_diff_event.unified_diff,
    };
    outgoing
        .send_server_notification(ServerNotification::TurnDiffUpdated(notification))
        .await;
}

async fn handle_turn_plan_update(
    conversation_id: ThreadId,
    event_turn_id: &str,
    plan_update_event: UpdatePlanArgs,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    // `update_plan` is a todo/checklist tool; it is not related to plan-mode updates
    let notification = TurnPlanUpdatedNotification {
        thread_id: conversation_id.to_string(),
        turn_id: event_turn_id.to_string(),
        explanation: plan_update_event.explanation,
        plan: plan_update_event
            .plan
            .into_iter()
            .map(TurnPlanStep::from)
            .collect(),
    };
    outgoing
        .send_server_notification(ServerNotification::TurnPlanUpdated(notification))
        .await;
}

struct TurnCompletionMetadata {
    status: TurnStatus,
    root_turn_id: Option<String>,
    error: Option<TurnError>,
    last_agent_message: Option<ThreadItem>,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    duration_ms: Option<i64>,
}

async fn emit_turn_completed_with_status(
    conversation_id: ThreadId,
    event_turn_id: String,
    turn_completion_metadata: TurnCompletionMetadata,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let (items, items_view) = match turn_completion_metadata.last_agent_message {
        Some(item) => (vec![item], TurnItemsView::Summary),
        None => (Vec::new(), TurnItemsView::NotLoaded),
    };
    let notification = TurnCompletedNotification {
        thread_id: conversation_id.to_string(),
        turn: Turn {
            id: event_turn_id,
            root_turn_id: turn_completion_metadata.root_turn_id,
            items,
            items_view,
            error: turn_completion_metadata.error,
            status: turn_completion_metadata.status,
            started_at: turn_completion_metadata.started_at,
            completed_at: turn_completion_metadata.completed_at,
            duration_ms: turn_completion_metadata.duration_ms,
        },
    };
    outgoing
        .send_server_notification(ServerNotification::TurnCompleted(notification))
        .await;
}

async fn apply_canonical_item_completed_side_effects(
    thread_manager: &Arc<ThreadManager>,
    thread_watch_manager: &ThreadWatchManager,
    thread_state: &Arc<Mutex<ThreadState>>,
    event: &mut codex_protocol::protocol::ItemCompletedEvent,
) {
    match &event.item {
        CoreTurnItem::CommandExecution(item) => {
            let observed_start = thread_state
                .lock()
                .await
                .turn_summary
                .command_start_times
                .remove(&item.id);
            // A client can already be displaying the command's approval-time start.
            event.started_at_ms = observed_start.or(event.started_at_ms);
        }
        CoreTurnItem::SubAgentActivity(activity)
            if activity.kind == SubAgentActivityKind::Interrupted =>
        {
            remove_missing_thread_watch(
                thread_manager,
                thread_watch_manager,
                activity.agent_thread_id,
            )
            .await;
        }
        CoreTurnItem::CollabAgentToolCall(item) if item.tool == CoreCollabAgentTool::CloseAgent => {
            for thread_id in &item.receiver_thread_ids {
                remove_missing_thread_watch(thread_manager, thread_watch_manager, *thread_id).await;
            }
        }
        _ => {}
    }
}

async fn remove_missing_thread_watch(
    thread_manager: &Arc<ThreadManager>,
    thread_watch_manager: &ThreadWatchManager,
    thread_id: ThreadId,
) {
    if thread_manager.get_thread(thread_id).await.is_err() {
        thread_watch_manager
            .remove_thread(&thread_id.to_string())
            .await;
    }
}

async fn start_command_execution_item(
    conversation_id: &ThreadId,
    turn_id: String,
    item_id: String,
    started_at_ms: i64,
    details: &CommandExecutionCompletionItem,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) -> bool {
    let first_start = thread_state
        .lock()
        .await
        .turn_summary
        .record_command_start(item_id.clone(), started_at_ms);
    if first_start {
        let notification = ItemStartedNotification::new(
            conversation_id.to_string(),
            turn_id,
            ThreadItemPayload::CommandExecution {
                id: item_id,
                model_context: details.model_context.clone(),
                sandbox_type: None,
                plugin_id: details.plugin_id.clone(),
                script_path: details.script_path.clone(),
                command: details.command.clone(),
                cwd: details.cwd.clone(),
                process_id: None,
                source: CommandExecutionSource::Agent,
                status: CommandExecutionStatus::InProgress,
                command_actions: details.command_actions.clone(),
                aggregated_output: None,
                exit_code: None,
                duration_ms: None,
            },
            started_at_ms,
        );
        outgoing
            .send_server_notification(ServerNotification::ItemStarted(notification))
            .await;
    }
    first_start
}

#[allow(clippy::too_many_arguments)]
async fn complete_command_execution_item(
    conversation_id: &ThreadId,
    turn_id: String,
    item_id: String,
    completion_item: CommandExecutionCompletionItem,
    status: CommandExecutionStatus,
    completed_at_ms: Option<i64>,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let Some(started_at_ms) = thread_state
        .lock()
        .await
        .turn_summary
        .command_start_times
        .remove(&item_id)
    else {
        return;
    };

    let item = ThreadItemPayload::CommandExecution {
        id: item_id,
        model_context: completion_item.model_context,
        sandbox_type: None,
        plugin_id: completion_item.plugin_id,
        script_path: completion_item.script_path,
        command: completion_item.command,
        cwd: completion_item.cwd,
        process_id: None,
        source: CommandExecutionSource::Agent,
        status,
        command_actions: completion_item.command_actions,
        aggregated_output: None,
        exit_code: None,
        duration_ms: None,
    };
    let notification = ItemCompletedNotification::new(
        conversation_id.to_string(),
        turn_id,
        item,
        Some(started_at_ms),
        // Guardian supplies the decision time used on resume. Manual approvals are timed here.
        completed_at_ms.unwrap_or_else(now_unix_timestamp_ms),
    );
    outgoing
        .send_server_notification(ServerNotification::ItemCompleted(notification))
        .await;
}

async fn find_and_remove_turn_summary(
    _conversation_id: ThreadId,
    thread_state: &Arc<Mutex<ThreadState>>,
) -> TurnSummary {
    let mut state = thread_state.lock().await;
    std::mem::take(&mut state.turn_summary)
}

async fn handle_turn_complete(
    conversation_id: ThreadId,
    event_turn_id: String,
    turn_complete_event: TurnCompleteEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let turn_summary = find_and_remove_turn_summary(conversation_id, thread_state).await;

    let (status, error, last_agent_message) = match turn_summary.last_error {
        Some(error) => (TurnStatus::Failed, Some(error), None),
        None => (TurnStatus::Completed, None, turn_summary.last_agent_message),
    };

    emit_turn_completed_with_status(
        conversation_id,
        event_turn_id,
        TurnCompletionMetadata {
            status,
            root_turn_id: turn_complete_event.root_turn_id,
            error,
            last_agent_message,
            started_at: turn_summary.started_at,
            completed_at: turn_complete_event.completed_at,
            duration_ms: turn_complete_event.duration_ms,
        },
        outgoing,
    )
    .await;
}

async fn handle_turn_interrupted(
    conversation_id: ThreadId,
    event_turn_id: String,
    turn_aborted_event: TurnAbortedEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let turn_summary = find_and_remove_turn_summary(conversation_id, thread_state).await;

    emit_turn_completed_with_status(
        conversation_id,
        event_turn_id,
        TurnCompletionMetadata {
            status: TurnStatus::Interrupted,
            root_turn_id: turn_aborted_event.root_turn_id,
            error: turn_aborted_event.error.map(|error| TurnError {
                message: error.message,
                codex_error_info: error.codex_error_info.map(Into::into),
                misalignment: error.misalignment.map(Into::into),
                additional_details: None,
            }),
            last_agent_message: None,
            started_at: turn_summary.started_at,
            completed_at: turn_aborted_event.completed_at,
            duration_ms: turn_aborted_event.duration_ms,
        },
        outgoing,
    )
    .await;
}

async fn respond_to_pending_interrupts(
    thread_state: &Arc<Mutex<ThreadState>>,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let pending = {
        let mut state = thread_state.lock().await;
        std::mem::take(&mut state.pending_interrupts)
    };

    for request_id in pending {
        outgoing
            .send_response(request_id, TurnInterruptResponse {})
            .await;
    }
}

async fn handle_token_count_event(
    conversation_id: ThreadId,
    turn_id: String,
    token_count_event: TokenCountEvent,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    let TokenCountEvent { info, rate_limits } = token_count_event;
    if let Some(token_usage) = info.map(ThreadTokenUsage::from) {
        let notification = ThreadTokenUsageUpdatedNotification {
            thread_id: conversation_id.to_string(),
            turn_id,
            token_usage,
        };
        outgoing
            .send_server_notification(ServerNotification::ThreadTokenUsageUpdated(notification))
            .await;
    }
    if let Some(rate_limits) = rate_limits {
        outgoing
            .send_server_notification(ServerNotification::AccountRateLimitsUpdated(
                AccountRateLimitsUpdatedNotification {
                    rate_limits: rate_limits.into(),
                },
            ))
            .await;
    }
}

async fn handle_error(
    _conversation_id: ThreadId,
    error: TurnError,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    let mut state = thread_state.lock().await;
    state.turn_summary.last_error = Some(error);
}

async fn handle_error_notification(
    conversation_id: ThreadId,
    event_turn_id: &str,
    error: TurnError,
    outgoing: &ThreadScopedOutgoingMessageSender,
    thread_state: &Arc<Mutex<ThreadState>>,
) {
    handle_error(conversation_id, error.clone(), thread_state).await;
    outgoing
        .send_server_notification(ServerNotification::Error(ErrorNotification {
            error,
            will_retry: false,
            thread_id: conversation_id.to_string(),
            turn_id: event_turn_id.to_string(),
        }))
        .await;
}

async fn on_request_user_input_response(
    event_turn_id: String,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<CodexThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    user_input_guard: ThreadWatchActiveGuard,
) {
    let response = receiver.await;
    resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    drop(user_input_guard);
    let value = match response {
        Ok(Ok(value)) => value,
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            let empty = CoreRequestUserInputResponse {
                answers: HashMap::new(),
            };
            if let Err(err) = conversation
                .submit(Op::UserInputAnswer {
                    id: event_turn_id,
                    response: empty,
                })
                .await
            {
                error!("failed to submit UserInputAnswer: {err}");
            }
            return;
        }
        Err(err) => {
            error!("request failed: {err:?}");
            let empty = CoreRequestUserInputResponse {
                answers: HashMap::new(),
            };
            if let Err(err) = conversation
                .submit(Op::UserInputAnswer {
                    id: event_turn_id,
                    response: empty,
                })
                .await
            {
                error!("failed to submit UserInputAnswer: {err}");
            }
            return;
        }
    };

    let response =
        serde_json::from_value::<ToolRequestUserInputResponse>(value).unwrap_or_else(|err| {
            error!("failed to deserialize ToolRequestUserInputResponse: {err}");
            ToolRequestUserInputResponse {
                answers: HashMap::new(),
            }
        });
    let response = CoreRequestUserInputResponse {
        answers: response
            .answers
            .into_iter()
            .map(|(id, answer)| {
                (
                    id,
                    CoreRequestUserInputAnswer {
                        answers: answer.answers,
                    },
                )
            })
            .collect(),
    };

    if let Err(err) = conversation
        .submit(Op::UserInputAnswer {
            id: event_turn_id,
            response,
        })
        .await
    {
        error!("failed to submit UserInputAnswer: {err}");
    }
}

struct PendingMcpElicitationResponse {
    receiver: oneshot::Receiver<ClientRequestResult>,
    user_verification: bool,
    mcp_tool_approval: bool,
}

async fn on_mcp_server_elicitation_response(
    server_name: String,
    request_id: codex_protocol::mcp::RequestId,
    pending_request_id: RequestId,
    pending: PendingMcpElicitationResponse,
    conversation: Arc<CodexThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    permission_guard: ThreadWatchActiveGuard,
) {
    let response = pending.receiver.await;
    resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    drop(permission_guard);
    let interrupted = !matches!(&response, Ok(Ok(_)));
    let mut response = if pending.user_verification {
        crate::user_verification_response::from_client_result(response)
    } else {
        mcp_server_elicitation_response_from_client_result(response)
    };

    if pending.mcp_tool_approval
        && interrupted
        && response.action == McpServerElicitationAction::Cancel
    {
        // Only the synthesized response is ours to attribute. Successful client
        // replies retain their provenance, including an absent reason.
        let mut meta = match response.meta.take() {
            Some(serde_json::Value::Object(meta)) => meta,
            _ => serde_json::Map::new(),
        };
        meta.insert(
            CANCELLATION_REASON_KEY.to_string(),
            serde_json::json!(McpApprovalCancellationReason::Interrupted),
        );
        response.meta = Some(serde_json::Value::Object(meta));
    }

    if let Err(err) = conversation
        .submit(Op::ResolveElicitation {
            server_name,
            request_id,
            decision: response.action.to_core(),
            content: response.content,
            meta: response.meta,
        })
        .await
    {
        error!("failed to submit ResolveElicitation: {err}");
    }
}

fn mcp_server_elicitation_response_from_client_result(
    response: std::result::Result<ClientRequestResult, oneshot::error::RecvError>,
) -> McpServerElicitationRequestResponse {
    match response {
        Ok(Ok(value)) => serde_json::from_value::<McpServerElicitationRequestResponse>(value)
            .unwrap_or_else(|err| {
                error!("failed to deserialize McpServerElicitationRequestResponse: {err}");
                McpServerElicitationRequestResponse {
                    action: McpServerElicitationAction::Decline,
                    content: None,
                    meta: None,
                }
            }),
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => {
            McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Cancel,
                content: None,
                meta: None,
            }
        }
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Decline,
                content: None,
                meta: None,
            }
        }
        Err(err) => {
            error!("request failed: {err:?}");
            McpServerElicitationRequestResponse {
                action: McpServerElicitationAction::Decline,
                content: None,
                meta: None,
            }
        }
    }
}

async fn on_request_permissions_response(
    pending_response: PendingRequestPermissionsResponse,
    conversation: Arc<CodexThread>,
    thread_state: Arc<Mutex<ThreadState>>,
) {
    let PendingRequestPermissionsResponse {
        call_id,
        conversation_id,
        turn_id,
        pending_request_id,
        outgoing,
        receiver,
        request_permissions_guard,
    } = pending_response;
    let response = receiver.await;
    resolve_server_request_on_thread_listener(&thread_state, pending_request_id.clone()).await;
    drop(request_permissions_guard);
    let response = match request_permissions_response_from_client_result(response) {
        Ok(Some(response)) => response,
        Ok(None) => return,
        // TODO(anp): Remove this native-path localization error path once core permission paths
        // remain PathUri after crossing the app-server boundary.
        Err(err) => {
            let message = format!("failed to localize granted filesystem paths: {err}");
            handle_error_notification(
                conversation_id,
                &turn_id,
                TurnError {
                    misalignment: None,
                    message,
                    codex_error_info: None,
                    additional_details: None,
                },
                &outgoing,
                &thread_state,
            )
            .await;
            if let Err(err) = conversation.submit(Op::Interrupt).await {
                error!("failed to interrupt turn after invalid permission paths: {err}");
            }
            return;
        }
    };
    outgoing.track_effective_permissions_approval_response(pending_request_id, response.clone());

    if let Err(err) = conversation
        .submit(Op::RequestPermissionsResponse {
            id: call_id,
            response,
        })
        .await
    {
        error!("failed to submit RequestPermissionsResponse: {err}");
    }
}

struct PendingRequestPermissionsResponse {
    call_id: String,
    conversation_id: ThreadId,
    turn_id: String,
    pending_request_id: RequestId,
    outgoing: ThreadScopedOutgoingMessageSender,
    receiver: oneshot::Receiver<ClientRequestResult>,
    request_permissions_guard: ThreadWatchActiveGuard,
}

fn request_permissions_response_from_client_result(
    response: std::result::Result<ClientRequestResult, oneshot::error::RecvError>,
) -> std::io::Result<Option<CoreRequestPermissionsResponse>> {
    let value = match response {
        Ok(Ok(value)) => value,
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return Ok(None),
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            return Ok(Some(CoreRequestPermissionsResponse {
                permissions: Default::default(),
                scope: CorePermissionGrantScope::Turn,
                strict_auto_review: false,
            }));
        }
        Err(err) => {
            error!("request failed: {err:?}");
            return Ok(Some(CoreRequestPermissionsResponse {
                permissions: Default::default(),
                scope: CorePermissionGrantScope::Turn,
                strict_auto_review: false,
            }));
        }
    };

    let response = serde_json::from_value::<PermissionsRequestApprovalResponse>(value)
        .unwrap_or_else(|err| {
            error!("failed to deserialize PermissionsRequestApprovalResponse: {err}");
            PermissionsRequestApprovalResponse {
                permissions: V2GrantedPermissionProfile::default(),
                scope: codex_app_server_protocol::PermissionGrantScope::Turn,
                strict_auto_review: None,
            }
        });
    let strict_auto_review = response.strict_auto_review.unwrap_or(false);
    if strict_auto_review
        && matches!(
            response.scope,
            codex_app_server_protocol::PermissionGrantScope::Session
        )
    {
        error!("strict auto review is only supported for turn-scoped permission grants");
        return Ok(Some(CoreRequestPermissionsResponse {
            permissions: Default::default(),
            scope: CorePermissionGrantScope::Turn,
            strict_auto_review: false,
        }));
    }
    let granted_permissions: CoreAdditionalPermissionProfile = response.permissions.try_into()?;
    // Core intersects with the request using the originating environment's policy context.
    Ok(Some(CoreRequestPermissionsResponse {
        permissions: CoreRequestPermissionProfile::from(granted_permissions),
        scope: response.scope.to_core(),
        strict_auto_review,
    }))
}

fn map_file_change_approval_decision(decision: FileChangeApprovalDecision) -> ReviewDecision {
    match decision {
        FileChangeApprovalDecision::Accept => ReviewDecision::Approved,
        FileChangeApprovalDecision::AcceptForSession => ReviewDecision::ApprovedForSession,
        FileChangeApprovalDecision::Decline => ReviewDecision::denied("rejected by user"),
        FileChangeApprovalDecision::Cancel => ReviewDecision::Abort,
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_file_change_request_approval_response(
    item_id: String,
    turn_id: String,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    codex: Arc<CodexThread>,
    thread_state: Arc<Mutex<ThreadState>>,
    permission_guard: ThreadWatchActiveGuard,
) {
    let response = receiver.await;
    resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
    drop(permission_guard);
    let decision = match response {
        Ok(Ok(value)) => match serde_json::from_value::<FileChangeRequestApprovalResponse>(value) {
            Ok(response) => map_file_change_approval_decision(response.decision),
            Err(err) => {
                error!("failed to deserialize FileChangeRequestApprovalResponse: {err}");
                ReviewDecision::denied("approval request failed")
            }
        },
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            ReviewDecision::denied("approval request failed")
        }
        Err(err) => {
            error!("request failed: {err:?}");
            ReviewDecision::denied("approval request failed")
        }
    };

    let op = match decision {
        ReviewDecision::Abort => Op::CancelPatchApproval {
            id: item_id,
            turn_id,
        },
        decision => Op::PatchApproval {
            id: item_id,
            decision,
        },
    };
    if let Err(err) = codex.submit(op).await {
        error!("failed to submit PatchApproval: {err}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn on_command_execution_request_approval_response(
    event_turn_id: String,
    conversation_id: ThreadId,
    approval_id: Option<String>,
    item_id: String,
    completion_item: Option<CommandExecutionCompletionItem>,
    pending_request_id: RequestId,
    receiver: oneshot::Receiver<ClientRequestResult>,
    conversation: Arc<CodexThread>,
    outgoing: ThreadScopedOutgoingMessageSender,
    thread_state: Arc<Mutex<ThreadState>>,
    permission_guard: Option<ThreadWatchActiveGuard>,
) {
    let response = receiver.await;
    // Core resolves network prompts in event order, including when the execution disappears.
    if let Some(permission_guard) = permission_guard {
        resolve_server_request_on_thread_listener(&thread_state, pending_request_id).await;
        drop(permission_guard);
    } else if response.is_err()
        && !thread_state
            .lock()
            .await
            .network_approvals
            .contains_key(approval_id.as_ref().unwrap_or(&item_id))
    {
        return;
    }
    let (decision, completion_status) = match response {
        Ok(Ok(value)) => {
            match serde_json::from_value::<CommandExecutionRequestApprovalResponse>(value) {
                Ok(response) => match response.decision {
                    CommandExecutionApprovalDecision::Accept => (ReviewDecision::Approved, None),
                    CommandExecutionApprovalDecision::AcceptForSession => {
                        (ReviewDecision::ApprovedForSession, None)
                    }
                    CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
                        execpolicy_amendment,
                    } => (
                        ReviewDecision::ApprovedExecpolicyAmendment {
                            proposed_execpolicy_amendment: execpolicy_amendment.into_core(),
                        },
                        None,
                    ),
                    CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                        network_policy_amendment,
                    } => {
                        let completion_status = match network_policy_amendment.action {
                            V2NetworkPolicyRuleAction::Allow => None,
                            V2NetworkPolicyRuleAction::Deny => {
                                Some(CommandExecutionStatus::Declined)
                            }
                        };
                        (
                            ReviewDecision::NetworkPolicyAmendment {
                                network_policy_amendment: network_policy_amendment.into_core(),
                            },
                            completion_status,
                        )
                    }
                    CommandExecutionApprovalDecision::Decline => (
                        ReviewDecision::denied("rejected by user"),
                        Some(CommandExecutionStatus::Declined),
                    ),
                    CommandExecutionApprovalDecision::Cancel => (
                        ReviewDecision::Abort,
                        Some(CommandExecutionStatus::Declined),
                    ),
                },
                Err(err) => {
                    error!("failed to deserialize CommandExecutionRequestApprovalResponse: {err}");
                    (
                        ReviewDecision::denied("approval request failed"),
                        Some(CommandExecutionStatus::Failed),
                    )
                }
            }
        }
        Ok(Err(err)) if is_turn_transition_server_request_error(&err) => return,
        Ok(Err(err)) => {
            error!("request failed with client error: {err:?}");
            (
                ReviewDecision::denied("approval request failed"),
                Some(CommandExecutionStatus::Failed),
            )
        }
        Err(err) => {
            error!("request failed: {err:?}");
            (
                ReviewDecision::denied("approval request failed"),
                Some(CommandExecutionStatus::Failed),
            )
        }
    };

    let suppress_subcommand_completion_item = {
        // For regular shell/unified_exec approvals, approval_id is null.
        // For zsh-fork subcommand approvals, approval_id is present and
        // item_id points to the parent command item.
        if approval_id.is_some() {
            let state = thread_state.lock().await;
            state
                .turn_summary
                .command_start_times
                .contains_key(&item_id)
        } else {
            false
        }
    };

    if let Some(status) = completion_status
        && !suppress_subcommand_completion_item
        && let Some(completion_item) = completion_item
    {
        complete_command_execution_item(
            &conversation_id,
            event_turn_id.clone(),
            item_id.clone(),
            completion_item,
            status,
            /*completed_at_ms*/ None,
            &outgoing,
            &thread_state,
        )
        .await;
    }

    if let Err(err) = conversation
        .submit(Op::ExecApproval {
            id: approval_id.unwrap_or_else(|| item_id.clone()),
            turn_id: Some(event_turn_id),
            decision,
        })
        .await
    {
        error!("failed to submit ExecApproval: {err}");
    }
}

fn now_unix_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "bespoke_event_handling_tests.rs"]
mod tests;
