pub(crate) mod startup;

use crate::PromptCacheKey;
use crate::WithTurnExtensionData;
use crate::runtime_auth::RuntimeAuthSlot;
use codex_login::AuthOwner;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::RestrictRootCapabilities;
use crate::agent::AgentStatus;
use crate::agent::agent_status_from_event;
use crate::agent::api::AgentConfigUpdate;
use crate::agent::api::AgentTurnOutcome;
use crate::agent::control::AgentControlInit;
use crate::agent::status::is_final;
use crate::agents_md_manager::SessionInstructions;
use crate::attestation::AttestationProvider;
use crate::compact;
use crate::compact::CompactedHistoryMetadata;
use crate::config::ManagedFeatures;
use crate::context::ContextualUserFragment;
use crate::context::DeveloperInstructions;
use crate::context::GuardianPolicy;
use crate::context::ManagedDeveloperInstructions;
use crate::context::ModelSwitchInstructions;
use crate::context::MultiAgentRoleInstructions;
use crate::context::NetworkRuleSaved;
use crate::context::RecommendedPluginsInstructions;
use crate::context::world_state::Placement;
use crate::context::world_state::WorldState;
use crate::context::world_state::WorldStateSnapshot;
use crate::context::world_state::WorldStateUpdate;
use crate::context::world_state::WorldStateUpdateContent;
use crate::context::world_state::split_prefix_updates;
use crate::current_time::TimeProvider;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::exec_policy::BANNED_PREFIX_SUGGESTIONS;
use crate::exec_policy::ExecPolicyManager;
use crate::exec_policy::default_policy_path;
use crate::image_preparation::ImagePreparationMode;
use crate::image_preparation::ImageResizeNoticeMode;
use crate::image_preparation::prepare_response_items as prepare_image_response_items;
use crate::image_preparation::unified_image_budget_enabled;
use crate::parse_turn_item;
use crate::realtime_conversation::RealtimeConversationManager;
use crate::realtime_history::RealtimeEventOrder;
use crate::session::step_context::StepContext;
use crate::session::step_settings::ResolvedStepSettings;
use crate::session::step_settings::StepSettings;
use crate::session::turn_context::TurnEnvironment;
use crate::shell_snapshot::SnapshotCredentialBrokerState;
use crate::stream_events_utils::mark_thread_memory_mode_polluted_if_external_context;
use crate::turn_metadata::TurnMetadataState;
use crate::turn_timing::now_unix_timestamp_ms;
use async_channel::Receiver;
use async_channel::Sender;
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_analytics::AnalyticsEventsClient;
use codex_analytics::ImagePreparationFact;
use codex_analytics::ImagePreparationMetadata;
use codex_analytics::SubAgentThreadStartedInput;
use codex_analytics::ThreadInitializationMode;
use codex_analytics::TurnCodexErrorFact;
use codex_async_utils::OrCancelExt;
use codex_attachment_store::AttachmentStore;
use codex_attachment_store::InlineAttachmentStore;
use codex_connectors::connector_runtime_context_key;
use codex_context_fragments::RenderedFragment;
use codex_exec_server::Environment;
use codex_exec_server::EnvironmentManager;
use codex_execpolicy::prefix_rule_migration;
use codex_extension_api::ConversationHistorySnapshot;
use codex_extension_api::ExtensionDataInit;
use codex_extension_api::PromptSlot;
use codex_extension_api::TurnContextContributionInput;
use codex_features::FEATURES;
use codex_features::Feature;
use codex_features::Features;
use codex_features::unstable_features_warning_event;
use codex_history::HistoryInitialization;
use codex_history::RolloutItem;
use codex_hooks::Hooks;
use codex_hooks::HooksConfig;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::auth_env_telemetry::collect_auth_env_telemetry;
use codex_mcp::McpResourceClient;
use codex_mcp::McpRuntime;
use codex_mcp::McpRuntimeContext;
use codex_mcp::McpRuntimeInput;
use codex_models_manager::manager::ModelsManager;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_network_proxy::NetworkProxy;
use codex_network_proxy::NetworkProxyAuditMetadata;
use codex_network_proxy::normalize_host;
use codex_otel::auth_storage::AuthStorageOriginator;
use codex_otel::current_span_trace_id;
use codex_otel::current_span_w3c_trace_context;
use codex_otel::set_parent_from_w3c_trace_context;
use codex_prompts::ResolvedModelMessages;
use codex_prompts::render_model_instructions;
use codex_protocol::ResponseUsageMetadata;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::approvals::ElicitationRequest;
use codex_protocol::approvals::ElicitationRequestEvent;
use codex_protocol::approvals::ExecPolicyAmendment;
use codex_protocol::approvals::NetworkPolicyAmendment;
use codex_protocol::approvals::NetworkPolicyRuleAction;
use codex_protocol::capabilities::SelectedCapabilityRoot;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::items::EnteredReviewModeItem;
use codex_protocol::items::ModelInvocationContext;
use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::AdditionalPermissionProfile;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::ContentItemNamespace;
use codex_protocol::models::ImageReference;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::SandboxEnforcement;
use codex_protocol::openai_models::CatalogModel;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSandboxPolicyContext;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::HasLegacyEvent;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::PrivateSessionEvent;
use codex_protocol::protocol::PrivateSessionEventPayload;
use codex_protocol::protocol::RawResponseItemEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadSettingsAppliedEvent;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::ThreadSettingsSnapshot;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnContextItem;
use codex_protocol::protocol::TurnEnvironmentRequest;
use codex_protocol::protocol::TurnEnvironmentSelection;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::protocol::WorldStateItem;
use codex_protocol::request_permissions::PermissionGrantScope;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::request_permissions::RequestPermissionsEvent;
use codex_protocol::request_permissions::RequestPermissionsResponse;
use codex_protocol::request_user_input::RequestUserInputArgs;
use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_rmcp_client::ElicitationResponse;
use codex_rollout::should_persist_response_item;
use codex_rollout::state_db;
use codex_rollout_trace::ThreadStartedTraceMetadata;
use codex_rollout_trace::ThreadTraceContext;
use codex_sandboxing::SandboxType;
use codex_sandboxing::policy_transforms::intersect_permission_profiles_with_context;
use codex_shell_command::parse_command::parse_command;
use codex_terminal_detection::user_agent;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::LiveThread;
use codex_thread_store::LiveThreadInitGuard;
use codex_thread_store::LocalThreadStore;
use codex_thread_store::PersistContext;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::ThreadPersistenceMetadata;
use codex_thread_store::ThreadStore;
use codex_thread_store::ThreadStoreResumePreparation;
use codex_utils_audio::prepare_response_items as prepare_audio_response_items;
use codex_utils_git_discovery::GitRootDiscovery;
use codex_utils_output_truncation::with_serialization_allowance;
use codex_utils_path_uri::PathUri;
use futures::future::BoxFuture;
use futures::future::Shared;
use futures::prelude::*;
use rmcp::model::RequestId;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toml::Value as TomlValue;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::info_span;
use tracing::instrument;
use tracing::warn;
use uuid::Uuid;

