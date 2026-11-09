use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::Router;
use axum::routing::get;
use axum::routing::post;
use codex_config::McpServerOAuthConfig;
use codex_config::types::AuthKeyringBackendKind;
use codex_config::types::OAuthCredentialsStoreMode;
use codex_exec_server::ExecServerError;
use codex_exec_server::HttpClient;
use codex_exec_server::HttpRequestParams;
use codex_exec_server::HttpRequestResponse;
use codex_exec_server::HttpResponseBodyStream;
use codex_exec_server::RouteAwareHttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use futures::future::BoxFuture;
use http::HeaderMap;
use oauth2::TokenResponse;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::net::TcpListener;
use url::Url;

use super::CallbackOutcome;
use super::McpOAuthClientRegistration;
use super::OAuthHttpClientAdapter;
use super::OAuthHttpContext;
use super::OAuthLoginPurpose;
use super::OAuthProviderError;
use super::OauthLoginFlow;
use super::StreamableHttpRedirectMode;
use super::append_callback_id_to_redirect_uri;
use super::append_query_param;
use super::callback_id_from_server_url;
use super::callback_path_from_redirect_uri;
use super::parse_oauth_callback;
use super::perform_oauth_login;
use super::perform_oauth_login_silent;
use super::resolve_authorization_manager;
use super::start_authorization;
use crate::oauth::stored_oauth_credentials;
use crate::oauth::test_support::TempCodexHome;

#[derive(Default)]
struct RecordingHttpClient {
    requests: AtomicUsize,
}

impl HttpClient for RecordingHttpClient {
    fn http_request(
        &self,
        _params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<HttpRequestResponse, ExecServerError>> {
        Box::pin(async {
            Err(ExecServerError::HttpRequest(
                "unexpected buffered OAuth request".to_string(),
            ))
        })
    }

    fn http_request_stream(
        &self,
        _params: HttpRequestParams,
    ) -> BoxFuture<'_, Result<(HttpRequestResponse, HttpResponseBodyStream), ExecServerError>> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(ExecServerError::HttpRequest(
                "configured OAuth client was used".to_string(),
            ))
        })
    }
}

async fn spawn_oauth_metadata_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind metadata listener");
    let addr = listener.local_addr().expect("read metadata listener addr");
    let base_url = format!("http://{addr}");
    let metadata = json!({
        "issuer": format!("{base_url}/mcp"),
        "authorization_endpoint": format!("{base_url}/oauth/authorize"),
        "token_endpoint": format!("{base_url}/oauth/token"),
        "registration_endpoint": format!("{base_url}/oauth/register"),
        "scopes_supported": ["read", "offline_access"],
    });
    let registration_requests = Arc::new(AtomicUsize::new(0));
    let captured_registration_requests = Arc::clone(&registration_requests);
    let path_scoped_metadata = metadata.clone();
    let app = Router::new()
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(move || {
                let metadata = path_scoped_metadata.clone();
                async move { Json(metadata) }
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(move || {
                let metadata = metadata.clone();
                async move { Json(metadata) }
            }),
        )
        .route(
            "/oauth/register",
            post(move || {
                let registration_requests = Arc::clone(&captured_registration_requests);
                async move {
                    registration_requests.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"client_id": "unexpected-dynamic-client"}))
                }
            }),
        )
        .route(
            "/oauth/token",
            post(|| async {
                Json(json!({
                    "access_token": "test-access-token",
                    "token_type": "Bearer",
                }))
            }),
        );

    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve oauth metadata");
    });

    (base_url, registration_requests)
}

