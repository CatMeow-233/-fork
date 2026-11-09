use std::sync::Arc;

use codex_api::AuthProvider;
use codex_http_client::OutboundProxyPolicy;
use http::HeaderMap;
use http::HeaderValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use pretty_assertions::assert_eq;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tracing::Instrument;
use tracing_subscriber::prelude::*;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::header_regex;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

#[derive(Debug)]
struct StaticRegistryAuthProvider;

impl AuthProvider for StaticRegistryAuthProvider {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), codex_api::AuthError> {
        Ok(())
    }

    fn apply_auth_headers(&self, mut headers: HeaderMap) -> codex_api::AuthHeadersFuture<'_> {
        Box::pin(async move {
            let _ = headers.insert(
                http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer registry-token"),
            );
            let _ = headers.insert(
                "ChatGPT-Account-ID",
                HeaderValue::from_static("workspace-123"),
            );
            Ok(headers)
        })
    }
}

fn static_registry_auth_provider() -> SharedAuthProvider {
    Arc::new(StaticRegistryAuthProvider)
}

struct ScopedExecutorAuth;

impl AuthProvider for ScopedExecutorAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), codex_api::AuthError> {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer synthetic-scoped-token"),
        );
        Ok(())
    }
}

#[tokio::test]
async fn invalid_executor_credential_headers_are_never_sent() {
    struct InvalidExecutorAuth(HeaderMap);

    impl AuthProvider for InvalidExecutorAuth {
        fn add_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), codex_api::AuthError> {
            headers.extend(self.0.clone());
            Ok(())
        }
    }

    let server = MockServer::start().await;
    let public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    for credentials in [
        vec![],
        vec![(AUTHORIZATION, HeaderValue::from_static("Bearer "))],
        vec![(
            AUTHORIZATION,
            HeaderValue::from_static("Basic synthetic-secret"),
        )],
        vec![(
            AUTHORIZATION,
            HeaderValue::from_bytes(b"Bearer synthetic-\xff-secret").expect("header value"),
        )],
        vec![
            (
                AUTHORIZATION,
                HeaderValue::from_static("Bearer synthetic-first-secret"),
            ),
            (
                AUTHORIZATION,
                HeaderValue::from_static("Bearer synthetic-second-secret"),
            ),
        ],
        vec![
            (
                AUTHORIZATION,
                HeaderValue::from_static("Bearer synthetic-secret"),
            ),
            (
                HeaderName::from_static("chatgpt-account-id"),
                HeaderValue::from_static("account-id"),
            ),
        ],
    ] {
        let mut headers = HeaderMap::new();
        for (name, value) in credentials {
            headers.append(name, value);
        }
        let mut client =
            EnvironmentRegistryClient::new(server.uri(), Arc::new(InvalidExecutorAuth(headers)))
                .expect("registry client");
        client.auth_mode = RegistryAuthMode::ExecutorCredential;
        let registration = client
            .register_environment("environment-requested", &public_key)
            .await
            .expect_err("invalid credentials must be rejected");
        assert!(matches!(
            registration,
            ExecServerError::EnvironmentRegistryAuth(_)
        ));
        assert!(!format!("{registration:?} {registration}").contains("synthetic"));

        let validator = RegistryHarnessKeyValidator {
            client,
            environment_id: "environment-requested".to_string(),
            executor_registration_id: "registration".to_string(),
        };
        let validation = validator
            .validate_harness_key(
                &public_key,
                "harness-authorization",
                /*stream_id*/ None,
            )
            .await
            .expect_err("invalid credentials must be rejected");
        assert!(matches!(
            validation,
            ExecServerError::EnvironmentRegistryAuth(_)
        ));
        assert!(!format!("{validation:?} {validation}").contains("synthetic"));
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
}

#[tokio::test]
async fn executor_credentials_require_https_or_loopback() {
    for (url, allowed) in [
        ("https://registry.example", true),
        ("http://localhost:8080", true),
        ("http://127.0.0.1:8080", true),
        ("http://[::1]:8080", true),
        ("http://registry.example", false),
        ("http://localhost.example", false),
        ("not a url", false),
    ] {
        let mut client =
            EnvironmentRegistryClient::new(url.to_string(), Arc::new(ScopedExecutorAuth))
                .expect("registry client");
        client.auth_mode = RegistryAuthMode::ExecutorCredential;
        assert_eq!(client.resolve_auth().await.is_ok(), allowed, "{url}");
    }

    let legacy = EnvironmentRegistryClient::new(
        "http://registry.example".to_string(),
        static_registry_auth_provider(),
    )
    .expect("registry client");
    let (headers, credential) = legacy.resolve_auth().await.expect("legacy auth");
    assert_eq!(headers[AUTHORIZATION], "Bearer registry-token");
    assert_eq!(headers["ChatGPT-Account-ID"], "workspace-123");
    assert!(credential.is_none());
}

