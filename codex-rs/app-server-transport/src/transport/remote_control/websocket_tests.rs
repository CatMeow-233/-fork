use super::*;
use crate::outgoing_message::OutgoingMessage;
use crate::transport::remote_control::ServerEvent;
use crate::transport::remote_control::auth::mark_recovery_auth_change_seen;
use crate::transport::remote_control::protocol::StreamId;
use crate::transport::remote_control::protocol::normalize_remote_control_url;
use chrono::Utc;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerNotificationEnvelope;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::test_support::auth_manager_from_auth;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::save_auth;
use codex_login::token_data::TokenData;
use codex_login::token_data::parse_chatgpt_jwt_claims;
use codex_protocol::auth::AuthMode;
use codex_state::StateRuntime;
use codex_utils_absolute_path::test_support::PathExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::accept_async;

// State initialization can delay the first local connection on loaded Bazel workers.
pub(super) const TEST_HTTP_ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const TEST_INSTALLATION_ID: &str = "11111111-1111-4111-8111-111111111111";
pub(super) const TEST_REMOTE_CONTROL_SERVER_TOKEN: &str = "Remote Control Token";

pub(super) fn remote_control_enrollment(
    remote_control_token: Option<&str>,
) -> RemoteControlEnrollment {
    RemoteControlEnrollment {
        remote_control_target: normalize_remote_control_url("http://localhost/backend-api/")
            .expect("target should normalize"),
        account_id: "account_id".to_string(),
        environment_id: "env_test".to_string(),
        server_id: "srv_e_test".to_string(),
        server_name: "test-server".to_string(),
        remote_control_token: remote_control_token.map(str::to_string),
        expires_at: remote_control_token
            .map(|_| time::OffsetDateTime::now_utc() + time::Duration::hours(1)),
        next_refresh_at: None,
    }
}

pub(super) fn test_current_enrollment(
    enrollment: Option<RemoteControlEnrollment>,
) -> CurrentRemoteControlEnrollment {
    Arc::new(RemoteControlEnrollmentState::new(enrollment))
}

#[tokio::test(start_paused = true)]
async fn queued_auth_change_waits_for_server_delay() {
    let (auth_change_tx, mut auth_change_rx) = watch::channel(0);
    auth_change_tx.send_replace(1);
    let started = tokio::time::Instant::now();
    let auth_change = wait_for_auth_change(&mut auth_change_rx, Some(Duration::from_secs(5)));
    tokio::pin!(auth_change);

    assert!(
        timeout(Duration::from_secs(4), auth_change.as_mut())
            .await
            .is_err()
    );
    timeout(Duration::from_secs(2), auth_change)
        .await
        .expect("queued credentials should be usable as soon as the server delay expires")
        .expect("auth watch should remain open");
    assert_eq!(started.elapsed(), Duration::from_secs(5));
}

