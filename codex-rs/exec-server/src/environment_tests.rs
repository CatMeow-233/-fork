use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::Environment;
use super::EnvironmentManager;
use super::EnvironmentObservedStatus;
use super::LOCAL_ENVIRONMENT_ID;
use super::REMOTE_ENVIRONMENT_ID;
use super::noise_environment_config_from_values;
use crate::ExecServerRuntimeOptions;
use crate::ProcessId;
use crate::client_api::DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT;
use crate::client_api::ExecServerTransportParams;
use crate::client_api::StdioExecServerCommand;
use crate::environment_provider::EnvironmentDefault;
use crate::environment_provider::EnvironmentProviderEntry;
use crate::environment_provider::EnvironmentProviderSnapshot;
use codex_config::ScopedSkillsConfig;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tokio::net::TcpListener;
use tokio::time::timeout;

fn legacy_http_client_factory() -> HttpClientFactory {
    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
}

fn prepared_websocket_environment() -> ExecServerTransportParams {
    ExecServerTransportParams::websocket_url(
        "ws://127.0.0.1:8765".to_string(),
        DEFAULT_REMOTE_EXEC_SERVER_CONNECT_TIMEOUT,
    )
}

fn test_runtime_paths() -> ExecServerRuntimeOptions {
    ExecServerRuntimeOptions::new(
        std::env::current_exe().expect("current exe"),
        /*codex_linux_sandbox_exe*/ None,
    )
    .expect("runtime paths")
}

fn assert_local_environment_unavailable(manager: &EnvironmentManager) {
    assert!(manager.try_local_environment().is_none());
}

#[test]
fn local_environment_info_includes_current_directory() {
    let info = super::EnvironmentInfo::local();

    assert_eq!(
        info.cwd,
        Some(
            PathUri::from_host_native_path(std::env::current_dir().expect("current directory"))
                .expect("cwd URI")
        )
    );
    assert_eq!(
        info.temp_dir,
        PathUri::from_host_native_path(std::env::temp_dir()).ok()
    );
}

#[tokio::test]
async fn noise_environment_config_selects_remote_as_default() {
    let config = noise_environment_config_from_values(
        Some("http://registry.example/api".to_string()),
        Some("environment-requested".to_string()),
        Some("registry-token".to_string()),
        Some("workspace-123".to_string()),
    )
    .expect("parse noise environment configuration")
    .expect("noise environment configuration");

    let manager = EnvironmentManager::from_noise_environment_config(
        config,
        /*local_runtime_paths*/ None,
        HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
    )
    .expect("build environment manager");

    assert_eq!(
        manager.http_client_factory().outbound_proxy_policy(),
        OutboundProxyPolicy::RespectSystemProxy
    );
    assert_eq!(
        manager.default_environment_id(),
        Some(REMOTE_ENVIRONMENT_ID)
    );
    assert!(
        manager
            .default_environment()
            .expect("remote environment")
            .is_remote()
    );
    assert_local_environment_unavailable(&manager);
}

#[tokio::test]
async fn create_local_environment_does_not_connect() {
    let environment = Environment::create(
        /*exec_server_url*/ None,
        test_runtime_paths(),
        legacy_http_client_factory(),
    )
    .expect("create environment");

    assert!(!environment.is_remote());
    assert!(environment.info().await.is_ok());
}

#[tokio::test]
async fn environment_manager_normalizes_empty_url() {
    let manager =
        EnvironmentManager::create_for_tests(Some(String::new()), Some(test_runtime_paths())).await;

    let environment = manager.default_environment().expect("default environment");
    assert_eq!(manager.default_environment_id(), Some(LOCAL_ENVIRONMENT_ID));
    assert!(Arc::ptr_eq(
        &environment,
        &manager
            .get_environment(LOCAL_ENVIRONMENT_ID)
            .expect("local environment")
    ));
    assert!(Arc::ptr_eq(
        &environment,
        &manager.try_local_environment().expect("local environment")
    ));
    assert!(manager.try_local_environment().is_some());
    assert!(manager.get_environment(REMOTE_ENVIRONMENT_ID).is_none());
    assert!(!environment.is_remote());
}