use crate::client::ModelClient;
use crate::codex_thread::CodexThreadSettingsOverrides;
use crate::codex_thread::ThreadConfigSnapshot;
#[cfg(test)]
use crate::compact::collect_user_messages;
use crate::config::Config;
use crate::config::ConstraintResult;
use crate::config::PermissionProfileSnapshot;
use crate::config::PermissionProfileState;
use crate::config::StartedNetworkProxy;
use crate::config::resolve_web_search_mode_for_turn;
use crate::context_manager::ContextManager;
use crate::context_manager::HistoryReplacement;
use crate::thread_rollout_truncation::initial_history_has_prior_user_turns;
use codex_config::CONFIG_TOML_FILE;
use codex_config::ConfigLayerSource;
use codex_config::types::McpServerConfig;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::ResponseIncompleteError;
use codex_protocol::error::Result as CodexResult;
#[cfg(test)]
use codex_protocol::exec_output::StreamOutput;

pub(crate) mod async_compaction;
pub(crate) mod file_audio;
pub(crate) use file_audio::FileAudioDurations;

mod code_mode_warning;
mod config_refresh;
pub(crate) mod context_window;
mod daemon_recovery;
mod environment;
mod extension_interruption;
pub(crate) mod extension_metrics;
mod guardian_checkpoint;
mod handlers;
pub(crate) mod inject;
mod reasoning_effort;
mod submission;
pub(crate) use reasoning_effort::RequestEffortUsage;
pub(crate) use submission::Submission;
mod approvals;
mod conversation_history;
mod event_emission;
mod initial_history;
mod input_queue;
mod mcp;
mod mcp_prewarm;
mod mcp_refresh;
mod mcp_runtime;
mod model_catalog_settings;
pub(crate) mod multi_agents;
mod plugin_selection;
mod realtime_history;
mod retained_context;
mod review;
mod rollout_budget;
mod rollout_reconstruction;
mod runtime_state;
#[allow(clippy::module_inception)]
pub(crate) mod session;
mod session_settings;
mod spawn;
pub(crate) mod startup_prewarm;
mod step_activation;
mod step_capture;
pub(crate) mod step_context;
pub(crate) mod step_settings;
mod thread_settings;
pub(crate) mod time_reminder;
mod token_accounting;
mod token_budget;
pub(crate) mod turn;
pub(crate) mod turn_context;
mod turn_input;
mod turn_suspension;
mod world_state;
use self::code_mode_warning::unsupported_code_mode_warning;
pub(crate) use self::environment::ThreadEnvironmentDefaults;
#[cfg(test)]
use self::handlers::submission_dispatch_span;
use self::handlers::submission_loop;
pub(crate) use self::input_queue::InputQueueActivity;
pub(crate) use self::input_queue::TurnInput;
pub(crate) use self::input_queue::TurnInputQueue;
pub(crate) use self::input_queue::UserInputMetadata;
use self::review::spawn_review_thread;
use self::session::AppServerClientMetadata;
use self::session::Session;
use self::session::SessionConfiguration;
use self::session::SessionSettingsCommit;
pub(crate) use self::session::SessionSettingsUpdate;
#[cfg(test)]
pub(crate) use self::spawn::completed_session_loop_termination;
pub(crate) use self::spawn::get_service_tier;
pub(crate) use self::spawn::new_submission_id;
#[cfg(test)]
pub(crate) use self::spawn::session_loop_termination_from_handle;
#[cfg(test)]
use self::turn::AssistantMessageStreamParsers;
use self::turn::RealtimeEventText;
use self::turn::agent_message_text;
#[cfg(test)]
use self::turn::collect_explicit_app_ids_from_skill_items;
use self::turn::realtime_text_for_event;
use self::turn_context::TurnContext;
#[cfg(test)]
mod rollout_reconstruction_tests;

