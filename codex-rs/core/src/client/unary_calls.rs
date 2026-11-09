//! Unary (non-streaming) provider calls: Realtime WebRTC call setup and memory summarization.

use super::*;

impl ModelClient {
    pub(super) async fn realtime_client_setup(
        &self,
        auth_context: &codex_model_provider::ProviderAuthContext,
        http_client_factory: HttpClientFactory,
        base_url: Option<&str>,
        headers: Option<&ApiHeaderMap>,
        call_setup: Option<&CurrentClientSetup>,
    ) -> Result<CurrentClientSetup> {
        self.validate_required_cyber_access_program(/*programs*/ None)?;
        // Realtime uses the OpenAI-compatible API protocol independently of the inference transport.
        let mut info = ModelProviderInfo {
            name: OPENAI_PROVIDER_NAME.to_owned(),
            aws: None,
            ..self.state.provider.info().clone()
        };
        if let Some(headers) = headers {
            info.http_headers = Some(
                headers
                    .iter()
                    .map(|(name, value)| {
                        value
                            .to_str()
                            .map(|value| (name.to_string(), value.to_string().into()))
                            .map_err(|_| CodexErr::InvalidRequest(format!("invalid {name} header")))
                    })
                    .collect::<Result<_>>()?,
            );
            info.env_http_headers = None;
        }
        if let Some(base_url) = base_url {
            let mut url = url::Url::parse(base_url).map_err(|error| {
                CodexErr::InvalidRequest(format!("invalid realtime URL: {error}"))
            })?;
            if url.scheme() == "wss" {
                url.set_scheme("https")
                    .map_err(|_| CodexErr::InvalidRequest("invalid realtime URL".into()))?;
            }
            info.base_url = Some(url.to_string());
        }
        if info
            .base_url
            .as_deref()
            .is_none_or(codex_runtime_agent_instance::is_openai_api_url)
        {
            info.requires_openai_auth = true;
        }
        let provider = auth_context.model_provider(info.clone());
        if let Some(call_setup) = call_setup
            && call_setup.runtime_id.is_none()
            && provider.runtime_auth().is_none()
        {
            // Call creation and its sideband share the captured credentials and account policy.
            return Ok(CurrentClientSetup {
                auth: call_setup.auth.clone(),
                auth_owner: call_setup.auth_owner.clone(),
                auth_revision: call_setup.auth_revision,
                api_provider: provider.api_provider_for_auth(call_setup.auth.as_ref())?,
                redirect_policy: call_setup.redirect_policy,
                api_auth: Arc::clone(&call_setup.api_auth),
                agent_identity_telemetry: call_setup.agent_identity_telemetry.clone(),
                runtime_id: None,
                http_client_factory,
            });
        }
        let mut setup = codex_model_provider::resolve_request(
            provider.as_ref(),
            Some(ProviderAuthScope {
                agent_identity_policy: self.agent_identity_policy,
                session_source: self.state.session_source.clone(),
                agent_identity_session_fallback: self.state.agent_identity_session_fallback.clone(),
            }),
            /*routing*/ None,
            http_client_factory,
        )
        .await?;
        if let (Some((runtime, mode)), Some(expected_auth)) = (
            provider.runtime_auth(),
            call_setup.and_then(|setup| setup.auth.as_ref()),
        ) {
            let credentials = provider.auth_manager();
            let runtime = codex_model_provider::select_runtime_provider(
                &info,
                credentials.as_ref(),
                runtime,
            )?;
            let selected = codex_model_provider::select_runtime_auth_for(
                &runtime,
                expected_auth,
                &setup.api_provider.headers,
            )
            .await?;
            setup.api_auth = provider
                .compose_auth(codex_model_provider::ResolvedProviderAuth {
                    credential: Some(selected.auth().clone()),
                    auth: selected.clone().into_auth_provider(*mode),
                    agent_identity_telemetry: setup.agent_identity_telemetry.clone(),
                    runtime: Some(selected.clone()),
                })
                .await?
                .auth;
            setup.auth = Some(selected.auth().clone());
            setup.auth_revision = Some(selected.change_state().generation);
            setup.auth_owner = Some(selected.auth_owner().clone());
            setup.runtime_id = Some(selected.runtime().id().to_string());
            setup.http_client_factory = selected
                .bind_http_client_factory(setup.http_client_factory)
                .map_err(|error| codex_api::map_api_error(error.into()))?;
        }
        Ok(setup)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_realtime_call_with_headers(
        &self,
        sdp: String,
        session_config: ApiRealtimeSessionConfig,
        mut extra_headers: ApiHeaderMap,
        api_provider_override: Option<ApiProvider>,
        sideband_base_url: &str,
        auth_context: &codex_model_provider::ProviderAuthContext,
        http_client_factory: HttpClientFactory,
    ) -> Result<RealtimeWebrtcCallStart> {
        let mut setup = self
            .realtime_client_setup(
                auth_context,
                http_client_factory,
                api_provider_override
                    .as_ref()
                    .map(|provider| provider.base_url.as_str()),
                api_provider_override
                    .as_ref()
                    .map(|provider| &provider.headers),
                /*call_setup*/ None,
            )
            .await?;
        if let Some(header_value) = self.generate_attestation_header_for().await {
            extra_headers.insert(X_OAI_ATTESTATION_HEADER, header_value);
        }
        if let Some(api_provider) = api_provider_override {
            setup.api_provider = api_provider;
        }
        let sideband = self
            .realtime_client_setup(
                auth_context,
                setup.http_client_factory.clone(),
                Some(sideband_base_url),
                Some(&setup.api_provider.headers),
                Some(&setup),
            )
            .await?;
        let transport = setup.http_transport(REALTIME_CALLS_ENDPOINT)?;
        let response = ApiRealtimeCallClient::new(transport, setup.api_provider, setup.api_auth)
            .create_with_session_and_headers(sdp, session_config, extra_headers.clone())
            .await
            .map_err(|error| self.state.provider.map_api_error(error))?;
        let mut sideband_headers = sideband.api_provider.headers;
        sideband_headers.extend(extra_headers);
        Ok(RealtimeWebrtcCallStart {
            sdp: response.sdp,
            call_id: response.call_id,
            sideband_headers,
            sideband_auth: sideband.api_auth,
            sideband_http_client_factory: sideband.http_client_factory,
        })
    }

    pub(crate) async fn realtime_sideband_auth(
        &self,
        mut extra_headers: ApiHeaderMap,
        base_url: &str,
        auth_context: &codex_model_provider::ProviderAuthContext,
        http_client_factory: HttpClientFactory,
    ) -> Result<(SharedAuthProvider, ApiHeaderMap, HttpClientFactory)> {
        let setup = self
            .realtime_client_setup(
                auth_context,
                http_client_factory,
                Some(base_url),
                /*headers*/ None,
                /*call_setup*/ None,
            )
            .await?;
        if let Some(header_value) = self.generate_attestation_header_for().await {
            extra_headers.insert(X_OAI_ATTESTATION_HEADER, header_value);
        }
        let mut headers = setup.api_provider.headers;
        headers.extend(extra_headers);
        Ok((setup.api_auth, headers, setup.http_client_factory))
    }

    /// Builds memory summaries for each provided normalized raw memory.
    ///
    /// This is a unary call (no streaming) to `/v1/memories/trace_summarize`.
    ///
    /// The model selection, reasoning effort, and telemetry context are passed explicitly to keep
    /// `ModelClient` session-scoped.
    pub async fn summarize_memories(
        &self,
        raw_memories: Vec<ApiRawMemory>,
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        session_telemetry: &SessionTelemetry,
    ) -> Result<Vec<ApiMemorySummarizeOutput>> {
        if raw_memories.is_empty() {
            return Ok(Vec::new());
        }
        self.validate_required_cyber_access_program(/*programs*/ None)?;

        let client_setup = self
            .current_client_setup(ClientRouting::ConfiguredProvider)
            .await?;
        let transport = client_setup.http_transport(MEMORIES_SUMMARIZE_ENDPOINT)?;
        let request_telemetry = Self::build_request_telemetry(
            session_telemetry,
            AuthRequestTelemetryContext::new(
                client_setup.auth.as_ref().map(CodexAuth::auth_mode),
                client_setup.api_auth.as_ref(),
                client_setup.agent_identity_telemetry.clone(),
                PendingUnauthorizedRetry::default(),
            )
            .map_err(|error| self.state.provider.map_api_error(error))?,
            RequestRouteTelemetry::for_endpoint(MEMORIES_SUMMARIZE_ENDPOINT),
            self.state.auth_env_telemetry.clone(),
        );
        let client =
            ApiMemoriesClient::new(transport, client_setup.api_provider, client_setup.api_auth)
                .with_telemetry(Some(request_telemetry));

        let payload = ApiMemorySummarizeInput {
            model: model_info.slug.clone(),
            raw_memories,
            reasoning: effort
                .map(|effort| model_info.resolve_reasoning_effort(effort))
                .map(|effort| Reasoning {
                    effort: Some(effort),
                    summary: None,
                    context: None,
                }),
        };

        client
            .summarize_input(&payload, self.build_subagent_headers())
            .await
            .map_err(|error| self.state.provider.map_api_error(error))
    }
}