#[test]
fn websocket_404_only_reports_explicit_missing_remote_app_server() {
    let cases = [
        (
            Some(br#"{"detail":"Remote app server not found"}"#.to_vec()),
            true,
        ),
        (
            Some(br#" { "detail": "Remote app server not found", "extra": true } "#.to_vec()),
            true,
        ),
        (Some(br#"{"detail":"Not Found"}"#.to_vec()), false),
        (Some(b"Not Found".to_vec()), false),
        (Some(b"{".to_vec()), false),
        (Some(Vec::new()), false),
        (None, false),
    ];

    for (body, expected) in cases {
        let response = tungstenite::http::Response::builder()
            .status(/*status*/ 404)
            .body(body)
            .expect("response should build");
        assert_eq!(
            websocket_response_reports_missing_remote_app_server(&response),
            expected
        );
    }

    let response = tungstenite::http::Response::builder()
        .status(/*status*/ 503)
        .body(Some(
            br#"{"detail":"Remote app server not found"}"#.to_vec(),
        ))
        .expect("response should build");
    assert!(!websocket_response_reports_missing_remote_app_server(
        &response
    ));
}

pub(super) fn remote_control_status_channel() -> (
    RemoteControlStatusPublisher,
    watch::Receiver<RemoteControlStatusChangedNotification>,
) {
    let (status_tx, status_rx) = watch::channel(RemoteControlStatusChangedNotification {
        status: RemoteControlConnectionStatus::Connecting,
        server_name: "test-server".to_string(),
        installation_id: TEST_INSTALLATION_ID.to_string(),
        environment_id: None,
    });
    (RemoteControlStatusPublisher::new(status_tx), status_rx)
}

pub(super) fn enabled_desired_state_sender() -> watch::Sender<RemoteControlDesiredState> {
    watch::channel(RemoteControlDesiredState::Enabled {
        persistence_preference: None,
    })
    .0
}

#[test]
fn mark_recovery_auth_change_seen_marks_only_recovery_revision_seen() {
    let (auth_change_tx, mut auth_change_rx) = watch::channel(0u64);
    let auth_change_revision_before_recovery = *auth_change_rx.borrow();
    auth_change_tx.send_modify(|revision| *revision += 1);

    mark_recovery_auth_change_seen(&mut auth_change_rx, auth_change_revision_before_recovery);

    assert!(
        !auth_change_rx
            .has_changed()
            .expect("auth change watch should remain open")
    );
}

#[test]
fn mark_recovery_auth_change_seen_preserves_racing_auth_change() {
    let (auth_change_tx, mut auth_change_rx) = watch::channel(0u64);
    let auth_change_revision_before_recovery = *auth_change_rx.borrow();
    auth_change_tx.send_modify(|revision| *revision += 1);
    auth_change_tx.send_modify(|revision| *revision += 1);

    mark_recovery_auth_change_seen(&mut auth_change_rx, auth_change_revision_before_recovery);

    assert!(
        auth_change_rx
            .has_changed()
            .expect("auth change watch should remain open")
    );
}

pub(super) async fn remote_control_state_runtime(codex_home: &TempDir) -> Arc<StateRuntime> {
    StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(codex_home.path().abs()),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime should initialize")
}

pub(super) fn remote_control_auth_manager() -> Arc<AuthManager> {
    auth_manager_from_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
}

pub(super) fn remote_control_url_for_listener(listener: &TcpListener) -> String {
    let addr = listener
        .local_addr()
        .expect("listener should have a local addr");
    format!("http://{addr}/backend-api/")
}

pub(super) fn remote_control_auth_dot_json(access_token: &str) -> AuthDotJson {
    #[derive(serde::Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({
        "email": "user@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "user-12345",
            "user_id": "user-12345",
            "chatgpt_account_id": "account_id"
        }
    });
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = b64(&serde_json::to_vec(&header).expect("header should serialize"));
    let payload_b64 = b64(&serde_json::to_vec(&payload).expect("payload should serialize"));
    let fake_jwt = format!("{header_b64}.{payload_b64}.sig");

    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&fake_jwt).expect("fake jwt should parse"),
            access_token: access_token.to_string(),
            refresh_token: "refresh-token".to_string(),
            oauth_provenance: None,
            login_owner_id: None,
            account_id: Some("account_id".to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

#[test]
fn remote_control_websocket_reaches_server_with_invalid_custom_ca() {
    let temp = TempDir::new().unwrap();
    let invalid_ca = temp.path().join("invalid.pem");
    std::fs::write(&invalid_ca, "not a certificate").unwrap();
    for variable in ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "transport::remote_control::websocket::tests::connect_remote_control_websocket_includes_http_error_details",
                ])
                .env_remove("CODEX_CA_CERTIFICATE")
                .env_remove("SSL_CERT_FILE")
                .env(variable, &invalid_ca)
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "{variable}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[tokio::test]
async fn connect_remote_control_websocket_includes_http_error_details() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let remote_control_url = remote_control_url_for_listener(&listener);
    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should parse");
    let expected_error = format!(
        "failed to connect app-server remote control websocket `{}`: HTTP error: 503 Service Unavailable, request-id: <none>, cf-ray: <none>, body: upstream unavailable",
        remote_control_target.websocket_url
    );
    let codex_home = TempDir::new().expect("temp dir should create");
    let state_db = remote_control_state_runtime(&codex_home).await;
    let auth_manager = remote_control_auth_manager();
    let session_auth = RemoteControlAuth::capture(auth_manager.clone(), /*runtime_auth*/ None).0;
    let mut auth_recovery = session_auth.unauthorized_recovery();
    let mut auth_change_rx = auth_manager.auth_change_receiver();
    let current_enrollment = test_current_enrollment(Some(remote_control_enrollment(Some(
        TEST_REMOTE_CONTROL_SERVER_TOKEN,
    ))));
    let (status_publisher, status_rx) = remote_control_status_channel();

    let server_task = tokio::spawn(async move {
        let (stream, request_line) = accept_http_request(&listener).await;
        assert_eq!(
            request_line,
            "GET /backend-api/wham/remote/control/server HTTP/1.1"
        );
        respond_with_status_and_headers(
            stream,
            "503 Service Unavailable",
            &[("x-trace-id", "trace-503"), ("x-region", "us-east-1")],
            "upstream unavailable",
        )
        .await;
    });

    let err = match connect_remote_control_websocket(
        &remote_control_target,
        Some(state_db.as_ref()),
        RemoteControlAuthContext {
            auth_manager: &session_auth,
            auth_recovery: &mut auth_recovery,
            auth_change_rx: &mut auth_change_rx,
        },
        &current_enrollment,
        RemoteControlConnectOptions {
            installation_id: TEST_INSTALLATION_ID,
            server_name: "test-server",
            subscribe_cursor: None,
            app_server_client_name: None,
            desired_state_tx: &enabled_desired_state_sender(),
            persistence: &RemoteControlPersistence::default(),
        },
        &status_publisher,
    )
    .await
    {
        Ok(_) => panic!("http error response should fail the websocket connect"),
        Err(err) => err,
    };

    server_task.await.expect("server task should succeed");
    assert_eq!(err.to_string(), expected_error);
    assert!(current_enrollment.lock().await.is_some());
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connecting,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: Some("env_test".to_string()),
        }
    );
}

#[tokio::test]
async fn connect_remote_control_websocket_invalidates_unauthorized_server_token() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let remote_control_url = remote_control_url_for_listener(&listener);
    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should parse");
    let codex_home = TempDir::new().expect("temp dir should create");
    let state_db = remote_control_state_runtime(&codex_home).await;
    let auth_manager = remote_control_auth_manager();
    let session_auth = RemoteControlAuth::capture(auth_manager.clone(), /*runtime_auth*/ None).0;
    let mut auth_recovery = session_auth.unauthorized_recovery();
    let mut auth_change_rx = auth_manager.auth_change_receiver();
    let next_refresh_at = time::OffsetDateTime::now_utc() + time::Duration::minutes(2);
    let mut enrollment = remote_control_enrollment(Some(TEST_REMOTE_CONTROL_SERVER_TOKEN));
    enrollment.next_refresh_at = Some(next_refresh_at);
    let current_enrollment = test_current_enrollment(Some(enrollment));
    let (status_publisher, status_rx) = remote_control_status_channel();

    let server_task = tokio::spawn(async move {
        let (stream, request_line) = accept_http_request(&listener).await;
        assert_eq!(
            request_line,
            "GET /backend-api/wham/remote/control/server HTTP/1.1"
        );
        respond_with_status_and_headers(stream, "401 Unauthorized", &[], "unauthorized").await;
    });

    let err = connect_remote_control_websocket(
        &remote_control_target,
        Some(state_db.as_ref()),
        RemoteControlAuthContext {
            auth_manager: &session_auth,
            auth_recovery: &mut auth_recovery,
            auth_change_rx: &mut auth_change_rx,
        },
        &current_enrollment,
        RemoteControlConnectOptions {
            installation_id: TEST_INSTALLATION_ID,
            server_name: "test-server",
            subscribe_cursor: None,
            app_server_client_name: None,
            desired_state_tx: &enabled_desired_state_sender(),
            persistence: &RemoteControlPersistence::default(),
        },
        &status_publisher,
    )
    .await
    .expect_err("unauthorized response should fail the websocket connect");

    server_task.await.expect("server task should succeed");
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connecting,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: Some("env_test".to_string()),
        }
    );
    assert_eq!(
        err.to_string(),
        "remote control websocket auth failed with HTTP 401 Unauthorized; refreshing server token before reconnect"
    );
    let mut expected_enrollment = remote_control_enrollment(/*remote_control_token*/ None);
    expected_enrollment.remote_control_target = remote_control_target;
    expected_enrollment.next_refresh_at = Some(next_refresh_at);
    assert_eq!(*current_enrollment.lock().await, Some(expected_enrollment));
}