use crate::exec_policy::ExecPolicyUpdateError;
use crate::guardian::GuardianReviewSessionManager;
use crate::mcp::McpEnvironmentScope;
use crate::mcp::McpManager;
use crate::mcp::McpThreadIdentity;
use crate::rollout::map_session_init_error;
use crate::session::startup_prewarm::SessionStartupPrewarmHandle;
use crate::shell;
use crate::state::AcceptedUserInputResponse;
use crate::state::AutoCompactWindowIds;
use crate::state::AutoCompactWindowSnapshot;
use crate::state::PendingApproval;
use crate::state::PendingRequestPermissions;
use crate::state::ReasoningEffortPin;
use crate::state::SessionServices;
use crate::state::SessionState;
use crate::state::TaskKind;
#[cfg(test)]
use crate::stream_events_utils::HandleOutputCtx;
#[cfg(test)]
use crate::stream_events_utils::handle_output_item_done;
use crate::tasks::ReviewTask;
use crate::tools::ApprovalContext;
use crate::tools::handlers::PreparedPermissionRequest;
use crate::tools::network_approval::NetworkApprovalService;
use crate::tools::network_approval::build_blocked_request_observer;
use crate::tools::network_approval::build_network_policy_decider;
#[cfg(test)]
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::sandboxing::ApprovalStore;
use crate::turn_timing::TurnTimingState;
use crate::turn_timing::record_turn_ttfm_metric;
use crate::unified_exec::UnifiedExecProcessManager;
use crate::windows_sandbox::WindowsSandboxLevelExt;
use crate::windows_sandbox::local_binding_policy_for_sandbox;
use crate::windows_sandbox::managed_proxy_routing_for_windows_sandbox;
use codex_core_plugins::PluginCommandAttribution;
use codex_core_plugins::PluginsManager;
use codex_core_plugins::RecommendedPluginCandidatesInput;
use codex_git_utils::get_git_repo_root;
use codex_history::CodexHarnessMetadata;
use codex_history::CompactedItem;
use codex_history::CompactionResumeMetadata;
use codex_history::DynamicToolsSnapshot;
use codex_history::InitialHistory;
pub(crate) use codex_history::PreviousTurnSettings;
use codex_history::ResponseItemEnvelope;
use codex_mcp::McpConfig;
use codex_mcp::effective_mcp_servers;
use codex_otel::SessionTelemetry;
use codex_otel::THREAD_STARTED_METRIC;
use codex_otel::TelemetryAuthMode;
use codex_protocol::ContextWindowId;
use codex_protocol::ResponseItemId;
use codex_protocol::approvals::ExecApprovalKind;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::Settings;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::mcp::OPENAI_STANDARD_FORM_INPUT_EXTENSION_ID;
use codex_protocol::models::LocalImagePreparation;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::DeprecationNoticeEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::ModelRerouteEvent;
use codex_protocol::protocol::ModelRerouteReason;
use codex_protocol::protocol::ModelVerification;
use codex_protocol::protocol::ModelVerificationEvent;
use codex_protocol::protocol::NetworkApprovalContext;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::RequestUserInputEvent;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionConfiguredEvent;
use codex_protocol::protocol::SessionNetworkProxyRuntime;
use codex_protocol::protocol::StreamErrorEvent;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_protocol::protocol::TokenCountEvent;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::protocol::TokenUsageRecord;
use codex_protocol::protocol::TurnModerationMetadataEvent;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::turn_input::TurnInputMode;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::turn_input::TurnStartOptions;
use codex_protocol::user_input::UserInput;
use codex_skills_extension::HostSkillsService;
use codex_tools::ToolName;
use codex_utils_absolute_path::AbsolutePathBuf;
#[cfg(test)]
use codex_utils_stream_parser::ProposedPlanSegment;

