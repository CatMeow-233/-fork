//! Mapping turn events to the text forwarded to the realtime conversation.

use super::*;

pub(in crate::session) fn realtime_text_for_event(msg: &EventMsg) -> Option<RealtimeEventText> {
    match msg {
        EventMsg::ElicitationRequest(request)
            if matches!(
                &request.request,
                codex_protocol::approvals::ElicitationRequest::UserVerification { .. }
            ) =>
        {
            Some(RealtimeEventText::Handoff(
                UserVerificationNotice
                    .render()
                    .into_text()
                    .unwrap_or_default(),
                None,
            ))
        }
        EventMsg::AgentMessage(event) => Some(RealtimeEventText::Handoff(
            event.message.clone(),
            event.phase.clone(),
        )),
        EventMsg::ItemCompleted(event) => match &event.item {
            TurnItem::AgentMessage(item) => Some(RealtimeEventText::Handoff(
                agent_message_text(item),
                item.phase.clone(),
            )),
            TurnItem::Reasoning(item) => item
                .summary_text
                .iter()
                .rev()
                .map(|summary| summary.trim())
                .find(|summary| !summary.is_empty())
                .map(|summary| RealtimeEventText::QuietReasoning(summary.to_owned())),
            _ => None,
        },
        EventMsg::ExecApprovalRequest(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::RequestUserInput(_)
        | EventMsg::ElicitationRequest(_) => {
            let message = if matches!(
                msg,
                EventMsg::RequestUserInput(_) | EventMsg::ElicitationRequest(_)
            ) {
                "I need your input. Please respond in the app."
            } else {
                "I need your approval to continue. Please review the request in the app."
            };
            serde_json::to_string(msg)
                .ok()
                .map(|request| RealtimeEventText::Handoff(format!("{message}\n\n{request}"), None))
        }
        EventMsg::Error(_)
        | EventMsg::Warning(_)
        | EventMsg::AuthRecoveryStarted(_)
        | EventMsg::AuthRecoveryCompleted(_)
        | EventMsg::GuardianWarning(_)
        | EventMsg::RealtimeConversationStarted(_)
        | EventMsg::RealtimeConversationSdp(_)
        | EventMsg::RealtimeConversationRealtime(_)
        | EventMsg::RealtimeConversationClosed(_)
        | EventMsg::ModelReroute(_)
        | EventMsg::ModelVerification(_)
        | EventMsg::TurnModerationMetadata(_)
        | EventMsg::SafetyBuffering(_)
        | EventMsg::ContextCompacted(_)
        | EventMsg::ThreadRolledBack(_)
        | EventMsg::TurnStarted(_)
        | EventMsg::ThreadSettingsApplied(_)
        | EventMsg::TurnComplete(_)
        | EventMsg::TokenCount(_)
        | EventMsg::UserMessage(_)
        | EventMsg::AgentReasoning(_)
        | EventMsg::AgentReasoningRawContent(_)
        | EventMsg::AgentReasoningSectionBreak(_)
        | EventMsg::SessionConfigured(_)
        | EventMsg::EnvironmentConnected(_)
        | EventMsg::EnvironmentDisconnected(_)
        | EventMsg::ThreadGoalUpdated(_)
        | EventMsg::ThreadQueueChanged(_)
        | EventMsg::McpStartupUpdate(_)
        | EventMsg::McpStartupComplete(_)
        | EventMsg::McpToolCallBegin(_)
        | EventMsg::McpToolCallEnd(_)
        | EventMsg::ElicitationAbandoned(_)
        | EventMsg::NetworkApprovalResolved { .. }
        | EventMsg::WebSearchBegin(_)
        | EventMsg::WebSearchEnd(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::ExecCommandOutputDelta(_)
        | EventMsg::TerminalInteraction(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::PatchApplyBegin(_)
        | EventMsg::PatchApplyUpdated(_)
        | EventMsg::PatchApplyEnd(_)
        | EventMsg::ImageGenerationBegin(_)
        | EventMsg::ImageGenerationEnd(_)
        | EventMsg::ViewImageToolCall(_)
        | EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_)
        | EventMsg::GuardianAssessment(_)
        | EventMsg::DeprecationNotice(_)
        | EventMsg::StreamError(_)
        | EventMsg::TurnDiff(_)
        | EventMsg::RealtimeConversationListVoicesResponse(_)
        | EventMsg::PlanUpdate(_)
        | EventMsg::TurnAborted(_)
        | EventMsg::ShutdownComplete
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_)
        | EventMsg::RawResponseItem(_)
        | EventMsg::PrivateSessionEvent(_)
        | EventMsg::RawResponseCompleted(_)
        | EventMsg::ItemStarted(_)
        | EventMsg::HookStarted(_)
        | EventMsg::HookCompleted(_)
        | EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
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
        | EventMsg::SubAgentActivity(_) => None,
    }
}
