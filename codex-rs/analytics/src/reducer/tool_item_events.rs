//! Builders for completed tool item events and their outcome, count and attribution helpers.

use super::*;

pub(crate) fn tracked_tool_item_id(item: &ThreadItemPayload) -> Option<&str> {
    match item {
        ThreadItemPayload::CommandExecution { id, .. }
        | ThreadItemPayload::FileChange { id, .. }
        | ThreadItemPayload::McpToolCall { id, .. }
        | ThreadItemPayload::DynamicToolCall { id, .. }
        | ThreadItemPayload::CollabAgentToolCall { id, .. } => Some(id),
        ThreadItemPayload::WebSearch(item) => Some(&item.id),
        ThreadItemPayload::ImageGeneration(item) => Some(&item.id),
        ThreadItemPayload::UserMessage { .. }
        | ThreadItemPayload::HookPrompt { .. }
        | ThreadItemPayload::AgentMessage { .. }
        | ThreadItemPayload::FunctionCallOutput { .. }
        | ThreadItemPayload::Plan { .. }
        | ThreadItemPayload::Reasoning { .. }
        | ThreadItemPayload::SubAgentActivity { .. }
        | ThreadItemPayload::ImageView { .. }
        | ThreadItemPayload::Sleep(_)
        | ThreadItemPayload::EnteredReviewMode { .. }
        | ThreadItemPayload::ExitedReviewMode { .. }
        | ThreadItemPayload::ContextCompaction { .. } => None,
    }
}

pub(super) fn cua_mcp_tool_identity(
    item: &ThreadItemPayload,
) -> Option<(&str, &str, Option<&str>, Option<&str>)> {
    let ThreadItemPayload::McpToolCall {
        server,
        tool,
        plugin_id,
        app_context,
        ..
    } = item
    else {
        return None;
    };
    let connector_id = app_context
        .as_ref()
        .map(|app_context| app_context.connector_id.as_str());
    (tool == "js" && is_node_repl_backed_connector(server, connector_id)).then_some((
        server,
        tool,
        plugin_id.as_deref(),
        connector_id,
    ))
}

