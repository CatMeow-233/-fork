//! Session- and turn-scoped helpers for talking to model provider APIs.
//!
//! `ModelClient` is intended to live for the lifetime of a Codex session and holds the stable
//! configuration and state needed to talk to a provider (auth, provider selection, conversation id,
//! and transport fallback state).
//!
//! Per-turn settings (model selection, reasoning controls, telemetry context, and turn metadata)
//! are passed explicitly to streaming and unary methods so that the turn lifetime is visible at the
//! call site.
//!
//! A [`ModelClientSession`] is created per turn and is used to stream one or more Responses API
//! requests during that turn. It caches a Responses WebSocket connection (opened lazily) and stores
//! per-turn state such as the `x-codex-turn-state` token used for sticky routing.
//! Cached connections, incremental response state, and turn routing are discarded when auth
//! ownership changes.
//!
//! WebSocket prewarm is a v2-only `response.create` with `generate=false`; it waits for completion
//! so the next request can reuse the same connection and `previous_response_id`.
//!
//! Turn execution performs prewarm as a best-effort step before the first stream request so the
//! subsequent request can reuse the same connection.
//!
//! ## Retry-Budget Tradeoff
//!
//! WebSocket prewarm is treated as the first websocket connection attempt for a turn. If it
//! fails, normal stream retry/fallback logic handles recovery on the same turn.
//!
//! ## Module Layout
//!
//! This file holds the shared types and the provider setup helper every transport uses; behavior
//! lives in child modules: `lifecycle` (construction, session settings, turn-session lifecycle),
//! `request_building`, `websocket_connection`, `websocket_stream` (the `stream` entry point,
//! prewarm, continuation reuse), `http_transport`, `response_stream`, `unauthorized_recovery`,
//! `api_telemetry`, and `unary_calls` (Realtime and memories).

use codex_login::AuthOwner;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[path = "client_tool_metadata.rs"]
mod tool_metadata;

mod api_telemetry;
mod http_transport;
mod lifecycle;
mod request_building;
mod response_stream;
mod unary_calls;
mod unauthorized_recovery;
mod websocket_connection;
mod websocket_stream;

use api_telemetry::api_error_http_status;
use request_building::add_responses_lite_header;
use request_building::build_responses_headers;
use request_building::is_guardian_reviewer;
use request_building::session_telemetry_for_request;
#[cfg(test)]
use response_stream::map_response_events;
use response_stream::map_response_stream;
use unauthorized_recovery::handle_unauthorized;

use crate::CodexResponsesHeaders;
use crate::PromptCacheKey;
use crate::tools::ExecutedToolCalls;
use async_channel::Sender;
use codex_api::AgentIdentityTelemetry;
use codex_api::ApiError;
use codex_api::AuthProvider;
use codex_api::Compression;
use codex_api::MemoriesClient as ApiMemoriesClient;
use codex_api::MemorySummarizeInput as ApiMemorySummarizeInput;
use codex_api::MemorySummarizeOutput as ApiMemorySummarizeOutput;
use codex_api::Provider as ApiProvider;
use codex_api::RESPONSES_PATH;
use codex_api::RawMemory as ApiRawMemory;
use codex_api::RealtimeCallClient as ApiRealtimeCallClient;
use codex_api::RealtimeSessionConfig as ApiRealtimeSessionConfig;
use codex_api::Reasoning;
use codex_api::ReasoningContext;
use codex_api::RequestTelemetry;
use codex_api::ResponseCreateWsRequest;
use codex_api::ResponsesApiRequest;
use codex_api::ResponsesClient as ApiResponsesClient;
use codex_api::ResponsesOptions as ApiResponsesOptions;
use codex_api::ResponsesWebsocketClient as ApiWebSocketResponsesClient;
use codex_api::ResponsesWebsocketConnection as ApiWebSocketConnection;
use codex_api::ResponsesWsRequest;
use codex_api::SharedAuthProvider;
use codex_api::SseTelemetry;
use codex_api::StreamOptions;
use codex_api::TransportError;
use codex_api::WebsocketTelemetry;
use codex_api::auth_header_telemetry;
use codex_api::build_session_headers;
use codex_api::create_text_param_for_request;
use codex_api::response_create_client_metadata;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::RefreshTokenError;
use codex_login::UnauthorizedRecovery;
use codex_login::default_client::add_originator_header;
use codex_otel::SessionTelemetry;
use codex_otel::WEBSOCKET_CONTINUATION_COUNT_METRIC;
use codex_otel::current_span_w3c_trace_context;
use codex_protocol::ItemIdGenerator;
use codex_protocol::ResponseItemId;
use codex_protocol::auth::AuthMode;

