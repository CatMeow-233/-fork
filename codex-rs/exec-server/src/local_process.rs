use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::process_telemetry::ProcessTelemetry;
use crate::process_telemetry::ProcessTelemetryEvent;
use codex_exec_server_protocol::JSONRPCErrorError;
use codex_network_proxy::NetworkPolicyAuditEvent;
use codex_network_proxy::NetworkPolicyAuditObserver;
use codex_network_proxy::NetworkProtocol;
use codex_network_proxy::NetworkProxyHandle;
use codex_protocol::config_types::EnvironmentVariablePattern;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_protocol::exec_output::ExecToolCallOutput;
use codex_protocol::exec_output::StreamOutput;
use codex_protocol::shell_environment;
use codex_protocol::shell_environment::CODEX_THREAD_ID_ENV_VAR;
use codex_sandboxing::SandboxType;
use codex_sandboxing::is_likely_sandbox_denied;
use codex_utils_pty::ExecCommandSession;
use codex_utils_pty::ProcessSignal as PtyProcessSignal;
use opentelemetry::trace::SpanContext;
use opentelemetry::trace::TraceContextExt;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;

use crate::ExecBackend;
use crate::ExecBackendFuture;
use crate::ExecProcess;
use crate::ExecProcessEvent;
use crate::ExecProcessEventReceiver;
use crate::ExecProcessFuture;
use crate::ExecServerError;
use crate::ExecServerRuntimeOptions;
use crate::ProcessId;
use crate::StartedExecProcess;
use crate::network_policy_decisions::network_policy_decider;
use crate::process::ExecProcessEventLog;
use crate::process::sandbox_type_from_protocol;
use crate::process_sandbox::prepare_exec_request_with_telemetry;
use crate::protocol::ByteChunk;
use crate::protocol::EXEC_CLOSED_METHOD;
use crate::protocol::ExecClosedNotification;
use crate::protocol::ExecEnvPolicy;
use crate::protocol::ExecExitedNotification;
use crate::protocol::ExecOutputDeltaNotification;
use crate::protocol::ExecOutputStream;
use crate::protocol::ExecParams;
use crate::protocol::ExecResponse;
use crate::protocol::ExecServerNetworkProtocol;
use crate::protocol::MAX_NETWORK_POLICY_PROCESS_ID_BYTES;
use crate::protocol::NETWORK_POLICY_DECISION_METHOD;
use crate::protocol::NetworkPolicyDecisionNotification;
use crate::protocol::ProcessOutputChunk;
use crate::protocol::ProcessSandboxType;
use crate::protocol::ProcessSignal;
use crate::protocol::ReadParams;
use crate::protocol::ReadResponse;
use crate::protocol::SignalParams;
use crate::protocol::SignalResponse;
use crate::protocol::TerminateParams;
use crate::protocol::TerminateResponse;
use crate::protocol::WriteParams;
use crate::protocol::WriteResponse;
use crate::protocol::WriteStatus;
use crate::rpc::RpcNotificationSender;
use crate::rpc::RpcServerOutboundMessage;
use crate::rpc::internal_error;
use crate::rpc::invalid_params;
use crate::rpc::invalid_request;
use crate::rpc_server_requests::RpcServerRequestSender;
#[cfg(unix)]
use crate::shell_snapshot::CapturePurpose;
use crate::telemetry::ExecServerTelemetry;
use crate::telemetry::ProcessMetricGuard;
use crate::telemetry::record_recovery_retention_expired;

const RETAINED_OUTPUT_BYTES_PER_PROCESS: usize = 1024 * 1024;
// Each process/read chunk needs four JSON values. Keep retained replay below the
// shared 256K-value JSON-RPC decoder budget even when output arrives in tiny chunks.
const RETAINED_OUTPUT_CHUNKS_PER_PROCESS: usize = 50_000;
const NOTIFICATION_CHANNEL_CAPACITY: usize = 256;
const PROCESS_EVENT_CHANNEL_CAPACITY: usize = 256;
const RETAINED_STDIN_WRITE_IDS_PER_PROCESS: usize = 4096;
static NEXT_LOCAL_STDIN_WRITE_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
const EXITED_PROCESS_RETENTION: Duration = Duration::from_millis(25);
#[cfg(not(test))]
const EXITED_PROCESS_RETENTION: Duration = Duration::from_secs(30);

struct RunningProcess {
    session: ExecCommandSession,
    tty: bool,
    pipe_stdin: bool,
    accepted_stdin_write_ids: Arc<Mutex<AcceptedStdinWriteIds>>,
    output: VecDeque<ProcessOutputChunk>,
    retained_bytes: usize,
    next_seq: u64,
    exit_code: Option<i32>,
    wake_tx: watch::Sender<u64>,
    events: ExecProcessEventLog,
    output_notify: Arc<Notify>,
    open_streams: usize,
    closed: bool,
    metrics: Option<ProcessMetricGuard>,
    termination_requested: bool,
    sandbox: SandboxType,
    sandbox_denied: bool,
    network_proxy_handle: Option<NetworkProxyHandle>,
    network_policy_shutdown: Option<CancellationToken>,
}