#[tokio::test]
async fn connect_remote_control_websocket_recovers_after_unauthorized_enrollment() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let remote_control_url = remote_control_url_for_listener(&listener);
    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should parse");
    let enroll_url = remote_control_target.enroll_url.clone();
    let codex_home = TempDir::new().expect("temp dir should create");
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json("stale-token"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("stale auth should save");
    let state_db = remote_control_state_runtime(&codex_home).await;
    let auth_manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        codex_login::test_support::transport_default_auth_route_config(),
    )
    .await;
    let session_auth = RemoteControlAuth::capture(auth_manager.clone(), /*runtime_auth*/ None).0;
    let mut auth_recovery = session_auth.unauthorized_recovery();
    let mut auth_change_rx = auth_manager.auth_change_receiver();
    let current_enrollment = test_current_enrollment(/*enrollment*/ None);
    let (status_publisher, status_rx) = remote_control_status_channel();
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json("fresh-token"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("fresh auth should save");

    let server_task = tokio::spawn(async move {
        let (stream, request_line) = accept_http_request(&listener).await;
        assert_eq!(
            request_line,
            "POST /backend-api/wham/remote/control/server/enroll HTTP/1.1"
        );
        respond_with_status_and_headers(stream, "401 Unauthorized", &[], "unauthorized").await;
    });

    let err = connect_remote_control_websocket(
        &remote_control_target,
        Some(state_db.as_ref()),
        RemoteControlAuthContext {
            auth_manager: &session_auth,
            auth_recovery: &mut auth_recovery,
            auth_change_rx: &mut auth_change_rx,
        },
        &current_enrollment,
        RemoteControlConnectOptions {
            installation_id: TEST_INSTALLATION_ID,
            server_name: "test-server",
            subscribe_cursor: None,
            app_server_client_name: None,
            desired_state_tx: &enabled_desired_state_sender(),
            persistence: &RemoteControlPersistence::default(),
        },
        &status_publisher,
    )
    .await
    .expect_err("unauthorized enrollment should fail the websocket connect");

    server_task.await.expect("server task should succeed");
    assert!(
        !status_rx
            .has_changed()
            .expect("remote control status watch should remain open")
    );
    assert_eq!(
        err.to_string(),
        format!(
            "remote control server enrollment failed at `{enroll_url}`: HTTP 401 Unauthorized, request-id: <none>, cf-ray: <none>, body: unauthorized; retrying after auth recovery"
        )
    );
    assert_eq!(
        auth_manager
            .auth()
            .await
            .expect("auth should remain available")
            .get_token()
            .expect("token should be readable"),
        "fresh-token"
    );
    assert!(
        !auth_change_rx
            .has_changed()
            .expect("auth change watch should remain open"),
        "recovery's own auth reload should not wake the reconnect loop"
    );
}

#[tokio::test]
async fn connect_remote_control_websocket_recovers_after_unauthorized_refresh() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let remote_control_url = remote_control_url_for_listener(&listener);
    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should parse");
    let refresh_url = remote_control_target.refresh_url.clone();
    let codex_home = TempDir::new().expect("temp dir should create");
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json("stale-token"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("stale auth should save");
    let state_db = remote_control_state_runtime(&codex_home).await;
    let auth_manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        codex_login::test_support::transport_default_auth_route_config(),
    )
    .await;
    let session_auth = RemoteControlAuth::capture(auth_manager.clone(), /*runtime_auth*/ None).0;
    let mut auth_recovery = session_auth.unauthorized_recovery();
    let mut auth_change_rx = auth_manager.auth_change_receiver();
    let mut expected_enrollment = remote_control_enrollment(Some(TEST_REMOTE_CONTROL_SERVER_TOKEN));
    expected_enrollment.remote_control_target = remote_control_target.clone();
    expected_enrollment.expires_at =
        Some(time::OffsetDateTime::now_utc() + time::Duration::minutes(4));
    let current_enrollment = test_current_enrollment(Some(expected_enrollment.clone()));
    let (status_publisher, status_rx) = remote_control_status_channel();
    save_auth(
        codex_home.path(),
        &remote_control_auth_dot_json("fresh-token"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("fresh auth should save");

    let server_task = tokio::spawn(async move {
        let (stream, request_line) = accept_http_request(&listener).await;
        assert_eq!(
            request_line,
            "POST /backend-api/wham/remote/control/server/refresh HTTP/1.1"
        );
        respond_with_status_and_headers(stream, "401 Unauthorized", &[], "unauthorized").await;
    });

    let err = connect_remote_control_websocket(
        &remote_control_target,
        Some(state_db.as_ref()),
        RemoteControlAuthContext {
            auth_manager: &session_auth,
            auth_recovery: &mut auth_recovery,
            auth_change_rx: &mut auth_change_rx,
        },
        &current_enrollment,
        RemoteControlConnectOptions {
            installation_id: TEST_INSTALLATION_ID,
            server_name: "test-server",
            subscribe_cursor: None,
            app_server_client_name: None,
            desired_state_tx: &enabled_desired_state_sender(),
            persistence: &RemoteControlPersistence::default(),
        },
        &status_publisher,
    )
    .await
    .expect_err("unauthorized refresh should fail the websocket connect");

    server_task.await.expect("server task should succeed");
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connecting,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: Some("env_test".to_string()),
        }
    );
    assert_eq!(
        err.to_string(),
        format!(
            "remote control server refresh failed at `{refresh_url}`: HTTP 401 Unauthorized, request-id: <none>, cf-ray: <none>, body: unauthorized; retrying after auth recovery"
        )
    );
    assert_eq!(
        auth_manager
            .auth()
            .await
            .expect("auth should remain available")
            .get_token()
            .expect("token should be readable"),
        "fresh-token"
    );
    assert_eq!(current_enrollment.snapshot(), Some(expected_enrollment));
    assert!(
        !auth_change_rx
            .has_changed()
            .expect("auth change watch should remain open"),
        "recovery's own auth reload should not wake the reconnect loop"
    );
}

