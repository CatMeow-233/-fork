//! Approval reviews: approval requests/responses, guardian reviews and review summaries.

use super::*;

impl AnalyticsReducer {
    pub(super) fn ingest_guardian_review(
        &mut self,
        input: GuardianReviewEventParams,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some((connection_state, thread_state, thread_metadata)) =
            self.thread_context_or_warn(AnalyticsDropSite::guardian(&input))
        else {
            return;
        };
        out.push(TrackEventRequest::GuardianReview(Box::new(
            GuardianReviewEventRequest {
                event_type: "codex_guardian_review",
                event_params: GuardianReviewEventPayload {
                    session_id: thread_metadata.session_id.clone(),
                    app_server_client: thread_state.app_server_client(connection_state),
                    runtime: connection_state.runtime.clone(),
                    guardian_review: input,
                },
            },
        )));
    }

    pub(super) fn ingest_server_request(&mut self, _connection_id: u64, request: ServerRequest) {
        match request {
            ServerRequest::CommandExecutionRequestApproval { request_id, params } => {
                let is_stdin_review = match params.kind {
                    CommandExecutionApprovalKind::WriteStdin => true,
                    CommandExecutionApprovalKind::Command
                    | CommandExecutionApprovalKind::HumanCommand => false,
                };
                let is_network_access_review = params.network_approval_context.is_some();
                let requested_network_access = is_network_access_review
                    || params
                        .proposed_network_policy_amendments
                        .as_ref()
                        .is_some_and(|amendments| !amendments.is_empty())
                    || params
                        .additional_permissions
                        .as_ref()
                        .and_then(|permissions| permissions.network.as_ref())
                        .and_then(|network| network.enabled)
                        .unwrap_or(false);
                let requested_additional_permissions = params.additional_permissions.is_some();
                let trigger = if is_stdin_review {
                    ReviewTrigger::Initial
                } else if params.kind != CommandExecutionApprovalKind::HumanCommand
                    && params.approval_id.is_some()
                {
                    ReviewTrigger::ExecveIntercept
                } else if requested_network_access {
                    ReviewTrigger::NetworkPolicyDenial
                } else if requested_additional_permissions {
                    ReviewTrigger::SandboxDenial
                } else {
                    ReviewTrigger::Initial
                };
                let Some(started_at_ms) = option_i64_to_u64(Some(params.started_at_ms)) else {
                    return;
                };
                self.turn_state(params.turn_id.clone()).thread_id = Some(params.thread_id.clone());
                self.pending_reviews.insert(
                    request_id.clone(),
                    PendingReviewState {
                        thread_id: params.thread_id,
                        turn_id: params.turn_id,
                        item_id: Some(params.item_id),
                        review_id: user_review_id(&request_id),
                        subject_kind: if is_stdin_review {
                            ReviewSubjectKind::WriteStdin
                        } else if is_network_access_review {
                            ReviewSubjectKind::NetworkAccess
                        } else {
                            ReviewSubjectKind::CommandExecution
                        },
                        subject_name: if is_stdin_review {
                            "write_stdin".to_string()
                        } else if is_network_access_review {
                            "network_access".to_string()
                        } else {
                            "command_execution".to_string()
                        },
                        trigger,
                        started_at_ms,
                        requested_additional_permissions,
                        requested_network_access,
                    },
                );
            }
            ServerRequest::FileChangeRequestApproval { request_id, params } => {
                let requested_additional_permissions = params.grant_root.is_some();
                let Some(started_at_ms) = option_i64_to_u64(Some(params.started_at_ms)) else {
                    return;
                };
                self.turn_state(params.turn_id.clone()).thread_id = Some(params.thread_id.clone());
                self.pending_reviews.insert(
                    request_id.clone(),
                    PendingReviewState {
                        thread_id: params.thread_id,
                        turn_id: params.turn_id,
                        item_id: Some(params.item_id),
                        review_id: user_review_id(&request_id),
                        subject_kind: ReviewSubjectKind::FileChange,
                        subject_name: "apply_patch".to_string(),
                        trigger: if requested_additional_permissions {
                            ReviewTrigger::SandboxDenial
                        } else {
                            ReviewTrigger::Initial
                        },
                        started_at_ms,
                        requested_additional_permissions,
                        requested_network_access: false,
                    },
                );
            }
            ServerRequest::PermissionsRequestApproval { request_id, params } => {
                let requested_network_access = params
                    .permissions
                    .network
                    .as_ref()
                    .and_then(|network| network.enabled)
                    .unwrap_or(false);
                let requested_additional_permissions =
                    requested_network_access || params.permissions.file_system.is_some();
                let trigger = if requested_network_access {
                    ReviewTrigger::NetworkPolicyDenial
                } else if requested_additional_permissions {
                    ReviewTrigger::SandboxDenial
                } else {
                    ReviewTrigger::Initial
                };
                let Some(started_at_ms) = option_i64_to_u64(Some(params.started_at_ms)) else {
                    return;
                };
                self.turn_state(params.turn_id.clone()).thread_id = Some(params.thread_id.clone());
                self.pending_reviews.insert(
                    request_id.clone(),
                    PendingReviewState {
                        thread_id: params.thread_id,
                        turn_id: params.turn_id,
                        item_id: Some(params.item_id),
                        review_id: user_review_id(&request_id),
                        subject_kind: ReviewSubjectKind::Permissions,
                        subject_name: "permissions".to_string(),
                        trigger,
                        started_at_ms,
                        requested_additional_permissions,
                        requested_network_access,
                    },
                );
            }
            _ => {}
        }
    }

