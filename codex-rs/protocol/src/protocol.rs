//! Defines the protocol for a Codex session between a client and an agent.
//!
//! Uses a SQ (Submission Queue) / EQ (Event Queue) pattern to asynchronously communicate
//! between user and agent.
//!
//! `Op` and `EventMsg` are defined here. Their payload types live in child modules grouped by
//! responsibility (realtime, sandbox policy, hooks, turn/item/session/collab events, token
//! usage, session metadata) and are re-exported from this module.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::ops::Mul;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use strum_macros::EnumIter;

use crate::AgentPath;
use crate::ResponseItemId;
use crate::SanitizedGitUrl;
use crate::SessionId;
use crate::ThreadId;
use crate::approvals::ElicitationAbandonedEvent;
use crate::approvals::ElicitationRequestEvent;
use crate::capabilities::SelectedCapabilityRoot;
use crate::config_types::ApprovalsReviewer;
use crate::config_types::CollaborationMode;
use crate::config_types::ModeKind;
use crate::config_types::Personality;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::config_types::WindowsSandboxLevel;
use crate::dynamic_tools::DynamicToolCallOutputContentItem;
use crate::dynamic_tools::DynamicToolCallRequest;
use crate::dynamic_tools::DynamicToolResponse;
use crate::dynamic_tools::DynamicToolSpec;
use crate::error::Result as CodexResult;
use crate::items::AgentMessageDelivery;
use crate::items::AsyncUserInputQuestion;
use crate::items::TurnItem;
use crate::mcp::CallToolResult;
use crate::mcp::RequestId;
use crate::memory_citation::MemoryCitation;
use crate::models::ActivePermissionProfile;
use crate::models::AgentMessageInputContent;
use crate::models::AudioReference;
use crate::models::BaseInstructions;
use crate::models::ContentItem;
use crate::models::ImageDetail;
use crate::models::InternalChatMessageMetadataPassthrough;
use crate::models::MessagePhase;
use crate::models::PermissionProfile;
use crate::models::ProfileWorkspaceRoot;
use crate::models::ResponseInputItem;
use crate::models::ResponseItem;
use crate::models::SandboxEnforcement;
use crate::models::WebSearchAction;
use crate::num_format::format_with_separators;
use crate::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::parse_command::ParsedCommand;
use crate::plan_tool::UpdatePlanArgs;
use crate::request_permissions::RequestPermissionsEvent;
use crate::request_permissions::RequestPermissionsResponse;
use crate::request_user_input::RequestUserInputResponse;
use crate::turn_input::CyberAccessProgram;
use crate::turn_input::SuspendTurnOutcome;
use crate::turn_input::TurnInputMode;
use crate::turn_input::TurnInputRequest;
use crate::turn_input::TurnInputSubmission;
use crate::turn_input::TurnStartOptions;
use codex_extension_items::image_generation::GeneratedImageRef;
use codex_extension_items::image_generation::ImageGenerationFailure;
use codex_extension_items::image_generation::ImageGenerationInputImage;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::PathUri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as _;
use serde_json::Map;
use serde_json::Value;
use serde_with::serde_as;
use strum_macros::Display;
use tokio::sync::oneshot;
use tracing::error;
use ts_rs::TS;

pub use crate::approvals::ApplyPatchApprovalRequestEvent;
pub use crate::approvals::ElicitationAction;
pub use crate::approvals::ExecApprovalRequestEvent;
pub use crate::approvals::ExecPolicyAmendment;
pub use crate::approvals::GuardianAssessmentAction;
pub use crate::approvals::GuardianAssessmentDecisionSource;
pub use crate::approvals::GuardianAssessmentEvent;
pub use crate::approvals::GuardianAssessmentOutcome;
pub use crate::approvals::GuardianAssessmentStatus;
pub use crate::approvals::GuardianCommandSource;
pub use crate::approvals::GuardianRiskLevel;
pub use crate::approvals::GuardianUserAuthorization;
pub use crate::approvals::NetworkApprovalContext;
pub use crate::approvals::NetworkApprovalProtocol;
pub use crate::approvals::NetworkPolicyAmendment;
pub use crate::approvals::NetworkPolicyRuleAction;
pub use crate::environment::EnvironmentConfig;
pub use crate::environment::EnvironmentConfigState;
pub use crate::environment::TurnEnvironmentRequest;
pub use crate::environment::TurnEnvironmentRequests;
pub use crate::environment::TurnEnvironmentSelection;
pub use crate::environment::TurnEnvironmentSelections;
pub use crate::environment::has_full_access;
pub use crate::legacy_events::HasLegacyEvent;
pub use crate::permissions::FileSystemAccessMode;
pub use crate::permissions::FileSystemPath;
pub use crate::permissions::FileSystemSandboxEntry;
pub use crate::permissions::FileSystemSandboxKind;
pub use crate::permissions::FileSystemSandboxPolicy;
pub use crate::permissions::FileSystemSpecialPath;
pub use crate::permissions::NetworkSandboxPolicy;
pub use crate::permissions::RawFileSystemSandboxPolicy;
use crate::permissions::default_read_only_subpaths_for_writable_root;
pub use crate::request_permissions::RequestPermissionsArgs;
pub use crate::request_user_input::RequestUserInputEvent;

mod collab_events;
mod hook_events;
mod item_events;
mod realtime_conversation;
mod sandbox_policy;
mod session_events;
mod session_meta;
mod token_usage;
mod turn_events;

