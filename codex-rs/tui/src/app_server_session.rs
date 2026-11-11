//! App-server session facade used by the TUI event loop.
//!
//! This module owns the typed JSON-RPC calls needed by the TUI and keeps
//! request/response plumbing out of `App` and `ChatWidget`.

mod account;
pub(crate) use account::SessionAccount;
mod cloud_project;
mod daybreak_api_key_compat;
mod events;
mod external_agent_config;
pub(crate) mod fs;
mod history;
mod models;
pub(crate) mod provider_selection;
#[cfg(test)]
#[path = "app_server_session/provider_selection_tests.rs"]
mod provider_selection_tests;
mod read_state;
mod realtime;
mod rollout_history;
pub(crate) use read_state::unread_revision;
mod side_chat_history;
use side_chat_history::side_context_checkpoint;
mod startup_launch;
mod thread_list;
pub(crate) mod user_message_submission;
mod web_search;

#[cfg(test)]
#[path = "app_server_session/collaboration_catalog_tests.rs"]
mod collaboration_catalog_tests;

pub(crate) use events::AppServerSessionEvent;
pub(crate) use history::HISTORY_ITEM_PAGE_LIMIT;
pub(crate) use history::HISTORY_ITEM_SCAN_LIMIT;
pub(crate) use history::HistoryHydrationScope;
pub(crate) use history::INITIAL_HISTORY_TURN_LIMIT;
pub(crate) use history::thread_items_page_params;
pub(crate) use side_chat_history::SideChat;
pub(crate) use side_chat_history::side_chat_prompt_preview;
pub(crate) use startup_launch::StartupLaunchChoices;

