//! Responses API streaming over HTTP via `ResponsesHttpExecutor`.

use super::*;

impl<'a> ResponsesHttpExecutor<'a> {
    pub(crate) fn new(client: &'a ModelClient) -> Self {
        Self { client }
    }

    #[allow(clippy::too_many_arguments)]
    /// Builds shared Responses API transport options and request-body options.
    ///
    /// Keeping option construction in one place ensures request-scoped headers are consistent
    /// regardless of transport choice.
    async fn build_responses_options(
        &self,
        turn_state: &Arc<OnceLock<String>>,
        responses_metadata: &CodexResponsesMetadata,
        compression: Compression,
        use_responses_lite: bool,
    ) -> ApiResponsesOptions {
        ApiResponsesOptions {
            session_id: Some(self.client.responses_session_id(responses_metadata)),
            thread_id: Some(responses_metadata.thread_id.to_string()),
            session_source: Some(self.client.state.session_source.clone()),
            extra_headers: {
                let mut headers = build_responses_headers(
                    self.client.state.beta_features_header.as_deref(),
                    Some(turn_state),
                );
                add_originator_header(&mut headers, self.client.state.originator.as_str());
                headers.extend(
                    self.client
                        .build_responses_compatibility_headers(responses_metadata),
                );
                if let Some(header_value) = self.client.generate_attestation_header_for().await {
                    headers.insert(X_OAI_ATTESTATION_HEADER, header_value);
                }
                add_responses_lite_header(&mut headers, use_responses_lite);
                headers
            },
            compression,
            turn_state: Some(Arc::clone(turn_state)),
        }
    }
}