pub use collab_events::CollabAgentInteractionBeginEvent;
pub use collab_events::CollabAgentInteractionEndEvent;
pub use collab_events::CollabAgentRef;
pub use collab_events::CollabAgentSpawnBeginEvent;
pub use collab_events::CollabAgentSpawnEndEvent;
pub use collab_events::CollabAgentStatusEntry;
pub use collab_events::CollabCloseBeginEvent;
pub use collab_events::CollabCloseEndEvent;
pub use collab_events::CollabResumeBeginEvent;
pub use collab_events::CollabResumeEndEvent;
pub use collab_events::CollabWaitingBeginEvent;
pub use collab_events::CollabWaitingEndEvent;
pub use collab_events::SubAgentActivityEvent;
pub use collab_events::SubAgentActivityKind;
pub use hook_events::HookCompletedEvent;
pub use hook_events::HookEventName;
pub use hook_events::HookExecutionMode;
pub use hook_events::HookHandlerType;
pub use hook_events::HookOutputEntry;
pub use hook_events::HookOutputEntryKind;
pub use hook_events::HookRunStatus;
pub use hook_events::HookRunSummary;
pub use hook_events::HookScope;
pub use hook_events::HookSource;
pub use hook_events::HookStartedEvent;
pub use hook_events::HookTrustStatus;
pub use item_events::AgentMessageEvent;
pub use item_events::AgentReasoningEvent;
pub use item_events::AgentReasoningRawContentEvent;
pub use item_events::AgentReasoningSectionBreakEvent;
pub use item_events::Chunk;
pub use item_events::DynamicToolCallResponseEvent;
pub use item_events::ExecCommandBeginEvent;
pub use item_events::ExecCommandEndEvent;
pub use item_events::ExecCommandOutputDeltaEvent;
pub use item_events::ExecCommandSource;
pub use item_events::ExecCommandStatus;
pub use item_events::ExecOutputStream;
pub use item_events::FileChange;
pub use item_events::ImageGenerationBeginEvent;
pub use item_events::ImageGenerationEndEvent;
pub use item_events::McpInvocation;
pub use item_events::McpToolCallBeginEvent;
pub use item_events::McpToolCallEndEvent;
pub use item_events::PatchApplyBeginEvent;
pub use item_events::PatchApplyEndEvent;
pub use item_events::PatchApplyStatus;
pub use item_events::PatchApplyUpdatedEvent;
pub use item_events::TerminalInteractionEvent;
pub use item_events::TurnDiffEvent;
pub use item_events::UserMessageAudioKind;
pub use item_events::UserMessageEvent;
pub use item_events::UserMessageImageKind;
pub use item_events::ViewImageToolCallEvent;
pub use item_events::WebSearchBeginEvent;
pub use item_events::WebSearchEndEvent;
pub use item_events::user_message_preview;
pub use realtime_conversation::CodexResponseHandoffMode;
pub use realtime_conversation::ConversationAudioParams;
pub use realtime_conversation::ConversationSpeechParams;
pub use realtime_conversation::ConversationStartParams;
pub use realtime_conversation::ConversationStartTransport;
pub use realtime_conversation::ConversationTextParams;
pub use realtime_conversation::ConversationTextRole;
pub use realtime_conversation::RealtimeAudioFrame;
pub use realtime_conversation::RealtimeConversationClosedEvent;
pub use realtime_conversation::RealtimeConversationListVoicesResponseEvent;
pub use realtime_conversation::RealtimeConversationRealtimeEvent;
pub use realtime_conversation::RealtimeConversationSdpEvent;
pub use realtime_conversation::RealtimeConversationStartedEvent;
pub use realtime_conversation::RealtimeConversationVersion;
pub use realtime_conversation::RealtimeEvent;
pub use realtime_conversation::RealtimeHandoffRequested;
pub use realtime_conversation::RealtimeInputAudioSpeechStarted;
pub use realtime_conversation::RealtimeNoopRequested;
pub use realtime_conversation::RealtimeOutputModality;
pub use realtime_conversation::RealtimeResponseCancelled;
pub use realtime_conversation::RealtimeResponseCreated;
pub use realtime_conversation::RealtimeResponseDone;
pub use realtime_conversation::RealtimeSidebandTransport;
pub use realtime_conversation::RealtimeTranscriptDelta;
pub use realtime_conversation::RealtimeTranscriptDone;
pub use realtime_conversation::RealtimeTranscriptEntry;
pub use realtime_conversation::RealtimeVoice;
pub use realtime_conversation::RealtimeVoicesList;
pub use sandbox_policy::AskForApproval;
pub use sandbox_policy::GranularApprovalConfig;
pub use sandbox_policy::NetworkAccess;
pub use sandbox_policy::ReviewDecision;
pub use sandbox_policy::SandboxPolicy;
pub use sandbox_policy::WritableRoot;
pub use session_events::ConversationPathResponseEvent;
pub use session_events::MAX_THREAD_GOAL_OBJECTIVE_CHARS;
pub use session_events::McpAuthStatus;
pub use session_events::McpStartupCompleteEvent;
pub use session_events::McpStartupFailure;
pub use session_events::McpStartupFailureReason;
pub use session_events::McpStartupStatus;
pub use session_events::McpStartupUpdateEvent;
pub use session_events::Product;
pub use session_events::SessionConfiguredEvent;
pub use session_events::SessionNetworkProxyRuntime;
pub use session_events::SkillDependencies;
pub use session_events::SkillInterface;
pub use session_events::SkillMetadata;
pub use session_events::SkillScope;
pub use session_events::SkillToolDependency;
pub use session_events::ThreadGoal;
pub use session_events::ThreadGoalStatus;
pub use session_events::ThreadGoalUpdatedEvent;
pub use session_events::ThreadQueueChangedEvent;
pub use session_events::validate_thread_goal_objective;
pub use session_meta::GitInfo;
pub use session_meta::HistoryPosition;
pub use session_meta::InternalSessionSource;
pub use session_meta::MultiAgentVersion;
pub use session_meta::SessionContextWindow;
pub use session_meta::SessionMeta;
pub use session_meta::SessionMetaLine;
pub use session_meta::SessionSource;
pub use session_meta::SubAgentSource;
pub use session_meta::ThreadSource;
pub use session_meta::TruncationPolicy;
pub use session_meta::TurnContextItem;
pub use session_meta::WorldStateItem;
pub use session_meta::estimate_compaction_content_length;
pub use token_usage::CreditsSnapshot;
pub use token_usage::FinalOutput;
pub use token_usage::RateLimitReachedType;
pub use token_usage::RateLimitSnapshot;
pub use token_usage::RateLimitWindow;
pub use token_usage::SpendControlLimitSnapshot;
pub use token_usage::TokenCountEvent;
pub use token_usage::TokenUsage;
pub use token_usage::TokenUsageInfo;
pub use token_usage::TokenUsageRecord;
pub use turn_events::AgentMessageContentDeltaEvent;
pub use turn_events::AgentStatus;
pub use turn_events::AuthRecoveryEvent;
pub use turn_events::CodexErrorInfo;
pub use turn_events::ContextCompactedEvent;
pub use turn_events::DeprecationNoticeEvent;
pub use turn_events::EnteredReviewModeEvent;
pub use turn_events::ErrorEvent;
pub use turn_events::ExitedReviewModeEvent;
pub use turn_events::ItemCompletedEvent;
pub use turn_events::ItemStartedEvent;
pub use turn_events::MisalignmentErrorDetails;
pub use turn_events::MisalignmentSteer;
pub use turn_events::ModelRerouteEvent;
pub use turn_events::ModelRerouteReason;
pub use turn_events::ModelVerification;
pub use turn_events::ModelVerificationEvent;
pub use turn_events::NonSteerableTurnKind;
pub use turn_events::PlanDeltaEvent;
pub use turn_events::PrivateSessionEvent;
pub use turn_events::PrivateSessionEventPayload;
pub use turn_events::RawResponseCompletedEvent;
pub use turn_events::RawResponseItemEvent;
pub use turn_events::ReasoningContentDeltaEvent;
pub use turn_events::ReasoningRawContentDeltaEvent;
pub use turn_events::ReviewCodeLocation;
pub use turn_events::ReviewFinding;
pub use turn_events::ReviewLineRange;
pub use turn_events::ReviewOutputEvent;
pub use turn_events::ReviewRequest;
pub use turn_events::ReviewTarget;
pub use turn_events::SafetyBufferingEvent;
pub use turn_events::StreamErrorEvent;
pub use turn_events::StreamInfoEvent;
pub use turn_events::TargetedAnswerModeration;
pub use turn_events::ThreadRolledBackEvent;
pub use turn_events::ThreadSettingsAppliedEvent;
pub use turn_events::ThreadSettingsSnapshot;
pub use turn_events::TurnAbortReason;
pub use turn_events::TurnAbortedEvent;
pub use turn_events::TurnCompleteEvent;
pub use turn_events::TurnModerationMetadataEvent;
pub use turn_events::TurnStartedEvent;
pub use turn_events::WarningEvent;