use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::Verbosity as VerbosityConfig;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::AuthRecoveryEvent;
use codex_protocol::protocol::Event as ProtocolEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::W3cTraceContext;
use codex_rollout_trace::InferenceTraceAttempt;
use codex_rollout_trace::InferenceTraceContext;
use codex_tools::ToolSpec;
use codex_tools::create_tools_json_for_responses_lite;
use codex_tools::create_tools_raw_json_for_responses_api;
use eventsource_stream::Event;
use eventsource_stream::EventStreamError;
use futures::StreamExt;
use http::HeaderMap as ApiHeaderMap;
use http::HeaderValue;
use http::StatusCode;
use serde::Deserialize;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::trace;
use tracing::warn;
use uuid::Uuid;

use crate::attestation::AttestationContext;
use crate::attestation::AttestationProvider;
use crate::attestation::X_OAI_ATTESTATION_HEADER;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::client_common::ResponseStream;
use crate::context::BaseInstructionsFragment;
use crate::context::ContextualUserFragment;
use crate::cyber_access_program;
use crate::feedback_tags;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::subagent_header_value;
use crate::util::emit_feedback_auth_recovery_tags;
use codex_feedback::FeedbackRequestTags;
use codex_feedback::emit_feedback_request_tags_with_auth_env;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_login::auth_env_telemetry::AuthEnvTelemetry;
use codex_login::auth_env_telemetry::collect_auth_env_telemetry;
use codex_model_provider::AgentIdentitySessionFallback;
use codex_model_provider::ProviderAuthScope;
use codex_model_provider::ProviderUnauthorizedRecovery;
use codex_model_provider::ResolvedProviderRequest as CurrentClientSetup;
use codex_model_provider::ResponsesConnectionKey;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::WorkspaceRoutingContext;
#[cfg(test)]
use codex_model_provider_info::DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_NAME;
use codex_model_provider_info::WireApi;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_response_debug_context::extract_response_debug_context;
use codex_response_debug_context::extract_response_debug_context_from_api_error;
use codex_response_debug_context::telemetry_api_error_message;
use codex_response_debug_context::telemetry_transport_error_message;

pub const OPENAI_BETA_HEADER: &str = "OpenAI-Beta";
pub const X_CODEX_INSTALLATION_ID_HEADER: &str = "x-codex-installation-id";
pub const X_CODEX_ROUTING_HINT_HEADER: &str = "x-codex-routing-hint";
pub const X_CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";
pub const X_CODEX_TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";
pub const X_CODEX_PARENT_THREAD_ID_HEADER: &str = "x-codex-parent-thread-id";
pub const X_CODEX_WINDOW_ID_HEADER: &str = "x-codex-window-id";
pub const X_OPENAI_MEMGEN_REQUEST_HEADER: &str = "x-openai-memgen-request";
pub const X_OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";
pub const X_RESPONSESAPI_INCLUDE_TIMING_METRICS_HEADER: &str =
    "x-responsesapi-include-timing-metrics";
const X_CODEX_WS_STREAM_REQUEST_START_MS_CLIENT_METADATA_KEY: &str =
    "x-codex-ws-stream-request-start-ms";
const WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY: &str =
    "ws_request_header_x_openai_internal_codex_responses_lite";
const RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE: &str = "responses_websockets=2026-02-06";
const X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER: &str =
    "x-openai-internal-codex-responses-lite";
const REALTIME_CALLS_ENDPOINT: &str = "/realtime/calls";
const MEMORIES_SUMMARIZE_ENDPOINT: &str = "/memories/trace_summarize";
#[cfg(test)]
pub(crate) const WEBSOCKET_CONNECT_TIMEOUT: Duration =
    Duration::from_millis(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS);

/// Session-scoped state shared by all [`ModelClient`] clones.
///
/// This is intentionally kept minimal so `ModelClient` does not need to hold a full `Config`. Most
/// configuration is per turn and is passed explicitly to streaming/unary methods.
struct ModelClientState {
    thread_id: ThreadId,
    item_id_generator: ItemIdGenerator,
    provider: SharedModelProvider,
    workspace_routing: WorkspaceRoutingContext,
    auth_env_telemetry: AuthEnvTelemetry,
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
    include_attestation: bool,
    attestation_provider: Option<Arc<dyn AttestationProvider>>,
    disable_websockets: AtomicBool,
    agent_identity_session_fallback: AgentIdentitySessionFallback,
    cached_websocket_session: StdMutex<WebsocketSession>,
    /// Last full tool list used for sampling, retained across turns and connection resets.
    last_inference_tools: StdMutex<Option<Arc<[ToolSpec]>>>,
}

impl std::fmt::Debug for ModelClientState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelClientState")
            .field("thread_id", &self.thread_id)
            .field("provider", &self.provider)
            .field("workspace_routing", &self.workspace_routing)
            .field("auth_env_telemetry", &self.auth_env_telemetry)
            .field("session_source", &self.session_source)
            .field("originator", &self.originator)
            .field("model_verbosity", &self.model_verbosity)
            .field(
                "content_item_kinds_enabled",
                &self.content_item_kinds_enabled,
            )
            .field(
                "reasoning_effort_override_enabled",
                &self.reasoning_effort_override_enabled,
            )
            .field(
                "enable_request_compression",
                &self.enable_request_compression,
            )
            .field("include_timing_metrics", &self.include_timing_metrics)
            .field("beta_features_header", &self.beta_features_header)
            .field(
                "concurrent_reasoning_summaries_enabled",
                &self.concurrent_reasoning_summaries_enabled,
            )
            .field(
                "output_token_replay_enabled",
                &self.output_token_replay_enabled,
            )
            .field("include_attestation", &self.include_attestation)
            .field("attestation_provider", &self.attestation_provider)
            .field("disable_websockets", &self.disable_websockets)
            .field(
                "agent_identity_session_fallback",
                &self.agent_identity_session_fallback,
            )
            .field("cached_websocket_session", &self.cached_websocket_session)
            .field("last_inference_tools", &self.last_inference_tools)
            .finish()
    }
}

enum ClientRouting {
    Workspace,
    ConfiguredProvider,
}

#[derive(Clone, Copy)]
struct RequestRouteTelemetry {
    endpoint: &'static str,
}

impl RequestRouteTelemetry {
    fn for_endpoint(endpoint: &'static str) -> Self {
        Self { endpoint }
    }
}