/// Bounded cache of stdin write ids that have already been accepted for one process.
///
/// A remote client can retry `process/write` after reconnecting. Remembering accepted
/// ids lets the server acknowledge the retried request without writing the same bytes
/// to child stdin twice.
#[derive(Default)]
struct AcceptedStdinWriteIds {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl AcceptedStdinWriteIds {
    fn contains(&self, write_id: &str) -> bool {
        self.ids.contains(write_id)
    }

    fn remember(&mut self, write_id: String) {
        if !self.ids.insert(write_id.clone()) {
            return;
        }

        self.order.push_back(write_id);
        while self.order.len() > RETAINED_STDIN_WRITE_IDS_PER_PROCESS {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.ids.remove(&evicted);
        }
    }
}

struct ProcessStart;

enum ProcessEntry {
    Starting(Arc<ProcessStart>),
    Running(Box<RunningProcess>),
}

struct Inner {
    notifications: std::sync::RwLock<Option<RpcNotificationSender>>,
    requests: Arc<std::sync::RwLock<Option<RpcServerRequestSender>>>,
    processes: Mutex<HashMap<ProcessId, ProcessEntry>>,
    rpc_output_retention: Option<Duration>,
    credential_brokers: codex_network_proxy::RemoteCredentialBrokerCache,
    #[cfg(unix)]
    shell_snapshots: crate::shell_snapshot::ShellSnapshotCache,
    telemetry: ExecServerTelemetry,
}

#[derive(Clone)]
pub(crate) struct LocalProcess {
    inner: Arc<Inner>,
    runtime_paths: Option<ExecServerRuntimeOptions>,
}

struct LocalExecProcess {
    process_id: ProcessId,
    backend: LocalProcess,
    wake_tx: watch::Sender<u64>,
    events: ExecProcessEventLog,
}

impl Default for LocalProcess {
    fn default() -> Self {
        Self::with_discarded_notifications(/*runtime_paths*/ None)
    }
}

impl LocalProcess {
    pub(crate) fn with_local_runtime_paths(runtime_paths: ExecServerRuntimeOptions) -> Self {
        Self::with_discarded_notifications(Some(runtime_paths))
    }

    fn with_discarded_notifications(runtime_paths: Option<ExecServerRuntimeOptions>) -> Self {
        let (outgoing_tx, mut outgoing_rx) =
            mpsc::channel::<RpcServerOutboundMessage>(NOTIFICATION_CHANNEL_CAPACITY);
        tokio::spawn(async move { while outgoing_rx.recv().await.is_some() {} });
        Self::with_runtime_paths(
            RpcNotificationSender::new(outgoing_tx),
            ExecServerTelemetry::default(),
            runtime_paths,
            /*rpc_output_retention*/ None,
        )
    }

    pub(crate) fn new(
        notifications: RpcNotificationSender,
        telemetry: ExecServerTelemetry,
        runtime_paths: ExecServerRuntimeOptions,
        output_retention: Duration,
    ) -> Self {
        Self::with_runtime_paths(
            notifications,
            telemetry,
            Some(runtime_paths),
            Some(output_retention),
        )
    }

    fn with_runtime_paths(
        notifications: RpcNotificationSender,
        telemetry: ExecServerTelemetry,
        runtime_paths: Option<ExecServerRuntimeOptions>,
        rpc_output_retention: Option<Duration>,
    ) -> Self {
        let requests = notifications.request_sender();
        Self {
            inner: Arc::new(Inner {
                notifications: std::sync::RwLock::new(Some(notifications)),
                requests: Arc::new(std::sync::RwLock::new(Some(requests))),
                processes: Mutex::new(HashMap::new()),
                rpc_output_retention,
                credential_brokers: codex_network_proxy::RemoteCredentialBrokerCache::default(),
                #[cfg(unix)]
                shell_snapshots: crate::shell_snapshot::ShellSnapshotCache::default(),
                telemetry,
            }),
            runtime_paths,
        }
    }

    pub(crate) async fn shutdown(&self) {
        let remaining = {
            let mut processes = self.inner.processes.lock().await;
            processes
                .drain()
                .filter_map(|(_, process)| match process {
                    ProcessEntry::Starting(_) => None,
                    ProcessEntry::Running(process) => Some(process),
                })
                .collect::<Vec<_>>()
        };
        for mut process in remaining {
            if let Some(network_policy_shutdown) = process.network_policy_shutdown.take() {
                network_policy_shutdown.cancel();
            }
            if let Some(metrics) = process.metrics.take() {
                metrics.finish("terminated");
            }
            process.session.terminate();
        }
    }

    pub(crate) fn set_notification_sender(&self, notifications: Option<RpcNotificationSender>) {
        let requests = notifications
            .as_ref()
            .map(RpcNotificationSender::request_sender);
        let mut notification_sender = self
            .inner
            .notifications
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *notification_sender = notifications;
        let previous_requests = std::mem::replace(
            &mut *self
                .inner
                .requests
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            requests,
        );
        if let Some(previous_requests) = previous_requests {
            previous_requests.close();
        }
    }