pub(super) fn tool_item_event(input: ToolItemEventInput<'_>) -> Option<TrackEventRequest> {
    let ToolItemEventInput {
        thread_id,
        turn_id,
        item,
        model_context,
        started_at_ms,
        completed_at_ms,
        connection_state,
        thread_state,
        thread_metadata,
        review_summary,
        elicitation_type,
    } = input;
    match item {
        ThreadItemPayload::CommandExecution {
            id,
            sandbox_type,
            plugin_id,
            script_path,
            source,
            status,
            command_actions,
            exit_code,
            duration_ms,
            ..
        } => {
            let (terminal_status, failure_kind) = command_execution_outcome(status)?;
            let action_counts = command_action_counts(command_actions);
            let base = tool_item_base(
                thread_id,
                turn_id,
                id.clone(),
                command_execution_tool_name(*source).to_string(),
                ToolItemOutcome {
                    terminal_status,
                    failure_kind,
                    execution_duration_ms: option_i64_to_u64(*duration_ms),
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::CommandExecution(
                CodexCommandExecutionEventRequest {
                    event_type: "codex_command_execution_event",
                    event_params: CodexCommandExecutionEventParams {
                        sandbox_backend: sandbox_type
                            .map(|sandbox| sandbox.as_metric_tag().to_string()),
                        model_slug: model_context.map(|context| context.model_slug.clone()),
                        reasoning_effort: model_context
                            .and_then(|context| context.reasoning_effort.clone()),
                        base,
                        plugin_id: plugin_id.clone(),
                        script_path: safe_plugin_relative_script_path(
                            plugin_id.as_deref(),
                            script_path.as_deref(),
                        ),
                        command_execution_source: *source,
                        exit_code: *exit_code,
                        command_total_action_count: action_counts.total,
                        command_read_action_count: action_counts.read,
                        command_list_files_action_count: action_counts.list_files,
                        command_search_action_count: action_counts.search,
                        command_unknown_action_count: action_counts.unknown,
                    },
                },
            ))
        }
        ThreadItemPayload::FileChange {
            id,
            changes,
            status,
        } => {
            let (terminal_status, failure_kind) = patch_apply_outcome(status)?;
            let counts = file_change_counts(changes);
            let base = tool_item_base(
                thread_id,
                turn_id,
                id.clone(),
                "apply_patch".to_string(),
                ToolItemOutcome {
                    terminal_status,
                    failure_kind,
                    execution_duration_ms: None,
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::FileChange(CodexFileChangeEventRequest {
                event_type: "codex_file_change_event",
                event_params: CodexFileChangeEventParams {
                    base,
                    file_change_count: usize_to_u64(changes.len()),
                    file_add_count: counts.add,
                    file_update_count: counts.update,
                    file_delete_count: counts.delete,
                    file_move_count: counts.move_,
                },
            }))
        }
        ThreadItemPayload::McpToolCall {
            id,
            server,
            tool,
            status,
            error,
            duration_ms,
            plugin_id,
            app_context,
            ..
        } => {
            let (terminal_status, failure_kind) = mcp_tool_call_outcome(status)?;
            let base = tool_item_base(
                thread_id,
                turn_id,
                id.clone(),
                tool.clone(),
                ToolItemOutcome {
                    terminal_status,
                    failure_kind,
                    execution_duration_ms: option_i64_to_u64(*duration_ms),
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::McpToolCall(
                CodexMcpToolCallEventRequest {
                    event_type: "codex_mcp_tool_call_event",
                    event_params: CodexMcpToolCallEventParams {
                        base,
                        mcp_server_name: server.clone(),
                        mcp_tool_name: tool.clone(),
                        mcp_error_present: error.is_some(),
                        plugin_id: plugin_id.clone(),
                        connector_id: app_context
                            .as_ref()
                            .map(|app_context| app_context.connector_id.clone()),
                        voice_session_id: None,
                        elicitation_type,
                    },
                },
            ))
        }
        ThreadItemPayload::DynamicToolCall {
            id,
            tool,
            status,
            content_items,
            success,
            duration_ms,
            ..
        } => {
            let (terminal_status, failure_kind) = dynamic_tool_call_outcome(status)?;
            let counts = content_items
                .as_ref()
                .map(|items| dynamic_content_counts(items));
            let base = tool_item_base(
                thread_id,
                turn_id,
                id.clone(),
                tool.clone(),
                ToolItemOutcome {
                    terminal_status,
                    failure_kind,
                    execution_duration_ms: option_i64_to_u64(*duration_ms),
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::DynamicToolCall(
                CodexDynamicToolCallEventRequest {
                    event_type: "codex_dynamic_tool_call_event",
                    event_params: CodexDynamicToolCallEventParams {
                        base,
                        dynamic_tool_name: tool.clone(),
                        success: *success,
                        output_content_item_count: counts.map(|counts| counts.total),
                        output_text_item_count: counts.map(|counts| counts.text),
                        output_image_item_count: counts.map(|counts| counts.image),
                        output_audio_item_count: counts.map(|counts| counts.audio),
                    },
                },
            ))
        }
        ThreadItemPayload::CollabAgentToolCall {
            id,
            tool,
            status,
            sender_thread_id,
            receiver_thread_ids,
            model,
            reasoning_effort,
            agents_states,
            ..
        } => {
            let (terminal_status, failure_kind) = collab_tool_call_outcome(status)?;
            let base = tool_item_base(
                thread_id,
                turn_id,
                id.clone(),
                collab_agent_tool_name(tool).to_string(),
                ToolItemOutcome {
                    terminal_status,
                    failure_kind,
                    execution_duration_ms: observed_duration_ms(started_at_ms, completed_at_ms),
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::CollabAgentToolCall(
                CodexCollabAgentToolCallEventRequest {
                    event_type: "codex_collab_agent_tool_call_event",
                    event_params: CodexCollabAgentToolCallEventParams {
                        base,
                        sender_thread_id: sender_thread_id.clone(),
                        receiver_thread_count: usize_to_u64(receiver_thread_ids.len()),
                        receiver_thread_ids: Some(receiver_thread_ids.clone()),
                        requested_model: model.clone(),
                        requested_reasoning_effort: reasoning_effort
                            .as_ref()
                            .and_then(serialize_enum_as_string),
                        agent_state_count: Some(usize_to_u64(agents_states.len())),
                        completed_agent_count: Some(usize_to_u64(
                            agents_states
                                .values()
                                .filter(|state| state.status == CollabAgentStatus::Completed)
                                .count(),
                        )),
                        failed_agent_count: Some(usize_to_u64(
                            agents_states
                                .values()
                                .filter(|state| {
                                    matches!(
                                        state.status,
                                        CollabAgentStatus::Errored
                                            | CollabAgentStatus::Shutdown
                                            | CollabAgentStatus::NotFound
                                    )
                                })
                                .count(),
                        )),
                    },
                },
            ))
        }
        ThreadItemPayload::WebSearch(item) => {
            let base = tool_item_base(
                thread_id,
                turn_id,
                item.id.clone(),
                "web_search".to_string(),
                ToolItemOutcome {
                    terminal_status: ToolItemTerminalStatus::Completed,
                    failure_kind: None,
                    execution_duration_ms: None,
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::WebSearch(CodexWebSearchEventRequest {
                event_type: "codex_web_search_event",
                event_params: CodexWebSearchEventParams {
                    base,
                    web_search_action: item.action.as_ref().map(web_search_action_kind),
                    query_present: !item.query.trim().is_empty(),
                    query_count: web_search_query_count(&item.query, item.action.as_ref()),
                },
            }))
        }
        ThreadItemPayload::ImageGeneration(item) => {
            let (terminal_status, failure_kind) = image_generation_outcome(item.status.as_str());
            let base = tool_item_base(
                thread_id,
                turn_id,
                item.id.clone(),
                "image_generation".to_string(),
                ToolItemOutcome {
                    terminal_status,
                    failure_kind,
                    execution_duration_ms: None,
                },
                ToolItemContext {
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary,
                },
            );
            Some(TrackEventRequest::ImageGeneration(
                CodexImageGenerationEventRequest {
                    event_type: "codex_image_generation_event",
                    event_params: CodexImageGenerationEventParams {
                        base,
                        revised_prompt_present: item.revised_prompt.is_some(),
                        saved_path_present: item.saved_path.is_some(),
                        transparent_background: item.transparent_background,
                        imagegen_request_id: item.imagegen_request_id.clone(),
                        generation_id: item.generation_id.clone(),
                    },
                },
            ))
        }
        _ => None,
    }
}

pub(super) fn safe_plugin_relative_script_path(
    plugin_id: Option<&str>,
    script_path: Option<&str>,
) -> Option<String> {
    let script_path = script_path.filter(|path| is_safe_plugin_relative_path(path))?;
    plugin_id.map(|_| script_path.to_string())
}

fn command_action_counts(command_actions: &[CommandAction]) -> CommandActionCounts {
    let mut counts = CommandActionCounts {
        total: usize_to_u64(command_actions.len()),
        ..Default::default()
    };
    for action in command_actions {
        match action {
            CommandAction::Read { .. } => counts.read += 1,
            CommandAction::ListFiles { .. } => counts.list_files += 1,
            CommandAction::Search { .. } => counts.search += 1,
            CommandAction::Unknown { .. } => counts.unknown += 1,
        }
    }
    counts
}

pub(super) fn tool_item_base(
    thread_id: &str,
    turn_id: &str,
    item_id: String,
    tool_name: String,
    outcome: ToolItemOutcome,
    context: ToolItemContext<'_>,
) -> CodexToolItemEventBase {
    let thread_metadata = context.thread_metadata;
    let review_summary = context.review_summary.cloned().unwrap_or_default();
    CodexToolItemEventBase {
        thread_id: thread_id.to_string(),
        session_id: thread_metadata.session_id.clone(),
        turn_id: turn_id.to_string(),
        turn_trigger: None,
        root_turn_id: None,
        item_id,
        cell_id: None,
        parent_call_id: None,
        originating_response_id: None,
        subsequent_response_id: None,
        app_server_client: context
            .thread_state
            .app_server_client(context.connection_state),
        runtime: context.connection_state.runtime.clone(),
        thread_source: thread_metadata.thread_source.clone(),
        subagent_source: thread_metadata.subagent_source.clone(),
        parent_thread_id: thread_metadata.parent_thread_id.clone(),
        tool_name,
        tool_event_type: None,
        started_at_ms: context.started_at_ms,
        completed_at_ms: context.completed_at_ms,
        // duration_ms reflects item lifecycle observed by app-server. For web
        // search and image generation in particular, that can be narrower than
        // full upstream execution time.
        duration_ms: observed_duration_ms(context.started_at_ms, context.completed_at_ms),
        execution_duration_ms: outcome.execution_duration_ms,
        review_count: review_summary.review_count,
        guardian_review_count: review_summary.guardian_review_count,
        user_review_count: review_summary.user_review_count,
        final_approval_outcome: review_summary
            .final_approval_outcome
            .unwrap_or(FinalApprovalOutcome::Unknown),
        terminal_status: outcome.terminal_status,
        failure_kind: outcome.failure_kind,
        requested_additional_permissions: review_summary.requested_additional_permissions,
        requested_network_access: review_summary.requested_network_access,
    }
}

pub(super) fn observed_duration_ms(started_at_ms: u64, completed_at_ms: u64) -> Option<u64> {
    completed_at_ms.checked_sub(started_at_ms)
}

fn command_execution_tool_name(source: CommandExecutionSource) -> &'static str {
    match source {
        CommandExecutionSource::UnifiedExecStartup
        | CommandExecutionSource::UnifiedExecInteraction => "unified_exec",
        CommandExecutionSource::UserShell => "user_shell",
        CommandExecutionSource::Agent => "shell",
    }
}

fn command_execution_outcome(
    status: &CommandExecutionStatus,
) -> Option<(ToolItemTerminalStatus, Option<ToolItemFailureKind>)> {
    match status {
        CommandExecutionStatus::InProgress => None,
        CommandExecutionStatus::Completed => Some((ToolItemTerminalStatus::Completed, None)),
        CommandExecutionStatus::Failed => Some((
            ToolItemTerminalStatus::Failed,
            Some(ToolItemFailureKind::ToolError),
        )),
        CommandExecutionStatus::Declined => Some((
            ToolItemTerminalStatus::Rejected,
            Some(ToolItemFailureKind::ApprovalDenied),
        )),
    }
}

fn patch_apply_outcome(
    status: &PatchApplyStatus,
) -> Option<(ToolItemTerminalStatus, Option<ToolItemFailureKind>)> {
    match status {
        PatchApplyStatus::InProgress => None,
        PatchApplyStatus::Completed => Some((ToolItemTerminalStatus::Completed, None)),
        PatchApplyStatus::Failed => Some((
            ToolItemTerminalStatus::Failed,
            Some(ToolItemFailureKind::ToolError),
        )),
        PatchApplyStatus::Declined => Some((
            ToolItemTerminalStatus::Rejected,
            Some(ToolItemFailureKind::ApprovalDenied),
        )),
    }
}

fn mcp_tool_call_outcome(
    status: &McpToolCallStatus,
) -> Option<(ToolItemTerminalStatus, Option<ToolItemFailureKind>)> {
    match status {
        McpToolCallStatus::InProgress => None,
        McpToolCallStatus::Completed => Some((ToolItemTerminalStatus::Completed, None)),
        McpToolCallStatus::Failed => Some((
            ToolItemTerminalStatus::Failed,
            Some(ToolItemFailureKind::ToolError),
        )),
    }
}

fn dynamic_tool_call_outcome(
    status: &DynamicToolCallStatus,
) -> Option<(ToolItemTerminalStatus, Option<ToolItemFailureKind>)> {
    match status {
        DynamicToolCallStatus::InProgress => None,
        DynamicToolCallStatus::Completed => Some((ToolItemTerminalStatus::Completed, None)),
        DynamicToolCallStatus::Failed => Some((
            ToolItemTerminalStatus::Failed,
            Some(ToolItemFailureKind::ToolError),
        )),
    }
}

fn collab_tool_call_outcome(
    status: &CollabAgentToolCallStatus,
) -> Option<(ToolItemTerminalStatus, Option<ToolItemFailureKind>)> {
    match status {
        CollabAgentToolCallStatus::InProgress => None,
        CollabAgentToolCallStatus::Completed => Some((ToolItemTerminalStatus::Completed, None)),
        CollabAgentToolCallStatus::Interrupted => Some((ToolItemTerminalStatus::Interrupted, None)),
        CollabAgentToolCallStatus::Failed => Some((
            ToolItemTerminalStatus::Failed,
            Some(ToolItemFailureKind::ToolError),
        )),
    }
}

fn image_generation_outcome(status: &str) -> (ToolItemTerminalStatus, Option<ToolItemFailureKind>) {
    match status {
        "failed" | "error" => (
            ToolItemTerminalStatus::Failed,
            Some(ToolItemFailureKind::ToolError),
        ),
        _ => (ToolItemTerminalStatus::Completed, None),
    }
}

fn collab_agent_tool_name(tool: &CollabAgentTool) -> &'static str {
    match tool {
        CollabAgentTool::SpawnAgent => "spawn_agent",
        CollabAgentTool::SendInput => "send_input",
        CollabAgentTool::ResumeAgent => "resume_agent",
        CollabAgentTool::Wait => "wait_agent",
        CollabAgentTool::CloseAgent => "close_agent",
        CollabAgentTool::SendMessage => "send_message",
        CollabAgentTool::FollowupTask => "followup_task",
        CollabAgentTool::InterruptAgent => "interrupt_agent",
        CollabAgentTool::ListAgents => "list_agents",
    }
}

fn file_change_counts(changes: &[codex_app_server_protocol::FileUpdateChange]) -> FileChangeCounts {
    let mut counts = FileChangeCounts::default();
    for change in changes {
        match &change.kind {
            PatchChangeKind::Add => counts.add += 1,
            PatchChangeKind::Delete => counts.delete += 1,
            PatchChangeKind::Update { move_path: Some(_) } => counts.move_ += 1,
            PatchChangeKind::Update { move_path: None } => counts.update += 1,
        }
    }
    counts
}

fn web_search_action_kind(action: &WebSearchAction) -> WebSearchActionKind {
    match action {
        WebSearchAction::Search { .. } => WebSearchActionKind::Search,
        WebSearchAction::OpenPage { .. } => WebSearchActionKind::OpenPage,
        WebSearchAction::FindInPage { .. } => WebSearchActionKind::FindInPage,
        WebSearchAction::Other => WebSearchActionKind::Other,
    }
}

fn web_search_query_count(query: &str, action: Option<&WebSearchAction>) -> Option<u64> {
    match action {
        Some(WebSearchAction::Search { query, queries }) => queries
            .as_ref()
            .map(|queries| usize_to_u64(queries.len()))
            .or_else(|| query.as_ref().map(|_| 1)),
        Some(WebSearchAction::OpenPage { .. })
        | Some(WebSearchAction::FindInPage { .. })
        | Some(WebSearchAction::Other) => None,
        None => (!query.trim().is_empty()).then_some(1),
    }
}
