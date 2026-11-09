//! Session provenance and persisted rollout metadata such as `SessionMeta` and `TurnContextItem`.

use super::*;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum SessionSource {
    Cli,
    #[default]
    VSCode,
    Exec,
    Mcp,
    Custom(String),
    Internal(InternalSessionSource),
    SubAgent(SubAgentSource),
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
#[ts(type = "string")]
pub enum ThreadSource {
    User,
    Subagent,
    GuardianReview,
    Feature(String),
    MemoryConsolidation,
}

impl ThreadSource {
    pub fn as_str(&self) -> &str {
        match self {
            ThreadSource::User => "user",
            ThreadSource::Subagent => "subagent",
            ThreadSource::GuardianReview => "guardian_review",
            ThreadSource::Feature(feature) => feature,
            ThreadSource::MemoryConsolidation => "memory_consolidation",
        }
    }
}

impl fmt::Display for ThreadSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for ThreadSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ThreadSource> for String {
    fn from(value: ThreadSource) -> Self {
        value.to_string()
    }
}

impl FromStr for ThreadSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user" => Ok(ThreadSource::User),
            "subagent" => Ok(ThreadSource::Subagent),
            "guardian_review" => Ok(ThreadSource::GuardianReview),
            "memory_consolidation" => Ok(ThreadSource::MemoryConsolidation),
            other => Ok(ThreadSource::Feature(other.to_string())),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum InternalSessionSource {
    MemoryConsolidation,
    Guardian,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SubAgentSource {
    Review,
    Compact,
    ThreadSpawn {
        parent_thread_id: ThreadId,
        depth: i32,
        #[serde(default)]
        agent_path: Option<AgentPath>,
        #[serde(default)]
        agent_nickname: Option<String>,
        #[serde(default, alias = "agent_type")]
        agent_role: Option<String>,
    },
    MemoryConsolidation,
    Other(String),
}

impl fmt::Display for SessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionSource::Cli => f.write_str("cli"),
            SessionSource::VSCode => f.write_str("vscode"),
            SessionSource::Exec => f.write_str("exec"),
            SessionSource::Mcp => f.write_str("mcp"),
            SessionSource::Custom(source) => f.write_str(source),
            SessionSource::Internal(source) => write!(f, "internal_{source}"),
            SessionSource::SubAgent(sub_source) => write!(f, "subagent_{sub_source}"),
            SessionSource::Unknown => f.write_str("unknown"),
        }
    }
}

impl SessionSource {
    pub fn from_startup_arg(value: &str) -> Result<Self, &'static str> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("session source must not be empty");
        }

        let normalized = trimmed.to_ascii_lowercase();
        Ok(match normalized.as_str() {
            "cli" => SessionSource::Cli,
            "vscode" => SessionSource::VSCode,
            "exec" => SessionSource::Exec,
            "mcp" | "appserver" | "app-server" | "app_server" => SessionSource::Mcp,
            "unknown" => SessionSource::Unknown,
            _ => SessionSource::Custom(normalized),
        })
    }

    pub fn is_internal(&self) -> bool {
        matches!(self, SessionSource::Internal(_))
    }

    pub fn is_non_root_agent(&self) -> bool {
        matches!(
            self,
            SessionSource::Internal(_) | SessionSource::SubAgent(_)
        )
    }

    pub fn get_nickname(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_nickname, .. }) => {
                agent_nickname.clone()
            }
            _ => None,
        }
    }

    pub fn get_agent_role(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_role, .. }) => {
                agent_role.clone()
            }
            _ => None,
        }
    }

    pub fn get_agent_path(&self) -> Option<AgentPath> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) => {
                agent_path.clone()
            }
            _ => None,
        }
    }

    pub fn restriction_product(&self) -> Option<Product> {
        match self {
            SessionSource::Custom(source) => Product::from_session_source_name(source),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Unknown => Some(Product::Codex),
            SessionSource::Internal(_) | SessionSource::SubAgent(_) => None,
        }
    }

    pub fn matches_product_restriction(&self, products: &[Product]) -> bool {
        products.is_empty()
            || self
                .restriction_product()
                .is_some_and(|product| product.matches_product_restriction(products))
    }

    pub fn parent_thread_id(&self) -> Option<ThreadId> {
        match self {
            SessionSource::SubAgent(subagent_source) => subagent_source.parent_thread_id(),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Custom(_)
            | SessionSource::Internal(_)
            | SessionSource::Unknown => None,
        }
    }
}

