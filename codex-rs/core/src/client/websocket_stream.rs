//! The turn `stream` entry point and the Responses WebSocket stream path, including prewarm
//! and incremental continuation reuse.

use super::*;

impl NonIncrementalReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::NoPreviousRequest => "no_previous_request",
            Self::NoPreviousResponse => "no_previous_response",
            Self::NoPreviousResponseId => "no_previous_response_id",
            Self::RequestPropertyChanged { reason } => reason,
            Self::InputShortened { .. } => "input_shortened",
            Self::InputMismatch { .. } => "input_mismatch",
        }
    }

    fn item_types(&self) -> (&'static str, &'static str) {
        match self {
            Self::InputShortened { previous } => (previous, "none"),
            Self::InputMismatch { previous, current } => (previous, current),
            Self::NoPreviousRequest
            | Self::NoPreviousResponse
            | Self::NoPreviousResponseId
            | Self::RequestPropertyChanged { .. } => ("none", "none"),
        }
    }
}

// This is intentionally not a `PartialEq` implementation: request equality includes `input` and
// `client_metadata`, while websocket reuse compares input and late tool-result metadata separately.
// Access programs are authorized per response, including continuations, without replaying input.
// Keep the destructuring exhaustive so new request fields require an explicit reuse decision.
fn responses_request_properties_mismatch(
    previous: &ResponsesApiRequest,
    current: &ResponsesApiRequest,
) -> Option<&'static str> {
    let ResponsesApiRequest {
        model: previous_model,
        input: _,
        tools: previous_tools,
        tool_choice: previous_tool_choice,
        parallel_tool_calls: previous_parallel_tool_calls,
        reasoning: previous_reasoning,
        store: previous_store,
        stream: previous_stream,
        stream_options: _,
        include: previous_include,
        service_tier: previous_service_tier,
        prompt_cache_key: previous_prompt_cache_key,
        prompt_cache_options: previous_prompt_cache_options,
        text: previous_text,
        client_metadata: _,
        access_programs: _,
    } = previous;
    let ResponsesApiRequest {
        model: current_model,
        input: _,
        tools: current_tools,
        tool_choice: current_tool_choice,
        parallel_tool_calls: current_parallel_tool_calls,
        reasoning: current_reasoning,
        store: current_store,
        stream: current_stream,
        stream_options: _,
        include: current_include,
        service_tier: current_service_tier,
        prompt_cache_key: current_prompt_cache_key,
        prompt_cache_options: current_prompt_cache_options,
        text: current_text,
        client_metadata: _,
        access_programs: _,
    } = current;

    // Stream options control delivery for this response, not the context
    // referenced by `previous_response_id`.
    let reason = if previous_model != current_model {
        "model_changed"
    } else if previous_tools != current_tools {
        "tools_changed"
    } else if previous_tool_choice != current_tool_choice {
        "tool_choice_changed"
    } else if previous_parallel_tool_calls != current_parallel_tool_calls {
        "parallel_tool_calls_changed"
    } else if previous_reasoning != current_reasoning {
        "reasoning_changed"
    } else if previous_store != current_store {
        "store_changed"
    } else if previous_stream != current_stream {
        "stream_changed"
    } else if previous_include != current_include {
        "include_changed"
    } else if previous_service_tier != current_service_tier {
        "service_tier_changed"
    } else if previous_prompt_cache_key != current_prompt_cache_key {
        "prompt_cache_key_changed"
    } else if previous_prompt_cache_options != current_prompt_cache_options {
        "prompt_cache_options_changed"
    } else if previous_text != current_text {
        "text_changed"
    } else {
        return None;
    };
    Some(reason)
}

fn response_items_equal_ignoring_internal_metadata(
    previous: &ResponseItem,
    current: &ResponseItem,
) -> bool {
    if previous == current {
        return true;
    }

    // Late results update an already-sent output. A delta cannot carry that update.
    if !previous.has_same_tool_result_metadata(current) {
        return false;
    }

    let mut previous = previous.clone();
    previous.clear_internal_chat_message_metadata_passthrough();
    let mut current = current.clone();
    current.clear_internal_chat_message_metadata_passthrough();
    previous == current
}

