use crate::endpoint::realtime_websocket::methods_common::conversation_function_call_output_message;
use crate::endpoint::realtime_websocket::methods_common::conversation_handoff_append_message;
use crate::endpoint::realtime_websocket::methods_common::conversation_item_create_message;
use crate::endpoint::realtime_websocket::methods_common::normalized_session_mode;
use crate::endpoint::realtime_websocket::methods_common::session_update_message;
use crate::endpoint::realtime_websocket::methods_common::standalone_handoff_message;
use crate::endpoint::realtime_websocket::methods_common::websocket_intent;
use crate::endpoint::realtime_websocket::methods_frameless_bidi::context_append_chunks;
use crate::endpoint::realtime_websocket::methods_frameless_bidi::delegation_context_append_message as frameless_delegation_context_append_message;
use crate::endpoint::realtime_websocket::methods_frameless_bidi::session_context_append_message as frameless_session_context_append_message;
use crate::endpoint::realtime_websocket::protocol::RealtimeAudioFrame;
use crate::endpoint::realtime_websocket::protocol::RealtimeContextAppendChannel;
use crate::endpoint::realtime_websocket::protocol::RealtimeEvent;
use crate::endpoint::realtime_websocket::protocol::RealtimeEventParser;
use crate::endpoint::realtime_websocket::protocol::RealtimeOutboundMessage;
use crate::endpoint::realtime_websocket::protocol::RealtimeOutputModality;
use crate::endpoint::realtime_websocket::protocol::RealtimeSessionConfig;
use crate::endpoint::realtime_websocket::protocol::RealtimeSessionMode;
use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptEntry;
use crate::endpoint::realtime_websocket::protocol::RealtimeVoice;
use crate::endpoint::realtime_websocket::protocol::parse_realtime_event;
use crate::error::ApiError;
use crate::provider::Provider;
use codex_client::backoff;
use codex_http_client::HttpClientFactory;
use codex_http_client::NetworkPolicyDenied;
use codex_http_client::maybe_build_rustls_client_config_with_custom_ca;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::RealtimeTranscriptDelta;
use codex_utils_rustls_provider::ensure_rustls_crypto_provider;
use codex_websocket_client::WebSocketConnection;
use codex_websocket_client::WebSocketConnector;
use codex_websocket_client::WebSocketTlsMode;
use futures::SinkExt;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::hash::BuildHasher;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tracing::warn;
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::WebSocketConfig;
use tungstenite::protocol::frame::coding::CloseCode;
use url::Url;

const REALTIME_WIRE_LOG_TARGET: &str = "codex_api::realtime_websocket::wire";
mod sideband;
const OPENAI_REALTIME_API_BASE_URL: &str = "https://api.openai.com/v1";

struct WsStream {
    tx_command: mpsc::Sender<WsCommand>,
    transport_task: tokio::task::JoinHandle<()>,
    sideband_input_stop: Option<tokio::sync::watch::Sender<bool>>,
    terminal_policy_error: tokio::sync::watch::Receiver<Option<NetworkPolicyDenied>>,
}

enum WsCommand {
    Send {
        message: Message,
        tx_result: oneshot::Sender<Result<(), WsError>>,
    },
    Close {
        frame: Option<CloseFrame>,
        tx_result: oneshot::Sender<Result<(), WsError>>,
    },
}

impl WsStream {
    fn new(
        inner: WebSocketConnection,
    ) -> (Self, async_channel::Receiver<Result<Message, WsError>>) {
        let (tx_command, mut rx_command) = mpsc::channel::<WsCommand>(32);
        let (tx_message, rx_message) = async_channel::unbounded::<Result<Message, WsError>>();

        let (terminal_policy_tx, terminal_policy_error) = tokio::sync::watch::channel(None);
        let transport_task = tokio::spawn(async move {
            let mut inner = inner;
            let report_write_policy_error = |error: &WsError| {
                if let Some(denied) = codex_websocket_client::network_policy_denial(error) {
                    terminal_policy_tx.send_replace(Some(denied));
                    let _ = tx_message.try_send(Err(Self::policy_error(denied)));
                }
            };
            loop {
                tokio::select! {
                    command = rx_command.recv() => {
                        let Some(command) = command else {
                            break;
                        };
                        match command {
                            WsCommand::Send { message, tx_result } => {
                                debug!("realtime websocket sending message");
                                let result = inner.send(message).await;
                                let should_break = result.is_err();
                                if let Err(err) = &result {
                                    error!("realtime websocket send failed: {err}");
                                    report_write_policy_error(err);
                                }
                                let _ = tx_result.send(result);
                                if should_break {
                                    break;
                                }
                            }
                            WsCommand::Close { frame, tx_result } => {
                                info!("realtime websocket sending close");
                                let result = inner.send(Message::Close(frame)).await;
                                if let Err(err) = &result {
                                    error!("realtime websocket close failed: {err}");
                                    report_write_policy_error(err);
                                }
                                let _ = tx_result.send(result);
                                break;
                            }
                        }
                    }
                    message = inner.next() => {
                        let Some(message) = message else {
                            break;
                        };
                        match message {
                            Ok(Message::Ping(payload)) => {
                                trace!(payload_len = payload.len(), "realtime websocket received ping");
                                if let Err(err) = inner.send(Message::Pong(payload)).await {
                                    error!("realtime websocket failed to send pong: {err}");
                                    terminal_policy_tx.send_replace(codex_websocket_client::network_policy_denial(&err));
                                    let _ = tx_message.send(Err(err)).await;
                                    break;
                                }
                            }
                            Ok(Message::Pong(_)) => {}
                            Ok(message @ (Message::Text(_)
                                | Message::Binary(_)
                                | Message::Close(_)
                                | Message::Frame(_))) => {
                                let is_close = matches!(message, Message::Close(_));
                                match &message {
                                    Message::Text(_) => trace!("realtime websocket received text frame"),
                                    Message::Binary(binary) => {
                                        error!(
                                            payload_len = binary.len(),
                                            "realtime websocket received unexpected binary frame"
                                        );
                                    }
                                    Message::Close(frame) => info!(
                                        "realtime websocket received close frame: code={:?} reason={:?}",
                                        frame.as_ref().map(|frame| frame.code),
                                        frame.as_ref().map(|frame| frame.reason.as_str())
                                    ),
                                    Message::Frame(_) => {
                                        trace!("realtime websocket received raw frame");
                                    }
                                    Message::Ping(_) | Message::Pong(_) => {}
                                }
                                if tx_message.send(Ok(message)).await.is_err() {
                                    break;
                                }
                                if is_close {
                                    break;
                                }
                            }
                            Err(err) => {
                                error!("realtime websocket receive failed: {err}");
                                    terminal_policy_tx.send_replace(codex_websocket_client::network_policy_denial(&err));
                                let _ = tx_message.send(Err(err)).await;
                                break;
                            }
                        }
                    }
                }
            }
            info!("realtime websocket pump exiting");
        });

        (
            Self {
                tx_command,
                transport_task,
                sideband_input_stop: None,
                terminal_policy_error,
            },
            rx_message,
        )
    }

