use crate::accepted_lines::AcceptedLineFingerprintEventInput;
use crate::accepted_lines::accepted_line_counts_from_unified_diff;
use crate::accepted_lines::accepted_line_fingerprint_event_requests;
use crate::accepted_lines::accepted_line_repo_hash_for_cwd;
use crate::events::AppServerRpcTransport;
use crate::events::CodexAppMentionedEventRequest;
use crate::events::CodexAppServerClientMetadata;
use crate::events::CodexAppUsedEventRequest;
use crate::events::CodexAppUsedMetadata;
use crate::events::CodexCollabAgentToolCallEventParams;
use crate::events::CodexCollabAgentToolCallEventRequest;
use crate::events::CodexCommandExecutionEventParams;
use crate::events::CodexCommandExecutionEventRequest;
use crate::events::CodexCompactionEventRequest;
use crate::events::CodexControlToolCallEventParams;
use crate::events::CodexControlToolCallEventRequest;
use crate::events::CodexCuaToolCallStartedEventParams;
use crate::events::CodexCuaToolCallStartedEventRequest;
use crate::events::CodexDynamicToolCallEventParams;
use crate::events::CodexDynamicToolCallEventRequest;
use crate::events::CodexFileChangeEventParams;
use crate::events::CodexFileChangeEventRequest;
use crate::events::CodexGoalEventRequest;
use crate::events::CodexHookRunEventRequest;
use crate::events::CodexImageGenerationEventParams;
use crate::events::CodexImageGenerationEventRequest;
use crate::events::CodexMcpToolCallEventParams;
use crate::events::CodexMcpToolCallEventRequest;
use crate::events::CodexOnboardingExternalAgentImportCompleteEventRequest;
use crate::events::CodexOnboardingExternalAgentImportCompleteMetadata;
use crate::events::CodexOnboardingExternalAgentImportFailureEventRequest;
use crate::events::CodexOnboardingExternalAgentImportFailureMetadata;
use crate::events::CodexPluginEventRequest;
use crate::events::CodexPluginInstallFailedEventRequest;
use crate::events::CodexPluginInstallFailedMetadata;
use crate::events::CodexPluginInstallRequestedEventRequest;
use crate::events::CodexPluginMeasurementEventParams;
use crate::events::CodexPluginMeasurementEventRequest;
use crate::events::CodexPluginUsedEventRequest;
use crate::events::CodexReviewEventParams;
use crate::events::CodexReviewEventRequest;
use crate::events::CodexRuntimeMetadata;
use crate::events::CodexToolItemEventBase;
use crate::events::CodexTurnEventParams;
use crate::events::CodexTurnEventRequest;
use crate::events::CodexTurnSteerEventParams;
use crate::events::CodexTurnSteerEventRequest;
use crate::events::CodexWebSearchEventParams;
use crate::events::CodexWebSearchEventRequest;
use crate::events::FinalApprovalOutcome;
use crate::events::GuardianReviewEventParams;
use crate::events::GuardianReviewEventPayload;
use crate::events::GuardianReviewEventRequest;
use crate::events::ReviewResolution;
use crate::events::ReviewStatus;
use crate::events::ReviewSubjectKind;
use crate::events::ReviewTrigger;
use crate::events::Reviewer;
use crate::events::SkillInvocationEventParams;
use crate::events::SkillInvocationEventRequest;
use crate::events::ThreadArchiveAction;
use crate::events::ThreadArchiveEvent;
use crate::events::ThreadArchiveEventParams;
use crate::events::ThreadInitializedEvent;
use crate::events::ThreadInitializedEventParams;
use crate::events::ToolEventType;
use crate::events::ToolItemFailureKind;
use crate::events::ToolItemTerminalStatus;
use crate::events::TrackEventRequest;
use crate::events::WebSearchActionKind;
use crate::events::codex_app_metadata;
use crate::events::codex_artifact_operation_event_request;
use crate::events::codex_compaction_event_params;
use crate::events::codex_goal_event_params;
use crate::events::codex_hook_run_metadata;
use crate::events::codex_plugin_install_requested_metadata;
use crate::events::codex_plugin_metadata;
use crate::events::codex_plugin_used_metadata;
use crate::events::plugin_state_event_type;
use crate::events::subagent_source_name;
use crate::events::subagent_thread_started_event_request;
use crate::facts::AnalyticsFact;
use crate::facts::AnalyticsJsonRpcError;
use crate::facts::AppMentionedInput;
use crate::facts::AppUsedInput;
use crate::facts::ArtifactOperationInput;
use crate::facts::CodeModeToolCallFact;
use crate::facts::CodeModeToolCallStatus;
use crate::facts::CodexCompactionEvent;
use crate::facts::CodexGoalEvent;
use crate::facts::ControlToolCallFact;
use crate::facts::ControlToolCallStatus;
use crate::facts::CustomAnalyticsFact;
use crate::facts::ElicitationType;
use crate::facts::ExternalAgentConfigImportCompletedInput;
use crate::facts::ExternalAgentConfigImportFailureInput;
use crate::facts::HookRunInput;
use crate::facts::ImagePreparationFact;
use crate::facts::ImagePreparationMetadata;
use crate::facts::InvocationType;
use crate::facts::McpToolCallElicitation;
use crate::facts::PluginInstallFailedInput;
use crate::facts::PluginInstallRequestedInput;
use crate::facts::PluginMeasurementRow;
use crate::facts::PluginMeasurementsInput;
use crate::facts::PluginState;
use crate::facts::PluginStateChangedInput;
use crate::facts::PluginUsedInput;
use crate::facts::SkillInvocationLocation;
use crate::facts::SkillInvokedInput;
use crate::facts::SubAgentThreadStartedInput;
use crate::facts::ThreadInitializationMode;
use crate::facts::TurnAnalyticsMetadata;
use crate::facts::TurnCodexError;
use crate::facts::TurnCodexErrorFact;
use crate::facts::TurnProfile;
use crate::facts::TurnProfileFact;
use crate::facts::TurnResolvedConfigFact;
use crate::facts::TurnStatus;
use crate::facts::TurnSteerRejectionReason;
use crate::facts::TurnSteerResult;
use crate::facts::TurnTokenUsageFact;
use crate::guardian_v2::GuardianV2EventKind;
use crate::guardian_v2::GuardianV2EventParams;
use crate::guardian_v2::GuardianV2EventRequest;
use crate::now_unix_millis;
use crate::now_unix_seconds;
use crate::option_i64_to_u64;
use crate::serialize_enum_as_string;
use crate::usize_to_u64;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ClientResponse;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::CollabAgentStatus;
use codex_app_server_protocol::CollabAgentTool;
use codex_app_server_protocol::CollabAgentToolCallStatus;
use codex_app_server_protocol::CommandAction;
use codex_app_server_protocol::CommandExecutionApprovalDecision;
use codex_app_server_protocol::CommandExecutionApprovalKind;
use codex_app_server_protocol::CommandExecutionSource;
use codex_app_server_protocol::CommandExecutionStatus;
use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallStatus;
use codex_app_server_protocol::FileChangeApprovalDecision;
use codex_app_server_protocol::GuardianApprovalReviewAction;
use codex_app_server_protocol::GuardianApprovalReviewStatus;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::McpToolCallStatus;
use codex_app_server_protocol::NetworkPolicyRuleAction;
use codex_app_server_protocol::PatchApplyStatus;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::RequestPermissionProfile;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ServerResponse;
use codex_app_server_protocol::SubAgentActivityKind;
use codex_app_server_protocol::ThreadItemPayload;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::UserInput;
use codex_app_server_protocol::WebSearchAction;
use codex_git_utils::SanitizedGitUrl;
use codex_git_utils::get_git_origin_url;
use codex_git_utils::get_git_repo_root;
use codex_login::default_client::originator;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::items::ModelInvocationContext;
use codex_protocol::items::is_safe_plugin_relative_path;
use codex_protocol::mcp::is_node_repl_backed_connector;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SkillScope;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::request_permissions::PermissionGrantScope as CorePermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionsResponse as CoreRequestPermissionsResponse;
use codex_utils_path_uri::PathUri;
use sha1::Digest;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

