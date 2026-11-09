//! `ModelClient` construction, session-scoped settings, and the turn-session lifecycle
//! (creation, cached WebSocket handoff, and HTTP fallback).

use super::*;

impl ModelClient {
    #[allow(clippy::too_many_arguments)]
    /// Creates a new session-scoped `ModelClient`.
    ///
    /// All arguments are expected to be stable for the lifetime of a Codex session. Per-turn values
    /// are passed to [`ModelClientSession::stream`] (and other turn-scoped methods) explicitly. The
    /// HTTP client factory must come from the effective session configuration so every transport
    /// observes the resolved outbound proxy policy.
    pub fn new(
        model_provider: SharedModelProvider,
        agent_identity_policy: AgentIdentityAuthPolicy,
        thread_id: ThreadId,
        item_id_generator: ItemIdGenerator,
        session_source: SessionSource,
        originator: String,
        model_verbosity: Option<VerbosityConfig>,
        content_item_kinds_enabled: bool,
        reasoning_effort_override_enabled: bool,
        enable_request_compression: bool,
        include_timing_metrics: bool,
        beta_features_header: Option<String>,
        concurrent_reasoning_summaries_enabled: bool,
        output_token_replay_enabled: bool,
        attestation_provider: Option<Arc<dyn AttestationProvider>>,
        http_client_factory: HttpClientFactory,
        workspace_routing: WorkspaceRoutingContext,
        request_contributors: Vec<Arc<dyn codex_extension_api::ModelRequestContributor>>,
    ) -> Self {
        let codex_api_key_env_enabled = model_provider
            .auth_manager()
            .as_ref()
            .is_some_and(|manager| manager.codex_api_key_env_enabled());
        let auth_env_telemetry =
            collect_auth_env_telemetry(model_provider.info(), codex_api_key_env_enabled);
        let include_attestation = model_provider.supports_attestation();
        // Fixed-effort workers use request-level effort even when managed requirements
        // pin the feature on. Share this decision with update injection and pinning.
        let memory_consolidation = matches!(
            &session_source,
            SessionSource::Internal(InternalSessionSource::MemoryConsolidation)
                | SessionSource::SubAgent(SubAgentSource::MemoryConsolidation)
        );
        let reasoning_effort_override_enabled = reasoning_effort_override_enabled
            && !crate::guardian::is_basic_session_source(&session_source)
            && !memory_consolidation;
        Self {
            state: Arc::new(ModelClientState {
                thread_id,
                item_id_generator,
                provider: model_provider,
                workspace_routing,
                auth_env_telemetry,
                session_source,
                originator,
                model_verbosity,
                content_item_kinds_enabled,
                reasoning_effort_override_enabled,
                enable_request_compression,
                include_timing_metrics,
                beta_features_header,
                concurrent_reasoning_summaries_enabled,
                output_token_replay_enabled,
                include_attestation,
                attestation_provider,
                disable_websockets: AtomicBool::new(false),
                agent_identity_session_fallback: AgentIdentitySessionFallback::default(),
                cached_websocket_session: StdMutex::new(WebsocketSession::default()),
                last_inference_tools: StdMutex::new(None),
            }),
            agent_identity_policy,
            prompt_cache_key_policy: PromptCacheKey::default(),
            session_id_override: None,
            codex_responses_headers: None,
            event_sender: None,
            http_client_factory,
            restored_history: false,
            request_contributors,
            executed_tool_calls: None,
            api_key_cyber_access_programs:
                cyber_access_program::ApiKeyCyberAccessPrograms::UnsupportedProvider,
            required_cyber_access_programs: None,
        }
    }

    pub(crate) fn with_executed_tool_calls(mut self, recorder: ExecutedToolCalls) -> Self {
        self.executed_tool_calls = Some(recorder);
        self
    }

    pub(crate) fn reasoning_effort_override_enabled(&self, model_info: &ModelInfo) -> bool {
        self.state.reasoning_effort_override_enabled
            && self.state.provider.info().is_openai()
            && model_info.supports_reasoning_effort_updates
    }

    pub(crate) fn with_restored_history(mut self, restored_history: bool) -> Self {
        self.restored_history = restored_history;
        self
    }

    pub(crate) fn with_session_context(
        mut self,
        prompt_cache_key: PromptCacheKey,
        session_id_override: Option<String>,
        event_sender: Sender<ProtocolEvent>,
        codex_responses_headers: Option<Arc<CodexResponsesHeaders>>,
        api_key_cyber_access_programs: cyber_access_program::ApiKeyCyberAccessPrograms,
        required_cyber_access_programs: Option<
            Arc<cyber_access_program::RequiredCyberAccessPrograms>,
        >,
    ) -> Self {
        self.prompt_cache_key_policy = prompt_cache_key;
        self.session_id_override = session_id_override;
        self.event_sender = Some(event_sender);
        self.codex_responses_headers = codex_responses_headers;
        self.api_key_cyber_access_programs = api_key_cyber_access_programs;
        self.required_cyber_access_programs = required_cyber_access_programs;
        self
    }

