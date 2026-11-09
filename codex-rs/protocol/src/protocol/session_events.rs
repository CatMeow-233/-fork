//! Session-level events: MCP startup, skills, session configuration, and thread goals.

use super::*;

// Conversation kept for backward compatibility.
/// Response payload for `Op::GetHistory` containing the current session's
/// in-memory transcript.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ConversationPathResponseEvent {
    pub conversation_id: ThreadId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupUpdateEvent {
    /// Server name being started.
    pub server: String,
    /// Current startup status.
    pub status: McpStartupStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "state")]
#[ts(rename_all = "snake_case", tag = "state")]
pub enum McpStartupStatus {
    Starting,
    Ready,
    Failed {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional = nullable)]
        reason: Option<McpStartupFailureReason>,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum McpStartupFailureReason {
    ReauthenticationRequired,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, Default)]
pub struct McpStartupCompleteEvent {
    pub ready: Vec<String>,
    pub failed: Vec<McpStartupFailure>,
    pub cancelled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupFailure {
    pub server: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum McpAuthStatus {
    Unknown,
    Unsupported,
    NotLoggedIn,
    BearerToken,
    OAuth,
}

impl fmt::Display for McpAuthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            McpAuthStatus::Unknown => "Unknown",
            McpAuthStatus::Unsupported => "Unsupported",
            McpAuthStatus::NotLoggedIn => "Not logged in",
            McpAuthStatus::BearerToken => "Bearer token",
            McpAuthStatus::OAuth => "OAuth",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum Product {
    #[serde(alias = "CHATGPT")]
    Chatgpt,
    #[serde(alias = "CODEX")]
    Codex,
    #[serde(alias = "ATLAS")]
    Atlas,
}
impl Product {
    pub fn to_app_platform(self) -> &'static str {
        match self {
            Self::Chatgpt => "chat",
            Self::Codex => "codex",
            Self::Atlas => "atlas",
        }
    }

    pub fn from_session_source_name(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "chatgpt" => Some(Self::Chatgpt),
            "codex" => Some(Self::Codex),
            "atlas" => Some(Self::Atlas),
            _ => None,
        }
    }

    pub fn matches_product_restriction(&self, products: &[Product]) -> bool {
        products.is_empty() || products.contains(self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SkillScope {
    User,
    Repo,
    System,
    Admin,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Legacy short_description from SKILL.md. Prefer SKILL.json interface.short_description.
    pub short_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interface: Option<SkillInterface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dependencies: Option<SkillDependencies>,
    pub path: AbsolutePathBuf,
    pub scope: SkillScope,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillInterface {
    #[ts(optional)]
    pub display_name: Option<String>,
    #[ts(optional)]
    pub short_description: Option<String>,
    #[ts(optional)]
    pub icon_small: Option<AbsolutePathBuf>,
    #[ts(optional)]
    pub icon_large: Option<AbsolutePathBuf>,
    #[ts(optional)]
    pub brand_color: Option<String>,
    #[ts(optional)]
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillDependencies {
    pub tools: Vec<SkillToolDependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillToolDependency {
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub r#type: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SessionNetworkProxyRuntime {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, TS)]
pub struct SessionConfiguredEvent {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    /// Optional analytics source classification for this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,

    /// Optional user-facing thread name (may be unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_name: Option<String>,

    /// Tell the client what model is being queried.
    pub model: String,

    pub model_provider_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// When to escalate for approval for execution
    pub approval_policy: AskForApproval,

    /// Configures who approval requests are routed to for review once they have
    /// been escalated. This does not disable separate safety checks such as
    /// ARC.
    #[serde(default)]
    pub approvals_reviewer: ApprovalsReviewer,

    /// Canonical effective permissions for commands executed in the session.
    pub permission_profile: PermissionProfile,

    /// Named or implicit built-in profile that produced `permission_profile`,
    /// when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,

    /// Working directory that should be treated as the *root* of the
    /// session.
    pub cwd: AbsolutePathBuf,

    /// The effort the model is putting into reasoning about the user's request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,

    /// Runtime proxy bind addresses, when the managed proxy was started for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network_proxy: Option<SessionNetworkProxyRuntime>,

    /// Path in which the rollout is stored. Can be `None` for ephemeral threads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollout_path: Option<PathBuf>,
}

impl<'de> Deserialize<'de> for SessionConfiguredEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            session_id: SessionId,
            #[serde(default)]
            thread_id: Option<ThreadId>,
            forked_from_id: Option<ThreadId>,
            parent_thread_id: Option<ThreadId>,
            #[serde(default)]
            thread_source: Option<ThreadSource>,
            #[serde(default)]
            thread_name: Option<String>,
            model: String,
            model_provider_id: String,
            service_tier: Option<String>,
            approval_policy: AskForApproval,
            #[serde(default)]
            approvals_reviewer: ApprovalsReviewer,
            // `SessionConfiguredEvent` is persisted into rollout history. Older
            // rollouts only have `sandbox_policy`, so accept it on deserialize
            // and immediately project it into the canonical `permission_profile`.
            sandbox_policy: Option<SandboxPolicy>,
            permission_profile: Option<PermissionProfile>,
            #[serde(default)]
            active_permission_profile: Option<ActivePermissionProfile>,
            cwd: AbsolutePathBuf,
            reasoning_effort: Option<ReasoningEffortConfig>,
            network_proxy: Option<SessionNetworkProxyRuntime>,
            rollout_path: Option<PathBuf>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let permission_profile = match (wire.permission_profile, wire.sandbox_policy) {
            (Some(permission_profile), _) => permission_profile,
            (None, Some(sandbox_policy)) => PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &sandbox_policy,
                wire.cwd.as_path(),
            ),
            (None, None) => {
                return Err(serde::de::Error::missing_field("permission_profile"));
            }
        };

        Ok(Self {
            session_id: wire.session_id,
            thread_id: wire.thread_id.unwrap_or_else(|| wire.session_id.into()),
            forked_from_id: wire.forked_from_id,
            parent_thread_id: wire.parent_thread_id,
            thread_source: wire.thread_source,
            thread_name: wire.thread_name,
            model: wire.model,
            model_provider_id: wire.model_provider_id,
            service_tier: wire.service_tier,
            approval_policy: wire.approval_policy,
            approvals_reviewer: wire.approvals_reviewer,
            permission_profile,
            active_permission_profile: wire.active_permission_profile,
            cwd: wire.cwd,
            reasoning_effort: wire.reasoning_effort,
            network_proxy: wire.network_proxy,
            rollout_path: wire.rollout_path,
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub enum ThreadGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

pub const MAX_THREAD_GOAL_OBJECTIVE_CHARS: usize = 4_000;

pub fn validate_thread_goal_objective(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("goal objective must not be empty".to_string());
    }
    if value.chars().count() > MAX_THREAD_GOAL_OBJECTIVE_CHARS {
        return Err(format!(
            "goal objective must be at most {MAX_THREAD_GOAL_OBJECTIVE_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoal {
    pub thread_id: ThreadId,
    pub objective: String,
    pub status: ThreadGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoalUpdatedEvent {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    pub goal: ThreadGoal,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadQueueChangedEvent {
    pub thread_id: ThreadId,
}