/// Open/close tags for special context blocks. Used across crates to avoid duplicated hardcoded
/// strings.
pub const USER_INSTRUCTIONS_OPEN_TAG: &str = "<user_instructions>";
pub const USER_INSTRUCTIONS_CLOSE_TAG: &str = "</user_instructions>";
pub const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub const ENVIRONMENT_CONTEXT_CLOSE_TAG: &str = "</environment_context>";
pub const ENVIRONMENTS_INSTRUCTIONS_OPEN_TAG: &str = "<environments_instructions>";
pub const ENVIRONMENTS_INSTRUCTIONS_CLOSE_TAG: &str = "</environments_instructions>";
pub const APPS_INSTRUCTIONS_OPEN_TAG: &str = "<apps_instructions>";
pub const APPS_INSTRUCTIONS_CLOSE_TAG: &str = "</apps_instructions>";
pub const SKILLS_INSTRUCTIONS_OPEN_TAG: &str = "<skills_instructions>";
pub const SKILLS_INSTRUCTIONS_CLOSE_TAG: &str = "</skills_instructions>";
pub const PLUGINS_INSTRUCTIONS_OPEN_TAG: &str = "<plugins_instructions>";
pub const PLUGINS_INSTRUCTIONS_CLOSE_TAG: &str = "</plugins_instructions>";
pub const TOOLS_OPEN_TAG: &str = "<tools>";
pub const TOOLS_CLOSE_TAG: &str = "</tools>";
pub const COLLABORATION_MODE_OPEN_TAG: &str = "<collaboration_mode>";
pub const COLLABORATION_MODE_CLOSE_TAG: &str = "</collaboration_mode>";
pub const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
pub const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
pub const REALTIME_CONVERSATION_OPEN_TAG: &str = "<realtime_conversation>";
pub const REALTIME_CONVERSATION_CLOSE_TAG: &str = "</realtime_conversation>";
pub const CONTEXT_WINDOW_OPEN_TAG: &str = "<context_window>";
pub const CONTEXT_WINDOW_CLOSE_TAG: &str = "</context_window>";
pub const CONTEXT_WINDOW_GUIDANCE_OPEN_TAG: &str = "<context_window_guidance>";
pub const CONTEXT_WINDOW_GUIDANCE_CLOSE_TAG: &str = "</context_window_guidance>";
pub const USER_MESSAGE_BEGIN: &str = "## My request for Codex:";

/// Removes the model-context prefix from a user message before displaying it.
pub fn strip_user_message_prefix(text: &str) -> &str {
    match text.find(USER_MESSAGE_BEGIN) {
        Some(idx) => text[idx + USER_MESSAGE_BEGIN.len()..].trim(),
        None => text.trim(),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(transparent)]
#[ts(type = "string")]
pub struct GitSha(pub String);

impl GitSha {
    pub fn new(sha: &str) -> Self {
        Self(sha.to_string())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct W3cTraceContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tracestate: Option<String>,
}

/// Supported sparse changes to one live task's current settings, regardless of
/// task kind. Child sessions and consumers of frozen initial settings are unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnSettingsUpdate {
    /// Changes the reviewer for subsequent approval requests, not pending reviews.
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    /// Replaces the selection for subsequent steps, without changing future turns.
    /// Environments may inherit the running turn's defaults or provide their own configuration,
    /// which can be pending. An already-selected environment with its own cannot switch back.
    pub environments: Option<Vec<TurnEnvironmentRequest>>,
    pub model: Option<String>,
    /// `None` preserves the selection; `Some(None)` clears it.
    pub effort: Option<Option<ReasoningEffortConfig>>,
    pub summary: Option<ReasoningSummaryConfig>,
    /// `None` preserves the requested tier; `Some(None)` clears it.
    pub service_tier: Option<Option<String>>,
}

/// The result of processing a turn-settings update, not merely queueing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSettingsUpdateOutcome {
    /// Published for subsequent captures; already captured steps are unchanged.
    /// The task need not sample or consume every selected preference.
    Applied,
    /// The named live task was absent or lost before publication.
    TargetUnavailable,
    Rejected {
        reason: String,
    },
}

/// Thread-settings overrides that can be applied before user input or on their
/// own. Standalone updates change the settings inherited by future turns.
// Diagnostics retain requested policy categories, never paths or instruction payloads.
#[derive(derive_more::Debug, Clone, Default, PartialEq)]
#[debug(
    "ThreadSettingsOverrides {{ approval_policy: {approval_policy:?}, approvals_reviewer: {approvals_reviewer:?}, sandbox_policy: {:?}, permission_profile: {:?} }}",
    sandbox_policy.as_ref().map(tracing::field::display),
    permission_profile.as_ref().map(|profile| match profile {
        PermissionProfile::Managed { .. } => "managed",
        PermissionProfile::Disabled => "disabled",
        PermissionProfile::External { .. } => "external",
    }),
)]
pub struct ThreadSettingsOverrides {
    /// Updated fallback `cwd` and environments supplied together as a complete pair.
    pub environments: Option<TurnEnvironmentRequests>,