async fn send_oauth_callback(callback_url: Url) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let host = callback_url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("callback URL should include a host"))?;
        let port = callback_url
            .port()
            .ok_or_else(|| anyhow::anyhow!("callback URL should include a port"))?;
        let mut stream = TcpStream::connect((host, port))?;
        let mut path = callback_url.path().to_string();
        if let Some(query) = callback_url.query() {
            path.push('?');
            path.push_str(query);
        }
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
        )?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        anyhow::ensure!(
            response.starts_with("HTTP/1.1 200"),
            "OAuth callback failed: {response}"
        );
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn ordinary_oauth_login_persists_issuer_without_a_refresh_token() -> anyhow::Result<()> {
    let _env = TempCodexHome::new();
    let (base_url, _registration_requests) = spawn_oauth_metadata_server().await;
    let server_url = format!("{base_url}/mcp");
    let flow = OauthLoginFlow::new(
        "issuer-persistence-test",
        &server_url,
        OAuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
        OAuthHttpContext {
            http_headers: None,
            env_http_headers: None,
            http_client: Arc::new(RouteAwareHttpClient::new(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            ))),
            redirect_mode: StreamableHttpRedirectMode::Legacy,
        },
        &[],
        Some(&McpServerOAuthConfig {
            client_id: Some("test-client".to_string()),
            ..Default::default()
        }),
        OAuthLoginPurpose::Mcp,
        McpOAuthClientRegistration::Auto,
        /*oauth_resource*/ None,
        /*launch_browser*/ false,
        /*callback_port*/ None,
        /*callback_url*/ None,
        /*global_callback_url*/ None,
        Some(/*timeout_secs*/ 5),
    )
    .await?;
    let authorization_url = Url::parse(&flow.authorization_url())?;
    let query = authorization_url.query_pairs().collect::<HashMap<_, _>>();
    let redirect_uri = query
        .get("redirect_uri")
        .ok_or_else(|| anyhow::anyhow!("authorization URL should include redirect_uri"))?;
    let state = query
        .get("state")
        .ok_or_else(|| anyhow::anyhow!("authorization URL should include state"))?;
    let mut callback_url = Url::parse(redirect_uri)?;
    callback_url
        .query_pairs_mut()
        .append_pair("code", "test-code")
        .append_pair("state", state);
    send_oauth_callback(callback_url).await?;
    flow.finish(/*emit_browser_url*/ false).await?;

    let stored = stored_oauth_credentials(
        "issuer-persistence-test",
        &server_url,
        OAuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )?
    .expect("OAuth login should persist credentials");
    assert_eq!(stored.issuer.as_deref(), Some(server_url.as_str()));
    assert!(stored.token_response.0.refresh_token().is_none());
    Ok(())
}