    async fn start_process(
        &self,
        params: ExecParams,
        mut telemetry: ProcessTelemetry,
    ) -> Result<(ExecResponse, watch::Sender<u64>, ExecProcessEventLog), JSONRPCErrorError> {
        telemetry.launch_context = telemetry.launch_context.filter(SpanContext::is_valid);
        let metadata = params.metadata.as_ref();
        telemetry.thread_id = metadata
            .and_then(|metadata| metadata.thread_id.as_ref())
            .map(ToString::to_string);
        // Correlation is controller-supplied, not authorization or arbitrary diagnostic text.
        telemetry.tool_call_id = metadata
            .and_then(|metadata| metadata.tool_call_id.as_ref())
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 256
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte))
            })
            .cloned();
        let process_id = params.process_id.clone();
        let policy_decision_timeout_ms = params
            .network_proxy
            .as_ref()
            .and_then(|launch| launch.policy_decision_timeout_ms);
        if policy_decision_timeout_ms == Some(0) {
            return Err(invalid_params(
                "network policy decision callback timeout must be nonzero".to_string(),
            ));
        }
        if policy_decision_timeout_ms.is_some()
            && (process_id.is_empty() || process_id.len() > MAX_NETWORK_POLICY_PROCESS_ID_BYTES)
        {
            return Err(invalid_params(format!(
                "callback-enabled process ID must be non-empty and at most {MAX_NETWORK_POLICY_PROCESS_ID_BYTES} bytes"
            )));
        }
        let policy_decision_timeout = policy_decision_timeout_ms.map(Duration::from_millis);
        let network_policy_shutdown = policy_decision_timeout.map(|_| CancellationToken::new());
        let network_policy_decider = network_policy_shutdown
            .as_ref()
            .zip(policy_decision_timeout)
            .map(|(process_shutdown, controller_timeout)| {
                network_policy_decider(
                    process_id.clone(),
                    Arc::clone(&self.inner.requests),
                    controller_timeout,
                    process_shutdown.clone(),
                )
            });
        let network_policy_audit_observer = params.network_proxy.as_ref().map(|_| {
            let process_id = process_id.clone();
            let inner = Arc::downgrade(&self.inner);
            Arc::new(move |event: NetworkPolicyAuditEvent| {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let Some(notifications) = notification_sender(&inner) else {
                    return;
                };
                let notification = NetworkPolicyDecisionNotification {
                    process_id: process_id.clone(),
                    timestamp: event.timestamp,
                    scope: event.scope,
                    decision: event.decision,
                    source: event.source,
                    reason: event.reason,
                    protocol: match event.protocol {
                        NetworkProtocol::Http => ExecServerNetworkProtocol::Http,
                        NetworkProtocol::HttpsConnect => ExecServerNetworkProtocol::HttpsConnect,
                        NetworkProtocol::Socks5Tcp => ExecServerNetworkProtocol::Socks5Tcp,
                        NetworkProtocol::Socks5Udp => ExecServerNetworkProtocol::Socks5Udp,
                    },
                    host: event.host,
                    port: event.port,
                    method: event.method,
                    client: event.client,
                    policy_override: event.policy_override,
                };
                let _ = notifications.try_notify(NETWORK_POLICY_DECISION_METHOD, &notification);
            }) as NetworkPolicyAuditObserver
        });
        #[cfg(not(unix))]
        if params.shell_snapshot.is_some() {
            return Err(invalid_params(
                "shell snapshots are unsupported on this platform".to_string(),
            ));
        }
        let prepared = prepare_exec_request_with_telemetry(
            &params,
            child_env(&params),
            self.runtime_paths.as_ref(),
            network_policy_decider,
            network_policy_audit_observer,
            &self.inner.credential_brokers,
            &telemetry,
        )
        .await?;
        #[cfg(unix)]
        let mut prepared = prepared;
        #[cfg(unix)]
        let snapshot_file = self
            .inner
            .shell_snapshots
            .prepare(
                &params,
                &mut prepared,
                &self.inner.telemetry,
                CapturePurpose::Execution,
                &self.inner.credential_brokers,
            )
            .await?;
        if prepared.command.is_empty() {
            return Err(invalid_params("argv must not be empty".to_string()));
        }
        let sandbox_type = match prepared.sandbox {
            SandboxType::None => Some(ProcessSandboxType::None),
            SandboxType::MacosSeatbelt => Some(ProcessSandboxType::MacosSeatbelt),
            SandboxType::LinuxSeccomp => Some(ProcessSandboxType::LinuxSeccomp),
            SandboxType::WindowsRestrictedToken => Some(ProcessSandboxType::WindowsRestrictedToken),
            SandboxType::WindowsMxc => Some(ProcessSandboxType::WindowsMxc),
        };

        let start = Arc::new(ProcessStart);
        {
            let mut process_map = self.inner.processes.lock().await;
            if process_map.contains_key(&process_id) {
                return Err(invalid_request(format!(
                    "process {process_id} already exists"
                )));
            }
            process_map.insert(
                process_id.clone(),
                ProcessEntry::Starting(Arc::clone(&start)),
            );
        }

        #[cfg(unix)]
        let inherited_fds = snapshot_file
            .iter()
            .map(std::os::fd::AsRawFd::as_raw_fd)
            .collect::<Vec<_>>();
        #[cfg(not(unix))]
        let inherited_fds = Vec::new();
        let spawned_result = codex_sandboxing::spawn_process(codex_sandboxing::SpawnRequest {
            command: &prepared.command,
            cwd: prepared.cwd.as_path(),
            env: &prepared.env,
            arg0: &prepared.arg0,
            sandbox: prepared.sandbox,
            windows_sandbox: prepared.windows_sandbox_spawn_request(),
            tty: params.tty,
            stdin_open: params.tty || params.pipe_stdin,
            inherited_fds: codex_utils_pty::ChildFds::Attached(&inherited_fds),
        })
        .await;
        #[cfg(unix)]
        drop(snapshot_file);
        let spawned = match spawned_result {
            Ok(spawned) => spawned,
            Err(err) => {
                telemetry.log(ProcessTelemetryEvent::SpawnFailed, prepared.sandbox);
                let mut process_map = self.inner.processes.lock().await;
                if matches!(
                    process_map.get(&process_id),
                    Some(ProcessEntry::Starting(current)) if Arc::ptr_eq(current, &start)
                ) {
                    process_map.remove(&process_id);
                }
                return Err(internal_error(err.to_string()));
            }
        };
        let metrics = self.inner.telemetry.process_started(&process_id);

        let output_notify = Arc::new(Notify::new());
        let (wake_tx, _wake_rx) = watch::channel(0);
        let events = ExecProcessEventLog::new(
            PROCESS_EVENT_CHANNEL_CAPACITY,
            RETAINED_OUTPUT_BYTES_PER_PROCESS,
        );
        {
            let mut process_map = self.inner.processes.lock().await;
            if !matches!(
                process_map.get(&process_id),
                Some(ProcessEntry::Starting(current)) if Arc::ptr_eq(current, &start)
            ) {
                drop(process_map);
                spawned.session.terminate();
                metrics.finish("terminated");
                return Err(invalid_request(format!(
                    "process {process_id} start was cancelled"
                )));
            }
            process_map.insert(
                process_id.clone(),
                ProcessEntry::Running(Box::new(RunningProcess {
                    session: spawned.session,
                    tty: params.tty,
                    pipe_stdin: params.pipe_stdin,
                    accepted_stdin_write_ids: Arc::new(
                        Mutex::new(AcceptedStdinWriteIds::default()),
                    ),
                    output: VecDeque::new(),
                    retained_bytes: 0,
                    next_seq: 1,
                    exit_code: None,
                    wake_tx: wake_tx.clone(),
                    events: events.clone(),
                    output_notify: Arc::clone(&output_notify),
                    open_streams: 2,
                    closed: false,
                    metrics: Some(metrics),
                    termination_requested: false,
                    sandbox: prepared.sandbox,
                    sandbox_denied: false,
                    network_proxy_handle: prepared.network_proxy_handle,
                    network_policy_shutdown,
                })),
            );
        }
        telemetry.log(ProcessTelemetryEvent::Start, prepared.sandbox);
        tokio::spawn(stream_output(
            process_id.clone(),
            if params.tty {
                ExecOutputStream::Pty
            } else {
                ExecOutputStream::Stdout
            },
            spawned.stdout_rx,
            Arc::clone(&self.inner),
            Arc::clone(&output_notify),
        ));
        tokio::spawn(stream_output(
            process_id.clone(),
            if params.tty {
                ExecOutputStream::Pty
            } else {
                ExecOutputStream::Stderr
            },
            spawned.stderr_rx,
            Arc::clone(&self.inner),
            Arc::clone(&output_notify),
        ));
        // Keep the subscriber, but let the request span close independently of process completion.
        tokio::spawn(
            watch_exit(
                process_id.clone(),
                spawned.exit_rx,
                Arc::clone(&self.inner),
                output_notify,
                telemetry,
            )
            .with_current_subscriber(),
        );

        Ok((
            ExecResponse {
                process_id,
                sandbox_type,
            },
            wake_tx,
            events,
        ))
    }

    pub(crate) async fn exec(
        &self,
        params: ExecParams,
        telemetry: ProcessTelemetry,
    ) -> Result<ExecResponse, JSONRPCErrorError> {
        self.start_process(params, telemetry)
            .await
            .map(|(response, _, _)| response)
    }

    pub(crate) async fn exec_read(
        &self,
        params: ReadParams,
    ) -> Result<ReadResponse, JSONRPCErrorError> {
        let after_seq = params.after_seq.unwrap_or(0);
        let max_bytes = params.max_bytes.unwrap_or(usize::MAX);
        let wait = Duration::from_millis(params.wait_ms.unwrap_or(0));
        let deadline = tokio::time::Instant::now() + wait;

        loop {
            let (response, output_notify) = {
                let process_map = self.inner.processes.lock().await;
                let process = process_map.get(&params.process_id).ok_or_else(|| {
                    invalid_request(format!("unknown process id {}", params.process_id))
                })?;
                let ProcessEntry::Running(process) = process else {
                    return Err(invalid_request(format!(
                        "process id {} is starting",
                        params.process_id
                    )));
                };

                let mut chunks = Vec::new();
                let mut total_bytes = 0;
                let mut next_seq = process.next_seq;
                for retained in process.output.iter().filter(|chunk| chunk.seq > after_seq) {
                    let chunk_len = retained.chunk.0.len();
                    if !chunks.is_empty() && total_bytes + chunk_len > max_bytes {
                        break;
                    }
                    total_bytes += chunk_len;
                    chunks.push(retained.clone());
                    next_seq = retained.seq + 1;
                    if total_bytes >= max_bytes {
                        break;
                    }
                }
                if params.max_bytes.is_none() {
                    next_seq = process.next_seq;
                }
                (
                    ReadResponse {
                        chunks,
                        next_seq,
                        exited: process.exit_code.is_some(),
                        exit_code: process.exit_code,
                        closed: process.closed,
                        failure: None,
                        sandbox_denied: process.sandbox_denied,
                    },
                    Arc::clone(&process.output_notify),
                )
            };

            let has_new_terminal_event =
                response.exited && after_seq < response.next_seq.saturating_sub(1);
            if !response.chunks.is_empty()
                || response.closed
                || has_new_terminal_event
                || tokio::time::Instant::now() >= deadline
            {
                let _total_bytes: usize = response
                    .chunks
                    .iter()
                    .map(|chunk| chunk.chunk.0.len())
                    .sum();
                return Ok(response);
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(response);
            }
            let _ = tokio::time::timeout(remaining, output_notify.notified()).await;
        }
    }

    pub(crate) async fn exec_write(
        &self,
        params: WriteParams,
    ) -> Result<WriteResponse, JSONRPCErrorError> {
        let _input_bytes = params.chunk.0.len();
        if params.write_id.is_empty() {
            return Err(invalid_params("writeId must not be empty".to_string()));
        }

        let (writer_tx, accepted_stdin_write_ids) = {
            let process_map = self.inner.processes.lock().await;
            let Some(process) = process_map.get(&params.process_id) else {
                return Ok(WriteResponse {
                    status: WriteStatus::UnknownProcess,
                });
            };
            let ProcessEntry::Running(process) = process else {
                return Ok(WriteResponse {
                    status: WriteStatus::Starting,
                });
            };
            if !process.tty && !process.pipe_stdin {
                return Ok(WriteResponse {
                    status: WriteStatus::StdinClosed,
                });
            }
            (
                process.session.writer_sender(),
                Arc::clone(&process.accepted_stdin_write_ids),
            )
        };

        if accepted_stdin_write_ids
            .lock()
            .await
            .contains(&params.write_id)
        {
            return Ok(WriteResponse {
                status: WriteStatus::Accepted,
            });
        }

        let permit = writer_tx
            .reserve()
            .await
            .map_err(|_| internal_error("failed to write to process stdin".to_string()))?;
        let mut accepted_stdin_write_ids = accepted_stdin_write_ids.lock().await;
        if accepted_stdin_write_ids.contains(&params.write_id) {
            return Ok(WriteResponse {
                status: WriteStatus::Accepted,
            });
        }

        // After this synchronous send, record the write id before any further await.
        // Otherwise a cancelled RPC handler could retry and write the same bytes again.
        permit.send(params.chunk.into_inner());
        accepted_stdin_write_ids.remember(params.write_id);

        Ok(WriteResponse {
            status: WriteStatus::Accepted,
        })
    }

    pub(crate) async fn signal_process(
        &self,
        params: SignalParams,
    ) -> Result<SignalResponse, JSONRPCErrorError> {
        {
            let process_map = self.inner.processes.lock().await;
            match process_map.get(&params.process_id) {
                Some(ProcessEntry::Running(process)) => {
                    if process.exit_code.is_some() {
                        return Ok(SignalResponse {});
                    }
                    process
                        .session
                        .signal(pty_process_signal(params.signal))
                        .map_err(|err| internal_error(format!("failed to signal process: {err}")))?
                }
                Some(ProcessEntry::Starting(_)) | None => {}
            }
        }

        Ok(SignalResponse {})
    }

    pub(crate) async fn terminate_process(
        &self,
        params: TerminateParams,
    ) -> Result<TerminateResponse, JSONRPCErrorError> {
        let running = {
            let mut process_map = self.inner.processes.lock().await;
            match process_map.get_mut(&params.process_id) {
                Some(ProcessEntry::Running(process)) => {
                    if let Some(network_policy_shutdown) = &process.network_policy_shutdown {
                        network_policy_shutdown.cancel();
                    }
                    if process.exit_code.is_some() {
                        return Ok(TerminateResponse { running: false });
                    }
                    process.termination_requested = true;
                    process.session.terminate();
                    true
                }
                Some(ProcessEntry::Starting(_)) => {
                    process_map.remove(&params.process_id);
                    true
                }
                None => false,
            }
        };

        Ok(TerminateResponse { running })
    }
}