    /// Updated top-level runtime workspace roots for default environments.
    /// Explicit environment requests own their workspace roots separately.
    pub runtime_workspace_roots: Option<Vec<AbsolutePathBuf>>,

    /// Updated profile-defined workspace roots for status summaries and
    /// per-turn config reconstruction.
    pub profile_workspace_roots: Option<Vec<ProfileWorkspaceRoot>>,

    /// Updated command approval policy.
    pub approval_policy: Option<AskForApproval>,

    /// Updated approval reviewer for future approval prompts.
    pub approvals_reviewer: Option<ApprovalsReviewer>,

    /// Updated sandbox policy for tool calls.
    pub sandbox_policy: Option<SandboxPolicy>,

    /// Updated permissions profile for tool calls.
    pub permission_profile: Option<PermissionProfile>,

    /// Named or built-in profile that produced `permission_profile`, if the
    /// update selected a profile rather than supplying raw permissions.
    pub active_permission_profile: Option<ActivePermissionProfile>,

    /// Updated Windows sandbox mode for tool execution.
    pub windows_sandbox_level: Option<WindowsSandboxLevel>,

    /// Updated model slug. When set, the model info is derived automatically.
    pub model: Option<String>,

    /// Updated reasoning effort (honored only for reasoning-capable models).
    ///
    /// Use `Some(Some(_))` to set a specific effort, `Some(None)` to clear the
    /// effort, or `None` to leave the existing value unchanged.
    pub effort: Option<Option<ReasoningEffortConfig>>,

    /// Updated reasoning summary preference (honored only for reasoning-capable models).
    pub summary: Option<ReasoningSummaryConfig>,

    /// Updated service tier preference for future turns.
    ///
    /// Use `Some(Some(_))` to set a specific tier, `Some(None)` to clear the
    /// preference, or `None` to leave the existing value unchanged.
    pub service_tier: Option<Option<String>>,

    /// EXPERIMENTAL - set a pre-set collaboration mode.
    /// Takes precedence over model, effort, and developer instructions if set.
    pub collaboration_mode: Option<CollaborationMode>,

    /// Updated personality preference.
    pub personality: Option<Personality>,

    /// Replace the thread's disabled plugin IDs. Omission preserves the current
    /// selection, and an empty list clears it.
    pub disabled_plugin_ids: Option<Vec<String>>,
}

/// Source classification for client-supplied context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdditionalContextKind {
    Untrusted,
    Application,
}

/// Client-supplied context keyed by an opaque source identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalContextEntry {
    pub value: String,
    pub kind: AdditionalContextKind,
    /// Optional name for untrusted context, used to label its tool output.
    pub name: Option<String>,
}

impl AdditionalContextEntry {
    /// Creates an unnamed context entry.
    pub fn new(value: String, kind: AdditionalContextKind) -> Self {
        Self {
            value,
            kind,
            name: None,
        }
    }

    /// Sets the name for untrusted context.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Validates the optional untrusted-context name before input admission.
    pub fn validate(&self) -> Result<(), &'static str> {
        let Some(name) = &self.name else {
            return Ok(());
        };
        if self.kind == AdditionalContextKind::Application {
            return Err("additional context name is only supported for untrusted context");
        }
        if name.is_empty()
            || name.len() > 64
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err("additional context name must match [a-z][a-z0-9_]{0,63}");
        }
        Ok(())
    }
}

