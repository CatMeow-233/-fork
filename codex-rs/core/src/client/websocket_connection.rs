//! Opening, reusing, and resetting the turn-scoped Responses WebSocket connection.

use super::*;

impl WebsocketSession {
    pub(super) fn reset(&mut self, reason: Option<&'static str>) {
        // Per-socket backend metrics call a resend after reconnect "initial".
        // Retain the loss reason across reconnects/turns until the next send.
        let continuation_reset_reason = self
            .continuation_reset_reason
            .or_else(|| self.last_request.as_ref().and(reason));
        *self = Self {
            auth_owner: self.auth_owner.clone(),
            continuation_reset_reason,
            last_connected_auth_revision: self.last_connected_auth_revision,
            ..Default::default()
        };
    }

    fn set_connection_reused(&self, connection_reused: bool) {
        *self
            .connection_reused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = connection_reused;
    }

    pub(super) fn connection_reused(&self) -> bool {
        *self
            .connection_reused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl ModelClient {
    /// Opens a websocket connection using the same header and telemetry wiring as normal turns.
    ///
    /// Both startup prewarm and in-turn `needs_new` reconnects call this path so handshake
    /// behavior remains consistent across both flows.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn connect_websocket(
        &self,
        session_telemetry: &SessionTelemetry,
        api_provider: codex_api::Provider,
        api_auth: SharedAuthProvider,
        http_client_factory: &HttpClientFactory,
        responses_metadata: &CodexResponsesMetadata,
        auth_context: AuthRequestTelemetryContext,
        request_route_telemetry: RequestRouteTelemetry,
        responses_headers: &ApiHeaderMap,
        parent_routing_affinity: Option<&codex_http_client::ChatGptRoutingAffinity>,
        reauth: bool,
    ) -> std::result::Result<ApiWebSocketConnection, ApiError> {
        let mut headers = self.build_websocket_headers(responses_metadata).await;
        headers.extend(responses_headers.clone());
        if let Some(affinity) = parent_routing_affinity {
            let uri = api_provider
                .websocket_url_for_path(RESPONSES_PATH)
                .map_err(|err| ApiError::Stream(format!("failed to build websocket URL: {err}")))?
                .as_str()
                .parse::<http::Uri>()
                .map_err(|err| ApiError::Stream(format!("invalid websocket URI: {err}")))?;
            let cookies = headers
                .get(http::header::COOKIE)
                .cloned()
                .or_else(|| api_provider.headers.get(http::header::COOKIE).cloned())
                .or_else(|| http_client_factory.chatgpt_cookie_header(&uri));
            if let Some(cookies) = affinity.merge_cookie_header(&uri, cookies.as_ref()) {
                headers.insert(http::header::COOKIE, cookies);
            }
        }
        let websocket_telemetry = ModelClientSession::build_websocket_telemetry(
            session_telemetry,
            auth_context.clone(),
            request_route_telemetry,
            self.state.auth_env_telemetry.clone(),
        );
        let websocket_connect_timeout = self.state.provider.info().websocket_connect_timeout();
        let start = Instant::now();
        let result = match tokio::time::timeout(
            websocket_connect_timeout,
            ApiWebSocketResponsesClient::new(api_provider, api_auth).connect(
                http_client_factory,
                headers,
                codex_login::default_client::default_headers(),
                /*turn_state*/ None,
                Some(websocket_telemetry),
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(ApiError::Transport(TransportError::Timeout)),
        };
        let error_message = result.as_ref().err().map(telemetry_api_error_message);
        let response_debug = result
            .as_ref()
            .err()
            .map(extract_response_debug_context_from_api_error)
            .unwrap_or_default();
        let status = result.as_ref().err().and_then(api_error_http_status);
        session_telemetry.record_websocket_connect(
            start.elapsed(),
            status,
            error_message.as_deref(),
            auth_context.auth_header_attached,
            auth_context.auth_header_name,
            auth_context.retry_after_unauthorized,
            auth_context.recovery_mode,
            auth_context.recovery_phase,
            request_route_telemetry.endpoint,
            reauth,
            response_debug.request_id.as_deref(),
            response_debug.cf_ray.as_deref(),
            response_debug.auth_error.as_deref(),
            response_debug.auth_error_code.as_deref(),
            auth_context.agent_identity_telemetry(),
        );
        emit_feedback_request_tags_with_auth_env(
            &FeedbackRequestTags {
                endpoint: request_route_telemetry.endpoint,
                auth_header_attached: auth_context.auth_header_attached,
                auth_header_name: auth_context.auth_header_name,
                auth_mode: auth_context.auth_mode,
                auth_retry_after_unauthorized: Some(auth_context.retry_after_unauthorized),
                auth_recovery_mode: auth_context.recovery_mode,
                auth_recovery_phase: auth_context.recovery_phase,
                auth_connection_reused: Some(false),
                auth_request_id: response_debug.request_id.as_deref(),
                auth_cf_ray: response_debug.cf_ray.as_deref(),
                auth_error: response_debug.auth_error.as_deref(),
                auth_error_code: response_debug.auth_error_code.as_deref(),
                auth_recovery_followup_success: auth_context
                    .retry_after_unauthorized
                    .then_some(result.is_ok()),
                auth_recovery_followup_status: auth_context
                    .retry_after_unauthorized
                    .then_some(status)
                    .flatten(),
            },
            &self.state.auth_env_telemetry,
        );
        result
    }

    /// Builds websocket handshake headers for both prewarm and turn-time reconnect.
    pub(super) async fn build_websocket_headers(
        &self,
        responses_metadata: &CodexResponsesMetadata,
    ) -> ApiHeaderMap {
        let mut headers = build_responses_headers(
            self.state.beta_features_header.as_deref(),
            /*turn_state*/ None,
        );
        add_originator_header(&mut headers, self.state.originator.as_str());
        if let Ok(header_value) = HeaderValue::from_str(&responses_metadata.thread_id) {
            headers.insert("x-client-request-id", header_value);
        }
        headers.extend(build_session_headers(
            Some(self.responses_session_id(responses_metadata)),
            Some(responses_metadata.thread_id.to_string()),
        ));
        headers.extend(self.build_responses_compatibility_headers(responses_metadata));
        if let Some(routing_hint) = &responses_metadata.routing_hint {
            headers.insert(X_CODEX_ROUTING_HINT_HEADER, routing_hint.clone());
        }
        if let Some(header_value) = self.generate_attestation_header_for().await {
            headers.insert(X_OAI_ATTESTATION_HEADER, header_value);
        }
        headers.insert(
            OPENAI_BETA_HEADER,
            HeaderValue::from_static(RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE),
        );
        if self.state.include_timing_metrics {
            headers.insert(
                X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER,
                HeaderValue::from_static("true"),
            );
        }
        headers
    }
}

impl ModelClientSession {
    /// Opportunistically preconnects a websocket for this turn-scoped client session.
    ///
    /// This performs only connection setup; it never sends prompt payloads.
    pub async fn preconnect_websocket(
        &mut self,
        model_info: &ModelInfo,
        service_tier: Option<String>,
        session_telemetry: &SessionTelemetry,
        responses_metadata: &CodexResponsesMetadata,
    ) -> Result<()> {
        // Runtime admission belongs to the request that will use the connection.
        if !self.client.responses_websocket_enabled()
            || self.client.state.provider.runtime_auth().is_some()
        {
            return Ok(());
        }
        let provider = Arc::clone(&self.client.state.provider);
        let auth_manager = provider.auth_manager();
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
            let auth_context = AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                pending_retry,
            )
            .map_err(|error| provider.map_api_error(error))?;
            let responses_headers = self
                .client
                .responses_headers(client_setup.auth.as_ref(), &model_info.slug);
            let mut websocket_metadata = responses_metadata.clone();
            websocket_metadata.routing_hint = self.client.build_routing_hint_header(
                client_setup.auth.as_ref(),
                &responses_headers,
                &model_info.slug,
                model_info
                    .service_tier_for_request(service_tier.clone())
                    .as_deref(),
            );
            match self
                .websocket_connection(WebsocketConnectParams {
                    session_telemetry,
                    api_provider: client_setup.api_provider,
                    auth_revision: client_setup.auth_revision,
                    api_auth: client_setup.api_auth,
                    http_client_factory: client_setup.http_client_factory,
                    auth_owner: client_setup.auth_owner,
                    responses_metadata: &websocket_metadata,
                    auth_context,
                    request_route_telemetry: RequestRouteTelemetry::for_endpoint("/responses"),
                    responses_headers: &responses_headers,
                    runtime_id: client_setup.runtime_id,
                })
                .await
            {
                Ok(_) => return Ok(()),
                Err(ApiError::Transport(TransportError::Http { status, .. }))
                    if status == StatusCode::UPGRADE_REQUIRED =>
                {
                    self.try_switch_fallback_transport(session_telemetry, model_info);
                    return Ok(());
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
                }
                Err(err) => return Err(provider.map_api_error(err)),
            }
        }
    }