use crate::app_event::PermissionProfileSelection;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::FeedbackAudience;
use crate::dynamic_tools_mcp::DynamicToolMcpServer;
use crate::dynamic_tools_mcp::ThreadToolTransport;
use crate::legacy_core::config::Config;
use crate::local_settings::LocalSettings;
use crate::service_tier_resolution;
use crate::session_state::MessageHistoryMetadata;
use crate::session_state::ThreadSessionState;
use crate::status::StatusAccountDisplay;
use crate::status::plan_type_display_name;
use crate::terminal_visualization_instructions::with_terminal_visualization_instructions;
use codex_app_server_client::AppServerClient;
use codex_app_server_client::AppServerPath;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::AuthMode;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::ConfigRequirementsReadResponse;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::LogoutAccountResponse;
use codex_app_server_protocol::MemoryResetResponse;
use codex_app_server_protocol::Model as ApiModel;
use codex_app_server_protocol::NewThreadModelDefaults;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ReviewDelivery;
use codex_app_server_protocol::ReviewStartParams;
use codex_app_server_protocol::ReviewStartResponse;
use codex_app_server_protocol::ReviewTarget;
use codex_app_server_protocol::SessionSource;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadApproveGuardianDeniedActionParams;
use codex_app_server_protocol::ThreadApproveGuardianDeniedActionResponse;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadBackgroundTerminal;
use codex_app_server_protocol::ThreadBackgroundTerminalsCleanParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsCleanResponse;
use codex_app_server_protocol::ThreadBackgroundTerminalsListParams;
use codex_app_server_protocol::ThreadBackgroundTerminalsListResponse;
use codex_app_server_protocol::ThreadCompactStartParams;
use codex_app_server_protocol::ThreadCompactStartResponse;
use codex_app_server_protocol::ThreadDeleteParams;
use codex_app_server_protocol::ThreadDeleteResponse;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadGoalClearParams;
use codex_app_server_protocol::ThreadGoalClearResponse;
use codex_app_server_protocol::ThreadGoalGetParams;
use codex_app_server_protocol::ThreadGoalGetResponse;
use codex_app_server_protocol::ThreadGoalSetParams;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadInjectItemsParams;
use codex_app_server_protocol::ThreadInjectItemsResponse;
use codex_app_server_protocol::ThreadListParams;
use codex_app_server_protocol::ThreadListResponse;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadLoadedListResponse;
use codex_app_server_protocol::ThreadMemoryMode;
use codex_app_server_protocol::ThreadMemoryModeSetParams;
use codex_app_server_protocol::ThreadMemoryModeSetResponse;
use codex_app_server_protocol::ThreadMetadataGitInfoUpdateParams;
use codex_app_server_protocol::ThreadMetadataUpdateParams;
use codex_app_server_protocol::ThreadMetadataUpdateResponse;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadSetNameParams;
use codex_app_server_protocol::ThreadSetNameResponse;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSettingsUpdateResponse;
use codex_app_server_protocol::ThreadShellCommandParams;
use codex_app_server_protocol::ThreadShellCommandResponse;
use codex_app_server_protocol::ThreadSource;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadStartSource;
use codex_app_server_protocol::ThreadStatusChangedNotification;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use codex_app_server_protocol::ThreadUnsubscribeParams;
use codex_app_server_protocol::ThreadUnsubscribeResponse;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::UserInput;
use codex_config::ConfigLayerSource;
use codex_context_fragments::SideConversationPolicy;
use codex_otel::TelemetryAuthMode;
use codex_protocol::ThreadId;
use codex_protocol::approvals::GuardianAssessmentEvent;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelAvailabilityNux;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelServiceTier;
use codex_protocol::openai_models::ModelUpgrade;
use codex_protocol::openai_models::ReasoningEffortPreset;
use codex_protocol::protocol::SubAgentSource;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use futures::FutureExt;
use futures::future::Either;
use futures::future::RemoteHandle;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const JSONRPC_INVALID_REQUEST: i64 = -32600;
const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub(crate) const EXTERNAL_AGENT_CONFIG_IMPORT_IN_PROGRESS_MESSAGE: &str = "A previous external agent import is still running. Wait for it to finish before importing again.";
const THREAD_SETTINGS_UPDATE_METHOD: &str = "thread/settings/update";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForkGoalContinuation {
    StartIfIdle,
    DeferUntilNextTurn,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForkPermissionMode {
    InheritSaved,
    OverrideFromCurrentConfig,
}

enum EmptyCatalogFallback {
    Error,
    ManagedNewThread,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkPresentation {
    Regular,
    SideConversation(SideChatStorage),
}

pub(crate) const ATTACHED_SIDE_CHAT_SOURCE: &str = "tui_side_chat";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SideChatStorage {
    Temporary,
    Attached,
}

impl SideChatStorage {
    fn policy(self) -> SideConversationPolicy {
        match self {
            Self::Temporary => SideConversationPolicy::Temporary,
            Self::Attached => SideConversationPolicy::Attached,
        }
    }

    pub(crate) fn configure_fork(self, config: &mut Config) {
        config.ephemeral = self == Self::Temporary;
        config.developer_instructions = Some(
            self.policy()
                .instructions(config.developer_instructions.as_deref()),
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ForkConfigSource {
    Local,
    Session,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThreadHistorySupport {
    Paginated,
    LegacyOnly,
}

fn bootstrap_request_error(context: &'static str, err: TypedRequestError) -> color_eyre::Report {
    let message = format!("{context}: {err}");
    color_eyre::Report::new(err).wrap_err(message)
}

pub(crate) fn is_history_pagination_unsupported(source: &JSONRPCErrorError) -> bool {
    if source.code == JSONRPC_METHOD_NOT_FOUND {
        return true;
    }

    if !matches!(
        source.code,
        JSONRPC_INVALID_REQUEST | JSONRPC_INVALID_PARAMS
    ) {
        return false;
    }

    let message = source.message.to_ascii_lowercase();
    [
        "historymode",
        "history mode",
        "excludeturns",
        "exclude turns",
        "thread/turns/list",
        "thread/items/list",
    ]
    .into_iter()
    .any(|field| message.contains(field))
        || (message.contains("paginated")
            && ["unknown variant", "unsupported variant", "invalid enum"]
                .into_iter()
                .any(|error| message.contains(error)))
}

pub(crate) async fn request_thread_start_with_history_fallback(
    request_handle: &AppServerRequestHandle,
    mut request_id: RequestId,
    mut params: ThreadStartParams,
) -> std::result::Result<(ThreadStartResponse, ThreadHistorySupport, bool), TypedRequestError> {
    let mut history_support = ThreadHistorySupport::Paginated;
    loop {
        match request_handle
            .request_typed(ClientRequest::ThreadStart {
                request_id,
                params: params.clone(),
            })
            .await
        {
            Ok(response) => {
                let task_tools_available = params.dynamic_tools.is_some()
                    || params
                        .config
                        .as_ref()
                        .is_some_and(|config| config.contains_key("mcp_servers.codex_tui"));
                return Ok((response, history_support, task_tools_available));
            }
            Err(TypedRequestError::Server { source, .. })
                if params.history_mode.is_some() && is_history_pagination_unsupported(&source) =>
            {
                params.history_mode = None;
                history_support = ThreadHistorySupport::LegacyOnly;
                request_id = RequestId::String(format!("legacy-thread-start-{}", Uuid::new_v4()));
            }
            Err(TypedRequestError::Server { source, .. })
                if params.dynamic_tools.is_some()
                    && matches!(
                        source.code,
                        JSONRPC_INVALID_REQUEST | JSONRPC_INVALID_PARAMS
                    )
                    && {
                        let message = source.message.to_ascii_lowercase();
                        ["dynamictools", "dynamic tool", "namespace", "inputschema"]
                            .into_iter()
                            .any(|field| message.contains(field))
                    } =>
            {
                tracing::warn!(
                    error = %source.message,
                    "app server does not support TUI dynamic tools; starting without them"
                );
                params.dynamic_tools = None;
                request_id = RequestId::String(format!("legacy-thread-start-{}", Uuid::new_v4()));
            }
            Err(err) => return Err(err),
        }
    }
}

fn is_thread_settings_update_unsupported(source: &JSONRPCErrorError) -> bool {
    source.code == JSONRPC_METHOD_NOT_FOUND
        || (source.code == JSONRPC_INVALID_REQUEST
            && source.message.contains(THREAD_SETTINGS_UPDATE_METHOD))
}

/// Data collected during the TUI bootstrap phase that the main event loop
/// needs to configure the UI, telemetry, and initial rate-limit prefetch.
///
/// Rate-limit snapshots are intentionally **not** included here; they are
/// fetched asynchronously after bootstrap returns so that the TUI can render
/// its first frame without waiting for the rate-limit round-trip.
pub(crate) struct AppServerBootstrap {
    pub(crate) duration: Duration,
    pub(crate) account_email: Option<String>,
    pub(crate) auth_mode: Option<TelemetryAuthMode>,
    pub(crate) status_account_display: Option<StatusAccountDisplay>,
    pub(crate) plan_type: Option<codex_protocol::account::PlanType>,
    /// Whether the configured model provider needs OpenAI-style auth. Combined
    /// with `has_chatgpt_account` to decide if a startup rate-limit prefetch
    /// should be fired.
    pub(crate) requires_openai_auth: bool,
    /// Cloud supplies its model when it starts or resumes a thread.
    pub(crate) default_model: Option<String>,
    pub(crate) feedback_audience: FeedbackAudience,
    pub(crate) has_chatgpt_account: bool,
    pub(crate) available_models: Vec<ModelPreset>,
    pub(crate) collaboration_modes: Vec<codex_protocol::config_types::CollaborationModeMask>,
    pub(crate) allowed_approval_policies: Option<Vec<AskForApproval>>,
}

pub(crate) struct AppServerSession {
    backend_id: crate::backend_id::BackendId,
    /// Owns events and the connection lifetime for live sessions. Request-only views share the
    /// request handle below and have no connection owner.
    connection_owner: Option<AppServerClient>,
    request_handle: AppServerRequestHandle,
    request_id_namespace: Option<Uuid>,
    next_request_id: i64,
    automatic_name_writes: HashMap<ThreadId, Arc<Semaphore>>,
    /// The newest docked replay refresh on this connection.
    pub(crate) pending_side_skills_refresh: Option<Uuid>,
    history_pagination: HashMap<ThreadId, history::ThreadHistoryPagination>,
    pub(crate) focused_conversation: Option<ThreadId>,
    pub(crate) attempted_revision: Option<String>,
    pub(crate) frozen_receipt: Option<(ThreadId, String)>,
    pub(crate) unread_revisions: HashMap<ThreadId, String>,
    task_tool_threads: HashSet<ThreadId>,
    task_tool_capabilities_dir: Option<AbsolutePathBuf>,
    task_search_generation: Arc<AtomicU64>,
    remote_cwd_override: Option<PathBuf>,
    thread_params_mode: ThreadParamsMode,
    /// Explicit CLI provider selection, distinct from resolved configuration defaults.
    pub(crate) model_provider_override: Option<String>,
    history_support: ThreadHistorySupport,
    pub(crate) thread_settings_update_supported: bool,
    default_model: Option<String>,
    available_models: Vec<ModelPreset>,
    prefetched_model_list: Option<RemoteHandle<Result<Vec<ModelPreset>>>>,
    daybreak_api_key_compat: Arc<tokio::sync::Semaphore>,
    managed_new_thread_defaults: Option<NewThreadModelDefaults>,
    external_agent_config_import_id: Mutex<Option<String>>,
    dynamic_tool_mcp: Option<Arc<DynamicToolMcpServer>>,
    pub(crate) worktree_source_config_builder:
        Option<Box<crate::legacy_core::config::ConfigBuilder>>,
}

/// An automatic save holds the gate only while writing, so a manual rename can skip a slow read.
pub(crate) struct AutomaticThreadName {
    session: AppServerSession,
    thread_id: ThreadId,
    gate: Arc<Semaphore>,
}

impl AutomaticThreadName {
    pub(crate) async fn save(mut self, title: String, cancellation: &CancellationToken) {
        let read = self
            .session
            .thread_read(self.thread_id, /*include_turns*/ false);
        let Some(Ok(thread)) = cancellation.run_until_cancelled(read).await else {
            return;
        };
        if thread.name.is_some() {
            return;
        }
        let Ok(_permit) = self.gate.acquire().await else {
            return;
        };
        if !cancellation.is_cancelled()
            && let Err(error) = self.session.thread_set_name(self.thread_id, title).await
        {
            tracing::debug!(%error, "failed to apply generated thread title");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadParamsMode {
    Embedded,
    Remote,
    /// Native hosted sessions own their execution settings and workspace.
    Cloud,
}

/// Identifies which side supplies execution settings for a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadSettingsOwner {
    /// The client may send its configured settings as request overrides.
    Client,
    /// The server retains its settings unless changed through a dedicated API.
    Server,
}

/// Determines where model settings come from when resuming a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeModelSettings {
    /// Sends the current config's model, provider, and reasoning effort as explicit overrides.
    OverrideFromCurrentConfig,
    /// Omits those overrides so app-server restores the settings saved with the thread.
    RestoreFromThread,
    /// Rejoins a loaded thread without changing any of its existing settings.
    PreserveExistingThread,
    /// Restore the saved settings and reapply the side-chat instructions on cold resume.
    PreserveSideConversation,
}

impl ThreadParamsMode {
    pub(crate) fn settings_owner(self) -> ThreadSettingsOwner {
        match self {
            Self::Embedded | Self::Remote => ThreadSettingsOwner::Client,
            Self::Cloud => ThreadSettingsOwner::Server,
        }
    }

    fn workspace_roots_from_config(self, config: &Config) -> Option<Vec<AbsolutePathBuf>> {
        match self {
            Self::Embedded => Some(config.workspace_roots.clone()),
            // Client config paths belong to the client host. Let the server resolve its
            // defaults or restore the saved roots, then adopt the roots in its response.
            Self::Remote | Self::Cloud => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SidebarFork {
    pub(crate) thread: AppServerStartedThread,
    pub(crate) boundary: Result<(), String>,
    pub(crate) inherited_profile: bool,
    pub(crate) storage: SideChatStorage,
    legacy: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct AppServerStartedThread {
    pub(crate) session: ThreadSessionState,
    pub(crate) turns: Vec<Turn>,
    pub(crate) blocks_direct_input: bool,
    pub(crate) task_tools_available: bool,
}

/// Recognize typed server errors without depending on wording or an older server's behavior.
pub(crate) fn model_provider_error(
    error: &color_eyre::eyre::Report,
) -> Option<codex_app_server_protocol::ModelProviderError> {
    error.chain().find_map(|cause| {
        let codex_app_server_client::TypedRequestError::Server { source, .. } =
            cause.downcast_ref::<codex_app_server_client::TypedRequestError>()?
        else {
            return None;
        };
        serde_json::from_value(source.data.clone()?).ok()
    })
}

pub(crate) fn is_active_writer_error(err: &color_eyre::eyre::Report) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("already has an active writer"))
}

/// Recognizes a draining server's refusal, using the legacy message only when data is absent.
pub(crate) fn is_server_shutting_down_error(error: &JSONRPCErrorError) -> bool {
    error.code == JSONRPC_INVALID_REQUEST
        && match &error.data {
            Some(data) => {
                data.get("reason").and_then(serde_json::Value::as_str) == Some("serverShuttingDown")
            }
            None => error.message == "Server is draining; retry after reconnecting",
        }
}

pub(crate) fn source_agent_path(source: &SessionSource) -> Option<String> {
    match source {
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) => {
            agent_path.clone().map(String::from)
        }
        _ => None,
    }
}

/// Uses the server capability when available and preserves compatibility with older servers.
pub(crate) fn thread_blocks_direct_input(thread: &Thread) -> bool {
    thread
        .can_accept_direct_input
        .map(|can_accept| !can_accept)
        .unwrap_or_else(|| source_agent_path(&thread.source).is_some())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TurnPermissionsOverride {
    /// Leave the app-server thread's sticky permission profile unchanged.
    Preserve,
    /// Select a named or built-in profile by id.
    ActiveProfile(ActivePermissionProfile),
    /// Apply a user-selected legacy/custom permission profile.
    LegacySandbox(PermissionProfile),
}

#[derive(Debug, thiserror::Error)]
#[error(
    "the selected permission profile cannot be safely represented by the legacy app-server sandbox policy; select a named or legacy-compatible permission profile"
)]
pub(crate) struct UnsupportedLegacyPermissionProfile;

impl AppServerSession {
    /// Platform of the app-server process, not necessarily its executor.
    pub(crate) fn app_server_platform_family(&self) -> Option<&str> {
        self.connection_owner
            .as_ref()
            .and_then(AppServerClient::platform_family)
    }

    pub(crate) fn app_server_platform_os(&self) -> Option<&str> {
        self.connection_owner
            .as_ref()
            .and_then(AppServerClient::platform_os)
    }

    pub(crate) fn new(
        backend_id: crate::backend_id::BackendId,
        client: AppServerClient,
        thread_params_mode: ThreadParamsMode,
    ) -> Self {
        let request_handle = client.request_handle();
        Self::from_parts(backend_id, Some(client), request_handle, thread_params_mode)
    }

    fn from_parts(
        backend_id: crate::backend_id::BackendId,
        connection_owner: Option<AppServerClient>,
        request_handle: AppServerRequestHandle,
        thread_params_mode: ThreadParamsMode,
    ) -> Self {
        Self {
            backend_id,
            connection_owner,
            request_handle,
            request_id_namespace: None,
            next_request_id: 1,
            automatic_name_writes: HashMap::new(),
            pending_side_skills_refresh: None,
            history_pagination: HashMap::new(),
            focused_conversation: None,
            attempted_revision: None,
            frozen_receipt: None,
            unread_revisions: HashMap::new(),
            task_tool_threads: HashSet::new(),
            task_tool_capabilities_dir: None,
            task_search_generation: Arc::new(AtomicU64::new(0)),
            remote_cwd_override: None,
            thread_params_mode,
            model_provider_override: None,
            history_support: ThreadHistorySupport::Paginated,
            thread_settings_update_supported: true,
            default_model: None,
            available_models: Vec::new(),
            prefetched_model_list: None,
            daybreak_api_key_compat: Arc::new(tokio::sync::Semaphore::new(/*permits*/ 1)),
            managed_new_thread_defaults: None,
            external_agent_config_import_id: Mutex::default(),
            dynamic_tool_mcp: None,
            worktree_source_config_builder: None,
        }
    }

    pub(crate) fn backend_id(&self) -> crate::backend_id::BackendId {
        self.backend_id
    }

    /// Creates a request-only view without transferring the live connection's events.
    pub(crate) fn request_view(&self) -> Self {
        let mut view = Self::from_parts(
            self.backend_id,
            /*connection_owner*/ None,
            self.request_handle(),
            self.thread_params_mode,
        );
        view.request_id_namespace = Some(Uuid::new_v4());
        view.remote_cwd_override = self.remote_cwd_override.clone();
        view.model_provider_override = self.model_provider_override.clone();
        view.history_support = self.history_support;
        view.thread_settings_update_supported = self.thread_settings_update_supported;
        view.default_model = self.default_model.clone();
        view.available_models = self.available_models.clone();
        view
    }

    pub(crate) async fn start_dynamic_tool_mcp(
        &mut self,
        config: Config,
        app_event_tx: AppEventSender,
        status_updates: tokio::sync::broadcast::Sender<ThreadStatusChangedNotification>,
    ) -> std::io::Result<()> {
        if self.thread_params_mode == ThreadParamsMode::Cloud {
            return Ok(());
        }
        let worktrees = if !self.uses_remote_workspace() && !config.active_project.is_untrusted() {
            // Listing a fresh, nonexistent thread is read-only and returns an empty page
            // on supported stores. Probe before advertising tools or allocating a checkout.
            let request_handle = self.request_handle();
            let support = request_handle
                .request_typed::<codex_app_server_protocol::ThreadAttachmentListResponse>(
                    ClientRequest::ThreadAttachmentList {
                        request_id: RequestId::String(uuid::Uuid::new_v4().to_string()),
                        params: codex_app_server_protocol::ThreadAttachmentListParams {
                            thread_id: uuid::Uuid::new_v4().to_string(),
                            cursor: None,
                            limit: Some(1),
                        },
                    },
                );
            // Loading the host settings only reads config files, so overlap it with the probe.
            let (support, service) = tokio::join!(
                support,
                crate::managed_worktree_tools::ManagedWorktreeTools::new(&config)
            );
            match (support, service) {
                (Err(error), _) => {
                    tracing::warn!(%error, "managed worktree attachment storage unavailable");
                    None
                }
                (Ok(_), Ok(service)) => {
                    Some(match self.worktree_source_config_builder.as_deref() {
                        Some(builder) => service.with_source_config_builder(builder.clone()),
                        None => service,
                    })
                }
                (Ok(_), Err(error)) => {
                    tracing::warn!(%error, "managed worktree tools unavailable");
                    None
                }
            }
        } else {
            None
        };
        if config
            .mcp_servers
            .get()
            .contains_key(crate::dynamic_tools::NAMESPACE)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a user-configured MCP server already owns the TUI tools namespace",
            ));
        }
        let managed_requirement = config
            .config_layer_stack
            .requirements()
            .mcp_servers
            .as_ref()
            .map(|requirements| {
                requirements
                    .value
                    .get(crate::dynamic_tools::NAMESPACE)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "managed MCP requirements do not permit the TUI task-tools server",
                        )
                    })
            })
            .transpose()?;
        let mut thread_start_params = thread_start_params_from_config(
            &config,
            self.thread_params_mode(),
            self.remote_cwd_override(),
            /*session_start_source*/ None,
        );
        let daybreak_launch_override = config
            .config_layer_stack
            .layers_high_to_low()
            .find(|layer| layer.config.get("daybreak").is_some())
            .is_some_and(|layer| {
                matches!(
                    layer.name,
                    codex_config::ConfigLayerSource::SessionFlags
                        | codex_config::ConfigLayerSource::User {
                            profile: Some(_),
                            ..
                        }
                )
            });
        thread_start_params.daybreak_enabled =
            daybreak_launch_override.then_some(config.daybreak_enabled);
        self.dynamic_tool_mcp = Some(Arc::new(
            DynamicToolMcpServer::start(
                self.request_handle(),
                thread_start_params,
                config.features.get().clone(),
                app_event_tx,
                status_updates,
                managed_requirement,
                worktrees,
            )
            .await?,
        ));
        Ok(())
    }

    pub(crate) fn thread_tool_transport(&self) -> ThreadToolTransport {
        if let Some(server) = self.dynamic_tool_mcp.as_ref() {
            ThreadToolTransport::Mcp(Arc::clone(server))
        } else {
            ThreadToolTransport::Dynamic
        }
    }

    pub(crate) fn with_thread_tool_transport(mut self, transport: ThreadToolTransport) -> Self {
        if let ThreadToolTransport::Mcp(server) = transport {
            self.dynamic_tool_mcp = Some(server);
        }
        self
    }

    pub(crate) fn with_remote_cwd_override(mut self, remote_cwd_override: Option<PathBuf>) -> Self {
        self.remote_cwd_override = remote_cwd_override;
        self
    }

    pub(crate) fn remote_cwd_override(&self) -> Option<&std::path::Path> {
        self.remote_cwd_override.as_deref()
    }

    pub(crate) fn uses_remote_workspace(&self) -> bool {
        matches!(
            self.thread_params_mode,
            ThreadParamsMode::Remote | ThreadParamsMode::Cloud
        )
    }

    pub(crate) fn uses_embedded_app_server(&self) -> bool {
        matches!(&self.connection_owner, Some(AppServerClient::InProcess(_)))
    }

    /// Carry capabilities that may exist only in memory when the optional cache is unwritable.
    pub(crate) fn inherit_task_tool_capabilities(&mut self, previous: &Self) {
        self.task_tool_threads.extend(&previous.task_tool_threads);
    }

    pub(crate) fn task_tools_available(&self, thread_id: ThreadId) -> bool {
        self.task_tool_threads.contains(&thread_id)
            || self
                .task_tool_capabilities_dir
                .as_ref()
                .is_some_and(|directory| directory.join(thread_id.to_string()).is_file())
    }

    pub(crate) fn remember_task_tool_thread(&mut self, thread_id: ThreadId) {
        if self.task_tool_threads.insert(thread_id)
            && let Some(directory) = &self.task_tool_capabilities_dir
            && let Err(error) = std::fs::create_dir_all(directory)
                .and_then(|()| std::fs::write(directory.join(thread_id.to_string()), []))
        {
            tracing::warn!(%error, %thread_id, "failed to persist task-reference capability");
        }
    }

    pub(crate) fn task_search_generation(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.task_search_generation)
    }

    pub(crate) fn codex_home_path(
        &self,
        local_codex_home: &AbsolutePathBuf,
    ) -> Option<AppServerPath> {
        self.connection_owner
            .as_ref()
            .and_then(|client| client.codex_home(local_codex_home))
    }

    pub(crate) fn server_version(&self) -> Option<&str> {
        let Some(AppServerClient::Remote(client)) = &self.connection_owner else {
            return None;
        };
        client.server_version()
    }

    pub(crate) fn server_codex_home(&self) -> Option<&str> {
        let Some(AppServerClient::Remote(client)) = &self.connection_owner else {
            return None;
        };
        client.codex_home()
    }

    pub(crate) fn prefetch_model_list(&mut self) {
        let (request, models) = self.request_model_list().remote_handle();
        tokio::spawn(request);
        self.prefetched_model_list = Some(models);
    }

    fn request_model_list(&mut self) -> impl Future<Output = Result<Vec<ModelPreset>>> + use<> {
        let mode = self.thread_params_mode;
        let handle = self.request_handle();
        let request_id = self.next_request_id();
        async move {
            models::list(mode, handle, request_id).await.map_err(|err| {
                bootstrap_request_error("model/list failed during TUI bootstrap", err)
            })
        }
    }

    pub(crate) async fn bootstrap(&mut self, config: &Config) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let account = self.read_account().await?;
        let mut bootstrap = self.bootstrap_with_account(config, account).await?;
        bootstrap.duration = started_at.elapsed();
        Ok(bootstrap)
    }

    pub(crate) async fn bootstrap_for_new_thread(
        &mut self,
        config: &Config,
    ) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let account = self.read_account().await?;
        let mut bootstrap = self
            .bootstrap_for_new_thread_with_account(config, account)
            .await?;
        bootstrap.duration = started_at.elapsed();
        Ok(bootstrap)
    }

    /// Bootstraps a new thread using a previously read account.
    ///
    /// Callers must discard a prefetched account after authentication, server, or provider changes.
    pub(crate) async fn bootstrap_for_new_thread_with_account(
        &mut self,
        config: &Config,
        account: GetAccountResponse,
    ) -> Result<AppServerBootstrap> {
        self.bootstrap_with_account_inner(config, account, EmptyCatalogFallback::ManagedNewThread)
            .await
    }

    /// Bootstraps using a previously read account.
    ///
    /// Callers must discard a prefetched account after authentication, server, or provider changes.
    pub(crate) async fn bootstrap_with_account(
        &mut self,
        config: &Config,
        account: GetAccountResponse,
    ) -> Result<AppServerBootstrap> {
        self.bootstrap_with_account_inner(config, account, EmptyCatalogFallback::Error)
            .await
    }

    async fn bootstrap_with_account_inner(
        &mut self,
        config: &Config,
        account: GetAccountResponse,
        fallback: EmptyCatalogFallback,
    ) -> Result<AppServerBootstrap> {
        let started_at = Instant::now();
        let models = match self.prefetched_model_list.take() {
            Some(models) => Either::Left(models),
            None => Either::Right(self.request_model_list()),
        };
        let requirements_request_id = self.next_request_id();
        let (mut available_models, requirements, collaboration_modes) = tokio::try_join!(
            models,
            async {
                match self
                    .request_handle
                    .request_typed::<ConfigRequirementsReadResponse>(
                        ClientRequest::ConfigRequirementsRead {
                            request_id: requirements_request_id,
                            params: None,
                        },
                    )
                    .await
                {
                    Ok(requirements) => Ok(Some(requirements)),
                    // A native transport may expose resolved thread permissions without
                    // exposing the conventional requirements catalog. Keep that absence
                    // distinct from an authoritative response with no requirements.
                    Err(TypedRequestError::Server { source, .. })
                        if self.thread_params_mode == ThreadParamsMode::Cloud
                            && source.code == -32601 =>
                    {
                        Ok(None)
                    }
                    Err(error) => Err(bootstrap_request_error(
                        "configRequirements/read failed during TUI bootstrap",
                        error,
                    )),
                }
            },
            async { Ok(crate::collaboration_modes::list(self.request_handle()).await) },
        )?;
        if let Some(requirements) = &requirements {
            crate::service_tier_resolution::constrain_server_service_tiers(
                &mut available_models,
                requirements,
            );
        }
        if matches!(account.account, Some(Account::ApiKey {})) {
            available_models = self
                .constrain_api_key_models(
                    config,
                    /*thread_id*/ None,
                    requirements
                        .as_ref()
                        .and_then(|response| response.requirements.as_ref()),
                    available_models,
                )
                .await;
        }
        let allowed_approval_policies = requirements
            .as_ref()
            .and_then(|response| response.requirements.as_ref())
            .and_then(|requirements| requirements.allowed_approval_policies.clone());
        self.managed_new_thread_defaults = requirements
            .and_then(|response| response.requirements)
            .and_then(|requirements| requirements.models)
            .and_then(|models| models.new_thread);
        let default_model = (self.thread_params_mode.settings_owner()
            == ThreadSettingsOwner::Client)
            .then(|| config.model.clone())
            .flatten()
            .or_else(|| {
                available_models
                    .iter()
                    .find(|model| model.is_default)
                    .map(|model| model.model.clone())
            })
            .or_else(|| available_models.first().map(|model| model.model.clone()))
            .or_else(|| match fallback {
                EmptyCatalogFallback::Error => None,
                EmptyCatalogFallback::ManagedNewThread => self
                    .managed_new_thread_defaults
                    .as_ref()
                    .and_then(|defaults| defaults.model.clone()),
            });
        if default_model.is_none() && self.thread_params_mode != ThreadParamsMode::Cloud {
            color_eyre::eyre::bail!(
                "No models are available. Set `model` explicitly or check your model catalog configuration."
            );
        }
        self.default_model.clone_from(&default_model);
        self.available_models = available_models.clone();

        let account = SessionAccount::from(account);
        Ok(AppServerBootstrap {
            duration: started_at.elapsed(),
            account_email: account.email().map(str::to_owned),
            auth_mode: account.auth_mode(),
            feedback_audience: account.feedback_audience(),
            has_chatgpt_account: account.has_chatgpt_account(),
            status_account_display: account.display,
            plan_type: account.plan,
            requires_openai_auth: account.requires_openai_auth,
            default_model,
            available_models,
            collaboration_modes,
            allowed_approval_policies,
        })
    }

    pub(crate) fn managed_new_thread_defaults(&self) -> Option<&NewThreadModelDefaults> {
        self.managed_new_thread_defaults.as_ref()
    }

    pub(crate) fn supports_paginated_history(&self) -> bool {
        self.history_support == ThreadHistorySupport::Paginated
    }

    /// Fetches the current account info without refreshing the auth token.
    ///
    /// Used by both `bootstrap` (to populate the initial UI) and `get_login_status`
    /// (to check auth mode without the overhead of a full bootstrap).
    pub(crate) async fn read_account(&mut self) -> Result<GetAccountResponse> {
        let account_request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::GetAccount {
                request_id: account_request_id,
                params: GetAccountParams {
                    refresh_token: false,
                },
            })
            .await
            .map_err(|err| bootstrap_request_error("account/read failed", err))
    }

    pub(crate) async fn next_event(&mut self) -> Option<AppServerSessionEvent> {
        let event = self.connection_owner.as_mut()?.next_event().await?;
        if let codex_app_server_client::AppServerEvent::ServerNotification(notification) = &event
            && let codex_app_server_protocol::ServerNotification::ThreadReadStateChanged(change) =
                notification.as_ref()
            && let Ok(id) = ThreadId::from_string(&change.thread_id)
        {
            self.remember_read_state(id, Some(change.read_state.clone()));
        }
        Some(AppServerSessionEvent {
            backend_id: self.backend_id,
            event,
        })
    }

    #[cfg(test)]
    pub(crate) async fn start_thread(&mut self, config: &Config) -> Result<AppServerStartedThread> {
        self.start_thread_with_session_start_source(
            &LocalSettings::from(config),
            config,
            /*session_start_source*/ None,
            /*remote_cwd_override*/ None,
            /*selected_profile*/ None,
            /*host_project_cwd*/ None,
        )
        .await
    }

    pub(crate) async fn start_thread_with_session_start_source(
        &mut self,
        local_settings: &LocalSettings,
        config: &Config,
        session_start_source: Option<ThreadStartSource>,
        remote_cwd_override: Option<&std::path::Path>,
        selected_profile: Option<&PermissionProfileSelection>,
        host_project_cwd: Option<&std::path::Path>,
    ) -> Result<AppServerStartedThread> {
        if self.thread_params_mode.settings_owner() == ThreadSettingsOwner::Server
            && selected_profile.is_some()
        {
            color_eyre::eyre::bail!(
                "Cloud conversations currently use server-selected permissions."
            );
        }
        let request_id = self.next_request_id();
        let session_config = self.session_config_with_effective_service_tier(config);
        let mut params = thread_start_params_from_config(
            &session_config,
            self.thread_params_mode(),
            remote_cwd_override.or(self.remote_cwd_override.as_deref()),
            session_start_source,
        );
        if self.thread_params_mode.settings_owner() == ThreadSettingsOwner::Client {
            params.model_provider = self
                .model_provider_override
                .clone()
                .or(params.model_provider);
            params.daybreak_enabled =
                (config.daybreak_enabled && !config.ephemeral).then_some(true);
        }
        if let Some(selected_profile) = selected_profile {
            params.runtime_workspace_roots = None;
            params.permissions = Some(selected_profile.profile_id.clone());
            params.sandbox = None;
            params.approval_policy = selected_profile.approval_policy;
            params.approvals_reviewer = selected_profile.approvals_reviewer.map(Into::into);
            remove_permission_config_overrides(&mut params.config);
        }
        if self.history_support == ThreadHistorySupport::LegacyOnly {
            params.history_mode = None;
        }
        if self.thread_params_mode.settings_owner() == ThreadSettingsOwner::Client {
            self.thread_tool_transport().validate_config(config)?;
            self.thread_tool_transport().configure(&mut params);
        }
        let request_handle = self.request_handle();
        let project = if self.thread_params_mode == ThreadParamsMode::Cloud {
            match host_project_cwd {
                Some(cwd) => cloud_project::from_local_cwd(cwd).await,
                None => None,
            }
        } else {
            None
        };
        let started = match project {
            Some(project) => request_handle
                .start_cloud_project_thread(request_id, params, project)
                .await
                .map(|response| (response, ThreadHistorySupport::Paginated, false)),
            None => {
                request_thread_start_with_history_fallback(&request_handle, request_id, params)
                    .await
            }
        };
        let (response, history_support, task_tools_available) = started.map_err(|err| {
            bootstrap_request_error("thread/start failed during TUI bootstrap", err)
        })?;
        if history_support == ThreadHistorySupport::LegacyOnly {
            self.history_support = ThreadHistorySupport::LegacyOnly;
        }
        // An explicit server selection must not reuse the client's local display policy.
        let selected_permissions = selected_profile.map(|_| {
            PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &response.sandbox.to_core(),
                response.cwd.as_path(),
            )
        });
        let mut started = started_thread_from_start_response(
            response,
            local_settings,
            config,
            self.thread_params_mode(),
        )
        .await?;
        if let Some(permissions) = selected_permissions {
            started.session.permission_profile = permissions;
        }
        started.task_tools_available = task_tools_available;
        if task_tools_available {
            self.remember_task_tool_thread(started.session.thread_id);
        }
        Ok(started)
    }

    #[cfg(test)]
    pub(crate) async fn fork_thread(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_with_permission_mode(
            local_settings,
            config,
            thread_id,
            ForkPermissionMode::InheritSaved,
        )
        .await
    }

    pub(crate) async fn fork_thread_with_permission_mode(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        permission_mode: ForkPermissionMode,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_at_with_presentation(
            local_settings,
            config,
            thread_id,
            /*last_turn_id*/ None,
            /*before_turn_id*/ None,
            ForkGoalContinuation::StartIfIdle,
            ForkPresentation::Regular,
            /*selected_profile*/ None,
            permission_mode,
            ForkConfigSource::Local,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fork position and named permissions are independent"
    )]
    pub(crate) async fn fork_thread_at(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        selected_profile: Option<&PermissionProfileSelection>,
    ) -> Result<AppServerStartedThread> {
        self.fork_thread_at_with_presentation(
            local_settings,
            config,
            thread_id,
            last_turn_id,
            before_turn_id,
            goal_continuation,
            ForkPresentation::Regular,
            selected_profile,
            ForkPermissionMode::InheritSaved,
            ForkConfigSource::Session,
        )
        .await
    }

    pub(crate) async fn fork_thread_for_directory_change(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        selected_profile: Option<&PermissionProfileSelection>,
    ) -> Result<AppServerStartedThread> {
        let permission_mode = match self.thread_params_mode() {
            ThreadParamsMode::Embedded => ForkPermissionMode::OverrideFromCurrentConfig,
            ThreadParamsMode::Remote | ThreadParamsMode::Cloud => ForkPermissionMode::InheritSaved,
        };
        self.fork_thread_at_with_presentation(
            local_settings,
            config,
            thread_id,
            /*last_turn_id*/ None,
            /*before_turn_id*/ None,
            ForkGoalContinuation::DeferUntilNextTurn,
            ForkPresentation::Regular,
            selected_profile,
            permission_mode,
            ForkConfigSource::Session,
        )
        .await
    }

    /// Fork and inject the side boundary on a request-only view. The live session
    /// keeps receiving notifications and drawing frames while these RPCs run.
    pub(crate) fn sidebar_fork(
        &self,
        settings: LocalSettings,
        config: Config,
        parent: ThreadId,
        profile: Option<PermissionProfileSelection>,
        preferred_storage: SideChatStorage,
    ) -> impl Future<Output = Result<SidebarFork>> + Send + use<> {
        let mut view = self
            .request_view()
            .with_thread_tool_transport(self.thread_tool_transport());
        if self.task_tools_available(parent) {
            view.remember_task_tool_thread(parent);
        }
        async move {
            let mut storage = SideChatStorage::Temporary;
            if preferred_storage == SideChatStorage::Attached
                && view.attached_side_chat_query(parent).await?.is_some()
                && {
                    let thread = view.thread_read(parent, /*include_turns*/ false).await?;
                    !thread.ephemeral && thread.history_mode == ThreadHistoryMode::Paginated
                }
            {
                storage = SideChatStorage::Attached;
            }
            let fork = view
                .fork_side_thread(&settings, config.clone(), parent, profile.as_ref(), storage)
                .await;
            let thread = match fork {
                Err(err)
                    if storage == SideChatStorage::Attached
                        && side_chat_history::is_attached_thread_store_unsupported(&err) =>
                {
                    storage = SideChatStorage::Temporary;
                    view.fork_side_thread(&settings, config, parent, profile.as_ref(), storage)
                        .await?
                }
                result => result?,
            };
            let child = thread.session.thread_id;
            let items = storage.policy().fork_items();
            let boundary = match view.thread_inject_items(child, items).await {
                Ok(_) if storage == SideChatStorage::Attached => {
                    match side_context_checkpoint(thread.session.rollout_path.as_deref()).await {
                        Ok(Some(_)) => Ok(()),
                        Ok(None) => Err("Side conversation context was not saved".to_string()),
                        Err(err) => Err(err.to_string()),
                    }
                }
                result => result.map(|_| ()).map_err(|err| err.to_string()),
            };
            if let Err(message) = &boundary
                && storage == SideChatStorage::Attached
            {
                if let Err(error) = view.thread_delete(child).await {
                    tracing::warn!(%child, %error, "failed to roll back attached side chat");
                } else {
                    color_eyre::eyre::bail!(
                        "Failed to prepare side conversation {child}: {message}"
                    );
                }
            }
            Ok(SidebarFork {
                thread,
                boundary,
                inherited_profile: profile.is_some(),
                storage,
                legacy: view.history_support == ThreadHistorySupport::LegacyOnly,
            })
        }
    }

    pub(crate) fn observe_sidebar_fork(&mut self, fork: &SidebarFork) {
        if fork.legacy {
            self.history_support = ThreadHistorySupport::LegacyOnly;
        }
        if fork.thread.task_tools_available {
            self.remember_task_tool_thread(fork.thread.session.thread_id);
        }
    }

    pub(crate) async fn fork_side_thread(
        &mut self,
        local_settings: &LocalSettings,
        mut config: Config,
        thread_id: ThreadId,
        selected_profile: Option<&PermissionProfileSelection>,
        storage: SideChatStorage,
    ) -> Result<AppServerStartedThread> {
        storage.configure_fork(&mut config);
        self.fork_thread_at_with_presentation(
            local_settings,
            config,
            thread_id,
            /*last_turn_id*/ None,
            /*before_turn_id*/ None,
            ForkGoalContinuation::StartIfIdle,
            ForkPresentation::SideConversation(storage),
            selected_profile,
            ForkPermissionMode::InheritSaved,
            ForkConfigSource::Session,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "keep local preferences separate while the legacy Config parameter is still required"
    )]
    async fn fork_thread_at_with_presentation(
        &mut self,
        local_settings: &LocalSettings,
        config: Config,
        thread_id: ThreadId,
        last_turn_id: Option<String>,
        before_turn_id: Option<String>,
        goal_continuation: ForkGoalContinuation,
        presentation: ForkPresentation,
        selected_profile: Option<&PermissionProfileSelection>,
        permission_mode: ForkPermissionMode,
        config_source: ForkConfigSource,
    ) -> Result<AppServerStartedThread> {
        if self.thread_params_mode == ThreadParamsMode::Cloud {
            color_eyre::eyre::bail!("Cloud conversations cannot be forked in the TUI yet.");
        }
        self.thread_tool_transport().validate_config(&config)?;
        let fork_parent = match presentation {
            ForkPresentation::Regular => self
                .thread_read(thread_id, /*include_turns*/ false)
                .await
                .ok(),
            ForkPresentation::SideConversation(_) => None,
        };
        let exclude_turns = self.history_support == ThreadHistorySupport::Paginated
            && (fork_parent
                .as_ref()
                .is_some_and(|thread| thread.history_mode == ThreadHistoryMode::Paginated)
                || presentation != ForkPresentation::Regular);
        let request_id = self.next_request_id();
        let session_config = if config.model.is_none() {
            // Avoid inferring a tier from the stale client default model.
            config.clone()
        } else {
            self.session_config_with_effective_service_tier(&config)
        };
        let mut params = ThreadForkParams {
            attach_to_source: presentation
                == ForkPresentation::SideConversation(SideChatStorage::Attached),
            last_turn_id,
            before_turn_id,
            defer_goal_continuation: goal_continuation == ForkGoalContinuation::DeferUntilNextTurn,
            exclude_turns,
            ..thread_fork_params_from_config(
                session_config,
                thread_id,
                self.thread_params_mode(),
                self.remote_cwd_override.as_deref(),
            )
        };
        if params.attach_to_source {
            params.thread_source =
                Some(ThreadSource::Feature(ATTACHED_SIDE_CHAT_SOURCE.to_string()));
        }
        params.model_provider = match config_source {
            ForkConfigSource::Local => self
                .model_provider_override
                .clone()
                .or(params.model_provider),
            ForkConfigSource::Session => Some(config.model_provider_id.clone()),
        };
        if config_source == ForkConfigSource::Session {
            // Active-session config has already adopted the server's roots. Fork does
            // not restore saved roots when omitted, so preserve this explicit selection.
            params.runtime_workspace_roots = Some(config.workspace_roots.clone());
            if permission_mode == ForkPermissionMode::InheritSaved && selected_profile.is_none() {
                params.approvals_reviewer = None;
                if let Some(overrides) = params.config.as_mut() {
                    overrides.remove("approvals_reviewer");
                }
            }
        }
        if self.thread_params_mode() == ThreadParamsMode::Remote
            && permission_mode == ForkPermissionMode::InheritSaved
            && selected_profile.is_none()
        {
            params.approval_policy = None;
            params.approvals_reviewer = None;
            params.sandbox = None;
            params.permissions = None;
            remove_permission_config_overrides(&mut params.config);
        } else if let Some(selected_profile) = selected_profile {
            params.runtime_workspace_roots = None;
            params.permissions = Some(selected_profile.profile_id.clone());
            params.sandbox = None;
            params.approval_policy = selected_profile.approval_policy;
            params.approvals_reviewer = selected_profile.approvals_reviewer.map(Into::into);
            remove_permission_config_overrides(&mut params.config);
        }
        self.thread_tool_transport()
            .configure_mcp(&mut params.config);
        let response: ThreadForkResponse = match self
            .request_handle
            .request_typed(ClientRequest::ThreadFork {
                request_id,
                params: params.clone(),
            })
            .await
        {
            Ok(response) => response,
            Err(TypedRequestError::Server { source, .. })
                if params.exclude_turns
                    && !params.attach_to_source
                    && is_history_pagination_unsupported(&source) =>
            {
                self.history_support = ThreadHistorySupport::LegacyOnly;
                params.exclude_turns = false;
                let request_id = self.next_request_id();
                self.request_handle
                    .request_typed(ClientRequest::ThreadFork { request_id, params })
                    .await
                    .map_err(|err| {
                        bootstrap_request_error("thread/fork failed during TUI bootstrap", err)
                    })?
            }
            Err(err) => {
                return Err(bootstrap_request_error(
                    "thread/fork failed during TUI bootstrap",
                    err,
                ));
            }
        };
        let mut response = response;
        if presentation == ForkPresentation::Regular
            && !response.thread.ephemeral
            && let Err(error) = self
                .hydrate_initial_thread_history(
                    &mut response.thread,
                    /*turn_cursor*/ None,
                    /*item_cursor*/ None,
                    Some(&config),
                    Some(local_settings),
                    HistoryHydrationScope::Initial,
                )
                .await
        {
            tracing::warn!(
                thread_id = %response.thread.id,
                error = %error,
                "preserving the created fork after bounded history hydration failed"
            );
        }
        // Explicit selections use the server's effective policy, including on local daemons.
        let selected_permissions = selected_profile.map(|_| {
            PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &response.sandbox.to_core(),
                response.cwd.as_path(),
            )
        });
        let mut started = started_thread_from_fork_response(
            response,
            local_settings,
            &config,
            self.thread_params_mode(),
        )
        .await?;
        if let Some(permissions) = selected_permissions {
            started.session.permission_profile = permissions;
        }
        if presentation != ForkPresentation::Regular {
            started.session.daybreak_enabled = false;
        }
        started.session.fork_parent_title = fork_parent.and_then(|thread| thread.name);
        if self.task_tools_available(thread_id) {
            started.task_tools_available = true;
            self.remember_task_tool_thread(started.session.thread_id);
        }
        Ok(started)
    }

    pub(crate) fn thread_params_mode(&self) -> ThreadParamsMode {
        self.thread_params_mode
    }

    fn session_config_with_effective_service_tier(&self, config: &Config) -> Config {
        let Some(model) = config.model.as_deref().or(self.default_model.as_deref()) else {
            return config.clone();
        };
        let local_settings = LocalSettings::from(config);
        let mut session_config = config.clone();
        match service_tier_resolution::service_tier_update_for_core(
            config,
            &local_settings.notices,
            model,
            &self.available_models,
        ) {
            Some(Some(service_tier)) => {
                session_config.service_tier = Some(service_tier);
                session_config.notices.fast_default_opt_out = None;
            }
            Some(None) => {
                session_config.service_tier = Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string());
                session_config.notices.fast_default_opt_out = None;
            }
            None => {
                session_config.service_tier = None;
                session_config.notices.fast_default_opt_out = None;
            }
        }
        session_config
    }

    async fn fork_parent_title_from_app_server(
        &mut self,
        forked_from_id: Option<&str>,
    ) -> Option<String> {
        let forked_from_id = forked_from_id?;
        let forked_from_id = match ThreadId::from_string(forked_from_id) {
            Ok(thread_id) => thread_id,
            Err(err) => {
                tracing::warn!("Failed to parse fork parent thread id from app server: {err}");
                return None;
            }
        };

        match self
            .thread_read(forked_from_id, /*include_turns*/ false)
            .await
        {
            Ok(thread) => thread.name,
            Err(err) => {
                tracing::warn!("Failed to read fork parent metadata from app server: {err}");
                None
            }
        }
    }

    /// Lists thread ids that the app server currently holds in memory.
    ///
    /// Used by `App::backfill_loaded_subagent_threads` to discover subagent threads that were
    /// spawned before the TUI connected. The caller then fetches full metadata per thread via
    /// `thread_read` and walks the spawn tree.
    pub(crate) async fn thread_loaded_list(
        &mut self,
        params: ThreadLoadedListParams,
    ) -> Result<ThreadLoadedListResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadLoadedList { request_id, params })
            .await
            .wrap_err("failed to list loaded threads from app server")
    }

    pub(crate) async fn thread_read(
        &mut self,
        thread_id: ThreadId,
        include_turns: bool,
    ) -> Result<Thread> {
        let request_id = self.next_request_id();
        let response = self
            .request_handle
            .request_typed::<ThreadReadResponse>(ClientRequest::ThreadRead {
                request_id,
                params: ThreadReadParams {
                    thread_id: thread_id.to_string(),
                    include_turns,
                },
            })
            .await;
        let mut response: ThreadReadResponse = match response {
            Ok(response) => {
                // Freeze the metadata revision observed before history hydration.
                if !include_turns {
                    self.remember_read_state(thread_id, response.read_state);
                }
                return Ok(response.thread);
            }
            Err(TypedRequestError::Server { source, .. })
                if include_turns
                    && source.message
                        == "paginated threads do not support thread/read(includeTurns=true)" =>
            {
                let request_id = self.next_request_id();
                self.request_handle
                    .request_typed(ClientRequest::ThreadRead {
                        request_id,
                        params: ThreadReadParams {
                            thread_id: thread_id.to_string(),
                            include_turns: false,
                        },
                    })
                    .await
                    .wrap_err("thread/read failed during TUI session lookup")?
            }
            Err(err) => return Err(err).wrap_err("thread/read failed during TUI session lookup"),
        };
        self.hydrate_initial_thread_history(
            &mut response.thread,
            /*turn_cursor*/ None,
            /*item_cursor*/ None,
            /*config*/ None,
            /*local_settings*/ None,
            HistoryHydrationScope::Initial,
        )
        .await?;
        Ok(response.thread)
    }

    pub(crate) async fn thread_archive(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadArchiveResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadArchive {
                request_id,
                params: ThreadArchiveParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to archive session")?;
        Ok(())
    }

    pub(crate) async fn thread_delete(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadDeleteResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadDelete {
                request_id,
                params: ThreadDeleteParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to delete session")?;
        Ok(())
    }

    pub(crate) async fn thread_unarchive(&mut self, thread_id: ThreadId) -> Result<Thread> {
        let request_id = self.next_request_id();
        let response: ThreadUnarchiveResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadUnarchive {
                request_id,
                params: ThreadUnarchiveParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("failed to unarchive session")?;
        Ok(response.thread)
    }

    pub(crate) async fn thread_metadata_update_branch(
        &mut self,
        thread_id: ThreadId,
        branch: String,
    ) -> Result<ThreadMetadataUpdateResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadMetadataUpdate {
                request_id,
                params: ThreadMetadataUpdateParams {
                    thread_id: thread_id.to_string(),
                    project_id: None,
                    daybreak_enabled: None,
                    git_info: Some(ThreadMetadataGitInfoUpdateParams {
                        sha: None,
                        branch: Some(Some(branch)),
                        origin_url: None,
                    }),
                },
            })
            .await
            .wrap_err("thread/metadata/update failed while syncing git branch")
    }

    pub(crate) async fn thread_settings_update(
        &mut self,
        params: ThreadSettingsUpdateParams,
    ) -> Result<bool> {
        if !self.thread_settings_update_supported {
            return Ok(false);
        }
        let request_id = self.next_request_id();
        match self
            .request_handle
            .request_typed::<ThreadSettingsUpdateResponse>(ClientRequest::ThreadSettingsUpdate {
                request_id,
                params,
            })
            .await
        {
            Ok(_) => Ok(true),
            Err(TypedRequestError::Server { source, .. })
                if is_thread_settings_update_unsupported(&source) =>
            {
                // Older remote app servers can reject this experimental method as
                // method-not-found, experimental-capability-gated, or an unknown
                // request variant. Treat those as a session-level capability
                // downgrade so local TUI setting changes stay best-effort instead
                // of showing an error every time the user changes model, effort, or mode.
                self.thread_settings_update_supported = false;
                Ok(false)
            }
            Err(err) => Err(err).wrap_err("thread/settings/update failed in TUI"),
        }
    }

    pub(crate) async fn thread_inject_items(
        &mut self,
        thread_id: ThreadId,
        items: Vec<ResponseItem>,
    ) -> Result<ThreadInjectItemsResponse> {
        let items = items
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .wrap_err("failed to encode thread/inject_items payload")?;
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadInjectItems {
                request_id,
                params: ThreadInjectItemsParams {
                    thread_id: thread_id.to_string(),
                    items,
                },
            })
            .await
            .wrap_err("thread/inject_items failed during TUI side conversation setup")
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn turn_start(
        &mut self,
        thread_id: ThreadId,
        turn_trigger: &str,
        client_user_message_id: String,
        items: Vec<UserInput>,
        cwd: PathBuf,
        approval_policy: Option<AskForApproval>,
        approvals_reviewer: Option<codex_app_server_protocol::ApprovalsReviewer>,
        permissions_override: TurnPermissionsOverride,
        workspace_roots: &[AbsolutePathBuf],
        model: String,
        effort: Option<codex_protocol::openai_models::ReasoningEffort>,
        summary: Option<codex_protocol::config_types::ReasoningSummary>,
        service_tier: Option<Option<String>>,
        collaboration_mode: Option<codex_protocol::config_types::CollaborationMode>,
        output_schema: Option<serde_json::Value>,
        cyber_access_program: Option<codex_app_server_protocol::CyberAccessProgram>,
    ) -> Result<TurnStartResponse> {
        let request_id = self.next_request_id();
        let mut params = TurnStartParams {
            thread_id: thread_id.to_string(),
            turn_trigger: Some(turn_trigger.to_string()),
            client_user_message_id: Some(client_user_message_id),
            input: items,
            output_schema,
            cyber_access_program,
            ..TurnStartParams::default()
        };
        if self.thread_params_mode.settings_owner() == ThreadSettingsOwner::Client {
            let (sandbox_policy, permissions) =
                turn_permissions_overrides(permissions_override, cwd.as_path())?;
            params.cwd = Some(cwd);
            params.runtime_workspace_roots = Some(workspace_roots.to_vec());
            params.approval_policy = approval_policy;
            params.approvals_reviewer = approvals_reviewer;
            params.sandbox_policy = sandbox_policy;
            params.permissions = permissions;
            params.model = Some(model);
            params.service_tier = service_tier;
            params.effort = effort;
            params.summary = summary;
            params.collaboration_mode = collaboration_mode;
        }
        self.request_handle
            .request_typed(ClientRequest::TurnStart { request_id, params })
            .await
            .wrap_err("turn/start failed in TUI")
    }

    pub(crate) async fn turn_interrupt(
        &mut self,
        thread_id: ThreadId,
        turn_id: String,
    ) -> std::result::Result<(), TypedRequestError> {
        let request_id = self.next_request_id();
        let _: TurnInterruptResponse = self
            .request_handle
            .request_typed(ClientRequest::TurnInterrupt {
                request_id,
                params: TurnInterruptParams {
                    thread_id: thread_id.to_string(),
                    turn_id,
                },
            })
            .await?;
        Ok(())
    }

    pub(crate) async fn startup_interrupt(
        &mut self,
        thread_id: ThreadId,
    ) -> std::result::Result<(), TypedRequestError> {
        self.turn_interrupt(thread_id, String::new()).await
    }

    pub(crate) async fn turn_steer(
        &mut self,
        thread_id: ThreadId,
        turn_id: String,
        client_user_message_id: String,
        items: Vec<UserInput>,
    ) -> std::result::Result<TurnSteerResponse, TypedRequestError> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::TurnSteer {
                request_id,
                params: TurnSteerParams {
                    thread_id: thread_id.to_string(),
                    client_user_message_id: Some(client_user_message_id),
                    input: items,
                    responsesapi_client_metadata: None,
                    responsesapi_client_metadata_patch: None,
                    additional_context: None,
                    expected_turn_id: turn_id,
                },
            })
            .await
    }

    pub(crate) fn automatic_thread_name_writer(
        &mut self,
        thread_id: ThreadId,
    ) -> AutomaticThreadName {
        self.automatic_name_writes
            .retain(|_, gate| Arc::strong_count(gate) > 1);
        let session = self.request_view();
        let gate = self
            .automatic_name_writes
            .entry(thread_id)
            .or_insert_with(|| Arc::new(Semaphore::new(/*permits*/ 1)));
        AutomaticThreadName {
            session,
            thread_id,
            gate: Arc::clone(gate),
        }
    }

    pub(crate) async fn thread_set_name(
        &mut self,
        thread_id: ThreadId,
        name: String,
    ) -> Result<()> {
        let gate = self.automatic_name_writes.get(&thread_id).cloned();
        let _permit = match &gate {
            Some(gate) => Some(gate.acquire().await?),
            None => None,
        };
        let request_id = self.next_request_id();
        let _: ThreadSetNameResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadSetName {
                request_id,
                params: ThreadSetNameParams {
                    thread_id: thread_id.to_string(),
                    name,
                },
            })
            .await
            .wrap_err("thread/name/set failed in TUI")?;
        if let Some(gate) = gate.as_ref() {
            gate.close();
            self.automatic_name_writes.remove(&thread_id);
        }
        Ok(())
    }

    pub(crate) async fn thread_memory_mode_set(
        &mut self,
        thread_id: ThreadId,
        mode: ThreadMemoryMode,
    ) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadMemoryModeSetResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadMemoryModeSet {
                request_id,
                params: ThreadMemoryModeSetParams {
                    thread_id: thread_id.to_string(),
                    mode,
                },
            })
            .await
            .wrap_err("thread/memoryMode/set failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn memory_reset(&mut self) -> Result<()> {
        let request_id = self.next_request_id();
        let _: MemoryResetResponse = self
            .request_handle
            .request_typed(ClientRequest::MemoryReset {
                request_id,
                params: None,
            })
            .await
            .wrap_err("memory/reset failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn thread_goal_get(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<ThreadGoalGetResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadGoalGet {
                request_id,
                params: ThreadGoalGetParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/goal/get failed in TUI")
    }

    pub(crate) async fn thread_goal_set(
        &mut self,
        thread_id: ThreadId,
        objective: Option<String>,
        status: Option<ThreadGoalStatus>,
        token_budget: Option<Option<i64>>,
    ) -> Result<ThreadGoalSetResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadGoalSet {
                request_id,
                params: ThreadGoalSetParams {
                    origin: Some(codex_app_server_protocol::ThreadGoalMutationOrigin::User),
                    thread_id: thread_id.to_string(),
                    objective,
                    status,
                    token_budget,
                },
            })
            .await
            .wrap_err("thread/goal/set failed in TUI")
    }

    pub(crate) async fn thread_goal_clear(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<ThreadGoalClearResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ThreadGoalClear {
                request_id,
                params: ThreadGoalClearParams {
                    origin: Some(codex_app_server_protocol::ThreadGoalMutationOrigin::User),
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/goal/clear failed in TUI")
    }

    pub(crate) async fn logout_account(&mut self) -> Result<()> {
        let request_id = self.next_request_id();
        let _: LogoutAccountResponse = self
            .request_handle
            .request_typed(ClientRequest::LogoutAccount {
                request_id,
                params: None,
            })
            .await
            .wrap_err("account/logout failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn thread_unsubscribe(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadUnsubscribeResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadUnsubscribe {
                request_id,
                params: ThreadUnsubscribeParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/unsubscribe failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn thread_compact_start(&mut self, thread_id: ThreadId) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadCompactStartResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadCompactStart {
                request_id,
                params: ThreadCompactStartParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/compact/start failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn thread_shell_command(
        &mut self,
        thread_id: ThreadId,
        command: String,
    ) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadShellCommandResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadShellCommand {
                request_id,
                params: ThreadShellCommandParams {
                    thread_id: thread_id.to_string(),
                    command,
                    timeout_ms: None,
                },
            })
            .await
            .wrap_err("thread/shellCommand failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn thread_approve_guardian_denied_action(
        &mut self,
        thread_id: ThreadId,
        event: &GuardianAssessmentEvent,
    ) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadApproveGuardianDeniedActionResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadApproveGuardianDeniedAction {
                request_id,
                params: ThreadApproveGuardianDeniedActionParams {
                    thread_id: thread_id.to_string(),
                    event: serde_json::to_value(event)
                        .wrap_err("failed to serialize Auto Review denial event")?,
                },
            })
            .await
            .wrap_err("thread/approveGuardianDeniedAction failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn list_background_terminals(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<Vec<ThreadBackgroundTerminal>> {
        let mut terminals = Vec::new();
        let mut cursor = None;
        loop {
            let request_id = self.next_request_id();
            let page: ThreadBackgroundTerminalsListResponse = self
                .request_handle
                .request_typed(ClientRequest::ThreadBackgroundTerminalsList {
                    request_id,
                    params: ThreadBackgroundTerminalsListParams {
                        thread_id: thread_id.to_string(),
                        cursor,
                        limit: None,
                    },
                })
                .await?;
            terminals.extend(page.data);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(terminals);
            }
        }
    }

    pub(crate) async fn thread_background_terminals_clean(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ThreadBackgroundTerminalsCleanResponse = self
            .request_handle
            .request_typed(ClientRequest::ThreadBackgroundTerminalsClean {
                request_id,
                params: ThreadBackgroundTerminalsCleanParams {
                    thread_id: thread_id.to_string(),
                },
            })
            .await
            .wrap_err("thread/backgroundTerminals/clean failed in TUI")?;
        Ok(())
    }

    pub(crate) async fn review_start(
        &mut self,
        thread_id: ThreadId,
        target: ReviewTarget,
    ) -> Result<ReviewStartResponse> {
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::ReviewStart {
                request_id,
                params: ReviewStartParams {
                    thread_id: thread_id.to_string(),
                    target,
                    delivery: Some(ReviewDelivery::Inline),
                },
            })
            .await
            .wrap_err("review/start failed in TUI")
    }

    pub(crate) async fn skills_list(
        &mut self,
        params: SkillsListParams,
    ) -> Result<SkillsListResponse> {
        self.pending_side_skills_refresh = None;
        let request_id = self.next_request_id();
        self.request_handle
            .request_typed(ClientRequest::SkillsList { request_id, params })
            .await
            .wrap_err("skills/list failed in TUI")
    }

    pub(crate) async fn reload_user_config(&mut self) -> Result<()> {
        let request_id = self.next_request_id();
        let _: ConfigWriteResponse = self
            .request_handle
            .request_typed(ClientRequest::ConfigBatchWrite {
                request_id,
                params: ConfigBatchWriteParams {
                    edits: Vec::new(),
                    file_path: None,
                    expected_version: None,
                    reload_user_config: true,
                },
            })
            .await
            .wrap_err("config/batchWrite failed while reloading user config in TUI")?;
        Ok(())
    }

    pub(crate) async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: serde_json::Value,
    ) -> std::io::Result<()> {
        let client = self.connection_owner.as_ref().ok_or_else(|| {
            std::io::Error::other("request-only session cannot resolve server requests")
        })?;
        client.resolve_server_request(request_id, result).await
    }

    pub(crate) async fn shutdown(self) -> std::io::Result<()> {
        match self.connection_owner {
            Some(client) => client.shutdown().await,
            None => Ok(()),
        }
    }

    pub(crate) fn request_handle(&self) -> AppServerRequestHandle {
        self.request_handle.clone()
    }

    pub(crate) fn next_request_id(&mut self) -> RequestId {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.request_id_namespace.map_or_else(
            || RequestId::Integer(request_id),
            |namespace| RequestId::String(format!("picker-{namespace}-{request_id}")),
        )
    }
}