#[tokio::test]
async fn configured_client_preserves_exact_scopes_and_redirect_without_registration() {
    for (scopes, expected_scope) in [(&[][..], None), (&["read"][..], Some("read"))] {
        let (base_url, registration_requests) = spawn_oauth_metadata_server().await;
        let redirect_uri = "http://127.0.0.1:43123/callback/configured-client";
        let (auth_manager, metadata) = resolve_authorization_manager(
            &format!("{base_url}/mcp"),
            Arc::new(OAuthHttpClientAdapter::new(
                Arc::new(RouteAwareHttpClient::new(HttpClientFactory::new(
                    OutboundProxyPolicy::ReqwestDefault,
                ))),
                HeaderMap::new(),
                &format!("{base_url}/mcp"),
            )),
            OAuthLoginPurpose::Mcp,
            /*client_secret*/ None,
            /*token_endpoint_auth_method*/ None,
        )
        .await
        .expect("resolve pre-registered OAuth metadata");
        let prepared = start_authorization(
            auth_manager,
            metadata,
            scopes,
            redirect_uri,
            "configured-client",
            "eci-prd-pub-codex-123",
            /*client_secret*/ None,
            OAuthLoginPurpose::Mcp,
        )
        .await
        .expect("start pre-registered OAuth authorization");
        let oauth_state = prepared.oauth_state;

        let authorization_url = oauth_state
            .get_authorization_url()
            .await
            .expect("read authorization URL");
        let query = Url::parse(&authorization_url)
            .expect("authorization URL should parse")
            .query_pairs()
            .into_owned()
            .collect::<HashMap<String, String>>();

        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("eci-prd-pub-codex-123")
        );
        assert_eq!(
            query.get("redirect_uri").map(String::as_str),
            Some(redirect_uri)
        );
        assert_eq!(query.get("scope").map(String::as_str), expected_scope);
        assert_eq!(registration_requests.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn oauth_callback_validates_rfc_9207_issuer_before_token_exchange() {
    for (supports_issuer, callback_issuer, expected_token_requests) in [
        (true, Some("matching"), 1),
        (true, Some("mismatched"), 0),
        (true, None, 0),
        (false, Some("mismatched"), 0),
        (false, None, 1),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind authorization metadata listener");
        let issuer = format!(
            "http://{}",
            listener.local_addr().expect("read listener address")
        );
        let token_requests = Arc::new(AtomicUsize::new(0));
        let captured_token_requests = Arc::clone(&token_requests);
        let authorization_issuer = format!("{issuer}/mcp");
        let metadata = json!({
            "issuer": authorization_issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "authorization_response_iss_parameter_supported": supports_issuer,
        });
        let scoped_metadata = metadata.clone();
        let app = Router::new()
            .route(
                "/.well-known/oauth-authorization-server/mcp",
                get(move || {
                    let metadata = scoped_metadata.clone();
                    async move { Json(metadata) }
                }),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                get(move || {
                    let metadata = metadata.clone();
                    async move { Json(metadata) }
                }),
            )
            .route(
                "/token",
                post(move || {
                    let token_requests = Arc::clone(&captured_token_requests);
                    async move {
                        token_requests.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"access_token":"test-token","token_type":"Bearer"}))
                    }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve authorization metadata fixture");
        });
        let redirect_uri = if supports_issuer {
            "http://127.0.0.1/callback"
        } else {
            "http://127.0.0.1/callback/test-callback"
        };
        let (auth_manager, metadata) = resolve_authorization_manager(
            &format!("{issuer}/mcp"),
            Arc::new(OAuthHttpClientAdapter::new(
                Arc::new(RouteAwareHttpClient::new(HttpClientFactory::new(
                    OutboundProxyPolicy::ReqwestDefault,
                ))),
                HeaderMap::new(),
                &format!("{issuer}/mcp"),
            )),
            OAuthLoginPurpose::Mcp,
            /*client_secret*/ None,
            /*token_endpoint_auth_method*/ None,
        )
        .await
        .expect("resolve issuer-aware authorization metadata");
        let prepared = start_authorization(
            auth_manager,
            metadata,
            &[],
            redirect_uri,
            "test-callback",
            "test-client",
            /*client_secret*/ None,
            OAuthLoginPurpose::Mcp,
        )
        .await
        .expect("start issuer-aware authorization");
        let mut state = prepared.oauth_state;
        let csrf_state = Url::parse(
            &state
                .get_authorization_url()
                .await
                .expect("retrieve authorization URL"),
        )
        .expect("parse authorization URL")
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL should contain state");
        let callback_issuer = match callback_issuer {
            Some("matching") => Some(authorization_issuer.as_str()),
            Some(_) => Some("https://unexpected.example"),
            None => None,
        };
        let result = state
            .handle_callback_with_issuer("test-code", &csrf_state, callback_issuer)
            .await;

        assert_eq!(
            token_requests.load(Ordering::SeqCst),
            expected_token_requests
        );
        assert_eq!(result.is_ok(), expected_token_requests == 1);

        if expected_token_requests == 0 {
            state
                .handle_callback_with_issuer(
                    "legitimate-code",
                    &csrf_state,
                    Some(authorization_issuer.as_str()),
                )
                .await
                .expect("issuer validation failures must preserve OAuth authorization state");
            assert_eq!(token_requests.load(Ordering::SeqCst), 1);
        }
        server.abort();
    }
}