#[tokio::test]
async fn connect_remote_control_websocket_requires_sqlite_state_db() {
    let remote_control_target = normalize_remote_control_url("http://127.0.0.1:9/backend-api/")
        .expect("target should parse");
    let auth_manager = remote_control_auth_manager();
    let session_auth = RemoteControlAuth::capture(auth_manager.clone(), /*runtime_auth*/ None).0;
    let mut auth_recovery = session_auth.unauthorized_recovery();
    let mut auth_change_rx = auth_manager.auth_change_receiver();
    let current_enrollment = test_current_enrollment(Some(remote_control_enrollment(Some(
        TEST_REMOTE_CONTROL_SERVER_TOKEN,
    ))));
    let (status_publisher, _status_rx) = remote_control_status_channel();

    let err = connect_remote_control_websocket(
        &remote_control_target,
        /*state_db*/ None,
        RemoteControlAuthContext {
            auth_manager: &session_auth,
            auth_recovery: &mut auth_recovery,
            auth_change_rx: &mut auth_change_rx,
        },
        &current_enrollment,
        RemoteControlConnectOptions {
            installation_id: TEST_INSTALLATION_ID,
            server_name: "test-server",
            subscribe_cursor: None,
            app_server_client_name: None,
            desired_state_tx: &enabled_desired_state_sender(),
            persistence: &RemoteControlPersistence::default(),
        },
        &status_publisher,
    )
    .await
    .expect_err("missing sqlite state db should fail remote control");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(err.to_string(), "remote control requires sqlite state db");
    assert_eq!(*current_enrollment.lock().await, None);
}

#[tokio::test]
async fn connect_remote_control_websocket_requires_chatgpt_auth() {
    let remote_control_target = normalize_remote_control_url("http://127.0.0.1:9/backend-api/")
        .expect("target should parse");
    let codex_home = TempDir::new().expect("temp dir should create");
    let state_db = remote_control_state_runtime(&codex_home).await;
    let auth_manager = AuthManager::shared(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        codex_login::test_support::transport_default_auth_route_config(),
    )
    .await;
    let session_auth = RemoteControlAuth::capture(auth_manager.clone(), /*runtime_auth*/ None).0;
    let mut auth_recovery = session_auth.unauthorized_recovery();
    let mut auth_change_rx = auth_manager.auth_change_receiver();
    let current_enrollment = test_current_enrollment(Some(remote_control_enrollment(Some(
        TEST_REMOTE_CONTROL_SERVER_TOKEN,
    ))));
    let (status_publisher, mut status_rx) = remote_control_status_channel();
    status_publisher.publish_environment_id(Some("env_test".to_string()));
    status_rx
        .changed()
        .await
        .expect("remote control status watch should remain open");

    let err = connect_remote_control_websocket(
        &remote_control_target,
        Some(state_db.as_ref()),
        RemoteControlAuthContext {
            auth_manager: &session_auth,
            auth_recovery: &mut auth_recovery,
            auth_change_rx: &mut auth_change_rx,
        },
        &current_enrollment,
        RemoteControlConnectOptions {
            installation_id: TEST_INSTALLATION_ID,
            server_name: "test-server",
            subscribe_cursor: None,
            app_server_client_name: None,
            desired_state_tx: &enabled_desired_state_sender(),
            persistence: &RemoteControlPersistence::default(),
        },
        &status_publisher,
    )
    .await
    .expect_err("missing auth should fail remote control");

    assert_eq!(err.kind(), ErrorKind::PermissionDenied);
    assert_eq!(
        err.to_string(),
        "remote control requires ChatGPT authentication"
    );
    assert_eq!(*current_enrollment.lock().await, None);
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connecting,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: None,
        }
    );
}