pub(crate) async fn start_thread_with_request_handle(
    request_handle: AppServerRequestHandle,
    local_settings: &LocalSettings,
    config: Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<PathBuf>,
    thread_tool_transport: ThreadToolTransport,
    launch_choices: StartupLaunchChoices,
) -> Result<AppServerStartedThread> {
    let request_id = RequestId::String(format!("startup-thread-start-{}", Uuid::new_v4()));
    let mut params = thread_start_params_with_reviewer(
        &config,
        thread_params_mode,
        remote_cwd_override.as_deref(),
        /*session_start_source*/ None,
        launch_choices.approvals_reviewer,
        &launch_choices.approval_policy,
    );
    if thread_params_mode.settings_owner() == ThreadSettingsOwner::Client {
        thread_tool_transport.validate_config(&config)?;
        launch_choices.configure(&mut params);
        params.daybreak_enabled = (config.daybreak_enabled && !config.ephemeral).then_some(true);
        thread_tool_transport.configure(&mut params);
    }
    let (response, _history_support, task_tools_available) =
        request_thread_start_with_history_fallback(&request_handle, request_id, params)
            .await
            .map_err(|err| {
                bootstrap_request_error("thread/start failed during TUI bootstrap", err)
            })?;
    let mut started =
        started_thread_from_start_response(response, local_settings, &config, thread_params_mode)
            .await?;
    started.task_tools_available = task_tools_available;
    Ok(started)
}