mod client_rpc;
mod notification_ingest;
mod product_usage;
mod reviews;
mod thread_lifecycle;
mod tool_ingest;
mod tool_item_events;
mod turn_events;
#[cfg(test)]
pub(crate) use product_usage::normalize_path_for_skill_id;
#[cfg(test)]
pub(crate) use product_usage::skill_id_for_local_skill;
#[cfg(test)]
use reviews::guardian_review_result;
use thread_lifecycle::warn_missing_analytics_context;
use tool_ingest::tool_response_attribution;
use tool_item_events::cua_mcp_tool_identity;
use tool_item_events::observed_duration_ms;
#[cfg(test)]
use tool_item_events::safe_plugin_relative_script_path;
use tool_item_events::tool_item_base;
use tool_item_events::tool_item_event;
pub(crate) use tool_item_events::tracked_tool_item_id;
use turn_events::accepted_line_event_input;
use turn_events::codex_turn_event_params;
use turn_events::num_input_images;

const MAX_TOOL_RESPONSE_ENTRIES: usize = 256;
const MAX_RECENT_VOICE_SESSION_WINDOWS: usize = 16;

pub(crate) const MAX_PLUGIN_MEASUREMENTS_PER_BATCH: usize = 100;
const MAX_PLUGIN_MEASUREMENT_DIMENSIONS: usize = 8;
const MAX_PLUGIN_MEASUREMENT_IDENTIFIER_BYTES: usize = 64;

pub(crate) fn valid_plugin_measurement_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some('a'..='z'))
        && value.len() <= MAX_PLUGIN_MEASUREMENT_IDENTIFIER_BYTES
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}

pub(crate) fn valid_plugin_measurement_row(row: &PluginMeasurementRow) -> bool {
    row.number_value.is_finite()
        && valid_plugin_measurement_identifier(&row.measurement_name)
        && row.dimensions.len() <= MAX_PLUGIN_MEASUREMENT_DIMENSIONS
        && row.dimensions.iter().all(|(name, value)| {
            valid_plugin_measurement_identifier(name) && valid_plugin_measurement_identifier(value)
        })
}

// Match the fact queue capacity: analytics must not retain an unlimited number
// of incomplete turns when lifecycle facts are dropped.
const MAX_PENDING_TURNS: usize = 256;

#[derive(Default)]
pub(crate) struct AnalyticsReducer {
    requests: HashMap<(u64, RequestId), RequestState>,
    turns: HashMap<String, TurnState>,
    turn_order: VecDeque<String>,
    connections: HashMap<u64, ConnectionState>,
    threads: HashMap<String, ThreadAnalyticsState>,
    tool_items_started_at_ms: HashMap<ToolItemKey, (u64, Option<ModelInvocationContext>)>,
    tool_response_states: HashMap<(String, String), ToolResponseState>,
    code_mode_cells: HashMap<String, HashMap<String, CodeModeCellState>>,
    pending_reviews: HashMap<RequestId, PendingReviewState>,
    item_review_summaries: HashMap<ToolItemKey, ItemReviewSummary>,
    // Bounded to 256 entries, keeping linear lookups small; evict the oldest entry when full.
    pending_mcp_tool_elicitations: VecDeque<(ToolItemKey, ElicitationType)>,
}

struct ConnectionState {
    app_server_client: CodexAppServerClientMetadata,
    runtime: CodexRuntimeMetadata,
}

#[derive(Default)]
struct ThreadAnalyticsState {
    connection_id: Option<u64>,
    metadata: Option<ThreadMetadataState>,
    originator: Option<String>,
    active_voice_session: Option<VoiceSessionWindow>,
    recent_voice_sessions: VecDeque<VoiceSessionWindow>,
    active_turn_id: Option<String>,
    pending_voice_handoffs: VecDeque<Option<String>>,
}