    pub(super) fn ingest_server_response(
        &mut self,
        completed_at_ms: u64,
        response: ServerResponse,
        out: &mut Vec<TrackEventRequest>,
    ) {
        match response {
            ServerResponse::CommandExecutionRequestApproval {
                request_id,
                response,
            } => {
                let Some(pending_review) = self.pending_reviews.remove(&request_id) else {
                    return;
                };
                let (status, resolution) = command_execution_review_result(response.decision);
                self.emit_review_event(
                    pending_review,
                    Reviewer::User,
                    status,
                    resolution,
                    completed_at_ms,
                    out,
                );
            }
            ServerResponse::FileChangeRequestApproval {
                request_id,
                response,
            } => {
                let Some(pending_review) = self.pending_reviews.remove(&request_id) else {
                    return;
                };
                let (status, resolution) = file_change_review_result(response.decision);
                self.emit_review_event(
                    pending_review,
                    Reviewer::User,
                    status,
                    resolution,
                    completed_at_ms,
                    out,
                );
            }
            _ => {}
        }
    }

    pub(super) fn ingest_effective_permissions_approval_response(
        &mut self,
        completed_at_ms: u64,
        request_id: RequestId,
        response: CoreRequestPermissionsResponse,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some(pending_review) = self.pending_reviews.remove(&request_id) else {
            return;
        };
        let (status, resolution) = effective_permissions_review_result(&response);
        self.emit_review_event(
            pending_review,
            Reviewer::User,
            status,
            resolution,
            completed_at_ms,
            out,
        );
    }

    pub(super) fn ingest_server_request_aborted(
        &mut self,
        completed_at_ms: u64,
        request_id: RequestId,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some(pending_review) = self.pending_reviews.remove(&request_id) else {
            return;
        };
        self.emit_review_event(
            pending_review,
            Reviewer::User,
            ReviewStatus::Aborted,
            ReviewResolution::None,
            completed_at_ms,
            out,
        );
    }