#[tokio::test]
async fn run_remote_control_websocket_loop_shutdown_cancels_reconnect_backoff() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let remote_control_url = remote_control_url_for_listener(&listener);
    drop(listener);

    let remote_control_target =
        normalize_remote_control_url(&remote_control_url).expect("target should parse");
    let (transport_event_tx, transport_event_rx) = mpsc::channel(1);
    drop(transport_event_rx);
    let (status_publisher, _status_rx) = remote_control_status_channel();
    let shutdown_token = CancellationToken::new();
    let (desired_state_tx, _desired_state_rx) =
        watch::channel(RemoteControlDesiredState::Enabled {
            persistence_preference: None,
        });
    let websocket_task = tokio::spawn({
        let shutdown_token = shutdown_token.clone();
        async move {
            RemoteControlWebsocket::new(
                RemoteControlWebsocketConfig {
                    remote_control_url,
                    installation_id: TEST_INSTALLATION_ID.to_string(),
                    remote_control_target: Some(remote_control_target),
                    server_name: "test-server".to_string(),
                },
                /*state_db*/ None,
                RemoteControlAuth::capture(
                    remote_control_auth_manager(),
                    /*runtime_auth*/ None,
                )
                .0,
                RemoteControlChannels {
                    transport_event_tx,
                    status_publisher,
                    current_enrollment: test_current_enrollment(/*enrollment*/ None),
                    pairing_persistence_key: watch::channel(None).0,
                    persistence: RemoteControlPersistence::default(),
                },
                shutdown_token,
                Arc::new(desired_state_tx),
            )
            .run(/*app_server_client_name_rx*/ None)
            .await
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    shutdown_token.cancel();

    timeout(Duration::from_millis(100), websocket_task)
        .await
        .expect("shutdown should cancel reconnect backoff")
        .expect("websocket task should join");
}

#[tokio::test]
async fn publish_status_if_changed_sends_only_status_changes() {
    let (status_publisher, mut status_rx) = remote_control_status_channel();

    status_publisher.publish_environment_id(/*environment_id*/ None);
    assert!(
        timeout(Duration::from_millis(20), status_rx.changed())
            .await
            .is_err()
    );

    status_publisher.publish_environment_id(Some("env_first".to_string()));
    status_rx
        .changed()
        .await
        .expect("remote control status watch should remain open");
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connecting,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: Some("env_first".to_string()),
        }
    );

    status_publisher.publish_environment_id(Some("env_first".to_string()));
    assert!(
        timeout(Duration::from_millis(20), status_rx.changed())
            .await
            .is_err()
    );

    status_publisher.publish_status(RemoteControlConnectionStatus::Connected);
    status_rx
        .changed()
        .await
        .expect("remote control status watch should remain open");
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connected,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: Some("env_first".to_string()),
        }
    );

    status_publisher.publish_environment_id(/*environment_id*/ None);
    status_rx
        .changed()
        .await
        .expect("remote control status watch should remain open");
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Connected,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: None,
        }
    );

    status_publisher.publish_environment_id(Some("env_disabled".to_string()));
    status_publisher.publish_status(RemoteControlConnectionStatus::Disabled);
    status_rx
        .changed()
        .await
        .expect("remote control status watch should remain open");
    assert_eq!(
        status_rx.borrow().clone(),
        RemoteControlStatusChangedNotification {
            status: RemoteControlConnectionStatus::Disabled,
            server_name: "test-server".to_string(),
            installation_id: TEST_INSTALLATION_ID.to_string(),
            environment_id: None,
        }
    );

    status_publisher.publish_environment_id(Some("env_disabled".to_string()));
    assert!(
        timeout(Duration::from_millis(20), status_rx.changed())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn run_server_writer_inner_sends_periodic_ping_frames() {
    let (client_stream, mut server_stream) = connected_websocket_pair().await;
    let (websocket_writer, _websocket_reader) = client_stream.split();
    let (outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
    let state = Arc::new(Mutex::new(WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    }));
    let (_server_event_tx, server_event_rx) = mpsc::channel(super::super::CHANNEL_CAPACITY);
    let server_event_rx = Arc::new(Mutex::new(server_event_rx));
    let shutdown_token = CancellationToken::new();
    let writer_task = tokio::spawn(RemoteControlWebsocket::run_server_writer_inner(
        state,
        server_event_rx,
        used_rx,
        websocket_writer,
        Duration::from_millis(20),
        shutdown_token.clone(),
    ));

    let message = timeout(Duration::from_secs(5), server_stream.next())
        .await
        .expect("ping frame should arrive in time")
        .expect("server websocket should stay open")
        .expect("ping frame should read");
    assert!(matches!(message, tungstenite::Message::Ping(_)));

    shutdown_token.cancel();
    writer_task
        .await
        .expect("writer task should join")
        .expect("writer should stop cleanly");
}

#[tokio::test]
async fn join_connection_workers_aborts_stuck_worker_after_timeout() {
    let mut join_set = tokio::task::JoinSet::new();
    join_set.spawn(futures::future::pending::<()>());

    RemoteControlWebsocket::join_connection_workers(&mut join_set, Duration::from_millis(10)).await;

    assert!(join_set.is_empty());
}

#[tokio::test]
async fn run_server_writer_inner_assigns_contiguous_seq_ids_per_stream() {
    let (client_stream, mut server_stream) = connected_websocket_pair().await;
    let (websocket_writer, _websocket_reader) = client_stream.split();
    let (outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
    let state = Arc::new(Mutex::new(WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    }));
    let (server_event_tx, server_event_rx) = mpsc::channel(super::super::CHANNEL_CAPACITY);
    let server_event_rx = Arc::new(Mutex::new(server_event_rx));
    let shutdown_token = CancellationToken::new();
    let writer_task = tokio::spawn(RemoteControlWebsocket::run_server_writer_inner(
        state,
        server_event_rx,
        used_rx,
        websocket_writer,
        Duration::from_secs(60),
        shutdown_token.clone(),
    ));

    let client_id = ClientId("client-1".to_string());
    let first_stream = StreamId("stream-1".to_string());
    let second_stream = StreamId("stream-2".to_string());
    for stream_id in [&first_stream, &second_stream, &first_stream] {
        server_event_tx
            .send(super::super::QueuedServerEnvelope {
                event: ServerEvent::Pong {
                    status: crate::transport::remote_control::protocol::PongStatus::Active,
                },
                client_id: client_id.clone(),
                stream_id: stream_id.clone(),
                write_complete_tx: None,
            })
            .await
            .expect("server event should queue");
    }

    assert_eq!(
        read_server_text_event(&mut server_stream).await,
        serde_json::json!({
            "type": "pong",
            "client_id": "client-1",
            "stream_id": "stream-1",
            "seq_id": 1,
            "status": "active",
        })
    );
    assert_eq!(
        read_server_text_event(&mut server_stream).await,
        serde_json::json!({
            "type": "pong",
            "client_id": "client-1",
            "stream_id": "stream-2",
            "seq_id": 1,
            "status": "active",
        })
    );
    assert_eq!(
        read_server_text_event(&mut server_stream).await,
        serde_json::json!({
            "type": "pong",
            "client_id": "client-1",
            "stream_id": "stream-1",
            "seq_id": 2,
            "status": "active",
        })
    );

    shutdown_token.cancel();
    writer_task
        .await
        .expect("writer task should join")
        .expect("writer should stop cleanly");
}

#[tokio::test]
async fn run_websocket_reader_inner_times_out_without_pong_frames() {
    let (client_stream, _server_stream) = connected_websocket_pair().await;
    let (_websocket_writer, websocket_reader) = client_stream.split();
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let state = Arc::new(Mutex::new(WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    }));
    let (server_event_tx, _server_event_rx) = mpsc::channel(super::super::CHANNEL_CAPACITY);
    let (transport_event_tx, _transport_event_rx) = mpsc::channel(super::super::CHANNEL_CAPACITY);
    let shutdown_token = CancellationToken::new();
    let client_tracker = Arc::new(Mutex::new(ClientTracker::new(
        server_event_tx,
        transport_event_tx,
        &shutdown_token,
    )));

    let err = timeout(
        Duration::from_secs(5),
        RemoteControlWebsocket::run_websocket_reader_inner(
            client_tracker,
            state,
            websocket_reader,
            Duration::from_millis(100),
            shutdown_token,
        ),
    )
    .await
    .expect("reader should time out waiting for pong")
    .expect_err("missing pong should fail the websocket reader");

    assert_eq!(err.kind(), ErrorKind::TimedOut);
    assert_eq!(err.to_string(), "remote control websocket pong timeout");
}