/// Whether reported usage updates the installed context baseline or only spending.
#[derive(Clone, Copy)]
pub(crate) enum TokenUsageContext {
    Sampling {
        includes_reasoning: bool,
        /// Input history length before the response, including requests with no output items.
        input_item_count: usize,
    },
    SpendingOnly,
}

/// Which active turn can service a network approval.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkApprovalTurn {
    Originating,
    AnyActive,
}

/// Queue and lifecycle endpoints for a running [`Session`].
///
/// Runtime state lives on `Session`; keeping these endpoints separate lets all
/// submission senders be dropped to terminate the session loop. The shared
/// completion future observes that shutdown.
#[derive(Clone)]
pub(crate) struct SessionIo {
    pub(crate) tx_sub: Sender<Submission>,
    pub(crate) rx_event: Receiver<Event>,
    // Last known status of the agent.
    pub(crate) agent_status: watch::Receiver<AgentStatus>,
    // Shared future for the background submission loop completion so multiple
    // callers can wait for shutdown.
    pub(crate) session_loop_termination: SessionLoopTermination,
}

pub(crate) type SessionLoopTermination = Shared<BoxFuture<'static, ()>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitEnrichmentPolicy {
    Fresh,
    Skip,
}

/// Controls which fork history belongs in the newly created thread's own rollout.
pub(crate) enum ForkPersistence {
    Copied,
    /// The spawn caller owns the durability barrier before acknowledging the child.
    CopiedDeferred,
    Referenced {
        history_base: Option<HistoryPosition>,
        inherited_item_count: usize,
    },
}

/// Normal appends keep this generation; compaction and rollback start a new one.
#[derive(Clone)]
pub(crate) struct CompactionPrefix {
    pub(crate) generation: u64,
    pub(crate) len: usize,
    pub(crate) auth_owner: Option<AuthOwner>,
}

pub(crate) enum CompactedHistoryInstallation<'a> {
    Complete(CompactedHistoryMetadata),
    Prefix {
        prefix: CompactionPrefix,
        prefix_context: Vec<ResponseItem>,
        initial_context: Vec<ResponseItemEnvelope>,
        reset_world_state_sections: Vec<&'static str>,
        model_info: &'a ModelInfo,
        compaction_response_id: String,
        reviewer_compaction_hash: Option<String>,
        turn_context: &'a TurnContext,
        started_item: Option<&'a TurnItem>,
    },
}

impl From<CompactedHistoryMetadata> for CompactedHistoryInstallation<'_> {
    fn from(metadata: CompactedHistoryMetadata) -> Self {
        Self::Complete(metadata)
    }
}

