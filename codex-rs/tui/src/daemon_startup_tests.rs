//! Opportunistic attachment may fall back; automatic startup requires a shared server.

use crate::legacy_core::config::ConfigBuilder;
use crate::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[test]
fn feature_overrides_allow_daemon_without_allowing_arbitrary_config() {
    for (raw, eligible) in [
        ("features.app_server_websocket_compression=true", true),
        ("features.app_server_websocket_compression=false", true),
        ("features.worktrees='true'", false),
        ("features={worktrees=true,shell_tool=false}", true),
        ("suppress_unstable_features_warning=true", true),
        ("suppress_unstable_features_warning='true'", false),
        ("features.shell_tool=false", true),
        ("features.worktrees.enabled=true", false),
        ("tui.fullscreen_transcript=true", true),
        ("tui.fullscreen_transcript=false", true),
        ("tui.fullscreen_transcript=\"true\"", false),
        ("tui={fullscreen_transcript=true}", true),
        ("tui={fullscreen_transcript='true'}", false),
        ("tui={fullscreen_transcript=true,animations=false}", false),
        ("features={}", false),
        ("model='test'", false),
    ] {
        let overrides = codex_utils_cli::CliConfigOverrides {
            raw_overrides: vec![raw.to_string()],
        }
        .parse_overrides()
        .unwrap();
        assert_eq!(
            daemon_startup::config_exclusion(
                &overrides,
                &LoaderOverrides::default(),
                /*strict_config*/ false,
                /*bypass_hook_trust*/ false,
            )
            .is_none(),
            eligible,
            "{raw}"
        );
    }
}

#[test]
fn daemon_launch_telemetry_records_once_on_connection_update_or_early_return() {
    for mode in [Some("in_process"), Some("update_requested"), None] {
        let observations = std::cell::RefCell::new(Vec::new());
        let launch = daemon_telemetry::Launch(Some(|actual| {
            observations.borrow_mut().push(actual);
            Vec::new()
        }));
        if let Some(mode) = mode {
            launch.record(mode);
        } else {
            drop(launch);
        }
        assert_eq!(
            observations.into_inner(),
            vec![mode.unwrap_or("unconfirmed")]
        );
    }
}

