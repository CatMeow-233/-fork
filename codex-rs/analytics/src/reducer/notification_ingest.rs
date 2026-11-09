//! Server notification handling for thread, turn and item lifecycle analytics.

use super::*;

impl AnalyticsReducer {
    pub(super) async fn ingest_notification(
        &mut self,
        notification: ServerNotification,
        out: &mut Vec<TrackEventRequest>,
    ) {
        match notification {
            ServerNotification::ThreadArchived(notification) => {
                out.push(TrackEventRequest::ThreadArchive(ThreadArchiveEvent {
                    event_type: "codex_thread_archive_event",
                    event_params: self.thread_archive_event_params(
                        notification.thread_id,
                        ThreadArchiveAction::Archived,
                    ),
                }));
            }
            ServerNotification::ThreadUnarchived(notification) => {
                out.push(TrackEventRequest::ThreadArchive(ThreadArchiveEvent {
                    event_type: "codex_thread_archive_event",
                    event_params: self.thread_archive_event_params(
                        notification.thread_id,
                        ThreadArchiveAction::Unarchived,
                    ),
                }));
            }
            ServerNotification::ItemStarted(notification) => {
                let Some(item_id) = tracked_tool_item_id(&notification.item.payload) else {
                    return;
                };
                let Some(started_at_ms) = option_i64_to_u64(Some(notification.started_at_ms))
                else {
                    return;
                };
                let model_context = match &notification.item.payload {
                    ThreadItemPayload::CommandExecution { model_context, .. } => {
                        model_context.clone()
                    }
                    _ => None,
                };
                self.turn_state(notification.turn_id.clone())
                    .thread_id
                    .get_or_insert(notification.thread_id.clone());
                let key = ToolItemKey {
                    thread_id: notification.thread_id.clone(),
                    turn_id: notification.turn_id.clone(),
                    item_id: item_id.to_string(),
                };
                let cua_identity = cua_mcp_tool_identity(&notification.item.payload);
                if cua_identity.is_some()
                    && self
                        .turns
                        .get(&notification.turn_id)
                        .is_some_and(|turn| turn.completed_cua_tool_item_ids.contains(item_id))
                {
                    return;
                }
                let first_notification = !self.tool_items_started_at_ms.contains_key(&key);
                self.tool_items_started_at_ms
                    .entry(key)
                    .and_modify(|(timestamp, _)| *timestamp = started_at_ms)
                    .or_insert((started_at_ms, model_context));
                if !first_notification {
                    return;
                }
                let Some((server, tool, plugin_id, connector_id)) = cua_identity else {
                    return;
                };
                let Some((connection_state, thread_state, thread_metadata)) = self
                    .thread_context_or_warn(AnalyticsDropSite::cua_tool_call_started(
                        &notification.thread_id,
                        &notification.turn_id,
                        item_id,
                    ))
                else {
                    return;
                };
                let attribution = tool_response_attribution(
                    item_id,
                    /*cell_id*/ None,
                    self.tool_response_states
                        .get(&(notification.thread_id.clone(), notification.turn_id.clone())),
                    self.code_mode_cells.get(&notification.thread_id),
                );
                let turn_metadata = self
                    .turns
                    .get(&notification.turn_id)
                    .and_then(|turn| turn.resolved_config.as_ref())
                    .map(|config| config.turn_metadata.as_ref());
                out.push(TrackEventRequest::CuaToolCallStarted(
                    CodexCuaToolCallStartedEventRequest {
                        event_type: "codex_cua_tool_call_started",
                        event_params: CodexCuaToolCallStartedEventParams {
                            thread_id: notification.thread_id,
                            session_id: thread_metadata.session_id.clone(),
                            turn_id: notification.turn_id,
                            root_turn_id: turn_metadata
                                .and_then(TurnAnalyticsMetadata::root_turn_id),
                            turn_trigger: turn_metadata
                                .and_then(TurnAnalyticsMetadata::turn_trigger),
                            item_id: item_id.to_string(),
                            cell_id: attribution.cell_id,
                            parent_call_id: attribution.parent_call_id,
                            originating_response_id: attribution.originating_response_id,
                            app_server_client: thread_state.app_server_client(connection_state),
                            runtime: connection_state.runtime.clone(),
                            thread_source: thread_metadata.thread_source.clone(),
                            subagent_source: thread_metadata.subagent_source.clone(),
                            parent_thread_id: thread_metadata.parent_thread_id.clone(),
                            tool_event_type: attribution.tool_event_type,
                            started_at_ms,
                            mcp_server_name: server.to_string(),
                            mcp_tool_name: tool.to_string(),
                            plugin_id: plugin_id.map(str::to_string),
                            connector_id: connector_id.map(str::to_string),
                        },
                    },
                ));
            }
            ServerNotification::ItemCompleted(notification) => {
                if matches!(
                    notification.item.payload,
                    ThreadItemPayload::SubAgentActivity { .. }
                ) {
                    let Some(turn_state) = self.turns.get_mut(&notification.turn_id) else {
                        tracing::warn!(
                            thread_id = %notification.thread_id,
                            turn_id = %notification.turn_id,
                            "dropping sub-agent activity tool count update: missing turn state"
                        );
                        return;
                    };
                    turn_state.tool_counts.record(&notification.item.payload);
                    return;
                }
                let Some(item_id) = tracked_tool_item_id(&notification.item.payload) else {
                    return;
                };
                let key = ToolItemKey {
                    thread_id: notification.thread_id.clone(),
                    turn_id: notification.turn_id.clone(),
                    item_id: item_id.to_string(),
                };
                let is_cua = cua_mcp_tool_identity(&notification.item.payload).is_some();
                if is_cua
                    && self
                        .turns
                        .get(&notification.turn_id)
                        .is_some_and(|turn| turn.completed_cua_tool_item_ids.contains(item_id))
                {
                    return;
                }
                let elicitation_type = if matches!(
                    notification.item.payload,
                    ThreadItemPayload::McpToolCall { .. }
                ) {
                    // Producers enqueue known classifications before completion on
                    // the same analytics queue, so completed keys need no history.
                    self.pending_mcp_tool_elicitations
                        .iter()
                        .position(|(pending, _)| pending == &key)
                        .and_then(|index| self.pending_mcp_tool_elicitations.remove(index))
                        .map(|(_, elicitation_type)| elicitation_type)
                } else {
                    None
                };
                let Some(turn_state) = self.turns.get_mut(&notification.turn_id) else {
                    tracing::warn!(
                        thread_id = %notification.thread_id,
                        turn_id = %notification.turn_id,
                        item_id,
                        "dropping turn tool count update: missing turn state"
                    );
                    return;
                };
                turn_state.tool_counts.record(&notification.item.payload);
                let Some((started_at_ms, model_context)) =
                    self.tool_items_started_at_ms.remove(&key)
                else {
                    tracing::warn!(
                        thread_id = %notification.thread_id,
                        turn_id = %notification.turn_id,
                        item_id,
                        "dropping tool item analytics event: missing item started notification"
                    );
                    return;
                };
                let Some(completed_at_ms) = option_i64_to_u64(Some(notification.completed_at_ms))
                else {
                    return;
                };
                let Some((connection_state, thread_state, thread_metadata)) = self
                    .thread_context_or_warn(AnalyticsDropSite::tool_item(&notification, item_id))
                else {
                    return;
                };
                if let Some(mut event) = tool_item_event(ToolItemEventInput {
                    thread_id: &notification.thread_id,
                    turn_id: &notification.turn_id,
                    item: &notification.item.payload,
                    model_context: model_context.as_ref(),
                    started_at_ms,
                    completed_at_ms,
                    connection_state,
                    thread_state,
                    thread_metadata,
                    review_summary: self.item_review_summaries.get(&key),
                    elicitation_type,
                }) {
                    if let TrackEventRequest::McpToolCall(mcp) = &mut event {
                        mcp.event_params.voice_session_id = self
                            .turns
                            .get(&notification.turn_id)
                            .and_then(|turn| turn.voice_session_id.clone());
                    }
                    let turn_metadata = self
                        .turns
                        .get(&notification.turn_id)
                        .and_then(|turn| turn.resolved_config.as_ref())
                        .map(|config| Arc::clone(&config.turn_metadata));
                    // Fast collaborator tools can complete before their sampling response.
                    // Keep the event until that response can supply its originating ID.
                    let emission = if matches!(
                        &notification.item.payload,
                        ThreadItemPayload::CollabAgentToolCall { .. }
                    ) {
                        ToolEventEmission::AwaitResponse
                    } else {
                        ToolEventEmission::ImmediateUnlessCorrelated
                    };
                    self.record_tool_event(
                        &notification.thread_id,
                        &notification.turn_id,
                        turn_metadata.as_deref(),
                        event,
                        emission,
                        out,
                    );
                    if is_cua && let Some(turn) = self.turns.get_mut(&notification.turn_id) {
                        turn.completed_cua_tool_item_ids.insert(item_id.to_string());
                    }
                }
                self.item_review_summaries.remove(&key);
                if self
                    .turns
                    .get(&notification.turn_id)
                    .is_some_and(|turn_state| turn_state.turn_event_emitted)
                    && !self.has_pending_tool_items_for_turn(&notification.turn_id)
                {
                    self.turns.remove(&notification.turn_id);
                }
            }
            ServerNotification::ItemGuardianApprovalReviewStarted(notification) => {
                let _ = notification;
            }
            ServerNotification::ItemGuardianApprovalReviewCompleted(notification) => {
                self.ingest_guardian_review_completed(notification, out);
            }
            ServerNotification::ThreadClosed(notification) => {
                self.tool_response_states.retain(|(thread_id, _), state| {
                    if thread_id == &notification.thread_id {
                        out.extend(state.pending_tool_events.drain(..));
                        false
                    } else {
                        true
                    }
                });
                let closed_turns: Vec<_> = self
                    .turns
                    .iter()
                    .filter(|(_, turn)| turn.thread_id.as_deref() == Some(&notification.thread_id))
                    .map(|(turn_id, _)| turn_id.clone())
                    .collect();
                for turn_id in closed_turns {
                    self.discard_turn(&turn_id);
                }
                self.pending_reviews
                    .retain(|_, review| review.thread_id != notification.thread_id);
                self.code_mode_cells.remove(&notification.thread_id);
                self.pending_mcp_tool_elicitations
                    .retain(|(key, _)| key.thread_id != notification.thread_id);
                if let Some(thread) = self.threads.get_mut(&notification.thread_id) {
                    thread.active_voice_session = None;
                    thread.recent_voice_sessions.clear();
                    thread.active_turn_id = None;
                    thread.pending_voice_handoffs.clear();
                }
            }
            ServerNotification::ThreadRealtimeStarted(notification) => {
                self.start_voice_session(
                    notification.thread_id,
                    notification.realtime_session_id,
                    now_unix_seconds(),
                );
            }
            ServerNotification::ThreadRealtimeClosed(notification) => {
                self.close_voice_session(&notification.thread_id, now_unix_seconds());
            }
            ServerNotification::TurnStarted(notification) => {
                let voice_session_id = {
                    let thread = self
                        .threads
                        .entry(notification.thread_id.clone())
                        .or_default();
                    thread.active_turn_id = Some(notification.turn.id.clone());
                    thread
                        .pending_voice_handoffs
                        .pop_front()
                        .unwrap_or_else(|| thread.active_voice_session_id())
                };
                let turn_state = self.turn_state(notification.turn.id);
                turn_state.thread_id = Some(notification.thread_id);
                turn_state.voice_session_id = voice_session_id;
                turn_state.started_at = notification
                    .turn
                    .started_at
                    .and_then(|started_at| u64::try_from(started_at).ok());
            }
            ServerNotification::TurnDiffUpdated(notification) => {
                let turn_state = self.turn_state(notification.turn_id.clone());
                turn_state.thread_id = Some(notification.thread_id);
                turn_state.latest_diff = Some(notification.diff);
            }
            ServerNotification::TurnCompleted(notification) => {
                if let Some(thread) = self.threads.get_mut(&notification.thread_id)
                    && thread.active_turn_id.as_deref() == Some(notification.turn.id.as_str())
                {
                    thread.active_turn_id = None;
                }
                self.flush_pending_tool_events(&notification.thread_id, &notification.turn.id, out);
                let turn_state = self.turn_state(notification.turn.id.clone());
                turn_state.thread_id = Some(notification.thread_id);
                turn_state.completed = Some(CompletedTurnState {
                    status: analytics_turn_status(notification.turn.status),
                    turn_error: notification
                        .turn
                        .error
                        .and_then(|error| error.codex_error_info),
                    completed_at: notification
                        .turn
                        .completed_at
                        .and_then(|completed_at| u64::try_from(completed_at).ok())
                        .unwrap_or_default(),
                    duration_ms: notification
                        .turn
                        .duration_ms
                        .and_then(|duration_ms| u64::try_from(duration_ms).ok()),
                });
                let turn_id = notification.turn.id;
                self.maybe_emit_turn_event(&turn_id, out).await;
            }
            _ => {}
        }
    }
}

fn analytics_turn_status(status: codex_app_server_protocol::TurnStatus) -> Option<TurnStatus> {
    match status {
        codex_app_server_protocol::TurnStatus::Completed => Some(TurnStatus::Completed),
        codex_app_server_protocol::TurnStatus::Failed => Some(TurnStatus::Failed),
        codex_app_server_protocol::TurnStatus::Interrupted => Some(TurnStatus::Interrupted),
        codex_app_server_protocol::TurnStatus::InProgress => None,
    }
}