    async fn request(
        &self,
        make_command: impl FnOnce(oneshot::Sender<Result<(), WsError>>) -> WsCommand,
    ) -> Result<(), WsError> {
        let (tx_result, rx_result) = oneshot::channel();
        if self.tx_command.send(make_command(tx_result)).await.is_err() {
            return Err(self.closed_error());
        }
        rx_result.await.unwrap_or_else(|_| Err(self.closed_error()))
    }

    fn closed_error(&self) -> WsError {
        match *self.terminal_policy_error.borrow() {
            Some(denied) => Self::policy_error(denied),
            None => WsError::ConnectionClosed,
        }
    }

    fn policy_error(denied: NetworkPolicyDenied) -> WsError {
        WsError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            denied,
        ))
    }

    async fn send(&self, message: Message) -> Result<(), WsError> {
        self.request(|tx_result| WsCommand::Send { message, tx_result })
            .await
    }

    /// Ask the socket pump to send a close frame to the connected peer and
    /// return the write result.
    async fn close(&self, frame: Option<CloseFrame>) -> Result<(), WsError> {
        self.request(|tx_result| WsCommand::Close { frame, tx_result })
            .await
    }
}

impl Drop for WsStream {
    fn drop(&mut self) {
        self.transport_task.abort();
    }
}

pub struct RealtimeWebsocketConnection {
    writer: RealtimeWebsocketWriter,
    events: RealtimeWebsocketEvents,
}

#[derive(Clone)]
pub struct RealtimeWebsocketWriter {
    stream: Arc<WsStream>,
    is_closed: Arc<AtomicBool>,
    event_parser: RealtimeEventParser,
    context_append_channel: Option<RealtimeContextAppendChannel>,
}

#[derive(Clone)]
pub struct RealtimeWebsocketEvents {
    rx_message: async_channel::Receiver<Result<Message, WsError>>,
    pending_events: Arc<Mutex<VecDeque<RealtimeEvent>>>,
    transcript_state: RealtimeTranscriptState,
    event_parser: RealtimeEventParser,
    is_closed: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub struct RealtimeTranscriptState {
    active: Arc<Mutex<ActiveTranscriptState>>,
}

#[derive(Default)]
struct ActiveTranscriptState {
    entries: Vec<RealtimeTranscriptEntry>,
    // Handoff input can echo an utterance already written to child history.
    flushed_user_inputs: HashSet<u64>,
    new_input_entry: bool,
    new_output_entry: bool,
}

impl ActiveTranscriptState {
    fn take_entries(&mut self) -> Vec<RealtimeTranscriptEntry> {
        self.flushed_user_inputs.clear();
        std::mem::take(&mut self.entries)
    }
}

impl RealtimeWebsocketConnection {
    pub async fn send_audio_frame(&self, frame: RealtimeAudioFrame) -> Result<(), ApiError> {
        self.writer.send_audio_frame(frame).await
    }

    pub async fn send_conversation_item_create(
        &self,
        text: String,
        role: ConversationTextRole,
    ) -> Result<(), ApiError> {
        self.writer.send_conversation_item_create(text, role).await
    }

    pub async fn send_conversation_function_call_output(
        &self,
        call_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.writer
            .send_conversation_function_call_output(call_id, output_text)
            .await
    }

    pub async fn close(&self) -> Result<(), ApiError> {
        self.writer.close().await
    }

