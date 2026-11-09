use super::*;
use crate::endpoint::realtime_websocket::protocol::RealtimeTranscriptEntry;
use crate::provider::RetryConfig;
use codex_http_client::OutboundProxyPolicy::ReqwestDefault;
use codex_protocol::protocol::RealtimeHandoffRequested;
use codex_protocol::protocol::RealtimeInputAudioSpeechStarted;
use codex_protocol::protocol::RealtimeNoopRequested;
use codex_protocol::protocol::RealtimeResponseCancelled;
use codex_protocol::protocol::RealtimeResponseCreated;
use codex_protocol::protocol::RealtimeResponseDone;
use codex_protocol::protocol::RealtimeTranscriptDelta;
use codex_protocol::protocol::RealtimeTranscriptDone;
use codex_protocol::protocol::RealtimeVoice;
use http::HeaderValue;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tungstenite::protocol::CloseFrame;

fn test_client(provider: Provider) -> RealtimeWebsocketClient {
    RealtimeWebsocketClient::new(provider, HttpClientFactory::new(ReqwestDefault))
}

#[test]
fn parse_session_updated_event() {
    let payload = json!({
        "type": "session.updated",
        "session": {"id": "sess_123", "instructions": "backend prompt"}
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::SessionUpdated {
            realtime_session_id: "sess_123".to_string(),
            instructions: Some("backend prompt".to_string()),
        })
    );
}

#[test]
fn parse_audio_delta_event() {
    let payload = json!({
        "type": "conversation.output_audio.delta",
        "delta": "AAA=",
        "sample_rate": 48000,
        "channels": 1,
        "samples_per_channel": 960
    })
    .to_string();
    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
            data: "AAA=".to_string(),
            sample_rate: 48000,
            num_channels: 1,
            samples_per_channel: Some(960),
            item_id: None,
        }))
    );
}

#[test]
fn parse_conversation_item_added_event() {
    let payload = json!({
        "type": "conversation.item.added",
        "item": {"type": "message", "seq": 7}
    })
    .to_string();
    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::ConversationItemAdded(
            json!({"type": "message", "seq": 7})
        ))
    );
}

#[test]
fn parse_conversation_item_done_event() {
    let payload = json!({
        "type": "conversation.item.done",
        "item": {"id": "item_123", "type": "message"}
    })
    .to_string();
    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::ConversationItemDone {
            item_id: "item_123".to_string(),
        })
    );
}

#[test]
fn parse_handoff_requested_event() {
    let payload = json!({
        "type": "conversation.handoff.requested",
        "handoff_id": "handoff_123",
        "item_id": "item_123",
        "input_transcript": "delegate this"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
            handoff_id: "handoff_123".to_string(),
            item_id: "item_123".to_string(),
            input_transcript: "delegate this".to_string(),
            active_transcript: Vec::new(),
        }))
    );
}

#[tokio::test]
async fn takes_only_transcript_after_last_handoff_once() {
    let (_tx_message, rx_message) = async_channel::unbounded();
    let events = RealtimeWebsocketEvents {
        rx_message,
        pending_events: Arc::new(Mutex::new(VecDeque::new())),
        transcript_state: RealtimeTranscriptState::default(),
        event_parser: RealtimeEventParser::V1,
        is_closed: Arc::new(AtomicBool::new(false)),
    };

    assert_eq!(events.take_transcript_tail().await, vec![]);

    let mut covered = RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
        delta: "already handed off".to_string(),
    });
    events.update_active_transcript(&mut covered).await;
    let mut handoff = RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
        handoff_id: "handoff_1".to_string(),
        item_id: "item_1".to_string(),
        input_transcript: "already handed off".to_string(),
        active_transcript: vec![],
    });
    events.update_active_transcript(&mut handoff).await;
    assert_eq!(
        handoff,
        RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
            handoff_id: "handoff_1".to_string(),
            item_id: "item_1".to_string(),
            input_transcript: "already handed off".to_string(),
            active_transcript: vec![RealtimeTranscriptEntry {
                role: "user".to_string(),
                text: "already handed off".to_string(),
            }],
        })
    );

    let mut tail = RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta {
        delta: "tail".to_string(),
    });
    events.update_active_transcript(&mut tail).await;
    assert_eq!(
        events.take_transcript_tail().await,
        vec![RealtimeTranscriptEntry {
            role: "assistant".to_string(),
            text: "tail".to_string(),
        }]
    );
    assert_eq!(events.take_transcript_tail().await, vec![]);
}

#[tokio::test]
async fn transcript_state_survives_reconnect_and_reconciles_done() {
    let transcript_state = RealtimeTranscriptState::default();
    let (_first_tx, first_rx) = async_channel::unbounded();
    let first_events = RealtimeWebsocketEvents {
        rx_message: first_rx,
        pending_events: Arc::new(Mutex::new(VecDeque::new())),
        transcript_state: transcript_state.clone(),
        event_parser: RealtimeEventParser::FramelessBidi,
        is_closed: Arc::new(AtomicBool::new(false)),
    };
    let mut partial = RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
        delta: "hello wor".to_string(),
    });
    first_events.update_active_transcript(&mut partial).await;

    let (_second_tx, second_rx) = async_channel::unbounded();
    let second_events = RealtimeWebsocketEvents {
        rx_message: second_rx,
        pending_events: Arc::new(Mutex::new(VecDeque::new())),
        transcript_state,
        event_parser: RealtimeEventParser::FramelessBidi,
        is_closed: Arc::new(AtomicBool::new(false)),
    };
    let mut done = RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
        text: "hello world".to_string(),
    });
    second_events.update_active_transcript(&mut done).await;

    assert_eq!(
        second_events.take_transcript_tail().await,
        vec![RealtimeTranscriptEntry {
            role: "user".to_string(),
            text: "hello world".to_string(),
        }]
    );
    assert_eq!(second_events.take_transcript_tail().await, vec![]);
}

