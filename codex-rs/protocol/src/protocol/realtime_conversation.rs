//! Realtime conversation start parameters, voices, transports, and streamed realtime events.

use super::*;

#[derive(Clone, PartialEq)]
pub struct ConversationStartParams {
    /// Whether Codex response handoffs are managed through explicit client append calls.
    pub client_managed_handoffs: bool,
    /// Whether a realtime V3 delegation produces an acknowledgement filler.
    /// `None` preserves the Realtime API's default behavior.
    pub delegation_ack_filler: Option<bool>,
    /// Whether to route any remaining transcript tail through Codex when the session ends.
    /// TODO: Remove this rollout knob once transcript-tail flushing is always enabled.
    pub flush_transcript_tail_on_session_end: bool,
    /// Sends automatic Codex responses as realtime conversation items instead of handoff appends.
    pub codex_responses_as_items: bool,
    /// Optional prefix added to automatic Codex response items when `codex_responses_as_items` is set.
    pub codex_response_item_prefix: Option<String>,
    /// Selects how automatic Codex handoffs are routed in Frameless Bidi sessions.
    /// Realtime V1 and V2 ignore this setting.
    pub codex_response_handoff_mode: CodexResponseHandoffMode,
    /// Relays public reasoning summaries as quiet context for realtime V3 delegations.
    pub backend_reasoning_status: bool,
    /// Optional client-selected BEM prefixes keyed by `analysis`, `commentary`, and `final`.
    pub codex_response_handoff_channel_prefixes: Option<BTreeMap<String, Vec<String>>>,
    /// Overrides the configured realtime model for this session only.
    pub model: Option<String>,
    /// Selects whether the realtime session should produce text or audio output.
    pub output_modality: RealtimeOutputModality,
    /// Whether to append Codex's startup context to the realtime backend prompt.
    pub include_startup_context: bool,
    /// Complete role-bearing text items to include in the initial realtime session history.
    pub initial_items: Vec<ConversationTextParams>,
    /// Developer instructions given to Codex when this realtime session starts.
    pub realtime_start_instructions: Option<String>,
    /// Developer instructions given to Codex when this realtime session ends.
    pub realtime_end_instructions: Option<String>,
    pub prompt: Option<Option<String>>,
    pub realtime_session_id: Option<String>,
    /// Client-generated identifier for the product voice session. This is distinct from the
    /// Realtime API session identifier.
    pub voice_session_id: Option<String>,
    pub transport: Option<ConversationStartTransport>,
    /// Overrides the configured realtime protocol version for this session only.
    pub version: Option<RealtimeConversationVersion>,
    pub voice: Option<RealtimeVoice>,
}

/// A caller-owned realtime sideband. Dropping either direction seals input and ends
/// the attachment after accepted messages drain. The caller supplies bounded channels
/// and one complete JSON message per item.
#[derive(Debug)]
pub struct RealtimeSidebandTransport {
    pub incoming: tokio::sync::mpsc::Receiver<String>,
    pub outgoing: tokio::sync::mpsc::Sender<String>,
}

