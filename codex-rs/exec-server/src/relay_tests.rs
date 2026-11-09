use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use codex_exec_server_protocol::JSONRPCRequest;
use codex_exec_server_protocol::JSONRPCResponse;
use codex_exec_server_protocol::RequestId;
use futures::Sink;
use futures::Stream;
use futures::channel::mpsc as futures_mpsc;
use futures::task::AtomicWaker;
use pretty_assertions::assert_eq;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::*;

#[test]
fn handshake_diagnostics_are_capped_per_window() {
    let start = Instant::now();
    let mut window = (start, 0);
    for slot in 0..MAX_HANDSHAKE_DIAGNOSTICS_PER_MINUTE {
        assert_eq!(handshake_diagnostic_slot(&mut window, start), slot);
    }
    assert_eq!(
        handshake_diagnostic_slot(&mut window, start),
        MAX_HANDSHAKE_DIAGNOSTICS_PER_MINUTE
    );
    assert_eq!(
        handshake_diagnostic_slot(&mut window, start),
        MAX_HANDSHAKE_DIAGNOSTICS_PER_MINUTE + 1
    );
    let next = start + HANDSHAKE_DIAGNOSTIC_WINDOW;
    for slot in 0..MAX_HANDSHAKE_DIAGNOSTICS_PER_MINUTE {
        assert_eq!(handshake_diagnostic_slot(&mut window, next), slot);
    }
    assert_eq!(
        handshake_diagnostic_slot(&mut window, next),
        MAX_HANDSHAKE_DIAGNOSTICS_PER_MINUTE
    );
    assert_eq!(
        handshake_diagnostic_slot(&mut window, next),
        MAX_HANDSHAKE_DIAGNOSTICS_PER_MINUTE + 1
    );
}

#[tokio::test]
async fn harness_connection_sends_keepalive_and_receives_relay_data() -> anyhow::Result<()> {
    let (client_websocket, mut server_websocket) = websocket_pair().await?;
    let mut connection = harness_connection_from_websocket(client_websocket, "test".to_string());
    let stream_id = read_resume_stream_id(&mut server_websocket).await?;
    read_keepalive_ping(&mut server_websocket).await?;
    server_websocket
        .send(Message::Pong(b"keepalive".to_vec().into()))
        .await?;
    let message = test_jsonrpc_message();

    server_websocket
        .send(Message::Binary(
            encode_relay_message_frame(&RelayMessageFrame::data(
                stream_id,
                /*seq*/ 0,
                jsonrpc_payload(&message)?,
                /*trace*/ None,
            ))
            .into(),
        ))
        .await?;
    let Some(JsonRpcConnectionEvent::QueuedRequest { request, .. }) =
        timeout(Duration::from_secs(1), connection.incoming_rx.recv()).await?
    else {
        anyhow::bail!("expected a queued JSON-RPC request");
    };
    assert_eq!(JSONRPCMessage::Request(request), message);

    drop(connection);
    Ok(())
}

