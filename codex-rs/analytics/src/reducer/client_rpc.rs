//! Client JSON-RPC requests, responses and error responses, including turn steer events.

use super::*;

impl AnalyticsReducer {
    pub(super) fn ingest_request(
        &mut self,
        connection_id: u64,
        request_id: RequestId,
        request: ClientRequest,
    ) {
        match request {
            ClientRequest::TurnStart { params, .. } => {
                self.requests.insert(
                    (connection_id, request_id),
                    RequestState::TurnStart(PendingTurnStartState {
                        thread_id: params.thread_id,
                        num_input_images: num_input_images(&params.input),
                    }),
                );
            }
            ClientRequest::TurnSteer { params, .. } => {
                self.requests.insert(
                    (connection_id, request_id),
                    RequestState::TurnSteer(PendingTurnSteerState {
                        thread_id: params.thread_id,
                        expected_turn_id: params.expected_turn_id,
                        num_input_images: num_input_images(&params.input),
                        created_at: now_unix_seconds(),
                    }),
                );
            }
            _ => {}
        }
    }

    pub(super) async fn ingest_response(
        &mut self,
        connection_id: u64,
        response: ClientResponse,
        thread_originator: Option<String>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        match response {
            ClientResponse::ThreadStart { response, .. } => {
                self.emit_thread_initialized(
                    connection_id,
                    response.thread,
                    response.model,
                    ThreadInitializationMode::New,
                    thread_originator,
                    out,
                );
            }
            ClientResponse::ThreadResume { response, .. } => {
                self.emit_thread_initialized(
                    connection_id,
                    response.thread,
                    response.model,
                    ThreadInitializationMode::Resumed,
                    thread_originator,
                    out,
                );
            }
            ClientResponse::ThreadFork { response, .. } => {
                self.emit_thread_initialized(
                    connection_id,
                    response.thread,
                    response.model,
                    ThreadInitializationMode::Forked,
                    thread_originator,
                    out,
                );
            }
            ClientResponse::TurnStart {
                request_id,
                response,
            } => {
                let turn_id = response.turn.id;
                let Some(RequestState::TurnStart(pending_request)) =
                    self.requests.remove(&(connection_id, request_id))
                else {
                    return;
                };
                let turn_state = self.turn_state(turn_id.clone());
                turn_state.connection_id = Some(connection_id);
                turn_state.thread_id = Some(pending_request.thread_id);
                turn_state.num_input_images = Some(pending_request.num_input_images);
                self.maybe_emit_turn_event(&turn_id, out).await;
            }
            ClientResponse::TurnSteer {
                request_id,
                response,
            } => {
                self.ingest_turn_steer_response(connection_id, request_id, response, out);
            }
            ClientResponse::TurnInterrupt { request_id, .. } => {
                let Some(RequestState::ExplicitClientInterrupt(pending_request)) =
                    self.requests.remove(&(connection_id, request_id))
                else {
                    return;
                };
                let turn_id = pending_request.turn_id;
                let turn_state = self.turn_state(turn_id.clone());
                let earliest_requested_at_ms = turn_state
                    .explicit_client_interrupt_requested_at_ms
                    .get_or_insert(pending_request.requested_at_ms);
                *earliest_requested_at_ms =
                    (*earliest_requested_at_ms).min(pending_request.requested_at_ms);
                self.maybe_emit_turn_event(&turn_id, out).await;
            }
            _ => {}
        }
    }

    pub(super) fn ingest_error_response(
        &mut self,
        connection_id: u64,
        request_id: RequestId,
        error_type: Option<AnalyticsJsonRpcError>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some(request) = self.requests.remove(&(connection_id, request_id)) else {
            return;
        };
        self.ingest_request_error_response(connection_id, request, error_type, out);
    }

    fn ingest_request_error_response(
        &mut self,
        connection_id: u64,
        request: RequestState,
        error_type: Option<AnalyticsJsonRpcError>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        match request {
            RequestState::TurnStart(_) => {}
            RequestState::TurnSteer(pending_request) => {
                self.ingest_turn_steer_error_response(
                    connection_id,
                    pending_request,
                    error_type,
                    out,
                );
            }
            RequestState::ExplicitClientInterrupt(_) => {}
        }
    }

    fn ingest_turn_steer_error_response(
        &mut self,
        connection_id: u64,
        pending_request: PendingTurnSteerState,
        error_type: Option<AnalyticsJsonRpcError>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        self.emit_turn_steer_event(
            connection_id,
            pending_request,
            /*accepted_turn_id*/ None,
            TurnSteerResult::Rejected,
            rejection_reason_from_error_type(error_type),
            out,
        );
    }

    fn ingest_turn_steer_response(
        &mut self,
        connection_id: u64,
        request_id: RequestId,
        response: TurnSteerResponse,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some(RequestState::TurnSteer(pending_request)) =
            self.requests.remove(&(connection_id, request_id))
        else {
            return;
        };
        if let Some(turn_state) = self.turns.get_mut(&response.turn_id) {
            turn_state.steer_count += 1;
        }
        self.emit_turn_steer_event(
            connection_id,
            pending_request,
            Some(response.turn_id),
            TurnSteerResult::Accepted,
            /*rejection_reason*/ None,
            out,
        );
    }

    fn emit_turn_steer_event(
        &mut self,
        connection_id: u64,
        pending_request: PendingTurnSteerState,
        accepted_turn_id: Option<String>,
        result: TurnSteerResult,
        rejection_reason: Option<TurnSteerRejectionReason>,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let Some(connection_state) = self.connections.get(&connection_id) else {
            return;
        };
        let drop_site = AnalyticsDropSite::turn_steer(&pending_request.thread_id);
        let Some(thread_state) = self.threads.get(drop_site.thread_id) else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadMetadata);
            return;
        };
        let Some(thread_metadata) = thread_state.metadata.as_ref() else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadMetadata);
            return;
        };
        out.push(TrackEventRequest::TurnSteer(CodexTurnSteerEventRequest {
            event_type: "codex_turn_steer_event",
            event_params: CodexTurnSteerEventParams {
                thread_id: pending_request.thread_id,
                session_id: thread_metadata.session_id.clone(),
                expected_turn_id: Some(pending_request.expected_turn_id),
                accepted_turn_id,
                app_server_client: thread_state.app_server_client(connection_state),
                runtime: connection_state.runtime.clone(),
                thread_source: thread_metadata.thread_source.clone(),
                subagent_source: thread_metadata.subagent_source.clone(),
                parent_thread_id: thread_metadata.parent_thread_id.clone(),
                num_input_images: pending_request.num_input_images,
                result,
                rejection_reason,
                created_at: pending_request.created_at,
            },
        }));
    }
}

fn rejection_reason_from_error_type(
    error_type: Option<AnalyticsJsonRpcError>,
) -> Option<TurnSteerRejectionReason> {
    match error_type? {
        AnalyticsJsonRpcError::TurnSteer(error) => Some(error.into()),
        AnalyticsJsonRpcError::Input(error) => Some(error.into()),
    }
}