pub(crate) struct SessionSpawnArgs {
    pub(crate) attached_thread: bool,
    pub(crate) startup: Option<Arc<startup::SessionStartup>>,
    pub(crate) config: Config,
    pub(crate) prompt_cache_key: PromptCacheKey,
    pub(crate) allow_provider_model_fallback: bool,
    pub(crate) instructions: SessionInstructions,
    pub(crate) installation_id: String,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) runtime_auth: RuntimeAuthSlot,
    pub(crate) models_manager: SharedModelsManager,
    pub(crate) git_root_discovery: Arc<GitRootDiscovery>,
    pub(crate) environment_manager: Arc<EnvironmentManager>,
    pub(crate) skills_service: Arc<HostSkillsService>,
    pub(crate) plugins_manager: Arc<PluginsManager>,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) code_mode_session_provider: Arc<dyn codex_code_mode::CodeModeSessionProvider>,
    pub(crate) extensions: Arc<codex_extension_api::ExtensionRegistry<crate::config::Config>>,
    pub(crate) conversation_history: InitialHistory,
    pub(crate) resume_preparation: Option<ThreadStoreResumePreparation>,
    pub(crate) history_initialization: HistoryInitialization,
    pub(crate) disabled_plugin_ids: Option<Vec<String>>,
    pub(crate) requested_history_mode: Option<ThreadHistoryMode>,
    pub(crate) fork_persistence: ForkPersistence,
    pub(crate) session_source: SessionSource,
    pub(crate) forked_from_thread_id: Option<ThreadId>,
    pub(crate) parent_thread_id: Option<ThreadId>,
    pub(crate) thread_source: Option<ThreadSource>,
    pub(crate) originator: String,
    pub(crate) agent_control: AgentControlInit,
    pub(crate) dynamic_tools: Vec<DynamicToolSpec>,
    pub(crate) metrics_service_name: Option<String>,
    pub(crate) inherited_exec_policy: Option<Arc<ExecPolicyManager>>,
    pub(crate) inherited_environments: Option<TurnEnvironmentSnapshot>,
    /// Parent rollout trace used only to derive fresh spawned child traces.
    ///
    /// Root sessions and non-thread-spawn subagents pass a disabled context;
    /// `Session::new` creates the root trace itself when rollout tracing is enabled.
    pub(crate) parent_rollout_thread_trace: ThreadTraceContext,
    pub(crate) user_shell_override: Option<shell::Shell>,
    pub(crate) parent_trace: Option<W3cTraceContext>,
    pub(crate) environment_requests: Vec<TurnEnvironmentRequest>,
    pub(crate) thread_extension_init: ExtensionDataInit,
    pub(crate) turn_extension_init: ExtensionDataInit,
    pub(crate) client_mcp_extensions: ClientMcpExtensions,
    pub(crate) reserved_thread_id: Option<ThreadId>,
    pub(crate) analytics_events_client: Option<AnalyticsEventsClient>,
    pub(crate) image_store: Arc<dyn AttachmentStore>,
    pub(crate) thread_store: Arc<dyn ThreadStore>,
    pub(crate) attestation_provider: Option<Arc<dyn AttestationProvider>>,
    pub(crate) external_time_provider: Option<Arc<dyn TimeProvider>>,
    pub(crate) inherited_multi_agent_version: Option<MultiAgentVersion>,
    pub(crate) git_enrichment_policy: GitEnrichmentPolicy,
    pub(crate) windows_sandbox_proxy_settings_mode:
        codex_sandboxing::WindowsSandboxProxySettingsMode,
}

pub(crate) fn resolve_multi_agent_version(
    conversation_history: &InitialHistory,
    inherited_multi_agent_version: Option<MultiAgentVersion>,
) -> Option<MultiAgentVersion> {
    if inherited_multi_agent_version == Some(MultiAgentVersion::Disabled) {
        return Some(MultiAgentVersion::Disabled);
    }

    conversation_history
        .get_multi_agent_version()
        .or(inherited_multi_agent_version)
        .or(match conversation_history {
            InitialHistory::New | InitialHistory::Cleared => None,
            // Threads created before runtime metadata existed keep the legacy V1 tool surface.
            InitialHistory::Resumed(_) | InitialHistory::Forked(_) => Some(MultiAgentVersion::V1),
        })
}

pub(crate) const INITIAL_SUBMIT_ID: &str = "";
pub(crate) const SUBMISSION_CHANNEL_CAPACITY: usize = 512;
const CYBER_VERIFY_URL: &str = "https://chatgpt.com/cyber";
const CYBER_SAFETY_URL: &str = "https://developers.openai.com/codex/concepts/cyber-safety";

pub(crate) fn assign_missing_item_id(
    item: &mut ResponseItem,
    item_id_generator: &codex_protocol::ItemIdGenerator,
) {
    if item.id().is_none_or(|id| id.is_empty())
        && let Some(prefix) = item.id_prefix()
    {
        item.set_id(Some(item_id_generator(prefix)));
    }
}

