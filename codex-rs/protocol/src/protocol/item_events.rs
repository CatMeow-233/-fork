//! Per-item events for messages, reasoning, tool calls, command execution, and patches.

use super::*;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentMessageEvent {
    /// Legacy history assigns synthetic item IDs. Preserve the native ID for a moderated answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub moderation: Option<TargetedAnswerModeration>,
    pub message: String,
    #[serde(default)]
    pub phase: Option<MessagePhase>,
    #[serde(default)]
    pub memory_citation: Option<MemoryCitation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delivery: Option<AgentMessageDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub questions: Option<Vec<AsyncUserInputQuestion>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum UserMessageImageKind {
    Inline,
    File,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum UserMessageAudioKind {
    Inline,
    File,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct UserMessageEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub message: String,
    /// Image URLs sourced from `UserInput::Image`. These are safe
    /// to replay in legacy UI history events and correspond to images sent to
    /// the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Detail hints for `images`, indexed in parallel. Missing entries imply
    /// default image detail behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_details: Vec<Option<ImageDetail>>,
    /// File IDs sourced from `UserInput::Image`. These are passed through as
    /// opaque references and are not created by image preparation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_ids: Option<Vec<String>>,
    /// Detail hints for `file_ids`, indexed in parallel. Missing entries imply
    /// default image detail behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_id_details: Vec<Option<ImageDetail>>,
    /// Inline and file-backed image kinds in their original input order.
    /// New producers populate this alongside `images` and `file_ids`; when it
    /// is absent, consumers retain the legacy inline-then-file ordering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_order: Vec<UserMessageImageKind>,
    /// Local file paths sourced from `UserInput::LocalImage`. These are kept so
    /// the UI can reattach images when editing history. Local image prompts may
    /// include a display form of the path, but these should not be treated as
    /// API-ready URLs.
    #[serde(default)]
    pub local_images: Vec<std::path::PathBuf>,
    /// Detail hints for `local_images`, indexed in parallel. Missing entries
    /// imply default image detail behavior.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_image_details: Vec<Option<ImageDetail>>,
    /// Audio URLs sourced from `UserInput::Audio`. These are safe to replay in
    /// legacy UI history events and correspond to audio sent to the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Vec<String>>,
    /// File-backed audio references. When `audio_order` is complete, consumers
    /// combine this with `audio` without duplicating inline payloads. Older
    /// producers may instead store the complete ordered reference list here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_references: Option<Vec<AudioReference>>,
    /// Inline and file-backed audio kinds in their original input order.
    /// New mixed-input producers populate this alongside the split `audio` and
    /// `audio_references` fields; when absent, consumers retain the prior shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_order: Vec<UserMessageAudioKind>,
    /// Local file paths sourced from `UserInput::LocalAudio`. These are kept so
    /// clients can reattach audio when editing history and should not be
    /// treated as API-ready URLs.
    #[serde(default)]
    pub local_audio: Vec<std::path::PathBuf>,
    /// UI-defined spans within `message` used to render or persist special elements.
    #[serde(default)]
    pub text_elements: Vec<crate::user_input::TextElement>,
}

impl UserMessageEvent {
    /// Returns whether `image_order` accounts for every split image reference exactly once.
    pub fn has_complete_image_order(&self) -> bool {
        if self.image_order.is_empty() {
            return false;
        }

        let mut inline_count = 0;
        let mut file_count = 0;
        for image_kind in &self.image_order {
            match image_kind {
                UserMessageImageKind::Inline => inline_count += 1,
                UserMessageImageKind::File => file_count += 1,
            }
        }

        inline_count == self.images.as_ref().map_or(0, Vec::len)
            && file_count == self.file_ids.as_ref().map_or(0, Vec::len)
    }

    /// Returns whether `audio_order` accounts for every split audio reference exactly once.
    pub fn has_complete_audio_order(&self) -> bool {
        if self.audio_order.is_empty() {
            return false;
        }

        let mut inline_count = 0;
        let mut file_count = 0;
        for audio_kind in &self.audio_order {
            match audio_kind {
                UserMessageAudioKind::Inline => inline_count += 1,
                UserMessageAudioKind::File => file_count += 1,
            }
        }

        inline_count == self.audio.as_ref().map_or(0, Vec::len)
            && file_count == self.audio_references.as_ref().map_or(0, Vec::len)
            && self.audio_references.as_ref().is_none_or(|references| {
                references
                    .iter()
                    .all(|reference| matches!(reference, AudioReference::File { .. }))
            })
    }
}

/// Returns the user-facing preview text for a user message.
pub fn user_message_preview(user: &UserMessageEvent) -> Option<String> {
    let message = strip_user_message_prefix(user.message.as_str());
    if !message.is_empty() {
        return Some(message.to_string());
    }
    if user
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
        || user
            .file_ids
            .as_ref()
            .is_some_and(|file_ids| !file_ids.is_empty())
        || !user.local_images.is_empty()
    {
        return Some("[Image]".to_string());
    }
    if user.audio.as_ref().is_some_and(|audio| !audio.is_empty())
        || user
            .audio_references
            .as_ref()
            .is_some_and(|audio| !audio.is_empty())
        || !user.local_audio.is_empty()
    {
        return Some("[Audio]".to_string());
    }
    None
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningRawContentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningSectionBreakEvent {
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpInvocation {
    /// Name of the MCP server as defined in the config.
    pub server: String,
    /// Name of the tool as given by the MCP server.
    pub tool: String,
    /// Arguments to the tool call.
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallBeginEvent {
    /// Identifier so this can be paired with the McpToolCallEnd event.
    pub call_id: String,
    /// Originating turn; absent in older rollout records.
    #[serde(default)]
    pub turn_id: String,
    pub invocation: McpInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_ui: Option<crate::items::McpAppUi>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
    /// Whether the selected tool is annotated as read-only, not its execution outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub read_only_hint: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallEndEvent {
    /// Identifier for the corresponding McpToolCallBegin that finished.
    pub call_id: String,
    /// Originating turn; absent in older rollout records.
    #[serde(default)]
    pub turn_id: String,
    pub invocation: McpInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_ui: Option<crate::items::McpAppUi>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub read_only_hint: Option<bool>,
    #[ts(type = "string")]
    pub duration: Duration,
    /// Result of the tool call. Note this could be an error.
    pub result: Result<CallToolResult, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct DynamicToolCallResponseEvent {
    /// Identifier for the corresponding DynamicToolCallRequest.
    pub call_id: String,
    /// Turn ID that this dynamic tool call belongs to.
    pub turn_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// Dynamic tool namespace, when one was provided.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Dynamic tool name.
    pub tool: String,
    /// Dynamic tool call arguments.
    pub arguments: serde_json::Value,
    /// Dynamic tool response content items.
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    /// Whether the tool call succeeded.
    pub success: bool,
    /// Optional error text when the tool call failed before producing a response.
    pub error: Option<String>,
    /// The duration of the dynamic tool call.
    #[ts(type = "string")]
    pub duration: Duration,
}

impl McpToolCallEndEvent {
    pub fn is_success(&self) -> bool {
        match &self.result {
            Ok(result) => !result.is_error.unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: String,
    pub action: WebSearchAction,
    /// Structured search results returned out-of-band by standalone web search.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub results: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationEndEvent {
    pub call_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revised_prompt: Option<String>,
    #[serde(flatten)]
    pub image: GeneratedImageRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transparent_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure: Option<ImageGenerationFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub saved_path: Option<AbsolutePathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<_>", optional)]
    pub input_images: Vec<ImageGenerationInputImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub generation_id: Option<String>,
}

#[derive(
    Debug, Clone, Copy, Display, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandSource {
    #[default]
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandBeginEvent {
    /// Identifier so this can be paired with the ExecCommandEnd event.
    pub call_id: String,
    /// Trusted first-party plugin attributed to this command, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
    /// Safe plugin-relative path attributed to this command, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub script_path: Option<String>,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    /// The command to be executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathUri,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandEndEvent {
    /// Identifier for the ExecCommandBegin that finished.
    pub call_id: String,
    /// Trusted first-party plugin attributed to this command, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
    /// Safe plugin-relative path attributed to this command, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub script_path: Option<String>,
    /// Identifier for the underlying PTY process (when available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    /// Turn ID that this command belongs to.
    pub turn_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    /// The command that was executed.
    pub command: Vec<String>,
    /// The command's working directory if not the default cwd for the agent.
    pub cwd: PathUri,
    pub parsed_cmd: Vec<ParsedCommand>,
    /// Where the command originated. Defaults to Agent for backward compatibility.
    #[serde(default)]
    pub source: ExecCommandSource,
    /// Raw input sent to a unified exec session (if this is an interaction event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,

    /// Captured aggregated output
    #[serde(default)]
    pub aggregated_output: String,
    /// The command's exit code.
    pub exit_code: i32,
    /// The duration of the command execution.
    #[ts(type = "string")]
    pub duration: Duration,
    /// Completion status for this command execution.
    pub status: ExecCommandStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ViewImageToolCallEvent {
    /// Identifier for the originating tool call.
    pub call_id: String,
    /// Filesystem path resolved for the selected environment.
    ///
    /// This core event is not exposed directly in the app-server API. App-server
    /// converts the path to `LegacyAppPathString` when building its public item.
    pub path: PathUri,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ExecCommandOutputDeltaEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Which stream produced this chunk.
    pub stream: ExecOutputStream,
    /// Raw bytes from the stream (may not be valid UTF-8).
    #[serde_as(as = "serde_with::base64::Base64")]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    pub chunk: Vec<u8>,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TerminalInteractionEvent {
    /// Identifier for the ExecCommandBegin that produced this chunk.
    pub call_id: String,
    /// Process id associated with the running command.
    pub process_id: String,
    /// Stdin sent to the running session.
    pub stdin: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyBeginEvent {
    /// Identifier so this can be paired with the PatchApplyEnd event.
    pub call_id: String,
    /// Turn ID that this patch belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    /// If true, there was no ApplyPatchApprovalRequest for this patch.
    pub auto_approved: bool,
    /// The changes to be applied.
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyUpdatedEvent {
    /// Identifier for the originating `apply_patch` tool call.
    pub call_id: String,
    /// Structured file changes parsed from the model-generated patch input so far.
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyEndEvent {
    /// Identifier for the PatchApplyBegin that finished.
    pub call_id: String,
    /// Turn ID that this patch belongs to.
    /// Uses `#[serde(default)]` for backwards compatibility.
    #[serde(default)]
    pub turn_id: String,
    /// Captured stdout (summary printed by apply_patch).
    pub stdout: String,
    /// Captured stderr (parser errors, IO failures, etc.).
    pub stderr: String,
    /// Whether the patch was applied successfully.
    pub success: bool,
    /// The changes that were applied (mirrors PatchApplyBeginEvent::changes).
    #[serde(default)]
    pub changes: HashMap<PathBuf, FileChange>,
    /// Completion status for this patch application.
    pub status: PatchApplyStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnDiffEvent {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum FileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct Chunk {
    /// 1-based line index of the first line in the original file
    pub orig_index: u32,
    pub deleted_lines: Vec<String>,
    pub inserted_lines: Vec<String>,
}