fn child_env(params: &ExecParams) -> HashMap<String, String> {
    let mut env = match &params.env_policy {
        Some(env_policy) => {
            let policy = shell_environment_policy(env_policy);
            let mut env = shell_environment::create_env(&policy, /*thread_id*/ None);
            env.extend(params.env.clone());
            env
        }
        None => params.env.clone(),
    };
    env.remove(crate::CODEX_EXEC_SERVER_EXIT_ON_STDIN_CLOSE_ENV_VAR);
    env.retain(|name, _| !shell_environment::is_non_inheritable_env_var(name));
    apply_exec_metadata(&mut env, params.metadata.as_ref());
    env
}

pub(crate) fn apply_exec_metadata(
    env: &mut HashMap<String, String>,
    metadata: Option<&crate::protocol::ExecMetadata>,
) {
    if let Some(metadata) = metadata {
        #[cfg(windows)]
        env.retain(|name, _| !name.eq_ignore_ascii_case(CODEX_THREAD_ID_ENV_VAR));
        match metadata.thread_id {
            Some(thread_id) => {
                env.insert(CODEX_THREAD_ID_ENV_VAR.to_string(), thread_id.to_string());
            }
            None => {
                env.remove(CODEX_THREAD_ID_ENV_VAR);
            }
        }
    }
    shell_environment::set_tool_call_id_env_var(
        env,
        metadata.and_then(|metadata| metadata.tool_call_id.as_deref()),
    );
}

