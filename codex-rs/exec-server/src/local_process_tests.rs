use super::*;
use codex_exec_server_protocol::JSONRPCMessage;
use codex_exec_server_protocol::JSONRPCResponse;
use codex_exec_server_protocol::RequestId;
use codex_network_proxy::NetworkProxy;
use codex_network_proxy::NetworkProxyConfig;
use codex_network_proxy::NetworkProxyState;
use codex_network_proxy::RemoteNetworkProxyConfig;
use codex_network_proxy::RemoteNetworkProxyLaunchConfig;
use codex_otel::MetricsConfig;
use codex_protocol::config_types::ShellEnvironmentPolicyInherit;
use codex_utils_path_uri::PathUri;
use codex_utils_pty::ProcessDriver;
use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::MetricData;
use pretty_assertions::assert_eq;
#[cfg(not(target_os = "windows"))]
use tokio::io::AsyncReadExt;
#[cfg(not(target_os = "windows"))]
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[cfg(not(target_os = "windows"))]
use crate::protocol::ExecServerNetworkPolicyDecision;
#[cfg(not(target_os = "windows"))]
use crate::protocol::NETWORK_POLICY_REQUEST_METHOD;
#[cfg(not(target_os = "windows"))]
use crate::protocol::NetworkPolicyRequestParams;
#[cfg(not(target_os = "windows"))]
use crate::protocol::NetworkPolicyRequestResponse;

#[cfg(target_os = "linux")]
#[ctor::ctor]
fn initialize_spawn_helper() {
    use std::os::unix::ffi::OsStringExt;
    let command_line = std::fs::read("/proc/self/cmdline").expect("test command line");
    codex_utils_pty::init_spawn_helper(
        command_line
            .strip_suffix(&[0])
            .unwrap_or(&command_line)
            .split(|byte| *byte == 0)
            .map(|arg| std::ffi::OsString::from_vec(arg.to_vec())),
    );
}

fn test_exec_params(env: HashMap<String, String>) -> ExecParams {
    ExecParams {
        metadata: None,
        process_id: ProcessId::from("env-test"),
        argv: vec!["true".to_string()],
        cwd: PathUri::from_host_native_path(std::env::current_dir().expect("cwd"))
            .expect("cwd URI"),
        shell_snapshot: None,
        env_policy: None,
        env,
        tty: false,
        pipe_stdin: false,
        arg0: None,
        sandbox: None,
        enforce_managed_network: false,
        managed_network: None,
        network_proxy: None,
    }
}