impl ResponsesHttpExecutor<'_> {
    fn responses_request_compression(&self, auth: Option<&CodexAuth>) -> Compression {
        if self.client.state.enable_request_compression
            && auth.is_some_and(CodexAuth::uses_codex_backend)
            && self.client.state.provider.info().is_openai()
        {
            Compression::Zstd
        } else {
            Compression::None
        }
    }

    /// Streams a turn via the OpenAI Responses API over HTTP.
    ///
    /// Handles reasoning summaries, verbosity, and the `text` controls used for output schemas.
    #[allow(clippy::too_many_arguments)]
    #[instrument(
        name = "model_client.stream_responses_api",
        level = "info",
        skip_all,
        fields(
            model = %model_info.slug,
            wire_api = %self.client.state.provider.info().wire_api,
            transport = "responses_http",
            http.method = "POST",
            api.path = tracing::field::Empty,
            turn.has_metadata_header = responses_metadata.has_turn_metadata()
        )
    )]
    pub(crate) async fn stream_responses_http(
        &self,
        turn_state: &Arc<OnceLock<String>>,
        expected_auth_owner: Option<AuthOwner>,
        event_sender: Option<&Sender<ProtocolEvent>>,
        prompt: &Prompt,
        model_info: &ModelInfo,
        session_telemetry: &SessionTelemetry,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &CodexResponsesMetadata,
        inference_trace: &InferenceTraceContext,
    ) -> Result<ResponseStream> {
        let auth_manager = self.client.state.provider.auth_manager();
        let mut auth_recovery = auth_manager
            .as_ref()
            .map(AuthManager::unauthorized_recovery);
        let mut provider_auth_recovery_attempted = false;
        let mut pending_retry = PendingUnauthorizedRetry::default();
        loop {
            let client_setup = self
                .client
                .current_client_setup(ClientRouting::Workspace)
                .await?;
            if expected_auth_owner.as_ref().is_some_and(|owner| {
                Some(owner) != client_setup.auth_owner.as_ref() || !owner.is_current()
            }) {
                return Err(CodexErr::TurnAborted);
            }
            let include_internal = self
                .client
                .state
                .provider
                .include_internal_metadata(&client_setup.api_provider);
            let responses_headers = self
                .client
                .responses_headers(client_setup.auth.as_ref(), &model_info.slug);
            tracing::Span::current().record("api.path", "/responses");
            let transport = client_setup.http_transport("/responses")?;
            let request_auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            )
            .map_err(|error| self.client.state.provider.map_api_error(error))?;
            let (request_telemetry, sse_telemetry) = ModelClientSession::build_streaming_telemetry(
                session_telemetry,
                request_auth_context,
                RequestRouteTelemetry::for_endpoint("/responses"),
                self.client.state.auth_env_telemetry.clone(),
            );
            let compression = self.responses_request_compression(client_setup.auth.as_ref());
            let mut options = self
                .build_responses_options(
                    turn_state,
                    responses_metadata,
                    compression,
                    model_info.use_responses_lite,
                )
                .await;

            let mut request = self.client.build_responses_request(
                prompt,
                model_info,
                effort.clone(),
                summary,
                service_tier.clone(),
                responses_metadata,
                include_internal,
            )?;
            self.client.set_guardian_metadata(
                &mut request.client_metadata,
                responses_metadata.parent_response_id.as_deref(),
                client_setup.auth.as_ref(),
                &responses_headers,
            );
            if is_guardian_reviewer(&responses_headers) {
                request.service_tier = None;
            }
            if let Some(header_value) = self.client.build_routing_hint_header(
                client_setup.auth.as_ref(),
                &responses_headers,
                &request.model,
                request.service_tier.as_deref(),
            ) {
                options
                    .extra_headers
                    .insert(X_CODEX_ROUTING_HINT_HEADER, header_value);
            }
            request.access_programs = cyber_access_program::for_auth(
                client_setup.auth.as_ref(),
                prompt.cyber_access_program,
                self.client.api_key_cyber_access_programs,
            )?;
            self.client
                .validate_required_cyber_access_program(request.access_programs)?;
            self.client
                .prepare_response_items_for_request(&mut request.input);
            let request_session_telemetry =
                session_telemetry_for_request(session_telemetry, &request);
            options.extra_headers.extend(responses_headers);
            let thread_id = self.client.state.thread_id.to_string();
            let kind = codex_extension_api::ModelRequestKind::Generation;
            crate::model_request::check_budget(&self.client.request_contributors, &thread_id, kind)
                .await?;
            let interceptors = crate::model_request::prepare(
                &self.client.request_contributors,
                &thread_id,
                &model_info.slug,
                kind,
                &mut request.client_metadata,
            );
            let inference_trace_attempt = inference_trace.start_attempt();
            inference_trace_attempt.add_request_headers(&mut options.extra_headers);
            if let Some(input) = tool_metadata::bounded_input(&request, &request.input) {
                if let Some(recorder) = &self.client.executed_tool_calls {
                    recorder.invalidate_wire_inventory_loss(&request.input, &input);
                }
                request.input = input;
            }
            inference_trace_attempt.record_started(&request);
            let client = ApiResponsesClient::new(
                transport,
                client_setup.api_provider,
                client_setup.api_auth,
            )
            .with_telemetry(Some(request_telemetry), Some(sse_telemetry));
            let stream_result = client.stream_request(request, options).await;

            match stream_result {
                Ok(stream) => {
                    return Ok(map_response_stream(
                        stream,
                        request_session_telemetry,
                        inference_trace_attempt,
                        &self.client.state,
                        interceptors,
                        /*last_response_tx*/ None,
                    ));
                }
                Err(ApiError::Transport(unauthorized_transport))
                    if self
                        .client
                        .state
                        .provider
                        .is_recoverable_auth_error(&unauthorized_transport) =>
                {
                    let response_debug_context =
                        extract_response_debug_context(&unauthorized_transport);
                    inference_trace_attempt.record_failed(
                        &unauthorized_transport,
                        response_debug_context.request_id.as_deref(),
                        /*output_items*/ &[],
                    );
                    pending_retry = PendingUnauthorizedRetry::from_recovery(
                        handle_unauthorized(
                            unauthorized_transport,
                            &mut auth_recovery,
                            &mut provider_auth_recovery_attempted,
                            session_telemetry,
                            &self.client.state.provider,
                            event_sender,
                            responses_metadata.turn_id.as_deref(),
                        )
                        .await?,
                    );
                    continue;
                }
                Err(err) => {
                    let response_debug_context =
                        extract_response_debug_context_from_api_error(&err);
                    let err = self.client.state.provider.map_api_error(err);
                    inference_trace_attempt.record_failed(
                        &err,
                        response_debug_context.request_id.as_deref(),
                        /*output_items*/ &[],
                    );
                    return Err(err);
                }
            }
        }
    }
}