    pub async fn next_event(&self) -> Result<Option<RealtimeEvent>, ApiError> {
        self.events.next_event().await
    }

    pub fn writer(&self) -> RealtimeWebsocketWriter {
        self.writer.clone()
    }

    pub fn events(&self) -> RealtimeWebsocketEvents {
        self.events.clone()
    }

    fn new(
        stream: WsStream,
        rx_message: async_channel::Receiver<Result<Message, WsError>>,
        event_parser: RealtimeEventParser,
        transcript_state: RealtimeTranscriptState,
    ) -> Self {
        let stream = Arc::new(stream);
        let is_closed = Arc::new(AtomicBool::new(false));
        Self {
            writer: RealtimeWebsocketWriter {
                stream: Arc::clone(&stream),
                is_closed: Arc::clone(&is_closed),
                event_parser,
                context_append_channel: None,
            },
            events: RealtimeWebsocketEvents {
                rx_message,
                pending_events: Arc::new(Mutex::new(VecDeque::new())),
                transcript_state,
                event_parser,
                is_closed,
            },
        }
    }
}

impl RealtimeWebsocketWriter {
    pub fn with_context_append_channel(mut self, channel: RealtimeContextAppendChannel) -> Self {
        self.context_append_channel = Some(channel);
        self
    }

    pub async fn send_audio_frame(&self, frame: RealtimeAudioFrame) -> Result<(), ApiError> {
        let message = match self.event_parser {
            RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
                RealtimeOutboundMessage::InputAudioBufferAppend { audio: frame.data }
            }
            RealtimeEventParser::FramelessBidi => {
                RealtimeOutboundMessage::InputAudioAppend { audio: frame.data }
            }
        };
        self.send_json(&message).await
    }