#[cfg(unix)]
#[tokio::test]
async fn executor_proxy_sends_final_network_policy_notification() {
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(NOTIFICATION_CHANNEL_CAPACITY);
    let backend = LocalProcess::with_runtime_paths(
        RpcNotificationSender::new(outgoing_tx),
        ExecServerTelemetry::default(),
        /*runtime_paths*/ None,
        /*rpc_output_retention*/ None,
    );
    let proxy_config = RemoteNetworkProxyConfig::from_effective_config(&NetworkProxyConfig {
        enabled: true,
        ..NetworkProxyConfig::default()
    })
    .expect("build remote network proxy config");
    let mut params = test_exec_params(HashMap::new());
    params.process_id = ProcessId::from("audit-process");
    params.argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "printf '%s\\n' \"$HTTP_PROXY\"; exec sleep 60".to_string(),
    ];
    params.network_proxy = Some(
        RemoteNetworkProxyLaunchConfig::new(proxy_config)
            .for_execution("environment-1".to_string(), "execution-1".to_string()),
    );
    backend
        .exec(params, ProcessTelemetry::default())
        .await
        .expect("start process with proxy");
    let output = backend
        .exec_read(ReadParams {
            process_id: ProcessId::from("audit-process"),
            after_seq: None,
            max_bytes: None,
            wait_ms: Some(1_000),
        })
        .await
        .expect("read executor proxy address");
    let proxy_addr = String::from_utf8(
        output
            .chunks
            .into_iter()
            .find(|chunk| matches!(chunk.stream, ExecOutputStream::Stdout))
            .expect("executor proxy address output")
            .chunk
            .into_inner(),
    )
    .expect("UTF-8 proxy address");
    let proxy_addr = proxy_addr
        .trim()
        .strip_prefix("http://")
        .expect("HTTP executor proxy address");
    let mut stream = tokio::net::TcpStream::connect(proxy_addr)
        .await
        .expect("connect to executor proxy");
    stream
        .write_all(b"CONNECT 8.8.8.8:443 HTTP/1.1\r\nHost: 8.8.8.8:443\r\n\r\n")
        .await
        .expect("write CONNECT request");
    let mut response = [0_u8; 256];
    let response_len = timeout(Duration::from_secs(2), stream.read(&mut response))
        .await
        .expect("proxy response timeout")
        .expect("read proxy response");
    assert!(String::from_utf8_lossy(&response[..response_len]).starts_with("HTTP/1.1 403"));

    let notification = timeout(Duration::from_secs(2), async {
        loop {
            match outgoing_rx.recv().await {
                Some(RpcServerOutboundMessage::Notification(notification))
                    if notification.method == NETWORK_POLICY_DECISION_METHOD =>
                {
                    break serde_json::from_value::<NetworkPolicyDecisionNotification>(
                        notification
                            .params
                            .expect("network policy notification params"),
                    )
                    .expect("deserialize network policy notification");
                }
                Some(_) => {}
                None => panic!("outbound notifications closed"),
            }
        }
    })
    .await
    .expect("network policy notification timeout");
    assert_eq!(notification.process_id, ProcessId::from("audit-process"));
    assert_eq!(notification.decision, "deny");
    assert_eq!(notification.host, "8.8.8.8");
    assert_eq!(
        notification.protocol,
        ExecServerNetworkProtocol::HttpsConnect
    );
    backend.shutdown().await;
}

fn telemetry_backend() -> (
    LocalProcess,
    codex_otel::MetricsClient,
    InMemoryMetricExporter,
) {
    let exporter = InMemoryMetricExporter::default();
    let metrics = codex_otel::MetricsClient::new(MetricsConfig::in_memory(
        "test",
        "codex-exec-server",
        env!("CARGO_PKG_VERSION"),
        exporter.clone(),
    ))
    .expect("metrics");
    let telemetry = ExecServerTelemetry::new(metrics.clone());
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(NOTIFICATION_CHANNEL_CAPACITY);
    tokio::spawn(async move { while outgoing_rx.recv().await.is_some() {} });
    (
        LocalProcess::with_runtime_paths(
            RpcNotificationSender::new(outgoing_tx),
            telemetry,
            /*runtime_paths*/ None,
            /*rpc_output_retention*/ None,
        ),
        metrics,
        exporter,
    )
}