pub(crate) fn shell_environment_policy(env_policy: &ExecEnvPolicy) -> ShellEnvironmentPolicy {
    ShellEnvironmentPolicy {
        inherit: env_policy.inherit.clone(),
        ignore_default_excludes: env_policy.ignore_default_excludes,
        exclude: env_policy
            .exclude
            .iter()
            .map(|pattern| EnvironmentVariablePattern::new_case_insensitive(pattern))
            .collect(),
        r#set: env_policy.r#set.clone(),
        include_only: env_policy
            .include_only
            .iter()
            .map(|pattern| EnvironmentVariablePattern::new_case_insensitive(pattern))
            .collect(),
        use_profile: false,
    }
}

impl LocalProcess {
    async fn start(&self, params: ExecParams) -> Result<StartedExecProcess, ExecServerError> {
        let (response, wake_tx, events) = self
            .start_process(params, ProcessTelemetry::default())
            .await
            .map_err(map_handler_error)?;
        let sandbox_type = sandbox_type_from_protocol(response.sandbox_type);
        Ok(StartedExecProcess {
            process: Arc::new(LocalExecProcess {
                process_id: response.process_id,
                backend: self.clone(),
                wake_tx,
                events,
            }),
            sandbox_type,
        })
    }
}

impl ExecBackend for LocalProcess {
    fn start(&self, params: ExecParams) -> ExecBackendFuture<'_> {
        Box::pin(LocalProcess::start(self, params))
    }

    #[cfg(unix)]
    fn prewarm_shell_snapshot(&self, params: ExecParams) -> ExecProcessFuture<'_, ()> {
        Box::pin(async move {
            if params.enforce_managed_network
                || params.managed_network.is_some()
                || params.network_proxy.is_some()
            {
                return Err(ExecServerError::Protocol(
                    "shell snapshot prewarming does not support managed networking".to_string(),
                ));
            }
            let mut prepared = prepare_exec_request_with_telemetry(
                &params,
                child_env(&params),
                self.runtime_paths.as_ref(),
                /*network_policy_decider*/ None,
                /*network_policy_audit_observer*/ None,
                &self.inner.credential_brokers,
                &ProcessTelemetry::default(),
            )
            .await
            .map_err(map_handler_error)?;
            self.inner
                .shell_snapshots
                .prepare(
                    &params,
                    &mut prepared,
                    &self.inner.telemetry,
                    CapturePurpose::Prewarm,
                    &self.inner.credential_brokers,
                )
                .await
                .map(|_| ())
                .map_err(map_handler_error)
        })
    }
}