/// Submission operation
// Keep diagnostic fields explicit so new payload fields do not enter logs by default.
#[derive(derive_more::Debug)]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum Op {
    /// Abort current task without terminating background terminal processes.
    /// This server sends [`EventMsg::TurnAborted`] in response.
    Interrupt,

    /// Interrupt the named turn only if no input is queued for it.
    /// The decision is acknowledged before cancellation finishes.
    #[debug("InterruptIfNoPendingInput {{ turn_id: {turn_id:?} }}")]
    InterruptIfNoPendingInput {
        turn_id: String,
        reply: oneshot::Sender<bool>,
    },

    /// Terminate all running background terminal processes for this thread.
    /// Use this when callers intentionally want to stop long-lived background shells.
    CleanBackgroundTerminals,

    /// Start a realtime conversation stream.
    #[debug("RealtimeConversationStart {{ session_id: {:?}, version: {:?}, output_modality: {:?} }}", _0.realtime_session_id, _0.version, _0.output_modality)]
    RealtimeConversationStart(ConversationStartParams),

    /// Start a realtime conversation and report completion of the connection attempt.
    RealtimeConversationAttach {
        params: ConversationStartParams,
        reply: oneshot::Sender<CodexResult<()>>,
    },

    /// Attach an existing call using caller-provided I/O without opening a socket.
    RealtimeConversationAttachStream {
        params: ConversationStartParams,
        transport: RealtimeSidebandTransport,
        reply: oneshot::Sender<CodexResult<()>>,
    },

    /// Send audio input to the running realtime conversation stream.
    #[debug("RealtimeConversationAudio {{ item_id: {:?}, sample_rate: {}, num_channels: {}, samples_per_channel: {:?} }}", _0.frame.item_id, _0.frame.sample_rate, _0.frame.num_channels, _0.frame.samples_per_channel)]
    RealtimeConversationAudio(ConversationAudioParams),

    /// Send text input to the running realtime conversation stream.
    #[debug("RealtimeConversationText {{ role: {:?} }}", _0.role)]
    RealtimeConversationText(ConversationTextParams),

    /// Append speakable text to the running realtime conversation stream.
    #[debug("RealtimeConversationSpeech")]
    RealtimeConversationSpeech(ConversationSpeechParams),

    /// Close the running realtime conversation stream.
    RealtimeConversationClose,

    /// Close only the matching session after earlier start operations finish.
    RealtimeConversationDetach {
        realtime_session_id: String,
        reply: oneshot::Sender<CodexResult<()>>,
    },

    /// Request the list of voices supported by realtime conversation streams.
    RealtimeConversationListVoices,

    /// Submit turn input using the requested routing behavior.
    #[debug("TurnInput {{ requested_settings: {:?} }}", request.thread_settings)]
    TurnInput {
        request: Box<TurnInputRequest>,
        mode: TurnInputMode,
        reply: oneshot::Sender<CodexResult<TurnInputSubmission>>,
    },

    /// Resume an interrupted regular turn.
    #[debug("RecoverTurn {{ requested_settings: {thread_settings:?} }}")]
    RecoverTurn {
        thread_settings: ThreadSettingsOverrides,
        start_options: TurnStartOptions,
        reply: oneshot::Sender<CodexResult<TurnInputSubmission>>,
    },

    /// Stop the active root turn without recording a terminal turn event.
    #[debug("SuspendTurnAndShutdown")]
    SuspendTurnAndShutdown {
        reply: oneshot::Sender<CodexResult<SuspendTurnOutcome>>,
    },

    /// Apply thread-settings overrides without starting a turn.
    ///
    /// This uses the same submission queue as turn starts so app-server can
    /// preserve caller order between both kinds of mutation.
    #[debug("ThreadSettings {{ requested_settings: {thread_settings:?} }}")]
    ThreadSettings {
        /// Sparse thread-settings overrides to apply.
        thread_settings: ThreadSettingsOverrides,
        /// When present, report validation errors here instead of emitting an error event.
        /// Successful updates still emit `ThreadSettingsApplied` for all callers.
        reply: Option<oneshot::Sender<CodexResult<()>>>,
    },

    /// Replace the dynamic tool catalog without starting a turn.
    #[debug("ReplaceDynamicTools")]
    ReplaceDynamicTools {
        tools: Vec<DynamicToolSpec>,
        reply: oneshot::Sender<CodexResult<()>>,
    },

    /// Update only the named running turn, without changing future settings.
    /// The reply reports the actual publication or why it did not occur.
    #[debug("TurnSettings {{ turn_id: {turn_id:?}, requested_approvals_reviewer: {:?} }}", update.approvals_reviewer)]
    TurnSettings {
        turn_id: String,
        update: TurnSettingsUpdate,
        reply: oneshot::Sender<TurnSettingsUpdateOutcome>,
    },

    /// Inter-agent communication that should be recorded as agent-message history
    /// while still using the normal thread submission lifecycle.
    #[debug("InterAgentCommunication {{ id: {:?}, author: {:?}, recipient: {:?}, trigger_turn: {} }}", communication.id, communication.author, communication.recipient, communication.trigger_turn)]
    InterAgentCommunication {
        communication: InterAgentCommunication,
        start_options: TurnStartOptions,
    },

    /// Approve a command execution
    #[debug("ExecApproval {{ id: {id:?}, turn_id: {turn_id:?}, decision: {} }}", decision.to_opaque_string())]
    ExecApproval {
        /// The id of the submission we are approving
        id: String,
        /// Turn id associated with the approval event, when available.
        turn_id: Option<String>,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Approve a code patch
    #[debug("PatchApproval {{ id: {id:?}, decision: {} }}", decision.to_opaque_string())]
    PatchApproval {
        /// The id of the submission we are approving
        id: String,
        /// The user's decision in response to the request.
        decision: ReviewDecision,
    },

    /// Cancel the file change approval issued by the named turn.
    #[debug("CancelPatchApproval {{ id: {id:?}, turn_id: {turn_id:?} }}")]
    CancelPatchApproval { id: String, turn_id: String },

    /// Resolve an MCP elicitation request.
    #[debug("ResolveElicitation {{ request_id: {request_id:?}, decision: {decision:?} }}")]
    ResolveElicitation {
        /// Name of the MCP server that issued the request.
        server_name: String,
        /// Request identifier from the MCP server.
        request_id: RequestId,
        /// User's decision for the request.
        decision: ElicitationAction,
        /// Structured user input supplied for accepted elicitations.
        content: Option<Value>,
        /// Optional client metadata associated with the elicitation response.
        meta: Option<Value>,
    },

    /// Resolve a request_user_input tool call.
    #[debug("UserInputAnswer {{ id: {id:?} }}")]
    UserInputAnswer {
        /// Turn id for the in-flight request.
        id: String,
        /// User-provided answers.
        response: RequestUserInputResponse,
    },

    /// Resolve a request_permissions tool call.
    #[debug("RequestPermissionsResponse {{ id: {id:?}, scope: {:?}, strict_auto_review: {} }}", response.scope, response.strict_auto_review)]
    RequestPermissionsResponse {
        /// Call id for the in-flight request.
        id: String,
        /// User-granted permissions.
        response: RequestPermissionsResponse,
    },

    /// Resolve a dynamic tool call request.
    #[debug("DynamicToolResponse {{ id: {id:?}, success: {} }}", response.success)]
    DynamicToolResponse {
        /// Call id for the in-flight request.
        id: String,
        /// Tool output payload.
        response: DynamicToolResponse,
    },

    /// Request MCP servers to reinitialize and refresh cached tool lists.
    RefreshMcpServers,

    /// Reload user config layer overrides for the active session.
    ///
    /// This updates runtime config-derived behavior (for example app
    /// enable/disable state) without restarting the thread.
    ReloadUserConfig,

    /// Request the agent to summarize the current conversation context.
    /// The agent will use its existing context (either conversation history or previous response id)
    /// to generate a summary which will be returned as an AgentMessage event.
    Compact,

    /// Set whether the thread remains eligible for memory generation.
    ///
    /// This persists thread-level memory mode metadata without involving the
    /// model.
    #[debug("SetThreadMemoryMode {{ mode: {mode:?} }}")]
    SetThreadMemoryMode { mode: ThreadMemoryMode },

    /// Request a code review from the agent.
    #[debug(
        "Review {{ target: {} }}",
        match &review_request.target {
            ReviewTarget::UncommittedChanges => "uncommitted_changes",
            ReviewTarget::BaseBranch { .. } => "base_branch",
            ReviewTarget::Commit { .. } => "commit",
            ReviewTarget::Custom { .. } => "custom",
        },
    )]
    Review { review_request: ReviewRequest },

    /// Record that the user approved one retry of a concrete Guardian-denied action.
    // Assessment events are transient; retain the review identity when approving a retry.
    #[debug("ApproveGuardianDeniedAction {{ review_id: {:?}, target_item_id: {:?}, turn_id: {:?}, status: {:?} }}", event.id, event.target_item_id, event.turn_id, event.status)]
    ApproveGuardianDeniedAction { event: GuardianAssessmentEvent },

    /// Request to shut down codex instance.
    Shutdown,

    /// Execute a user-initiated one-off shell command (triggered by "!cmd").
    ///
    /// The command string is executed using the user's default shell and may
    /// include shell syntax (pipes, redirects, etc.). Output is streamed via
    /// `ExecCommand*` events and the UI regains control upon `TurnComplete`.
    #[debug("RunUserShellCommand {{ timeout_ms: {timeout_ms:?} }}")]
    RunUserShellCommand {
        /// The raw command string after '!'
        command: String,
        /// Maximum execution time in milliseconds. Defaults to one hour.
        timeout_ms: Option<u64>,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThreadMemoryMode {
    Enabled,
    Disabled,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum ThreadHistoryMode {
    #[default]
    Legacy,
    Paginated,
}

impl ThreadHistoryMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Paginated => "paginated",
        }
    }
}