#[tokio::test]
async fn harness_connection_rejects_oversized_request_and_accepts_large_response()
-> anyhow::Result<()> {
    let (client_websocket, mut server_websocket) = websocket_pair().await?;
    let mut connection = harness_connection_from_websocket(client_websocket, "test".to_string());
    let stream_id = read_resume_stream_id(&mut server_websocket).await?;
    let response = JSONRPCMessage::Response(JSONRPCResponse {
        id: RequestId::Integer(1),
        result: serde_json::json!({
            "padding": "x".repeat(MAX_CLIENT_INBOUND_REQUEST_LEN),
        }),
    });

    server_websocket
        .send(Message::Binary(
            encode_relay_message_frame(&RelayMessageFrame {
                version: RELAY_MESSAGE_FRAME_VERSION,
                stream_id: stream_id.clone(),
                body: Some(relay_message_frame::Body::Data(RelayData {
                    seq: 0,
                    segment_index: 0,
                    segment_count: 1,
                    payload: jsonrpc_payload(&response)?,
                })),
                ..RelayMessageFrame::default()
            })
            .into(),
        ))
        .await?;
    assert!(matches!(
        timeout(Duration::from_secs(1), connection.incoming_rx.recv()).await?,
        Some(JsonRpcConnectionEvent::Message(actual)) if actual == response
    ));

    let request = JSONRPCMessage::Request(JSONRPCRequest {
        id: RequestId::Integer(2),
        method: "network/policyRequest".to_string(),
        params: Some(serde_json::json!({
            "padding": "x".repeat(MAX_CLIENT_INBOUND_REQUEST_LEN),
        })),
        trace: None,
    });
    server_websocket
        .send(Message::Binary(
            encode_relay_message_frame(&RelayMessageFrame {
                version: RELAY_MESSAGE_FRAME_VERSION,
                stream_id,
                body: Some(relay_message_frame::Body::Data(RelayData {
                    seq: 1,
                    segment_index: 0,
                    segment_count: 1,
                    payload: jsonrpc_payload(&request)?,
                })),
                ..RelayMessageFrame::default()
            })
            .into(),
        ))
        .await?;
    assert!(matches!(
        timeout(Duration::from_secs(1), connection.incoming_rx.recv()).await?,
        Some(JsonRpcConnectionEvent::Disconnected { reason: Some(reason) })
            if reason == format!(
                "relay JSON-RPC message from test exceeds maximum length of {MAX_CLIENT_INBOUND_REQUEST_LEN} bytes"
            )
    ));

    drop(connection);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn multiplexed_environment_sends_keepalive_and_cancels_blocked_writer() -> anyhow::Result<()>
{
    let (client_websocket, control, mut outbound_rx) =
        ControlledWebSocket::new(/*write_ready*/ true);
    let runtime_paths = crate::ExecServerRuntimeOptions::new(
        std::env::current_exe()?,
        /*codex_linux_sandbox_exe*/ None,
    )
    .map_err(anyhow::Error::from)?;
    let environment_task = tokio::spawn(run_multiplexed_environment(
        client_websocket,
        ConnectionProcessor::new(runtime_paths),
        "test-environment".to_string(),
        "test-registration".to_string(),
        NoiseChannelIdentity::generate()?,
        AllowHarnessKeyValidator,
    ));

    assert!(matches!(outbound_rx.next().await, Some(Message::Ping(_))));
    control.set_write_blocked();
    control.send_inbound(Message::Pong(Vec::new().into()))?;
    control.wait_for_blocked_write().await?;

    environment_task.abort();
    let _ = environment_task.await;
    assert_eq!(
        timeout(Duration::from_millis(1), outbound_rx.next()).await?,
        None
    );
    Ok(())
}

#[derive(Clone)]
struct AllowHarnessKeyValidator;

impl HarnessKeyValidator for AllowHarnessKeyValidator {
    async fn validate_harness_key(
        &self,
        _harness_public_key: &NoiseChannelPublicKey,
        _authorization: &str,
        _stream_id: Option<Uuid>,
    ) -> Result<(), ExecServerError> {
        Ok(())
    }
}

#[tokio::test]
async fn send_event_with_keepalive_pings_while_incoming_queue_is_full() -> anyhow::Result<()> {
    let (mut websocket, _control, mut outbound_rx) =
        ControlledWebSocket::new(/*write_ready*/ true);
    let (incoming_tx, mut incoming_rx) = mpsc::channel(/*buffer*/ 1);
    let message = test_jsonrpc_message();
    let expected_message = message.clone();
    incoming_tx
        .send(JsonRpcConnectionEvent::MalformedMessage {
            reason: "first".to_string(),
        })
        .await?;
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + WEBSOCKET_KEEPALIVE_INTERVAL,
        WEBSOCKET_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let send_task = tokio::spawn(async move {
        send_event_with_keepalive(
            &mut websocket,
            &mut keepalive,
            &incoming_tx,
            JsonRpcConnectionEvent::Message(message),
        )
        .await
    });

    assert!(matches!(
        timeout(Duration::from_secs(1), outbound_rx.next()).await?,
        Some(Message::Ping(_))
    ));
    assert!(matches!(
        incoming_rx.recv().await,
        Some(JsonRpcConnectionEvent::MalformedMessage { reason }) if reason == "first"
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), send_task).await??,
        Ok(())
    ));
    assert!(matches!(
        incoming_rx.recv().await,
        Some(JsonRpcConnectionEvent::Message(actual)) if actual == expected_message
    ));
    Ok(())
}