impl Session {
    #[tracing::instrument(level = "trace", skip_all, fields(item_count = items.len()))]
    async fn send_raw_response_items(&self, turn_context: &TurnContext, items: &[ResponseItem]) {
        for item in items {
            self.send_event(
                turn_context,
                EventMsg::RawResponseItem(RawResponseItemEvent { item: item.clone() }),
            )
            .await;
        }
    }
}

async fn thread_title_from_thread_store(
    live_thread: Option<&LiveThread>,
    thread_store: &Arc<dyn ThreadStore>,
    conversation_id: ThreadId,
) -> Option<String> {
    let thread = match live_thread {
        Some(live_thread) => {
            live_thread
                .read_thread(
                    /*include_archived*/ true, /*include_history*/ false,
                )
                .await
        }
        None => {
            thread_store
                .read_thread(ReadThreadParams {
                    thread_id: conversation_id,
                    include_archived: true,
                    include_history: false,
                })
                .await
        }
    }
    .ok()?;

    let title = thread.name.as_deref()?.trim();
    (!title.is_empty() && thread.preview.trim() != title).then(|| title.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_subagent_session_started(
    analytics_events_client: &AnalyticsEventsClient,
    client_metadata: AppServerClientMetadata,
    session_id: SessionId,
    thread_id: ThreadId,
    parent_thread_id: Option<ThreadId>,
    thread_config: ThreadConfigSnapshot,
    subagent_source: SubAgentSource,
    resumed_created_at: Option<DateTime<Utc>>,
) {
    let initialization_mode = if resumed_created_at.is_some() {
        ThreadInitializationMode::Resumed
    } else {
        ThreadInitializationMode::New
    };
    let created_at =
        u64::try_from(resumed_created_at.unwrap_or_else(Utc::now).timestamp()).unwrap_or_default();
    let AppServerClientMetadata {
        client_name,
        client_version,
    } = client_metadata;
    if (client_name.is_none() || client_version.is_none())
        && subagent_source.kind() != crate::guardian::GUARDIAN_REVIEWER_NAME
    {
        tracing::warn!("skipping subagent thread analytics: missing inherited client metadata");
        return;
    }
    let input = SubAgentThreadStartedInput {
        session_id: session_id.to_string(),
        thread_id: thread_id.to_string(),
        parent_thread_id: parent_thread_id.map(|thread_id| thread_id.to_string()),
        forked_from_thread_id: thread_config
            .forked_from_thread_id
            .map(|thread_id| thread_id.to_string()),
        product_client_id: thread_config.originator.clone(),
        client_name,
        client_version,
        model: thread_config.model,
        ephemeral: thread_config.ephemeral,
        thread_source: thread_config.thread_source,
        subagent_source,
        initialization_mode,
        created_at,
    };
    analytics_events_client.track_subagent_thread_started(input);
}

/// Builds hook configuration for one config snapshot, including any enabled plugin hooks.
async fn build_hooks_config(
    config: &Config,
    plugins_manager: &PluginsManager,
    environment: Option<&TurnEnvironment>,
    disabled_plugin_ids: &[String],
) -> HooksConfig {
    let (hook_shell_program, hook_shell_argv) = environment
        .and_then(|environment| environment.shell.as_ref())
        .map(|shell| {
            let mut argv = shell.derive_exec_args("", /*use_login_shell*/ false);
            let program = argv.remove(0);
            let _ = argv.pop();
            (Some(program), argv)
        })
        .unwrap_or_default();
    let plugins_input = config.plugins_config_input();
    let plugin_outcome = plugins_manager
        .plugins_for_config(&plugins_input)
        .await
        .without_plugins(disabled_plugin_ids);
    let plugin_hook_sources = plugin_outcome.effective_plugin_hook_sources();
    let plugin_hook_load_warnings = plugin_outcome.effective_plugin_hook_warnings();
    HooksConfig {
        legacy_notify_argv: config.notify.clone(),
        feature_enabled: config.features.enabled(Feature::CodexHooks),
        bypass_hook_trust: config.bypass_hook_trust,
        config_layer_stack: Some(config.config_layer_stack.clone()),
        plugin_hook_sources,
        plugin_hook_load_warnings,
        shell_program: hook_shell_program,
        shell_args: hook_shell_argv,
    }
}

#[cfg(test)]
#[path = "elicitation_holders_tests.rs"]
mod elicitation_holders_tests;

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
#[path = "world_state_item_tests.rs"]
mod world_state_item_tests;