    pub async fn send_conversation_item_create(
        &self,
        text: String,
        role: ConversationTextRole,
    ) -> Result<(), ApiError> {
        self.send_json(&conversation_item_create_message(
            self.event_parser,
            text,
            role,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_conversation_handoff_append(
        &self,
        handoff_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(&conversation_handoff_append_message(
            self.event_parser,
            handoff_id,
            output_text,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_standalone_handoff(
        &self,
        handoff_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(&standalone_handoff_message(
            self.event_parser,
            handoff_id,
            output_text,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_conversation_function_call_output(
        &self,
        call_id: String,
        output_text: String,
    ) -> Result<(), ApiError> {
        self.send_json(&conversation_function_call_output_message(
            self.event_parser,
            call_id,
            output_text,
            self.context_append_channel,
        ))
        .await
    }

    pub async fn send_response_create(&self) -> Result<(), ApiError> {
        self.send_json(&RealtimeOutboundMessage::ResponseCreate)
            .await
    }

    pub async fn send_session_update(
        &self,
        instructions: String,
        initial_items: Vec<ConversationTextParams>,
        session_mode: RealtimeSessionMode,
        output_modality: RealtimeOutputModality,
        voice: RealtimeVoice,
        delegation_ack_filler: Option<bool>,
    ) -> Result<(), ApiError> {
        let session_mode = normalized_session_mode(self.event_parser, session_mode);
        let message = session_update_message(
            self.event_parser,
            instructions,
            initial_items,
            session_mode,
            output_modality,
            voice,
            delegation_ack_filler,
        );
        self.send_json(&message).await
    }

    pub async fn close(&self) -> Result<(), ApiError> {
        if self.is_closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        if self.event_parser == RealtimeEventParser::FramelessBidi {
            let payload =
                serde_json::to_string(&RealtimeOutboundMessage::SessionClose).map_err(|err| {
                    ApiError::Stream(format!("failed to encode realtime request: {err}"))
                })?;
            trace!(target: REALTIME_WIRE_LOG_TARGET, "realtime websocket request: {payload}");
            if let Err(err) = self.stream.send(Message::Text(payload.into())).await
                && !matches!(err, WsError::ConnectionClosed | WsError::AlreadyClosed)
            {
                return Err(ApiError::Stream(format!(
                    "failed to close frameless realtime session: {err}"
                )));
            }
        }
        if let Err(err) = self.stream.close(/*frame*/ None).await
            && !matches!(err, WsError::ConnectionClosed | WsError::AlreadyClosed)
        {
            return Err(ApiError::Stream(format!(
                "failed to close websocket: {err}"
            )));
        }
        Ok(())
    }

    /// Detach an observer without ending the frontend-owned realtime session.
    pub async fn close_transport(&self) -> Result<(), ApiError> {
        if self.is_closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.stream
            .close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            }))
            .await
            .map_err(|err| ApiError::Stream(format!("failed to close websocket transport: {err}")))
    }

    async fn send_json(&self, message: &RealtimeOutboundMessage) -> Result<(), ApiError> {
        match message {
            RealtimeOutboundMessage::DelegationContextAppend {
                delegation_item_id,
                channel,
                content,
            } => {
                if let Some(content) = content.first() {
                    for chunk in context_append_chunks(&content.text) {
                        self.send_json_frame(&frameless_delegation_context_append_message(
                            delegation_item_id.clone(),
                            chunk,
                            *channel,
                        ))
                        .await?;
                    }
                    return Ok(());
                }
            }
            RealtimeOutboundMessage::SessionContextAppend { channel, content } => {
                if let Some(content) = content.first() {
                    for chunk in context_append_chunks(&content.text) {
                        self.send_json_frame(&frameless_session_context_append_message(
                            chunk, *channel,
                        ))
                        .await?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        }
        self.send_json_frame(message).await
    }

    async fn send_json_frame(&self, message: &RealtimeOutboundMessage) -> Result<(), ApiError> {
        let payload = serde_json::to_string(message)
            .map_err(|err| ApiError::Stream(format!("failed to encode realtime request: {err}")))?;
        debug!(?message, "realtime websocket request");
        self.send_payload(payload).await
    }

    pub async fn send_payload(&self, payload: String) -> Result<(), ApiError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Err(map_realtime_websocket_connect_error(
                self.stream.closed_error(),
            ));
        }

        trace!(target: REALTIME_WIRE_LOG_TARGET, "realtime websocket request: {payload}");
        self.stream
            .send(Message::Text(payload.into()))
            .await
            .map_err(map_realtime_websocket_connect_error)?;
        Ok(())
    }
}

impl RealtimeWebsocketEvents {
    pub fn transcript_state(&self) -> RealtimeTranscriptState {
        self.transcript_state.clone()
    }

    pub async fn take_transcript_tail(&self) -> Vec<RealtimeTranscriptEntry> {
        self.transcript_state.take_transcript_tail().await
    }

    pub async fn next_event(&self) -> Result<Option<RealtimeEvent>, ApiError> {
        if self.is_closed.load(Ordering::SeqCst) {
            return Ok(None);
        }

        if let Some(event) = self.pending_events.lock().await.pop_front() {
            return Ok(Some(event));
        }

        loop {
            let msg = match self.rx_message.recv().await {
                Ok(Ok(msg)) => msg,
                Ok(Err(err)) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    error!("realtime websocket read failed: {err}");
                    return Err(map_realtime_websocket_connect_error(err));
                }
                Err(_) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    if self.event_parser == RealtimeEventParser::FramelessBidi {
                        error!("realtime websocket event stream ended unexpectedly");
                        return Err(ApiError::Stream(
                            "realtime websocket event stream ended unexpectedly".to_string(),
                        ));
                    }
                    info!("realtime websocket event stream ended");
                    return Ok(None);
                }
            };

            match msg {
                Message::Text(text) => {
                    trace!(target: REALTIME_WIRE_LOG_TARGET, "realtime websocket event: {text}");
                    if let Some(mut event) = parse_realtime_event(&text, self.event_parser) {
                        self.update_active_transcript(&mut event).await;
                        debug!(?event, "realtime websocket parsed event");
                        return Ok(Some(event));
                    }
                    debug!("realtime websocket ignored unsupported text frame");
                }
                Message::Close(frame) => {
                    self.is_closed.store(true, Ordering::SeqCst);
                    info!(
                        "realtime websocket closed: code={:?} reason={:?}",
                        frame.as_ref().map(|frame| frame.code),
                        frame.as_ref().map(|frame| frame.reason.as_str())
                    );
                    if self.event_parser != RealtimeEventParser::FramelessBidi {
                        return Ok(None);
                    }
                    return match frame {
                        Some(frame) if frame.code == CloseCode::Normal => Ok(None),
                        Some(frame) => Err(ApiError::Stream(format!(
                            "realtime websocket closed unexpectedly: code={:?} reason={:?}",
                            frame.code, frame.reason
                        ))),
                        None => Err(ApiError::Stream(
                            "realtime websocket closed unexpectedly without a status code"
                                .to_string(),
                        )),
                    };
                }
                Message::Binary(_) => {
                    return Ok(Some(RealtimeEvent::Error(
                        "unexpected binary realtime websocket event".to_string(),
                    )));
                }
                Message::Frame(_) | Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    }

    async fn wait_for_session_started(&self) -> Result<(), ApiError> {
        let Some(event) = self.next_event().await? else {
            return Err(ApiError::Stream(
                "frameless realtime session ended before session.started".to_string(),
            ));
        };
        match &event {
            RealtimeEvent::SessionUpdated { .. } => {
                self.pending_events.lock().await.push_back(event);
                Ok(())
            }
            RealtimeEvent::Error(message) => Err(ApiError::Stream(message.clone())),
            _ => Err(ApiError::Stream(
                "frameless realtime session received an event before session.started".to_string(),
            )),
        }
    }

    async fn update_active_transcript(&self, event: &mut RealtimeEvent) {
        let mut active_transcript = self.transcript_state.active.lock().await;
        match event {
            RealtimeEvent::InputAudioSpeechStarted(_) => {
                active_transcript.new_input_entry = true;
            }
            RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta { delta, .. }) => {
                let force_new = active_transcript.new_input_entry;
                append_transcript_delta(
                    &mut active_transcript.entries,
                    "user",
                    delta,
                    force_new,
                    self.event_parser,
                );
                if !delta.is_empty() || self.event_parser != RealtimeEventParser::FramelessBidi {
                    active_transcript.new_input_entry = false;
                }
            }
            RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta { delta, .. }) => {
                let force_new = active_transcript.new_output_entry;
                append_transcript_delta(
                    &mut active_transcript.entries,
                    "assistant",
                    delta,
                    force_new,
                    self.event_parser,
                );
                if !delta.is_empty() || self.event_parser != RealtimeEventParser::FramelessBidi {
                    active_transcript.new_output_entry = false;
                }
            }
            RealtimeEvent::InputTranscriptDone(done) => {
                let force_new = active_transcript.new_input_entry;
                apply_transcript_done(
                    &mut active_transcript.entries,
                    "user",
                    &done.text,
                    force_new,
                    self.event_parser,
                );
                active_transcript.new_input_entry =
                    self.event_parser == RealtimeEventParser::FramelessBidi;
            }
            RealtimeEvent::OutputTranscriptDone(done) => {
                let force_new = active_transcript.new_output_entry;
                apply_transcript_done(
                    &mut active_transcript.entries,
                    "assistant",
                    &done.text,
                    force_new,
                    self.event_parser,
                );
                active_transcript.new_output_entry =
                    self.event_parser == RealtimeEventParser::FramelessBidi;
            }
            RealtimeEvent::HandoffRequested(handoff) => {
                append_handoff_input(&mut active_transcript, &handoff.input_transcript);
                handoff.active_transcript = active_transcript.take_entries();
                active_transcript.new_input_entry = true;
                active_transcript.new_output_entry = true;
            }
            RealtimeEvent::ResponseCreated(_) => {
                active_transcript.new_output_entry = true;
            }
            RealtimeEvent::SessionUpdated { .. }
            | RealtimeEvent::AudioOut(_)
            | RealtimeEvent::ResponseCancelled(_)
            | RealtimeEvent::ResponseDone(_)
            | RealtimeEvent::ConversationItemDone { .. }
            | RealtimeEvent::NoopRequested(_)
            | RealtimeEvent::ConversationItemAdded(_)
            | RealtimeEvent::Error(_)
            | RealtimeEvent::HistoryItemStarted(_)
            | RealtimeEvent::HistoryTranscriptDelta { .. }
            | RealtimeEvent::HistoryItemCompleted(_) => {}
        }
    }

    pub async fn take_completed_transcript(
        &self,
        event: &RealtimeEvent,
        threshold: usize,
    ) -> Vec<RealtimeTranscriptEntry> {
        if self.event_parser != RealtimeEventParser::FramelessBidi
            || !matches!(
                event,
                RealtimeEvent::InputTranscriptDone(_) | RealtimeEvent::OutputTranscriptDone(_)
            )
        {
            return Vec::new();
        }
        let mut active = self.transcript_state.active.lock().await;
        // Only the other speaker can still be open after turn.done. Keep their utterance
        // and everything after it in place so history stays in conversation order.
        let open_role = match event {
            RealtimeEvent::InputTranscriptDone(_) if !active.new_output_entry => Some("assistant"),
            RealtimeEvent::OutputTranscriptDone(_) if !active.new_input_entry => Some("user"),
            _ => None,
        };
        let entries = &active.entries;
        let end = open_role
            .and_then(|role| entries.iter().rposition(|entry| entry.role == role))
            .unwrap_or(entries.len());
        if end == 0 || transcript_entries_bytes(entries) <= threshold {
            return Vec::new();
        }
        let completed: Vec<_> = active.entries.drain(..end).collect();
        let recorded = &mut active.flushed_user_inputs;
        let hash = recorded.hasher().clone();
        recorded.extend(
            completed
                .iter()
                .filter(|entry| entry.role == "user")
                .map(|entry| hash.hash_one(entry.text.trim())),
        );
        completed
    }
}

impl RealtimeTranscriptState {
    pub async fn take_transcript_tail(&self) -> Vec<RealtimeTranscriptEntry> {
        self.active.lock().await.take_entries()
    }
}

fn transcript_entries_bytes(entries: &[RealtimeTranscriptEntry]) -> usize {
    entries
        .iter()
        .map(|entry| entry.role.len() + entry.text.len() + 3)
        .sum()
}

fn append_transcript_delta(
    entries: &mut Vec<RealtimeTranscriptEntry>,
    role: &str,
    delta: &str,
    force_new: bool,
    event_parser: RealtimeEventParser,
) {
    if delta.is_empty() {
        return;
    }

    // V3 interleaves the two speakers' transcripts. Legacy protocols retain
    // adjacent speaker runs and their existing response lifecycle semantics.
    let entry = match event_parser {
        RealtimeEventParser::FramelessBidi => {
            entries.iter_mut().rev().find(|entry| entry.role == role)
        }
        RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
            entries.last_mut().filter(|entry| entry.role == role)
        }
    };
    if !force_new && let Some(last_entry) = entry {
        last_entry.text.push_str(delta);
        return;
    }

    entries.push(RealtimeTranscriptEntry {
        role: role.to_string(),
        text: delta.to_string(),
    });
}

fn apply_transcript_done(
    entries: &mut Vec<RealtimeTranscriptEntry>,
    role: &str,
    text: &str,
    force_new: bool,
    event_parser: RealtimeEventParser,
) {
    if text.is_empty() {
        return;
    }

    // V3 interleaves the two speakers' transcripts. Legacy protocols retain
    // adjacent speaker runs and their existing response lifecycle semantics.
    let entry = match event_parser {
        RealtimeEventParser::FramelessBidi => {
            entries.iter_mut().rev().find(|entry| entry.role == role)
        }
        RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
            entries.last_mut().filter(|entry| entry.role == role)
        }
    };
    if !force_new && let Some(last_entry) = entry {
        // A delayed V3 final can arrive after deltas from the next utterance.
        // Preserve accumulated speech unless the final extends it.
        if event_parser != RealtimeEventParser::FramelessBidi || text.starts_with(&last_entry.text)
        {
            last_entry.text = text.to_string();
        }
        return;
    }

    entries.push(RealtimeTranscriptEntry {
        role: role.to_string(),
        text: text.to_string(),
    });
}

fn append_handoff_input(active: &mut ActiveTranscriptState, input: &str) {
    let input = input.trim();
    let recorded = &active.flushed_user_inputs;
    if input.is_empty()
        || contains_transcript_entry(&active.entries, "user", input)
        || (!recorded.is_empty() && recorded.contains(&recorded.hasher().hash_one(input)))
    {
        return;
    }

    active.entries.push(RealtimeTranscriptEntry {
        role: "user".to_string(),
        text: input.to_string(),
    });
}

fn contains_transcript_entry(entries: &[RealtimeTranscriptEntry], role: &str, text: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.role == role && entry.text.trim() == text.trim())
}

pub struct RealtimeWebsocketClient {
    provider: Provider,
    http_client_factory: HttpClientFactory,
    auth: Option<crate::SharedAuthProvider>,
    webrtc_sideband_base_url: String,
}

#[derive(Clone, Copy)]
enum RealtimeSessionInitialization {
    NewSession,
    LegacyWebrtcSideband,
    ExistingCall,
}

impl RealtimeWebsocketClient {
    pub fn new(provider: Provider, http_client_factory: HttpClientFactory) -> Self {
        Self {
            provider,
            http_client_factory,
            auth: None,
            webrtc_sideband_base_url: OPENAI_REALTIME_API_BASE_URL.to_string(),
        }
    }

    /// Use the transport captured with the selected authentication source.
    pub fn with_http_client_factory(mut self, factory: HttpClientFactory) -> Self {
        self.http_client_factory = factory;
        self
    }

    /// Resolve request auth on every handshake, including sideband retries and reconnects.
    pub fn with_auth(mut self, auth: crate::SharedAuthProvider) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Overrides the direct WebRTC sideband URL for local development and tests.
    pub fn with_webrtc_sideband_base_url(mut self, base_url: String) -> Self {
        self.webrtc_sideband_base_url = base_url;
        self
    }

    pub async fn connect(
        &self,
        config: RealtimeSessionConfig,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        let ws_url = websocket_url_from_api_url(
            self.provider.base_url.as_str(),
            self.provider.query_params.as_ref(),
            config.model.as_deref(),
            config.event_parser,
            config.session_mode,
        )?;
        self.connect_realtime_websocket_url(
            ws_url,
            config,
            extra_headers,
            default_headers,
            RealtimeSessionInitialization::NewSession,
            RealtimeTranscriptState::default(),
        )
        .await
    }

    pub async fn connect_webrtc_sideband(
        &self,
        config: RealtimeSessionConfig,
        call_id: &str,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
        transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        self.connect_sideband(
            config,
            call_id,
            extra_headers,
            default_headers,
            RealtimeSessionInitialization::LegacyWebrtcSideband,
            transcript_state,
        )
        .await
    }

    /// Attaches to a client-created call without overwriting its session configuration.
    pub async fn connect_existing_call_sideband(
        &self,
        config: RealtimeSessionConfig,
        call_id: &str,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
        transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        // Instant Connect returns a call ID before ICE activates the call. Its sideband
        // can briefly return 404 while the client connects media in parallel.
        let mut activation_delays = [100, 300, 800].into_iter();
        loop {
            let result = self
                .connect_sideband(
                    config.clone(),
                    call_id,
                    extra_headers.clone(),
                    default_headers.clone(),
                    RealtimeSessionInitialization::ExistingCall,
                    transcript_state.clone(),
                )
                .await;
            if matches!(&result, Err(ApiError::Api { status, .. }) if *status == StatusCode::NOT_FOUND)
                && let Some(delay_ms) = activation_delays.next()
            {
                sleep(Duration::from_millis(delay_ms)).await;
                continue;
            }
            return result;
        }
    }

    async fn connect_sideband(
        &self,
        config: RealtimeSessionConfig,
        call_id: &str,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
        session_initialization: RealtimeSessionInitialization,
        transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        // The WebRTC call already exists; this loop only retries joining its sideband control
        // socket. Once joined, the returned connection is the same reader/writer state that the
        // ordinary websocket start path uses.
        for attempt in 0..=self.provider.retry.max_attempts {
            let result = self
                .connect_webrtc_sideband_once(
                    config.clone(),
                    call_id,
                    extra_headers.clone(),
                    default_headers.clone(),
                    session_initialization,
                    transcript_state.clone(),
                )
                .await;
            match result {
                Ok(connection) => return Ok(connection),
                Err(err @ ApiError::Transport(codex_http_client::TransportError::Policy(_))) => {
                    return Err(err);
                }
                Err(err) if webrtc_sideband_session_ended(&err) => return Err(err),
                Err(err) if attempt < self.provider.retry.max_attempts => {
                    let delay = backoff(self.provider.retry.base_delay, attempt + 1);
                    warn!(
                        attempt = attempt + 1,
                        call_id,
                        delay_ms = delay.as_millis(),
                        "realtime sideband websocket connect failed; retrying: {err}"
                    );
                    sleep(delay).await;
                }
                Err(err) => return Err(err),
            }
        }

        Err(ApiError::Stream(
            "realtime sideband websocket retry loop exhausted".to_string(),
        ))
    }

    async fn connect_webrtc_sideband_once(
        &self,
        config: RealtimeSessionConfig,
        call_id: &str,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
        session_initialization: RealtimeSessionInitialization,
        transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        // Keep the parser/session query shaping from standalone realtime while replacing the model
        // query with a call_id join onto an existing WebRTC session.
        let ws_url = self.webrtc_sideband_url(config.event_parser, config.session_mode, call_id)?;
        self.connect_realtime_websocket_url(
            ws_url,
            config,
            extra_headers,
            default_headers,
            session_initialization,
            transcript_state,
        )
        .await
    }

    fn webrtc_sideband_url(
        &self,
        event_parser: RealtimeEventParser,
        session_mode: RealtimeSessionMode,
        call_id: &str,
    ) -> Result<Url, ApiError> {
        websocket_url_from_api_url_for_call(
            self.webrtc_sideband_base_url.as_str(),
            /*query_params*/ None,
            event_parser,
            session_mode,
            call_id,
        )
    }

    async fn connect_realtime_websocket_url(
        &self,
        ws_url: Url,
        config: RealtimeSessionConfig,
        extra_headers: HeaderMap,
        default_headers: HeaderMap,
        session_initialization: RealtimeSessionInitialization,
        transcript_state: RealtimeTranscriptState,
    ) -> Result<RealtimeWebsocketConnection, ApiError> {
        ensure_rustls_crypto_provider();

        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|err| ApiError::Stream(format!("failed to build websocket request: {err}")))?;
        let headers = merge_request_headers(
            &self.provider.headers,
            with_session_id_header(extra_headers, config.session_id.as_deref())?,
            default_headers,
        );
        request.headers_mut().extend(headers);
        if let Some(auth) = &self.auth {
            let headers = std::mem::take(request.headers_mut());
            *request.headers_mut() = auth.apply_auth_headers(headers).await?;
        }

        info!("connecting realtime websocket: {ws_url}");
        // Realtime websocket TLS should honor the same custom-CA env vars as the rest of Codex's
        // outbound HTTPS and websocket traffic.
        let tls_config = maybe_build_rustls_client_config_with_custom_ca()
            .map_err(|err| ApiError::Stream(format!("failed to configure websocket TLS: {err}")))?;
        // A fresh Windows install may not have downloaded the server's trusted root yet.
        // Use platform validation only for system trust, preserving custom CA semantics.
        #[cfg(windows)]
        let tls_config = match tls_config {
            Some(tls_config) => Some(tls_config),
            None => Some(
                codex_http_client::build_windows_platform_tls_config().map_err(|err| {
                    ApiError::Stream(format!("failed to configure websocket TLS: {err}"))
                })?,
            ),
        };
        let tls_mode = match tls_config {
            Some(tls_config) => WebSocketTlsMode::Rustls(tls_config),
            None => WebSocketTlsMode::TungsteniteDefault,
        };
        let connector = WebSocketConnector::new_with_tls_mode(&self.http_client_factory, tls_mode)
            .map_err(|err| ApiError::Stream(format!("failed to configure websocket TLS: {err}")))?;
        let (stream, response) = connector
            .connect(request, websocket_config())
            .await
            .map_err(map_realtime_websocket_connect_error)?;
        info!(
            ws_url = %ws_url,
            status = %response.status(),
            "realtime websocket connected"
        );

        let (stream, rx_message) = WsStream::new(stream);
        let connection = RealtimeWebsocketConnection::new(
            stream,
            rx_message,
            config.event_parser,
            transcript_state,
        );
        let initialize_session = match session_initialization {
            RealtimeSessionInitialization::NewSession => true,
            RealtimeSessionInitialization::LegacyWebrtcSideband => {
                config.event_parser != RealtimeEventParser::FramelessBidi
            }
            RealtimeSessionInitialization::ExistingCall => false,
        };
        if initialize_session {
            debug!(
                session_id = config.session_id.as_deref().unwrap_or("<none>"),
                "realtime websocket sending session.update"
            );
            connection
                .writer
                .send_session_update(
                    config.instructions,
                    config.initial_items,
                    config.session_mode,
                    config.output_modality,
                    config.voice,
                    config.delegation_ack_filler,
                )
                .await?;
        }
        if matches!(
            session_initialization,
            RealtimeSessionInitialization::NewSession
        ) && config.event_parser == RealtimeEventParser::FramelessBidi
        {
            connection.events.wait_for_session_started().await?;
        }
        Ok(connection)
    }
}

fn webrtc_sideband_session_ended(err: &ApiError) -> bool {
    matches!(
        err,
        ApiError::Api { status, .. }
            if matches!(*status, StatusCode::NOT_FOUND | StatusCode::GONE)
    )
}

fn map_realtime_websocket_connect_error(err: WsError) -> ApiError {
    if let Some(denied) = codex_websocket_client::network_policy_denial(&err) {
        return ApiError::Transport(codex_http_client::TransportError::Policy(denied));
    }
    match err {
        WsError::Http(response) => ApiError::Api {
            status: response.status(),
            message: "realtime websocket handshake failed".to_string(),
        },
        err => ApiError::Stream(format!("failed to connect realtime websocket: {err}")),
    }
}

fn merge_request_headers(
    provider_headers: &HeaderMap,
    extra_headers: HeaderMap,
    default_headers: HeaderMap,
) -> HeaderMap {
    let mut headers = provider_headers.clone();
    headers.extend(extra_headers);
    for (name, value) in &default_headers {
        if let http::header::Entry::Vacant(entry) = headers.entry(name) {
            entry.insert(value.clone());
        }
    }
    headers
}

fn with_session_id_header(
    mut headers: HeaderMap,
    session_id: Option<&str>,
) -> Result<HeaderMap, ApiError> {
    let Some(session_id) = session_id else {
        return Ok(headers);
    };
    headers.insert(
        "x-session-id",
        HeaderValue::from_str(session_id).map_err(|err| {
            ApiError::Stream(format!("invalid realtime session id header: {err}"))
        })?,
    );
    Ok(headers)
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
}

fn websocket_url_from_api_url(
    api_url: &str,
    query_params: Option<&HashMap<String, String>>,
    model: Option<&str>,
    event_parser: RealtimeEventParser,
    _session_mode: RealtimeSessionMode,
) -> Result<Url, ApiError> {
    let mut url = Url::parse(api_url)
        .map_err(|err| ApiError::Stream(format!("failed to parse realtime api_url: {err}")))?;

    normalize_realtime_path(&mut url, event_parser);

    match url.scheme() {
        "ws" | "wss" => {}
        "http" | "https" => {
            let scheme = if url.scheme() == "http" { "ws" } else { "wss" };
            let _ = url.set_scheme(scheme);
        }
        scheme => {
            return Err(ApiError::Stream(format!(
                "unsupported realtime api_url scheme: {scheme}"
            )));
        }
    }

    let intent = websocket_intent(event_parser);
    let has_extra_query_params = query_params.is_some_and(|query_params| {
        query_params
            .iter()
            .any(|(key, _)| key != "intent" && !(key == "model" && model.is_some()))
    });
    if intent.is_some() || model.is_some() || has_extra_query_params {
        let mut query = url.query_pairs_mut();
        if let Some(intent) = intent {
            query.append_pair("intent", intent);
        }
        if let Some(model) = model {
            query.append_pair("model", model);
        }
        if let Some(query_params) = query_params {
            for (key, value) in query_params {
                if key == "intent" || (key == "model" && model.is_some()) {
                    continue;
                }
                query.append_pair(key, value);
            }
        }
    }

    Ok(url)
}

fn websocket_url_from_api_url_for_call(
    api_url: &str,
    query_params: Option<&HashMap<String, String>>,
    event_parser: RealtimeEventParser,
    session_mode: RealtimeSessionMode,
    call_id: &str,
) -> Result<Url, ApiError> {
    let mut url = websocket_url_from_api_url(
        api_url,
        query_params,
        /*model*/ None,
        event_parser,
        session_mode,
    )?;
    match event_parser {
        RealtimeEventParser::FramelessBidi => {
            if matches!(call_id, "." | "..") {
                return Err(ApiError::InvalidRequest {
                    message: format!("invalid realtime call id: {call_id}"),
                });
            }
            url.path_segments_mut()
                .map_err(|()| {
                    ApiError::Stream(
                        "realtime sideband URL cannot contain path segments".to_string(),
                    )
                })?
                .pop_if_empty()
                .push(call_id);
        }
        RealtimeEventParser::V1 | RealtimeEventParser::RealtimeV2 => {
            url.query_pairs_mut().append_pair("call_id", call_id);
        }
    }
    Ok(url)
}

fn normalize_realtime_path(url: &mut Url, event_parser: RealtimeEventParser) {
    if event_parser == RealtimeEventParser::FramelessBidi {
        let path = url.path().to_string();
        if path.is_empty() || path == "/" || path == "/v1" || path == "/v1/" {
            url.set_path("/v1/live");
        } else if let Some(prefix) = path.trim_end_matches('/').strip_suffix("/realtime") {
            url.set_path(&format!("{prefix}/live"));
        } else if path.ends_with("/live/") {
            url.set_path(path.trim_end_matches('/'));
        }
        return;
    }

    let path = url.path().to_string();
    if path.is_empty() || path == "/" {
        url.set_path("/v1/realtime");
        return;
    }

    if path.ends_with("/realtime") {
        return;
    }

    if path.ends_with("/realtime/") {
        url.set_path(path.trim_end_matches('/'));
        return;
    }

    if path.ends_with("/v1") {
        url.set_path(&format!("{path}/realtime"));
        return;
    }

    if path.ends_with("/v1/") {
        url.set_path(&format!("{path}realtime"));
    }
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod transcript_tests;

#[cfg(test)]
#[path = "methods_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "methods_auth_tests.rs"]
mod auth_tests;