#[derive(Clone)]
struct VoiceSessionWindow {
    session_id: Option<String>,
    started_at: u64,
    closed_at: Option<u64>,
}

impl ThreadAnalyticsState {
    fn app_server_client(
        &self,
        connection_state: &ConnectionState,
    ) -> CodexAppServerClientMetadata {
        let mut app_server_client = connection_state.app_server_client.clone();
        if let Some(originator) = self.originator.as_ref() {
            app_server_client.product_client_id.clone_from(originator);
        }
        app_server_client
    }

    fn active_voice_session_id(&self) -> Option<String> {
        self.active_voice_session
            .as_ref()
            .and_then(|session| session.session_id.clone())
    }

    fn voice_session_id_at(&self, timestamp: u64) -> Option<String> {
        self.active_voice_session
            .iter()
            .chain(self.recent_voice_sessions.iter().rev())
            .find(|session| {
                session.started_at <= timestamp
                    && session
                        .closed_at
                        .is_none_or(|closed_at| timestamp <= closed_at)
            })
            .and_then(|session| session.session_id.clone())
    }
}

#[derive(Clone, Copy)]
struct AnalyticsDropSite<'a> {
    event_name: &'static str,
    thread_id: &'a str,
    turn_id: Option<&'a str>,
    review_id: Option<&'a str>,
    item_id: Option<&'a str>,
}

impl<'a> AnalyticsDropSite<'a> {
    fn guardian(input: &'a GuardianReviewEventParams) -> Self {
        Self {
            event_name: "guardian",
            thread_id: &input.thread_id,
            turn_id: Some(&input.turn_id),
            review_id: Some(&input.review_id),
            item_id: None,
        }
    }

    fn review(input: &'a PendingReviewState) -> Self {
        Self {
            event_name: "review",
            thread_id: &input.thread_id,
            turn_id: Some(&input.turn_id),
            review_id: Some(&input.review_id),
            item_id: input.item_id.as_deref(),
        }
    }

    fn compaction(input: &'a CodexCompactionEvent) -> Self {
        Self {
            event_name: "compaction",
            thread_id: &input.thread_id,
            turn_id: Some(&input.turn_id),
            review_id: None,
            item_id: None,
        }
    }

    fn goal(input: &'a CodexGoalEvent) -> Self {
        Self {
            event_name: "goal",
            thread_id: &input.thread_id,
            turn_id: input.turn_id.as_deref(),
            review_id: None,
            item_id: None,
        }
    }

    fn tool_item(
        notification: &'a codex_app_server_protocol::ItemCompletedNotification,
        item_id: &'a str,
    ) -> Self {
        Self {
            event_name: "tool item",
            thread_id: &notification.thread_id,
            turn_id: Some(&notification.turn_id),
            review_id: None,
            item_id: Some(item_id),
        }
    }

    fn cua_tool_call_started(thread_id: &'a str, turn_id: &'a str, item_id: &'a str) -> Self {
        Self {
            event_name: "CUA tool call start",
            thread_id,
            turn_id: Some(turn_id),
            review_id: None,
            item_id: Some(item_id),
        }
    }

    fn turn_steer(thread_id: &'a str) -> Self {
        Self {
            event_name: "turn steer",
            thread_id,
            turn_id: None,
            review_id: None,
            item_id: None,
        }
    }

    fn turn(thread_id: &'a str, turn_id: &'a str) -> Self {
        Self {
            event_name: "turn",
            thread_id,
            turn_id: Some(turn_id),
            review_id: None,
            item_id: None,
        }
    }
}

enum MissingAnalyticsContext {
    ThreadConnection,
    Connection { connection_id: u64 },
    ThreadMetadata,
}

#[derive(Clone)]
struct PendingReviewState {
    thread_id: String,
    turn_id: String,
    item_id: Option<String>,
    review_id: String,
    subject_kind: ReviewSubjectKind,
    subject_name: String,
    trigger: ReviewTrigger,
    started_at_ms: u64,
    requested_additional_permissions: bool,
    requested_network_access: bool,
}

#[derive(Clone, Default)]
struct ItemReviewSummary {
    review_count: u64,
    guardian_review_count: u64,
    user_review_count: u64,
    final_approval_outcome: Option<FinalApprovalOutcome>,
    requested_additional_permissions: bool,
    requested_network_access: bool,
}

#[derive(Clone)]
struct ThreadMetadataState {
    session_id: String,
    thread_source: Option<ThreadSource>,
    initialization_mode: ThreadInitializationMode,
    subagent_source: Option<String>,
    parent_thread_id: Option<String>,
}

impl ThreadMetadataState {
    fn from_thread_metadata(
        session_id: String,
        session_source: &SessionSource,
        thread_source: Option<ThreadSource>,
        parent_thread_id: Option<String>,
        initialization_mode: ThreadInitializationMode,
    ) -> Self {
        let subagent_source = match session_source {
            SessionSource::SubAgent(subagent_source) => Some(subagent_source_name(subagent_source)),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Custom(_)
            | SessionSource::Internal(_)
            | SessionSource::Unknown => None,
        };
        Self {
            session_id,
            thread_source,
            initialization_mode,
            subagent_source,
            parent_thread_id,
        }
    }
}

enum RequestState {
    TurnStart(PendingTurnStartState),
    TurnSteer(PendingTurnSteerState),
    ExplicitClientInterrupt(PendingTurnInterruptState),
}

struct PendingTurnStartState {
    thread_id: String,
    num_input_images: usize,
}

struct PendingTurnSteerState {
    thread_id: String,
    expected_turn_id: String,
    num_input_images: usize,
    created_at: u64,
}

struct PendingTurnInterruptState {
    turn_id: String,
    requested_at_ms: u64,
}