#[test]
fn outbound_buffer_acks_by_stream_id() {
    let (mut outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
    let client_1 = ClientId("client-1".to_string());
    let client_2 = ClientId("client-2".to_string());
    let stream_1 = StreamId("stream-1".to_string());

    outbound_buffer.insert(&server_envelope(
        &client_1,
        "stream-1",
        /*seq_id*/ 1,
        "first-client-old-stream",
    ));
    outbound_buffer.insert(&server_envelope(
        &client_2,
        "stream-1",
        /*seq_id*/ 2,
        "second-client",
    ));
    outbound_buffer.insert(&server_envelope(
        &client_1,
        "stream-2",
        /*seq_id*/ 3,
        "first-client-new-stream",
    ));

    outbound_buffer.ack(
        &client_1, &stream_1, /*acked_seq_id*/ 3, /*acked_segment_id*/ None,
    );

    let mut retained = outbound_buffer
        .server_envelopes()
        .map(|server_envelope| {
            (
                server_envelope.client_id.0.as_str(),
                server_envelope.stream_id.0.as_str(),
                server_envelope.seq_id,
            )
        })
        .collect::<Vec<_>>();
    retained.sort_unstable();
    assert_eq!(
        retained,
        vec![("client-1", "stream-2", 3), ("client-2", "stream-1", 2)]
    );
    assert_eq!(*used_rx.borrow(), 2);
}

#[test]
fn outbound_buffer_retains_unacked_messages_until_ack_advances() {
    let (mut outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
    let client_1 = ClientId("client-1".to_string());
    let client_2 = ClientId("client-2".to_string());
    let stream_1 = StreamId("stream-1".to_string());

    outbound_buffer.insert(&server_envelope(
        &client_1,
        "stream-1",
        /*seq_id*/ 1,
        "first-old",
    ));
    outbound_buffer.insert(&server_envelope(
        &client_1,
        "stream-2",
        /*seq_id*/ 2,
        "first-new",
    ));
    outbound_buffer.insert(&server_envelope(
        &client_2, "stream-1", /*seq_id*/ 3, "second",
    ));

    outbound_buffer.ack(
        &client_1, &stream_1, /*acked_seq_id*/ 1, /*acked_segment_id*/ None,
    );

    let mut retained = outbound_buffer
        .server_envelopes()
        .map(|server_envelope| {
            (
                server_envelope.client_id.0.as_str(),
                server_envelope.stream_id.0.as_str(),
                server_envelope.seq_id,
            )
        })
        .collect::<Vec<_>>();
    retained.sort_unstable();
    assert_eq!(
        retained,
        vec![("client-1", "stream-2", 2), ("client-2", "stream-1", 3)]
    );
    assert_eq!(*used_rx.borrow(), 2);
}

#[test]
fn outbound_buffer_advances_segmented_acks_by_wire_cursor() {
    let (mut outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
    let client_id = ClientId("client-1".to_string());
    let stream_id = StreamId("stream-1".to_string());

    outbound_buffer.insert(&server_chunk_envelope(
        &client_id, "stream-1", /*seq_id*/ 4, /*segment_id*/ 0,
    ));
    outbound_buffer.insert(&server_chunk_envelope(
        &client_id, "stream-1", /*seq_id*/ 4, /*segment_id*/ 1,
    ));

    outbound_buffer.ack(
        &client_id,
        &stream_id,
        /*acked_seq_id*/ 4,
        /*acked_segment_id*/ Some(1),
    );

    let retained = outbound_buffer
        .server_envelopes()
        .map(|server_envelope| server_envelope.event.segment_id())
        .collect::<Vec<_>>();
    assert_eq!(retained, Vec::<Option<usize>>::new());
    assert_eq!(*used_rx.borrow(), 0);
}

#[test]
fn outbound_buffer_treats_segmentless_acks_as_seq_level_acks() {
    let (mut outbound_buffer, used_rx) = BoundedOutboundBuffer::new();
    let client_id = ClientId("client-1".to_string());
    let stream_id = StreamId("stream-1".to_string());

    outbound_buffer.insert(&server_chunk_envelope(
        &client_id, "stream-1", /*seq_id*/ 4, /*segment_id*/ 0,
    ));
    outbound_buffer.insert(&server_chunk_envelope(
        &client_id, "stream-1", /*seq_id*/ 4, /*segment_id*/ 1,
    ));

    outbound_buffer.ack(
        &client_id, &stream_id, /*acked_seq_id*/ 4, /*acked_segment_id*/ None,
    );

    let retained = outbound_buffer
        .server_envelopes()
        .map(|server_envelope| server_envelope.event.segment_id())
        .collect::<Vec<_>>();
    assert_eq!(retained, Vec::<Option<usize>>::new());
    assert_eq!(*used_rx.borrow(), 0);
}

#[test]
fn websocket_state_drops_duplicate_client_chunks_while_pending() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let first_chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 0, /*segment_count*/ 2,
        /*message_size_bytes*/ 2, b"x",
    );
    let second_chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 1, /*segment_count*/ 2,
        /*message_size_bytes*/ 2, b"y",
    );

    assert!(matches!(
        observe_client_message(&mut state, first_chunk.clone()),
        ClientSegmentObservation::Pending
    ));
    assert!(matches!(
        observe_client_message(&mut state, first_chunk.clone()),
        ClientSegmentObservation::Dropped
    ));
    assert!(matches!(
        observe_client_message(&mut state, second_chunk),
        ClientSegmentObservation::Dropped
    ));
    assert!(matches!(
        observe_client_message(&mut state, first_chunk),
        ClientSegmentObservation::Pending
    ));
}