impl LocalExecProcess {
    async fn read(
        &self,
        after_seq: Option<u64>,
        max_bytes: Option<usize>,
        wait_ms: Option<u64>,
    ) -> Result<ReadResponse, ExecServerError> {
        self.backend
            .read(&self.process_id, after_seq, max_bytes, wait_ms)
            .await
    }

    async fn write(&self, chunk: Vec<u8>) -> Result<WriteResponse, ExecServerError> {
        self.backend.write(&self.process_id, chunk).await
    }

    async fn signal(&self, signal: ProcessSignal) -> Result<(), ExecServerError> {
        self.backend.signal(&self.process_id, signal).await
    }

    async fn terminate(&self) -> Result<(), ExecServerError> {
        self.backend.terminate(&self.process_id).await
    }
}

impl ExecProcess for LocalExecProcess {
    fn process_id(&self) -> &ProcessId {
        &self.process_id
    }

    fn subscribe_wake(&self) -> watch::Receiver<u64> {
        self.wake_tx.subscribe()
    }

    fn subscribe_events(&self) -> ExecProcessEventReceiver {
        self.events.subscribe()
    }

    fn read(
        &self,
        after_seq: Option<u64>,
        max_bytes: Option<usize>,
        wait_ms: Option<u64>,
    ) -> ExecProcessFuture<'_, ReadResponse> {
        Box::pin(LocalExecProcess::read(self, after_seq, max_bytes, wait_ms))
    }

    fn write(&self, chunk: Vec<u8>) -> ExecProcessFuture<'_, WriteResponse> {
        Box::pin(LocalExecProcess::write(self, chunk))
    }

    fn signal(&self, signal: ProcessSignal) -> ExecProcessFuture<'_, ()> {
        Box::pin(LocalExecProcess::signal(self, signal))
    }

    fn terminate(&self) -> ExecProcessFuture<'_, ()> {
        Box::pin(LocalExecProcess::terminate(self))
    }
}