#[derive(Clone)]
struct CompletedTurnState {
    status: Option<TurnStatus>,
    turn_error: Option<CodexErrorInfo>,
    completed_at: u64,
    duration_ms: Option<u64>,
}

#[derive(Default)]
struct TurnState {
    connection_id: Option<u64>,
    thread_id: Option<String>,
    voice_session_id: Option<String>,
    num_input_images: Option<usize>,
    image_preparations: Vec<ImagePreparationMetadata>,
    resolved_config: Option<TurnResolvedConfigFact>,
    started_at: Option<u64>,
    token_usage: Option<TokenUsage>,
    profile: Option<TurnProfile>,
    completed: Option<CompletedTurnState>,
    explicit_client_interrupt_requested_at_ms: Option<u64>,
    codex_error: Option<TurnCodexError>,
    latest_diff: Option<String>,
    steer_count: usize,
    tool_counts: TurnToolCounts,
    completed_cua_tool_item_ids: HashSet<String>,
    resource_skill_invocations: HashSet<String>,
    turn_event_emitted: bool,
}

#[derive(Clone, Hash, Eq, PartialEq)]
struct ToolItemKey {
    thread_id: String,
    turn_id: String,
    item_id: String,
}

#[derive(Default)]
struct ToolResponseState {
    response_ids_by_call_id: HashMap<String, String>,
    cell_ids_by_child_call_id: HashMap<String, String>,
    pending_tool_events: VecDeque<TrackEventRequest>,
}

struct CodeModeCellState {
    parent_call_id: String,
    originating_response_id: Option<String>,
    closed_in_turn_id: Option<String>,
}

enum ToolEventEmission {
    ImmediateUnlessCorrelated,
    AwaitResponse,
}

#[derive(Default)]
struct TurnToolCounts {
    total: usize,
    shell_command: usize,
    file_change: usize,
    mcp_tool_call: usize,
    dynamic_tool_call: usize,
    subagent_tool_call: usize,
    subagent_tool_call_ids: HashSet<String>,
    web_search: usize,
    image_generation: usize,
}

impl TurnToolCounts {
    fn record(&mut self, item: &ThreadItemPayload) {
        match item {
            ThreadItemPayload::CommandExecution { .. } => self.shell_command += 1,
            ThreadItemPayload::FileChange { .. } => self.file_change += 1,
            ThreadItemPayload::McpToolCall { .. } => self.mcp_tool_call += 1,
            ThreadItemPayload::DynamicToolCall { .. } => self.dynamic_tool_call += 1,
            ThreadItemPayload::SubAgentActivity {
                kind: SubAgentActivityKind::Completed,
                ..
            } => return,
            ThreadItemPayload::CollabAgentToolCall { id, .. }
            | ThreadItemPayload::SubAgentActivity { id, .. } => {
                if !self.subagent_tool_call_ids.insert(id.clone()) {
                    return;
                }
                self.subagent_tool_call += 1;
            }
            ThreadItemPayload::WebSearch(_) => self.web_search += 1,
            ThreadItemPayload::ImageGeneration(_) => self.image_generation += 1,
            ThreadItemPayload::UserMessage { .. }
            | ThreadItemPayload::HookPrompt { .. }
            | ThreadItemPayload::AgentMessage { .. }
            | ThreadItemPayload::FunctionCallOutput { .. }
            | ThreadItemPayload::Plan { .. }
            | ThreadItemPayload::Reasoning { .. }
            | ThreadItemPayload::ImageView { .. }
            | ThreadItemPayload::Sleep(_)
            | ThreadItemPayload::EnteredReviewMode { .. }
            | ThreadItemPayload::ExitedReviewMode { .. }
            | ThreadItemPayload::ContextCompaction { .. } => return,
        }
        self.total += 1;
    }
}

impl AnalyticsReducer {
    fn turn_state(&mut self, turn_id: String) -> &mut TurnState {
        if !self.turns.contains_key(&turn_id) {
            // Remove stale ordering entries before evicting the oldest retained turn.
            // At capacity this may discard incomplete or pending background-tool analytics.
            self.turn_order.retain(|id| self.turns.contains_key(id));
            if self.turns.len() >= MAX_PENDING_TURNS
                && let Some(oldest) = self.turn_order.pop_front()
            {
                self.discard_turn(&oldest);
            }
            self.turn_order.push_back(turn_id.clone());
        }
        self.turns.entry(turn_id).or_default()
    }

    fn discard_turn(&mut self, turn_id: &str) {
        self.turns.remove(turn_id);
        self.tool_items_started_at_ms
            .retain(|key, _| key.turn_id != turn_id);
        self.tool_response_states
            .retain(|(_, pending_turn_id), _| pending_turn_id != turn_id);
        self.pending_reviews
            .retain(|_, review| review.turn_id != turn_id);
        self.item_review_summaries
            .retain(|key, _| key.turn_id != turn_id);
        self.pending_mcp_tool_elicitations
            .retain(|(key, _)| key.turn_id != turn_id);
    }