#[tokio::test]
async fn disabled_environment_manager_has_no_default_or_local_environment() {
    let manager = EnvironmentManager::without_environments(HttpClientFactory::new(
        OutboundProxyPolicy::RespectSystemProxy,
    ));

    assert!(manager.default_environment().is_none());
    assert_eq!(manager.default_environment_id(), None);
    assert_local_environment_unavailable(&manager);
    assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
    assert!(manager.get_environment(REMOTE_ENVIRONMENT_ID).is_none());
    assert_eq!(
        manager.http_client_factory().outbound_proxy_policy(),
        OutboundProxyPolicy::RespectSystemProxy
    );
}

#[tokio::test]
async fn environment_manager_creates_remote_environment_for_url() {
    let manager = EnvironmentManager::create_for_tests(
        Some("ws://127.0.0.1:8765".to_string()),
        Some(test_runtime_paths()),
    )
    .await;

    let environment = manager.default_environment().expect("default environment");
    assert_eq!(
        manager.default_environment_id(),
        Some(REMOTE_ENVIRONMENT_ID)
    );
    assert!(environment.is_remote());
    assert!(Arc::ptr_eq(
        &environment,
        &manager
            .get_environment(REMOTE_ENVIRONMENT_ID)
            .expect("remote environment")
    ));
    assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
    assert_local_environment_unavailable(&manager);
}

#[tokio::test]
async fn environment_manager_default_environment_caches_environment() {
    let manager = EnvironmentManager::default_for_tests();

    let first = manager.default_environment().expect("default environment");
    let second = manager.default_environment().expect("default environment");

    assert!(Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(
        &first.get_filesystem(),
        &second.get_filesystem()
    ));
}

#[tokio::test]
async fn environment_manager_builds_from_snapshot() {
    let snapshot = EnvironmentProviderSnapshot {
        environments: vec![EnvironmentProviderEntry {
            id: REMOTE_ENVIRONMENT_ID.to_string(),
            transport: prepared_websocket_environment(),
            skills: Default::default(),
        }],
        default: EnvironmentDefault::EnvironmentId(REMOTE_ENVIRONMENT_ID.to_string()),
        include_local: false,
    };
    let manager = EnvironmentManager::from_snapshot(
        snapshot,
        Some(test_runtime_paths()),
        legacy_http_client_factory(),
    )
    .expect("environment manager");

    assert_eq!(
        manager.default_environment_id(),
        Some(REMOTE_ENVIRONMENT_ID)
    );
    assert!(
        manager
            .get_environment(REMOTE_ENVIRONMENT_ID)
            .expect("remote environment")
            .is_remote()
    );
    assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
    assert_local_environment_unavailable(&manager);
}

#[tokio::test]
async fn environment_manager_rejects_empty_environment_id() {
    let snapshot = EnvironmentProviderSnapshot {
        environments: vec![EnvironmentProviderEntry {
            id: "".to_string(),
            transport: prepared_websocket_environment(),
            skills: Default::default(),
        }],
        default: EnvironmentDefault::Disabled,
        include_local: false,
    };
    let err = EnvironmentManager::from_snapshot(
        snapshot,
        Some(test_runtime_paths()),
        legacy_http_client_factory(),
    )
    .expect_err("empty id should fail");

    assert_eq!(
        err.to_string(),
        "exec-server protocol error: environment id cannot be empty"
    );
}

#[tokio::test]
async fn environment_manager_rejects_provider_supplied_local_environment() {
    let snapshot = EnvironmentProviderSnapshot {
        environments: vec![EnvironmentProviderEntry {
            id: LOCAL_ENVIRONMENT_ID.to_string(),
            transport: prepared_websocket_environment(),
            skills: Default::default(),
        }],
        default: EnvironmentDefault::Disabled,
        include_local: false,
    };
    let err = EnvironmentManager::from_snapshot(
        snapshot,
        Some(test_runtime_paths()),
        legacy_http_client_factory(),
    )
    .expect_err("local id should fail");

    assert_eq!(
        err.to_string(),
        "exec-server protocol error: environment id `local` is reserved for EnvironmentManager"
    );
}