fn assert_finished_process_result(
    metrics: codex_otel::MetricsClient,
    exporter: &InMemoryMetricExporter,
    expected: &str,
) {
    metrics.shutdown().expect("shutdown metrics");
    let resource_metrics = exporter
        .get_finished_metrics()
        .expect("finished metrics")
        .into_iter()
        .last()
        .expect("metrics export");
    let finished_processes = resource_metrics
        .scope_metrics()
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .find(|metric| metric.name() == "exec_server_processes_finished_total")
        .expect("finished process metric");
    let AggregatedMetrics::U64(MetricData::Sum(sum)) = finished_processes.data() else {
        panic!("finished process metric should be a u64 sum");
    };
    let results = sum
        .data_points()
        .flat_map(opentelemetry_sdk::metrics::data::SumDataPoint::attributes)
        .filter(|attribute| attribute.key.as_str() == "result")
        .map(|attribute| attribute.value.as_str().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(results, vec![expected.to_string()]);
}

#[tokio::test]
async fn start_process_rejects_non_native_cwd_before_launch() {
    #[cfg(unix)]
    let uri = "file://server/share/checkout";
    #[cfg(windows)]
    let uri = "file:///usr/local/checkout";
    let cwd = PathUri::parse(uri).expect("non-native cwd URI");
    let source = cwd
        .to_abs_path()
        .expect_err("cwd should not be native to this host");
    let expected = invalid_params(format!(
        "cwd URI `{cwd}` is not valid on this exec-server host: {source}"
    ));
    let mut params = test_exec_params(HashMap::new());
    params.cwd = cwd;

    let result = LocalProcess::default()
        .start_process(params, ProcessTelemetry::default())
        .await;
    let Err(error) = result else {
        panic!("non-native cwd should be rejected");
    };

    assert_eq!(error, expected);
}

#[tokio::test]
async fn callback_enabled_start_bounds_process_id_before_proxy_launch() {
    let proxy_config =
        RemoteNetworkProxyConfig::from_effective_config(&NetworkProxyConfig::default())
            .expect("remote proxy config");
    let mut proxy = RemoteNetworkProxyLaunchConfig::new(proxy_config);
    proxy.policy_decision_timeout_ms = Some(1_000);
    let expected = invalid_params(format!(
        "callback-enabled process ID must be non-empty and at most {MAX_NETWORK_POLICY_PROCESS_ID_BYTES} bytes"
    ));

    for process_id in [
        String::new(),
        "p".repeat(MAX_NETWORK_POLICY_PROCESS_ID_BYTES + 1),
    ] {
        let mut params = test_exec_params(HashMap::new());
        params.process_id = ProcessId::from(process_id);
        params.network_proxy = Some(proxy.clone());
        let error = LocalProcess::default()
            .start_process(params, ProcessTelemetry::default())
            .await
            .err()
            .expect("invalid callback process ID should be rejected");

        assert_eq!(error, expected);
    }

    let mut boundary = test_exec_params(HashMap::new());
    boundary.process_id = ProcessId::from("p".repeat(MAX_NETWORK_POLICY_PROCESS_ID_BYTES));
    boundary.network_proxy = Some(proxy);
    boundary.argv.clear();
    let error = LocalProcess::default()
        .start_process(boundary, ProcessTelemetry::default())
        .await
        .err()
        .expect("valid boundary process ID should proceed to process preparation");
    #[cfg(not(target_os = "windows"))]
    assert!(
        error
            .message
            .contains("executor-local network proxy launch requires an enabled proxy")
    );
    #[cfg(target_os = "windows")]
    assert_eq!(error, invalid_params("argv must not be empty".to_string()));

    for process_id in [
        String::new(),
        "p".repeat(MAX_NETWORK_POLICY_PROCESS_ID_BYTES + 1),
    ] {
        let mut ordinary = test_exec_params(HashMap::new());
        ordinary.process_id = ProcessId::from(process_id);
        ordinary.argv.clear();
        let error = LocalProcess::default()
            .start_process(ordinary, ProcessTelemetry::default())
            .await
            .err()
            .expect("empty argv should be rejected after ID validation");

        assert_eq!(error, invalid_params("argv must not be empty".to_string()));
    }
}

#[test]
fn child_env_defaults_to_exact_env() {
    let params = test_exec_params(HashMap::from([("ONLY_THIS".to_string(), "1".to_string())]));

    assert_eq!(
        child_env(&params),
        HashMap::from([("ONLY_THIS".to_string(), "1".to_string())])
    );
}

#[test]
fn child_env_applies_policy_then_overlay() {
    let thread_id = codex_protocol::ThreadId::from_string("019d25f0-6728-7ce2-908f-c3c187323e5b")
        .expect("thread id");
    let mut params = test_exec_params(HashMap::from([
        ("OVERLAY".to_string(), "overlay".to_string()),
        ("POLICY_SET".to_string(), "overlay-wins".to_string()),
        ("CODEX_THREAD_ID".into(), "overlay-thread".into()),
        ("CODEX_TOOL_CALL_ID".into(), "overlay-call".into()),
        (
            "openai_identity_token_file".to_string(),
            "/run/identity-token".to_string(),
        ),
    ]));
    params.env_policy = Some(ExecEnvPolicy {
        inherit: ShellEnvironmentPolicyInherit::None,
        ignore_default_excludes: true,
        exclude: Vec::new(),
        r#set: HashMap::from([
            ("POLICY_SET".to_string(), "policy".to_string()),
            ("OpenAI_Federation_Rule_Id".to_string(), "rule".to_string()),
            ("CODEX_THREAD_ID".into(), "policy-thread".into()),
            ("CODEX_TOOL_CALL_ID".into(), "policy-call".into()),
        ]),
        include_only: Vec::new(),
    });
    params.metadata = Some(crate::protocol::ExecMetadata {
        thread_id: Some(thread_id),
        tool_call_id: Some("current-call".into()),
    });

    let mut expected = HashMap::from([
        ("OVERLAY".to_string(), "overlay".to_string()),
        ("POLICY_SET".to_string(), "overlay-wins".to_string()),
        ("CODEX_THREAD_ID".into(), thread_id.to_string()),
        ("CODEX_TOOL_CALL_ID".into(), "current-call".into()),
    ]);
    if cfg!(target_os = "windows") {
        expected.insert("PATHEXT".to_string(), ".COM;.EXE;.BAT;.CMD".to_string());
    }

    assert_eq!(child_env(&params), expected);
}