    pub(crate) async fn ingest(&mut self, input: AnalyticsFact, out: &mut Vec<TrackEventRequest>) {
        match input {
            AnalyticsFact::Initialize {
                connection_id,
                params,
                product_client_id,
                runtime,
                rpc_transport,
            } => {
                self.ingest_initialize(
                    connection_id,
                    params,
                    product_client_id,
                    runtime,
                    rpc_transport,
                );
            }
            AnalyticsFact::ClientRequest {
                connection_id,
                request_id,
                request,
            } => {
                self.ingest_request(connection_id, request_id, *request);
            }
            AnalyticsFact::ExplicitClientInterruptRequest {
                connection_id,
                request_id,
                turn_id,
                requested_at_ms,
            } => {
                self.requests.insert(
                    (connection_id, request_id),
                    RequestState::ExplicitClientInterrupt(PendingTurnInterruptState {
                        turn_id,
                        requested_at_ms,
                    }),
                );
            }
            AnalyticsFact::ClientResponse {
                connection_id,
                request_id,
                response,
                thread_originator,
            } => {
                if let Some(response) = response.into_client_response(request_id) {
                    self.ingest_response(connection_id, response, thread_originator, out)
                        .await;
                }
            }
            AnalyticsFact::ErrorResponse {
                connection_id,
                request_id,
                error: _,
                error_type,
            } => {
                self.ingest_error_response(connection_id, request_id, error_type, out);
            }
            AnalyticsFact::Notification(notification) => {
                self.ingest_notification(*notification, out).await;
            }
            AnalyticsFact::ServerRequest {
                connection_id,
                request,
            } => {
                self.ingest_server_request(connection_id, *request);
            }
            AnalyticsFact::ServerResponse {
                completed_at_ms,
                response,
            } => {
                self.ingest_server_response(completed_at_ms, *response, out);
            }
            AnalyticsFact::EffectivePermissionsApprovalResponse {
                completed_at_ms,
                request_id,
                response,
            } => {
                self.ingest_effective_permissions_approval_response(
                    completed_at_ms,
                    request_id,
                    *response,
                    out,
                );
            }
            AnalyticsFact::ServerRequestAborted {
                completed_at_ms,
                request_id,
            } => {
                self.ingest_server_request_aborted(completed_at_ms, request_id, out);
            }
            AnalyticsFact::RealtimeHandoffRequested { thread_id } => {
                let thread = self.threads.entry(thread_id).or_default();
                // ThreadRealtimeStarted already attributes an active turn; only a new turn
                // needs the handoff queued after the realtime session closes.
                if thread.active_turn_id.is_none() {
                    thread
                        .pending_voice_handoffs
                        .push_back(thread.active_voice_session_id());
                }
            }
            AnalyticsFact::RealtimeSessionStarted {
                thread_id,
                realtime_session_id,
                started_at,
            } => {
                self.start_voice_session(thread_id, realtime_session_id, started_at);
            }
            AnalyticsFact::RealtimeSessionUpdated {
                thread_id,
                realtime_session_id,
            } => {
                self.set_active_voice_session_id(thread_id, Some(realtime_session_id));
            }
            AnalyticsFact::RealtimeSessionClosed {
                thread_id,
                closed_at,
            } => {
                self.close_voice_session(&thread_id, closed_at);
            }
            AnalyticsFact::Custom(input) => match input {
                CustomAnalyticsFact::ArtifactOperation(input) => {
                    self.ingest_artifact_operation(input, out);
                }
                CustomAnalyticsFact::CodeModeToolCall(input) => {
                    self.ingest_code_mode_tool_call(input, out);
                }
                CustomAnalyticsFact::ControlToolCall(input) => {
                    self.ingest_control_tool_call(input, out);
                }
                CustomAnalyticsFact::SubAgentThreadStarted(input) => {
                    self.ingest_subagent_thread_started(input, out);
                }
                CustomAnalyticsFact::Compaction(input) => {
                    self.ingest_compaction(*input, out);
                }
                CustomAnalyticsFact::Goal(input) => {
                    self.ingest_goal(*input, out);
                }
                CustomAnalyticsFact::ThreadHintStatus(input) => {
                    if let Some((connection, thread, metadata)) =
                        self.thread_context_or_warn(AnalyticsDropSite {
                            event_name: "codex_thread_hint_status",
                            thread_id: &input.thread_id,
                            turn_id: None,
                            review_id: None,
                            item_id: None,
                        })
                    {
                        out.push(TrackEventRequest::ThreadHintStatus(Box::new(
                            crate::thread_hint::ThreadHintStatusEventRequest {
                                event_type: "codex_thread_hint_status",
                                event_params: crate::thread_hint::ThreadHintStatusEventParams {
                                    thread_id: input.thread_id,
                                    session_id: metadata.session_id.clone(),
                                    app_server_client: thread.app_server_client(connection),
                                    runtime: connection.runtime.clone(),
                                    thread_source: metadata.thread_source.clone(),
                                    subagent_source: metadata.subagent_source.clone(),
                                    parent_thread_id: metadata.parent_thread_id.clone(),
                                    status: input.status,
                                    occurred_at_ms: input.occurred_at_ms,
                                },
                            },
                        )));
                    }
                }
                CustomAnalyticsFact::GuardianV2(input) => {
                    let event_type = match &input.kind {
                        GuardianV2EventKind::Classification { .. } => {
                            "codex_guardian_v2_classification"
                        }
                        GuardianV2EventKind::FastDecision { .. } => {
                            "codex_guardian_v2_fast_decision"
                        }
                    };
                    if let Some((connection, thread, metadata)) =
                        self.thread_context_or_warn(AnalyticsDropSite {
                            event_name: event_type,
                            thread_id: &input.thread_id,
                            turn_id: Some(&input.turn_id),
                            review_id: None,
                            item_id: input.item_id.as_deref(),
                        })
                    {
                        out.push(TrackEventRequest::GuardianV2(Box::new(
                            GuardianV2EventRequest {
                                event_type,
                                event_params: GuardianV2EventParams {
                                    session_id: metadata.session_id.clone(),
                                    app_server_client: thread.app_server_client(connection),
                                    runtime: connection.runtime.clone(),
                                    thread_source: metadata.thread_source.clone(),
                                    subagent_source: metadata.subagent_source.clone(),
                                    parent_thread_id: metadata.parent_thread_id.clone(),
                                    guardian_v2: *input,
                                },
                            },
                        )));
                    }
                }
                CustomAnalyticsFact::GuardianReview(input) => {
                    self.ingest_guardian_review(*input, out);
                }
                CustomAnalyticsFact::TurnResolvedConfig(input) => {
                    self.ingest_turn_resolved_config(*input, out).await;
                }
                CustomAnalyticsFact::TurnTokenUsage(input) => {
                    self.ingest_turn_token_usage(*input, out).await;
                }
                CustomAnalyticsFact::TurnProfile(input) => {
                    self.ingest_turn_profile(*input, out).await;
                }
                CustomAnalyticsFact::TurnCodexError(input) => {
                    self.ingest_turn_codex_error(*input);
                }
                CustomAnalyticsFact::ImagePreparation(input) => {
                    self.ingest_image_preparation(*input);
                }
                CustomAnalyticsFact::SkillInvoked(input) => {
                    self.ingest_skill_invoked(input, out).await;
                }
                CustomAnalyticsFact::AppMentioned(input) => {
                    self.ingest_app_mentioned(input, out);
                }
                CustomAnalyticsFact::AppUsed(input) => {
                    self.ingest_app_used(input, out);
                }
                CustomAnalyticsFact::McpToolCallElicitation(input) => {
                    let McpToolCallElicitation {
                        thread_id,
                        turn_id,
                        item_id,
                        elicitation_type,
                    } = input;
                    let key = ToolItemKey {
                        thread_id,
                        turn_id,
                        item_id,
                    };
                    if self
                        .pending_mcp_tool_elicitations
                        .iter()
                        .any(|(pending, _)| pending == &key)
                    {
                        return;
                    }
                    if self.pending_mcp_tool_elicitations.len() >= MAX_TOOL_RESPONSE_ENTRIES {
                        self.pending_mcp_tool_elicitations.pop_front();
                        tracing::warn!(
                            "expiring oldest MCP elicitation classification: state is full"
                        );
                    }
                    self.pending_mcp_tool_elicitations
                        .push_back((key, elicitation_type));
                }
                CustomAnalyticsFact::HookRun(input) => {
                    self.ingest_hook_run(input, out);
                }
                CustomAnalyticsFact::PluginUsed(input) => {
                    self.ingest_plugin_used(input, out);
                }
                CustomAnalyticsFact::PluginInstallRequested(input) => {
                    self.ingest_plugin_install_requested(input, out);
                }
                CustomAnalyticsFact::PluginStateChanged(input) => {
                    self.ingest_plugin_state_changed(input, out);
                }
                CustomAnalyticsFact::PluginInstallFailed(input) => {
                    self.ingest_plugin_install_failed(input, out);
                }
                CustomAnalyticsFact::PluginMeasurements(input) => {
                    self.ingest_plugin_measurements(input, out);
                }
                CustomAnalyticsFact::ExternalAgentConfigImportCompleted(input) => {
                    self.ingest_external_agent_config_import_completed(input, out);
                }
                CustomAnalyticsFact::ExternalAgentConfigImportFailure(input) => {
                    self.ingest_external_agent_config_import_failure(input, out);
                }
            },
        }
    }