#[test]
fn terminal_sideband_handshake_statuses_are_not_retryable() {
    for status in [StatusCode::NOT_FOUND, StatusCode::GONE] {
        assert!(webrtc_sideband_session_ended(&ApiError::Api {
            status,
            message: "session ended".to_string(),
        }));
    }
    assert!(!webrtc_sideband_session_ended(&ApiError::Api {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: "retry".to_string(),
    }));
}

enum TestRealtimeTermination {
    Close(Option<CloseCode>),
    EventChannelEnd,
}

async fn realtime_termination_is_transport_error(
    event_parser: RealtimeEventParser,
    termination: TestRealtimeTermination,
) -> bool {
    let (tx_message, rx_message) = async_channel::unbounded();
    let events = RealtimeWebsocketEvents {
        rx_message,
        pending_events: Arc::new(Mutex::new(VecDeque::new())),
        transcript_state: RealtimeTranscriptState::default(),
        event_parser,
        is_closed: Arc::new(AtomicBool::new(false)),
    };
    match termination {
        TestRealtimeTermination::Close(close_code) => {
            let frame = close_code.map(|code| CloseFrame {
                code,
                reason: "session ended".into(),
            });
            tx_message
                .send(Ok(Message::Close(frame)))
                .await
                .expect("send close frame");
        }
        TestRealtimeTermination::EventChannelEnd => drop(tx_message),
    }
    events.next_event().await.is_err()
}

#[tokio::test]
async fn writer_policy_failure_reaches_realtime_events() {
    use codex_http_client::DestinationPolicy::Unrestricted;
    use codex_http_client::NetworkPolicyController;
    use codex_http_client::NetworkPolicyDenied::Revoked;
    use codex_http_client::TransportError::Policy;
    use codex_websocket_client::network_policy_denial;
    use tokio::time::timeout;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let (writing, started) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let socket = accept_async(socket).await.unwrap();
        socket.get_ref().peek(&mut [0]).await.unwrap();
        writing.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let controller = NetworkPolicyController::default();
    let policy = controller.policy();
    controller.publish(policy.revision(), Unrestricted);
    let factory = HttpClientFactory::new(ReqwestDefault).with_network_policy(policy.clone());
    let connector = WebSocketConnector::new(&factory).unwrap();
    let request = url.into_client_request().unwrap();
    let connected = connector.connect(request, websocket_config());
    let (socket, _) = connected.await.unwrap();
    let (stream, rx_message) = WsStream::new(socket);
    let parser = RealtimeEventParser::V1;
    let transcript = RealtimeTranscriptState::default();
    let connection = RealtimeWebsocketConnection::new(stream, rx_message, parser, transcript);
    let stream = Arc::clone(&connection.writer.stream);
    let payload = Message::Binary(vec![0; 8 * 1024 * 1024].into());
    let writer = tokio::spawn(async move {
        loop {
            if let Err(error) = stream.send(payload.clone()).await {
                return error;
            }
        }
    });
    let deadline = Duration::from_secs(/*secs*/ 5);
    timeout(deadline, started).await.unwrap().unwrap();
    policy.invalidate();
    let writer = timeout(deadline, writer).await.unwrap();
    let error = writer.unwrap();
    assert_eq!(network_policy_denial(&error), Some(Revoked));
    let next = timeout(deadline, connection.next_event());
    let error = next.await.unwrap().unwrap_err();
    assert!(matches!(error, ApiError::Transport(Policy(Revoked))));
    assert!(connection.next_event().await.unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn websocket_termination_classification_preserves_protocol_behavior() {
    use CloseCode::Away;
    use CloseCode::Normal;
    use RealtimeEventParser::FramelessBidi;
    use RealtimeEventParser::RealtimeV2;
    use RealtimeEventParser::V1;
    use TestRealtimeTermination::Close;
    use TestRealtimeTermination::EventChannelEnd;

    for (event_parser, termination, expect_transport_error) in [
        (FramelessBidi, Close(Some(Normal)), false),
        (FramelessBidi, Close(Some(Away)), true),
        (FramelessBidi, Close(None), true),
        (FramelessBidi, EventChannelEnd, true),
        (V1, Close(Some(Away)), false),
        (V1, EventChannelEnd, false),
        (RealtimeV2, Close(Some(Away)), false),
    ] {
        assert_eq!(
            realtime_termination_is_transport_error(event_parser, termination).await,
            expect_transport_error,
            "unexpected termination classification for {event_parser:?}"
        );
    }
}

#[test]
fn websocket_handshake_error_preserves_http_status() {
    let response = http::Response::builder()
        .status(StatusCode::GONE)
        .body(None)
        .expect("build response");

    assert!(matches!(
        map_realtime_websocket_connect_error(WsError::Http(Box::new(response))),
        ApiError::Api {
            status: StatusCode::GONE,
            ..
        }
    ));
}

#[test]
fn parse_input_transcript_delta_event() {
    let payload = json!({
        "type": "conversation.input_transcript.delta",
        "delta": "hello "
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::InputTranscriptDelta(
            RealtimeTranscriptDelta {
                delta: "hello ".to_string(),
            }
        ))
    );
}

#[test]
fn parse_v1_input_audio_transcription_delta_event() {
    let payload = json!({
        "type": "conversation.item.input_audio_transcription.delta",
        "item_id": "item_input_1",
        "content_index": 0,
        "delta": "hello"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::InputTranscriptDelta(
            RealtimeTranscriptDelta {
                delta: "hello".to_string(),
            }
        ))
    );
}

#[test]
fn parse_v1_input_audio_transcription_completed_event() {
    let payload = json!({
        "type": "conversation.item.input_audio_transcription.completed",
        "item_id": "item_input_1",
        "content_index": 0,
        "transcript": "hello world"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
            text: "hello world".to_string(),
        }))
    );
}