impl fmt::Display for SubAgentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubAgentSource::Review => f.write_str("review"),
            SubAgentSource::Compact => f.write_str("compact"),
            SubAgentSource::MemoryConsolidation => f.write_str("memory_consolidation"),
            SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                ..
            } => {
                write!(f, "thread_spawn_{parent_thread_id}_d{depth}")
            }
            SubAgentSource::Other(other) => f.write_str(other),
        }
    }
}

impl SubAgentSource {
    pub fn kind(&self) -> &str {
        match self {
            SubAgentSource::Review => "review",
            SubAgentSource::Compact => "compact",
            SubAgentSource::ThreadSpawn { .. } => "thread_spawn",
            SubAgentSource::MemoryConsolidation => "memory_consolidation",
            SubAgentSource::Other(other) => other,
        }
    }

    pub fn parent_thread_id(&self) -> Option<ThreadId> {
        match self {
            SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            } => Some(*parent_thread_id),
            SubAgentSource::Review
            | SubAgentSource::Compact
            | SubAgentSource::MemoryConsolidation
            | SubAgentSource::Other(_) => None,
        }
    }
}

impl fmt::Display for InternalSessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InternalSessionSource::MemoryConsolidation => f.write_str("memory_consolidation"),
            InternalSessionSource::Guardian => f.write_str("guardian"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum MultiAgentVersion {
    Disabled,
    V1,
    V2,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct SessionContextWindow {
    /// UUIDv7 identity of this context window.
    pub window_id: String,
}

impl SessionContextWindow {
    pub fn new(window_id: String) -> Self {
        Self { window_id }
    }
}

/// Exclusive position in another rollout's paginated history.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct HistoryPosition {
    /// Rollout ID for the immutable prefix file.
    ///
    /// `HistoryPosition` predates `thread/revert`, so this field is named `thread_id`. Treat its
    /// value as a `rollout_id`: ordinary rollouts use the thread ID as their rollout ID, while a
    /// reverted thread's filename carries a distinct rollout ID. It is not necessarily
    /// [`SessionMeta::id`], which remains the stable thread ID across revert.
    pub thread_id: ThreadId,
    /// First rollout ordinal not included from the prefix file.
    pub end_ordinal_exclusive: u64,
    /// Byte offset immediately after the last included JSONL record from the prefix file.
    pub end_byte_offset: u64,
}

/// SessionMeta contains session-level data that doesn't correspond to a specific turn.
///
/// NOTE: There used to be an `instructions` field here, which stored user_instructions, but we
/// now save that on TurnContext. base_instructions stores the base instructions for the session,
/// and should be used when there is no config override.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct SessionMeta {
    /// Scope discovery to the creator. Absent on threads created before account filtering.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub account_scoped: bool,
    /// ChatGPT user that created this thread; absent when unavailable or for older threads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_user_id: Option<String>,
    /// ChatGPT account selected when this thread was created. Never updated on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_account_id: Option<String>,
    /// session_id is equal to the root thread's ID.
    pub session_id: SessionId,
    pub id: ThreadId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<ThreadId>,
    /// Marks a persisted fork as attached to `forked_from_id`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub attached_thread: bool,
    /// Exclusive ordinal inherited from the logical fork parent, independent of `history_base`.
    /// Revert may replace the physical history base while retaining this fork boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_ordinal_exclusive: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    pub timestamp: String,
    pub cwd: PathBuf,
    /// Top-level runtime workspace roots at creation for default environments,
    /// excluding roots supplied by explicit environment selections or permission profiles.
    /// An absent value means unknown; an empty list means no roots.
    /// Keep native paths parseable across hosts; validate them when restoring settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_workspace_roots: Option<Vec<PathBuf>>,
    pub originator: String,
    pub cli_version: String,
    #[serde(default)]
    pub source: SessionSource,
    /// Optional analytics source classification for this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,
    /// Optional random unique nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    /// Optional canonical agent path assigned to an AgentControl-spawned sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    pub model_provider: Option<String>,
    /// base_instructions for the session. This *should* always be present when creating a new session,
    /// but may be missing for older sessions. If not present, fall back to rendering the base_instructions
    /// from ModelsManager.
    pub base_instructions: Option<BaseInstructions>,
    #[serde(
        default,
        deserialize_with = "crate::dynamic_tools::deserialize_dynamic_tool_specs",
        skip_serializing_if = "Option::is_none"
    )]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    /// Capability roots selected for this thread by the hosting platform.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_capability_roots: Vec<SelectedCapabilityRoot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mode: Option<String>,
    #[serde(default)]
    pub history_mode: ThreadHistoryMode,
    /// Exclusive prefix of another paginated rollout inherited by this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_base: Option<HistoryPosition>,
    /// First rollout ordinal that belongs to this subagent's own projected history.
    ///
    /// Earlier rollout records are inherited model context and stay out of child
    /// turn/item projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_history_start_ordinal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
    /// Initial context-window identity for consumers that tail rollout JSONL before compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<SessionContextWindow>,
}

