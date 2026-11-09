#[path = "daemon_continuation.rs"]
mod daemon_continuation;

#[path = "daemon_snapshot.rs"]
mod daemon_snapshot;

mod workspace_switching;

#[path = "thread_resume_account.rs"]
mod thread_resume_account;

mod pagination;
mod runtime_lifecycle;
mod store_conversion;
mod thread_fork;
mod thread_listing;
mod thread_mutations;
mod thread_read;
mod thread_resume;
mod thread_start;

use super::persisted_resume_settings::PersistedResumeSettings;
use super::persisted_resume_settings::latest_owned_thread_settings;
use super::persisted_resume_settings::latest_persisted_model_settings;
use super::thread_enrichment::enrich_loaded_threads;
use super::thread_fork_goal::inherit_thread_goal_snapshot;
use super::thread_input::can_accept_direct_input;
use super::thread_input::ensure_direct_input_allowed;
use super::thread_lifecycle::thread_closing_for_resume;
use super::*;
use crate::connection_rpc_gate::ConnectionRpcGate;
use crate::error_code::method_not_found;
use crate::thread_sample::SampleTask;
use crate::thread_sample::SampleThread;
use crate::thread_sample::generate_sample;
use codex_app_server_protocol::SelectedCapabilityRoot;
use codex_app_server_protocol::ThreadHistoryMode as ApiThreadHistoryMode;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::ThreadItemsListAnchor;
use codex_app_server_protocol::ThreadItemsListCursor;
use codex_app_server_protocol::ThreadRevertParams;
use codex_app_server_protocol::ThreadRevertResponse;
use codex_app_server_protocol::ThreadRevertedNotification;
use codex_app_server_protocol::ThreadSampleParams;
use codex_app_server_protocol::ThreadSampleResponse;
use codex_app_server_protocol::ThreadSection;
use codex_app_server_protocol::ThreadSectionAppearance;
use codex_app_server_protocol::ThreadSectionChangedNotification;
use codex_app_server_protocol::ThreadSectionMoveParams;
use codex_app_server_protocol::ThreadSectionMoveResponse;
use codex_config::types::WindowsSandboxModeToml;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::ThreadIdleCause;
use codex_protocol::SanitizedGitUrl;
use codex_protocol::config_types::MultiAgentMode;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_thread_store::LocalHistoryAccount;
use codex_thread_store::PersistContext;
use std::ops::ControlFlow;

pub(super) use pagination::apply_thread_turns_items_view;
pub(super) use pagination::build_thread_resume_initial_turns_page;
use pagination::build_thread_turns_page_response;
pub(super) use pagination::normalize_thread_turns_status;
use pagination::paginate_background_terminals;
#[cfg(test)]
use pagination::reconstruct_thread_turns_for_turns_list;
use pagination::thread_backwards_cursor_for_sort_key;
pub(super) use pagination::thread_turns_page_size;
use store_conversion::build_thread_from_loaded_snapshot;
use store_conversion::build_thread_from_snapshot;
use store_conversion::conversation_summary_rollout_path_read_error;
use store_conversion::conversation_summary_thread_id_read_error;
pub(super) use store_conversion::core_thread_write_error;
use store_conversion::paginated_history_list_error;
use store_conversion::preview_from_rollout_items;
use store_conversion::set_thread_name_from_title;
use store_conversion::stored_thread_item_to_api_item;
use store_conversion::stored_thread_item_to_entry;
use store_conversion::stored_turn_to_api_turn;
#[cfg(test)]
use store_conversion::summary_from_state_db_metadata;
use store_conversion::summary_from_stored_thread;
#[cfg(test)]
use store_conversion::summary_from_thread_metadata;
use store_conversion::thread_from_stored_thread;
use store_conversion::thread_read_history_load_error;
pub(super) use store_conversion::thread_read_view_error;
use store_conversion::thread_store_list_error;
use store_conversion::thread_store_resume_read_error;
use store_conversion::thread_turns_list_history_load_error;
pub(super) use store_conversion::unsupported_thread_store_operation;

pub(super) const THREAD_LIST_DEFAULT_LIMIT: usize = 25;
pub(super) const THREAD_LIST_MAX_LIMIT: usize = 100;
const THREAD_LIST_MAX_EXCLUDED_IDS: usize = 100;
const PAGINATED_FULL_HISTORY_DEPRECATION_SUMMARY: &str = "Full-history hydration is deprecated for paginated threads; use `excludeTurns: true`, then page with `thread/turns/list` and `thread/items/list`.";
const PAGINATED_THREAD_READ_DEPRECATION_SUMMARY: &str = "Full-history hydration is deprecated for paginated threads; omit `includeTurns` or set it to `false`, then page with `thread/turns/list` and `thread/items/list`.";