#[test]
fn parse_v1_input_transcript_turn_marked_event() {
    let payload = json!({
        "type": "conversation.input_transcript.turn_marked",
        "transcript": "hello realtime"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
            text: "hello realtime".to_string(),
        }))
    );
}

#[test]
fn parse_output_transcript_delta_event() {
    let payload = json!({
        "type": "conversation.output_transcript.delta",
        "delta": "hi"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::OutputTranscriptDelta(
            RealtimeTranscriptDelta {
                delta: "hi".to_string(),
            }
        ))
    );
}

#[test]
fn parse_v1_output_audio_transcript_delta_event() {
    let payload = json!({
        "type": "response.output_audio_transcript.delta",
        "delta": "hi"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::OutputTranscriptDelta(
            RealtimeTranscriptDelta {
                delta: "hi".to_string(),
            }
        ))
    );
}

#[test]
fn parse_v1_output_audio_transcript_done_event() {
    let payload = json!({
        "type": "response.output_audio_transcript.done",
        "transcript": "hi there"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::OutputTranscriptDone(
            RealtimeTranscriptDone {
                text: "hi there".to_string(),
            }
        ))
    );
}

#[test]
fn parse_v1_item_done_output_text_event() {
    let payload = json!({
        "type": "conversation.item.done",
        "item": {
            "id": "item_output_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "output_text", "text": "hello"},
                {"type": "output_text", "text": " world"}
            ]
        }
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::V1),
        Some(RealtimeEvent::ConversationItemDone {
            item_id: "item_output_1".to_string(),
        })
    );
}

#[test]
fn parse_realtime_v2_handoff_tool_call_event() {
    let payload = json!({
        "type": "conversation.item.done",
        "item": {
            "id": "item_123",
            "type": "function_call",
            "name": "background_agent",
            "call_id": "call_123",
            "arguments": "{\"prompt\":\"delegate this\"}"
        }
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
            handoff_id: "call_123".to_string(),
            item_id: "item_123".to_string(),
            input_transcript: "delegate this".to_string(),
            active_transcript: Vec::new(),
        }))
    );
}

#[test]
fn parse_realtime_v2_noop_tool_call_event() {
    let payload = json!({
        "type": "conversation.item.done",
        "item": {
            "id": "item_silent",
            "type": "function_call",
            "name": "remain_silent",
            "call_id": "call_silent",
            "arguments": "{}"
        }
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::NoopRequested(RealtimeNoopRequested {
            call_id: "call_silent".to_string(),
            item_id: "item_silent".to_string(),
        }))
    );
}

#[test]
fn parse_realtime_v2_input_audio_transcription_delta_event() {
    let payload = json!({
        "type": "conversation.item.input_audio_transcription.delta",
        "item_id": "item_input_1",
        "content_index": 0,
        "delta": "hello"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::InputTranscriptDelta(
            RealtimeTranscriptDelta {
                delta: "hello".to_string(),
            }
        ))
    );
}

#[test]
fn parse_realtime_v2_output_audio_transcript_done_event() {
    let payload = json!({
        "type": "response.output_audio_transcript.done",
        "transcript": "hello there"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::OutputTranscriptDone(
            RealtimeTranscriptDone {
                text: "hello there".to_string(),
            }
        ))
    );
}

#[test]
fn parse_realtime_v2_output_text_done_event() {
    let payload = json!({
        "type": "response.output_text.done",
        "text": "hello there"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::OutputTranscriptDone(
            RealtimeTranscriptDone {
                text: "hello there".to_string(),
            }
        ))
    );
}

#[test]
fn parse_realtime_v2_conversation_item_created_event() {
    let payload = json!({
        "type": "conversation.item.created",
        "item": {"type": "message", "role": "user"}
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::ConversationItemAdded(
            json!({"type": "message", "role": "user"})
        ))
    );
}

#[test]
fn parse_realtime_v2_item_done_output_text_event() {
    let payload = json!({
        "type": "conversation.item.done",
        "item": {
            "id": "item_output_1",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "output_text", "text": "hello"},
                {"type": "output_text", "text": " world"}
            ]
        }
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::ConversationItemDone {
            item_id: "item_output_1".to_string(),
        })
    );
}

#[test]
fn parse_realtime_v2_output_audio_delta_defaults_audio_shape() {
    let payload = json!({
        "type": "response.output_audio.delta",
        "delta": "AQID"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
            data: "AQID".to_string(),
            sample_rate: 24_000,
            num_channels: 1,
            samples_per_channel: None,
            item_id: None,
        }))
    );
}

#[test]
fn parse_realtime_v2_response_audio_delta_with_item_id() {
    let payload = json!({
        "type": "response.audio.delta",
        "delta": "AQID",
        "item_id": "item_audio_1"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::AudioOut(RealtimeAudioFrame {
            data: "AQID".to_string(),
            sample_rate: 24_000,
            num_channels: 1,
            samples_per_channel: None,
            item_id: Some("item_audio_1".to_string()),
        }))
    );
}

#[test]
fn parse_realtime_v2_speech_started_event() {
    let payload = json!({
        "type": "input_audio_buffer.speech_started",
        "item_id": "item_input_1"
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::InputAudioSpeechStarted(
            RealtimeInputAudioSpeechStarted {
                item_id: Some("item_input_1".to_string()),
            }
        ))
    );
}