    fn ingest_app_mentioned(&mut self, input: AppMentionedInput, out: &mut Vec<TrackEventRequest>) {
        let AppMentionedInput { tracking, mentions } = input;
        out.extend(mentions.into_iter().map(|mention| {
            let event_params = codex_app_metadata(&tracking, mention);
            TrackEventRequest::AppMentioned(CodexAppMentionedEventRequest {
                event_type: "codex_app_mentioned",
                event_params,
            })
        }));
    }

    fn ingest_app_used(&mut self, input: AppUsedInput, out: &mut Vec<TrackEventRequest>) {
        let AppUsedInput {
            tracking,
            app,
            elicitation_type,
        } = input;
        let event_params = CodexAppUsedMetadata {
            app: codex_app_metadata(&tracking, app),
            voice_session_id: self
                .turns
                .get(&tracking.turn_id)
                .and_then(|turn| turn.voice_session_id.clone()),
            elicitation_type,
        };
        out.push(TrackEventRequest::AppUsed(CodexAppUsedEventRequest {
            event_type: "codex_app_used",
            event_params,
        }));
    }

    fn ingest_hook_run(&mut self, input: HookRunInput, out: &mut Vec<TrackEventRequest>) {
        let HookRunInput { tracking, hook } = input;
        out.push(TrackEventRequest::HookRun(CodexHookRunEventRequest {
            event_type: "codex_hook_run",
            event_params: codex_hook_run_metadata(&tracking, hook),
        }));
    }

    fn ingest_plugin_used(&mut self, input: PluginUsedInput, out: &mut Vec<TrackEventRequest>) {
        let PluginUsedInput { tracking, plugin } = input;
        out.push(TrackEventRequest::PluginUsed(CodexPluginUsedEventRequest {
            event_type: "codex_plugin_used",
            event_params: codex_plugin_used_metadata(&tracking, plugin),
        }));
    }

    fn ingest_compaction(&mut self, input: CodexCompactionEvent, out: &mut Vec<TrackEventRequest>) {
        let Some((connection_state, thread_state, thread_metadata)) =
            self.thread_context_or_warn(AnalyticsDropSite::compaction(&input))
        else {
            return;
        };
        let voice_session_id = thread_state
            .voice_session_id_at(input.started_at)
            .or_else(|| {
                self.turns
                    .get(&input.turn_id)
                    .and_then(|turn| turn.voice_session_id.clone())
            });
        let mut event_params = codex_compaction_event_params(
            input,
            thread_metadata.session_id.clone(),
            thread_state.app_server_client(connection_state),
            connection_state.runtime.clone(),
            thread_metadata.thread_source.clone(),
            thread_metadata.subagent_source.clone(),
            thread_metadata.parent_thread_id.clone(),
        );
        event_params.voice_session_id = voice_session_id;
        out.push(TrackEventRequest::Compaction(Box::new(
            CodexCompactionEventRequest {
                event_type: "codex_compaction_event",
                event_params,
            },
        )));
    }