async fn stage_pending_thread_metadata(
    thread_manager: &ThreadManager,
    thread_store: &dyn ThreadStore,
    patch: StoreThreadMetadataPatch,
    operation: &'static str,
) -> Result<Option<ThreadId>, JSONRPCErrorError> {
    if patch.is_empty() {
        return Ok(None);
    }
    let thread_id = thread_manager.reserve_thread_id();
    thread_store
        .stage_pending_thread_metadata(thread_id, patch)
        .await
        .map_err(|error| match error {
            ThreadStoreError::Unsupported { .. } => {
                method_not_found(format!("{operation} is unavailable without sqlite state"))
            }
            ThreadStoreError::InvalidRequest { message } => invalid_request(message),
            error => internal_error(format!("failed to stage {operation} metadata: {error}")),
        })?;
    Ok(Some(thread_id))
}

async fn remove_pending_thread_metadata(
    thread_store: &dyn ThreadStore,
    thread_id: Option<ThreadId>,
) {
    let Some(thread_id) = thread_id else {
        return;
    };
    if let Err(error) = thread_store.remove_pending_thread_metadata(thread_id).await {
        warn!("failed to remove staged thread metadata for {thread_id}: {error}");
    }
}

struct ThreadListFilters {
    account: Option<LocalHistoryAccount>,
    model_providers: Option<Vec<String>>,
    source_kinds: Option<Vec<ThreadSourceKind>>,
    archived: bool,
    section_id: Option<Option<String>>,
    project_id: StoreClearableField<String>,
    cwd_filters: Option<Vec<PathBuf>>,
    search_term: Option<String>,
    use_state_db_only: bool,
    relation_filter: Option<StoreThreadRelationFilter>,
    excluded_thread_ids: HashSet<ThreadId>,
}

// Persisted inputs that can change while loading configuration without the metadata permit.
#[derive(PartialEq)]
struct ResumeConfigState {
    history_cwd: Option<PathBuf>,
    workspace_roots: Option<Vec<AbsolutePathBuf>>,
    persisted_metadata: Option<ThreadMetadata>,
    persisted_settings: Option<PersistedResumeSettings>,
}

struct PreparedResumeConfig {
    state: ResumeConfigState,
    config: Config,
    model_context: Option<ResumedHistory>,
}

struct ThreadRevertRuntimeSnapshot {
    config: Config,
    settings: CodexThreadSettingsOverrides,
    client_mcp_extensions: ClientMcpExtensions,
}