/// A session-scoped client for model-provider API calls.
///
/// This holds configuration and state that should be shared across turns within a Codex session
/// (auth, provider selection, thread id, and transport fallback state).
///
/// WebSocket fallback is session-scoped: once a turn activates the HTTP fallback, subsequent turns
/// will also use HTTP for the remainder of the session.
///
/// Turn-scoped settings (model selection, reasoning controls, telemetry context, and turn
/// metadata) are passed explicitly to the relevant methods to keep turn lifetime visible at the
/// call site.
#[derive(Debug, Clone)]
pub struct ModelClient {
    state: Arc<ModelClientState>,
    agent_identity_policy: AgentIdentityAuthPolicy,
    prompt_cache_key_policy: PromptCacheKey,
    session_id_override: Option<String>,
    codex_responses_headers: Option<Arc<CodexResponsesHeaders>>,
    event_sender: Option<Sender<ProtocolEvent>>,
    http_client_factory: HttpClientFactory,
    restored_history: bool,
    request_contributors: Vec<Arc<dyn codex_extension_api::ModelRequestContributor>>,
    executed_tool_calls: Option<ExecutedToolCalls>,
    // Resolved once when the session is created, like other session feature flags.
    api_key_cyber_access_programs: cyber_access_program::ApiKeyCyberAccessPrograms,
    required_cyber_access_programs: Option<Arc<cyber_access_program::RequiredCyberAccessPrograms>>,
}

/// A turn-scoped streaming session created from a [`ModelClient`].
///
/// The session establishes a Responses WebSocket connection lazily and reuses it across multiple
/// requests within the turn. Runtime-authenticated requests reconnect between requests to
/// revalidate the registered credential source. It also caches per-turn state:
///
/// - The last full request, so subsequent calls can reuse incremental websocket request payloads
///   only when the current request is an incremental extension of the previous one.
/// - The `x-codex-turn-state` sticky-routing token, which must be replayed for all requests within
///   the same turn.
///
/// Create a fresh `ModelClientSession` for each Codex turn. Reusing it across turns would replay
/// the previous turn's sticky-routing token into the next turn, which violates the client/server
/// contract and can cause routing bugs.
pub struct ModelClientSession {
    client: ModelClient,
    websocket_session: WebsocketSession,
    /// Turn state for sticky routing.
    ///
    /// This is an `OnceLock` that stores the turn state value received from the server
    /// on turn start via the `x-codex-turn-state` response header. Once set, this value
    /// should be sent back to the server in the `x-codex-turn-state` request header for
    /// all subsequent requests within the same turn to maintain sticky routing.
    ///
    /// This is a contract between the client and server: we receive it at turn start,
    /// keep sending it unchanged between turn requests (e.g., for retries, incremental
    /// appends, or continuation requests), and must not send it between different turns.
    /// An auth ownership change clears it so the new owner gets fresh routing state.
    turn_state: Arc<OnceLock<String>>,
}

/// Executes HTTP Responses requests without owning a turn's WebSocket state.
pub(crate) struct ResponsesHttpExecutor<'a> {
    client: &'a ModelClient,
}

#[derive(Debug, Clone)]
struct LastResponse {
    response_id: String,
    items_added: Vec<ResponseItem>,
}

struct WebsocketContinuation {
    response_id: String,
    items: Vec<ResponseItem>,
    from_untraced_warmup: bool,
}

#[derive(Debug, Default)]
struct WebsocketSession {
    connection: Option<ApiWebSocketConnection>,
    // Outer None means no successful handshake yet. The inner revision is optional for
    // providers without managed auth. Keep it across resets so retries compare to the last socket.
    last_connected_auth_revision: Option<Option<u64>>,
    responses_headers: ApiHeaderMap,
    parent_routing_affinity: Option<codex_http_client::ChatGptRoutingAffinity>,
    runtime_id: Option<String>,
    /// Owner of the cached state, including before a connection is opened.
    auth_owner: Option<AuthOwner>,
    connection_key: Option<ResponsesConnectionKey>,
    last_request: Option<ResponsesApiRequest>,
    last_response_rx: Option<oneshot::Receiver<LastResponse>>,
    last_response_from_untraced_warmup: bool,
    connection_reused: StdMutex<bool>,
    continuation_reset_reason: Option<&'static str>,
}

// Continuation diagnostics use bounded labels, never request contents or identifiers.
#[derive(Debug, PartialEq, Eq)]
enum NonIncrementalReason {
    NoPreviousRequest,
    NoPreviousResponse,
    NoPreviousResponseId,
    RequestPropertyChanged {
        reason: &'static str,
    },
    InputShortened {
        previous: &'static str,
    },
    InputMismatch {
        previous: &'static str,
        current: &'static str,
    },
}