#[tokio::test]
async fn insecure_registry_never_receives_executor_credentials() {
    let proxy = MockServer::start().await;
    let registry_url = "http://scoped-registry-proxy.invalid";
    for endpoint in ["register", "validate"] {
        let request_url =
            format!("{registry_url}/cloud/environment/environment-requested/{endpoint}");
        codex_http_client::cache_system_proxy_route_for_test(&request_url, proxy.uri());
    }
    let mut client = EnvironmentRegistryClient::new_with_telemetry(
        registry_url.to_string(),
        Arc::new(ScopedExecutorAuth),
        ExecServerTelemetry::default(),
        HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
    )
    .expect("registry client");
    client.auth_mode = RegistryAuthMode::ExecutorCredential;
    let public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    let error = client
        .register_environment("environment-requested", &public_key)
        .await
        .expect_err("registration must reject a non-loopback HTTP registry");
    assert!(matches!(
        error,
        ExecServerError::EnvironmentRegistryConfig(_)
    ));

    let validator = RegistryHarnessKeyValidator {
        client,
        environment_id: "environment-requested".to_string(),
        executor_registration_id: "registration".to_string(),
    };
    let error = validator
        .validate_harness_key(
            &public_key,
            "harness-authorization",
            /*stream_id*/ None,
        )
        .await
        .expect_err("validation must reject a non-loopback HTTP registry");
    assert!(matches!(
        error,
        ExecServerError::EnvironmentRegistryConfig(_)
    ));
    assert!(
        proxy
            .received_requests()
            .await
            .expect("proxy requests")
            .is_empty()
    );
}

#[tokio::test]
async fn scoped_loopback_registry_bypasses_a_configured_proxy() {
    let registry = MockServer::start().await;
    let proxy = MockServer::start().await;
    let public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    for endpoint in ["register", "validate"] {
        let request_url = format!(
            "{}/cloud/environment/environment-requested/{endpoint}",
            registry.uri()
        );
        codex_http_client::cache_system_proxy_route_for_test(&request_url, proxy.uri());
    }
    Mock::given(method("POST"))
        .and(path("/cloud/environment/environment-requested/register"))
        .and(header(AUTHORIZATION, "Bearer synthetic-scoped-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "environment_id": "environment-requested",
            "url": "wss://rendezvous.test/",
            "security_profile": NOISE_RELAY_SECURITY_PROFILE,
            "executor_registration_id": "registration",
        })))
        .expect(1)
        .mount(&registry)
        .await;
    Mock::given(method("POST"))
        .and(path("/cloud/environment/environment-requested/validate"))
        .and(header(AUTHORIZATION, "Bearer synthetic-scoped-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"valid": true})))
        .expect(1)
        .mount(&registry)
        .await;
    let http_client_factory = HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy);
    assert!(matches!(
        http_client_factory.resolve_proxy_route(&format!(
            "{}/cloud/environment/environment-requested/register",
            registry.uri()
        )),
        codex_http_client::OutboundProxyRoute::Proxy { .. }
    ));
    let mut client = EnvironmentRegistryClient::new_with_telemetry(
        registry.uri(),
        Arc::new(ScopedExecutorAuth),
        ExecServerTelemetry::default(),
        http_client_factory,
    )
    .expect("registry client");
    client.auth_mode = RegistryAuthMode::ExecutorCredential;
    let (headers, credential) = client.resolve_auth().await.expect("scoped auth");
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[AUTHORIZATION], "Bearer synthetic-scoped-token");
    assert!(headers[AUTHORIZATION].is_sensitive());
    let credential = credential.expect("raw executor credential");
    assert_eq!(credential, "synthetic-scoped-token");
    assert!(credential.is_sensitive());
    client
        .register_environment("environment-requested", &public_key)
        .await
        .expect("register directly");
    RegistryHarnessKeyValidator {
        client,
        environment_id: "environment-requested".to_string(),
        executor_registration_id: "registration".to_string(),
    }
    .validate_harness_key(
        &public_key,
        "harness-authorization",
        /*stream_id*/ None,
    )
    .await
    .expect("validate directly");
    registry.verify().await;
    assert!(
        proxy
            .received_requests()
            .await
            .expect("proxy requests")
            .is_empty()
    );
}