fn collect_resume_override_mismatches(
    request: &ThreadResumeParams,
    config_snapshot: &ThreadConfigSnapshot,
    selected_service_tier: Option<&str>,
) -> Vec<String> {
    let mut mismatch_details = Vec::new();

    if let Some(requested_model) = request.model.as_deref()
        && requested_model != config_snapshot.model
    {
        mismatch_details.push(format!(
            "model requested={requested_model} active={}",
            config_snapshot.model
        ));
    }
    if let Some(requested_provider) = request.model_provider.as_deref()
        && requested_provider != config_snapshot.model_provider_id
    {
        mismatch_details.push(format!(
            "model_provider requested={requested_provider} active={}",
            config_snapshot.model_provider_id
        ));
    }
    if let Some(requested_service_tier) = request.service_tier.as_ref()
        && Some(requested_service_tier.as_deref().unwrap_or("default")) != selected_service_tier
    {
        mismatch_details.push(format!(
            "service_tier requested={requested_service_tier:?} active={:?}",
            config_snapshot.service_tier
        ));
    }
    if let Some(requested_cwd) = request.cwd.as_deref() {
        let requested_cwd_path = std::path::PathBuf::from(requested_cwd);
        if requested_cwd_path != config_snapshot.cwd().as_path() {
            mismatch_details.push(format!(
                "cwd requested={} active={}",
                requested_cwd_path.display(),
                config_snapshot.cwd().display()
            ));
        }
    }
    if let Some(requested_runtime_workspace_roots) = request.runtime_workspace_roots.as_ref() {
        let requested_runtime_workspace_roots = requested_runtime_workspace_roots.to_vec();
        if requested_runtime_workspace_roots != config_snapshot.workspace_roots {
            mismatch_details.push(format!(
                "runtime_workspace_roots requested={requested_runtime_workspace_roots:?} active={:?}",
                config_snapshot.workspace_roots
            ));
        }
    }
    if let Some(requested_approval) = request.approval_policy.as_ref() {
        let active_approval: AskForApproval = config_snapshot.approval_policy.into();
        if requested_approval != &active_approval {
            mismatch_details.push(format!(
                "approval_policy requested={requested_approval:?} active={active_approval:?}"
            ));
        }
    }
    if let Some(requested_review_policy) = request.approvals_reviewer.as_ref() {
        let active_review_policy: codex_app_server_protocol::ApprovalsReviewer =
            config_snapshot.approvals_reviewer.into();
        if requested_review_policy != &active_review_policy {
            mismatch_details.push(format!(
                "approvals_reviewer requested={requested_review_policy:?} active={active_review_policy:?}"
            ));
        }
    }
    if let Some(requested_sandbox) = request.sandbox.as_ref() {
        let active_sandbox = config_snapshot.sandbox_policy();
        let sandbox_matches = matches!(
            (requested_sandbox, &active_sandbox),
            (
                SandboxMode::ReadOnly,
                codex_protocol::protocol::SandboxPolicy::ReadOnly { .. }
            ) | (
                SandboxMode::WorkspaceWrite,
                codex_protocol::protocol::SandboxPolicy::WorkspaceWrite { .. }
            ) | (
                SandboxMode::DangerFullAccess,
                codex_protocol::protocol::SandboxPolicy::DangerFullAccess
            ) | (
                SandboxMode::DangerFullAccess,
                codex_protocol::protocol::SandboxPolicy::ExternalSandbox { .. }
            )
        );
        if !sandbox_matches {
            mismatch_details.push(format!(
                "sandbox requested={requested_sandbox:?} active={active_sandbox:?}"
            ));
        }
    }
    if request.permissions.is_some() {
        mismatch_details.push(format!(
            "permissions override was provided and ignored while running; active={:?}",
            config_snapshot.active_permission_profile
        ));
    }
    if let Some(requested_personality) = request.personality.as_ref()
        && config_snapshot.personality.as_ref() != Some(requested_personality)
    {
        mismatch_details.push(format!(
            "personality requested={requested_personality:?} active={:?}",
            config_snapshot.personality
        ));
    }

    if request.config.is_some() {
        mismatch_details
            .push("config overrides were provided and ignored while running".to_string());
    }
    if request.base_instructions.is_some() {
        mismatch_details
            .push("baseInstructions override was provided and ignored while running".to_string());
    }
    if request.developer_instructions.is_some() {
        mismatch_details.push(
            "developerInstructions override was provided and ignored while running".to_string(),
        );
    }
    mismatch_details
}

fn merge_persisted_resume_metadata(
    request_overrides: &mut Option<HashMap<String, serde_json::Value>>,
    typesafe_overrides: &mut ConfigOverrides,
    persisted_metadata: &ThreadMetadata,
) {
    let has_model_override =
        has_model_resume_override(request_overrides.as_ref(), typesafe_overrides);

    // Model and reasoning overrides do not imply a provider change.
    if typesafe_overrides.model_provider.is_none()
        && !request_overrides
            .as_ref()
            .is_some_and(|overrides| overrides.contains_key("model_provider"))
    {
        typesafe_overrides.model_provider = Some(persisted_metadata.model_provider.clone());
    }

    if has_model_override {
        return;
    }

    typesafe_overrides.model = persisted_metadata.model.clone();

    if let Some(reasoning_effort) = persisted_metadata.reasoning_effort.as_ref() {
        request_overrides.get_or_insert_with(HashMap::new).insert(
            "model_reasoning_effort".to_string(),
            serde_json::Value::String(reasoning_effort.to_string()),
        );
    }
}

fn normalize_thread_list_cwd_filters(
    cwd: Option<ThreadListCwdFilter>,
) -> Result<Option<Vec<PathBuf>>, JSONRPCErrorError> {
    let Some(cwd) = cwd else {
        return Ok(None);
    };

    let cwds = match cwd {
        ThreadListCwdFilter::One(cwd) => vec![cwd],
        ThreadListCwdFilter::Many(cwds) => cwds,
    };
    let mut normalized_cwds = Vec::with_capacity(cwds.len());
    for cwd in cwds {
        let cwd = AbsolutePathBuf::relative_to_current_dir(cwd.as_str())
            .map(AbsolutePathBuf::into_path_buf)
            .map_err(|err| {
                invalid_params(format!("invalid thread/list cwd filter `{cwd}`: {err}"))
            })?;
        normalized_cwds.push(cwd);
    }

    Ok(Some(normalized_cwds))
}