#[test]
fn websocket_state_drops_replayed_client_chunks_after_completion() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let message = JSONRPCMessage::Notification(JSONRPCNotification {
        method: "initialized".to_string(),
        params: None,
    });
    let raw = serde_json::to_vec(&message).expect("message should serialize");
    let split = raw.len() / 2;
    let first_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 4,
        /*segment_id*/ 0,
        /*segment_count*/ 2,
        raw.len(),
        &raw[..split],
    );
    let second_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 4,
        /*segment_id*/ 1,
        /*segment_count*/ 2,
        raw.len(),
        &raw[split..],
    );

    assert!(matches!(
        observe_client_message(&mut state, first_chunk.clone()),
        ClientSegmentObservation::Pending
    ));
    let completed_envelope = match observe_client_message(&mut state, second_chunk) {
        ClientSegmentObservation::Forward(client_envelope) => *client_envelope,
        _ => panic!("expected completed client message"),
    };
    state.record_client_message_delivery(
        &completed_envelope,
        Some((
            (
                ClientId("client-1".to_string()),
                Some(StreamId("stream-1".to_string())),
            ),
            4,
        )),
    );
    assert!(matches!(
        observe_client_message(&mut state, first_chunk),
        ClientSegmentObservation::Dropped
    ));
}

#[test]
fn websocket_state_allows_replay_before_completed_chunk_delivery() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let message = JSONRPCMessage::Notification(JSONRPCNotification {
        method: "initialized".to_string(),
        params: None,
    });
    let raw = serde_json::to_vec(&message).expect("message should serialize");
    let split = raw.len() / 2;
    let first_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 4,
        /*segment_id*/ 0,
        /*segment_count*/ 2,
        raw.len(),
        &raw[..split],
    );
    let second_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 4,
        /*segment_id*/ 1,
        /*segment_count*/ 2,
        raw.len(),
        &raw[split..],
    );

    assert!(matches!(
        observe_client_message(&mut state, first_chunk.clone()),
        ClientSegmentObservation::Pending
    ));
    assert!(matches!(
        observe_client_message(&mut state, second_chunk),
        ClientSegmentObservation::Forward(_)
    ));
    assert!(matches!(
        observe_client_message(&mut state, first_chunk),
        ClientSegmentObservation::Pending
    ));
}

#[test]
fn websocket_state_allows_replay_after_rejected_out_of_order_chunk() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let first_chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 0, /*segment_count*/ 2,
        /*message_size_bytes*/ 2, b"x",
    );
    let second_chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 1, /*segment_count*/ 2,
        /*message_size_bytes*/ 2, b"y",
    );

    assert!(matches!(
        observe_client_message(&mut state, second_chunk),
        ClientSegmentObservation::Dropped
    ));
    assert!(matches!(
        observe_client_message(&mut state, first_chunk),
        ClientSegmentObservation::Pending
    ));
}

#[test]
fn websocket_state_allows_replay_after_later_chunk_drops() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let first_chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 0, /*segment_count*/ 2,
        /*message_size_bytes*/ 2, b"x",
    );
    let invalid_second_chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 1, /*segment_count*/ 2,
        /*message_size_bytes*/ 2, b"",
    );

    assert!(matches!(
        observe_client_message(&mut state, first_chunk.clone()),
        ClientSegmentObservation::Pending
    ));
    assert!(matches!(
        observe_client_message(&mut state, invalid_second_chunk),
        ClientSegmentObservation::Dropped
    ));
    assert!(matches!(
        observe_client_message(&mut state, first_chunk),
        ClientSegmentObservation::Pending
    ));
}

#[test]
fn websocket_state_drops_oversized_client_chunk_frames() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let chunk = client_chunk_envelope(
        "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 0, /*segment_count*/ 1,
        /*message_size_bytes*/ 1, b"x",
    );

    assert!(matches!(
        state.observe_client_message(chunk, REMOTE_CONTROL_SEGMENT_MAX_BYTES + 1),
        ClientSegmentObservation::Dropped
    ));
}

#[test]
fn websocket_state_ignores_oversized_stale_chunks_without_dropping_newer_assembly() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let message = JSONRPCMessage::Notification(JSONRPCNotification {
        method: "initialized".to_string(),
        params: None,
    });
    let raw = serde_json::to_vec(&message).expect("message should serialize");
    let split = raw.len() / 2;
    let first_newer_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 8,
        /*segment_id*/ 0,
        /*segment_count*/ 2,
        raw.len(),
        &raw[..split],
    );
    let oversized_stale_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 7,
        /*segment_id*/ 0,
        /*segment_count*/ 2,
        raw.len(),
        &raw[..split],
    );
    let second_newer_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 8,
        /*segment_id*/ 1,
        /*segment_count*/ 2,
        raw.len(),
        &raw[split..],
    );

    assert!(matches!(
        observe_client_message(&mut state, first_newer_chunk),
        ClientSegmentObservation::Pending
    ));
    assert!(matches!(
        state.observe_client_message(oversized_stale_chunk, REMOTE_CONTROL_SEGMENT_MAX_BYTES + 1,),
        ClientSegmentObservation::Dropped
    ));
    assert!(matches!(
        observe_client_message(&mut state, second_newer_chunk),
        ClientSegmentObservation::Forward(_)
    ));
}

#[test]
fn websocket_state_ignores_oversized_duplicate_chunks_without_dropping_current_assembly() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let message = JSONRPCMessage::Notification(JSONRPCNotification {
        method: "initialized".to_string(),
        params: None,
    });
    let raw = serde_json::to_vec(&message).expect("message should serialize");
    let split = raw.len() / 2;
    let first_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 8,
        /*segment_id*/ 0,
        /*segment_count*/ 2,
        raw.len(),
        &raw[..split],
    );
    let oversized_duplicate_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 8,
        /*segment_id*/ 0,
        /*segment_count*/ 2,
        raw.len(),
        &raw[..split],
    );
    let second_chunk = client_chunk_envelope(
        "client-1",
        "stream-1",
        /*seq_id*/ 8,
        /*segment_id*/ 1,
        /*segment_count*/ 2,
        raw.len(),
        &raw[split..],
    );

    assert!(matches!(
        observe_client_message(&mut state, first_chunk),
        ClientSegmentObservation::Pending
    ));
    assert!(matches!(
        state.observe_client_message(
            oversized_duplicate_chunk,
            REMOTE_CONTROL_SEGMENT_MAX_BYTES + 1,
        ),
        ClientSegmentObservation::Dropped
    ));
    assert!(matches!(
        observe_client_message(&mut state, second_chunk),
        ClientSegmentObservation::Forward(_)
    ));
}