#[tokio::test]
async fn environment_manager_uses_explicit_provider_default() {
    let snapshot = EnvironmentProviderSnapshot {
        environments: vec![EnvironmentProviderEntry {
            id: "devbox".to_string(),
            transport: prepared_websocket_environment(),
            skills: Default::default(),
        }],
        default: EnvironmentDefault::EnvironmentId("devbox".to_string()),
        include_local: true,
    };
    let manager = EnvironmentManager::from_snapshot(
        snapshot,
        Some(test_runtime_paths()),
        legacy_http_client_factory(),
    )
    .expect("manager");

    assert_eq!(manager.default_environment_id(), Some("devbox"));
    assert_eq!(
        manager.default_environment_ids(),
        vec!["devbox".to_string(), LOCAL_ENVIRONMENT_ID.to_string()]
    );
    assert!(manager.default_environment().expect("default").is_remote());
}

#[tokio::test]
async fn environment_manager_disables_provider_default() {
    let snapshot = EnvironmentProviderSnapshot {
        environments: vec![EnvironmentProviderEntry {
            id: "devbox".to_string(),
            transport: prepared_websocket_environment(),
            skills: Default::default(),
        }],
        default: EnvironmentDefault::Disabled,
        include_local: true,
    };
    let manager = EnvironmentManager::from_snapshot(
        snapshot,
        Some(test_runtime_paths()),
        legacy_http_client_factory(),
    )
    .expect("manager");

    assert_eq!(manager.default_environment_id(), None);
    assert!(manager.default_environment().is_none());
    assert!(Arc::ptr_eq(
        &manager
            .get_environment(LOCAL_ENVIRONMENT_ID)
            .expect("local environment"),
        &manager.try_local_environment().expect("local environment")
    ));
}

#[tokio::test]
async fn environment_manager_rejects_unknown_provider_default() {
    let snapshot = EnvironmentProviderSnapshot {
        environments: vec![EnvironmentProviderEntry {
            id: "devbox".to_string(),
            transport: prepared_websocket_environment(),
            skills: Default::default(),
        }],
        default: EnvironmentDefault::EnvironmentId("missing".to_string()),
        include_local: true,
    };
    let err = EnvironmentManager::from_snapshot(
        snapshot,
        Some(test_runtime_paths()),
        legacy_http_client_factory(),
    )
    .expect_err("unknown default should fail");

    assert_eq!(
        err.to_string(),
        "exec-server protocol error: default environment `missing` is not configured"
    );
}

#[tokio::test]
async fn environment_manager_includes_local_for_default_provider_without_url() {
    let manager = EnvironmentManager::create_for_tests(
        /*exec_server_url*/ None,
        Some(test_runtime_paths()),
    )
    .await;

    let environment = manager.default_environment().expect("default environment");
    assert_eq!(manager.default_environment_id(), Some(LOCAL_ENVIRONMENT_ID));
    assert!(Arc::ptr_eq(
        &environment,
        &manager
            .get_environment(LOCAL_ENVIRONMENT_ID)
            .expect("local environment")
    ));
    assert!(Arc::ptr_eq(
        &environment,
        &manager.try_local_environment().expect("local environment")
    ));
    assert!(!environment.is_remote());
}

#[tokio::test]
async fn environment_manager_carries_local_runtime_paths() {
    let runtime_paths = test_runtime_paths();
    let manager = EnvironmentManager::create_for_tests(
        /*exec_server_url*/ None,
        Some(runtime_paths.clone()),
    )
    .await;

    let environment = manager.try_local_environment().expect("local environment");

    assert_eq!(environment.local_runtime_paths(), Some(&runtime_paths));
    let manager = EnvironmentManager::create_for_tests(
        /*exec_server_url*/ None,
        Some(
            environment
                .local_runtime_paths()
                .expect("local runtime paths")
                .clone(),
        ),
    )
    .await;
    let environment = manager.try_local_environment().expect("local environment");
    assert_eq!(environment.local_runtime_paths(), Some(&runtime_paths));
}

#[tokio::test]
async fn environment_manager_omits_default_provider_local_lookup_when_default_disabled() {
    let manager =
        EnvironmentManager::create_for_tests(Some("none".to_string()), Some(test_runtime_paths()))
            .await;

    assert!(manager.default_environment().is_none());
    assert_eq!(manager.default_environment_id(), None);
    assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
    assert!(manager.get_environment(REMOTE_ENVIRONMENT_ID).is_none());
    assert_local_environment_unavailable(&manager);
}