impl Default for SessionMeta {
    fn default() -> Self {
        let id = ThreadId::default();
        SessionMeta {
            account_scoped: false,
            creator_user_id: None,
            creator_account_id: None,
            session_id: id.into(),
            id,
            forked_from_id: None,
            attached_thread: false,
            forked_from_ordinal_exclusive: None,
            parent_thread_id: None,
            timestamp: String::new(),
            cwd: PathBuf::new(),
            runtime_workspace_roots: None,
            originator: String::new(),
            cli_version: String::new(),
            source: SessionSource::default(),
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
            selected_capability_roots: Vec::new(),
            memory_mode: None,
            history_mode: ThreadHistoryMode::default(),
            history_base: None,
            subagent_history_start_ordinal: None,
            multi_agent_version: None,
            context_window: None,
        }
    }
}

#[derive(Serialize, Debug, Clone, JsonSchema, TS)]
pub struct SessionMetaLine {
    #[serde(flatten)]
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

impl<'de> Deserialize<'de> for SessionMetaLine {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SessionMetaLineFields {
            #[serde(flatten)]
            meta: SessionMeta,
            git: Option<GitInfo>,
        }

        let mut value = Value::deserialize(deserializer)?;
        let fields = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("session metadata must be an object"))?;
        if !fields.contains_key("session_id") {
            let thread_id = fields
                .get("id")
                .cloned()
                .ok_or_else(|| D::Error::missing_field("id"))?;
            fields.insert("session_id".to_string(), thread_id);
        }
        let SessionMetaLineFields { meta, git } =
            serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Self { meta, git })
    }
}

/// Persisted comparison state used to resume model-visible world-state diffing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct WorldStateItem {
    /// Full snapshots establish a new baseline; patches update the current baseline.
    pub full: bool,
    pub state: Map<String, Value>,
}

impl WorldStateItem {
    pub fn full(state: Map<String, Value>) -> Self {
        Self { full: true, state }
    }

    pub fn patch(state: Map<String, Value>) -> Self {
        Self { full: false, state }
    }
}

/// Persist once per real user turn after computing that turn's model-visible
/// context updates, and again after mid-turn compaction when replacement
/// history re-establishes full context, so resume/fork replay can recover the
/// latest durable baseline.
#[derive(Serialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct TurnContextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Root turn that owns this turn's attribution, retained across recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_turn_id: Option<String>,
    /// Explicit routing for this turn. An empty list clears the thread's direct namespaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub direct_only_tool_namespaces: Option<Vec<String>>,
    /// Plugin selection captured for this turn. Absent in older histories.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub disabled_plugin_ids: Option<Vec<String>>,
    pub cwd: AbsolutePathBuf,
    pub approval_policy: AskForApproval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub permission_profile: PermissionProfile,
    /// Built-in or named profile that produced `permission_profile`, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cyber_access_program: Option<CyberAccessProgram>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffortConfig>,
    /// Legacy placeholder written as `Some(ReasoningSummaryConfig::None)` for older readers.
    /// Optional so newer readers also accept its future removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummaryConfig>,
}