#[test]
fn parse_realtime_v2_response_cancelled_event() {
    let payload = json!({
        "type": "response.cancelled",
        "response": {"id": "resp_cancelled_1"}
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::ResponseCancelled(
            RealtimeResponseCancelled {
                response_id: Some("resp_cancelled_1".to_string()),
            }
        ))
    );
}

#[test]
fn parse_realtime_v2_response_done_event() {
    let payload = json!({
        "type": "response.done",
        "response": {
            "output": [{
                "id": "item_123",
                "type": "function_call",
                "name": "background_agent",
                "call_id": "call_123",
                "arguments": "{\"prompt\":\"delegate from done\"}"
            }]
        }
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::ResponseDone(RealtimeResponseDone {
            response_id: None
        }))
    );
}

#[test]
fn parse_realtime_v2_response_created_event() {
    let payload = json!({
        "type": "response.created",
        "response": {"id": "resp_created_1"}
    })
    .to_string();

    assert_eq!(
        parse_realtime_event(payload.as_str(), RealtimeEventParser::RealtimeV2),
        Some(RealtimeEvent::ResponseCreated(RealtimeResponseCreated {
            response_id: Some("resp_created_1".to_string())
        }))
    );
}

#[test]
fn merge_request_headers_matches_http_precedence() {
    let mut provider_headers = HeaderMap::new();
    provider_headers.insert(
        "originator",
        HeaderValue::from_static("provider-originator"),
    );
    provider_headers.insert("x-priority", HeaderValue::from_static("provider"));

    let mut extra_headers = HeaderMap::new();
    extra_headers.insert("x-priority", HeaderValue::from_static("extra"));

    let mut default_headers = HeaderMap::new();
    default_headers.insert("originator", HeaderValue::from_static("default-originator"));
    default_headers.insert("x-priority", HeaderValue::from_static("default"));
    default_headers.insert("x-default-only", HeaderValue::from_static("default-only"));

    let merged = merge_request_headers(&provider_headers, extra_headers, default_headers);

    assert_eq!(
        merged.get("originator"),
        Some(&HeaderValue::from_static("provider-originator"))
    );
    assert_eq!(
        merged.get("x-priority"),
        Some(&HeaderValue::from_static("extra"))
    );
    assert_eq!(
        merged.get("x-default-only"),
        Some(&HeaderValue::from_static("default-only"))
    );
}

#[test]
fn websocket_url_from_http_base_defaults_to_ws_path() {
    let url = websocket_url_from_api_url(
        "http://127.0.0.1:8011",
        /*query_params*/ None,
        /*model*/ None,
        RealtimeEventParser::V1,
        RealtimeSessionMode::Conversational,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "ws://127.0.0.1:8011/v1/realtime?intent=quicksilver"
    );
}

#[test]
fn websocket_url_from_ws_base_defaults_to_ws_path() {
    let url = websocket_url_from_api_url(
        "wss://example.com",
        /*query_params*/ None,
        Some("realtime-test-model"),
        RealtimeEventParser::V1,
        RealtimeSessionMode::Conversational,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://example.com/v1/realtime?intent=quicksilver&model=realtime-test-model"
    );
}

#[test]
fn websocket_url_from_v1_base_appends_realtime_path() {
    let url = websocket_url_from_api_url(
        "https://api.openai.com/v1",
        /*query_params*/ None,
        Some("snapshot"),
        RealtimeEventParser::V1,
        RealtimeSessionMode::Conversational,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://api.openai.com/v1/realtime?intent=quicksilver&model=snapshot"
    );
}

#[test]
fn websocket_url_from_nested_v1_base_appends_realtime_path() {
    let url = websocket_url_from_api_url(
        "https://example.com/openai/v1",
        /*query_params*/ None,
        Some("snapshot"),
        RealtimeEventParser::V1,
        RealtimeSessionMode::Conversational,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://example.com/openai/v1/realtime?intent=quicksilver&model=snapshot"
    );
}

#[test]
fn websocket_url_preserves_existing_realtime_path_and_extra_query_params() {
    let url = websocket_url_from_api_url(
        "https://example.com/v1/realtime?foo=bar",
        Some(&HashMap::from([
            ("trace".to_string(), "1".to_string()),
            ("intent".to_string(), "ignored".to_string()),
        ])),
        Some("snapshot"),
        RealtimeEventParser::V1,
        RealtimeSessionMode::Conversational,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://example.com/v1/realtime?foo=bar&intent=quicksilver&model=snapshot&trace=1"
    );
}

#[test]
fn frameless_websocket_url_rewrites_existing_realtime_path() {
    let url = websocket_url_from_api_url(
        "wss://example.com/v1/realtime?foo=bar",
        /*query_params*/ None,
        Some("snapshot"),
        RealtimeEventParser::FramelessBidi,
        RealtimeSessionMode::Conversational,
    )
    .expect("build Frameless websocket url");
    assert_eq!(
        url.as_str(),
        "wss://example.com/v1/live?foo=bar&model=snapshot"
    );
}

#[test]
fn websocket_url_v1_ignores_transcription_mode() {
    let url = websocket_url_from_api_url(
        "https://example.com",
        /*query_params*/ None,
        /*model*/ None,
        RealtimeEventParser::V1,
        RealtimeSessionMode::Transcription,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://example.com/v1/realtime?intent=quicksilver"
    );
}

#[test]
fn websocket_url_omits_intent_for_realtime_v2_conversational_mode() {
    let url = websocket_url_from_api_url(
        "https://example.com/v1/realtime?foo=bar",
        Some(&HashMap::from([
            ("trace".to_string(), "1".to_string()),
            ("intent".to_string(), "ignored".to_string()),
        ])),
        Some("snapshot"),
        RealtimeEventParser::RealtimeV2,
        RealtimeSessionMode::Conversational,
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://example.com/v1/realtime?foo=bar&model=snapshot&trace=1"
    );
}