#[tokio::test]
async fn environment_manager_snapshot_without_local_environment_disables_local_default() {
    let mut snapshot = EnvironmentProviderSnapshot {
        environments: Vec::new(),
        default: EnvironmentDefault::EnvironmentId(LOCAL_ENVIRONMENT_ID.to_string()),
        include_local: true,
    };
    snapshot.include_local = false;
    snapshot.default = EnvironmentDefault::Disabled;
    let manager = EnvironmentManager::from_snapshot(
        snapshot,
        /*local_runtime_paths*/ None,
        legacy_http_client_factory(),
    )
    .expect("environment manager");

    assert!(manager.default_environment().is_none());
    assert_eq!(manager.default_environment_id(), None);
    assert!(manager.get_environment(LOCAL_ENVIRONMENT_ID).is_none());
    assert_local_environment_unavailable(&manager);
}

#[tokio::test]
async fn get_environment_returns_none_for_unknown_id() {
    let manager = EnvironmentManager::default_for_tests();

    assert!(manager.get_environment("does-not-exist").is_none());
}

#[tokio::test]
async fn environment_manager_upserts_named_remote_environment() {
    let manager = EnvironmentManager::without_environments(legacy_http_client_factory());

    manager
        .upsert_environment(
            "executor-a".to_string(),
            "ws://127.0.0.1:8765".to_string(),
            /*connect_timeout*/ None,
        )
        .expect("remote environment");
    let first = manager
        .get_environment("executor-a")
        .expect("first remote environment");
    assert!(first.is_remote());
    assert_eq!(manager.default_environment_id(), None);

    manager
        .upsert_environment(
            "executor-a".to_string(),
            "ws://127.0.0.1:9876".to_string(),
            /*connect_timeout*/ None,
        )
        .expect("updated remote environment");
    let second = manager
        .get_environment("executor-a")
        .expect("second remote environment");
    assert!(second.is_remote());
    assert!(!Arc::ptr_eq(&first, &second));

    let removed = manager
        .remove_environment("executor-a")
        .expect("removed remote environment");
    assert!(Arc::ptr_eq(&second, &removed));
    assert!(manager.get_environment("executor-a").is_none());
    assert!(manager.remove_environment("executor-a").is_none());

    manager
        .upsert_environment(
            "executor-a".to_string(),
            "ws://127.0.0.1:8765".to_string(),
            /*connect_timeout*/ None,
        )
        .expect("replacement remote environment");
    let replacement = manager
        .get_environment("executor-a")
        .expect("registered replacement");
    assert!(!Arc::ptr_eq(&removed, &replacement));
}

#[tokio::test]
async fn environment_manager_starts_remote_environment_when_upserted() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket listener");
    let manager = EnvironmentManager::without_environments(legacy_http_client_factory());

    manager
        .upsert_environment(
            "executor-a".to_string(),
            format!("ws://{}", listener.local_addr().expect("listener address")),
            /*connect_timeout*/ None,
        )
        .expect("remote environment");

    timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("environment should start connecting when registered")
        .expect("accept connection");
}

#[tokio::test]
async fn environment_status_keeps_stdio_environment_pending() {
    let environment = Environment::remote_with_transport(
        ExecServerTransportParams::StdioCommand {
            command: StdioExecServerCommand {
                program: "codex-missing-exec-server-for-test".to_string(),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
            },
            initialize_timeout: Duration::from_secs(1),
        },
        /*local_runtime_paths*/ None,
        legacy_http_client_factory(),
        ScopedSkillsConfig::default(),
    );

    assert_eq!(
        environment.status().await,
        EnvironmentObservedStatus::Pending
    );
    assert!(!environment.startup_finished());
}