    async fn maybe_emit_turn_event(&mut self, turn_id: &str, out: &mut Vec<TrackEventRequest>) {
        let Some(turn_state) = self.turns.get(turn_id) else {
            return;
        };
        if turn_state.turn_event_emitted {
            return;
        }
        if turn_state.thread_id.is_none()
            || turn_state.num_input_images.is_none()
            || turn_state.resolved_config.is_none()
            || turn_state.profile.is_none()
            || turn_state.completed.is_none()
        {
            return;
        }
        let Some(thread_id) = turn_state.thread_id.as_ref() else {
            return;
        };
        let drop_site = AnalyticsDropSite::turn(thread_id, turn_id);
        let connection_id = turn_state
            .connection_id
            .or_else(|| self.thread_connection_id(drop_site.thread_id));
        let Some(connection_id) = connection_id else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadConnection);
            return;
        };
        let Some(connection_state) = self.connections.get(&connection_id) else {
            warn_missing_analytics_context(
                &drop_site,
                MissingAnalyticsContext::Connection { connection_id },
            );
            return;
        };
        let Some(thread_state) = self.threads.get(drop_site.thread_id) else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadMetadata);
            return;
        };
        let Some(thread_metadata) = thread_state.metadata.as_ref() else {
            warn_missing_analytics_context(&drop_site, MissingAnalyticsContext::ThreadMetadata);
            return;
        };
        let turn_event = TrackEventRequest::TurnEvent(Box::new(CodexTurnEventRequest {
            event_type: "codex_turn_event",
            event_params: codex_turn_event_params(
                thread_state.app_server_client(connection_state),
                connection_state.runtime.clone(),
                turn_id.to_string(),
                turn_state,
                thread_metadata,
            ),
        }));
        let accepted_line_event = accepted_line_event_input(turn_id, turn_state);

        out.push(turn_event);
        if let Some((mut input, cwd)) = accepted_line_event {
            input.repo_hash = accepted_line_repo_hash_for_cwd(cwd.as_path()).await;
            out.extend(accepted_line_fingerprint_event_requests(input));
        }
        if self.has_pending_tool_items_for_turn(turn_id) {
            if let Some(turn_state) = self.turns.get_mut(turn_id) {
                turn_state.turn_event_emitted = true;
            }
        } else {
            self.turns.remove(turn_id);
        }
    }

    fn has_pending_tool_items_for_turn(&self, turn_id: &str) -> bool {
        self.tool_items_started_at_ms
            .keys()
            .any(|key| key.turn_id == turn_id)
    }
}

#[derive(Default)]
struct ToolResponseAttribution {
    tool_event_type: Option<ToolEventType>,
    cell_id: Option<String>,
    parent_call_id: Option<String>,
    originating_response_id: Option<String>,
}

struct ToolItemEventInput<'a> {
    thread_id: &'a str,
    turn_id: &'a str,
    item: &'a ThreadItemPayload,
    model_context: Option<&'a ModelInvocationContext>,
    started_at_ms: u64,
    completed_at_ms: u64,
    connection_state: &'a ConnectionState,
    thread_state: &'a ThreadAnalyticsState,
    thread_metadata: &'a ThreadMetadataState,
    review_summary: Option<&'a ItemReviewSummary>,
    elicitation_type: Option<ElicitationType>,
}

struct ToolItemOutcome {
    terminal_status: ToolItemTerminalStatus,
    failure_kind: Option<ToolItemFailureKind>,
    execution_duration_ms: Option<u64>,
}

#[derive(Default)]
struct CommandActionCounts {
    total: u64,
    read: u64,
    list_files: u64,
    search: u64,
    unknown: u64,
}

#[derive(Clone, Copy)]
struct ToolItemContext<'a> {
    started_at_ms: u64,
    completed_at_ms: u64,
    connection_state: &'a ConnectionState,
    thread_state: &'a ThreadAnalyticsState,
    thread_metadata: &'a ThreadMetadataState,
    review_summary: Option<&'a ItemReviewSummary>,
}