#[tokio::test]
async fn harness_connection_reports_text_frames_as_malformed() -> anyhow::Result<()> {
    let (client_websocket, mut server_websocket) = websocket_pair().await?;
    let mut connection = harness_connection_from_websocket(client_websocket, "test".to_string());

    read_resume_stream_id(&mut server_websocket).await?;
    server_websocket.send(Message::Text("nope".into())).await?;
    assert!(matches!(
        timeout(Duration::from_secs(1), connection.incoming_rx.recv()).await?,
        Some(JsonRpcConnectionEvent::MalformedMessage { reason })
            if reason == "relay exec-server transport expects binary protobuf frames"
    ));

    drop(connection);
    Ok(())
}

#[tokio::test]
async fn harness_connection_reports_server_close() -> anyhow::Result<()> {
    let (client_websocket, mut server_websocket) = websocket_pair().await?;
    let mut connection = harness_connection_from_websocket(client_websocket, "test".to_string());

    read_resume_stream_id(&mut server_websocket).await?;
    server_websocket.close(None).await?;
    assert!(matches!(
        timeout(Duration::from_secs(1), connection.incoming_rx.recv()).await?,
        Some(JsonRpcConnectionEvent::Disconnected { reason: None })
    ));

    drop(connection);
    Ok(())
}

#[tokio::test]
async fn harness_connection_keeps_outbound_frame_while_send_is_backpressured() -> anyhow::Result<()>
{
    let (websocket, control, mut outbound_rx) = ControlledWebSocket::new(/*write_ready*/ true);
    let mut connection = harness_connection_from_websocket(websocket, "test".to_string());
    let Message::Binary(resume_payload) = timeout(Duration::from_secs(1), outbound_rx.next())
        .await?
        .expect("resume frame")
    else {
        anyhow::bail!("expected relay resume frame");
    };
    let stream_id = decode_relay_message_frame(resume_payload.as_ref())?.stream_id;
    let message = test_jsonrpc_message();

    control.set_write_blocked();
    connection.outgoing_tx.send(message.clone()).await?;
    control.wait_for_blocked_write().await?;
    control.send_inbound(Message::Pong(b"check".to_vec().into()))?;
    assert!(
        timeout(Duration::from_millis(50), connection.incoming_rx.recv())
            .await
            .is_err()
    );

    control.set_write_ready();
    let Message::Binary(data_payload) = timeout(Duration::from_secs(1), outbound_rx.next())
        .await?
        .expect("data frame")
    else {
        anyhow::bail!("expected relay data frame");
    };
    let frame = decode_relay_message_frame(data_payload.as_ref())?;
    assert_eq!(frame.stream_id, stream_id);
    assert_eq!(
        serde_json::from_slice::<JSONRPCMessage>(&frame.into_data()?.payload)?,
        message
    );
    drop(connection);
    Ok(())
}

async fn websocket_pair() -> anyhow::Result<(
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WebSocketStream<tokio::net::TcpStream>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let websocket_url = format!("ws://{}", listener.local_addr()?);
    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        accept_async(stream).await.map_err(anyhow::Error::from)
    });
    let (client_websocket, _) = connect_async(websocket_url).await?;
    let server_websocket = server_task.await??;
    Ok((client_websocket, server_websocket))
}

async fn read_resume_stream_id(
    websocket: &mut WebSocketStream<tokio::net::TcpStream>,
) -> anyhow::Result<String> {
    let message = timeout(Duration::from_secs(1), websocket.next())
        .await?
        .expect("websocket should stay open")?;
    let Message::Binary(payload) = message else {
        anyhow::bail!("expected relay resume frame, got {message:?}");
    };
    let frame = decode_relay_message_frame(payload.as_ref())?;
    assert_eq!(frame.validate()?, RelayFrameBodyKind::Resume);
    Ok(frame.stream_id)
}