impl ModelClientSession {
    /// Checks whether the current request is an incremental extension of the previous request.
    /// We only reuse an incremental input delta when non-input request fields are unchanged and
    /// `input` extends or equals the previous known input. Server-returned output items
    /// are treated as part of the baseline so we do not resend them.
    fn get_incremental_items(
        &self,
        request: &ResponsesApiRequest,
        last_response: &LastResponse,
    ) -> std::result::Result<Vec<ResponseItem>, NonIncrementalReason> {
        let previous_request = self
            .websocket_session
            .last_request
            .as_ref()
            .ok_or(NonIncrementalReason::NoPreviousRequest)?;
        if let Some(reason) = responses_request_properties_mismatch(previous_request, request) {
            return Err(NonIncrementalReason::RequestPropertyChanged { reason });
        }

        let response_items = &last_response.items_added;
        let previous_items = previous_request.input.iter().chain(response_items);
        let mut current_items = request.input.iter();
        for previous in previous_items {
            let Some(current) = current_items.next() else {
                return Err(NonIncrementalReason::InputShortened {
                    previous: previous.item_type(),
                });
            };
            if !response_items_equal_ignoring_internal_metadata(previous, current) {
                return Err(NonIncrementalReason::InputMismatch {
                    previous: previous.item_type(),
                    current: current.item_type(),
                });
            }
        }
        Ok(current_items.cloned().collect())
    }

    fn get_last_response(&mut self) -> Option<LastResponse> {
        self.websocket_session
            .last_response_rx
            .take()
            .and_then(|mut receiver| match receiver.try_recv() {
                Ok(last_response) => Some(last_response),
                Err(TryRecvError::Closed) | Err(TryRecvError::Empty) => None,
            })
    }

    pub(super) fn prepare_websocket_request(
        &mut self,
        request: &ResponsesApiRequest,
    ) -> std::result::Result<WebsocketContinuation, NonIncrementalReason> {
        let last_response = self
            .get_last_response()
            .ok_or(NonIncrementalReason::NoPreviousResponse)?;
        let items = self.get_incremental_items(request, &last_response)?;

        if last_response.response_id.is_empty() {
            return Err(NonIncrementalReason::NoPreviousResponseId);
        }

        Ok(WebsocketContinuation {
            response_id: last_response.response_id,
            items,
            from_untraced_warmup: self.websocket_session.last_response_from_untraced_warmup,
        })
    }