impl<'de> Deserialize<'de> for TurnContextItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            turn_id: Option<String>,
            root_turn_id: Option<String>,
            direct_only_tool_namespaces: Option<Vec<String>>,
            disabled_plugin_ids: Option<Vec<String>>,
            cwd: AbsolutePathBuf,
            approval_policy: AskForApproval,
            approvals_reviewer: Option<ApprovalsReviewer>,
            // Older histories are normalized here; decoded items retain only the profile.
            sandbox_policy: Option<SandboxPolicy>,
            permission_profile: Option<PermissionProfile>,
            active_permission_profile: Option<ActivePermissionProfile>,
            file_system_sandbox_policy: Option<RawFileSystemSandboxPolicy>,
            model: String,
            comp_hash: Option<String>,
            collaboration_mode: Option<CollaborationMode>,
            multi_agent_version: Option<MultiAgentVersion>,
            realtime_active: Option<bool>,
            cyber_access_program: Option<CyberAccessProgram>,
            effort: Option<ReasoningEffortConfig>,
            summary: Option<ReasoningSummaryConfig>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let permission_profile = match wire.permission_profile {
            Some(permission_profile) => permission_profile,
            None => {
                let sandbox_policy = wire
                    .sandbox_policy
                    .ok_or_else(|| serde::de::Error::missing_field("permission_profile"))?;
                let file_system_sandbox_policy = wire
                    .file_system_sandbox_policy
                    .map(TryInto::try_into)
                    .transpose()
                    // Malformed historical split rules must not regain legacy grants.
                    .unwrap_or_else(|_| Some(FileSystemSandboxPolicy::restricted(Vec::new())))
                    .unwrap_or_else(|| {
                        FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
                            &sandbox_policy,
                            wire.cwd.as_path(),
                        )
                    });
                PermissionProfile::from_runtime_permissions_with_enforcement(
                    SandboxEnforcement::from_legacy_sandbox_policy(&sandbox_policy),
                    &file_system_sandbox_policy,
                    NetworkSandboxPolicy::from(&sandbox_policy),
                )
            }
        };
        Ok(Self {
            turn_id: wire.turn_id,
            root_turn_id: wire.root_turn_id,
            direct_only_tool_namespaces: wire.direct_only_tool_namespaces,
            disabled_plugin_ids: wire.disabled_plugin_ids,
            cwd: wire.cwd,
            approval_policy: wire.approval_policy,
            approvals_reviewer: wire.approvals_reviewer,
            permission_profile,
            active_permission_profile: wire.active_permission_profile,
            model: wire.model,
            comp_hash: wire.comp_hash,
            collaboration_mode: wire.collaboration_mode,
            multi_agent_version: wire.multi_agent_version,
            realtime_active: wire.realtime_active,
            cyber_access_program: wire.cyber_access_program,
            effort: wire.effort,
            summary: wire.summary,
        })
    }
}

/// Estimate the model-visible content length of an encrypted compaction.
///
/// The payload can contain summary text, sampled-token replay caches, and
/// encryption/base64 overhead. Normalize the roughly threefold payload growth
/// observed with replay caches before applying the usual four-byte text
/// heuristic: twelve ciphertext bytes per estimated token, rounded up.
/// This restores the earlier budgeting scale, not exact tokenizer counts, and
/// can underestimate legacy checkpoints without caches. Callers must enforce
/// transport-size limits separately.
pub fn estimate_compaction_content_length(encoded_len: usize) -> usize {
    encoded_len.div_ceil(3)
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", content = "limit", rename_all = "snake_case")]
pub enum TruncationPolicy {
    Bytes(usize),
    Tokens(usize),
}

impl From<crate::openai_models::TruncationPolicyConfig> for TruncationPolicy {
    fn from(config: crate::openai_models::TruncationPolicyConfig) -> Self {
        match config.mode {
            crate::openai_models::TruncationMode::Bytes => Self::Bytes(config.limit as usize),
            crate::openai_models::TruncationMode::Tokens => Self::Tokens(config.limit as usize),
        }
    }
}

impl TruncationPolicy {
    pub fn token_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                usize::try_from(codex_utils_string::approx_tokens_from_byte_count(*bytes))
                    .unwrap_or(usize::MAX)
            }
            TruncationPolicy::Tokens(tokens) => *tokens,
        }
    }

    pub fn byte_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => *bytes,
            TruncationPolicy::Tokens(tokens) => {
                codex_utils_string::approx_bytes_for_tokens(*tokens)
            }
        }
    }
}

impl Mul<f64> for TruncationPolicy {
    type Output = Self;

    fn mul(self, multiplier: f64) -> Self::Output {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                TruncationPolicy::Bytes((bytes as f64 * multiplier).ceil() as usize)
            }
            TruncationPolicy::Tokens(tokens) => {
                TruncationPolicy::Tokens((tokens as f64 * multiplier).ceil() as usize)
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct GitInfo {
    /// Current commit hash (SHA)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<GitSha>,
    /// Current branch name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Repository URL (if available from remote)
    #[serde(
        default,
        deserialize_with = "crate::sanitized_git_url::deserialize_optional_sanitized_git_url",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub repository_url: Option<SanitizedGitUrl>,
}