#[derive(Default)]
struct FileChangeCounts {
    add: u64,
    update: u64,
    delete: u64,
    move_: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DynamicContentCounts {
    total: u64,
    text: u64,
    image: u64,
    audio: u64,
}

fn dynamic_content_counts(items: &[DynamicToolCallOutputContentItem]) -> DynamicContentCounts {
    let mut text = 0;
    let mut image = 0;
    let mut audio = 0;
    for item in items {
        match item {
            DynamicToolCallOutputContentItem::InputText { .. } => text += 1,
            DynamicToolCallOutputContentItem::InputImage { .. } => image += 1,
            DynamicToolCallOutputContentItem::InputAudio { .. } => audio += 1,
        }
    }
    DynamicContentCounts {
        total: usize_to_u64(items.len()),
        text,
        image,
        audio,
    }
}

fn sandbox_policy_mode(permission_profile: &PermissionProfile, cwd: &Path) -> &'static str {
    match permission_profile {
        PermissionProfile::Disabled => "full_access",
        PermissionProfile::External { .. } => "external_sandbox",
        PermissionProfile::Managed { .. } => {
            let file_system_policy = permission_profile.file_system_sandbox_policy();
            if file_system_policy.has_full_disk_write_access() {
                if permission_profile.network_sandbox_policy().is_enabled() {
                    "full_access"
                } else {
                    "external_sandbox"
                }
            } else if file_system_policy
                .get_writable_roots_with_cwd(cwd)
                .is_empty()
            {
                "read_only"
            } else {
                "workspace_write"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::ItemCompletedNotification;
    use codex_app_server_protocol::JSONRPCErrorError;
    use codex_app_server_protocol::ThreadClosedNotification;
    use codex_protocol::models::SandboxEnforcement;
    use codex_protocol::permissions::FileSystemSandboxPolicy;
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn pending_classifications_evict_oldest_and_clean_up() {
        let mut reducer = AnalyticsReducer::default();
        let mut events = Vec::new();
        let keys = (0..=MAX_TOOL_RESPONSE_ENTRIES)
            .map(|index| ToolItemKey {
                thread_id: if index == MAX_TOOL_RESPONSE_ENTRIES {
                    "other-thread"
                } else {
                    "thread"
                }
                .to_string(),
                turn_id: "turn".to_string(),
                item_id: index.to_string(),
            })
            .collect::<Vec<_>>();
        let assert_pending = |reducer: &AnalyticsReducer, keys: &[ToolItemKey]| {
            let expected = keys
                .iter()
                .cloned()
                .map(|key| (key, ElicitationType::AuthOrLink))
                .collect::<VecDeque<_>>();
            assert!(reducer.pending_mcp_tool_elicitations == expected);
        };
        for key in &keys {
            reducer
                .ingest(
                    AnalyticsFact::Custom(CustomAnalyticsFact::McpToolCallElicitation(
                        McpToolCallElicitation {
                            thread_id: key.thread_id.clone(),
                            turn_id: key.turn_id.clone(),
                            item_id: key.item_id.clone(),
                            elicitation_type: ElicitationType::AuthOrLink,
                        },
                    )),
                    &mut events,
                )
                .await;
        }
        assert_pending(&reducer, &keys[1..]);

        // A duplicate at capacity keeps the first value and original arrival order.
        reducer
            .ingest(
                AnalyticsFact::Custom(CustomAnalyticsFact::McpToolCallElicitation(
                    McpToolCallElicitation {
                        thread_id: keys[1].thread_id.clone(),
                        turn_id: keys[1].turn_id.clone(),
                        item_id: keys[1].item_id.clone(),
                        elicitation_type: ElicitationType::Approval,
                    },
                )),
                &mut events,
            )
            .await;
        assert_pending(&reducer, &keys[1..]);

        // Out-of-order completion consumes its marker even without turn context.
        reducer
            .ingest(
                AnalyticsFact::Notification(Box::new(ServerNotification::ItemCompleted(
                    ItemCompletedNotification::new(
                        keys[2].thread_id.clone(),
                        keys[2].turn_id.clone(),
                        ThreadItemPayload::McpToolCall {
                            id: keys[2].item_id.clone(),
                            server: "server".to_string(),
                            tool: "tool".to_string(),
                            status: McpToolCallStatus::Failed,
                            arguments: serde_json::json!({}),
                            app_context: None,
                            mcp_app_resource_uri: None,
                            mcp_app_ui: None,
                            plugin_id: None,
                            read_only_hint: None,
                            result: None,
                            error: None,
                            duration_ms: None,
                        },
                        /*started_at_ms*/ None,
                        /*completed_at_ms*/ 1_000,
                    ),
                ))),
                &mut events,
            )
            .await;
        let mut remaining = keys[1..].to_vec();
        remaining.remove(1);
        assert_pending(&reducer, &remaining);

        reducer
            .ingest(
                AnalyticsFact::Notification(Box::new(ServerNotification::ThreadClosed(
                    ThreadClosedNotification {
                        thread_id: "thread".to_string(),
                    },
                ))),
                &mut events,
            )
            .await;
        assert_pending(&reducer, &keys[MAX_TOOL_RESPONSE_ENTRIES..]);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn rejected_turn_interrupt_removes_pending_analytics_request() {
        let mut reducer = AnalyticsReducer::default();
        let mut out = Vec::new();
        let connection_id = 7;
        let request_id = RequestId::Integer(4);
        let request_key = (connection_id, request_id.clone());

        reducer
            .ingest(
                AnalyticsFact::ExplicitClientInterruptRequest {
                    connection_id,
                    request_id: request_id.clone(),
                    turn_id: "turn-2".to_string(),
                    requested_at_ms: 1716000000123,
                },
                &mut out,
            )
            .await;

        assert!(reducer.requests.contains_key(&request_key));

        reducer
            .ingest(
                AnalyticsFact::ErrorResponse {
                    connection_id,
                    request_id,
                    error: JSONRPCErrorError {
                        code: -32600,
                        message: "no active turn to interrupt".to_string(),
                        data: None,
                    },
                    error_type: None,
                },
                &mut out,
            )
            .await;

        assert!(!reducer.requests.contains_key(&request_key));
        assert!(out.is_empty());
    }

    #[test]
    fn managed_full_disk_with_restricted_network_reports_external_sandbox() {
        let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::Managed,
            &FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Restricted,
        );

        assert_eq!(
            sandbox_policy_mode(&permission_profile, Path::new("/")),
            "external_sandbox"
        );
    }

    #[test]
    fn guardian_review_result_maps_terminal_statuses() {
        assert!(guardian_review_result(GuardianApprovalReviewStatus::InProgress).is_none());
        assert!(matches!(
            guardian_review_result(GuardianApprovalReviewStatus::TimedOut),
            Some((ReviewStatus::TimedOut, ReviewResolution::None))
        ));
    }

    #[test]
    fn dynamic_content_counts_include_audio() {
        let items = vec![
            DynamicToolCallOutputContentItem::InputText {
                text: "ok".to_string(),
            },
            DynamicToolCallOutputContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
            },
            DynamicToolCallOutputContentItem::InputAudio {
                audio_url: "data:audio/wav;base64,YXVkaW8=".to_string(),
            },
        ];

        assert_eq!(
            dynamic_content_counts(&items),
            DynamicContentCounts {
                total: 3,
                text: 1,
                image: 1,
                audio: 1,
            }
        );
    }

    #[test]
    fn command_execution_script_paths_reject_unsafe_values() {
        assert_eq!(
            safe_plugin_relative_script_path(
                Some("sample@openai-curated"),
                Some("/home/user/.codex/plugins/cache/openai-curated/sample/scripts/run.py"),
            ),
            None
        );
        assert_eq!(
            safe_plugin_relative_script_path(Some("sample@openai-curated"), Some("scripts/run.py"),),
            Some("scripts/run.py".to_string())
        );
        assert_eq!(
            safe_plugin_relative_script_path(/*plugin_id*/ None, Some("scripts/run.py"),),
            None
        );
    }
}