fn has_model_resume_override(
    request_overrides: Option<&HashMap<String, serde_json::Value>>,
    typesafe_overrides: &ConfigOverrides,
) -> bool {
    typesafe_overrides.model.is_some()
        || typesafe_overrides.model_provider.is_some()
        || request_overrides.is_some_and(|overrides| {
            overrides.contains_key("model") || overrides.contains_key("model_provider")
        })
        || request_overrides
            .is_some_and(|overrides| overrides.contains_key("model_reasoning_effort"))
}

fn has_permission_override(
    request_overrides: Option<&HashMap<String, serde_json::Value>>,
    typesafe_overrides: &ConfigOverrides,
) -> bool {
    typesafe_overrides.sandbox_mode.is_some()
        || typesafe_overrides.permission_profile.is_some()
        || typesafe_overrides.default_permissions.is_some()
        || request_overrides.is_some_and(|overrides| {
            overrides.contains_key("sandbox_mode") || overrides.contains_key("default_permissions")
        })
}

fn apply_persisted_permission_profile(
    settings: &PersistedResumeSettings,
    request_overrides: Option<&HashMap<String, serde_json::Value>>,
    config_overrides: &mut ConfigOverrides,
) {
    if has_permission_override(request_overrides, config_overrides) {
        return;
    }
    config_overrides.persisted_permission_profile_id = settings
        .active_permission_profile
        .as_ref()
        .map(|profile| profile.id.clone());
    if !request_overrides.is_some_and(|overrides| {
        overrides.keys().any(|key| {
            key == "sandbox_workspace_write" || key.starts_with("sandbox_workspace_write.")
        })
    }) {
        config_overrides
            .persisted_permission_profile
            .clone_from(&settings.unnamed_permission_profile);
    }
}