pub(crate) fn workspace_display(
    name: Option<String>,
    routing: Option<codex_app_server_protocol::WorkspaceRouting>,
) -> Option<String> {
    let name = name
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control));
    let id = routing.map(|routing| routing.chatgpt_account_id);
    match (name, id) {
        (Some(name), Some(id)) => Some(format!("{name} ({id})")),
        (name, id) => name.or(id),
    }
}

pub(crate) fn status_account_display_from_auth_mode(
    auth_mode: Option<AuthMode>,
    plan_type: Option<codex_protocol::account::PlanType>,
) -> Option<StatusAccountDisplay> {
    match auth_mode {
        Some(AuthMode::ApiKey) => Some(StatusAccountDisplay::ApiKey),
        Some(AuthMode::Chatgpt)
        | Some(AuthMode::ChatgptAuthTokens)
        | Some(AuthMode::AgentIdentity)
        | Some(AuthMode::PersonalAccessToken) => Some(StatusAccountDisplay::ChatGpt {
            email: None,
            plan: plan_type.map(plan_type_display_name),
            workspace: None,
        }),
        Some(AuthMode::Headers)
        | Some(AuthMode::BedrockApiKey)
        | Some(AuthMode::BedrockAccessKeys) => None,
        None => None,
    }
}

pub(crate) fn model_preset_from_api_model(model: ApiModel) -> ModelPreset {
    let upgrade = model.upgrade.map(|upgrade_id| {
        let upgrade_info = model.upgrade_info.clone();
        ModelUpgrade {
            id: upgrade_id,
            migration_config_key: model.model.clone(),
            model_link: upgrade_info
                .as_ref()
                .and_then(|info| info.model_link.clone()),
            upgrade_copy: upgrade_info
                .as_ref()
                .and_then(|info| info.upgrade_copy.clone()),
            migration_markdown: upgrade_info.and_then(|info| info.migration_markdown),
            retirement_at: model
                .upgrade_info
                .as_ref()
                .and_then(|info| info.retirement_at)
                .and_then(|retirement_at| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp(retirement_at, 0)
                }),
        }
    });

    ModelPreset {
        id: model.id,
        model: model.model,
        display_name: model.display_name,
        description: model.description,
        model_specialty: model.model_specialty,
        default_reasoning_effort: model.default_reasoning_effort,
        supported_reasoning_efforts: model
            .supported_reasoning_efforts
            .into_iter()
            .map(|effort| ReasoningEffortPreset {
                effort: effort.reasoning_effort,
                description: effort.description,
            })
            .collect(),
        supports_personality: false,
        additional_speed_tiers: model.additional_speed_tiers,
        service_tiers: model
            .service_tiers
            .into_iter()
            .map(|service_tier| ModelServiceTier {
                id: service_tier.id,
                name: service_tier.name,
                description: service_tier.description,
            })
            .collect(),
        default_service_tier: model.default_service_tier,
        available_access_programs: model.available_access_programs.map(Into::into),
        is_default: model.is_default,
        upgrade,
        show_in_picker: !model.hidden,
        multi_agent_version: None,
        availability_nux: model.availability_nux.map(|nux| ModelAvailabilityNux {
            message: nux.message,
        }),
        // `model/list` already returns models filtered for the active client/auth context.
        supported_in_api: true,
        input_modalities: model.input_modalities,
    }
}