async fn read_keepalive_ping(
    websocket: &mut WebSocketStream<tokio::net::TcpStream>,
) -> anyhow::Result<()> {
    loop {
        let Some(message) = timeout(Duration::from_secs(1), websocket.next()).await? else {
            anyhow::bail!("websocket closed before keepalive ping");
        };
        match message? {
            Message::Ping(_) => return Ok(()),
            Message::Binary(_) | Message::Text(_) | Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => anyhow::bail!("websocket closed before keepalive ping"),
        }
    }
}

fn test_jsonrpc_message() -> JSONRPCMessage {
    JSONRPCMessage::Request(JSONRPCRequest {
        id: RequestId::Integer(1),
        method: "test".to_string(),
        params: None,
        trace: None,
    })
}

struct ControlledWebSocket {
    inbound_rx: futures_mpsc::UnboundedReceiver<Result<Message, std::convert::Infallible>>,
    outbound_tx: futures_mpsc::UnboundedSender<Message>,
    write_ready: Arc<AtomicBool>,
    write_blocked: Arc<AtomicBool>,
    write_blocked_waker: Arc<AtomicWaker>,
    write_waker: Arc<AtomicWaker>,
}

struct ControlledWebSocketHandle {
    inbound_tx: futures_mpsc::UnboundedSender<Result<Message, std::convert::Infallible>>,
    write_ready: Arc<AtomicBool>,
    write_blocked: Arc<AtomicBool>,
    write_blocked_waker: Arc<AtomicWaker>,
    write_waker: Arc<AtomicWaker>,
}

impl ControlledWebSocket {
    fn new(
        write_ready: bool,
    ) -> (
        Self,
        ControlledWebSocketHandle,
        futures_mpsc::UnboundedReceiver<Message>,
    ) {
        let (inbound_tx, inbound_rx) = futures_mpsc::unbounded();
        let (outbound_tx, outbound_rx) = futures_mpsc::unbounded();
        let write_ready = Arc::new(AtomicBool::new(write_ready));
        let write_blocked = Arc::new(AtomicBool::new(false));
        let write_blocked_waker = Arc::new(AtomicWaker::new());
        let write_waker = Arc::new(AtomicWaker::new());
        (
            Self {
                inbound_rx,
                outbound_tx,
                write_ready: Arc::clone(&write_ready),
                write_blocked: Arc::clone(&write_blocked),
                write_blocked_waker: Arc::clone(&write_blocked_waker),
                write_waker: Arc::clone(&write_waker),
            },
            ControlledWebSocketHandle {
                inbound_tx,
                write_ready,
                write_blocked,
                write_blocked_waker,
                write_waker,
            },
            outbound_rx,
        )
    }
}

impl ControlledWebSocketHandle {
    fn send_inbound(&self, message: Message) -> anyhow::Result<()> {
        self.inbound_tx
            .unbounded_send(Ok(message))
            .map_err(anyhow::Error::from)
    }

    fn set_write_blocked(&self) {
        self.write_ready.store(false, Ordering::Release);
    }

    fn set_write_ready(&self) {
        self.write_ready.store(true, Ordering::Release);
        self.write_waker.wake();
    }

    async fn wait_for_blocked_write(&self) -> anyhow::Result<()> {
        timeout(
            Duration::from_secs(1),
            futures::future::poll_fn(|cx| {
                if self.write_blocked.load(Ordering::Acquire) {
                    Poll::Ready(())
                } else {
                    self.write_blocked_waker.register(cx.waker());
                    Poll::Pending
                }
            }),
        )
        .await?;
        Ok(())
    }
}

impl Sink<Message> for ControlledWebSocket {
    type Error = std::convert::Infallible;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.write_ready.load(Ordering::Acquire) {
            Poll::Ready(Ok(()))
        } else {
            self.write_blocked.store(true, Ordering::Release);
            self.write_blocked_waker.wake();
            self.write_waker.register(cx.waker());
            Poll::Pending
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        self.outbound_tx
            .unbounded_send(item)
            .expect("test outbound receiver should stay open");
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

impl Stream for ControlledWebSocket {
    type Item = Result<Message, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inbound_rx).poll_next(cx)
    }
}