impl LocalProcess {
    async fn read(
        &self,
        process_id: &ProcessId,
        after_seq: Option<u64>,
        max_bytes: Option<usize>,
        wait_ms: Option<u64>,
    ) -> Result<ReadResponse, ExecServerError> {
        self.exec_read(ReadParams {
            process_id: process_id.clone(),
            after_seq,
            max_bytes,
            wait_ms,
        })
        .await
        .map_err(map_handler_error)
    }

    async fn write(
        &self,
        process_id: &ProcessId,
        chunk: Vec<u8>,
    ) -> Result<WriteResponse, ExecServerError> {
        self.exec_write(WriteParams {
            process_id: process_id.clone(),
            chunk: chunk.into(),
            write_id: format!(
                "local-{}",
                NEXT_LOCAL_STDIN_WRITE_ID.fetch_add(1, Ordering::Relaxed)
            ),
        })
        .await
        .map_err(map_handler_error)
    }

    async fn signal(
        &self,
        process_id: &ProcessId,
        signal: ProcessSignal,
    ) -> Result<(), ExecServerError> {
        self.signal_process(SignalParams {
            process_id: process_id.clone(),
            signal,
        })
        .await
        .map_err(map_handler_error)?;
        Ok(())
    }

    async fn terminate(&self, process_id: &ProcessId) -> Result<(), ExecServerError> {
        self.terminate_process(TerminateParams {
            process_id: process_id.clone(),
        })
        .await
        .map_err(map_handler_error)?;
        Ok(())
    }
}

fn pty_process_signal(signal: ProcessSignal) -> PtyProcessSignal {
    match signal {
        ProcessSignal::Interrupt => PtyProcessSignal::Interrupt,
    }
}

fn map_handler_error(error: JSONRPCErrorError) -> ExecServerError {
    ExecServerError::Server {
        code: error.code,
        message: error.message,
    }
}

async fn stream_output(
    process_id: ProcessId,
    stream: ExecOutputStream,
    mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    inner: Arc<Inner>,
    output_notify: Arc<Notify>,
) {
    while let Some(chunk) = receiver.recv().await {
        let _chunk_len = chunk.len();
        let chunk = ByteChunk::from(chunk);
        let notification = {
            let mut processes = inner.processes.lock().await;
            let Some(entry) = processes.get_mut(&process_id) else {
                break;
            };
            let ProcessEntry::Running(process) = entry else {
                break;
            };
            let seq = process.next_seq;
            process.next_seq += 1;
            process.retained_bytes += chunk.0.len();
            let output = ProcessOutputChunk { seq, stream, chunk };
            process.output.push_back(output.clone());
            while process.retained_bytes > RETAINED_OUTPUT_BYTES_PER_PROCESS
                || process.output.len() > RETAINED_OUTPUT_CHUNKS_PER_PROCESS
            {
                let Some(evicted) = process.output.pop_front() else {
                    break;
                };
                process.retained_bytes =
                    process.retained_bytes.saturating_sub(evicted.chunk.0.len());
            }
            let _ = process.wake_tx.send(seq);
            process
                .events
                .publish(ExecProcessEvent::Output(output.clone()));
            ExecOutputDeltaNotification {
                process_id: process_id.clone(),
                seq,
                stream,
                chunk: output.chunk,
            }
        };
        output_notify.notify_waiters();
        if let Some(notifications) = notification_sender(&inner) {
            let _ = notifications
                .notify(crate::protocol::EXEC_OUTPUT_DELTA_METHOD, &notification)
                .await;
        }
    }

    finish_output_stream(process_id, inner).await;
}