fn approvals_reviewer_override_from_config(
    config: &Config,
) -> Option<codex_app_server_protocol::ApprovalsReviewer> {
    Some(config.approvals_reviewer.into())
}

fn config_request_overrides_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Option<HashMap<String, serde_json::Value>> {
    let mut session_config = toml::Value::Table(toml::Table::new());
    for layer in config.config_layer_stack.layers_low_to_high() {
        if matches!(&layer.name, ConfigLayerSource::SessionFlags) {
            codex_config::merge_toml_values(&mut session_config, &layer.config);
        }
    }
    let mut overrides: HashMap<_, _> = session_config
        .as_table()
        .into_iter()
        .flatten()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "allow_login_shell"
                    | "default_permissions"
                    | "features"
                    | "network"
                    | "permissions"
                    | "personality"
                    | "sandbox_workspace_write"
                    | "shell_environment_policy"
                    | "suppress_unstable_features_warning"
            )
        })
        .filter_map(|(key, value)| {
            serde_json::to_value(value)
                .ok()
                .map(|value| (key.clone(), value))
        })
        .collect();
    let mut insert = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            overrides.insert(key.to_string(), serde_json::Value::String(value));
        }
    };
    insert(
        "model_reasoning_effort",
        config
            .model_reasoning_effort
            .as_ref()
            .map(std::string::ToString::to_string),
    );
    // Only winning launch choices may replace server defaults or saved thread settings.
    let origins = config.config_layer_stack.origins();
    let effective = config.config_layer_stack.effective_config();
    web_search::apply_launch_override(
        config,
        thread_params_mode,
        &effective,
        &origins,
        &mut overrides,
    );
    let is_launch = |key: &str| {
        origins.get(key).is_some_and(|origin| {
            matches!(
                origin.name,
                ConfigLayerSource::SessionFlags
                    | ConfigLayerSource::User {
                        profile: Some(_),
                        ..
                    }
            )
        })
    };
    for key in ["model_reasoning_summary", "model_verbosity"] {
        if is_launch(key) {
            overrides.insert(key.to_string(), serde_json::json!(effective[key]));
        }
    }
    if is_launch("features.concurrent_reasoning_summaries") {
        overrides
            .entry("features".to_string())
            .or_insert_with(|| serde_json::json!({}))["concurrent_reasoning_summaries"] =
            serde_json::json!(effective["features"]["concurrent_reasoning_summaries"]);
    }
    if config.bypass_hook_trust {
        overrides.insert("bypass_hook_trust".to_string(), true.into());
    }
    Some(overrides)
}