#[tokio::test]
async fn environment_manager_leaves_stdio_environment_lazy() {
    let transport = ExecServerTransportParams::StdioCommand {
        command: StdioExecServerCommand {
            program: "codex-missing-exec-server-for-test".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
        },
        initialize_timeout: Duration::from_secs(1),
    };
    let manager = EnvironmentManager::from_snapshot(
        EnvironmentProviderSnapshot {
            environments: vec![EnvironmentProviderEntry {
                id: "stdio".to_string(),
                transport,
                skills: Default::default(),
            }],
            default: EnvironmentDefault::Disabled,
            include_local: false,
        },
        /*local_runtime_paths*/ None,
        legacy_http_client_factory(),
    )
    .expect("environment manager");
    let environment = manager.get_environment("stdio").expect("stdio environment");

    assert!(!environment.startup_finished());
    assert!(environment.wait_until_ready().await.is_err());
    assert!(environment.startup_finished());
}

#[tokio::test]
async fn selected_capability_inspection_keeps_stdio_environment_lazy() {
    use codex_protocol::capabilities::CapabilityRootLocation;
    use codex_protocol::capabilities::SelectedCapabilityRoot;

    let transport = ExecServerTransportParams::StdioCommand {
        command: StdioExecServerCommand {
            program: "codex-missing-exec-server-for-test".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
        },
        initialize_timeout: Duration::from_secs(1),
    };
    let manager = EnvironmentManager::from_snapshot(
        EnvironmentProviderSnapshot {
            environments: vec![EnvironmentProviderEntry {
                id: "stdio".to_string(),
                transport,
                skills: Default::default(),
            }],
            default: EnvironmentDefault::Disabled,
            include_local: false,
        },
        /*local_runtime_paths*/ None,
        legacy_http_client_factory(),
    )
    .expect("environment manager");
    let environment = manager.get_environment("stdio").expect("stdio environment");
    let selected_root = SelectedCapabilityRoot {
        id: "demo@1".to_string(),
        location: CapabilityRootLocation::Environment {
            environment_id: "stdio".to_string(),
            path: PathUri::parse("file:///plugins/demo").expect("plugin path URI"),
        },
    };

    let status = manager.inspect_selected_capability_roots(std::slice::from_ref(&selected_root));
    assert!(status.ready_roots.is_empty());
    assert_eq!(status.warnings, Vec::<String>::new());
    assert!(!environment.startup_finished());

    let missing_root = SelectedCapabilityRoot {
        id: "missing@1".to_string(),
        location: CapabilityRootLocation::Environment {
            environment_id: "missing".to_string(),
            path: PathUri::parse("file:///plugins/missing").expect("missing plugin path URI"),
        },
    };
    let status = manager.inspect_selected_capability_roots(&[missing_root]);
    assert!(status.ready_roots.is_empty());
    assert_eq!(
        status.warnings,
        vec![
            "selected capability root `missing@1` references unavailable environment `missing`"
                .to_string()
        ]
    );

    assert!(environment.wait_until_ready().await.is_err());

    let status = manager.inspect_selected_capability_roots(&[selected_root]);
    assert!(status.ready_roots.is_empty());
    assert_eq!(status.warnings.len(), 1);
    assert!(status.warnings[0].contains("environment `stdio` is unavailable"));
}