#[tokio::test]
async fn interactive_oauth_login_uses_supplied_http_client() {
    let http_client = Arc::new(RecordingHttpClient::default());
    perform_oauth_login(
        "configured-client",
        "http://127.0.0.1:1/mcp",
        OAuthCredentialsStoreMode::default(),
        AuthKeyringBackendKind::default(),
        /*http_headers*/ None,
        /*env_http_headers*/ None,
        &[],
        /*oauth_config*/ None,
        McpOAuthClientRegistration::Auto,
        /*oauth_resource*/ None,
        /*callback_port*/ None,
        /*callback_url*/ None,
        /*global_callback_url*/ None,
        http_client.clone(),
    )
    .await
    .expect_err("OAuth metadata discovery should fail through the supplied client");

    assert!(http_client.requests.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn silent_oauth_login_uses_supplied_http_client() {
    let http_client = Arc::new(RecordingHttpClient::default());
    perform_oauth_login_silent(
        "configured-client",
        "http://127.0.0.1:1/mcp",
        OAuthCredentialsStoreMode::default(),
        AuthKeyringBackendKind::default(),
        /*http_headers*/ None,
        /*env_http_headers*/ None,
        &[],
        /*oauth_config*/ None,
        McpOAuthClientRegistration::Auto,
        /*oauth_resource*/ None,
        /*callback_port*/ None,
        /*callback_url*/ None,
        /*global_callback_url*/ None,
        http_client.clone(),
        StreamableHttpRedirectMode::Legacy,
    )
    .await
    .expect_err("OAuth metadata discovery should fail through the supplied client");

    assert!(http_client.requests.load(Ordering::SeqCst) > 0);
}

#[test]
fn parse_oauth_callback_accepts_default_path() {
    let parsed = parse_oauth_callback("/callback?code=abc&state=xyz", "/callback");
    assert!(matches!(parsed, CallbackOutcome::Success(_)));
}

#[test]
fn parse_oauth_callback_preserves_rfc_9207_issuer() {
    let parsed = parse_oauth_callback(
        "/callback?code=abc&state=xyz&iss=https%3A%2F%2Fissuer.example",
        "/callback",
    );
    assert_eq!(
        parsed,
        CallbackOutcome::Success(super::OauthCallbackResult {
            code: "abc".to_string(),
            state: "xyz".to_string(),
            issuer: Some("https://issuer.example".to_string()),
        })
    );
}

#[test]
fn parse_oauth_callback_accepts_custom_path() {
    let parsed = parse_oauth_callback("/oauth/callback?code=abc&state=xyz", "/oauth/callback");
    assert!(matches!(parsed, CallbackOutcome::Success(_)));
}

#[test]
fn parse_oauth_callback_accepts_callback_id_path() {
    let parsed = parse_oauth_callback("/callback/abc123?code=abc&state=xyz", "/callback/abc123");
    assert!(matches!(parsed, CallbackOutcome::Success(_)));
}

#[test]
fn parse_oauth_callback_rejects_missing_callback_id_path() {
    let parsed = parse_oauth_callback("/callback?code=abc&state=xyz", "/callback/abc123");
    assert!(matches!(parsed, CallbackOutcome::Invalid));
}

#[test]
fn parse_oauth_callback_rejects_wrong_path() {
    let parsed = parse_oauth_callback("/callback?code=abc&state=xyz", "/oauth/callback");
    assert!(matches!(parsed, CallbackOutcome::Invalid));
}

#[test]
fn parse_oauth_callback_returns_provider_error() {
    let parsed = parse_oauth_callback(
        "/callback?error=invalid_scope&error_description=scope%20rejected",
        "/callback",
    );

    assert_eq!(
        parsed,
        CallbackOutcome::Error(OAuthProviderError::new(
            Some("invalid_scope".to_string()),
            Some("scope rejected".to_string()),
        ))
    );
}

#[test]
fn callback_path_comes_from_redirect_uri() {
    let path = callback_path_from_redirect_uri("https://example.com/oauth/callback")
        .expect("redirect URI should parse");
    assert_eq!(path, "/oauth/callback");
}

#[test]
fn callback_id_is_bound_to_server_url() {
    let callback_id = callback_id_from_server_url("https://mcp.example.com/mcp?tenant=one")
        .expect("server URL should parse");
    let same_without_fragment =
        callback_id_from_server_url("https://mcp.example.com/mcp?tenant=one#unused")
            .expect("server URL should parse");
    let different_path = callback_id_from_server_url("https://mcp.example.com/sse?tenant=one")
        .expect("server URL should parse");
    let different_query = callback_id_from_server_url("https://mcp.example.com/mcp?tenant=two")
        .expect("server URL should parse");
    let different_origin = callback_id_from_server_url("https://mcp.example.com:8443/mcp")
        .expect("server URL should parse");

    assert_eq!(callback_id, same_without_fragment);
    assert_ne!(callback_id, different_path);
    assert_ne!(callback_id, different_query);
    assert_ne!(callback_id, different_origin);
    assert_eq!(callback_id, "XuuuHAzzHOni");
}

#[test]
fn callback_id_is_appended_to_redirect_uri_path() {
    let redirect_uri =
        append_callback_id_to_redirect_uri("http://127.0.0.1:1234/callback", "abc123")
            .expect("redirect URI should parse");

    assert_eq!(redirect_uri, "http://127.0.0.1:1234/callback/abc123");
    assert_eq!(
        append_callback_id_to_redirect_uri(&redirect_uri, "abc123")
            .expect("resolved redirect URI should parse"),
        redirect_uri
    );
}

#[test]
fn callback_id_is_appended_before_redirect_uri_query() {
    let redirect_uri = append_callback_id_to_redirect_uri(
        "https://callbacks.example.com/oauth/callback?provider=github",
        "abc123",
    )
    .expect("redirect URI should parse");

    assert_eq!(
        redirect_uri,
        "https://callbacks.example.com/oauth/callback/abc123?provider=github"
    );
}

#[test]
fn portless_loopback_callbacks_use_the_active_listener_port() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("start callback listener");
    let listener_port = server
        .server_addr()
        .to_ip()
        .expect("resolve callback listener address")
        .port();

    for path in ["/callback", "/callback/callback-id", "/custom/callback"] {
        let callback = format!("http://127.0.0.1{path}");
        assert_eq!(
            super::resolve_redirect_uri(&server, Some(&callback))
                .expect("insert active listener port"),
            format!("http://127.0.0.1:{listener_port}{path}")
        );
    }

    for callback in [
        "http://localhost/callback",
        "http://127.0.0.1:3080/callback",
        "https://127.0.0.1/callback",
        "https://devbox.example.com/callback",
    ] {
        assert_eq!(
            super::resolve_redirect_uri(&server, Some(callback))
                .expect("preserve configured callback origin"),
            callback
        );
    }
}

#[test]
fn append_query_param_adds_resource_to_absolute_url() {
    let url = append_query_param(
        "https://example.com/authorize?scope=read",
        "resource",
        Some("https://api.example.com"),
    );

    assert_eq!(
        url,
        "https://example.com/authorize?scope=read&resource=https%3A%2F%2Fapi.example.com"
    );
}

#[test]
fn append_query_param_ignores_empty_values() {
    let url = append_query_param(
        "https://example.com/authorize?scope=read",
        "resource",
        Some("   "),
    );

    assert_eq!(url, "https://example.com/authorize?scope=read");
}

#[test]
fn append_query_param_handles_unparseable_url() {
    let url = append_query_param("not a url", "resource", Some("api/resource"));

    assert_eq!(url, "not a url?resource=api%2Fresource");
}