fn remove_permission_config_overrides(config: &mut Option<HashMap<String, serde_json::Value>>) {
    if let Some(overrides) = config.as_mut() {
        for key in [
            "default_permissions",
            "permissions",
            "network",
            "sandbox_workspace_write",
        ] {
            overrides.remove(key);
        }
    }
    if config.as_ref().is_some_and(HashMap::is_empty) {
        *config = None;
    }
}

fn service_tier_override_from_config(config: &Config) -> Option<Option<String>> {
    let local_settings = LocalSettings::from(config);
    config.service_tier.clone().map(Some).or_else(|| {
        (local_settings.notices.fast_default_opt_out == Some(true))
            .then(|| Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string()))
    })
}

fn sandbox_mode_from_permission_profile(
    permission_profile: &PermissionProfile,
    cwd: &std::path::Path,
) -> Option<codex_app_server_protocol::SandboxMode> {
    match permission_profile {
        PermissionProfile::Disabled => {
            Some(codex_app_server_protocol::SandboxMode::DangerFullAccess)
        }
        PermissionProfile::External { .. } => None,
        PermissionProfile::Managed { .. } => {
            let file_system_policy = permission_profile.file_system_sandbox_policy();
            if file_system_policy.has_full_disk_write_access() {
                permission_profile
                    .network_sandbox_policy()
                    .is_enabled()
                    .then_some(codex_app_server_protocol::SandboxMode::DangerFullAccess)
            } else if file_system_policy.can_write_local_path_with_cwd(cwd, cwd) {
                Some(codex_app_server_protocol::SandboxMode::WorkspaceWrite)
            } else {
                Some(codex_app_server_protocol::SandboxMode::ReadOnly)
            }
        }
    }
}