#[tokio::test]
async fn replacing_environment_stops_its_startup_task() {
    let first_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind first websocket listener");
    let second_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind second websocket listener");
    let manager = EnvironmentManager::without_environments(legacy_http_client_factory());
    manager
        .upsert_environment(
            "executor-a".to_string(),
            format!(
                "ws://{}",
                first_listener.local_addr().expect("first listener address")
            ),
            /*connect_timeout*/ None,
        )
        .expect("first remote environment");
    let environment = manager
        .get_environment("executor-a")
        .expect("first remote environment");
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    environment.observe_connection_attempts(move |_, outcome| {
        observed_tx
            .send(outcome)
            .expect("record cancelled connection");
    });
    let _connection = timeout(Duration::from_secs(1), first_listener.accept())
        .await
        .expect("connection should start")
        .expect("accept initial connection");
    let startup_abort = environment
        .startup_task
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .expect("startup task")
        .abort_handle();
    assert!(!startup_abort.is_finished());
    drop(environment);

    manager
        .upsert_environment(
            "executor-a".to_string(),
            format!(
                "ws://{}",
                second_listener
                    .local_addr()
                    .expect("second listener address")
            ),
            /*connect_timeout*/ None,
        )
        .expect("replacement remote environment");

    timeout(Duration::from_secs(1), async {
        while !startup_abort.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacing the environment should cancel its startup task");
    assert_eq!(
        observed_rx.try_recv(),
        Ok(super::ConnectionAttemptOutcome::Cancelled)
    );
    assert!(observed_rx.try_recv().is_err());
}

#[tokio::test]
async fn environment_manager_rejects_empty_remote_environment_url() {
    let manager = EnvironmentManager::without_environments(legacy_http_client_factory());

    let err = manager
        .upsert_environment(
            "executor-a".to_string(),
            String::new(),
            /*connect_timeout*/ None,
        )
        .expect_err("empty URL should fail");

    assert_eq!(
        err.to_string(),
        "exec-server protocol error: remote environment requires an exec-server url"
    );
}

#[tokio::test]
async fn default_environment_has_ready_local_executor() {
    let environment = Environment::default_for_tests();

    let response = environment
        .get_exec_backend()
        .start(crate::ExecParams {
            metadata: Default::default(),
            process_id: ProcessId::from("default-env-proc"),
            argv: vec!["true".to_string()],
            cwd: PathUri::from_host_native_path(std::env::current_dir().expect("read current dir"))
                .expect("cwd URI"),
            shell_snapshot: None,
            env_policy: None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: None,
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await
        .expect("start process");

    assert_eq!(response.process.process_id().as_str(), "default-env-proc");
}

#[tokio::test]
async fn local_environment_passes_runtime_paths_to_exec_backend() {
    let environment = Environment::local(test_runtime_paths(), legacy_http_client_factory());
    #[cfg(unix)]
    let uri = "file://server/share/checkout";
    #[cfg(windows)]
    let uri = "file:///usr/local/checkout";
    let sandbox_cwd = PathUri::parse(uri).expect("non-native sandbox cwd URI");
    let source = sandbox_cwd
        .to_abs_path()
        .expect_err("sandbox cwd should not be native to this host");
    let sandbox = crate::FileSystemSandboxContext::from_permission_profile(
        codex_protocol::models::PermissionProfile::workspace_write(),
        sandbox_cwd.clone(),
    );

    let result = environment
        .get_exec_backend()
        .start(crate::ExecParams {
            metadata: Default::default(),
            process_id: ProcessId::from("local-sandbox-proc"),
            argv: vec!["true".to_string()],
            cwd: PathUri::from_host_native_path(std::env::current_dir().expect("read current dir"))
                .expect("cwd URI"),
            shell_snapshot: None,
            env_policy: None,
            env: Default::default(),
            tty: false,
            pipe_stdin: false,
            arg0: None,
            sandbox: Some(sandbox),
            enforce_managed_network: false,
            managed_network: None,
            network_proxy: None,
        })
        .await;
    let Err(err) = result else {
        panic!("sandbox cwd should be rejected after resolving runtime paths");
    };

    assert_eq!(
        err.to_string(),
        format!(
            "exec-server rejected request (-32602): sandbox cwd URI `{sandbox_cwd}` is not valid on this exec-server host: {source}"
        )
    );
}

#[tokio::test]
async fn test_environment_rejects_sandboxed_filesystem_without_runtime_paths() {
    let environment = Environment::default_for_tests();
    let path = codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path(
        std::env::current_exe().expect("current exe").as_path(),
    )
    .expect("absolute current exe");
    let path = codex_utils_path_uri::PathUri::from_abs_path(&path);
    let sandbox = crate::FileSystemSandboxContext::from_permission_profile(
        codex_protocol::models::PermissionProfile::from_runtime_permissions(
            &codex_protocol::permissions::FileSystemSandboxPolicy::restricted(Vec::new()),
            codex_protocol::permissions::NetworkSandboxPolicy::Restricted,
        ),
        PathUri::from_host_native_path(std::env::current_dir().expect("read current dir"))
            .expect("cwd URI"),
    );

    let err = environment
        .get_filesystem()
        .read_file(&path, Default::default(), Some(&sandbox))
        .await
        .expect_err("sandboxed read should require runtime paths");

    assert_eq!(
        err.to_string(),
        "sandboxed filesystem operations require configured runtime paths"
    );
}
