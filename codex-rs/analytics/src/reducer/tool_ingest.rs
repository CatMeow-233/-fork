//! Tool-call facts and response attribution, buffering and flushing of tool events.

use super::*;

impl AnalyticsReducer {
    pub(super) fn ingest_artifact_operation(
        &mut self,
        input: ArtifactOperationInput,
        out: &mut Vec<TrackEventRequest>,
    ) {
        out.push(TrackEventRequest::ArtifactOperation(
            codex_artifact_operation_event_request(input.tracking, input.operation),
        ));
    }

    pub(super) fn ingest_code_mode_tool_call(
        &mut self,
        input: CodeModeToolCallFact,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let thread_id = match &input {
            CodeModeToolCallFact::CellStarted { thread_id, .. }
            | CodeModeToolCallFact::ChildStarted { thread_id, .. }
            | CodeModeToolCallFact::CellClosed { thread_id, .. }
            | CodeModeToolCallFact::SamplingResponseCompleted { thread_id, .. }
            | CodeModeToolCallFact::Completed { thread_id, .. } => thread_id,
        };
        let has_thread_context = self.threads.get(thread_id).is_some_and(|thread| {
            thread.metadata.is_some()
                && self
                    .thread_connection_id(thread_id)
                    .is_some_and(|connection_id| self.connections.contains_key(&connection_id))
        });
        if !has_thread_context {
            return;
        }

        match input {
            CodeModeToolCallFact::CellStarted {
                thread_id,
                turn_id,
                call_id,
                cell_id,
            } => {
                let cells = self.code_mode_cells.entry(thread_id.clone()).or_default();
                if cells.contains_key(&cell_id) || cells.len() < MAX_TOOL_RESPONSE_ENTRIES {
                    let originating_response_id = self
                        .tool_response_states
                        .get(&(thread_id, turn_id))
                        .and_then(|state| state.response_ids_by_call_id.get(&call_id))
                        .cloned();
                    cells.insert(
                        cell_id,
                        CodeModeCellState {
                            parent_call_id: call_id,
                            originating_response_id,
                            closed_in_turn_id: None,
                        },
                    );
                }
            }
            CodeModeToolCallFact::ChildStarted {
                thread_id,
                turn_id,
                call_id,
                cell_id,
            } => {
                if self
                    .code_mode_cells
                    .get(&thread_id)
                    .is_some_and(|cells| cells.contains_key(&cell_id))
                {
                    let state = self
                        .tool_response_states
                        .entry((thread_id, turn_id))
                        .or_default();
                    if state.cell_ids_by_child_call_id.contains_key(&call_id)
                        || state.cell_ids_by_child_call_id.len() < MAX_TOOL_RESPONSE_ENTRIES
                    {
                        state.cell_ids_by_child_call_id.insert(call_id, cell_id);
                    }
                }
            }
            CodeModeToolCallFact::CellClosed {
                thread_id,
                turn_id,
                cell_id,
            } => {
                if let Some(cell) = self
                    .code_mode_cells
                    .get_mut(&thread_id)
                    .and_then(|cells| cells.get_mut(&cell_id))
                {
                    cell.closed_in_turn_id = Some(turn_id);
                }
            }
            CodeModeToolCallFact::SamplingResponseCompleted {
                thread_id,
                turn_id,
                response_id,
                tool_call_ids,
            } => {
                self.ingest_sampling_response_completed(
                    thread_id,
                    turn_id,
                    response_id,
                    tool_call_ids,
                    out,
                );
            }
            CodeModeToolCallFact::Completed {
                thread_id,
                turn_id,
                turn_metadata,
                call_id,
                cell_id,
                tool_name,
                started_at_ms,
                completed_at_ms,
                status,
            } => {
                let drop_site = AnalyticsDropSite {
                    event_name: "code mode tool",
                    thread_id: &thread_id,
                    turn_id: Some(&turn_id),
                    review_id: None,
                    item_id: Some(&call_id),
                };
                let Some((connection_state, thread_state, thread_metadata)) =
                    self.thread_context_or_warn(drop_site)
                else {
                    return;
                };
                let terminal_status = match status {
                    CodeModeToolCallStatus::Completed => ToolItemTerminalStatus::Completed,
                    CodeModeToolCallStatus::Failed => ToolItemTerminalStatus::Failed,
                    CodeModeToolCallStatus::Interrupted => ToolItemTerminalStatus::Interrupted,
                };
                let success = status == CodeModeToolCallStatus::Completed;
                let mut base = tool_item_base(
                    &thread_id,
                    &turn_id,
                    call_id,
                    tool_name.clone(),
                    ToolItemOutcome {
                        terminal_status,
                        failure_kind: (status == CodeModeToolCallStatus::Failed)
                            .then_some(ToolItemFailureKind::ToolError),
                        execution_duration_ms: observed_duration_ms(started_at_ms, completed_at_ms),
                    },
                    ToolItemContext {
                        started_at_ms,
                        completed_at_ms,
                        connection_state,
                        thread_state,
                        thread_metadata,
                        review_summary: None,
                    },
                );
                base.cell_id = cell_id;
                let event = TrackEventRequest::DynamicToolCall(CodexDynamicToolCallEventRequest {
                    event_type: "codex_dynamic_tool_call_event",
                    event_params: CodexDynamicToolCallEventParams {
                        base,
                        dynamic_tool_name: tool_name,
                        success: Some(success),
                        output_content_item_count: None,
                        output_text_item_count: None,
                        output_image_item_count: None,
                        output_audio_item_count: None,
                    },
                });
                let counts = &mut self.turn_state(turn_id.clone()).tool_counts;
                counts.total += 1;
                counts.dynamic_tool_call += 1;
                self.record_tool_event(
                    &thread_id,
                    &turn_id,
                    Some(turn_metadata.as_ref()),
                    event,
                    ToolEventEmission::AwaitResponse,
                    out,
                );
            }
        }
    }