pub(crate) fn permission_profile_is_safely_represented_by_sandbox_mode(
    permission_profile: &PermissionProfile,
    cwd: &std::path::Path,
) -> bool {
    let Some(sandbox_mode) = sandbox_mode_from_permission_profile(permission_profile, cwd) else {
        return false;
    };
    let projected_profile = match sandbox_mode {
        codex_app_server_protocol::SandboxMode::ReadOnly => PermissionProfile::read_only(),
        codex_app_server_protocol::SandboxMode::WorkspaceWrite => {
            PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &codex_protocol::protocol::SandboxPolicy::new_workspace_write_policy(),
                cwd,
            )
        }
        codex_app_server_protocol::SandboxMode::DangerFullAccess => PermissionProfile::Disabled,
    };
    permission_profile.network_sandbox_policy() == projected_profile.network_sandbox_policy()
        && permission_profile
            .file_system_sandbox_policy()
            .is_semantically_equivalent_to(&projected_profile.file_system_sandbox_policy(), cwd)
}

fn permission_profile_id_from_active_profile(active: ActivePermissionProfile) -> String {
    active.id
}

pub(crate) fn turn_permissions_overrides(
    permissions_override: TurnPermissionsOverride,
    cwd: &std::path::Path,
) -> Result<(
    Option<codex_app_server_protocol::SandboxPolicy>,
    Option<String>,
)> {
    Ok(match permissions_override {
        TurnPermissionsOverride::Preserve => (None, None),
        TurnPermissionsOverride::ActiveProfile(active_permission_profile) => (
            None,
            Some(permission_profile_id_from_active_profile(
                active_permission_profile,
            )),
        ),
        TurnPermissionsOverride::LegacySandbox(permission_profile) => {
            let policy = permission_profile
                .to_legacy_sandbox_policy(cwd)
                .map_err(|_| UnsupportedLegacyPermissionProfile)?;
            let projected_profile =
                PermissionProfile::from_legacy_sandbox_policy_for_cwd(&policy, cwd);
            if !permission_profile
                .file_system_sandbox_policy()
                .is_semantically_equivalent_to(&projected_profile.file_system_sandbox_policy(), cwd)
            {
                return Err(UnsupportedLegacyPermissionProfile.into());
            }
            (Some(policy.into()), None)
        }
    })
}

fn permissions_selection_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Option<String> {
    if matches!(thread_params_mode, ThreadParamsMode::Remote) {
        return None;
    }

    config
        .permissions
        .active_permission_profile()
        .map(permission_profile_id_from_active_profile)
}

pub(crate) fn thread_start_params_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    session_start_source: Option<ThreadStartSource>,
) -> ThreadStartParams {
    thread_start_params_with_reviewer(
        config,
        thread_params_mode,
        remote_cwd_override,
        session_start_source,
        startup_launch::StartupApprovalsReviewer::Config,
        &startup_launch::StartupApprovalPolicy::ResolvedConfig,
    )
}

