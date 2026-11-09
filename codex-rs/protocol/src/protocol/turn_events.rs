//! Turn lifecycle events: agent status, errors, item and delta streaming, review mode, and aborts.

use super::*;

/// Agent lifecycle status, derived from emitted events.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent is waiting for initialization.
    #[default]
    PendingInit,
    /// Agent is currently running.
    Running,
    /// Agent's current turn was interrupted and it may receive more input.
    Interrupted,
    /// Agent is done. Contains the final assistant message.
    Completed(Option<String>),
    /// Agent encountered an error.
    Errored(String),
    /// Agent has been shutdown.
    Shutdown,
    /// Agent is not found.
    NotFound,
}

/// Turn kinds that reject same-turn steering.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum NonSteerableTurnKind {
    Review,
    Compact,
}

/// Codex errors that we expose to clients.
#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[schemars(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum CodexErrorInfo {
    ContextWindowExceeded,
    SessionBudgetExceeded,
    UsageLimitExceeded,
    RateLimitExceeded,
    FlexUnavailable,
    ServerOverloaded,
    ModelNotFound,
    /// Capacity is unavailable on a route restricted by server policy.
    PolicyCapacityUnavailable,
    CyberPolicy,
    BioPolicy,
    MisalignmentPolicyViolation,
    TooManyDenials,
    HttpConnectionFailed {
        http_status_code: Option<u16>,
    },
    /// Failed to connect to the response SSE stream.
    ResponseStreamConnectionFailed {
        http_status_code: Option<u16>,
    },
    InternalServerError,
    Unauthorized,
    BadRequest,
    InvalidPrompt,
    SandboxError,
    /// The response SSE stream disconnected in the middle of a turnbefore completion.
    ResponseStreamDisconnected {
        http_status_code: Option<u16>,
    },
    /// Reached the retry limit for responses.
    ResponseTooManyFailedAttempts {
        http_status_code: Option<u16>,
    },
    /// Returned when `turn/start` or `turn/steer` is submitted while the current active turn
    /// cannot accept same-turn steering, for example `/review` or manual `/compact`.
    ActiveTurnNotSteerable {
        turn_kind: NonSteerableTurnKind,
    },
    // Retained to deserialize errors recorded in legacy rollouts.
    ThreadRollbackFailed,
    Other,
}