#[tokio::test]
async fn executor_credential_is_never_returned_in_registry_diagnostics() {
    assert_registry_diagnostics_omit_bearer(RegistryAuthMode::ExecutorCredential).await;
}

#[tokio::test]
async fn chatgpt_token_is_never_returned_in_registry_diagnostics() {
    assert_registry_diagnostics_omit_bearer(RegistryAuthMode::ChatgptToken).await;
}

async fn assert_registry_diagnostics_omit_bearer(mode: RegistryAuthMode) {
    const TOKEN: &str = "synthetic-scoped-registry-secret";
    struct ExecutorAuth;
    impl AuthProvider for ExecutorAuth {
        fn add_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), codex_api::AuthError> {
            let mut credential = HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("bearer");
            credential.set_sensitive(true);
            headers.insert(AUTHORIZATION, credential);
            Ok(())
        }
    }
    let encoded_echo = serde_json::json!({
        "environment_id": "environment-requested",
        "url": "wss://rendezvous.test/",
        "security_profile": NOISE_RELAY_SECURITY_PROFILE,
        "executor_registration_id": TOKEN,
    })
    .to_string()
    .replace("synthetic", "\\u0073ynthetic");
    for (status, body) in [
        (401, format!(r#"{{"error":{{"message":"{TOKEN}"}}}}"#)),
        (
            502,
            format!(
                r#"{{"detail":{{"code":"authentication_service_unavailable","message":"{TOKEN}"}}}}"#
            ),
        ),
        (
            500,
            format!(r#"{{"error":{{"code":"{TOKEN}","message":"{TOKEN}"}}}}"#),
        ),
        (200, format!(r#"{{"environment_id":"{TOKEN}"}}"#)),
        (200, encoded_echo),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cloud/environment/environment-requested/register"))
            .and(header(AUTHORIZATION, format!("Bearer {TOKEN}")))
            .respond_with(ResponseTemplate::new(status).set_body_string(body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/cloud/environment/environment-requested/validate"))
            .and(header(AUTHORIZATION, format!("Bearer {TOKEN}")))
            .respond_with(
                ResponseTemplate::new(status).set_body_string(format!(r#"{{"valid":"{TOKEN}"}}"#)),
            )
            .mount(&server)
            .await;
        let mut client = EnvironmentRegistryClient::new(server.uri(), Arc::new(ExecutorAuth))
            .expect("registry client");
        client.auth_mode = mode;
        let public_key = NoiseChannelIdentity::generate()
            .expect("identity")
            .public_key();
        let registration = client
            .register_environment("environment-requested", &public_key)
            .await
            .expect_err("registry echoes cannot enter diagnostics");
        assert!(!format!("{registration:?} {registration}").contains(TOKEN));
        if status == 502 {
            assert!(matches!(
                &registration,
                ExecServerError::EnvironmentRegistryHttp { code: Some(code), .. }
                    if code == "authentication_service_unavailable"
            ));
        }
        let validator = RegistryHarnessKeyValidator {
            client,
            environment_id: "environment-requested".to_string(),
            executor_registration_id: "registration".to_string(),
        };
        let validation = validator
            .validate_harness_key(
                &public_key,
                "harness-authorization",
                /*stream_id*/ None,
            )
            .await
            .expect_err("invalid validation response");
        assert!(!format!("{validation:?} {validation}").contains(TOKEN));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn register_environment_posts_with_auth_provider_headers() {
    let provider = SdkTracerProvider::builder().build();
    let tracer = provider.tracer("exec-server-test");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    let _guard = subscriber.set_default();
    tracing::callsite::rebuild_interest_cache();
    let server = MockServer::start().await;
    let executor_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    Mock::given(method("POST"))
            .and(path("/cloud/environment/environment-requested/register"))
            .and(header("authorization", "Bearer registry-token"))
            .and(header("chatgpt-account-id", "workspace-123"))
            .and(header_regex(
                "traceparent",
                "^00-[0-9a-f]{32}-[0-9a-f]{16}-0[01]$",
            ))
            .and(body_partial_json(serde_json::json!({
                "security_profile": NOISE_RELAY_SECURITY_PROFILE,
                "executor_public_key": executor_public_key.clone(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "environment_id": "environment-requested",
                "url": "wss://rendezvous.test/cloud-agent/default/ws/environment/environment-requested?role=environment&sig=abc",
                "security_profile": NOISE_RELAY_SECURITY_PROFILE,
                "executor_registration_id": "registration-1",
            })))
            .mount(&server)
            .await;
    let client = EnvironmentRegistryClient::new(server.uri(), static_registry_auth_provider())
        .expect("client");

    let response = client
        .register_environment("environment-requested", &executor_public_key)
        .instrument(tracing::info_span!("remote-operation"))
        .await
        .expect("register environment");

    assert_eq!(
            response,
            EnvironmentRegistryRegistrationResponse {
                environment_id: "environment-requested".to_string(),
                url: "wss://rendezvous.test/cloud-agent/default/ws/environment/environment-requested?role=environment&sig=abc".to_string(),
                security_profile: NOISE_RELAY_SECURITY_PROFILE.to_string(),
                executor_registration_id: "registration-1".to_string(),
            }
        );
}

#[tokio::test]
async fn noise_connect_provider_requests_and_validates_a_full_bundle() {
    let server = MockServer::start().await;
    let harness_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    let executor_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    Mock::given(method("POST"))
            .and(path("/cloud/environment/environment-requested/connect"))
            .and(header("authorization", "Bearer registry-token"))
            .and(header("chatgpt-account-id", "workspace-123"))
            .and(body_partial_json(serde_json::json!({
                "harness_public_key": harness_public_key.clone(),
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "environment_id": "environment-requested",
                "url": "wss://rendezvous.test/cloud-agent/default/ws/environment/environment-requested?role=harness&sig=abc",
                "security_profile": NOISE_RELAY_SECURITY_PROFILE,
                "executor_registration_id": "registration-1",
                "executor_public_key": executor_public_key.clone(),
                "harness_key_authorization": "authorization-1",
            })))
            .mount(&server)
            .await;
    let config = NoiseRendezvousEnvironmentConfig::new(
        server.uri(),
        "environment-requested".to_string(),
        "registry-token".to_string(),
        Some("workspace-123".to_string()),
    )
    .expect("noise configuration");

    let bundle = config
        .into_connect_provider(HttpClientFactory::new(
            codex_http_client::OutboundProxyPolicy::ReqwestDefault,
        ))
        .expect("Noise connect provider")
        .connect_bundle(harness_public_key)
        .await
        .expect("Noise connect bundle");

    assert_eq!(
        bundle.websocket_url,
        "wss://rendezvous.test/cloud-agent/default/ws/environment/environment-requested?role=harness&sig=abc"
    );
    assert_eq!(bundle.environment_id, "environment-requested");
    assert_eq!(bundle.executor_registration_id, "registration-1");
    assert_eq!(bundle.executor_public_key, executor_public_key);
    assert_eq!(bundle.harness_key_authorization, "authorization-1");
}

#[tokio::test]
async fn connect_environment_times_out_when_registry_stalls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/cloud/environment/environment-requested/connect"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(1)))
        .mount(&server)
        .await;
    let mut client = EnvironmentRegistryClient::new(server.uri(), static_registry_auth_provider())
        .expect("client");
    client.connect_timeout = Duration::from_millis(50);
    let harness_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();

    let error = match client
        .connect_environment("environment-requested", harness_public_key)
        .await
    {
        Ok(_) => panic!("stalled connect response should time out"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ExecServerError::EnvironmentRegistryRequest(error) if error.is_timeout()
    ));
}

#[tokio::test]
async fn connect_environment_times_out_when_registry_response_body_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("registry listener should bind");
    let registry_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("registry listener should have an address")
    );
    tokio::spawn(async move {
        let (mut stream, _) = listener
            .accept()
            .await
            .expect("registry request should connect");
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 256\r\n\r\n{",
                )
                .await
                .expect("registry response headers should write");
        sleep(Duration::from_secs(1)).await;
    });
    let mut client = EnvironmentRegistryClient::new(registry_url, static_registry_auth_provider())
        .expect("client");
    client.connect_timeout = Duration::from_millis(50);
    let harness_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();

    let error = match client
        .connect_environment("environment-requested", harness_public_key)
        .await
    {
        Ok(_) => panic!("stalled connect response body should time out"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ExecServerError::EnvironmentRegistryRequest(error) if error.is_timeout()
    ));
}

#[tokio::test]
async fn connect_environment_retries_interrupted_registry_response_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("registry listener should bind");
    let registry_url = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("registry listener should have an address")
    );
    tokio::spawn(async move {
        let (mut stream, _) = listener
            .accept()
            .await
            .expect("registry request should connect");
        let mut request = [0_u8; 4096];
        let bytes_read = stream
            .read(&mut request)
            .await
            .expect("registry request should arrive before the response");
        assert_ne!(bytes_read, 0, "registry request should not be empty");
        stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 256\r\n\r\n{",
                )
                .await
                .expect("registry response headers should write");
        stream
            .shutdown()
            .await
            .expect("registry connection should close");
    });
    let client = EnvironmentRegistryClient::new(registry_url, static_registry_auth_provider())
        .expect("client");
    let harness_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();

    let error = client
        .connect_environment("environment-requested", harness_public_key)
        .await
        .err()
        .expect("interrupted response body must fail");

    assert!(
        crate::client::is_retryable_registry_error(&error),
        "interrupted registry response body should be retryable: {error:?}"
    );
    assert!(matches!(
        error,
        ExecServerError::EnvironmentRegistryRequest(_)
    ));
}

#[tokio::test]
async fn connect_environment_does_not_retry_malformed_successful_responses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/cloud/environment/environment-requested/connect"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{"))
        .mount(&server)
        .await;
    let client = EnvironmentRegistryClient::new(server.uri(), static_registry_auth_provider())
        .expect("client");
    let harness_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();

    let error = client
        .connect_environment("environment-requested", harness_public_key)
        .await
        .err()
        .expect("malformed response must fail");

    assert!(!crate::client::is_retryable_registry_error(&error));
    assert!(matches!(error, ExecServerError::Json(_)));
}

#[tokio::test]
async fn register_environment_does_not_follow_redirects_with_auth_headers() {
    let server = MockServer::start().await;
    let executor_public_key = NoiseChannelIdentity::generate()
        .expect("identity")
        .public_key();
    Mock::given(method("POST"))
        .and(path("/cloud/environment/environment-requested/register"))
        .and(header("authorization", "Bearer registry-token"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/redirect-target", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(path("/redirect-target"))
        .and(header("authorization", "Bearer registry-token"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let client = EnvironmentRegistryClient::new(server.uri(), static_registry_auth_provider())
        .expect("client");

    let error = client
        .register_environment("environment-requested", &executor_public_key)
        .await
        .expect_err("redirect response should not be followed");

    assert!(matches!(
        error,
        ExecServerError::EnvironmentRegistryHttp {
            status: StatusCode::FOUND,
            ..
        }
    ));
}

#[test]
fn remote_environment_config_preserves_http_client_factory_policy() {
    let config = RemoteEnvironmentConfig::new(
        "https://registry.example".to_string(),
        "env-1".to_string(),
        static_registry_auth_provider(),
        HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy),
    )
    .expect("config");

    assert_eq!(
        config.http_client_factory.outbound_proxy_policy(),
        OutboundProxyPolicy::RespectSystemProxy
    );
}

#[test]
fn remote_environment_config_new_defaults_to_noise_transport() {
    let config = RemoteEnvironmentConfig::new(
        "https://registry.example".to_string(),
        "env-1".to_string(),
        static_registry_auth_provider(),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
    .expect("config");

    assert_eq!(config.transport, RemoteEnvironmentTransport::Noise);
}

#[test]
fn remote_environment_config_new_with_transport_preserves_direct_transport() {
    let config = RemoteEnvironmentConfig::new_with_transport(
        "https://registry.example".to_string(),
        "env-1".to_string(),
        RemoteEnvironmentTransport::Direct,
        static_registry_auth_provider(),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
    .expect("config");

    assert_eq!(config.transport, RemoteEnvironmentTransport::Direct);
    assert!(matches!(
        config.with_executor_credential_auth(),
        Err(ExecServerError::EnvironmentRegistryConfig(_))
    ));
}

#[test]
fn debug_output_redacts_auth_provider() {
    let config = RemoteEnvironmentConfig::new(
        "https://registry.example".to_string(),
        "env-1".to_string(),
        static_registry_auth_provider(),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
    .expect("config");

    let debug = format!("{config:?}");

    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("workspace-123"));
}