fn thread_start_params_with_reviewer(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    session_start_source: Option<ThreadStartSource>,
    reviewer_source: startup_launch::StartupApprovalsReviewer,
    policy_source: &startup_launch::StartupApprovalPolicy,
) -> ThreadStartParams {
    if thread_params_mode.settings_owner() == ThreadSettingsOwner::Server {
        return ThreadStartParams::default();
    }
    let permissions = permissions_selection_from_config(config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    ThreadStartParams {
        model: config.model.clone(),
        model_provider: provider_selection::explicit_provider(config),
        service_tier: service_tier_override_from_config(config),
        cwd: thread_cwd_from_config(config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: thread_params_mode.workspace_roots_from_config(config),
        approval_policy: match policy_source {
            startup_launch::StartupApprovalPolicy::ResolvedConfig => {
                Some(config.permissions.approval_policy.value().into())
            }
            startup_launch::StartupApprovalPolicy::Invocation(policy) => *policy,
            startup_launch::StartupApprovalPolicy::ConfigOverride(_) => None,
        },
        approvals_reviewer: match reviewer_source {
            startup_launch::StartupApprovalsReviewer::Config => {
                approvals_reviewer_override_from_config(config)
            }
            startup_launch::StartupApprovalsReviewer::Invocation(reviewer) => reviewer,
        },
        sandbox,
        permissions,
        config: config_request_overrides_from_config(config, thread_params_mode),
        ephemeral: Some(config.ephemeral),
        history_mode: (!config.ephemeral).then_some(ThreadHistoryMode::Paginated),
        session_start_source,
        thread_source: Some(ThreadSource::User),
        developer_instructions: with_terminal_visualization_instructions(
            config, /*control_instructions*/ None,
        ),
        ..ThreadStartParams::default()
    }
}

fn thread_resume_params_from_config(
    config: Config,
    thread_id: ThreadId,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
    model_settings: ResumeModelSettings,
    permission_overrides: crate::resume_permissions::ResumePermissions,
) -> ThreadResumeParams {
    if thread_params_mode.settings_owner() == ThreadSettingsOwner::Server
        || matches!(
            model_settings,
            ResumeModelSettings::PreserveExistingThread
                | ResumeModelSettings::PreserveSideConversation
        )
    {
        return ThreadResumeParams {
            thread_id: thread_id.to_string(),
            developer_instructions: if model_settings
                == ResumeModelSettings::PreserveSideConversation
            {
                with_terminal_visualization_instructions(
                    &config,
                    config.developer_instructions.clone(),
                )
            } else {
                None
            },
            ..ThreadResumeParams::default()
        };
    }
    let permissions = permissions_selection_from_config(&config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    let mut config_overrides = config_request_overrides_from_config(&config, thread_params_mode);
    if model_settings == ResumeModelSettings::RestoreFromThread
        && let Some(overrides) = config_overrides.as_mut()
    {
        overrides.remove("model_reasoning_effort");
        if overrides.is_empty() {
            config_overrides = None;
        }
    }
    let (model, model_provider) = match model_settings {
        ResumeModelSettings::OverrideFromCurrentConfig => (
            config.model.clone(),
            provider_selection::explicit_provider(&config),
        ),
        ResumeModelSettings::RestoreFromThread
        | ResumeModelSettings::PreserveExistingThread
        | ResumeModelSettings::PreserveSideConversation => (None, None),
    };
    let mut params = ThreadResumeParams {
        thread_id: thread_id.to_string(),
        model,
        model_provider,
        service_tier: service_tier_override_from_config(&config),
        cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: thread_params_mode.workspace_roots_from_config(&config),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        approvals_reviewer: approvals_reviewer_override_from_config(&config),
        sandbox,
        permissions,
        config: config_overrides,
        developer_instructions: with_terminal_visualization_instructions(
            &config, /*control_instructions*/ None,
        ),
        ..ThreadResumeParams::default()
    };
    if thread_params_mode == ThreadParamsMode::Remote || !permission_overrides.approval_policy {
        params.approval_policy = None;
    }
    if thread_params_mode == ThreadParamsMode::Remote || !permission_overrides.approvals_reviewer {
        params.approvals_reviewer = None;
    }
    if thread_params_mode == ThreadParamsMode::Remote || !permission_overrides.profile {
        params.sandbox = None;
        params.permissions = None;
        remove_permission_config_overrides(&mut params.config);
    }
    if !permission_overrides.workspace_roots {
        params.runtime_workspace_roots = None;
    }
    params
}

fn thread_fork_params_from_config(
    config: Config,
    thread_id: ThreadId,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
) -> ThreadForkParams {
    let permissions = permissions_selection_from_config(&config, thread_params_mode);
    let sandbox = permissions
        .is_none()
        .then(|| {
            sandbox_mode_from_permission_profile(
                &config.permissions.effective_permission_profile(),
                config.cwd.as_path(),
            )
        })
        .flatten();
    ThreadForkParams {
        thread_id: thread_id.to_string(),
        model: config.model.clone(),
        model_provider: provider_selection::explicit_provider(&config),
        service_tier: service_tier_override_from_config(&config),
        cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
        runtime_workspace_roots: thread_params_mode.workspace_roots_from_config(&config),
        approval_policy: Some(config.permissions.approval_policy.value().into()),
        approvals_reviewer: approvals_reviewer_override_from_config(&config),
        sandbox,
        permissions,
        config: config_request_overrides_from_config(&config, thread_params_mode),
        base_instructions: config.base_instructions.clone().filter(|_| {
            !matches!(
                config.base_instructions_provenance,
                Some(BaseInstructionsProvenance::Model { .. })
            )
        }),
        developer_instructions: with_terminal_visualization_instructions(
            &config,
            config.developer_instructions.clone(),
        ),
        ephemeral: config.ephemeral,
        thread_source: Some(ThreadSource::User),
        ..ThreadForkParams::default()
    }
}

fn thread_cwd_from_config(
    config: &Config,
    thread_params_mode: ThreadParamsMode,
    remote_cwd_override: Option<&std::path::Path>,
) -> Option<String> {
    match thread_params_mode {
        ThreadParamsMode::Embedded => Some(config.cwd.to_string_lossy().to_string()),
        ThreadParamsMode::Remote | ThreadParamsMode::Cloud => {
            remote_cwd_override.map(|cwd| cwd.to_string_lossy().to_string())
        }
    }
}

fn started_thread(session: ThreadSessionState, thread: Thread) -> AppServerStartedThread {
    AppServerStartedThread {
        blocks_direct_input: thread_blocks_direct_input(&thread),
        session,
        turns: thread.turns,
        task_tools_available: false,
    }
}

async fn started_thread_from_start_response(
    response: ThreadStartResponse,
    local_settings: &LocalSettings,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<AppServerStartedThread> {
    let permission_profile = display_permission_profile_from_thread_response(
        &response.sandbox,
        response.cwd.as_path(),
        config,
        thread_params_mode,
    );
    let mut session = thread_session_state_from_thread_response(
        &response.thread.id,
        crate::windows_sandbox::host_from_environments(response.thread.environments.as_deref()),
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        response.approvals_reviewer.to_core(),
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        response.cwd.clone(),
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        local_settings,
    )
    .await
    .map_err(color_eyre::eyre::Report::msg)?;
    session.daybreak_enabled = response
        .thread
        .daybreak_enabled
        .unwrap_or(response.thread.ephemeral && config.daybreak_enabled);
    Ok(started_thread(session, response.thread))
}

async fn started_thread_from_resume_response(
    response: ThreadResumeResponse,
    local_settings: &LocalSettings,
    _thread_params_mode: ThreadParamsMode,
) -> Result<AppServerStartedThread> {
    // The server owns restored rules. This legacy projection is for display only;
    // normal turns do not send it back as an execution override.
    let permission_profile = PermissionProfile::from_legacy_sandbox_policy_for_cwd(
        &response.sandbox.to_core(),
        response.cwd.as_path(),
    );
    let mut session = thread_session_state_from_thread_response(
        &response.thread.id,
        crate::windows_sandbox::host_from_environments(response.thread.environments.as_deref()),
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        response.approvals_reviewer.to_core(),
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        response.cwd.clone(),
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        local_settings,
    )
    .await
    .map_err(color_eyre::eyre::Report::msg)?;
    session.collaboration_mode = response.collaboration_mode.clone().map(Box::new);
    session.daybreak_enabled = response.thread.daybreak_enabled.unwrap_or(false);
    Ok(started_thread(session, response.thread))
}

async fn started_thread_from_fork_response(
    response: ThreadForkResponse,
    local_settings: &LocalSettings,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> Result<AppServerStartedThread> {
    let permission_profile = display_permission_profile_from_thread_response(
        &response.sandbox,
        response.cwd.as_path(),
        config,
        thread_params_mode,
    );
    let mut session = thread_session_state_from_thread_response(
        &response.thread.id,
        crate::windows_sandbox::host_from_environments(response.thread.environments.as_deref()),
        response.thread.forked_from_id.clone(),
        response.thread.name.clone(),
        response.thread.path.clone(),
        response.model.clone(),
        response.model_provider.clone(),
        response.service_tier.clone(),
        response.approval_policy,
        response.approvals_reviewer.to_core(),
        permission_profile,
        response.active_permission_profile.clone().map(Into::into),
        response.cwd.clone(),
        response.runtime_workspace_roots.clone(),
        response.instruction_source_path_uris(),
        response.reasoning_effort.clone(),
        local_settings,
    )
    .await
    .map_err(color_eyre::eyre::Report::msg)?;
    session.daybreak_enabled = response.thread.daybreak_enabled.unwrap_or(false);
    Ok(started_thread(session, response.thread))
}

fn display_permission_profile_from_thread_response(
    sandbox: &codex_app_server_protocol::SandboxPolicy,
    cwd: &std::path::Path,
    config: &Config,
    thread_params_mode: ThreadParamsMode,
) -> PermissionProfile {
    match thread_params_mode {
        ThreadParamsMode::Embedded
            if matches!(
                config.permissions.effective_permission_profile(),
                PermissionProfile::Disabled
            ) && !matches!(
                sandbox,
                codex_app_server_protocol::SandboxPolicy::DangerFullAccess
            ) =>
        {
            PermissionProfile::from_legacy_sandbox_policy_for_cwd(&sandbox.to_core(), cwd)
        }
        ThreadParamsMode::Embedded => config.permissions.effective_permission_profile(),
        ThreadParamsMode::Remote | ThreadParamsMode::Cloud => {
            PermissionProfile::from_legacy_sandbox_policy_for_cwd(&sandbox.to_core(), cwd)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "session mapping keeps explicit fields"
)]
async fn thread_session_state_from_thread_response(
    thread_id: &str,
    windows_sandbox_host: crate::app::WindowsSandboxHost,
    forked_from_id: Option<String>,
    thread_name: Option<String>,
    rollout_path: Option<PathBuf>,
    model: String,
    model_provider_id: String,
    service_tier: Option<String>,
    approval_policy: AskForApproval,
    approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer,
    permission_profile: PermissionProfile,
    active_permission_profile: Option<ActivePermissionProfile>,
    cwd: AbsolutePathBuf,
    runtime_workspace_roots: Vec<AbsolutePathBuf>,
    instruction_source_paths: Vec<PathUri>,
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    local_settings: &LocalSettings,
) -> Result<ThreadSessionState, String> {
    let thread_id = ThreadId::from_string(thread_id)
        .map_err(|err| format!("thread id `{thread_id}` is invalid: {err}"))?;
    let forked_from_id = forked_from_id
        .as_deref()
        .map(ThreadId::from_string)
        .transpose()
        .map_err(|err| format!("forked_from_id is invalid: {err}"))?;
    let history_config = codex_message_history::HistoryConfig::new(
        local_settings.codex_home.clone(),
        &local_settings.history,
    );
    let (log_id, entry_count) = codex_message_history::history_metadata(&history_config).await;
    Ok(ThreadSessionState {
        daybreak_enabled: false,
        windows_sandbox_host,
        thread_id,
        forked_from_id,
        fork_parent_title: None,
        thread_name,
        model,
        model_provider_id,
        service_tier,
        approval_policy,
        approvals_reviewer,
        permission_profile,
        active_permission_profile,
        cwd,
        runtime_workspace_roots,
        instruction_source_paths,
        reasoning_effort,
        collaboration_mode: None,
        message_history: Some(MessageHistoryMetadata {
            log_id,
            entry_count,
        }),
        network_proxy: None,
        rollout_path,
    })
}

pub(crate) fn app_server_rate_limit_snapshots(
    response: GetAccountRateLimitsResponse,
) -> Vec<RateLimitSnapshot> {
    let primary_limit_id = response.rate_limits.limit_id.clone();
    let mut snapshots = vec![response.rate_limits];
    if let Some(by_limit_id) = response.rate_limits_by_limit_id {
        snapshots.extend(by_limit_id.into_iter().filter_map(|(limit_id, snapshot)| {
            if primary_limit_id.as_deref().is_some_and(|primary_limit_id| {
                primary_limit_id == limit_id
                    || Some(primary_limit_id) == snapshot.limit_id.as_deref()
            }) {
                None
            } else {
                Some(snapshot)
            }
        }));
    }
    snapshots
}

#[cfg(test)]
#[path = "app_server_session/reasoning_defaults_tests.rs"]
mod reasoning_defaults_tests;

#[cfg(test)]
#[path = "app_server_session/workspace_roots_tests.rs"]
mod workspace_roots_tests;

#[cfg(test)]
#[path = "app_server_session/prompt_history_tests.rs"]
mod prompt_history_tests;

#[cfg(test)]
#[path = "app_server_session_tests.rs"]
mod tests;