    pub(super) fn ingest_control_tool_call(
        &mut self,
        input: ControlToolCallFact,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let ControlToolCallFact {
            thread_id,
            turn_id,
            turn_metadata,
            call_id,
            cell_id,
            tool_name,
            started_at_ms,
            completed_at_ms,
            status,
        } = input;
        let drop_site = AnalyticsDropSite {
            event_name: "control tool",
            thread_id: &thread_id,
            turn_id: Some(&turn_id),
            review_id: None,
            item_id: Some(&call_id),
        };
        let Some((connection_state, thread_state, thread_metadata)) =
            self.thread_context_or_warn(drop_site)
        else {
            return;
        };
        let mut base = tool_item_base(
            &thread_id,
            &turn_id,
            call_id,
            tool_name,
            ToolItemOutcome {
                terminal_status: match status {
                    ControlToolCallStatus::Completed => ToolItemTerminalStatus::Completed,
                    ControlToolCallStatus::Failed => ToolItemTerminalStatus::Failed,
                    ControlToolCallStatus::Rejected => ToolItemTerminalStatus::Rejected,
                    ControlToolCallStatus::Interrupted => ToolItemTerminalStatus::Interrupted,
                },
                failure_kind: match status {
                    ControlToolCallStatus::Failed => Some(ToolItemFailureKind::ToolError),
                    ControlToolCallStatus::Rejected => Some(ToolItemFailureKind::PolicyForbidden),
                    ControlToolCallStatus::Completed | ControlToolCallStatus::Interrupted => None,
                },
                execution_duration_ms: observed_duration_ms(started_at_ms, completed_at_ms),
            },
            ToolItemContext {
                started_at_ms,
                completed_at_ms,
                connection_state,
                thread_state,
                thread_metadata,
                review_summary: None,
            },
        );
        base.cell_id = cell_id;
        let event = TrackEventRequest::ControlToolCall(CodexControlToolCallEventRequest {
            event_type: "codex_control_tool_call_event",
            event_params: CodexControlToolCallEventParams {
                base,
                success: status == ControlToolCallStatus::Completed,
            },
        });
        self.turn_state(turn_id.clone()).tool_counts.total += 1;
        self.record_tool_event(
            &thread_id,
            &turn_id,
            Some(turn_metadata.as_ref()),
            event,
            ToolEventEmission::AwaitResponse,
            out,
        );
    }

    fn ingest_sampling_response_completed(
        &mut self,
        thread_id: String,
        turn_id: String,
        response_id: String,
        tool_call_ids: Vec<String>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let turn_key = (thread_id.clone(), turn_id);
        let state = self.tool_response_states.entry(turn_key).or_default();
        for call_id in tool_call_ids {
            if state.response_ids_by_call_id.contains_key(&call_id)
                || state.response_ids_by_call_id.len() < MAX_TOOL_RESPONSE_ENTRIES
            {
                state
                    .response_ids_by_call_id
                    .insert(call_id, response_id.clone());
            }
        }
        if let Some(cells) = self.code_mode_cells.get_mut(&thread_id) {
            for cell in cells.values_mut() {
                if cell.originating_response_id.is_none()
                    && let Some(originating_response_id) =
                        state.response_ids_by_call_id.get(&cell.parent_call_id)
                {
                    cell.originating_response_id = Some(originating_response_id.clone());
                }
            }
        }

        let mut remaining = VecDeque::new();
        while let Some(mut event) = state.pending_tool_events.pop_front() {
            enrich_tool_response_event(&mut event, state, self.code_mode_cells.get(&thread_id));
            let is_subsequent_response = tool_event_base_mut(&mut event)
                .and_then(|base| base.originating_response_id.as_deref())
                .is_some_and(|originating_response_id| originating_response_id != response_id);
            if is_subsequent_response {
                if let Some(base) = tool_event_base_mut(&mut event) {
                    base.subsequent_response_id = Some(response_id.clone());
                }
                out.push(event);
            } else {
                remaining.push_back(event);
            }
        }
        state.pending_tool_events = remaining;
    }