impl FromStr for ThreadHistoryMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "paginated" => Ok(Self::Paginated),
            _ => Err(format!("unknown thread history mode `{value}`")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
pub struct InterAgentCommunication {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<ResponseItemId>,
    pub author: AgentPath,
    pub recipient: AgentPath,
    #[serde(default)]
    pub other_recipients: Vec<AgentPath>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
    pub trigger_turn: bool,
}

impl InterAgentCommunication {
    pub fn new(
        author: AgentPath,
        recipient: AgentPath,
        other_recipients: Vec<AgentPath>,
        content: String,
        trigger_turn: bool,
    ) -> Self {
        Self {
            id: None,
            author,
            recipient,
            other_recipients,
            content,
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
            trigger_turn,
        }
    }

    pub fn new_encrypted(
        author: AgentPath,
        recipient: AgentPath,
        other_recipients: Vec<AgentPath>,
        encrypted_content: String,
        trigger_turn: bool,
    ) -> Self {
        Self {
            id: None,
            author,
            recipient,
            other_recipients,
            content: String::new(),
            encrypted_content: Some(encrypted_content),
            internal_chat_message_metadata_passthrough: None,
            trigger_turn,
        }
    }

    pub fn set_turn_id_if_missing(&mut self, turn_id: &str) {
        InternalChatMessageMetadataPassthrough::set_turn_id_if_missing(
            &mut self.internal_chat_message_metadata_passthrough,
            turn_id,
        );
    }

    pub fn to_response_input_item(&self) -> ResponseInputItem {
        let mut communication = self.clone();
        communication.id = None;
        communication.internal_chat_message_metadata_passthrough = None;
        ResponseInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                annotations: None,
                logprobs: None,
                text: serde_json::to_string(&communication).unwrap_or_default(),
            }],
            phase: Some(MessagePhase::Commentary),
        }
    }

    pub fn to_model_input_item(&self) -> ResponseItem {
        let content = match &self.encrypted_content {
            Some(encrypted_content) => {
                let message_type = if self.trigger_turn {
                    "NEW_TASK"
                } else {
                    "MESSAGE"
                };
                vec![
                    AgentMessageInputContent::InputText {
                        text: format!(
                            "Message Type: {message_type}\nTask name: {}\nSender: {}\nPayload:\n",
                            self.recipient, self.author
                        ),
                    },
                    AgentMessageInputContent::EncryptedContent {
                        encrypted_content: encrypted_content.clone(),
                    },
                ]
            }
            None => vec![AgentMessageInputContent::InputText {
                text: self.content.clone(),
            }],
        };
        ResponseItem::AgentMessage {
            id: self.id.clone(),
            author: self.author.to_string(),
            recipient: self.recipient.to_string(),
            content,
            internal_chat_message_metadata_passthrough: self
                .internal_chat_message_metadata_passthrough
                .clone(),
        }
    }

    pub fn is_message_content(content: &[ContentItem]) -> bool {
        Self::from_message_content(content).is_some()
    }

    pub fn from_message_content(content: &[ContentItem]) -> Option<Self> {
        match content {
            [ContentItem::InputText { text }] | [ContentItem::OutputText { text, .. }] => {
                serde_json::from_str(text).ok()
            }
            _ => None,
        }
    }
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::InterruptIfNoPendingInput { .. } => "interrupt_if_no_pending_input",
            Self::CleanBackgroundTerminals => "clean_background_terminals",
            Self::RealtimeConversationStart(_) => "realtime_conversation_start",
            Self::RealtimeConversationAttach { .. } => "realtime_conversation_attach",
            Self::RealtimeConversationAttachStream { .. } => "realtime_conversation_attach_stream",
            Self::RealtimeConversationAudio(_) => "realtime_conversation_audio",
            Self::RealtimeConversationText(_) => "realtime_conversation_text",
            Self::RealtimeConversationSpeech(_) => "realtime_conversation_speech",
            Self::RealtimeConversationClose => "realtime_conversation_close",
            Self::RealtimeConversationDetach { .. } => "realtime_conversation_detach",
            Self::RealtimeConversationListVoices => "realtime_conversation_list_voices",
            Self::TurnInput { .. } => "turn_input",
            Self::RecoverTurn { .. } => "recover_turn",
            Self::SuspendTurnAndShutdown { .. } => "suspend_turn_and_shutdown",
            Self::ThreadSettings { .. } => "thread_settings",
            Self::ReplaceDynamicTools { .. } => "replace_dynamic_tools",
            Self::TurnSettings { .. } => "turn_settings",
            Self::InterAgentCommunication { .. } => "inter_agent_communication",
            Self::ExecApproval { .. } => "exec_approval",
            Self::PatchApproval { .. } | Self::CancelPatchApproval { .. } => "patch_approval",
            Self::ResolveElicitation { .. } => "resolve_elicitation",
            Self::UserInputAnswer { .. } => "user_input_answer",
            Self::RequestPermissionsResponse { .. } => "request_permissions_response",
            Self::DynamicToolResponse { .. } => "dynamic_tool_response",
            Self::RefreshMcpServers => "refresh_mcp_servers",
            Self::ReloadUserConfig => "reload_user_config",
            Self::Compact => "compact",
            Self::SetThreadMemoryMode { .. } => "set_thread_memory_mode",
            Self::Review { .. } => "review",
            Self::ApproveGuardianDeniedAction { .. } => "approve_guardian_denied_action",
            Self::Shutdown => "shutdown",
            Self::RunUserShellCommand { .. } => "run_user_shell_command",
        }
    }
}