#[test]
fn websocket_url_omits_intent_for_realtime_v2_transcription_mode() {
    let url = websocket_url_from_api_url(
        "https://example.com",
        /*query_params*/ None,
        /*model*/ None,
        RealtimeEventParser::RealtimeV2,
        RealtimeSessionMode::Transcription,
    )
    .expect("build ws url");
    assert_eq!(url.as_str(), "wss://example.com/v1/realtime");
}

#[test]
fn websocket_url_for_call_id_joins_existing_realtime_session() {
    let url = websocket_url_from_api_url_for_call(
        "https://api.openai.com/v1",
        /*query_params*/ None,
        RealtimeEventParser::RealtimeV2,
        RealtimeSessionMode::Conversational,
        "rtc_test",
    )
    .expect("build ws url");
    assert_eq!(
        url.as_str(),
        "wss://api.openai.com/v1/realtime?call_id=rtc_test"
    );
}

#[test]
fn frameless_websocket_url_encodes_call_id_as_one_path_segment() {
    let url = websocket_url_from_api_url_for_call(
        "https://example.com/proxy/v1/live?trace=1",
        /*query_params*/ None,
        RealtimeEventParser::FramelessBidi,
        RealtimeSessionMode::Conversational,
        "../../admin",
    )
    .expect("build existing-call websocket url");

    assert_eq!(
        url.as_str(),
        "wss://example.com/proxy/v1/live/..%2F..%2Fadmin?trace=1"
    );
}

#[test]
fn frameless_websocket_url_rejects_dot_only_call_ids() {
    for call_id in [".", ".."] {
        let error = websocket_url_from_api_url_for_call(
            "https://example.com/v1/live",
            /*query_params*/ None,
            RealtimeEventParser::FramelessBidi,
            RealtimeSessionMode::Conversational,
            call_id,
        )
        .expect_err("dot-only call IDs must not target the parent sideband URL");

        assert!(matches!(
            error,
            ApiError::InvalidRequest { message }
                if message == format!("invalid realtime call id: {call_id}")
        ));
    }
}