    pub(crate) fn validate_required_cyber_access_program(
        &self,
        programs: Option<codex_api::AccessPrograms>,
    ) -> Result<()> {
        if let Some(required) = &self.required_cyber_access_programs
            && !required
                .allowed
                .iter()
                .any(|program| programs == Some((*program).into()))
        {
            return Err(CodexErr::InvalidRequest(
                "this thread requires an allowed explicit access program".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn prompt_cache_key_policy(&self) -> PromptCacheKey {
        self.prompt_cache_key_policy.clone()
    }

    /// The caller's policy controls the request body independently of the session-id header.
    pub(super) fn prompt_cache_key(&self, metadata: &CodexResponsesMetadata) -> Option<String> {
        match &self.prompt_cache_key_policy {
            PromptCacheKey::SessionDerived => Some(self.session_derived_cache_key(metadata)),
            PromptCacheKey::Custom(key) => Some(key.clone()),
            PromptCacheKey::Omit => None,
        }
    }

    fn session_derived_cache_key(&self, metadata: &CodexResponsesMetadata) -> String {
        if let Some(session_id) = &self.session_id_override {
            return session_id.clone();
        }

        if let SessionSource::Internal(source) = &self.state.session_source
            && let Some(parent_thread_id) = metadata.parent_thread_id
        {
            return format!("{source}:{parent_thread_id}");
        }

        metadata.session_id.clone()
    }

    // ChatGPT derives cache affinity from the Responses session-id header. Keep the
    // actual session identity in turn metadata, hooks, and history/notes requests.
    pub(super) fn responses_session_id(&self, metadata: &CodexResponsesMetadata) -> String {
        if self.state.session_source.is_non_root_agent() {
            metadata.session_id.clone()
        } else {
            self.session_derived_cache_key(metadata)
        }
    }

    /// Creates a fresh turn-scoped streaming session.
    ///
    /// This constructor does not perform network I/O itself; the session opens a websocket lazily
    /// when the first stream request is issued.
    pub fn new_session(&self) -> ModelClientSession {
        let auth_owner = self.auth_owner();
        let mut websocket_session = self.take_cached_websocket_session();
        if websocket_session.auth_owner != auth_owner {
            // Drop the old owner's cache before this turn can establish fresh routing state.
            websocket_session.reset(Some("other"));
            websocket_session.auth_owner = auth_owner;
        }
        ModelClientSession {
            client: self.clone(),
            websocket_session,
            turn_state: Arc::new(OnceLock::new()),
        }
    }

    pub(crate) fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.state.provider.auth_manager()
    }

    pub(crate) fn auth_owner(&self) -> Option<AuthOwner> {
        codex_model_provider::request_auth_owner(self.state.provider.as_ref())
    }

    fn take_cached_websocket_session(&self) -> WebsocketSession {
        let mut cached_websocket_session = self
            .state
            .cached_websocket_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *cached_websocket_session)
    }

    fn store_cached_websocket_session(&self, websocket_session: WebsocketSession) {
        *self
            .state
            .cached_websocket_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = websocket_session;
    }

    pub(crate) fn force_http_fallback(
        &self,
        session_telemetry: &SessionTelemetry,
        _model_info: &ModelInfo,
    ) -> bool {
        let websocket_enabled = self.responses_websocket_enabled();
        let activated =
            websocket_enabled && !self.state.disable_websockets.swap(true, Ordering::Relaxed);
        if activated {
            warn!("falling back to HTTP");
            session_telemetry.counter(
                "codex.transport.fallback_to_http",
                /*inc*/ 1,
                &[("from_wire_api", "responses_websocket")],
            );
        }

        self.store_cached_websocket_session(WebsocketSession::default());
        activated
    }

    /// Returns whether the Responses-over-WebSocket transport is active for this session.
    ///
    /// WebSocket use is controlled by provider capability and session-scoped fallback state.
    pub fn responses_websocket_enabled(&self) -> bool {
        if !self.state.provider.info().supports_websockets
            || self.state.disable_websockets.load(Ordering::Relaxed)
        {
            return false;
        }

        true
    }

    pub(crate) async fn prewarm_auth(&self) -> Result<()> {
        self.current_client_setup(ClientRouting::Workspace)
            .await
            .map(|_| ())
    }
}

impl ModelClientSession {
    pub(crate) fn inference_tools_changed(&self, tools: &Arc<[ToolSpec]>) -> bool {
        let previous = self
            .client
            .state
            .last_inference_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(Arc::clone(tools));
        previous.is_some_and(|previous| previous != *tools)
    }

    pub(crate) fn turn_state_with_auth_owner(&self) -> (Option<String>, Option<AuthOwner>) {
        let owner = self.websocket_session.auth_owner.clone();
        let turn_state = owner
            .as_ref()
            .is_none_or(AuthOwner::is_current)
            .then(|| self.turn_state.get().cloned())
            .flatten();
        (turn_state, owner)
    }

    /// Permanently disables WebSockets for this Codex session and resets WebSocket state.
    ///
    /// This is used after exhausting the provider retry budget, to force subsequent requests onto
    /// the HTTP transport.
    ///
    /// Returns `true` if this call activated fallback, or `false` if fallback was already active.
    pub(crate) fn try_switch_fallback_transport(
        &mut self,
        session_telemetry: &SessionTelemetry,
        model_info: &ModelInfo,
    ) -> bool {
        let activated = self
            .client
            .force_http_fallback(session_telemetry, model_info);
        self.websocket_session = WebsocketSession::default();
        activated
    }
}

impl Drop for ModelClientSession {
    fn drop(&mut self) {
        let websocket_session = std::mem::take(&mut self.websocket_session);
        self.client
            .store_cached_websocket_session(websocket_session);
    }
}