    /// Streams a turn via the Responses API over WebSocket transport.
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_responses_websocket",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = %self.client.state.provider.info().wire_api,
            transport = "responses_websocket",
            api.path = tracing::field::Empty,
            turn.has_metadata_header = responses_metadata.has_turn_metadata(),
            websocket.warmup = warmup
        )
    )]
    async fn stream_responses_websocket(
        &mut self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &CodexResponsesMetadata,
        warmup: bool,
        request_trace: Option<W3cTraceContext>,
        inference_trace: &InferenceTraceContext,
    ) -> Result<WebsocketStreamOutcome> {
        let after_prewarm = !warmup && self.websocket_session.last_response_from_untraced_warmup;
        let provider = Arc::clone(&self.client.state.provider);
        let auth_manager = provider.auth_manager();
        let thread_id = self.client.state.thread_id.to_string();
        let kind = if warmup {
            codex_extension_api::ModelRequestKind::Warmup
        } else {
            codex_extension_api::ModelRequestKind::Generation
        };

        let mut auth_recovery = auth_manager
            .as_ref()
            .map(AuthManager::unauthorized_recovery);
        let mut provider_auth_recovery_attempted = false;
        let mut pending_retry = PendingUnauthorizedRetry::default();
        loop {
            crate::model_request::check_budget(&self.client.request_contributors, &thread_id, kind)
                .await?;
            let client_setup = self
                .client
                .current_client_setup(ClientRouting::Workspace)
                .await?;
            let include_internal = self
                .client
                .state
                .provider
                .include_internal_metadata(&client_setup.api_provider);
            let responses_headers = self
                .client
                .responses_headers(client_setup.auth.as_ref(), &model_info.slug);
            tracing::Span::current().record("api.path", "/responses");
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            )
            .map_err(|error| self.client.state.provider.map_api_error(error))?;
            let mut request = self.client.build_responses_request(
                prompt,
                model_info,
                effort.clone(),
                summary,
                service_tier.clone(),
                responses_metadata,
                include_internal,
            )?;
            if is_guardian_reviewer(&responses_headers) {
                request.service_tier = None;
            }
            request.access_programs = cyber_access_program::for_auth(
                client_setup.auth.as_ref(),
                prompt.cyber_access_program,
                self.client.api_key_cyber_access_programs,
            )?;
            self.client
                .validate_required_cyber_access_program(request.access_programs)?;
            let mut websocket_metadata = responses_metadata.clone();
            websocket_metadata.routing_hint = self.client.build_routing_hint_header(
                client_setup.auth.as_ref(),
                &responses_headers,
                &request.model,
                request.service_tier.as_deref(),
            );
            let request_session_telemetry = if warmup {
                // `generate=false` prewarm is connection setup, not an inference request.
                session_telemetry.clone()
            } else {
                session_telemetry_for_request(session_telemetry, &request)
            };
            match self
                .websocket_connection(WebsocketConnectParams {
                    session_telemetry,
                    api_provider: client_setup.api_provider,
                    auth_revision: client_setup.auth_revision,
                    api_auth: client_setup.api_auth,
                    http_client_factory: client_setup.http_client_factory,
                    auth_owner: client_setup.auth_owner,
                    runtime_id: client_setup.runtime_id,
                    responses_metadata: &websocket_metadata,
                    auth_context: request_auth_context,
                    request_route_telemetry: RequestRouteTelemetry::for_endpoint("/responses"),
                    responses_headers: &responses_headers,
                })
                .await
            {
                Ok(_) => {}
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UPGRADE_REQUIRED =>
                {
                    return Ok(WebsocketStreamOutcome::FallbackToHttp);
                }
                Err(ApiError::Transport(unauthorized_transport))
                    if provider.is_recoverable_auth_error(&unauthorized_transport) =>
                {
                    pending_retry = PendingUnauthorizedRetry::from_recovery(
                        handle_unauthorized(
                            unauthorized_transport,
                            &mut auth_recovery,
                            &mut provider_auth_recovery_attempted,
                            session_telemetry,
                            &provider,
                            self.client.event_sender.as_ref(),
                            responses_metadata.turn_id.as_deref(),
                        )
                        .await?,
                    );
                    continue;
                }
                Err(err) => return Err(provider.map_api_error(err)),
            }

            // The budget can be exhausted while the connection is being established.
            if !warmup && !self.client.request_contributors.is_empty() {
                crate::model_request::check_budget(
                    &self.client.request_contributors,
                    &thread_id,
                    kind,
                )
                .await?;
                // An account switch during the check requires fresh request setup and a new socket.
                if self.websocket_session.auth_owner != self.client.auth_owner() {
                    continue;
                }
            }

            let mut client_metadata = self.client.build_ws_client_metadata(
                responses_metadata,
                include_internal,
                model_info.use_responses_lite,
            );
            if let Some(turn_state) = self.turn_state.get() {
                client_metadata.insert(X_CODEX_TURN_STATE_HEADER.to_string(), turn_state.clone());
            }
            let (continuation, mode, reason, previous_item_type, current_item_type) =
                match self.prepare_websocket_request(&request) {
                    Ok(continuation) => (
                        Some(continuation),
                        "incremental",
                        "incremental",
                        "none",
                        "none",
                    ),
                    Err(failure) => {
                        let reason = self
                            .websocket_session
                            .continuation_reset_reason
                            .take()
                            .unwrap_or(if self.websocket_session.last_request.is_some() {
                                failure.as_str()
                            } else if self.client.restored_history {
                                "restored_history"
                            } else {
                                "no_previous_request"
                            });
                        let (previous_item_type, current_item_type) = failure.item_types();
                        trace!(
                            reason,
                            previous_item_type, current_item_type, "non-incremental request"
                        );
                        (None, "full", reason, previous_item_type, current_item_type)
                    }
                };
            let previous_response_id_from_untraced_warmup = continuation
                .as_ref()
                .is_some_and(|c| c.from_untraced_warmup);
            let inference_trace_attempt = if warmup {
                // Prewarm sends `generate=false`; it is connection setup, not a
                // model inference attempt that should appear in rollout traces.
                InferenceTraceAttempt::disabled()
            } else {
                inference_trace.start_attempt()
            };
            if previous_response_id_from_untraced_warmup {
                // The transport can reuse an untraced warmup response id and omit the
                // already-sent input, but rollout replay needs the logical model-visible
                // request rather than the compressed websocket delta.
                inference_trace_attempt.record_started(&request);
            }

            let (previous_response_id, mut incremental_items) = match continuation {
                Some(continuation) => (Some(continuation.response_id), Some(continuation.items)),
                None => (None, None),
            };
            let original_item_ids = if let Some(incremental_items) = &mut incremental_items {
                self.client
                    .prepare_response_items_for_request(incremental_items);
                None
            } else {
                let original_item_ids = request
                    .input
                    .iter()
                    .map(|item| item.id().cloned())
                    .collect::<Vec<_>>();
                self.client
                    .prepare_response_items_for_request(&mut request.input);
                Some(original_item_ids)
            };
            let mut ws_payload = ResponseCreateWsRequest {
                previous_response_id,
                input: incremental_items.as_deref().unwrap_or(&request.input),
                generate: if warmup { Some(false) } else { None },
                client_metadata: response_create_client_metadata(
                    Some(client_metadata),
                    request_trace.as_ref(),
                ),
                ..ResponseCreateWsRequest::from(&request)
            };
            self.client.set_guardian_metadata(
                &mut ws_payload.client_metadata,
                responses_metadata.parent_response_id.as_deref(),
                client_setup.auth.as_ref(),
                &responses_headers,
            );
            let interceptors = crate::model_request::prepare(
                &self.client.request_contributors,
                &thread_id,
                &model_info.slug,
                kind,
                &mut ws_payload.client_metadata,
            );
            let mut ws_request = ResponsesWsRequest::ResponseCreate(ws_payload);
            stamp_ws_stream_request_start_ms(&mut ws_request);
            let ResponsesWsRequest::ResponseCreate(payload) = &ws_request;
            let bounded_input = tool_metadata::bounded_input(&ws_request, payload.input);
            if let Some(input) = bounded_input.as_deref() {
                if let Some(recorder) = &self.client.executed_tool_calls {
                    recorder.invalidate_wire_inventory_loss(payload.input, input);
                }
                let ResponsesWsRequest::ResponseCreate(payload) = &mut ws_request;
                payload.input = input;
            }
            if !previous_response_id_from_untraced_warmup {
                inference_trace_attempt.record_started(&ws_request);
            }

            let websocket_connection =
                self.websocket_session.connection.as_ref().ok_or_else(|| {
                    self.client.state.provider.map_api_error(ApiError::Stream(
                        "websocket connection is unavailable".to_string(),
                    ))
                })?;
            request_session_telemetry.counter(
                WEBSOCKET_CONTINUATION_COUNT_METRIC,
                /*inc*/ 1,
                &[
                    ("mode", mode),
                    ("reason", reason),
                    ("previous_item_type", previous_item_type),
                    ("current_item_type", current_item_type),
                    ("phase", if warmup { "warmup" } else { "generation" }),
                    (
                        "after_prewarm",
                        if after_prewarm { "true" } else { "false" },
                    ),
                ],
            );
            let stream_result = websocket_connection
                .stream_request(
                    ws_request,
                    self.websocket_session.connection_reused(),
                    Some(Arc::clone(&self.turn_state)),
                )
                .await;
            if let Some(original_item_ids) = original_item_ids {
                for (item, original_item_id) in request.input.iter_mut().zip(original_item_ids) {
                    item.set_id(original_item_id);
                }
            }
            self.websocket_session.last_request = Some(request);
            self.websocket_session.last_response_from_untraced_warmup = warmup;
            let stream_result = stream_result.map_err(|err| {
                let response_debug_context = extract_response_debug_context_from_api_error(&err);
                let err = self.client.state.provider.map_api_error(err);
                inference_trace_attempt.record_failed(
                    &err,
                    response_debug_context.request_id.as_deref(),
                    /*output_items*/ &[],
                );
                err
            })?;
            let (last_response_tx, last_response_rx) = oneshot::channel();
            let stream = map_response_stream(
                stream_result,
                request_session_telemetry,
                inference_trace_attempt,
                &self.client.state,
                interceptors,
                Some(last_response_tx),
            );
            self.websocket_session.last_response_rx = Some(last_response_rx);
            return Ok(WebsocketStreamOutcome::Stream(stream));
        }
    }

    /// Whether a previous request prepared a connection that is not known to be closed.
    /// The next request still validates provider, auth, and headers before reuse.
    pub(crate) async fn is_websocket_prewarmed(&self) -> bool {
        if self.websocket_session.last_request.is_some()
            && let Some(connection) = self.websocket_session.connection.as_ref()
        {
            !connection.is_closed().await
        } else {
            false
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn prewarm_websocket(
        &mut self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &CodexResponsesMetadata,
    ) -> Result<()> {
        if !self.client.responses_websocket_enabled() {
            return Ok(());
        }
        if self.is_websocket_prewarmed().await {
            return Ok(());
        }

        let disabled_trace = InferenceTraceContext::disabled();
        match self
            .stream_responses_websocket(
                prompt,
                model_info,
                session_telemetry,
                effort,
                summary,
                service_tier,
                responses_metadata,
                /*warmup*/ true,
                current_span_w3c_trace_context(),
                &disabled_trace,
            )
            .await
        {
            Ok(WebsocketStreamOutcome::Stream(mut stream)) => {
                // Wait for the v2 warmup request to complete before sending the first turn request.
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(ResponseEvent::Completed { .. }) => break,
                        Err(err) => return Err(err),
                        _ => {}
                    }
                }
                Ok(())
            }
            Ok(WebsocketStreamOutcome::FallbackToHttp) => {
                self.try_switch_fallback_transport(session_telemetry, model_info);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// Streams a single model request within the current turn.
    ///
    /// The caller is responsible for passing per-turn settings explicitly (model selection,
    /// reasoning settings, telemetry context, and turn metadata). This method will prefer the
    /// Responses WebSocket transport when the provider supports it and it remains healthy, and will
    /// fall back to the HTTP Responses API transport otherwise. The trace context may be enabled or
    /// disabled, but is always explicit so transport paths do not need separate trace/no-trace
    /// branches.
    pub async fn stream(
        &mut self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &CodexResponsesMetadata,
        inference_trace: &InferenceTraceContext,
    ) -> Result<ResponseStream> {
        let wire_api = self.client.state.provider.info().wire_api;
        match wire_api {
            WireApi::Responses => {
                if self.client.responses_websocket_enabled() {
                    let request_trace = current_span_w3c_trace_context();
                    match self
                        .stream_responses_websocket(
                            prompt,
                            model_info,
                            session_telemetry,
                            effort.clone(),
                            summary,
                            service_tier.clone(),
                            responses_metadata,
                            /*warmup*/ false,
                            request_trace,
                            inference_trace,
                        )
                        .await?
                    {
                        WebsocketStreamOutcome::Stream(stream) => return Ok(stream),
                        WebsocketStreamOutcome::FallbackToHttp => {
                            self.try_switch_fallback_transport(session_telemetry, model_info);
                        }
                    }
                }

                ResponsesHttpExecutor::new(&self.client)
                    .stream_responses_http(
                        &self.turn_state,
                        /*expected_auth_owner*/ None,
                        self.client.event_sender.as_ref(),
                        prompt,
                        model_info,
                        session_telemetry,
                        effort,
                        summary,
                        service_tier,
                        responses_metadata,
                        inference_trace,
                    )
                    .await
            }
        }
    }
}

/// Stamp a ResponsesWsRequest with the current time.
///
/// Meant to be called just before sending the request over the socket, to capture realistic
/// transport timing.
fn stamp_ws_stream_request_start_ms(request: &mut ResponsesWsRequest<'_>) {
    let ResponsesWsRequest::ResponseCreate(payload) = request;
    payload
        .client_metadata
        .get_or_insert_with(HashMap::new)
        .insert(
            X_CODEX_WS_STREAM_REQUEST_START_MS_CLIENT_METADATA_KEY.to_string(),
            crate::turn_timing::now_unix_timestamp_ms().to_string(),
        );
}