    pub(super) fn ingest_guardian_review_completed(
        &mut self,
        notification: codex_app_server_protocol::ItemGuardianApprovalReviewCompletedNotification,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some((status, resolution)) = guardian_review_result(notification.review.status) else {
            return;
        };
        let (subject_kind, subject_name, trigger) =
            guardian_review_subject_metadata(&notification.action);
        let Some(started_at_ms) = option_i64_to_u64(Some(notification.started_at_ms)) else {
            return;
        };
        let pending_review = PendingReviewState {
            thread_id: notification.thread_id,
            turn_id: notification.turn_id,
            item_id: notification.target_item_id,
            review_id: notification.review_id,
            subject_kind,
            subject_name,
            trigger,
            started_at_ms,
            requested_additional_permissions: guardian_review_requested_additional_permissions(
                &notification.action,
            ),
            requested_network_access: guardian_review_requested_network_access(
                &notification.action,
            ),
        };
        let Some(completed_at_ms) = option_i64_to_u64(Some(notification.completed_at_ms)) else {
            return;
        };
        self.emit_review_event(
            pending_review,
            Reviewer::Guardian,
            status,
            resolution,
            completed_at_ms,
            out,
        );
    }

    fn emit_review_event(
        &mut self,
        pending_review: PendingReviewState,
        reviewer: Reviewer,
        status: ReviewStatus,
        resolution: ReviewResolution,
        completed_at_ms: u64,
        out: &mut Vec<TrackEventRequest>,
    ) {
        if let Some(item_key) = item_review_summary_key(&pending_review) {
            self.record_item_review_summary(
                item_key,
                reviewer,
                status,
                resolution,
                &pending_review,
            );
        }
        let Some((connection_state, thread_state, thread_metadata)) =
            self.thread_context_or_warn(AnalyticsDropSite::review(&pending_review))
        else {
            return;
        };
        out.push(TrackEventRequest::ReviewEvent(CodexReviewEventRequest {
            event_type: "codex_review_event",
            event_params: CodexReviewEventParams {
                thread_id: pending_review.thread_id,
                turn_id: pending_review.turn_id,
                item_id: pending_review.item_id,
                review_id: pending_review.review_id,
                app_server_client: thread_state.app_server_client(connection_state),
                runtime: connection_state.runtime.clone(),
                thread_source: thread_metadata.thread_source.clone(),
                subagent_source: thread_metadata.subagent_source.clone(),
                parent_thread_id: thread_metadata.parent_thread_id.clone(),
                subject_kind: pending_review.subject_kind,
                subject_name: pending_review.subject_name,
                reviewer,
                trigger: pending_review.trigger,
                status,
                resolution,
                started_at_ms: pending_review.started_at_ms,
                completed_at_ms,
                duration_ms: observed_duration_ms(pending_review.started_at_ms, completed_at_ms),
            },
        }));
    }

    fn record_item_review_summary(
        &mut self,
        item_key: ToolItemKey,
        reviewer: Reviewer,
        status: ReviewStatus,
        resolution: ReviewResolution,
        pending_review: &PendingReviewState,
    ) {
        let summary = self.item_review_summaries.entry(item_key).or_default();
        summary.review_count += 1;
        match reviewer {
            Reviewer::Guardian => summary.guardian_review_count += 1,
            Reviewer::User => summary.user_review_count += 1,
        }
        summary.final_approval_outcome = Some(final_approval_outcome(reviewer, status, resolution));
        summary.requested_additional_permissions |= pending_review.requested_additional_permissions;
        summary.requested_network_access |= pending_review.requested_network_access;
    }
}

fn item_review_summary_key(pending_review: &PendingReviewState) -> Option<ToolItemKey> {
    match pending_review.subject_kind {
        ReviewSubjectKind::CommandExecution
        | ReviewSubjectKind::FileChange
        | ReviewSubjectKind::McpToolCall => Some(ToolItemKey {
            thread_id: pending_review.thread_id.clone(),
            turn_id: pending_review.turn_id.clone(),
            item_id: pending_review.item_id.clone()?,
        }),
        ReviewSubjectKind::WriteStdin
        | ReviewSubjectKind::Permissions
        | ReviewSubjectKind::NetworkAccess => None,
    }
}