#[tokio::test]
async fn daemon_feature_compatibility_respects_required_and_optional_attachment()
-> color_eyre::Result<()> {
    use codex_app_server_protocol::JSONRPCMessage;
    use futures::SinkExt;
    use futures::StreamExt;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;

    for allow_embedded_fallback in [false, true] {
        for scenario in [
            "matching",
            "unreachable daemon",
            "conflicting client",
            "explicit enable",
            "mixed flags",
            "missing enabled",
            "managed",
            "legacy managed",
            "old requirements server",
            "unsupported RPC",
        ] {
            let cli_overrides = codex_utils_cli::CliConfigOverrides {
                raw_overrides: match scenario {
                    "matching" => vec![
                        "features.instant_interrupt=true",
                        "features.remote_compaction_v2=true",
                        "features.shell_tool=true",
                        "features.zz_missing=false",
                    ],
                    "unreachable daemon" => Vec::new(),
                    "explicit enable" => vec!["features.api_key_model_discovery=true"],
                    "mixed flags" => vec![
                        "features.zz_overridden=true",
                        "features={api_key_model_discovery=false,code_mode_host=false,instant_interrupt=true}",
                        "features.code_mode_host=true",
                    ],
                    "missing enabled" => vec!["features.zz_missing=true"],
                    _ => vec!["features.api_key_model_discovery=false"],
                }
                .into_iter()
                .map(str::to_string)
                .collect(),
            }
            .parse_overrides()
            .unwrap();
            let home = TempDir::new()?;
            let config = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
                .build()
                .await?;
            let features = [
                (Feature::ApiKeyModelDiscovery, scenario != "explicit enable", "beta"),
                (Feature::CodeModeHost, false, "beta"),
                (Feature::InstantInterrupt, true, "beta"),
                (Feature::RemoteCompactionV2, false, "stable"),
                (Feature::ShellTool, false, "removed"),
            ].map(|(feature, enabled, stage)| {
                json!({"name": feature.key(), "stage": stage, "enabled": enabled, "defaultEnabled": false})
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let target = AppServerTarget::LocalDaemon {
                endpoint: RemoteAppServerEndpoint::WebSocket {
                    websocket_url: format!("ws://{}", listener.local_addr()?),
                    auth_token: None,
                },
                allow_embedded_fallback,
            };
            if cli_overrides.is_empty() {
                drop(listener);
                let result =
                    daemon_startup::check_compatibility(&target, &config, &cli_overrides).await;
                assert!(matches!(
                    (allow_embedded_fallback, result),
                    (true, Ok(daemon_startup::Compatibility::Fallback(_))) | (false, Err(_))
                ));
                continue;
            }
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap()
                    else {
                        continue;
                    };
                    let response = match request.method.as_str() {
                        "initialize" => {
                            json!({"id": request.id, "result": {"userAgent": "daemon-test"}})
                        }
                        "config/read" => {
                            json!({"id": request.id, "result": {"config": {}, "layers": [{
                                "name": {"type": "legacyManagedConfigTomlFromFile"}, "config":
                                if scenario == "legacy managed" {
                                    json!({"features": {"api_key_model_discovery": true}})
                                } else { json!({}) }
                            }]}})
                        }
                        "configRequirements/read" if scenario == "old requirements server" => {
                            json!({"id": request.id, "error": {"code": -32600, "message": "Invalid request: unknown variant `configRequirements/read`"}})
                        }
                        "configRequirements/read" => json!({"id": request.id, "result": {
                            "requirements": if scenario == "managed" {
                                json!({"featureRequirements": {"api_key_model_discovery": true}})
                            } else { json!(null) }
                        }}),
                        "experimentalFeature/list" if scenario == "unsupported RPC" => {
                            json!({"id": request.id, "error": {"code": -32601, "message": "method not found"}})
                        }
                        "experimentalFeature/list" => {
                            assert_eq!(request.params.unwrap()["threadId"], json!(null));
                            json!({"id": request.id, "result": {"data": features, "nextCursor": null}})
                        }
                        _ => panic!("unexpected request: {}", request.method),
                    };
                    socket
                        .send(Message::Text(response.to_string().into()))
                        .await
                        .unwrap();
                }
            });
            let result = daemon_startup::check_compatibility(&target, &config, &cli_overrides)
                .await
                .map(daemon_startup::Compatibility::into_parts);
            // A passing check keeps its connection for the session; close it so the server exits.
            let result = match result {
                Ok((warning, Some(connection))) => {
                    let client = connection
                        .take_for(&target)
                        .await
                        .expect("connection was opened for this target");
                    client.shutdown().await?;
                    Ok((warning, true))
                }
                Ok((warning, None)) => Ok((warning, false)),
                Err(error) => Err(error),
            };
            server.await?;
            if matches!(scenario, "matching" | "managed" | "legacy managed") {
                assert_eq!(result?, (None, true));
                continue;
            }
            let reason = match scenario {
                "explicit enable" => "This session requires api_key_model_discovery to be enabled",
                "conflicting client" | "old requirements server" | "mixed flags" => {
                    "This session requires api_key_model_discovery to be disabled"
                }
                "missing enabled" => "This session requires zz_missing to be enabled",
                "unsupported RPC" => "Experimental feature request failed",
                _ => unreachable!(),
            };
            let restartable = scenario != "unsupported RPC";
            if allow_embedded_fallback && !restartable {
                assert_eq!(
                    result?,
                    (
                        Some(format!(
                            "Running without the shared background server: {reason}."
                        )),
                        false
                    )
                );
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.restart_features.is_some(), restartable);
                if scenario == "mixed flags" {
                    assert_eq!(
                        error.restart_features,
                        Some(std::collections::BTreeMap::from([
                            ("api_key_model_discovery".to_string(), false),
                            ("code_mode_host".to_string(), true),
                        ]))
                    );
                }
                let error = error.to_string();
                assert_eq!(
                    error,
                    format!(
                        "Cannot use the shared background server: {reason}.\n{}",
                        daemon_startup::FAILURE_HINT
                    )
                );
                if scenario == "explicit enable" && !allow_embedded_fallback {
                    insta::assert_snapshot!("daemon_feature_mismatch_error", error);
                }
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn passing_daemon_check_hands_its_connection_only_to_a_fresh_matching_connect()
-> color_eyre::Result<()> {
    use codex_app_server_protocol::ClientRequest;
    use codex_app_server_protocol::ConfigRequirementsReadResponse;
    use codex_app_server_protocol::JSONRPCMessage;
    use codex_app_server_protocol::RequestId;
    use futures::SinkExt;
    use futures::StreamExt;
    use serde_json::json;
    use std::time::Duration;
    use std::time::Instant;
    use tokio_tungstenite::tungstenite::Message;

    for scenario in ["same target", "embedded fallback", "stale"] {
        let home = TempDir::new()?;
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .build()
            .await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let target = AppServerTarget::LocalDaemon {
            endpoint: RemoteAppServerEndpoint::WebSocket {
                websocket_url: format!("ws://{}", listener.local_addr()?),
                auth_token: None,
            },
            allow_embedded_fallback: false,
        };
        // Serve a single connection and record its requests until the client closes it.
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut methods = Vec::new();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap() else {
                    continue;
                };
                let result = match request.method.as_str() {
                    "initialize" => json!({"userAgent": "daemon-test"}),
                    "configRequirements/read" => json!({"requirements": null}),
                    _ => panic!("unexpected request: {}", request.method),
                };
                methods.push(request.method);
                socket
                    .send(Message::Text(
                        json!({"id": request.id, "result": result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            methods
        });

        let (warning, connection) =
            daemon_startup::check_compatibility(&target, &config, /*cli_kv_overrides*/ &[])
                .await?
                .into_parts();
        assert_eq!(warning, None);
        let connection = connection.expect("a passing check keeps its connection");
        let client = match scenario {
            "same target" => connection.take_for(&target).await,
            "embedded fallback" => connection.take_for(&AppServerTarget::Embedded).await,
            "stale" => {
                connection
                    .take_for_at(&target, Instant::now() + Duration::from_secs(60))
                    .await
            }
            _ => unreachable!(),
        };
        assert_eq!(client.is_some(), scenario == "same target", "{scenario}");
        if let Some(client) = client {
            // The session issues its requests over the already initialized connection.
            client
                .request_handle()
                .request_typed::<ConfigRequirementsReadResponse>(
                    ClientRequest::ConfigRequirementsRead {
                        request_id: RequestId::String("session-requirements".to_string()),
                        params: None,
                    },
                )
                .await?;
            client.shutdown().await?;
        }
        let expected = if scenario == "same target" {
            vec!["initialize", "configRequirements/read"]
        } else {
            vec!["initialize"]
        };
        assert_eq!(server.await?, expected, "{scenario}");
    }
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn daemon_connection_rejects_unprotected_socket_before_handshake() -> color_eyre::Result<()> {
    let home = TempDir::new()?;
    let parent = home.path().join("control");
    std::fs::create_dir(&parent)?;
    let socket_path = AbsolutePathBuf::from_absolute_path_checked(parent.join("server.sock"))?;
    let mut listener = codex_uds::UnixListener::bind(socket_path.as_path()).await?;
    let target = AppServerTarget::LocalDaemon {
        allow_embedded_fallback: true,
        endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
    };
    tokio::select! {
        result = app_server_connection::connect(&target) => assert!(result.is_err()),
        _ = listener.accept() => panic!("unprotected listener must not receive a connection"),
    }
    Ok(())
}

#[tokio::test]
async fn daemon_startup_falls_back_only_for_implicit_endpoints() -> color_eyre::Result<()> {
    for scenario in [
        "missing socket",
        "failed handshake",
        "explicit endpoint",
        "required daemon",
    ] {
        let home = TempDir::new()?;
        let config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = if scenario == "missing socket" {
            RemoteAppServerEndpoint::UnixSocket {
                socket_path: AbsolutePathBuf::from_absolute_path_checked(
                    home.path().join("gone.sock"),
                )?,
            }
        } else {
            RemoteAppServerEndpoint::WebSocket {
                websocket_url: format!("ws://{}", listener.local_addr()?),
                auth_token: None,
            }
        };
        let reject_handshake = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let mut target = if scenario == "explicit endpoint" {
            AppServerTarget::Remote { endpoint }
        } else {
            AppServerTarget::LocalDaemon {
                endpoint,
                allow_embedded_fallback: scenario != "required daemon",
            }
        };
        let original_target = target.clone();
        let mut state_db = None;
        let result = start_app_server(
            &mut target,
            Arg0DispatchPaths::default(),
            config,
            Vec::new(),
            LoaderOverrides::default(),
            /*strict_config*/ false,
            CloudConfigBundleLoader::default(),
            codex_feedback::CodexFeedback::new(),
            /*log_db*/ None,
            &mut state_db,
            Arc::new(EnvironmentManager::default_for_tests()),
            Default::default(),
        )
        .await;
        reject_handshake.abort();
        if scenario == "explicit endpoint" || scenario == "required daemon" {
            assert!(result.is_err());
            if scenario == "required daemon" {
                let message = result.err().unwrap().to_string();
                assert!(message.contains("rerun the same command with --no-daemon"));
                assert!(message.contains("failed to connect to remote app server"));
            }
            assert_eq!(target, original_target);
            assert!(state_db.is_none());
        } else {
            let server = AppServerSession::new(
                crate::backend_id::BackendId::new(),
                result?,
                target.thread_params_mode(),
            );
            assert!(server.uses_embedded_app_server());
            assert_eq!(target, AppServerTarget::Embedded);
            assert!(state_db.is_some());
            server.shutdown().await?;
        }
    }
    Ok(())
}

#[test]
fn daemon_eligibility_preserves_launch_options_and_explains_exclusions() {
    use clap::Parser;
    for (args, expected) in [
        ("--no-daemon", Some("--no-daemon")),
        ("--worktree", None),
        ("--worktree --no-daemon", Some("--no-daemon")),
        ("--oss", Some("--oss")),
        ("--profile test", Some("--profile")),
        ("--strict-config", Some("--strict-config")),
        (
            "--dangerously-bypass-hook-trust",
            Some("--dangerously-bypass-hook-trust"),
        ),
        (
            "-m test --cd /tmp -i image.png -a never -s workspace-write --add-dir /tmp --no-alt-screen hello",
            None,
        ),
    ] {
        let cli = Cli::parse_from(std::iter::once("codex").chain(args.split_whitespace()));
        assert_eq!(
            daemon_startup::exclusion(
                &cli,
                &[],
                &LoaderOverrides::default(),
                /*workload_identity_selected*/ false,
                /*exec_server_url*/ None
            ),
            expected
        );
    }
    let cli = Cli::parse_from(["codex"]);
    let overrides = [("web_search".into(), toml::Value::String("live".into()))];
    let loader = LoaderOverrides {
        ignore_user_config: true,
        ..Default::default()
    };
    for (kv, loader, workload, executor, expected) in [
        (
            &overrides[..],
            LoaderOverrides::default(),
            false,
            None,
            "command-line configuration overrides (-c, --enable, --disable, or --search)",
        ),
        (&[][..], loader, false, None, "custom configuration loader"),
        (
            &[][..],
            LoaderOverrides::default(),
            true,
            None,
            "workload identity",
        ),
        (
            &[][..],
            LoaderOverrides::default(),
            false,
            Some(std::ffi::OsStr::new("executor")),
            "executor selection (CODEX_EXEC_SERVER_URL)",
        ),
    ] {
        assert_eq!(
            daemon_startup::exclusion(&cli, kv, &loader, workload, executor),
            Some(expected)
        );
    }
}

#[test]
fn daemon_exclusion_warning_snapshot() {
    use crate::history_cell::HistoryCell;
    let cell = crate::history_cell::StartupWarningsCell::new(vec![
        "Running without the shared background server: --strict-config requires embedded mode."
            .into(),
        format!(
            "Running without the shared background server: {} requires embedded mode.",
            daemon_startup::WSL_DRVFS_EXCLUSION
        ),
        format!(
            "Running without the shared background server: {} requires embedded mode.",
            daemon_startup::GATEWAY_SIGN_IN_EXCLUSION
        ),
    ]);
    let text = cell
        .transcript_lines(/*width*/ 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("daemon_exclusion_warning", text);
}