/// Event Queue Entry - events from agent
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    /// Submission `id` that this event is correlated with.
    pub id: String,
    /// Payload
    pub msg: EventMsg,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct EnvironmentConnectionEvent {
    pub environment_id: String,
}

/// Response event from the agent
/// NOTE: Make sure none of these values have optional types, as it will mess up the extension code-gen.
#[derive(Debug, Clone, Deserialize, Serialize, Display, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
#[strum(serialize_all = "snake_case")]
pub enum EventMsg {
    /// Error while executing a submission
    Error(ErrorEvent),

    /// Warning issued while processing a submission. Unlike `Error`, this
    /// indicates the turn continued but the user should still be notified.
    Warning(WarningEvent),

    /// Provider-owned authentication recovery has started for the current turn.
    AuthRecoveryStarted(AuthRecoveryEvent),

    /// Provider-owned authentication recovery has completed for the current turn.
    AuthRecoveryCompleted(AuthRecoveryEvent),

    /// Warning issued by the guardian automatic approval reviewer.
    GuardianWarning(WarningEvent),

    /// Realtime conversation lifecycle start event.
    RealtimeConversationStarted(RealtimeConversationStartedEvent),

    /// Realtime conversation streaming payload event.
    RealtimeConversationRealtime(RealtimeConversationRealtimeEvent),

    /// Realtime conversation lifecycle close event.
    RealtimeConversationClosed(RealtimeConversationClosedEvent),

    /// Realtime session description protocol payload.
    RealtimeConversationSdp(RealtimeConversationSdpEvent),

    /// Model routing changed from the requested model to a different model.
    ModelReroute(ModelRerouteEvent),

    /// Backend recommends additional account verification for this turn.
    ModelVerification(ModelVerificationEvent),

    /// Backend moderation metadata intended for first-party turn presentation.
    TurnModerationMetadata(TurnModerationMetadataEvent),

    /// Backend indicates that response output is waiting on a safety review.
    SafetyBuffering(SafetyBufferingEvent),

    /// Conversation history was compacted (either automatically or manually).
    ContextCompacted(ContextCompactedEvent),

    /// Legacy persisted marker for dropping the last N user turns.
    /// Retained for replay of existing rollouts; live rollback operations are unsupported.
    ThreadRolledBack(ThreadRolledBackEvent),

    /// Agent has started a turn.
    /// v1 wire format uses `task_started`; accept `turn_started` for v2 interop.
    #[serde(rename = "task_started", alias = "turn_started")]
    TurnStarted(TurnStartedEvent),

    /// Persistent thread-settings overrides from the correlated submission have
    /// been applied to the session configuration.
    ThreadSettingsApplied(ThreadSettingsAppliedEvent),

    /// Agent has completed all actions.
    /// v1 wire format uses `task_complete`; accept `turn_complete` for v2 interop.
    #[serde(rename = "task_complete", alias = "turn_complete")]
    TurnComplete(TurnCompleteEvent),

    /// Usage update for the current session, including totals and last turn.
    /// Optional means unknown — UIs should not display when `None`.
    TokenCount(TokenCountEvent),

    /// Agent text output message
    AgentMessage(AgentMessageEvent),

    /// User/system input message (what was sent to the model)
    UserMessage(UserMessageEvent),

    /// Reasoning event from agent.
    AgentReasoning(AgentReasoningEvent),

    /// Raw chain-of-thought from agent.
    AgentReasoningRawContent(AgentReasoningRawContentEvent),