fn user_review_id(request_id: &RequestId) -> String {
    format!("user:{request_id}")
}

fn command_execution_review_result(
    decision: CommandExecutionApprovalDecision,
) -> (ReviewStatus, ReviewResolution) {
    match decision {
        CommandExecutionApprovalDecision::Accept => {
            (ReviewStatus::Approved, ReviewResolution::None)
        }
        CommandExecutionApprovalDecision::AcceptForSession => {
            (ReviewStatus::Approved, ReviewResolution::SessionApproval)
        }
        CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment { .. } => (
            ReviewStatus::Approved,
            ReviewResolution::ExecPolicyAmendment,
        ),
        CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
            network_policy_amendment,
        } => match network_policy_amendment.action {
            NetworkPolicyRuleAction::Allow => (
                ReviewStatus::Approved,
                ReviewResolution::NetworkPolicyAmendment,
            ),
            NetworkPolicyRuleAction::Deny => (
                ReviewStatus::Denied,
                ReviewResolution::NetworkPolicyAmendment,
            ),
        },
        CommandExecutionApprovalDecision::Decline => (ReviewStatus::Denied, ReviewResolution::None),
        CommandExecutionApprovalDecision::Cancel => (ReviewStatus::Aborted, ReviewResolution::None),
    }
}

fn file_change_review_result(
    decision: FileChangeApprovalDecision,
) -> (ReviewStatus, ReviewResolution) {
    match decision {
        FileChangeApprovalDecision::Accept => (ReviewStatus::Approved, ReviewResolution::None),
        FileChangeApprovalDecision::AcceptForSession => {
            (ReviewStatus::Approved, ReviewResolution::SessionApproval)
        }
        FileChangeApprovalDecision::Decline => (ReviewStatus::Denied, ReviewResolution::None),
        FileChangeApprovalDecision::Cancel => (ReviewStatus::Aborted, ReviewResolution::None),
    }
}

fn effective_permissions_review_result(
    response: &CoreRequestPermissionsResponse,
) -> (ReviewStatus, ReviewResolution) {
    if response.permissions.is_empty() {
        return (ReviewStatus::Denied, ReviewResolution::None);
    }

    match response.scope {
        CorePermissionGrantScope::Turn => (ReviewStatus::Approved, ReviewResolution::None),
        CorePermissionGrantScope::Session => {
            (ReviewStatus::Approved, ReviewResolution::SessionApproval)
        }
    }
}

pub(super) fn guardian_review_result(
    status: GuardianApprovalReviewStatus,
) -> Option<(ReviewStatus, ReviewResolution)> {
    match status {
        GuardianApprovalReviewStatus::InProgress => None,
        GuardianApprovalReviewStatus::Approved => {
            Some((ReviewStatus::Approved, ReviewResolution::None))
        }
        GuardianApprovalReviewStatus::Denied => {
            Some((ReviewStatus::Denied, ReviewResolution::None))
        }
        GuardianApprovalReviewStatus::TimedOut => {
            Some((ReviewStatus::TimedOut, ReviewResolution::None))
        }
        GuardianApprovalReviewStatus::Aborted => {
            Some((ReviewStatus::Aborted, ReviewResolution::None))
        }
    }
}