impl fmt::Debug for ConversationStartParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Text, instructions, and transport endpoints can contain credentials.
        f.debug_struct("ConversationStartParams")
            .field("output_modality", &self.output_modality)
            .field("initial_items_count", &self.initial_items.len())
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationStartTransport {
    Websocket,
    Webrtc {
        sdp: String,
    },
    ExistingCall {
        call_id: String,
        /// Endpoint selected by the embedding runtime for this call's sideband.
        /// This is an in-process override, not a client-supplied API parameter.
        /// `None` uses the configured endpoint or the default public API.
        sideband_base_url: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeOutputModality {
    Text,
    Audio,
}

#[derive(
    Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash, JsonSchema, TS, Ord, PartialOrd,
)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum RealtimeVoice {
    Alloy,
    Arbor,
    Ash,
    Aube,
    Ballad,
    Breeze,
    Bubbie,
    Cedar,
    Coral,
    Cove,
    Echo,
    Ember,
    Haetsal,
    Hanul,
    Himari,
    Juniper,
    Leher,
    Maple,
    Marin,
    Miko,
    Neer,
    Porto,
    Rindo,
    Rio,
    Rivage,
    Sage,
    Selva,
    Shimmer,
    Sol,
    Sonna,
    Spruce,
    Tinta,
    Vale,
    Verse,
    Viola,
}

impl RealtimeVoice {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Alloy => "alloy",
            Self::Arbor => "arbor",
            Self::Ash => "ash",
            Self::Aube => "aube",
            Self::Ballad => "ballad",
            Self::Breeze => "breeze",
            Self::Bubbie => "bubbie",
            Self::Cedar => "cedar",
            Self::Coral => "coral",
            Self::Cove => "cove",
            Self::Echo => "echo",
            Self::Ember => "ember",
            Self::Haetsal => "haetsal",
            Self::Hanul => "hanul",
            Self::Himari => "himari",
            Self::Juniper => "juniper",
            Self::Leher => "leher",
            Self::Maple => "maple",
            Self::Marin => "marin",
            Self::Miko => "miko",
            Self::Neer => "neer",
            Self::Porto => "porto",
            Self::Rindo => "rindo",
            Self::Rio => "rio",
            Self::Rivage => "rivage",
            Self::Sage => "sage",
            Self::Selva => "selva",
            Self::Shimmer => "shimmer",
            Self::Sol => "sol",
            Self::Sonna => "sonna",
            Self::Spruce => "spruce",
            Self::Tinta => "tinta",
            Self::Vale => "vale",
            Self::Verse => "verse",
            Self::Viola => "viola",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RealtimeVoicesList {
    pub v1: Vec<RealtimeVoice>,
    pub v2: Vec<RealtimeVoice>,
    #[serde(default)]
    #[ts(as = "Option<_>", optional)]
    pub v3: Vec<RealtimeVoice>,
    pub default_v1: RealtimeVoice,
    pub default_v2: RealtimeVoice,
}

impl RealtimeVoicesList {
    pub fn builtin() -> Self {
        Self {
            v1: vec![
                RealtimeVoice::Juniper,
                RealtimeVoice::Maple,
                RealtimeVoice::Spruce,
                RealtimeVoice::Ember,
                RealtimeVoice::Vale,
                RealtimeVoice::Breeze,
                RealtimeVoice::Arbor,
                RealtimeVoice::Sol,
                RealtimeVoice::Cove,
            ],
            v2: vec![
                RealtimeVoice::Alloy,
                RealtimeVoice::Ash,
                RealtimeVoice::Ballad,
                RealtimeVoice::Coral,
                RealtimeVoice::Echo,
                RealtimeVoice::Sage,
                RealtimeVoice::Shimmer,
                RealtimeVoice::Verse,
                RealtimeVoice::Marin,
                RealtimeVoice::Cedar,
            ],
            v3: vec![
                RealtimeVoice::Juniper,
                RealtimeVoice::Maple,
                RealtimeVoice::Spruce,
                RealtimeVoice::Ember,
                RealtimeVoice::Vale,
                RealtimeVoice::Breeze,
                RealtimeVoice::Arbor,
                RealtimeVoice::Sol,
                RealtimeVoice::Cove,
                RealtimeVoice::Aube,
                RealtimeVoice::Bubbie,
                RealtimeVoice::Haetsal,
                RealtimeVoice::Hanul,
                RealtimeVoice::Himari,
                RealtimeVoice::Leher,
                RealtimeVoice::Miko,
                RealtimeVoice::Neer,
                RealtimeVoice::Porto,
                RealtimeVoice::Rindo,
                RealtimeVoice::Rio,
                RealtimeVoice::Rivage,
                RealtimeVoice::Selva,
                RealtimeVoice::Sonna,
                RealtimeVoice::Tinta,
                RealtimeVoice::Viola,
            ],
            default_v1: RealtimeVoice::Cove,
            default_v2: RealtimeVoice::Marin,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeAudioFrame {
    pub data: String,
    pub sample_rate: u32,
    pub num_channels: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_per_channel: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeTranscriptDelta {
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeTranscriptDone {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeTranscriptEntry {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeHandoffRequested {
    pub handoff_id: String,
    pub item_id: String,
    pub input_transcript: String,
    pub active_transcript: Vec<RealtimeTranscriptEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeNoopRequested {
    pub call_id: String,
    pub item_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeInputAudioSpeechStarted {
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeResponseCancelled {
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeResponseCreated {
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeResponseDone {
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub enum RealtimeEvent {
    SessionUpdated {
        realtime_session_id: String,
        instructions: Option<String>,
    },
    InputAudioSpeechStarted(RealtimeInputAudioSpeechStarted),
    InputTranscriptDelta(RealtimeTranscriptDelta),
    InputTranscriptDone(RealtimeTranscriptDone),
    OutputTranscriptDelta(RealtimeTranscriptDelta),
    OutputTranscriptDone(RealtimeTranscriptDone),
    AudioOut(RealtimeAudioFrame),
    ResponseCreated(RealtimeResponseCreated),
    ResponseCancelled(RealtimeResponseCancelled),
    ResponseDone(RealtimeResponseDone),
    ConversationItemAdded(Value),
    ConversationItemDone {
        item_id: String,
    },
    /// Canonical display history produced by Core, separate from provider events.
    HistoryItemStarted(crate::realtime::RealtimeItem),
    HistoryTranscriptDelta {
        item_id: String,
        delta: String,
    },
    HistoryItemCompleted(crate::realtime::RealtimeItem),
    HandoffRequested(RealtimeHandoffRequested),
    NoopRequested(RealtimeNoopRequested),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationAudioParams {
    pub frame: RealtimeAudioFrame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTextParams {
    pub text: String,
    pub role: ConversationTextRole,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ConversationTextRole {
    #[default]
    User,
    Developer,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSpeechParams {
    pub text: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeConversationVersion {
    V1,
    #[default]
    V2,
    V3,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum CodexResponseHandoffMode {
    #[default]
    Thinking,
    Commentary,
    BemTags,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationStartedEvent {
    pub realtime_session_id: Option<String>,
    pub version: RealtimeConversationVersion,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationRealtimeEvent {
    pub payload: RealtimeEvent,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationClosedEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationSdpEvent {
    pub sdp: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeConversationListVoicesResponseEvent {
    pub voices: RealtimeVoicesList,
}