    pub(super) fn record_tool_event(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        turn_metadata: Option<&dyn TurnAnalyticsMetadata>,
        mut event: TrackEventRequest,
        emission: ToolEventEmission,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some(base) = tool_event_base_mut(&mut event) else {
            out.push(event);
            return;
        };
        base.root_turn_id = turn_metadata.and_then(TurnAnalyticsMetadata::root_turn_id);
        base.turn_trigger = turn_metadata.and_then(TurnAnalyticsMetadata::turn_trigger);
        let state = self
            .tool_response_states
            .entry((thread_id.to_string(), turn_id.to_string()))
            .or_default();
        enrich_tool_response_event(&mut event, state, self.code_mode_cells.get(thread_id));
        let is_correlated = tool_event_base_mut(&mut event)
            .is_some_and(|base| base.cell_id.is_some() || base.originating_response_id.is_some());
        if matches!(emission, ToolEventEmission::ImmediateUnlessCorrelated) && !is_correlated {
            out.push(event);
            return;
        }
        if state.pending_tool_events.len() == MAX_TOOL_RESPONSE_ENTRIES
            && let Some(oldest) = state.pending_tool_events.pop_front()
        {
            out.push(oldest);
        }
        state.pending_tool_events.push_back(event);
    }

    pub(super) fn flush_pending_tool_events(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let key = (thread_id.to_string(), turn_id.to_string());
        if let Some(state) = self.tool_response_states.remove(&key) {
            out.extend(state.pending_tool_events);
        }
        let cells = self.code_mode_cells.get_mut(thread_id);
        let remove_cells = cells.is_some_and(|cells| {
            cells.retain(|_, cell| cell.closed_in_turn_id.as_deref() != Some(turn_id));
            cells.is_empty()
        });
        if remove_cells {
            self.code_mode_cells.remove(thread_id);
        }
    }

    pub(crate) fn flush(&mut self, out: &mut Vec<TrackEventRequest>) {
        for state in self.tool_response_states.values_mut() {
            out.extend(state.pending_tool_events.drain(..));
        }
    }

    pub(crate) fn flush_thread(&mut self, thread_id: &str, out: &mut Vec<TrackEventRequest>) {
        for ((pending_thread_id, _), state) in &mut self.tool_response_states {
            if pending_thread_id == thread_id {
                out.extend(state.pending_tool_events.drain(..));
            }
        }
    }
}

fn tool_event_base_mut(event: &mut TrackEventRequest) -> Option<&mut CodexToolItemEventBase> {
    match event {
        TrackEventRequest::CommandExecution(event) => Some(&mut event.event_params.base),
        TrackEventRequest::FileChange(event) => Some(&mut event.event_params.base),
        TrackEventRequest::McpToolCall(event) => Some(&mut event.event_params.base),
        TrackEventRequest::DynamicToolCall(event) => Some(&mut event.event_params.base),
        TrackEventRequest::ControlToolCall(event) => Some(&mut event.event_params.base),
        TrackEventRequest::CollabAgentToolCall(event) => Some(&mut event.event_params.base),
        TrackEventRequest::WebSearch(event) => Some(&mut event.event_params.base),
        TrackEventRequest::ImageGeneration(event) => Some(&mut event.event_params.base),
        _ => None,
    }
}

fn enrich_tool_response_event(
    event: &mut TrackEventRequest,
    state: &ToolResponseState,
    cells: Option<&HashMap<String, CodeModeCellState>>,
) {
    let Some(base) = tool_event_base_mut(event) else {
        return;
    };
    let attribution =
        tool_response_attribution(&base.item_id, base.cell_id.clone(), Some(state), cells);
    base.tool_event_type = attribution.tool_event_type;
    base.cell_id = attribution.cell_id;
    base.parent_call_id = attribution.parent_call_id;
    base.originating_response_id = attribution.originating_response_id;
}

pub(super) fn tool_response_attribution(
    item_id: &str,
    cell_id: Option<String>,
    state: Option<&ToolResponseState>,
    cells: Option<&HashMap<String, CodeModeCellState>>,
) -> ToolResponseAttribution {
    // A cell association can also describe a separately sampled wait call. Classify
    // only from evidence about this exact call ID, not its parent or response lineage.
    let tool_event_type = match (
        state.is_some_and(|state| state.response_ids_by_call_id.contains_key(item_id)),
        state.is_some_and(|state| state.cell_ids_by_child_call_id.contains_key(item_id)),
    ) {
        (true, false) => Some(ToolEventType::ModelToolCall),
        (false, true) => Some(ToolEventType::InnerToolCall),
        (false, false) | (true, true) => None,
    };
    let cell_id = cell_id
        .or_else(|| state.and_then(|state| state.cell_ids_by_child_call_id.get(item_id).cloned()));
    let cell = cell_id.as_ref().and_then(|cell_id| cells?.get(cell_id));
    let parent_call_id =
        cell.and_then(|cell| (cell.parent_call_id != item_id).then(|| cell.parent_call_id.clone()));
    let originating_response_id = state
        .and_then(|state| state.response_ids_by_call_id.get(item_id).cloned())
        .or_else(|| cell.and_then(|cell| cell.originating_response_id.clone()));
    ToolResponseAttribution {
        tool_event_type,
        cell_id: cell.is_some().then_some(cell_id).flatten(),
        parent_call_id,
        originating_response_id,
    }
}