fn watch_exit(
    process_id: ProcessId,
    exit_rx: tokio::sync::oneshot::Receiver<i32>,
    inner: Arc<Inner>,
    output_notify: Arc<Notify>,
    telemetry: ProcessTelemetry,
) -> impl std::future::Future<Output = ()> + Send {
    // Set the copied OTEL parent before entering; never retain the RPC tracing span.
    let process_span = tracing::info_span!(parent: None, "codex.exec_server.process");
    if let Some(launch_context) = &telemetry.launch_context {
        codex_otel::set_parent_from_context(
            &process_span,
            opentelemetry::Context::new().with_remote_span_context(launch_context.clone()),
        );
    }
    async move {
        let exit_code = exit_rx.await.unwrap_or(-1);
        let sandboxed = {
            let mut processes = inner.processes.lock().await;
            match processes.get_mut(&process_id) {
                Some(ProcessEntry::Running(process)) => {
                    let sandboxed = process.sandbox != SandboxType::None;
                    if let Some(metrics) = process.metrics.take() {
                        metrics.finish(if process.termination_requested {
                            "terminated"
                        } else if exit_code == 0 {
                            "success"
                        } else {
                            "error"
                        });
                    }
                    sandboxed
                }
                Some(ProcessEntry::Starting(_)) | None => false,
            }
        };
        if sandboxed {
            let _ = tokio::time::timeout(Duration::from_millis(20), output_notify.notified()).await;
        }
        let notification = {
            let mut processes = inner.processes.lock().await;
            if let Some(ProcessEntry::Running(process)) = processes.get_mut(&process_id) {
                let seq = process.next_seq;
                process.next_seq += 1;
                process.exit_code = Some(exit_code);
                if process.sandbox != SandboxType::None {
                    let mut stdout = Vec::new();
                    let mut stderr = Vec::new();
                    let mut aggregated = Vec::new();
                    for chunk in &process.output {
                        match chunk.stream {
                            ExecOutputStream::Stdout | ExecOutputStream::Pty => {
                                stdout.extend_from_slice(&chunk.chunk.0);
                            }
                            ExecOutputStream::Stderr => stderr.extend_from_slice(&chunk.chunk.0),
                        }
                        aggregated.extend_from_slice(&chunk.chunk.0);
                    }
                    let exec_output = ExecToolCallOutput {
                        exit_code,
                        stdout: StreamOutput::new(String::from_utf8_lossy(&stdout).into_owned()),
                        stderr: StreamOutput::new(String::from_utf8_lossy(&stderr).into_owned()),
                        aggregated_output: StreamOutput::new(
                            String::from_utf8_lossy(&aggregated).into_owned(),
                        ),
                        ..Default::default()
                    };
                    // Keep the classification in the result for caller approval/retry handling.
                    process.sandbox_denied =
                        is_likely_sandbox_denied(process.sandbox, &exec_output);
                    if process.sandbox_denied {
                        telemetry.log(ProcessTelemetryEvent::SandboxDenied, process.sandbox);
                    }
                }
                telemetry.log(
                    ProcessTelemetryEvent::Exit {
                        exit_code,
                        termination_requested: process.termination_requested,
                    },
                    process.sandbox,
                );
                let _ = process.wake_tx.send(seq);
                process.events.publish(ExecProcessEvent::Exited {
                    seq,
                    exit_code,
                    sandbox_denied: Some(process.sandbox_denied),
                });
                Some(ExecExitedNotification {
                    process_id: process_id.clone(),
                    seq,
                    exit_code,
                    sandbox_denied: Some(process.sandbox_denied),
                })
            } else {
                None
            }
        };
        output_notify.notify_waiters();
        if let Some(notification) = notification
            && let Some(notifications) = notification_sender(&inner)
        {
            let _ = notifications
                .notify(crate::protocol::EXEC_EXITED_METHOD, &notification)
                .await;
        }

        maybe_emit_closed(process_id, Arc::clone(&inner)).await;
    }
    .instrument(process_span)
}

async fn finish_output_stream(process_id: ProcessId, inner: Arc<Inner>) {
    {
        let mut processes = inner.processes.lock().await;
        let Some(ProcessEntry::Running(process)) = processes.get_mut(&process_id) else {
            return;
        };

        if process.open_streams > 0 {
            process.open_streams -= 1;
        }
    }

    maybe_emit_closed(process_id, inner).await;
}

async fn maybe_emit_closed(process_id: ProcessId, inner: Arc<Inner>) {
    let (notification, output_notify, network_proxy_handle) = {
        let mut processes = inner.processes.lock().await;
        let Some(ProcessEntry::Running(process)) = processes.get_mut(&process_id) else {
            return;
        };

        if process.closed || process.open_streams != 0 || process.exit_code.is_none() {
            return;
        }

        process.closed = true;
        if let Some(network_policy_shutdown) = process.network_policy_shutdown.take() {
            network_policy_shutdown.cancel();
        }
        let seq = process.next_seq;
        process.next_seq += 1;
        let _ = process.wake_tx.send(seq);
        process.events.publish(ExecProcessEvent::Closed { seq });
        (
            ExecClosedNotification {
                process_id: process_id.clone(),
                seq,
            },
            Arc::clone(&process.output_notify),
            process.network_proxy_handle.take(),
        )
    };

    if let Some(network_proxy_handle) = network_proxy_handle
        && let Err(err) = network_proxy_handle.shutdown().await
    {
        tracing::warn!("failed to shut down executor network proxy: {err}");
    }

    output_notify.notify_waiters();
    let cleanup_process_id = process_id.clone();
    let cleanup_inner = Arc::downgrade(&inner);
    let retention = inner
        .rpc_output_retention
        .unwrap_or(EXITED_PROCESS_RETENTION);
    tokio::spawn(async move {
        tokio::time::sleep(retention).await;
        let Some(cleanup_inner) = cleanup_inner.upgrade() else {
            return;
        };
        {
            let mut processes = cleanup_inner.processes.lock().await;
            let Entry::Occupied(entry) = processes.entry(cleanup_process_id) else {
                return;
            };
            if !matches!(entry.get(), ProcessEntry::Running(process)
                if process.closed && Arc::ptr_eq(&process.output_notify, &output_notify))
            {
                return;
            }
            entry.remove();
        }
        if cleanup_inner.rpc_output_retention.is_some()
            && notification_sender(&cleanup_inner).is_none()
        {
            record_recovery_retention_expired("closed_output_timer", retention);
        }
    });

    if let Some(notifications) = notification_sender(&inner) {
        let _ = notifications
            .notify(EXEC_CLOSED_METHOD, &notification)
            .await;
    }
}

fn notification_sender(inner: &Inner) -> Option<RpcNotificationSender> {
    inner
        .notifications
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
#[path = "local_process_tests.rs"]
mod tests;