#[test]
fn child_env_partial_metadata_removes_stale_counterpart() {
    let thread_id = codex_protocol::ThreadId::from_string("019d25f0-6728-7ce2-908f-c3c187323e5b")
        .expect("thread id");
    for (thread_id, call_id) in [
        (Some(thread_id), None),
        (None, Some("exec-call-only")),
        (None, None),
    ] {
        let mut params = test_exec_params(HashMap::from([
            ("CODEX_THREAD_ID".into(), "stale-thread".into()),
            ("Codex_Thread_Id".into(), "mixed-case-thread".into()),
            ("codex_thread_id".into(), "lowercase-thread".into()),
            ("CODEX_TOOL_CALL_ID".into(), "stale-call".into()),
            ("OTHER".into(), "unchanged".into()),
        ]));
        params.metadata = Some(crate::protocol::ExecMetadata {
            thread_id,
            tool_call_id: call_id.map(str::to_string),
        });

        let mut expected = HashMap::from([("OTHER".into(), "unchanged".into())]);
        #[cfg(not(windows))]
        expected.insert("Codex_Thread_Id".into(), "mixed-case-thread".into());
        #[cfg(not(windows))]
        expected.insert("codex_thread_id".into(), "lowercase-thread".into());
        if let Some(thread_id) = thread_id {
            expected.insert("CODEX_THREAD_ID".into(), thread_id.to_string());
        }
        if let Some(call_id) = call_id {
            expected.insert("CODEX_TOOL_CALL_ID".into(), call_id.to_string());
        }
        assert_eq!(
            child_env(&params),
            expected,
            "metadata: {:?}",
            params.metadata
        );
    }
}

#[test]
fn child_env_without_metadata_preserves_thread_only_context() {
    let params = test_exec_params(HashMap::from([
        ("CODEX_THREAD_ID".into(), "prewarm-thread".into()),
        ("Codex_Thread_Id".into(), "mixed-case-thread".into()),
        ("CODEX_TOOL_CALL_ID".into(), "stale-call".into()),
        ("Codex_Tool_Call_Id".into(), "mixed-case-call".into()),
    ]));

    assert_eq!(
        child_env(&params),
        HashMap::from([
            ("CODEX_THREAD_ID".into(), "prewarm-thread".into()),
            ("Codex_Thread_Id".into(), "mixed-case-thread".into()),
            #[cfg(not(windows))]
            ("Codex_Tool_Call_Id".into(), "mixed-case-call".into()),
        ]),
    );
}

#[tokio::test]
async fn exit_before_shutdown_records_success() {
    let (backend, metrics, exporter) = telemetry_backend();
    let mut process = spawn_test_process(&backend, "exit-before-shutdown").await;

    process.exit(/*exit_code*/ 0);
    let _ = read_process_until_change(&backend, &process.process_id, /*after_seq*/ None).await;
    backend.shutdown().await;

    assert_finished_process_result(metrics, &exporter, "success");
}

#[tokio::test]
async fn termination_request_before_exit_records_terminated() {
    let (backend, metrics, exporter) = telemetry_backend();
    let mut process = spawn_test_process(&backend, "terminate-before-exit").await;

    assert_eq!(
        backend
            .terminate_process(TerminateParams {
                process_id: process.process_id.clone(),
            })
            .await
            .expect("terminate process"),
        TerminateResponse { running: true },
    );
    process.exit(/*exit_code*/ 0);
    let _ = read_process_until_change(&backend, &process.process_id, /*after_seq*/ None).await;
    backend.shutdown().await;

    assert_finished_process_result(metrics, &exporter, "terminated");
}