#[test]
fn websocket_state_clears_chunk_cursor_when_stream_is_invalidated() {
    let (outbound_buffer, _used_rx) = BoundedOutboundBuffer::new();
    let mut state = WebsocketState {
        outbound_buffer,
        subscribe_cursor: None,
        next_seq_id_by_stream: HashMap::new(),
        last_completed_client_chunk_seq_id_by_stream: HashMap::new(),
        client_segment_reassembler: ClientSegmentReassembler::default(),
    };
    let client_id = ClientId("client-1".to_string());
    let stream_id = StreamId("stream-1".to_string());

    assert!(matches!(
        observe_client_message(
            &mut state,
            client_chunk_envelope(
                "client-1", "stream-1", /*seq_id*/ 4, /*segment_id*/ 0,
                /*segment_count*/ 2, /*message_size_bytes*/ 2, b"x",
            )
        ),
        ClientSegmentObservation::Pending
    ));
    state.invalidate_client_message_stream(&client_id, &stream_id);
    state
        .client_segment_reassembler
        .invalidate_stream(&client_id, &stream_id);

    assert!(matches!(
        observe_client_message(
            &mut state,
            client_chunk_envelope(
                "client-1", "stream-1", /*seq_id*/ 1, /*segment_id*/ 0,
                /*segment_count*/ 2, /*message_size_bytes*/ 2, b"x",
            )
        ),
        ClientSegmentObservation::Pending
    ));
}

fn server_envelope(
    client_id: &ClientId,
    stream_id: &str,
    seq_id: u64,
    summary: &str,
) -> ServerEnvelope {
    ServerEnvelope {
        event: ServerEvent::ServerMessage {
            message: Box::new(OutgoingMessage::AppServerNotification(
                ServerNotificationEnvelope {
                    notification: ServerNotification::ConfigWarning(ConfigWarningNotification {
                        summary: summary.to_string(),
                        details: None,
                        path: None,
                        range: None,
                    }),
                    emitted_at_ms: Some(1_234),
                },
            )),
        },
        client_id: client_id.clone(),
        stream_id: StreamId(stream_id.to_string()),
        seq_id,
    }
}

fn server_chunk_envelope(
    client_id: &ClientId,
    stream_id: &str,
    seq_id: u64,
    segment_id: usize,
) -> ServerEnvelope {
    ServerEnvelope {
        event: ServerEvent::ServerMessageChunk {
            segment_id,
            segment_count: 2,
            message_size_bytes: 2,
            message_chunk_base64: String::new(),
        },
        client_id: client_id.clone(),
        stream_id: StreamId(stream_id.to_string()),
        seq_id,
    }
}

fn client_chunk_envelope(
    client_id: &str,
    stream_id: &str,
    seq_id: u64,
    segment_id: usize,
    segment_count: usize,
    message_size_bytes: usize,
    chunk: &[u8],
) -> ClientEnvelope {
    ClientEnvelope {
        event: ClientEvent::ClientMessageChunk {
            segment_id,
            segment_count,
            message_size_bytes,
            message_chunk_base64: base64::engine::general_purpose::STANDARD.encode(chunk),
        },
        client_id: ClientId(client_id.to_string()),
        stream_id: Some(StreamId(stream_id.to_string())),
        seq_id: Some(seq_id),
        cursor: None,
    }
}

fn observe_client_message(
    state: &mut WebsocketState,
    envelope: ClientEnvelope,
) -> ClientSegmentObservation {
    let wire_size_bytes = serde_json::to_vec(&envelope)
        .expect("client envelope should serialize")
        .len();
    state.observe_client_message(envelope, wire_size_bytes)
}

pub(super) async fn accept_http_request(listener: &TcpListener) -> (TcpStream, String) {
    let (stream, _) = timeout(TEST_HTTP_ACCEPT_TIMEOUT, listener.accept())
        .await
        .expect("HTTP request should arrive in time")
        .expect("listener accept should succeed");
    let mut reader = BufReader::new(stream);

    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .expect("request line should read");
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("header line should read");
        if line == "\r\n" {
            break;
        }
    }

    (
        reader.into_inner(),
        request_line.trim_end_matches("\r\n").to_string(),
    )
}

async fn connected_websocket_pair() -> (
    codex_websocket_client::WebSocketConnection,
    WebSocketStream<TcpStream>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener should bind");
    let url = format!("ws://{}", listener.local_addr().expect("local address"));
    let connect_task = tokio::spawn(async move {
        codex_websocket_client::WebSocketConnector::new(&codex_http_client::HttpClientFactory::new(
            codex_http_client::OutboundProxyPolicy::ReqwestDefault,
        ))
        .unwrap()
        .connect(
            url.into_client_request().unwrap(),
            tungstenite::protocol::WebSocketConfig::default(),
        )
        .await
    });
    let (server_stream, _) = listener
        .accept()
        .await
        .expect("server should accept client");
    let server_stream = accept_async(server_stream)
        .await
        .expect("server websocket handshake should succeed");
    let (client_stream, _) = connect_task
        .await
        .expect("client connect task should join")
        .expect("client websocket handshake should succeed");

    (client_stream, server_stream)
}

async fn read_server_text_event(
    server_stream: &mut WebSocketStream<TcpStream>,
) -> serde_json::Value {
    let message = timeout(Duration::from_secs(5), server_stream.next())
        .await
        .expect("server event should arrive in time")
        .expect("server websocket should stay open")
        .expect("server event should read");
    let tungstenite::Message::Text(text) = message else {
        panic!("expected text event, got {message:?}");
    };
    serde_json::from_str(text.as_ref()).expect("server event should deserialize")
}

pub(super) async fn respond_with_status_and_headers(
    mut stream: TcpStream,
    status: &str,
    headers: &[(&str, &str)],
    body: &str,
) {
    let extra_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n{extra_headers}\r\n{body}",
        body.len(),
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("response should write");
    stream.flush().await.expect("response should flush");
}