fn guardian_review_subject_metadata(
    action: &GuardianApprovalReviewAction,
) -> (ReviewSubjectKind, String, ReviewTrigger) {
    match action {
        GuardianApprovalReviewAction::Command { .. } => (
            ReviewSubjectKind::CommandExecution,
            "command_execution".to_string(),
            ReviewTrigger::Initial,
        ),
        GuardianApprovalReviewAction::WriteStdin { .. } => (
            ReviewSubjectKind::WriteStdin,
            "write_stdin".to_string(),
            ReviewTrigger::Initial,
        ),
        GuardianApprovalReviewAction::Execve { .. } => (
            ReviewSubjectKind::CommandExecution,
            "command_execution".to_string(),
            ReviewTrigger::ExecveIntercept,
        ),
        GuardianApprovalReviewAction::ApplyPatch { .. } => (
            ReviewSubjectKind::FileChange,
            "apply_patch".to_string(),
            ReviewTrigger::SandboxDenial,
        ),
        GuardianApprovalReviewAction::NetworkAccess { .. } => (
            ReviewSubjectKind::NetworkAccess,
            "network_access".to_string(),
            ReviewTrigger::NetworkPolicyDenial,
        ),
        GuardianApprovalReviewAction::RequestPermissions { permissions, .. } => {
            let requested_network_access = permissions
                .network
                .as_ref()
                .and_then(|network| network.enabled)
                .unwrap_or(false);
            let trigger = if requested_network_access {
                ReviewTrigger::NetworkPolicyDenial
            } else if permissions.file_system.is_some() {
                ReviewTrigger::SandboxDenial
            } else {
                ReviewTrigger::Initial
            };
            (
                ReviewSubjectKind::Permissions,
                "permissions".to_string(),
                trigger,
            )
        }
        GuardianApprovalReviewAction::McpToolCall { tool_name, .. } => (
            ReviewSubjectKind::McpToolCall,
            tool_name.clone(),
            ReviewTrigger::Initial,
        ),
    }
}

fn guardian_review_requested_additional_permissions(action: &GuardianApprovalReviewAction) -> bool {
    match action {
        GuardianApprovalReviewAction::ApplyPatch { .. }
        | GuardianApprovalReviewAction::NetworkAccess { .. } => true,
        GuardianApprovalReviewAction::RequestPermissions { permissions, .. } => {
            guardian_review_request_permissions_network_enabled(permissions)
                || permissions.file_system.is_some()
        }
        GuardianApprovalReviewAction::Command { .. }
        | GuardianApprovalReviewAction::WriteStdin { .. }
        | GuardianApprovalReviewAction::Execve { .. }
        | GuardianApprovalReviewAction::McpToolCall { .. } => false,
    }
}

fn guardian_review_requested_network_access(action: &GuardianApprovalReviewAction) -> bool {
    match action {
        GuardianApprovalReviewAction::NetworkAccess { .. } => true,
        GuardianApprovalReviewAction::RequestPermissions { permissions, .. } => {
            guardian_review_request_permissions_network_enabled(permissions)
        }
        GuardianApprovalReviewAction::ApplyPatch { .. }
        | GuardianApprovalReviewAction::Command { .. }
        | GuardianApprovalReviewAction::WriteStdin { .. }
        | GuardianApprovalReviewAction::Execve { .. }
        | GuardianApprovalReviewAction::McpToolCall { .. } => false,
    }
}

fn guardian_review_request_permissions_network_enabled(
    permissions: &RequestPermissionProfile,
) -> bool {
    permissions
        .network
        .as_ref()
        .and_then(|network| network.enabled)
        .unwrap_or(false)
}

fn final_approval_outcome(
    reviewer: Reviewer,
    status: ReviewStatus,
    resolution: ReviewResolution,
) -> FinalApprovalOutcome {
    match (reviewer, status, resolution) {
        (Reviewer::Guardian, ReviewStatus::Approved, _) => FinalApprovalOutcome::GuardianApproved,
        (Reviewer::Guardian, ReviewStatus::Denied, _) => FinalApprovalOutcome::GuardianDenied,
        (Reviewer::Guardian, _, _) => FinalApprovalOutcome::GuardianAborted,
        (Reviewer::User, ReviewStatus::Approved, ReviewResolution::SessionApproval) => {
            FinalApprovalOutcome::UserApprovedForSession
        }
        (Reviewer::User, ReviewStatus::Approved, _) => FinalApprovalOutcome::UserApproved,
        (Reviewer::User, ReviewStatus::Denied, _) => FinalApprovalOutcome::UserDenied,
        (Reviewer::User, _, _) => FinalApprovalOutcome::UserAborted,
    }
}