#[tokio::test]
async fn termination_request_after_exit_cancels_network_policy_decisions() {
    let backend = LocalProcess::default();
    let mut process = spawn_test_process(&backend, "terminate-after-exit").await;
    let network_policy_shutdown = CancellationToken::new();
    {
        let mut processes = backend.inner.processes.lock().await;
        let Some(ProcessEntry::Running(running)) = processes.get_mut(&process.process_id) else {
            panic!("test process should be running");
        };
        running.network_policy_shutdown = Some(network_policy_shutdown.clone());
    }

    process.exit(/*exit_code*/ 0);
    let response =
        read_process_until_change(&backend, &process.process_id, /*after_seq*/ None).await;
    assert!(response.exited);
    assert!(!response.closed);
    assert!(!network_policy_shutdown.is_cancelled());
    assert_eq!(
        backend
            .terminate_process(TerminateParams {
                process_id: process.process_id.clone(),
            })
            .await
            .expect("terminate exited process"),
        TerminateResponse { running: false },
    );
    assert!(network_policy_shutdown.is_cancelled());

    drop(process.stdout_tx);
    drop(process.stderr_tx);
    let _ = read_process_until_closed(&backend, &process.process_id).await;
    backend.shutdown().await;
}

#[tokio::test]
async fn shutdown_before_exit_records_terminated() {
    let (backend, metrics, exporter) = telemetry_backend();
    let mut process = spawn_test_process(&backend, "shutdown-before-exit").await;

    backend.shutdown().await;
    process.exit(/*exit_code*/ 0);

    assert_finished_process_result(metrics, &exporter, "terminated");
}

#[tokio::test]
async fn exited_process_retains_late_output_past_retention() {
    let backend = LocalProcess::default();
    let mut process = spawn_test_process(&backend, "proc-late-output").await;

    process.exit(/*exit_code*/ 0);
    let exit_response =
        read_process_until_change(&backend, &process.process_id, /*after_seq*/ None).await;
    assert_eq!(
        exit_response,
        ReadResponse {
            chunks: Vec::new(),
            next_seq: 2,
            exited: true,
            exit_code: Some(0),
            closed: false,
            failure: None,
            sandbox_denied: false,
        }
    );

    tokio::time::sleep(EXITED_PROCESS_RETENTION + Duration::from_millis(10)).await;
    process
        .stdout_tx
        .send(b"late output after retention\n".to_vec())
        .await
        .expect("send late stdout");

    let late_response =
        read_process_until_change(&backend, &process.process_id, /*after_seq*/ Some(1)).await;
    assert_eq!(
        late_response.chunks,
        vec![ProcessOutputChunk {
            seq: 2,
            stream: ExecOutputStream::Stdout,
            chunk: b"late output after retention\n".to_vec().into(),
        }]
    );
    assert_eq!(late_response.exit_code, Some(0));
    assert!(!late_response.closed);

    drop(process.stdout_tx);
    drop(process.stderr_tx);
    let _closed_response = timeout(
        Duration::from_secs(1),
        read_process_until_closed(&backend, &process.process_id),
    )
    .await
    .expect("process should close");
    let replay_after_exit = backend
        .exec_read(ReadParams {
            process_id: process.process_id.clone(),
            after_seq: Some(1),
            max_bytes: None,
            wait_ms: Some(0),
        })
        .await
        .expect("closed process should remain readable");
    assert_eq!(replay_after_exit.next_seq, 4);
    backend.shutdown().await;
}