fn validate_dynamic_tools(tools: &[DynamicToolSpec]) -> Result<(), String> {
    const DYNAMIC_TOOL_NAME_MAX_LEN: usize = 128;
    const DYNAMIC_TOOL_NAMESPACE_MAX_LEN: usize = 64;
    const DYNAMIC_TOOL_NAMESPACE_DESCRIPTION_MAX_LEN: usize = 1024;
    const DYNAMIC_TOOL_IDENTIFIER_PATTERN: &str = "^[a-zA-Z0-9_-]+$";
    const RESERVED_RESPONSES_NAMESPACES: &[&str] = &[
        "api_tool",
        "browser",
        "computer",
        "container",
        "file_search",
        "functions",
        "image_gen",
        "multi_tool_use",
        "python",
        "python_user_visible",
        "submodel_delegator",
        "terminal",
        "tool_search",
        "web",
    ];

    fn escape_identifier_for_error(value: &str) -> String {
        value.escape_default().to_string()
    }

    fn validate_dynamic_tool_identifier(
        value: &str,
        label: &str,
        max_len: usize,
    ) -> Result<(), String> {
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(format!(
                "{label} must match {DYNAMIC_TOOL_IDENTIFIER_PATTERN} to match Responses API: {}",
                escape_identifier_for_error(value),
            ));
        }
        if value.chars().count() > max_len {
            return Err(format!(
                "{label} must be at most {max_len} characters to match Responses API: {}",
                escape_identifier_for_error(value),
            ));
        }
        Ok(())
    }

    fn validate_dynamic_tool<'a>(
        tool: &'a DynamicToolFunctionSpec,
        namespace: Option<&str>,
        seen: &mut HashSet<&'a str>,
    ) -> Result<(), String> {
        let name = tool.name.trim();
        if name.is_empty() {
            return Err("dynamic tool name must not be empty".to_string());
        }
        if name != tool.name {
            return Err(format!(
                "dynamic tool name has leading/trailing whitespace: {}",
                escape_identifier_for_error(&tool.name),
            ));
        }
        validate_dynamic_tool_identifier(name, "dynamic tool name", DYNAMIC_TOOL_NAME_MAX_LEN)?;
        if name == "mcp" || name.starts_with("mcp__") {
            return Err(format!("dynamic tool name is reserved: {name}"));
        }
        if !seen.insert(name) {
            if let Some(namespace) = namespace {
                return Err(format!(
                    "duplicate dynamic tool name in namespace {namespace}: {name}"
                ));
            }
            return Err(format!("duplicate dynamic tool name: {name}"));
        }
        if tool.defer_loading && namespace.is_none() {
            return Err(format!(
                "deferred dynamic tool must include a namespace: {name}"
            ));
        }

        if let Err(err) = codex_tools::parse_tool_input_schema(&tool.input_schema) {
            return Err(format!(
                "dynamic tool input schema is not supported for {name}: {err}"
            ));
        }
        Ok(())
    }

    let mut seen_tools = HashSet::new();
    let mut seen_namespaces = HashSet::new();
    for spec in tools {
        match spec {
            DynamicToolSpec::Function(tool) => {
                validate_dynamic_tool(tool, /*namespace*/ None, &mut seen_tools)?;
            }
            DynamicToolSpec::Namespace(namespace) => {
                let name = namespace.name.trim();
                if name.is_empty() {
                    return Err("dynamic tool namespace must not be empty".to_string());
                }
                if name != namespace.name {
                    return Err(format!(
                        "dynamic tool namespace has leading/trailing whitespace: {}",
                        escape_identifier_for_error(&namespace.name),
                    ));
                }
                validate_dynamic_tool_identifier(
                    name,
                    "dynamic tool namespace",
                    DYNAMIC_TOOL_NAMESPACE_MAX_LEN,
                )?;
                if namespace.description.chars().count()
                    > DYNAMIC_TOOL_NAMESPACE_DESCRIPTION_MAX_LEN
                {
                    return Err(format!(
                        "dynamic tool namespace description must be at most {DYNAMIC_TOOL_NAMESPACE_DESCRIPTION_MAX_LEN} characters"
                    ));
                }
                if name == "mcp" || name.starts_with("mcp__") {
                    return Err(format!("dynamic tool namespace is reserved: {name}"));
                }
                if RESERVED_RESPONSES_NAMESPACES.contains(&name) {
                    return Err(format!(
                        "dynamic tool namespace collides with a reserved Responses API namespace: {name}",
                    ));
                }
                if !seen_namespaces.insert(name) {
                    return Err(format!("duplicate dynamic tool namespace: {name}"));
                }
                if namespace.tools.is_empty() {
                    return Err(format!(
                        "dynamic tool namespace must contain at least one tool: {name}"
                    ));
                }
                let mut seen_namespace_tools = HashSet::new();
                for tool in &namespace.tools {
                    let DynamicToolNamespaceTool::Function(tool) = tool;
                    validate_dynamic_tool(tool, Some(name), &mut seen_namespace_tools)?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct ThreadRequestProcessor {
    pub(super) auth_manager: Arc<AuthManager>,
    pub(crate) thread_manager: Arc<ThreadManager>,
    pub(super) outgoing: Arc<OutgoingMessageSender>,
    pub(super) arg0_paths: Arg0DispatchPaths,
    pub(super) config: Arc<Config>,
    pub(super) config_manager: ConfigManager,
    pub(super) thread_store: Arc<dyn ThreadStore>,
    pub(crate) pending_thread_unloads: PendingThreadUnloads,
    pub(super) thread_state_manager: ThreadStateManager,
    pub(super) thread_watch_manager: ThreadWatchManager,
    pub(super) thread_list_state_permit: Arc<Semaphore>,
    pub(super) thread_goal_processor: ThreadGoalRequestProcessor,
    pub(super) state_db: Option<StateDbHandle>,
    pub(super) log_db: Option<LogDbLayer>,
    pub(super) background_tasks: TaskTracker,
    pub(super) sample_responses: Arc<crate::thread_sample::SampleResponseRegistry>,
    pub(super) skills_watcher: Arc<SkillsWatcher>,
    pub(super) turn_cost_worker: Option<crate::turn_cost_worker::TurnCostWorkerHandle>,
    pub(super) initial_config_warnings: Arc<Vec<ConfigWarningNotification>>,
}

/// Whether resume attaches a client or restores a cold runtime during daemon startup.
pub(crate) enum ThreadResumeTarget {
    Client(ConnectionRequestId),
    DaemonRecovery(Option<codex_app_server_transport::daemon_recovery::InterruptedTurn>),
}

/// Outcome of trying to satisfy a resume request from an already loaded thread.
enum RunningThreadResumeResult {
    /// The request was delegated to the loaded thread.
    Handled(tokio::sync::oneshot::Receiver<()>),
    /// No loaded thread handled the request.
    ///
    /// The optional stored thread contains the history-bearing probe that cold
    /// resume can reuse instead of reading the rollout again.
    NotRunning(Option<Box<StoredThread>>),
}

impl ThreadRequestProcessor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        outgoing: Arc<OutgoingMessageSender>,
        arg0_paths: Arg0DispatchPaths,
        config: Arc<Config>,
        config_manager: ConfigManager,
        thread_store: Arc<dyn ThreadStore>,
        pending_thread_unloads: PendingThreadUnloads,
        thread_state_manager: ThreadStateManager,
        thread_watch_manager: ThreadWatchManager,
        thread_list_state_permit: Arc<Semaphore>,
        thread_goal_processor: ThreadGoalRequestProcessor,
        state_db: Option<StateDbHandle>,
        log_db: Option<LogDbLayer>,
        skills_watcher: Arc<SkillsWatcher>,
        turn_cost_worker: Option<crate::turn_cost_worker::TurnCostWorkerHandle>,
        initial_config_warnings: Vec<ConfigWarningNotification>,
        sample_responses: Arc<crate::thread_sample::SampleResponseRegistry>,
    ) -> Self {
        Self {
            auth_manager,
            thread_manager,
            outgoing,
            arg0_paths,
            config,
            config_manager,
            thread_store,
            pending_thread_unloads,
            thread_state_manager,
            thread_watch_manager,
            thread_list_state_permit,
            thread_goal_processor,
            state_db,
            log_db,
            background_tasks: TaskTracker::new(),
            sample_responses,
            skills_watcher,
            turn_cost_worker,
            initial_config_warnings: Arc::new(initial_config_warnings),
        }
    }

    pub(crate) async fn thread_start(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadStartParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
        request_context: RequestContext,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_start_inner(
            request_id,
            params,
            app_server_client_name,
            app_server_client_version,
            client_mcp_extensions,
            request_context,
        )
        .await
        .map(|()| None)
    }

    pub(crate) async fn thread_unsubscribe(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadUnsubscribeParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_unsubscribe_response_inner(params, request_id.connection_id)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_resume(
        &self,
        target: ThreadResumeTarget,
        params: ThreadResumeParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let mut prepared_config = None;
        // Keep the resume future off the request handler's stack.
        while Box::pin(self.thread_resume_inner(
            &target,
            &params,
            app_server_client_name.clone(),
            app_server_client_version.clone(),
            client_mcp_extensions.clone(),
            &mut prepared_config,
        ))
        .await?
        .is_continue()
        {}
        Ok(None)
    }

    pub(crate) async fn thread_fork(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadForkParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
        client_mcp_extensions: ClientMcpExtensions,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        // Keep the large fork future out of the shared request dispatcher's stack frame.
        Box::pin(self.thread_fork_inner(
            request_id,
            params,
            app_server_client_name,
            app_server_client_version,
            client_mcp_extensions,
        ))
        .await
        .map(|()| None)
    }

    pub(crate) async fn thread_sample(
        &self,
        params: ThreadSampleParams,
        rpc_gate: Arc<ConnectionRpcGate>,
        context: RequestContext,
    ) -> Result<ThreadSampleResponse, JSONRPCErrorError> {
        let id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(error.to_string()))?;
        let parent = self
            .thread_manager
            .get_thread(id)
            .await
            .map_err(|_| invalid_request("thread/sample requires a loaded source"))?;
        self.config_manager
            .check_thread_model_provider(parent.config().await.as_ref())
            .await
            .map_err(|error| config_load_error(&error))?;
        generate_sample(
            SampleTask {
                manager: Arc::clone(&self.thread_manager),
                store: Arc::clone(&self.thread_store),
                parent,
                samples: Arc::clone(&self.sample_responses),
                params,
                trace: context.request_trace(),
                cancellation: context.cancellation,
            },
            &self.background_tasks,
            rpc_gate,
        )
        .await
    }

    pub(crate) async fn thread_revert(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadRevertParams,
        app_server_client_name: Option<String>,
        app_server_client_version: Option<String>,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (response, thread_id) = self
            .thread_revert_response(
                &request_id,
                params,
                app_server_client_name,
                app_server_client_version,
            )
            .await?;
        self.outgoing.send_response(request_id, response).await;
        self.outgoing
            .send_server_notification(ServerNotification::ThreadReverted(
                ThreadRevertedNotification { thread_id },
            ))
            .await;
        Ok(None)
    }

    pub(crate) async fn thread_archive(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadArchiveParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
        match self.thread_archive_response(params).await {
            Ok((response, archived_thread_ids)) => {
                self.outgoing
                    .send_response(request_id.clone(), response)
                    .await;
                for thread_id in archived_thread_ids {
                    self.outgoing
                        .send_server_notification(ServerNotification::ThreadArchived(
                            ThreadArchivedNotification { thread_id },
                        ))
                        .await;
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn thread_increment_elicitation(
        &self,
        params: ThreadIncrementElicitationParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_increment_elicitation_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_decrement_elicitation(
        &self,
        params: ThreadDecrementElicitationParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_decrement_elicitation_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_set_name(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadSetNameParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        match self.thread_set_name_response_inner(params).await {
            Ok((response, notification)) => {
                self.outgoing
                    .send_response(request_id.clone(), response)
                    .await;
                if let Some(notification) = notification {
                    self.outgoing
                        .send_server_notification(ServerNotification::ThreadNameUpdated(
                            notification,
                        ))
                        .await;
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn thread_metadata_update(
        &self,
        params: ThreadMetadataUpdateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_metadata_update_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_section_move(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadSectionMoveParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let ThreadSectionMoveParams {
            thread_id,
            section_id,
            before_thread_id,
        } = params;
        let thread_uuid = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        if section_id
            .as_deref()
            .is_some_and(|section| section.trim().is_empty())
        {
            return Err(invalid_request("sectionId must not be empty"));
        }
        if section_id.is_none() && before_thread_id.is_some() {
            return Err(invalid_request(
                "beforeThreadId requires a non-null sectionId",
            ));
        }
        let before_thread_uuid = before_thread_id
            .map(|thread_id| {
                ThreadId::from_string(&thread_id)
                    .map_err(|err| invalid_request(format!("invalid before thread id: {err}")))
            })
            .transpose()?;

        let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
        self.thread_manager
            .move_thread_to_section(thread_uuid, section_id.as_deref(), before_thread_uuid)
            .await
            .map_err(|err| core_thread_write_error("move thread in section", err))?;
        self.outgoing
            .send_response(request_id, ThreadSectionMoveResponse {})
            .await;
        self.outgoing
            .send_server_notification(ServerNotification::ThreadSectionChanged(
                ThreadSectionChangedNotification {
                    thread_id: thread_uuid.to_string(),
                    section_id,
                },
            ))
            .await;
        Ok(None)
    }

    pub(crate) async fn thread_memory_mode_set(
        &self,
        params: ThreadMemoryModeSetParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_memory_mode_set_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn memory_reset(
        &self,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.memory_reset_response_inner()
            .await
            .map(|response: MemoryResetResponse| Some(response.into()))
    }

    pub(crate) async fn thread_unarchive(
        &self,
        request_id: ConnectionRequestId,
        params: ThreadUnarchiveParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        // Queue every restoration notification before another list mutation can run.
        let _thread_list_state_permit = self.acquire_thread_list_state_permit().await?;
        match self.thread_unarchive_response(params).await {
            Ok((response, unarchived_thread_ids)) => {
                self.outgoing
                    .send_response(request_id.clone(), response)
                    .await;
                for thread_id in unarchived_thread_ids {
                    self.outgoing
                        .send_server_notification(ServerNotification::ThreadUnarchived(
                            ThreadUnarchivedNotification {
                                thread_id: thread_id.to_string(),
                            },
                        ))
                        .await;
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn thread_compact_start(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadCompactStartParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_compact_start_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_background_terminals_clean(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadBackgroundTerminalsCleanParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_background_terminals_clean_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_background_terminals_list(
        &self,
        params: ThreadBackgroundTerminalsListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_background_terminals_list_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_background_terminals_terminate(
        &self,
        params: ThreadBackgroundTerminalsTerminateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_background_terminals_terminate_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    async fn send_deprecation_notice(&self, connection_id: ConnectionId, summary: &str) {
        self.outgoing
            .send_server_notification_to_connections(
                &[connection_id],
                ServerNotification::DeprecationNotice(DeprecationNoticeNotification {
                    summary: summary.to_string(),
                    details: None,
                }),
            )
            .await;
    }

    pub(crate) async fn thread_list(
        &self,
        params: ThreadListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_list_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_search(
        &self,
        params: ThreadSearchParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_search_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_search_occurrences(
        &self,
        params: ThreadSearchOccurrencesParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_search_occurrences_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_loaded_list(
        &self,
        params: ThreadLoadedListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_loaded_list_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_read(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let include_turns = params.include_turns;
        let response = self.thread_read_response_inner(params).await?;
        if include_turns
            && matches!(
                response.thread.history_mode,
                ApiThreadHistoryMode::Paginated
            )
        {
            self.send_deprecation_notice(
                request_id.connection_id,
                PAGINATED_THREAD_READ_DEPRECATION_SUMMARY,
            )
            .await;
        }
        Ok(Some(response.into()))
    }

    pub(crate) async fn thread_turns_list(
        &self,
        params: ThreadTurnsListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_turns_list_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_items_list(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadItemsListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let cursor_kind = match &params.cursor {
            None => "none",
            Some(ThreadItemsListCursor::Opaque(_)) => "opaque",
            Some(ThreadItemsListCursor::Anchor(ThreadItemsListAnchor::Item { .. })) => {
                "item_anchor"
            }
        };
        let started = std::time::Instant::now();
        let result = self.thread_items_list_response_inner(params).await;
        let page = result.as_ref().ok();
        tracing::debug!(
            request_id = %request_id.request_id,
            cursor_kind,
            item_count = page.map(|page| page.data.len()),
            has_next_page = page.map(|page| page.next_cursor.is_some()),
            duration_ms = started.elapsed().as_millis() as u64,
            success = result.is_ok(),
            "thread/items/list finished"
        );
        result.map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_items_read(
        &self,
        params: ThreadItemsReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Self::read_thread_items(self.thread_store.as_ref(), params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_timeline_list(
        &self,
        params: ThreadTimelineListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let thread_id = ThreadId::from_string(&params.thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let page = self
            .thread_store
            .list_timeline(StoreListTimelineParams {
                thread_id,
                cursor: params.cursor,
                page_size: params
                    .limit
                    .map(|limit| limit as usize)
                    .unwrap_or(THREAD_ITEMS_DEFAULT_LIMIT)
                    .clamp(1, THREAD_ITEMS_MAX_LIMIT),
            })
            .await
            .map_err(paginated_history_list_error)?;
        Ok(Some(
            ThreadTimelineListResponse {
                data: page.items,
                next_cursor: page.next_cursor,
                active_realtime_session_at_page_start: page.active_realtime_session_at_page_start,
            }
            .into(),
        ))
    }

    pub(crate) async fn thread_shell_command(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadShellCommandParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_shell_command_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn thread_approve_guardian_denied_action(
        &self,
        request_id: &ConnectionRequestId,
        params: ThreadApproveGuardianDeniedActionParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_approve_guardian_denied_action_inner(request_id, params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn conversation_summary(
        &self,
        params: GetConversationSummaryParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.get_thread_summary_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    async fn memory_reset_response_inner(&self) -> Result<MemoryResetResponse, JSONRPCErrorError> {
        let state_db = self
            .state_db
            .clone()
            .ok_or_else(|| internal_error("sqlite state db unavailable for memory reset"))?;

        state_db.clear_all_memory_data().await.map_err(|err| {
            internal_error(format!("failed to clear memory rows in memories db: {err}"))
        })?;

        clear_memory_roots_contents(&self.config.codex_home)
            .await
            .map_err(|err| {
                internal_error(format!(
                    "failed to clear memory directories under {}: {err}",
                    self.config.codex_home.display()
                ))
            })?;

        Ok(MemoryResetResponse {})
    }
}

const THREAD_TURNS_DEFAULT_LIMIT: usize = 25;
const THREAD_TURNS_MAX_LIMIT: usize = 100;
const THREAD_ITEMS_DEFAULT_LIMIT: usize = 25;
pub(super) const THREAD_ITEMS_MAX_LIMIT: usize = 100;
const THREAD_SEARCH_OCCURRENCES_DEFAULT_LIMIT: usize = 50;
const THREAD_SEARCH_OCCURRENCES_MAX_LIMIT: usize = 250;

struct ThreadTurnsPage {
    pub(super) turns: Vec<Turn>,
    pub(super) next_cursor: Option<String>,
    pub(super) backwards_cursor: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadTurnsCursor {
    turn_id: String,
    include_anchor: bool,
}

struct ThreadTurnsPageOptions<'a> {
    cursor: Option<&'a str>,
    limit: Option<u32>,
    sort_direction: SortDirection,
    items_view: TurnItemsView,
}

pub(super) enum ThreadReadViewError {
    InvalidRequest(String),
    Unsupported(&'static str),
    Internal(String),
    JsonRpc(JSONRPCErrorError),
}

fn thread_store_mutation_error(operation: &str, err: ThreadStoreError) -> JSONRPCErrorError {
    match err {
        ThreadStoreError::InvalidRequest { message } | ThreadStoreError::Conflict { message } => {
            invalid_request(message)
        }
        ThreadStoreError::Unsupported {
            operation: unsupported_operation,
        } => unsupported_thread_store_operation(unsupported_operation),
        err => internal_error(format!("failed to {operation} session: {err}")),
    }
}

#[cfg(test)]
#[path = "thread_processor_tests.rs"]
mod thread_processor_tests;