#[test]
fn webrtc_frameless_sideband_ignores_provider_base_url() {
    let client = test_client(Provider {
        name: "chatgpt".to_string(),
        base_url: "https://chatgpt.com/backend-api/codex".to_string(),
        query_params: None,
        headers: HeaderMap::new(),
        retry: RetryConfig {
            max_attempts: 0,
            base_delay: Duration::ZERO,
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    });

    let url = client
        .webrtc_sideband_url(
            RealtimeEventParser::FramelessBidi,
            RealtimeSessionMode::Conversational,
            "rtc_test",
        )
        .expect("build ws url");

    assert_eq!(url.as_str(), "wss://api.openai.com/v1/live/rtc_test");
}

#[tokio::test]
async fn e2e_connect_and_exchange_events_against_mock_ws_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = accept_async(stream).await.expect("accept ws");

        let first = ws
            .next()
            .await
            .expect("first msg")
            .expect("first msg ok")
            .into_text()
            .expect("text");
        let first_json: Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first_json["type"], "session.update");
        assert_eq!(
            first_json["session"]["type"],
            Value::String("quicksilver".to_string())
        );
        assert_eq!(
            first_json["session"]["instructions"],
            Value::String("backend prompt".to_string())
        );
        assert_eq!(
            first_json["session"]["audio"]["input"]["format"]["type"],
            Value::String("audio/pcm".to_string())
        );
        assert_eq!(
            first_json["session"]["audio"]["input"]["format"]["rate"],
            Value::from(24_000)
        );
        assert_eq!(
            first_json["session"]["audio"]["output"]["voice"],
            Value::String("breeze".to_string())
        );

        ws.send(Message::Text(
            json!({
                "type": "session.updated",
                "session": {"id": "sess_mock", "instructions": "backend prompt"}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send session.updated");

        let second = ws
            .next()
            .await
            .expect("second msg")
            .expect("second msg ok")
            .into_text()
            .expect("text");
        let second_json: Value = serde_json::from_str(&second).expect("json");
        assert_eq!(second_json["type"], "input_audio_buffer.append");

        let third = ws
            .next()
            .await
            .expect("third msg")
            .expect("third msg ok")
            .into_text()
            .expect("text");
        let third_json: Value = serde_json::from_str(&third).expect("json");
        assert_eq!(third_json["type"], "conversation.item.create");
        assert_eq!(third_json["item"]["role"], "developer");
        assert_eq!(
            third_json["item"]["content"][0]["type"],
            Value::String("input_text".to_string())
        );
        assert_eq!(third_json["item"]["content"][0]["text"], "hello agent");

        let fourth = ws
            .next()
            .await
            .expect("fourth msg")
            .expect("fourth msg ok")
            .into_text()
            .expect("text");
        let fourth_json: Value = serde_json::from_str(&fourth).expect("json");
        assert_eq!(fourth_json["type"], "conversation.item.create");
        assert_eq!(fourth_json["item"]["role"], "assistant");
        assert_eq!(
            fourth_json["item"]["content"][0]["type"],
            Value::String("output_text".to_string())
        );
        assert_eq!(
            fourth_json["item"]["content"][0]["text"],
            Value::String("assistant context".to_string())
        );

        let fifth = ws
            .next()
            .await
            .expect("fifth msg")
            .expect("fifth msg ok")
            .into_text()
            .expect("text");
        let fifth_json: Value = serde_json::from_str(&fifth).expect("json");
        assert_eq!(fifth_json["type"], "conversation.handoff.append");
        assert_eq!(fifth_json["handoff_id"], "handoff_1");
        assert_eq!(
            fifth_json["output_text"],
            "\"Agent Final Message\":\n\nhello from background agent"
        );

        ws.send(Message::Text(
            json!({
                "type": "conversation.output_audio.delta",
                "delta": "AQID",
                "sample_rate": 48000,
                "channels": 1
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send audio");

        ws.send(Message::Text(
            json!({
                "type": "conversation.input_transcript.delta",
                "delta": "delegate "
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send input transcript delta");

        ws.send(Message::Text(
            json!({
                "type": "conversation.input_transcript.delta",
                "delta": "now"
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send input transcript delta");

        ws.send(Message::Text(
            json!({
                "type": "conversation.output_transcript.delta",
                "delta": "working"
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send output transcript delta");

        ws.send(Message::Text(
            json!({
                "type": "conversation.handoff.requested",
                "handoff_id": "handoff_1",
                "item_id": "item_2",
                "input_transcript": "delegate now"
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send item added");
    });

    let provider = Provider {
        name: "test".to_string(),
        base_url: format!("http://{addr}"),
        query_params: Some(HashMap::new()),
        headers: HeaderMap::new(),
        retry: crate::provider::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    };
    let client = test_client(provider);
    let connection = client
        .connect(
            RealtimeSessionConfig {
                instructions: "backend prompt".to_string(),
                initial_items: Vec::new(),
                delegation_ack_filler: None,
                model: Some("realtime-test-model".to_string()),
                session_id: Some("conv_1".to_string()),
                event_parser: RealtimeEventParser::V1,
                session_mode: RealtimeSessionMode::Conversational,
                output_modality: RealtimeOutputModality::Audio,
                voice: RealtimeVoice::Breeze,
            },
            HeaderMap::new(),
            HeaderMap::new(),
        )
        .await
        .expect("connect");

    let created = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        created,
        RealtimeEvent::SessionUpdated {
            realtime_session_id: "sess_mock".to_string(),
            instructions: Some("backend prompt".to_string()),
        }
    );

    connection
        .send_audio_frame(RealtimeAudioFrame {
            data: "AQID".to_string(),
            sample_rate: 48000,
            num_channels: 1,
            samples_per_channel: Some(960),
            item_id: None,
        })
        .await
        .expect("send audio");
    connection
        .send_conversation_item_create("hello agent".to_string(), ConversationTextRole::Developer)
        .await
        .expect("send item");
    connection
        .send_conversation_item_create(
            "assistant context".to_string(),
            ConversationTextRole::Assistant,
        )
        .await
        .expect("send assistant item");
    connection
        .send_conversation_function_call_output(
            "handoff_1".to_string(),
            "hello from background agent".to_string(),
        )
        .await
        .expect("send handoff");

    let audio_event = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        audio_event,
        RealtimeEvent::AudioOut(RealtimeAudioFrame {
            data: "AQID".to_string(),
            sample_rate: 48000,
            num_channels: 1,
            samples_per_channel: None,
            item_id: None,
        })
    );

    let input_delta_event = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        input_delta_event,
        RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
            delta: "delegate ".to_string(),
        })
    );

    let input_delta_event = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        input_delta_event,
        RealtimeEvent::InputTranscriptDelta(RealtimeTranscriptDelta {
            delta: "now".to_string(),
        })
    );

    let output_delta_event = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        output_delta_event,
        RealtimeEvent::OutputTranscriptDelta(RealtimeTranscriptDelta {
            delta: "working".to_string(),
        })
    );

    let added_event = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        added_event,
        RealtimeEvent::HandoffRequested(RealtimeHandoffRequested {
            handoff_id: "handoff_1".to_string(),
            item_id: "item_2".to_string(),
            input_transcript: "delegate now".to_string(),
            active_transcript: vec![
                RealtimeTranscriptEntry {
                    role: "user".to_string(),
                    text: "delegate now".to_string(),
                },
                RealtimeTranscriptEntry {
                    role: "assistant".to_string(),
                    text: "working".to_string(),
                },
            ],
        })
    );

    connection.close().await.expect("close");
    server.await.expect("server task");
}

#[tokio::test]
async fn realtime_v2_session_update_includes_background_agent_tool_and_handoff_output_item() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = accept_async(stream).await.expect("accept ws");

        let first = ws
            .next()
            .await
            .expect("first msg")
            .expect("first msg ok")
            .into_text()
            .expect("text");
        let first_json: Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first_json["type"], "session.update");
        assert_eq!(
            first_json["session"]["type"],
            Value::String("realtime".to_string())
        );
        assert_eq!(first_json["session"]["output_modalities"], json!(["audio"]));
        assert_eq!(
            first_json["session"]["audio"]["input"]["format"],
            json!({
                "type": "audio/pcm",
                "rate": 24_000,
            })
        );
        assert_eq!(
            first_json["session"]["audio"]["input"]["noise_reduction"],
            json!({
                "type": "near_field",
            })
        );
        assert_eq!(
            first_json["session"]["audio"]["input"]["transcription"],
            json!({
                "model": "gpt-4o-mini-transcribe",
            })
        );
        assert_eq!(
            first_json["session"]["audio"]["input"]["turn_detection"],
            json!({
                "type": "server_vad",
                "interrupt_response": true,
                "create_response": true,
                "silence_duration_ms": 500,
            })
        );
        assert_eq!(
            first_json["session"]["audio"]["output"]["format"],
            json!({
                "type": "audio/pcm",
                "rate": 24_000,
            })
        );
        assert_eq!(
            first_json["session"]["audio"]["output"]["voice"],
            Value::String("cedar".to_string())
        );
        assert_eq!(
            first_json["session"]["tools"][0]["type"],
            Value::String("function".to_string())
        );
        assert_eq!(
            first_json["session"]["tools"][0]["name"],
            Value::String("background_agent".to_string())
        );
        assert_eq!(
            first_json["session"]["tools"][0]["parameters"]["required"],
            json!(["prompt"])
        );
        assert_eq!(
            first_json["session"]["tools"][1]["type"],
            Value::String("function".to_string())
        );
        assert_eq!(
            first_json["session"]["tools"][1]["name"],
            Value::String("remain_silent".to_string())
        );
        assert_eq!(
            first_json["session"]["tools"][1]["parameters"]["properties"],
            json!({})
        );
        assert_eq!(
            first_json["session"]["tool_choice"],
            Value::String("auto".to_string())
        );

        ws.send(Message::Text(
            json!({
                "type": "session.updated",
                "session": {"id": "sess_v2", "instructions": "backend prompt"}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send session.updated");

        let second = ws
            .next()
            .await
            .expect("second msg")
            .expect("second msg ok")
            .into_text()
            .expect("text");
        let second_json: Value = serde_json::from_str(&second).expect("json");
        assert_eq!(second_json["type"], "conversation.item.create");
        assert_eq!(second_json["item"]["role"], "developer");
        assert_eq!(
            second_json["item"]["type"],
            Value::String("message".to_string())
        );
        assert_eq!(
            second_json["item"]["content"][0]["type"],
            Value::String("input_text".to_string())
        );
        assert_eq!(
            second_json["item"]["content"][0]["text"],
            Value::String("delegate this".to_string())
        );

        let third = ws
            .next()
            .await
            .expect("third msg")
            .expect("third msg ok")
            .into_text()
            .expect("text");
        let third_json: Value = serde_json::from_str(&third).expect("json");
        assert_eq!(third_json["type"], "conversation.item.create");
        assert_eq!(third_json["item"]["role"], "assistant");
        assert_eq!(
            third_json["item"]["content"][0]["type"],
            Value::String("output_text".to_string())
        );
        assert_eq!(
            third_json["item"]["content"][0]["text"],
            Value::String("assistant context".to_string())
        );

        let fourth = ws
            .next()
            .await
            .expect("fourth msg")
            .expect("fourth msg ok")
            .into_text()
            .expect("text");
        let fourth_json: Value = serde_json::from_str(&fourth).expect("json");
        assert_eq!(fourth_json["type"], "conversation.item.create");
        assert_eq!(
            fourth_json["item"]["type"],
            Value::String("function_call_output".to_string())
        );
        assert_eq!(
            fourth_json["item"]["call_id"],
            Value::String("call_1".to_string())
        );
        assert_eq!(
            fourth_json["item"]["output"],
            Value::String("delegated result".to_string())
        );
    });

    let provider = Provider {
        name: "test".to_string(),
        base_url: format!("http://{addr}"),
        query_params: Some(HashMap::new()),
        headers: HeaderMap::new(),
        retry: crate::provider::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    };
    let client = test_client(provider);
    let connection = client
        .connect(
            RealtimeSessionConfig {
                instructions: "backend prompt".to_string(),
                initial_items: Vec::new(),
                delegation_ack_filler: None,
                model: Some("realtime-test-model".to_string()),
                session_id: Some("conv_1".to_string()),
                event_parser: RealtimeEventParser::RealtimeV2,
                session_mode: RealtimeSessionMode::Conversational,
                output_modality: RealtimeOutputModality::Audio,
                voice: RealtimeVoice::Cedar,
            },
            HeaderMap::new(),
            HeaderMap::new(),
        )
        .await
        .expect("connect");

    let created = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        created,
        RealtimeEvent::SessionUpdated {
            realtime_session_id: "sess_v2".to_string(),
            instructions: Some("backend prompt".to_string()),
        }
    );

    connection
        .send_conversation_item_create("delegate this".to_string(), ConversationTextRole::Developer)
        .await
        .expect("send text item");
    connection
        .send_conversation_item_create(
            "assistant context".to_string(),
            ConversationTextRole::Assistant,
        )
        .await
        .expect("send assistant item");
    connection
        .send_conversation_function_call_output(
            "call_1".to_string(),
            "delegated result".to_string(),
        )
        .await
        .expect("send handoff output");

    connection.close().await.expect("close");
    server.await.expect("server task");
}

#[tokio::test]
async fn transcription_mode_session_update_omits_output_audio_and_instructions() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = accept_async(stream).await.expect("accept ws");

        let first = ws
            .next()
            .await
            .expect("first msg")
            .expect("first msg ok")
            .into_text()
            .expect("text");
        let first_json: Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first_json["type"], "session.update");
        assert_eq!(
            first_json["session"]["type"],
            Value::String("transcription".to_string())
        );
        assert!(first_json["session"].get("instructions").is_none());
        assert_eq!(
            first_json["session"]["audio"]["input"]["transcription"],
            json!({
                "model": "gpt-4o-mini-transcribe",
            })
        );
        assert!(first_json["session"]["audio"].get("output").is_none());
        assert!(first_json["session"].get("tools").is_none());

        ws.send(Message::Text(
            json!({
                "type": "session.updated",
                "session": {"id": "sess_transcription"}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send session.updated");

        let second = ws
            .next()
            .await
            .expect("second msg")
            .expect("second msg ok")
            .into_text()
            .expect("text");
        let second_json: Value = serde_json::from_str(&second).expect("json");
        assert_eq!(second_json["type"], "input_audio_buffer.append");
    });

    let provider = Provider {
        name: "test".to_string(),
        base_url: format!("http://{addr}"),
        query_params: Some(HashMap::new()),
        headers: HeaderMap::new(),
        retry: crate::provider::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    };
    let client = test_client(provider);
    let connection = client
        .connect(
            RealtimeSessionConfig {
                instructions: "backend prompt".to_string(),
                initial_items: Vec::new(),
                delegation_ack_filler: None,
                model: Some("realtime-test-model".to_string()),
                session_id: Some("conv_1".to_string()),
                event_parser: RealtimeEventParser::RealtimeV2,
                session_mode: RealtimeSessionMode::Transcription,
                output_modality: RealtimeOutputModality::Audio,
                voice: RealtimeVoice::Marin,
            },
            HeaderMap::new(),
            HeaderMap::new(),
        )
        .await
        .expect("connect");

    let created = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        created,
        RealtimeEvent::SessionUpdated {
            realtime_session_id: "sess_transcription".to_string(),
            instructions: None,
        }
    );

    connection
        .send_audio_frame(RealtimeAudioFrame {
            data: "AQID".to_string(),
            sample_rate: 24_000,
            num_channels: 1,
            samples_per_channel: Some(480),
            item_id: None,
        })
        .await
        .expect("send audio");

    connection.close().await.expect("close");
    server.await.expect("server task");
}

#[tokio::test]
async fn v1_transcription_mode_is_treated_as_conversational() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = accept_async(stream).await.expect("accept ws");

        let first = ws
            .next()
            .await
            .expect("first msg")
            .expect("first msg ok")
            .into_text()
            .expect("text");
        let first_json: Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first_json["type"], "session.update");
        assert_eq!(
            first_json["session"]["type"],
            Value::String("quicksilver".to_string())
        );
        assert_eq!(
            first_json["session"]["instructions"],
            Value::String("backend prompt".to_string())
        );
        assert_eq!(
            first_json["session"]["audio"]["output"]["voice"],
            Value::String("cove".to_string())
        );
        assert!(first_json["session"].get("tools").is_none());

        ws.send(Message::Text(
            json!({
                "type": "session.updated",
                "session": {"id": "sess_v1_mode"}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send session.updated");
    });

    let provider = Provider {
        name: "test".to_string(),
        base_url: format!("http://{addr}"),
        query_params: Some(HashMap::new()),
        headers: HeaderMap::new(),
        retry: crate::provider::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    };
    let client = test_client(provider);
    let connection = client
        .connect(
            RealtimeSessionConfig {
                instructions: "backend prompt".to_string(),
                initial_items: Vec::new(),
                delegation_ack_filler: None,
                model: Some("realtime-test-model".to_string()),
                session_id: Some("conv_1".to_string()),
                event_parser: RealtimeEventParser::V1,
                session_mode: RealtimeSessionMode::Transcription,
                output_modality: RealtimeOutputModality::Audio,
                voice: RealtimeVoice::Cove,
            },
            HeaderMap::new(),
            HeaderMap::new(),
        )
        .await
        .expect("connect");

    let created = connection
        .next_event()
        .await
        .expect("next event")
        .expect("event");
    assert_eq!(
        created,
        RealtimeEvent::SessionUpdated {
            realtime_session_id: "sess_v1_mode".to_string(),
            instructions: None,
        }
    );

    connection.close().await.expect("close");
    server.await.expect("server task");
}

#[tokio::test]
async fn send_does_not_block_while_next_event_waits_for_inbound_data() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut ws = accept_async(stream).await.expect("accept ws");

        let first = ws
            .next()
            .await
            .expect("first msg")
            .expect("first msg ok")
            .into_text()
            .expect("text");
        let first_json: Value = serde_json::from_str(&first).expect("json");
        assert_eq!(first_json["type"], "session.update");

        let second = ws
            .next()
            .await
            .expect("second msg")
            .expect("second msg ok")
            .into_text()
            .expect("text");
        let second_json: Value = serde_json::from_str(&second).expect("json");
        assert_eq!(second_json["type"], "input_audio_buffer.append");

        ws.send(Message::Text(
            json!({
                "type": "session.updated",
                "session": {"id": "sess_after_send", "instructions": "backend prompt"}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send session.updated");
    });

    let provider = Provider {
        name: "test".to_string(),
        base_url: format!("http://{addr}"),
        query_params: Some(HashMap::new()),
        headers: HeaderMap::new(),
        retry: crate::provider::RetryConfig {
            max_attempts: 1,
            base_delay: Duration::from_millis(1),
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: Duration::from_secs(5),
    };
    let client = test_client(provider);
    let connection = client
        .connect(
            RealtimeSessionConfig {
                instructions: "backend prompt".to_string(),
                initial_items: Vec::new(),
                delegation_ack_filler: None,
                model: Some("realtime-test-model".to_string()),
                session_id: Some("conv_1".to_string()),
                event_parser: RealtimeEventParser::V1,
                session_mode: RealtimeSessionMode::Conversational,
                output_modality: RealtimeOutputModality::Audio,
                voice: RealtimeVoice::Cove,
            },
            HeaderMap::new(),
            HeaderMap::new(),
        )
        .await
        .expect("connect");

    let (send_result, next_result) = tokio::join!(
        async {
            tokio::time::timeout(
                Duration::from_millis(200),
                connection.send_audio_frame(RealtimeAudioFrame {
                    data: "AQID".to_string(),
                    sample_rate: 48000,
                    num_channels: 1,
                    samples_per_channel: Some(960),
                    item_id: None,
                }),
            )
            .await
        },
        connection.next_event()
    );

    send_result
        .expect("send should not block on next_event")
        .expect("send audio");
    let next_event = next_result.expect("next event").expect("event");
    assert_eq!(
        next_event,
        RealtimeEvent::SessionUpdated {
            realtime_session_id: "sess_after_send".to_string(),
            instructions: Some("backend prompt".to_string()),
        }
    );

    connection.close().await.expect("close");
    server.await.expect("server task");
}