    /// Signaled when the model begins a new reasoning summary section (e.g., a new titled block).
    AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent),

    /// Ack the client's configure message.
    SessionConfigured(SessionConfiguredEvent),

    /// A selected environment completed its connection handshake.
    EnvironmentConnected(EnvironmentConnectionEvent),

    /// A selected environment lost its established connection.
    EnvironmentDisconnected(EnvironmentConnectionEvent),

    /// Updated long-running goal metadata for the thread.
    ThreadGoalUpdated(ThreadGoalUpdatedEvent),

    /// A durable thread-scoped user-message queue changed.
    ThreadQueueChanged(ThreadQueueChangedEvent),

    /// Incremental MCP startup progress updates.
    McpStartupUpdate(McpStartupUpdateEvent),

    /// Aggregate MCP startup completion summary.
    McpStartupComplete(McpStartupCompleteEvent),

    McpToolCallBegin(McpToolCallBeginEvent),

    McpToolCallEnd(McpToolCallEndEvent),

    WebSearchBegin(WebSearchBeginEvent),

    WebSearchEnd(WebSearchEndEvent),

    ImageGenerationBegin(ImageGenerationBeginEvent),

    ImageGenerationEnd(ImageGenerationEndEvent),

    /// Notification that the server is about to execute a command.
    ExecCommandBegin(ExecCommandBeginEvent),

    /// Incremental chunk of output from a running command.
    ExecCommandOutputDelta(ExecCommandOutputDeltaEvent),

    /// Terminal interaction for an in-progress command (stdin sent and stdout observed).
    TerminalInteraction(TerminalInteractionEvent),

    ExecCommandEnd(ExecCommandEndEvent),

    /// Notification that the agent attached a local image via the view_image tool.
    ViewImageToolCall(ViewImageToolCallEvent),

    ExecApprovalRequest(ExecApprovalRequestEvent),

    /// Ends an in-process network prompt, including when its proxy request is dropped.
    #[serde(skip)]
    #[schemars(skip)]
    #[ts(skip)]
    NetworkApprovalResolved {
        approval_id: String,
    },

    RequestPermissions(RequestPermissionsEvent),

    RequestUserInput(RequestUserInputEvent),

    DynamicToolCallRequest(DynamicToolCallRequest),

    DynamicToolCallResponse(DynamicToolCallResponseEvent),

    ElicitationRequest(ElicitationRequestEvent),

    /// An unanswered MCP responder was abandoned at its original unbounded source.
    ElicitationAbandoned(ElicitationAbandonedEvent),

    ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent),

    /// Structured lifecycle event for a guardian-reviewed approval request.
    GuardianAssessment(GuardianAssessmentEvent),

    /// Notification advising the user that something they are using has been
    /// deprecated and should be phased out.
    DeprecationNotice(DeprecationNoticeEvent),

    /// Notification that a model stream experienced an error or disconnect
    /// and the system is handling it (e.g., retrying with backoff).
    StreamError(StreamErrorEvent),

    /// Notification that the agent is about to apply a code patch. Mirrors
    /// `ExecCommandBegin` so front‑ends can show progress indicators.
    PatchApplyBegin(PatchApplyBeginEvent),

    /// Latest model-generated structured changes for an `apply_patch` call.
    PatchApplyUpdated(PatchApplyUpdatedEvent),

    /// Notification that a patch application has finished.
    PatchApplyEnd(PatchApplyEndEvent),

    TurnDiff(TurnDiffEvent),

    /// List of voices supported by realtime conversation streams.
    RealtimeConversationListVoicesResponse(RealtimeConversationListVoicesResponseEvent),

    PlanUpdate(UpdatePlanArgs),

    TurnAborted(TurnAbortedEvent),

    /// Notification that the agent is shutting down.
    ShutdownComplete,

    /// Entered review mode.
    EnteredReviewMode(EnteredReviewModeEvent),

    /// Exited review mode with an optional final result to apply.
    ExitedReviewMode(ExitedReviewModeEvent),

    RawResponseItem(RawResponseItemEvent),
    RawResponseCompleted(RawResponseCompletedEvent),

    /// In-process delivery from a private reviewer through its parent's subscription.
    #[serde(skip)]
    #[schemars(skip)]
    #[ts(skip)]
    PrivateSessionEvent(PrivateSessionEvent),

    ItemStarted(ItemStartedEvent),
    ItemCompleted(ItemCompletedEvent),
    HookStarted(HookStartedEvent),
    HookCompleted(HookCompletedEvent),

    AgentMessageContentDelta(AgentMessageContentDeltaEvent),
    PlanDelta(PlanDeltaEvent),
    ReasoningContentDelta(ReasoningContentDeltaEvent),
    ReasoningRawContentDelta(ReasoningRawContentDeltaEvent),

    /// Collab interaction: agent spawn begin.
    CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent),
    /// Collab interaction: agent spawn end.
    CollabAgentSpawnEnd(CollabAgentSpawnEndEvent),
    /// Collab interaction: agent interaction begin.
    CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent),
    /// Collab interaction: agent interaction end.
    CollabAgentInteractionEnd(CollabAgentInteractionEndEvent),
    /// Collab interaction: waiting begin.
    CollabWaitingBegin(CollabWaitingBeginEvent),
    /// Collab interaction: waiting end.
    CollabWaitingEnd(CollabWaitingEndEvent),
    /// Collab interaction: close begin.
    CollabCloseBegin(CollabCloseBeginEvent),
    /// Collab interaction: close end.
    CollabCloseEnd(CollabCloseEndEvent),
    /// Collab interaction: resume begin.
    CollabResumeBegin(CollabResumeBeginEvent),
    /// Collab interaction: resume end.
    CollabResumeEnd(CollabResumeEndEvent),

    /// Path-based v2 sub-agent activity.
    SubAgentActivity(SubAgentActivityEvent),
}

impl EventMsg {
    /// Returns the ID and start time of the command this approval opens.
    /// Stdin and network reviews have their own prompts for an existing operation.
    pub fn command_approval_start(&self) -> Option<(&str, i64)> {
        match self {
            Self::ExecApprovalRequest(request)
                if request.kind == crate::approvals::ExecApprovalKind::Command
                    && request.approval_id.is_none()
                    && request.network_approval_context.is_none() =>
            {
                Some((&request.call_id, request.started_at_ms))
            }
            Self::GuardianAssessment(assessment)
                if assessment.status == GuardianAssessmentStatus::InProgress
                    && matches!(
                        assessment.action,
                        GuardianAssessmentAction::Command { .. }
                            | GuardianAssessmentAction::Execve { .. }
                    ) =>
            {
                Some((
                    assessment.target_item_id.as_deref()?,
                    assessment.started_at_ms,
                ))
            }
            _ => None,
        }
    }
}

impl From<CollabAgentSpawnBeginEvent> for EventMsg {
    fn from(event: CollabAgentSpawnBeginEvent) -> Self {
        EventMsg::CollabAgentSpawnBegin(event)
    }
}

impl From<CollabAgentSpawnEndEvent> for EventMsg {
    fn from(event: CollabAgentSpawnEndEvent) -> Self {
        EventMsg::CollabAgentSpawnEnd(event)
    }
}

impl From<CollabAgentInteractionBeginEvent> for EventMsg {
    fn from(event: CollabAgentInteractionBeginEvent) -> Self {
        EventMsg::CollabAgentInteractionBegin(event)
    }
}

impl From<CollabAgentInteractionEndEvent> for EventMsg {
    fn from(event: CollabAgentInteractionEndEvent) -> Self {
        EventMsg::CollabAgentInteractionEnd(event)
    }
}

impl From<CollabWaitingBeginEvent> for EventMsg {
    fn from(event: CollabWaitingBeginEvent) -> Self {
        EventMsg::CollabWaitingBegin(event)
    }
}

impl From<CollabWaitingEndEvent> for EventMsg {
    fn from(event: CollabWaitingEndEvent) -> Self {
        EventMsg::CollabWaitingEnd(event)
    }
}

impl From<CollabCloseBeginEvent> for EventMsg {
    fn from(event: CollabCloseBeginEvent) -> Self {
        EventMsg::CollabCloseBegin(event)
    }
}

impl From<CollabCloseEndEvent> for EventMsg {
    fn from(event: CollabCloseEndEvent) -> Self {
        EventMsg::CollabCloseEnd(event)
    }
}

impl From<CollabResumeBeginEvent> for EventMsg {
    fn from(event: CollabResumeBeginEvent) -> Self {
        EventMsg::CollabResumeBegin(event)
    }
}

impl From<CollabResumeEndEvent> for EventMsg {
    fn from(event: CollabResumeEndEvent) -> Self {
        EventMsg::CollabResumeEnd(event)
    }
}

impl From<SubAgentActivityEvent> for EventMsg {
    fn from(event: SubAgentActivityEvent) -> Self {
        EventMsg::SubAgentActivity(event)
    }
}

#[cfg(test)]
#[path = "conversation_start_params_tests.rs"]
mod conversation_start_params_tests;

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