    /// Returns a websocket connection for this turn.
    #[instrument(
        name = "model_client.websocket_connection",
        level = "info",
        skip_all,
        fields(
            provider = %self.client.state.provider.info().name,
            wire_api = %self.client.state.provider.info().wire_api,
            transport = "responses_websocket",
            api.path = "/responses",
            turn.has_metadata_header = params.responses_metadata.has_turn_metadata()
        )
    )]
    pub(super) async fn websocket_connection(
        &mut self,
        params: WebsocketConnectParams<'_>,
    ) -> std::result::Result<&ApiWebSocketConnection, ApiError> {
        let WebsocketConnectParams {
            session_telemetry,
            api_provider,
            auth_revision,
            api_auth,
            http_client_factory,
            auth_owner,
            responses_metadata,
            auth_context,
            request_route_telemetry,
            responses_headers,
            runtime_id,
        } = params;
        let parent_routing_affinity = if is_guardian_reviewer(responses_headers) {
            responses_metadata.parent_routing_affinity.clone()
        } else {
            None
        };
        let connection_key = ResponsesConnectionKey::new(&api_provider, auth_revision);
        let reset_reason = match self.websocket_session.connection.as_ref() {
            // Reauthenticate before each runtime request; an open socket cannot prove liveness.
            Some(_)
                if runtime_id.is_some()
                    || self.websocket_session.responses_headers != *responses_headers
                    || self.websocket_session.connection_key.as_ref() != Some(&connection_key)
                    || self.websocket_session.runtime_id != runtime_id
                    || self.websocket_session.parent_routing_affinity
                        != parent_routing_affinity =>
            {
                Some("other")
            }
            Some(conn) if conn.is_closed().await => Some("connection_closed"),
            Some(_) | None => None,
        };
        let needs_new = self.websocket_session.connection.is_none() || reset_reason.is_some();
        // Resolving an external auth provider can change ownership during client setup.
        let owner_changed = self.websocket_session.auth_owner != auth_owner
            || auth_owner.as_ref().is_some_and(|owner| !owner.is_current());
        if owner_changed {
            self.turn_state = Arc::new(OnceLock::new());
        }

        if needs_new || owner_changed {
            self.websocket_session.reset(if owner_changed {
                Some("other")
            } else {
                reset_reason
            });
            let reauth = self
                .websocket_session
                .last_connected_auth_revision
                .is_some_and(|previous| previous != auth_revision);
            let new_conn = self
                .client
                .connect_websocket(
                    session_telemetry,
                    api_provider,
                    api_auth,
                    &http_client_factory,
                    responses_metadata,
                    auth_context,
                    request_route_telemetry,
                    responses_headers,
                    parent_routing_affinity.as_ref(),
                    reauth,
                )
                .await?;
            self.websocket_session.connection = Some(new_conn);
            self.websocket_session.last_connected_auth_revision = Some(auth_revision);
            self.websocket_session.parent_routing_affinity = parent_routing_affinity;
            self.websocket_session.responses_headers = responses_headers.clone();
            self.websocket_session.runtime_id = runtime_id;
            self.websocket_session.auth_owner = auth_owner;
            self.websocket_session.connection_key = Some(connection_key);
            self.websocket_session
                .set_connection_reused(/*connection_reused*/ false);
        } else {
            self.websocket_session
                .set_connection_reused(/*connection_reused*/ true);
        }

        self.websocket_session
            .connection
            .as_ref()
            .ok_or(ApiError::Stream(
                "websocket connection is unavailable".to_string(),
            ))
    }
}