enum WebsocketStreamOutcome {
    Stream(ResponseStream),
    FallbackToHttp,
}

/// Result of opening a WebRTC Realtime call.
///
/// The SDP answer goes back to the client. The call id and auth headers stay on the server so the
/// ordinary Realtime WebSocket machinery can join the same in-progress call as a sideband
/// controller.
pub(crate) struct RealtimeWebrtcCallStart {
    pub(crate) sdp: String,
    pub(crate) call_id: String,
    pub(crate) sideband_headers: ApiHeaderMap,
    pub(crate) sideband_auth: SharedAuthProvider,
    pub(crate) sideband_http_client_factory: HttpClientFactory,
}

impl ModelClient {
    /// Returns auth + provider configuration resolved from the current session auth state.
    ///
    /// This centralizes setup used by both prewarm and normal request paths so they stay in
    /// lockstep when auth/provider resolution changes.
    async fn current_client_setup(&self, routing: ClientRouting) -> Result<CurrentClientSetup> {
        self.provider_client_setup(routing, &self.state.provider)
            .await
    }

    async fn provider_client_setup(
        &self,
        routing: ClientRouting,
        provider: &SharedModelProvider,
    ) -> Result<CurrentClientSetup> {
        let routing = match routing {
            ClientRouting::Workspace => Some(&self.state.workspace_routing),
            ClientRouting::ConfiguredProvider => None,
        };
        codex_model_provider::resolve_request(
            provider.as_ref(),
            Some(ProviderAuthScope {
                agent_identity_policy: self.agent_identity_policy,
                session_source: self.state.session_source.clone(),
                agent_identity_session_fallback: self.state.agent_identity_session_fallback.clone(),
            }),
            routing,
            self.http_client_factory.clone(),
        )
        .await
    }
}

const RESPONSE_STREAM_CHANNEL_CAPACITY: usize = 1600;
const STREAM_DROPPED_REASON: &str = "response stream dropped before provider terminal event";

/// Handles a 401 response by optionally refreshing ChatGPT tokens once.
///
/// When refresh succeeds, the caller should retry the API call; otherwise
/// the mapped `CodexErr` is returned to the caller.
#[derive(Clone, Copy, Debug)]
struct UnauthorizedRecoveryExecution {
    mode: &'static str,
    phase: &'static str,
}

#[derive(Clone, Copy, Debug, Default)]
struct PendingUnauthorizedRetry {
    retry_after_unauthorized: bool,
    recovery_mode: Option<&'static str>,
    recovery_phase: Option<&'static str>,
}

#[derive(Clone, Debug, Default)]
struct AuthRequestTelemetryContext {
    auth_mode: Option<&'static str>,
    auth_header_attached: bool,
    auth_header_name: Option<&'static str>,
    agent_identity_telemetry: Option<AgentIdentityTelemetry>,
    retry_after_unauthorized: bool,
    recovery_mode: Option<&'static str>,
    recovery_phase: Option<&'static str>,
}

struct WebsocketConnectParams<'a> {
    runtime_id: Option<String>,
    session_telemetry: &'a SessionTelemetry,
    api_provider: codex_api::Provider,
    auth_revision: Option<u64>,
    api_auth: SharedAuthProvider,
    http_client_factory: HttpClientFactory,
    auth_owner: Option<AuthOwner>,
    responses_metadata: &'a CodexResponsesMetadata,
    auth_context: AuthRequestTelemetryContext,
    request_route_telemetry: RequestRouteTelemetry,
    responses_headers: &'a ApiHeaderMap,
}

struct ApiTelemetry {
    session_telemetry: SessionTelemetry,
    auth_context: AuthRequestTelemetryContext,
    request_route_telemetry: RequestRouteTelemetry,
    auth_env_telemetry: AuthEnvTelemetry,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