#[tokio::test]
async fn process_read_replay_is_bounded_by_chunk_count() {
    let backend = LocalProcess::default();
    let process = spawn_test_process(&backend, "proc-chunk-count").await;
    let retained_chunk_count = RETAINED_OUTPUT_CHUNKS_PER_PROCESS as u64;

    {
        let mut processes = backend.inner.processes.lock().await;
        let Some(ProcessEntry::Running(running)) = processes.get_mut(&process.process_id) else {
            panic!("process should be running");
        };
        running.output = (1..=retained_chunk_count)
            .map(|seq| ProcessOutputChunk {
                seq,
                stream: ExecOutputStream::Stdout,
                chunk: vec![b'x'].into(),
            })
            .collect();
        running.retained_bytes = RETAINED_OUTPUT_CHUNKS_PER_PROCESS;
        running.next_seq = retained_chunk_count + 1;
    }

    process
        .stdout_tx
        .send(vec![b'y'])
        .await
        .expect("send output beyond retained chunk limit");
    timeout(Duration::from_secs(1), async {
        loop {
            let output_recorded = {
                let processes = backend.inner.processes.lock().await;
                let Some(ProcessEntry::Running(running)) = processes.get(&process.process_id)
                else {
                    panic!("process should be running");
                };
                running.next_seq == retained_chunk_count + 2
            };
            if output_recorded {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("output should be retained");

    let response = backend
        .exec_read(ReadParams {
            process_id: process.process_id.clone(),
            after_seq: None,
            max_bytes: None,
            wait_ms: Some(0),
        })
        .await
        .expect("read retained output");
    let mut expected_chunks = (2..=retained_chunk_count)
        .map(|seq| ProcessOutputChunk {
            seq,
            stream: ExecOutputStream::Stdout,
            chunk: vec![b'x'].into(),
        })
        .collect::<Vec<_>>();
    expected_chunks.push(ProcessOutputChunk {
        seq: retained_chunk_count + 1,
        stream: ExecOutputStream::Stdout,
        chunk: vec![b'y'].into(),
    });
    assert_eq!(
        response,
        ReadResponse {
            chunks: expected_chunks,
            next_seq: retained_chunk_count + 2,
            exited: false,
            exit_code: None,
            closed: false,
            failure: None,
            sandbox_denied: false,
        }
    );

    let message = JSONRPCMessage::Response(JSONRPCResponse {
        id: RequestId::Integer(1),
        result: serde_json::to_value(response).expect("serialize process/read response"),
    });
    let encoded = serde_json::to_string(&message).expect("encode JSON-RPC response");
    let decoded = serde_json::from_str::<JSONRPCMessage>(&encoded)
        .expect("retained process/read response should fit the JSON value budget");
    assert_eq!(decoded, message);

    backend.shutdown().await;
}

#[tokio::test]
async fn exited_process_keeps_network_proxy_until_inherited_streams_close() {
    let backend = LocalProcess::default();
    let mut process = spawn_test_process(&backend, "proc-background-child").await;
    let (outgoing_tx, outgoing_rx) = mpsc::channel(NOTIFICATION_CHANNEL_CAPACITY);
    #[cfg(not(target_os = "windows"))]
    let mut outgoing_rx = outgoing_rx;
    #[cfg(target_os = "windows")]
    let _outgoing_rx = outgoing_rx;
    let requests = RpcNotificationSender::new(outgoing_tx).request_sender();
    *backend
        .inner
        .requests
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(requests.clone());
    let network_policy_shutdown = CancellationToken::new();
    let decider = network_policy_decider(
        process.process_id.clone(),
        Arc::clone(&backend.inner.requests),
        Duration::from_secs(30),
        network_policy_shutdown.clone(),
    );
    let config = NetworkProxyConfig {
        enabled: true,
        ..Default::default()
    };
    let proxy_config = RemoteNetworkProxyConfig::from_effective_config(&config)
        .expect("build remote network proxy config");
    let state = NetworkProxyState::from_remote_launch_config(
        RemoteNetworkProxyLaunchConfig::new(proxy_config),
        codex_utils_path_uri::Platform::native(),
        &codex_network_proxy::RemoteCredentialBrokerCache::default(),
        &HashMap::new(),
    )
    .expect("build network proxy state");
    let proxy = NetworkProxy::builder()
        .state(Arc::new(state))
        .policy_decider_arc(decider)
        .build()
        .await
        .expect("build network proxy");
    let handle = proxy.run().await.expect("start network proxy");
    let prepared = proxy
        .prepare_for_optional_environment(HashMap::new(), /*environment_id*/ None)
        .expect("prepare network proxy environment");
    let proxy_addr: std::net::SocketAddr = prepared
        .env
        .get("HTTP_PROXY")
        .and_then(|value| value.strip_prefix("http://"))
        .expect("HTTP proxy address")
        .parse()
        .expect("parse HTTP proxy address");

    {
        let mut processes = backend.inner.processes.lock().await;
        let Some(ProcessEntry::Running(running)) = processes.get_mut(&process.process_id) else {
            panic!("test process should be running");
        };
        running.network_proxy_handle = Some(handle);
        running.network_policy_shutdown = Some(network_policy_shutdown.clone());
    }

    process.exit(/*exit_code*/ 0);
    let exit_response =
        read_process_until_change(&backend, &process.process_id, /*after_seq*/ None).await;
    assert!(exit_response.exited);
    assert!(!exit_response.closed);
    assert!(!network_policy_shutdown.is_cancelled());
    let stream = tokio::net::TcpStream::connect(proxy_addr)
        .await
        .expect("proxy should remain available to a child holding inherited output streams");
    #[cfg(target_os = "windows")]
    {
        assert!(proxy.network_proxy_restricting_sid(None).is_some());
        drop(stream);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut stream = stream;
        stream
            .write_all(b"CONNECT 8.8.8.8:443 HTTP/1.1\r\nHost: 8.8.8.8:443\r\n\r\n")
            .await
            .expect("write CONNECT request");
        let outbound = timeout(Duration::from_secs(1), outgoing_rx.recv())
            .await
            .expect("policy request should arrive")
            .expect("policy request");
        let RpcServerOutboundMessage::Request(request) = outbound else {
            panic!("expected policy request");
        };
        assert_eq!(request.method, NETWORK_POLICY_REQUEST_METHOD);
        let params: NetworkPolicyRequestParams =
            serde_json::from_value(request.params.expect("request params"))
                .expect("deserialize policy request");
        assert_eq!(params.process_id, process.process_id);
        assert_eq!(params.request.host, "8.8.8.8");
        requests.complete(
            request.id,
            Ok(serde_json::to_value(NetworkPolicyRequestResponse {
                decision: ExecServerNetworkPolicyDecision::Deny {
                    reason: "not_allowed".to_string(),
                },
            })
            .expect("serialize policy response")),
        );
        let mut response = [0_u8; 256];
        let response_len = timeout(Duration::from_secs(1), stream.read(&mut response))
            .await
            .expect("proxy response timeout")
            .expect("read proxy response");
        assert!(String::from_utf8_lossy(&response[..response_len]).starts_with("HTTP/1.1 403"));
    }

    drop(process.stdout_tx);
    drop(process.stderr_tx);
    let closed_response = timeout(
        Duration::from_secs(1),
        read_process_until_closed(&backend, &process.process_id),
    )
    .await
    .expect("process should close");
    assert!(closed_response.closed);
    assert!(network_policy_shutdown.is_cancelled());
    #[cfg(target_os = "windows")]
    assert_eq!(proxy.network_proxy_restricting_sid(None), None);
    #[cfg(not(target_os = "windows"))]
    assert!(tokio::net::TcpStream::connect(proxy_addr).await.is_err());
    backend.shutdown().await;
}

#[tokio::test]
async fn closed_process_is_evicted_after_retention() {
    let backend = LocalProcess::default();
    let mut process = spawn_test_process(&backend, "proc-closed-eviction").await;
    let process_id = process.process_id.clone();

    process.exit(/*exit_code*/ 0);
    drop(process.stdout_tx);
    drop(process.stderr_tx);

    let closed_response = timeout(
        Duration::from_secs(1),
        read_process_until_closed(&backend, &process_id),
    )
    .await
    .expect("process should close");
    assert!(closed_response.closed);

    timeout(Duration::from_secs(1), async {
        loop {
            {
                let processes = backend.inner.processes.lock().await;
                if !processes.contains_key(&process_id) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("closed process should be evicted");
    backend.shutdown().await;
}

struct TestProcess {
    process_id: ProcessId,
    stdout_tx: mpsc::Sender<Vec<u8>>,
    stderr_tx: mpsc::Sender<Vec<u8>>,
    exit_tx: Option<oneshot::Sender<i32>>,
}

impl TestProcess {
    fn exit(&mut self, exit_code: i32) {
        self.exit_tx
            .take()
            .expect("process should not have exited")
            .send(exit_code)
            .expect("send process exit");
    }
}

async fn spawn_test_process(backend: &LocalProcess, process_id: &str) -> TestProcess {
    let process_id = ProcessId::from(process_id);
    let (stdout_tx, stdout_rx) = mpsc::channel(16);
    let (stderr_tx, stderr_rx) = mpsc::channel(16);
    let (exit_tx, exit_rx) = oneshot::channel();
    let output_notify = Arc::new(Notify::new());
    let (wake_tx, _wake_rx) = watch::channel(0);
    let events = ExecProcessEventLog::new(
        PROCESS_EVENT_CHANNEL_CAPACITY,
        RETAINED_OUTPUT_BYTES_PER_PROCESS,
    );

    let mut processes = backend.inner.processes.lock().await;
    let previous = processes.insert(
        process_id.clone(),
        ProcessEntry::Running(Box::new(RunningProcess {
            session: dummy_session(),
            tty: false,
            pipe_stdin: false,
            accepted_stdin_write_ids: Arc::new(Mutex::new(AcceptedStdinWriteIds::default())),
            output: VecDeque::new(),
            retained_bytes: 0,
            next_seq: 1,
            exit_code: None,
            wake_tx: wake_tx.clone(),
            events: events.clone(),
            output_notify: Arc::clone(&output_notify),
            open_streams: 2,
            closed: false,
            metrics: Some(backend.inner.telemetry.process_started(&process_id)),
            termination_requested: false,
            sandbox: SandboxType::None,
            sandbox_denied: false,
            network_proxy_handle: None,
            network_policy_shutdown: None,
        })),
    );
    assert!(previous.is_none());
    drop(processes);

    tokio::spawn(stream_output(
        process_id.clone(),
        ExecOutputStream::Stdout,
        stdout_rx,
        Arc::clone(&backend.inner),
        Arc::clone(&output_notify),
    ));
    tokio::spawn(stream_output(
        process_id.clone(),
        ExecOutputStream::Stderr,
        stderr_rx,
        Arc::clone(&backend.inner),
        Arc::clone(&output_notify),
    ));
    tokio::spawn(watch_exit(
        process_id.clone(),
        exit_rx,
        Arc::clone(&backend.inner),
        output_notify,
        ProcessTelemetry::default(),
    ));

    TestProcess {
        process_id,
        stdout_tx,
        stderr_tx,
        exit_tx: Some(exit_tx),
    }
}

fn dummy_session() -> ExecCommandSession {
    let (writer_tx, _writer_rx) = mpsc::channel(1);
    let (_stdout_tx, stdout_rx) = tokio::sync::broadcast::channel(1);
    let (_stderr_tx, stderr_rx) = tokio::sync::broadcast::channel(1);
    let (_exit_tx, exit_rx) = oneshot::channel();

    codex_utils_pty::spawn_from_driver(ProcessDriver {
        writer_tx,
        stdout_rx,
        stderr_rx: Some(stderr_rx),
        exit_rx,
        terminator: None,
        writer_handle: None,
        resizer: None,
        #[cfg(windows)]
        tty: false,
    })
    .session
}

async fn read_process_until_change(
    backend: &LocalProcess,
    process_id: &ProcessId,
    after_seq: Option<u64>,
) -> ReadResponse {
    timeout(
        Duration::from_secs(1),
        backend.exec_read(ReadParams {
            process_id: process_id.clone(),
            after_seq,
            max_bytes: None,
            wait_ms: Some(1_000),
        }),
    )
    .await
    .expect("process read should finish")
    .expect("process read")
}

async fn read_process_until_closed(backend: &LocalProcess, process_id: &ProcessId) -> ReadResponse {
    let mut after_seq = None;
    loop {
        let response = read_process_until_change(backend, process_id, after_seq).await;
        if response.closed {
            return response;
        }
        for chunk in &response.chunks {
            after_seq = Some(chunk.seq);
        }
        after_seq = response.next_seq.checked_sub(1).or(after_seq);
    }
}