impl CodexErrorInfo {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        match self {
            Self::ThreadRollbackFailed | Self::ActiveTurnNotSteerable { .. } => false,
            Self::ContextWindowExceeded
            | Self::SessionBudgetExceeded
            | Self::UsageLimitExceeded
            | Self::RateLimitExceeded
            | Self::FlexUnavailable
            | Self::ServerOverloaded
            | Self::ModelNotFound
            | Self::PolicyCapacityUnavailable
            | Self::CyberPolicy
            | Self::BioPolicy
            | Self::MisalignmentPolicyViolation
            | Self::TooManyDenials
            | Self::HttpConnectionFailed { .. }
            | Self::ResponseStreamConnectionFailed { .. }
            | Self::InternalServerError
            | Self::Unauthorized
            | Self::BadRequest
            | Self::InvalidPrompt
            | Self::SandboxError
            | Self::ResponseStreamDisconnected { .. }
            | Self::ResponseTooManyFailedAttempts { .. }
            | Self::Other => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct RawResponseItemEvent {
    pub item: ResponseItem,
}

/// Observer events that a private session forwards without modifying its parent's history.
#[derive(Debug, Clone)]
pub struct PrivateSessionEvent {
    pub thread_id: ThreadId,
    pub event: PrivateSessionEventPayload,
}

/// Recorded state and cancellation needed by observers awaiting response acceptance.
#[derive(Debug, Clone)]
pub enum PrivateSessionEventPayload {
    RecordedItem(RawResponseItemEvent),
    TurnAborted(TurnAbortedEvent),
}

/// Exact usage and metadata reported by one upstream Responses API completion.
///
/// Unlike TokenCountEvent, this is not accumulated, estimated, or replayed.
#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct RawResponseCompletedEvent {
    pub response_id: String,
    pub token_usage: Option<TokenUsage>,
    pub usage_metadata: Option<crate::ResponseUsageMetadata>,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemStartedEvent {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub item: TurnItem,
    pub started_at_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemCompletedEvent {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub item: TurnItem,
    /// Native tokens in the completed assistant message, including any sampled footer.
    /// Completion telemetry only; absent for items without a producer-supplied count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub content_token_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub started_at_ms: Option<i64>,
    // Old rollout files may contain ItemCompleted events for PlanItem without
    // this field. Default to 0 so those persisted rollouts still deserialize
    // after tightening the core event contract.
    #[serde(default = "default_item_completed_at_ms")]
    pub completed_at_ms: i64,
}

const fn default_item_completed_at_ms() -> i64 {
    0
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct AgentMessageContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct PlanDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningRawContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub content_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct EnteredReviewModeEvent {
    pub target: ReviewTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user_facing_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExitedReviewModeEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub item_id: Option<String>,
    pub review_output: Option<ReviewOutputEvent>,
}

// Individual event payload types matching each `EventMsg` variant.

/// Public, customer-facing details supplied by the Responses API for a misalignment block.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct MisalignmentErrorDetails {
    /// Open-ended classification; new values must not prevent the error from being surfaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// A localized explanation is required before a client may offer continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detailed_explanation: Option<String>,
    /// Model-visible instruction to submit if the user elects to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer: Option<MisalignmentSteer>,
    /// Opaque server-issued block target, echoed verbatim only on explicit continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_target: Option<String>,
}

impl fmt::Debug for MisalignmentErrorDetails {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MisalignmentErrorDetails")
            .field("error_type", &self.error_type)
            .field(
                "has_detailed_explanation",
                &self.detailed_explanation.is_some(),
            )
            .field("has_steer", &self.steer.is_some())
            .field("has_review_target", &self.review_target.is_some())
            .finish()
    }
}

/// Public steering instruction returned alongside a resumable misalignment block.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct MisalignmentSteer {
    pub message: String,
}

impl fmt::Debug for MisalignmentSteer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MisalignmentSteer")
            .field("message", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ErrorEvent {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
    /// Host-only file identities proven rejected against the submitted request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    #[ts(skip)]
    pub rejected_audio_file_ids: Vec<String>,
    /// Sensitive explanation and steering are delivered live but never enter rollout storage.
    #[serde(skip)]
    #[schemars(skip)]
    #[ts(skip)]
    pub misalignment: Option<MisalignmentErrorDetails>,
}

impl ErrorEvent {
    /// Whether this error should mark the current turn as failed when replaying history.
    pub fn affects_turn_status(&self) -> bool {
        self.codex_error_info
            .as_ref()
            .is_none_or(CodexErrorInfo::affects_turn_status)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WarningEvent {
    pub message: String,
}

/// User-facing progress for provider-owned authentication recovery.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct AuthRecoveryEvent {
    /// Display name of the model provider whose authentication is recovering.
    pub provider: String,
    /// User-facing description of the authentication recovery stage.
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRerouteReason {
    HighRiskCyberActivity,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelRerouteEvent {
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelVerification {
    TrustedAccessForCyber,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelVerificationEvent {
    pub verifications: Vec<ModelVerification>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TargetedAnswerModeration {
    pub item_id: String,
    pub moderated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display: Option<crate::items::AgentMessageDisplay>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TurnModerationMetadataEvent {
    pub metadata: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct SafetyBufferingEvent {
    pub model: String,
    pub use_cases: Vec<String>,
    pub reasons: Vec<String>,
    pub show_buffering_ui: bool,
    pub faster_model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ContextCompactedEvent;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnCompleteEvent {
    /// Resolved causal root for this turn; absent on older or synthetic events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root_turn_id: Option<String>,
    pub turn_id: String,
    pub last_agent_message: Option<String>,
    /// Terminal error details when the turn completed unsuccessfully.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<ErrorEvent>,
    /// Unix timestamp (in seconds) when the turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    /// Unix timestamp (in seconds) when the turn completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub completed_at: Option<i64>,
    /// Duration between turn start and completion in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub duration_ms: Option<i64>,
    /// Duration between turn start and the first model token in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub time_to_first_token_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnStartedEvent {
    /// Provenance of a regular turn, persisted before startup work can be suspended.
    /// Absent on older records and non-regular tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_attribution: Option<crate::turn_input::TurnAttribution>,
    pub turn_id: String,
    /// ID of the originating turn in the root thread; equals `turn_id` for root turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root_turn_id: Option<String>,
    // Persist for rollout consumers that correlate turns with telemetry traces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub trace_id: Option<String>,
    /// Unix timestamp (in seconds) when the turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    // TODO(aibrahim): make this not optional
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub collaboration_mode_kind: ModeKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ThreadSettingsAppliedEvent {
    /// Logical task that owns this snapshot, independent of the physical rollout file.
    /// Absent in older histories; copied snapshots retain their original owner's ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_id: Option<ThreadId>,
    pub thread_settings: ThreadSettingsSnapshot,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ThreadSettingsSnapshot {
    pub model: String,
    pub model_provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub approval_policy: AskForApproval,
    pub approvals_reviewer: ApprovalsReviewer,
    /// Original selection before managed policy resolved the effective reviewer.
    /// Older histories contain only `approvals_reviewer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub requested_approvals_reviewer: Option<ApprovalsReviewer>,
    pub permission_profile: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,
    pub cwd: AbsolutePathBuf,
    /// Top-level runtime workspace roots for default environments, excluding roots
    /// supplied by explicit environment selections or permission profiles.
    /// An absent value means unknown; an empty list means no roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_workspace_roots: Option<Vec<AbsolutePathBuf>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummaryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    pub collaboration_mode: CollaborationMode,
    /// Thread-owned plugin selection, retained even when a plugin is unavailable.
    #[serde(default)]
    pub disabled_plugin_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
pub enum ReviewTarget {
    /// Review the working tree: staged, unstaged, and untracked files.
    UncommittedChanges,

    /// Review changes between the current branch and the given base branch.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    BaseBranch { branch: String },

    /// Review the changes introduced by a specific commit.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Commit {
        sha: String,
        /// Optional human-readable label (e.g., commit subject) for UIs.
        title: Option<String>,
    },

    /// Arbitrary instructions provided by the user.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Custom { instructions: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
/// Review request sent to the review session.
pub struct ReviewRequest {
    pub target: ReviewTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user_facing_hint: Option<String>,
}

/// Structured review result produced by a child review session.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewOutputEvent {
    pub findings: Vec<ReviewFinding>,
    pub overall_correctness: String,
    pub overall_explanation: String,
    pub overall_confidence_score: f32,
}

impl Default for ReviewOutputEvent {
    fn default() -> Self {
        Self {
            findings: Vec::new(),
            overall_correctness: String::default(),
            overall_explanation: String::default(),
            overall_confidence_score: 0.0,
        }
    }
}

/// A single review finding describing an observed issue or recommendation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewFinding {
    pub title: String,
    pub body: String,
    pub confidence_score: f32,
    pub priority: i32,
    pub code_location: ReviewCodeLocation,
}

/// Location of the code related to a review finding.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewCodeLocation {
    pub absolute_file_path: PathBuf,
    pub line_range: ReviewLineRange,
}

/// Inclusive line range in a file associated with the finding.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewLineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct DeprecationNoticeEvent {
    /// Concise summary of what is deprecated.
    pub summary: String,
    /// Optional extra guidance, such as migration steps or rationale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadRolledBackEvent {
    /// Number of user turns that were removed from context.
    pub num_turns: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamErrorEvent {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
    /// Optional details about the underlying stream failure (often the same
    /// human-readable message that is surfaced as the terminal error if retries
    /// are exhausted).
    #[serde(default)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamInfoEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnAbortedEvent {
    /// Resolved causal root for this turn; absent on older or synthetic events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root_turn_id: Option<String>,
    pub turn_id: Option<String>,
    pub reason: TurnAbortReason,
    /// Optional error describing why the turn was interrupted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<ErrorEvent>,
    /// Unix timestamp (in seconds) when the turn started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    /// Unix timestamp (in seconds) when the turn was aborted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub completed_at: Option<i64>,
    /// Duration between turn start and abort in milliseconds, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    Interrupted,
    Replaced,
    ReviewEnded,
    BudgetLimited,
}
