use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::sync::Mutex;

use clap::Parser;
use codex_protocol::config_types::SandboxMode;
use pretty_assertions::assert_eq;

use super::*;

#[derive(Default)]
struct RecordingProgress {
    events: Mutex<Vec<String>>,
}

impl RecordingProgress {
    fn events(&self) -> Vec<String> {
        self.events.lock().expect("events lock").clone()
    }
}

impl DoctorProgress for RecordingProgress {
    fn begin(&self, label: &'static str) {
        self.events
            .lock()
            .expect("events lock")
            .push(format!("begin {label}"));
    }

    fn heartbeat(&self, label: &'static str, elapsed: Duration) {
        self.events
            .lock()
            .expect("events lock")
            .push(format!("heartbeat {label} {}", elapsed.as_secs()));
    }

    fn finish(&self, label: &'static str, status: CheckStatus) {
        self.events
            .lock()
            .expect("events lock")
            .push(format!("finish {label} {status:?}"));
    }

    fn settle(&self) {
        self.events
            .lock()
            .expect("events lock")
            .push("settle".to_string());
    }
}

fn respond_once(listener: &TcpListener, response: &[u8]) {
    let (mut stream, _) = listener.accept().expect("accept probe request");
    let mut request = [0; 1024];
    let _ = stream.read(&mut request);
    stream.write_all(response).expect("write response");
}

#[test]
fn overall_status_prefers_fail() {
    let checks = vec![
        DoctorCheck::new("a", "config", CheckStatus::Warning, "warning"),
        DoctorCheck::new("b", "auth", CheckStatus::Fail, "fail"),
    ];
    assert_eq!(overall_status(&checks), CheckStatus::Fail);
}

#[test]
fn websocket_probe_human_output() {
    let report = |websocket: DoctorCheck| DoctorReport {
        schema_version: 1,
        generated_at: "0s since unix epoch".to_string(),
        overall_status: websocket.status,
        codex_version: "0.0.0".to_string(),
        checks: vec![
            DoctorCheck::new(
                "network.environment",
                "network",
                CheckStatus::Ok,
                "network environment is readable",
            )
            .detail("HTTPS proxy: not set"),
            websocket,
            DoctorCheck::new(
                "network.provider_reachability",
                "reachability",
                CheckStatus::Ok,
                "active provider endpoints are reachable over HTTP",
            )
            .detail("OpenAI endpoint: HTTP 200"),
        ],
    };
    let options = HumanOutputOptions {
        show_details: true,
        show_all: true,
        ascii: true,
        color_enabled: false,
    };
    let success = render_human_report(
        &report(
            DoctorCheck::new(
                "network.websocket_reachability",
                "websocket",
                CheckStatus::Ok,
                "Responses WebSocket handshake succeeded",
            )
            .detail("handshake result: HTTP 101")
            .detail("reasoning header: true")
            .detail("server model present: true"),
        ),
        options,
    );
    let immediate_close = render_human_report(
        &report(
            DoctorCheck::new(
                "network.websocket_reachability",
                "websocket",
                CheckStatus::Warning,
                "Responses WebSocket closed immediately after handshake",
            )
            .detail("handshake result: HTTP 101")
            .detail("immediate close code: 1008")
            .detail(format!(
                "immediate close reason: {}",
                close_reason_detail("private-close-reason")
            ))
            .remediation(
                "Check proxy, VPN, firewall, DNS, custom CA, and WebSocket policy support.",
            ),
        ),
        options,
    );
    let generic_failure = render_human_report(
        &report(websocket_probe_warning(
            "Responses WebSocket failed; HTTPS fallback may still work",
            vec!["endpoint: wss://example.com/responses".to_string()],
            websocket_error_detail(&ApiError::Stream("private-stream-error".to_string())),
        )),
        options,
    );
    let rendered = format!(
        "successful handshake\n{success}\nimmediate close\n{immediate_close}\nstream failure\n{generic_failure}"
    );

    assert!(!rendered.contains("private-close-reason"));
    assert!(!rendered.contains("private-stream-error"));
    insta::assert_snapshot!("websocket_probe_human_output", rendered);
}

#[test]
fn websocket_error_detail_preserves_categories_without_wire_values() {
    let secret = "private-wire-value";
    let message = || secret.to_string();
    let cases = [
        ApiError::Transport(TransportError::Http {
            status: http::StatusCode::FORBIDDEN,
            url: Some(format!("https://example.com/?token={secret}")),
            headers: Some(HeaderMap::from_iter([(
                http::header::SET_COOKIE,
                HeaderValue::from_static(secret),
            )])),
            body: Some(message()),
            retry_after: None,
        }),
        ApiError::Transport(TransportError::Network(message())),
        ApiError::Transport(TransportError::Build(message())),
        ApiError::Api {
            status: http::StatusCode::UNAUTHORIZED,
            message: message(),
        },
        ApiError::Stream(message()),
        ApiError::ResponseIncomplete(Box::new(codex_protocol::error::ResponseIncompleteError {
            partial_output: vec![],
            reason: message(),
            response_id: Some(message()),
            token_usage: None,
        })),
        ApiError::Retryable {
            message: message(),
            retry_after: None,
        },
        ApiError::RateLimitExceeded {
            message: message(),
            retry_after: None,
        },
        ApiError::RateLimit(message()),
        ApiError::InvalidRequest { message: message() },
        ApiError::InvalidAudioInput {
            message: message(),
            per_recording: true,
        },
        ApiError::ModelNotFound {
            message: message(),
            status: Some(http::StatusCode::NOT_FOUND),
        },
        ApiError::InvalidPrompt { message: message() },
        ApiError::CyberPolicy { message: message() },
        ApiError::BioPolicy { message: message() },
        ApiError::MisalignmentPolicyViolation {
            message: message(),
            misalignment: None,
        },
    ];
    let details = cases.iter().map(websocket_error_detail).collect::<Vec<_>>();
    assert!(!details.join("\n").contains(secret));
    assert_eq!(
        details,
        [
            "handshake transport error: http 403 Forbidden: [redacted]",
            "handshake transport error: network error: [redacted]",
            "handshake transport error: request build error: [redacted]",
            "handshake API error: 401 Unauthorized [redacted]",
            "handshake stream error: [redacted]",
            "handshake error: incomplete response: [redacted]",
            "handshake error: retryable error: [redacted]",
            "handshake error: rate limit exceeded: [redacted]",
            "handshake error: rate limit: [redacted]",
            "handshake error: invalid request: [redacted]",
            "handshake error: invalid audio input: [redacted]",
            "handshake error: model not found: [redacted]",
            "handshake error: invalid prompt: [redacted]",
            "handshake error: cyber policy: [redacted]",
            "handshake error: bio policy: [redacted]",
            "handshake error: misalignment policy violation: [redacted]",
        ]
    );
}

#[test]
fn run_sync_check_notifies_progress() {
    let progress_impl = Arc::new(RecordingProgress::default());
    let progress: Arc<dyn DoctorProgress> = progress_impl.clone();

    let check = run_sync_check("test", progress, || {
        DoctorCheck::new("test", "test", CheckStatus::Ok, "ok")
    });

    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(
        progress_impl.events(),
        vec!["begin test".to_string(), "finish test Ok".to_string()]
    );
}

#[tokio::test]
async fn run_async_check_notifies_progress() {
    let progress_impl = Arc::new(RecordingProgress::default());
    let progress: Arc<dyn DoctorProgress> = progress_impl.clone();

    let check = run_async_check("test", progress, async {
        DoctorCheck::new("test", "test", CheckStatus::Warning, "warning")
    })
    .await;

    assert_eq!(check.status, CheckStatus::Warning);
    assert_eq!(
        progress_impl.events(),
        vec!["begin test".to_string(), "finish test Warning".to_string()]
    );
}

#[test]
fn startup_warning_counts_group_known_sources() {
    let warnings = vec![
        "Skipped loading 2 skill(s) due to invalid SKILL.md files.".to_string(),
        "[features].codex_hooks is deprecated. Use [features].hooks instead.".to_string(),
        "plugin example failed to load".to_string(),
        "MCP server example failed to start".to_string(),
    ];
    let mut details = Vec::new();

    push_startup_warning_counts(&mut details, &warnings);

    assert_eq!(
        details,
        vec![
            "startup warnings: 4",
            "startup warning skills: 1",
            "startup warning hooks: 1",
            "startup warning plugins: 1",
            "startup warning MCP: 1",
            "startup warning deprecated: 1",
        ]
    );
}

#[test]
fn config_overrides_from_interactive_preserves_global_options() {
    let interactive = TuiCli::parse_from([
        "codex",
        "--oss",
        "--local-provider",
        "ollama",
        "--model",
        "llama3.2",
        "--cd",
        "/tmp",
        "--sandbox",
        "danger-full-access",
        "--ask-for-approval",
        "never",
        "--add-dir",
        "/var/tmp",
    ]);
    let arg0_paths = Arg0DispatchPaths {
        codex_self_exe: Some(PathBuf::from("/bin/codex")),
        codex_linux_sandbox_exe: Some(PathBuf::from("/bin/codex-linux-sandbox")),
        ..Default::default()
    };

    let overrides = config_overrides_from_interactive(&interactive, &arg0_paths);

    assert_eq!(overrides.model.as_deref(), Some("llama3.2"));
    assert_eq!(overrides.model_provider.as_deref(), Some("ollama"));
    assert_eq!(overrides.cwd.as_deref(), Some(Path::new("/tmp")));
    assert_eq!(overrides.approval_policy, Some(AskForApproval::Never));
    assert_eq!(overrides.sandbox_mode, Some(SandboxMode::DangerFullAccess));
    assert_eq!(overrides.show_raw_agent_reasoning, Some(true));
    assert_eq!(
        overrides.additional_writable_roots,
        vec![PathBuf::from("/var/tmp")]
    );
    assert_eq!(overrides.codex_self_exe, arg0_paths.codex_self_exe);
    assert_eq!(
        overrides.codex_linux_sandbox_exe,
        arg0_paths.codex_linux_sandbox_exe
    );
}

#[test]
fn redacted_json_report_structures_and_sanitizes_details() {
    let report = DoctorReport {
            schema_version: 1,
            generated_at: "0s since unix epoch".to_string(),
            overall_status: CheckStatus::Warning,
            codex_version: "0.0.0".to_string(),
            checks: vec![
                DoctorCheck::new(
                    "system.environment",
                    "system",
                    CheckStatus::Ok,
                    "OS language en-US",
                )
                .detail("VISUAL: code --wait")
                .detail("EDITOR: env AWS_ACCESS_KEY_ID=AKIAEXAMPLE vim")
                .detail("PAGER: env PRIVATE_PAGER_VALUE=pager-secret less")
                .detail("GIT_PAGER: delta")
                .detail("GH_PAGER: less")
                .detail("LESS: -FRX"),
                DoctorCheck::new(
                    "mcp.config",
                    "mcp",
                    CheckStatus::Warning,
                    "MCP configuration has optional issues",
                )
                .detail(
                    "optional reachability failed: remote: https://user:pass@example.com/mcp?x=abc (connect failed)",
                )
                .detail("OPENAI_API_KEY: sk-live-secret")
                .detail("duplicate: one")
                .detail("duplicate: two")
                .detail("freeform note")
                .issue(
                    DoctorIssue::new(
                        CheckStatus::Warning,
                        "remote https://user:pass@example.com/mcp?x=abc is unreachable",
                    )
                    .measured("https://user:pass@example.com/mcp?x=abc")
                    .expected("reachable MCP endpoint")
                    .remedy("Check https://user:pass@example.com/help?x=abc.")
                    .field("optional reachability failed"),
                )
                .remediation("Open https://user:pass@example.com/help?x=abc."),
            ],
        };

    let redacted_report = redacted_json_report(&report);
    let redacted = serde_json::to_string(&redacted_report).expect("serialize report");
    let json = serde_json::to_value(redacted_report).expect("report should serialize");

    assert!(!redacted.contains("user:pass"));
    assert!(!redacted.contains("x=abc"));
    assert!(!redacted.contains("sk-live-secret"));
    assert!(!redacted.contains("AKIAEXAMPLE"));
    assert!(!redacted.contains("pager-secret"));
    assert!(!redacted.contains("code --wait"));
    assert!(redacted.contains("https://example.com/mcp"));
    assert_eq!(json["checks"].is_object(), true);
    assert_eq!(
        json["checks"]["system.environment"]["details"]["VISUAL"],
        "set"
    );
    assert_eq!(
        json["checks"]["system.environment"]["details"]["EDITOR"],
        "set"
    );
    assert_eq!(
        json["checks"]["system.environment"]["details"]["PAGER"],
        "set"
    );
    assert_eq!(
        json["checks"]["system.environment"]["details"]["GIT_PAGER"],
        "set"
    );
    assert_eq!(
        json["checks"]["system.environment"]["details"]["GH_PAGER"],
        "set"
    );
    assert_eq!(
        json["checks"]["system.environment"]["details"]["LESS"],
        "set"
    );
    assert_eq!(json["checks"]["mcp.config"]["id"], "mcp.config");
    assert_eq!(
        json["checks"]["mcp.config"]["details"]["OPENAI_API_KEY"],
        "<redacted>"
    );
    assert_eq!(
        json["checks"]["mcp.config"]["details"]["duplicate"],
        serde_json::json!(["one", "two"])
    );
    assert_eq!(
        json["checks"]["mcp.config"]["notes"],
        serde_json::json!(["freeform note"])
    );
    assert_eq!(
        json["checks"]["mcp.config"]["issues"][0]["measured"],
        "https://example.com/mcp"
    );
    assert_eq!(
        json["checks"]["mcp.config"]["issues"][0]["remedy"],
        "Check https://example.com/help."
    );
}

#[tokio::test]
async fn mcp_check_ignores_disabled_servers() {
    let disabled_server: McpServerConfig = toml::from_str(
        r#"
                url = "http://127.0.0.1:9/mcp"
                enabled = false
                required = true
                bearer_token_env_var = "CODEX_DOCTOR_DISABLED_MCP_TOKEN"
            "#,
    )
    .expect("should deserialize disabled MCP config");
    let servers = HashMap::from([("disabled".to_string(), disabled_server)]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.summary, "MCP configuration is locally consistent");
    assert!(check.details.contains(&"disabled servers: 1".to_string()));
    assert!(
        check
            .details
            .iter()
            .all(|detail| !detail.contains("CODEX_DOCTOR_DISABLED_MCP_TOKEN"))
    );
    assert!(
        check
            .details
            .iter()
            .all(|detail| !detail.contains("reachability failed"))
    );
}

#[tokio::test]
async fn mcp_check_warns_for_optional_http_reachability() {
    let optional_server: McpServerConfig = toml::from_str(
        r#"
                url = "http://127.0.0.1:9/mcp"
            "#,
    )
    .expect("should deserialize optional MCP config");
    let servers = HashMap::from([("optional".to_string(), optional_server)]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Warning);
    assert_eq!(check.summary, "MCP configuration has optional issues");
    insta::assert_snapshot!(check.remediation.unwrap(), @"Check that the MCP server is running and its URL is reachable. Alternatively, disable the affected server.");
    assert!(
        check
            .details
            .iter()
            .any(|detail| detail.contains("optional reachability failed: optional:"))
    );
}

#[tokio::test]
async fn mcp_check_reports_both_missing_token_and_unreachable_url() {
    let server = toml::from_str(
        r#"
                url = "http://127.0.0.1:9/mcp"
                bearer_token_env_var = "CODEX_DOCTOR_MISSING_MCP_TOKEN"
                required = true
            "#,
    )
    .expect("required MCP config");
    let check = mcp_check_from_servers(&HashMap::from([("required".to_string(), server)])).await;

    assert_eq!(check.status, CheckStatus::Fail);
    insta::assert_snapshot!(check.remediation.unwrap(), @"Set the missing MCP env vars. Check that the MCP server is running and its URL is reachable. Alternatively, disable the affected server.");
}

#[tokio::test]
async fn mcp_check_fails_required_remote_stdio_env_var() {
    let command = toml::Value::String(
        std::env::current_exe()
            .expect("current exe")
            .to_string_lossy()
            .into_owned(),
    );
    let required_server: McpServerConfig = toml::from_str(&format!(
        r#"
                command = {command}
                required = true
                env_vars = [{{ name = "REMOTE_ONLY_TOKEN", source = "remote" }}]
            "#,
    ))
    .expect("should deserialize required MCP config");
    let servers = HashMap::from([("required".to_string(), required_server)]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.details.iter().any(|detail| {
            detail.contains(
                "required: env_vars entry `REMOTE_ONLY_TOKEN` uses source `remote`, which requires remote MCP stdio",
            )
        }));
}

#[tokio::test]
async fn mcp_check_does_not_probe_environment_stdio_on_the_host() {
    let remote_server: McpServerConfig = toml::from_str(
        r#"
                command = "remote-only-command"
                environment_id = "remote"
                cwd = "C:\\plugins\\demo"
                required = true
                env_vars = [{ name = "REMOTE_ONLY_TOKEN", source = "remote" }]
            "#,
    )
    .expect("remote MCP config");
    let servers = HashMap::from([("remote".to_string(), remote_server)]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.summary, "MCP configuration is locally consistent");
}

#[test]
fn provider_specific_auth_allows_non_openai_provider_without_env_key() {
    let check = provider_specific_auth_check(
        /*requires_openai_auth*/ false,
        /*provider_env_key*/ None,
        /*provider_env_key_instructions*/ None,
        Vec::new(),
        |_| false,
    )
    .expect("non-OpenAI provider should produce a provider-specific check");

    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(
        check.summary,
        "OpenAI auth is not required for the active model provider"
    );
}

#[test]
fn provider_specific_auth_fails_when_provider_env_key_is_missing() {
    let check = provider_specific_auth_check(
        /*requires_openai_auth*/ false,
        Some("PROVIDER_API_KEY"),
        Some("Set PROVIDER_API_KEY before running Codex."),
        Vec::new(),
        |_| false,
    )
    .expect("non-OpenAI provider should produce a provider-specific check");

    assert_eq!(check.status, CheckStatus::Fail);
    assert_eq!(
        check.summary,
        "active model provider auth env var is missing"
    );
    assert_eq!(
        check.remediation,
        Some("Set PROVIDER_API_KEY before running Codex.".to_string())
    );
}

#[test]
fn stored_auth_validation_rejects_missing_api_key() {
    let auth = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: None,
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };

    assert_eq!(
        stored_auth_issues(&auth, |_| false),
        vec!["API key auth is missing an API key"]
    );
    assert!(stored_auth_issues(&auth, |name| name == OPENAI_API_KEY_ENV_VAR).is_empty());
}

#[test]
fn stored_auth_validation_rejects_missing_chatgpt_tokens() {
    let auth = AuthDotJson {
        auth_mode: None,
        openai_api_key: None,
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };

    assert_eq!(
        stored_auth_issues(&auth, |_| false),
        vec![
            "ChatGPT auth is missing token data",
            "ChatGPT auth is missing refresh metadata",
        ]
    );
}

#[test]
fn stored_auth_validation_handles_personal_access_token() {
    let mut auth = AuthDotJson {
        auth_mode: None,
        openai_api_key: None,
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: Some("at-test".to_string()),
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };

    assert_eq!(stored_auth_mode(&auth), "personal_access_token");
    assert!(stored_auth_issues(&auth, |_| false).is_empty());

    auth.auth_mode = Some(AuthMode::PersonalAccessToken);
    auth.personal_access_token = None;
    assert_eq!(
        stored_auth_issues(&auth, |_| false),
        vec!["personal access token auth is missing a personal access token"]
    );
}

#[test]
fn provider_reachability_mode_uses_api_key_auth() {
    let api_key_auth = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };

    assert_eq!(
        provider_auth_reachability_mode_from_auth(
            /*requires_openai_auth*/ true,
            /*provider_env_key*/ None,
            /*provider_base_url*/ None,
            |_| false,
            Some(&api_key_auth),
        ),
        ProviderAuthReachabilityMode::ApiKey
    );
    assert_eq!(
        provider_auth_reachability_mode_from_auth(
            /*requires_openai_auth*/ true,
            /*provider_env_key*/ None,
            /*provider_base_url*/ None,
            |name| name == CODEX_API_KEY_ENV_VAR,
            /*stored_auth*/ None,
        ),
        ProviderAuthReachabilityMode::ApiKey
    );

    let chatgpt_auth = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        ..api_key_auth
    };
    assert_eq!(
        provider_auth_reachability_mode_from_auth(
            /*requires_openai_auth*/ true,
            /*provider_env_key*/ None,
            Some("https://custom.example/v1"),
            |name| name == OPENAI_API_KEY_ENV_VAR,
            Some(&chatgpt_auth),
        ),
        ProviderAuthReachabilityMode::Chatgpt
    );
    assert_eq!(
        provider_auth_reachability_mode_from_auth(
            /*requires_openai_auth*/ true,
            Some(OPENAI_API_KEY_ENV_VAR),
            Some("https://custom.example/v1"),
            |name| name == OPENAI_API_KEY_ENV_VAR,
            Some(&chatgpt_auth),
        ),
        ProviderAuthReachabilityMode::ApiKey
    );
    assert_eq!(
        provider_auth_reachability_mode_from_auth(
            /*requires_openai_auth*/ true,
            /*provider_env_key*/ None,
            /*provider_base_url*/ None,
            |name| name == CODEX_API_KEY_ENV_VAR,
            Some(&chatgpt_auth),
        ),
        ProviderAuthReachabilityMode::ApiKey
    );
}

#[test]
fn provider_reachability_uses_active_provider_endpoint() {
    assert_eq!(
        provider_reachability_plan_from_parts(
            ProviderAuthReachabilityMode::NotRequired,
            "azure",
            "azure",
            Some("https://example.openai.azure.com/openai/v1"),
            /*provider_query_params*/ None,
            /*is_amazon_bedrock*/ false,
            "https://chatgpt.com/backend-api/",
        ),
        ReachabilityPlan {
            description: "provider auth".to_string(),
            endpoints: vec![ReachabilityEndpoint {
                label: "azure API".to_string(),
                url: "https://example.openai.azure.com/openai/v1/responses".to_string(),
                required: true,
                route_probe_url: None,
            }],
            http_client_factory: HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        }
    );
}

#[test]
fn provider_reachability_adds_models_route_probe_for_openai_compatible_base_urls() {
    let query_params = HashMap::from([("api-version".to_string(), "2026-01-01".to_string())]);

    for (mode, description) in [
        (ProviderAuthReachabilityMode::NotRequired, "provider auth"),
        (ProviderAuthReachabilityMode::Chatgpt, "ChatGPT auth"),
    ] {
        assert_eq!(
            provider_reachability_plan_from_parts(
                mode,
                "custom",
                "Custom",
                Some("https://example.com/openai/v1/"),
                Some(&query_params),
                /*is_amazon_bedrock*/ false,
                "https://chatgpt.com/backend-api/",
            ),
            ReachabilityPlan {
                description: description.to_string(),
                endpoints: vec![ReachabilityEndpoint {
                    label: "custom API".to_string(),
                    url: "https://example.com/openai/v1/responses?api-version=2026-01-01"
                        .to_string(),
                    required: true,
                    route_probe_url: Some(
                        "https://example.com/openai/v1/models?api-version=2026-01-01".to_string()
                    ),
                }],
                http_client_factory: HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            }
        );
    }
}

#[test]
fn provider_reachability_skips_route_probe_for_bedrock() {
    let plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::NotRequired,
        "amazon-bedrock",
        "Amazon Bedrock",
        Some("https://bedrock-runtime.us-east-1.amazonaws.com/openai/v1"),
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ true,
        "https://chatgpt.com/backend-api/",
    );

    assert_eq!(plan.endpoints[0].route_probe_url, None);
}

#[test]
fn provider_reachability_api_key_does_not_require_chatgpt() {
    let plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::ApiKey,
        "openai",
        "OpenAI",
        /*provider_base_url*/ None,
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ false,
        "https://chatgpt.com/backend-api/",
    );

    assert_eq!(
        plan.endpoints,
        vec![ReachabilityEndpoint {
            label: "openai API".to_string(),
            url: "https://api.openai.com/v1/responses".to_string(),
            required: true,
            route_probe_url: Some("https://api.openai.com/v1/models".to_string()),
        }]
    );
}

#[test]
fn provider_reachability_chatgpt_uses_inference_endpoint() {
    assert_eq!(
        default_reachability_plan().endpoints,
        vec![ReachabilityEndpoint {
            label: "ChatGPT".to_string(),
            url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            required: true,
            route_probe_url: None,
        }]
    );
}

#[test]
fn provider_reachability_outcome_reports_required_failures() {
    assert_eq!(
        provider_reachability_outcome(/*required_failures*/ 0, /*warnings*/ 1,),
        (
            CheckStatus::Warning,
            "provider endpoint checks returned warnings",
        )
    );
    assert_eq!(
        provider_reachability_outcome(/*required_failures*/ 1, /*warnings*/ 0,),
        (
            CheckStatus::Fail,
            "one or more required provider endpoints are unreachable over HTTP",
        )
    );
}

#[tokio::test]
async fn provider_reachability_route_404_fails_bad_base_url_path() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        respond_once(
            &listener,
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        respond_once(
            &listener,
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    });
    let plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::ApiKey,
        "openai",
        "OpenAI",
        Some(&format!("http://{addr}/xxxx")),
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ false,
        "https://chatgpt.com/backend-api/",
    );

    let check = provider_reachability_check(plan).await;
    server.join().expect("probe server thread should finish");

    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check
            .details
            .iter()
            .any(|detail| detail.contains("route probe:") && detail.contains("HTTP 404"))
    );
    assert_eq!(check.issues.len(), 1);
    assert_eq!(
        check.issues[0].remedy.as_deref(),
        Some("Set base_url to the provider API root, for example https://api.openai.com/v1")
    );
}

#[tokio::test]
async fn provider_reachability_route_401_keeps_reachability_ok() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        respond_once(
            &listener,
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        respond_once(
            &listener,
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    });
    let plan = provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::ApiKey,
        "openai",
        "OpenAI",
        Some(&format!("http://{addr}/v1")),
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ false,
        "https://chatgpt.com/backend-api/",
    );

    let check = provider_reachability_check(plan).await;
    server.join().expect("probe server thread should finish");

    assert_eq!(check.status, CheckStatus::Ok);
    assert!(
        check
            .details
            .iter()
            .any(|detail| detail.contains("route exists (HTTP 401)"))
    );
}

#[test]
fn rollout_stats_details_count_plain_and_compressed_files() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let nested = temp
        .path()
        .join("sessions")
        .join("2026")
        .join("05")
        .join("13");
    std::fs::create_dir_all(&nested).expect("create nested rollout dir");
    std::fs::write(
        nested.join("rollout-2026-05-13T00-00-00-test.jsonl"),
        "12345",
    )
    .expect("write rollout file");
    // Storage totals include both physical files if both representations exist.
    std::fs::write(
        nested.join("rollout-2026-05-13T00-00-00-test.jsonl.zst"),
        "1234567",
    )
    .expect("write compressed sibling");
    std::fs::write(
        nested.join("rollout-2026-05-13T01-00-00-compressed.jsonl.zst"),
        "123",
    )
    .expect("write compressed rollout");
    std::fs::write(nested.join("not-a-rollout.jsonl"), "ignored").expect("write ignored jsonl");
    std::fs::write(nested.join("not-a-rollout.jsonl.zst"), "ignored")
        .expect("write ignored compressed file");
    std::fs::write(
        nested.join("rollout-2026-05-13T01-00-00-compressed.jsonl.zst.tmp"),
        "ignored",
    )
    .expect("write compression temp file");

    let archived = temp.path().join("archived_sessions");
    std::fs::create_dir(&archived).expect("create archived rollout dir");
    std::fs::write(
        archived.join("rollout-2026-05-13T02-00-00-archived.jsonl.zst"),
        "1234",
    )
    .expect("write archived compressed rollout");

    let mut details = Vec::new();
    rollout_stats_details(&mut details, temp.path());
    insta::assert_snapshot!(details.join("\n"), @r"
        active rollout files: 3 files, 15 total bytes, 5 average bytes
        archived rollout files: 1 files, 4 total bytes, 4 average bytes
        ");
}

#[tokio::test]
async fn http_probe_treats_http_status_as_reachable() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept probe request");
        let mut request = [0; 1024];
        let _ = stream.read(&mut request);
        stream
                .write_all(
                    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write response");
    });

    let status =
        http_probe_url_with_timeout(&format!("http://{addr}/mcp"), Duration::from_secs(3)).await;
    server.join().expect("probe server thread should finish");

    assert_eq!(status, Ok("HTTP 405".to_string()));
}

#[tokio::test(start_paused = true)]
async fn mcp_http_probe_falls_back_to_get_when_head_times_out() {
    use std::io::BufRead as _;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener address");
    let (head_received_tx, head_received_rx) = tokio::sync::oneshot::channel();
    let (probe_done_tx, probe_done_rx) = std::sync::mpsc::channel::<()>();
    // A blocking task inhibits Tokio's automatic clock advancement while real socket I/O
    // is pending. Keep it alive until the client has consumed the GET response, too.
    let server = tokio::task::spawn_blocking(move || {
        let (mut head_stream, _) = listener.accept().expect("accept HEAD probe request");
        let mut head_request = String::new();
        std::io::BufReader::new(&mut head_stream)
            .read_line(&mut head_request)
            .expect("read HEAD request line");
        assert_eq!(head_request, "HEAD /mcp HTTP/1.1\r\n");
        head_received_tx.send(()).expect("signal HEAD receipt");

        let (mut get_stream, _) = listener.accept().expect("accept GET probe request");
        let mut get_request = String::new();
        std::io::BufReader::new(&mut get_stream)
            .read_line(&mut get_request)
            .expect("read GET request line");
        assert_eq!(get_request, "GET /mcp HTTP/1.1\r\n");
        get_stream
                .write_all(
                    b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write response");
        // Dropping the sender also releases this wait if the probe panics.
        let _ = probe_done_rx.recv();
        drop(head_stream);
    });

    let url = format!("http://{addr}/mcp");
    let probe = mcp_http_probe_url_with_timeout(&url, Duration::from_millis(10));
    let expire_head = async {
        head_received_rx.await.expect("server should receive HEAD");
        // Cross the unchanged 10 ms deadline only after HEAD is waiting for a response.
        tokio::time::advance(Duration::from_millis(11)).await;
    };
    let (status, ()) = tokio::join!(probe, expire_head);
    drop(probe_done_tx);
    server.await.expect("probe server task should finish");

    assert_eq!(status, Ok("HTTP 405".to_string()));
}

#[tokio::test]
async fn mcp_check_fails_required_missing_stdio_command() {
    let required_server: McpServerConfig = toml::from_str(
        r#"
                command = "definitely-missing-codex-doctor-mcp"
                required = true
            "#,
    )
    .expect("should deserialize required MCP config");
    let servers = HashMap::from([("required".to_string(), required_server)]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Fail);
    assert_eq!(
        check.summary,
        "MCP configuration has failing required inputs or reachability"
    );
    insta::assert_snapshot!(check.remediation.unwrap(), @"Correct the listed MCP server configuration errors. Alternatively, disable the affected server.");
    assert!(check.details.iter().any(|detail| {
        detail.contains(
            "required: stdio command \"definitely-missing-codex-doctor-mcp\" is not resolvable",
        )
    }));
}

#[tokio::test]
async fn mcp_check_skips_host_path_checks_for_remote_stdio() {
    #[cfg(not(windows))]
    let cwd = r"C:\Users\openai\share";
    #[cfg(windows)]
    let cwd = "/home/openai/share";
    let cwd = toml::Value::String(cwd.to_string());
    let remote_server: McpServerConfig = toml::from_str(&format!(
        r#"
                command = "definitely-missing-codex-doctor-mcp"
                environment_id = "remote"
                cwd = {cwd}
                required = true
                env_vars = [{{ name = "REMOTE_ONLY_TOKEN", source = "remote" }}]
            "#,
    ))
    .expect("should deserialize remote MCP config");
    let servers = HashMap::from([("remote".to_string(), remote_server)]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.summary, "MCP configuration is locally consistent");
}

#[tokio::test]
async fn mcp_check_validates_remote_stdio_cwd() {
    let missing_cwd: McpServerConfig = toml::from_str(
        r#"
                command = "echo"
                environment_id = "remote"
                required = true
            "#,
    )
    .expect("should deserialize remote MCP config without cwd");
    let relative_cwd: McpServerConfig = toml::from_str(
        r#"
                command = "echo"
                environment_id = "remote"
                cwd = "relative"
                required = true
            "#,
    )
    .expect("should deserialize remote MCP config with relative cwd");
    let servers = HashMap::from([
        ("missing".to_string(), missing_cwd),
        ("relative".to_string(), relative_cwd),
    ]);

    let check = mcp_check_from_servers(&servers).await;

    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check
            .details
            .contains(&"missing: remote stdio requires an explicit cwd".to_string())
    );
    assert!(
        check
            .details
            .contains(&"relative: remote stdio cwd is not absolute (relative)".to_string())
    );
}

#[cfg(unix)]
#[test]
fn read_probe_file_rejects_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let file = tempfile::NamedTempFile::new().expect("create temp file");
    std::fs::write(file.path(), "cert").expect("write temp file");
    let mut permissions = std::fs::metadata(file.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o000);
    std::fs::set_permissions(file.path(), permissions).expect("remove read permissions");

    let result = read_probe_file(file.path());

    let mut permissions = std::fs::metadata(file.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(file.path(), permissions).expect("restore read permissions");
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn executable_path_exists_rejects_non_executable_file() {
    use std::os::unix::fs::PermissionsExt;

    let file = tempfile::NamedTempFile::new().expect("create temp file");
    std::fs::write(file.path(), "#!/bin/sh\n").expect("write temp file");
    let mut permissions = std::fs::metadata(file.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(file.path(), permissions).expect("set non-executable mode");

    let result = executable_path_exists(file.path());

    assert!(result.is_err());
    let mut permissions = std::fs::metadata(file.path())
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(file.path(), permissions).expect("set executable mode");
    assert_eq!(executable_path_exists(file.path()), Ok(()));
}

#[test]
fn should_enable_color_respects_terminal_inputs() {
    assert!(should_enable_color(
        /*no_color_flag*/ false,
        /*no_color_env*/ false,
        Some("xterm-256color"),
        /*stdout_is_tty*/ true,
        /*stream_supports_color*/ true,
    ));
    assert!(!should_enable_color(
        /*no_color_flag*/ true,
        /*no_color_env*/ false,
        Some("xterm-256color"),
        /*stdout_is_tty*/ true,
        /*stream_supports_color*/ true,
    ));
    assert!(!should_enable_color(
        /*no_color_flag*/ false,
        /*no_color_env*/ true,
        Some("xterm-256color"),
        /*stdout_is_tty*/ true,
        /*stream_supports_color*/ true,
    ));
    assert!(!should_enable_color(
        /*no_color_flag*/ false,
        /*no_color_env*/ false,
        Some("dumb"),
        /*stdout_is_tty*/ true,
        /*stream_supports_color*/ true,
    ));
    assert!(!should_enable_color(
        /*no_color_flag*/ false,
        /*no_color_env*/ false,
        Some("xterm-256color"),
        /*stdout_is_tty*/ false,
        /*stream_supports_color*/ true,
    ));
}

fn terminal_inputs() -> TerminalCheckInputs {
    TerminalCheckInputs {
        info: TerminalInfo {
            backend: None,
            name: TerminalName::Unknown,
            term_program: None,
            version: None,
            term: Some("xterm-256color".to_string()),
            multiplexer: None,
        },
        env: BTreeMap::from([("TERM".to_string(), "xterm-256color".to_string())]),
        present_env: BTreeSet::from(["TERM".to_string()]),
        no_color_flag: false,
        stdin_is_terminal: true,
        stdout_is_terminal: true,
        stderr_is_terminal: true,
        stream_supports_color: true,
        terminal_size: Ok((120, 40)),
        tmux_details: Vec::new(),
        windows_console_details: Vec::new(),
    }
}

fn set_terminal_env(inputs: &mut TerminalCheckInputs, name: &str, value: &str) {
    inputs.present_env.insert(name.to_string());
    if value.is_empty() {
        inputs.env.remove(name);
    } else {
        inputs.env.insert(name.to_string(), value.to_string());
    }
}

#[test]
fn terminal_check_dumb_requires_interactive_stream() {
    for (stdin, stdout, stderr, expected) in [
        (false, false, false, CheckStatus::Warning),
        (false, false, true, CheckStatus::Warning),
        (false, true, false, CheckStatus::Fail),
        (true, false, false, CheckStatus::Fail),
    ] {
        let mut inputs = terminal_inputs();
        inputs.info.name = TerminalName::Dumb;
        inputs.info.term = Some("dumb".to_string());
        set_terminal_env(&mut inputs, "TERM", "dumb");
        inputs.stdin_is_terminal = stdin;
        inputs.stdout_is_terminal = stdout;
        inputs.stderr_is_terminal = stderr;

        let check = terminal_check_from_inputs(inputs);

        assert_eq!(check.status, expected);
        assert_eq!(check.issues.len(), 1);
        assert_eq!(
            check.issues[0].remedy.as_deref(),
            (expected == CheckStatus::Fail)
                .then_some("set TERM to a real value, for example xterm-256color")
        );
        assert_eq!(
            check.issues[0].expected.as_deref(),
            (expected == CheckStatus::Fail)
                .then_some("TERM=xterm-256color or another real terminal type")
        );
        if !stdin && !stdout && !stderr {
            let report = DoctorReport {
                schema_version: 1,
                generated_at: "0s since unix epoch".to_string(),
                overall_status: overall_status(std::slice::from_ref(&check)),
                codex_version: "0.0.0".to_string(),
                checks: vec![check],
            };
            insta::assert_snapshot!(
                "doctor_dumb_non_interactive_human",
                render_human_report(
                    &report,
                    HumanOutputOptions {
                        show_details: true,
                        show_all: true,
                        ascii: true,
                        color_enabled: false,
                    }
                )
            );
        }
    }
}

#[test]
fn terminal_check_warns_for_narrow_terminal() {
    let mut inputs = terminal_inputs();
    inputs.terminal_size = Ok((79, 24));

    let check = terminal_check_from_inputs(inputs);

    assert_eq!(check.status, CheckStatus::Warning);
    assert_eq!(
        check.summary,
        "width 79 cols - output may wrap (recommended >=80)"
    );
    assert_eq!(check.issues[0].expected.as_deref(), Some(">= 80 columns"));
    assert_eq!(
        check.issues[0].remedy.as_deref(),
        Some("resize the window to at least 80 columns")
    );
}

#[test]
fn terminal_check_warns_for_declared_narrow_terminal() {
    let mut inputs = terminal_inputs();
    set_terminal_env(&mut inputs, "COLUMNS", "60");

    let check = terminal_check_from_inputs(inputs);

    assert_eq!(check.status, CheckStatus::Warning);
    assert_eq!(
        check.summary,
        "COLUMNS=60 - output may wrap (recommended >=80)"
    );
    assert!(check.details.contains(&"COLUMNS: 60".to_string()));
    assert_eq!(check.issues[0].fields, vec!["COLUMNS".to_string()]);
}

#[test]
fn terminal_check_warns_for_non_utf8_locale() {
    let mut inputs = terminal_inputs();
    set_terminal_env(&mut inputs, "LANG", "C");

    let check = terminal_check_from_inputs(inputs);

    assert_eq!(check.status, CheckStatus::Warning);
    assert_eq!(
        check.summary,
        "locale is not UTF-8 - unicode glyphs may render incorrectly"
    );
    assert!(check.details.contains(&"effective locale: C".to_string()));
    assert_eq!(
        check.issues[0].remedy.as_deref(),
        Some("export LANG=en_US.UTF-8 or another UTF-8 locale")
    );
}

#[test]
fn terminal_check_prioritizes_unreadable_terminfo_over_warnings() {
    let tempdir = tempfile::tempdir().expect("create tempdir");
    let missing = tempdir.path().join("missing-terminfo");
    let mut inputs = terminal_inputs();
    inputs.info.name = TerminalName::Dumb;
    inputs.stdin_is_terminal = false;
    inputs.stdout_is_terminal = false;
    set_terminal_env(&mut inputs, "TERM", "dumb");
    set_terminal_env(&mut inputs, "LANG", "C");
    set_terminal_env(&mut inputs, "TERMINFO", &missing.to_string_lossy());

    let check = terminal_check_from_inputs(inputs);

    assert_eq!(check.status, CheckStatus::Fail);
    assert_eq!(
        check.summary,
        "TERMINFO unreadable - terminal capabilities are unknown"
    );
    assert!(
        check
            .details
            .iter()
            .any(|detail| detail.starts_with("TERMINFO: ") && detail.ends_with(" (missing)"))
    );
    assert_eq!(
        check.issues[0].remedy.as_deref(),
        Some("check that $TERMINFO points to a readable directory")
    );
    insta::assert_snapshot!(render_human_report(
        &DoctorReport {
            schema_version: 1,
            generated_at: "0s since unix epoch".to_string(),
            overall_status: check.status,
            codex_version: "0.0.0".to_string(),
            checks: vec![check],
        },
        HumanOutputOptions {
            show_details: false,
            show_all: false,
            ascii: true,
            color_enabled: false,
        }
    ));
}

#[test]
fn terminal_check_reports_remote_indicators_as_present_only() {
    let mut inputs = terminal_inputs();
    set_terminal_env(&mut inputs, "SSH_CONNECTION", "10.0.0.1 1 10.0.0.2 22");

    let check = terminal_check_from_inputs(inputs);

    assert!(
        check
            .details
            .contains(&"SSH_CONNECTION: present".to_string())
    );
    assert!(
        !check
            .details
            .iter()
            .any(|detail| detail.contains("10.0.0.1"))
    );
}

#[test]
fn terminal_check_includes_windows_console_details() {
    let mut inputs = terminal_inputs();
    inputs
        .windows_console_details
        .push("stdout console mode: 0x00000004 (VT processing: true)".to_string());

    let check = terminal_check_from_inputs(inputs);

    assert!(
        check
            .details
            .contains(&"stdout console mode: 0x00000004 (VT processing: true)".to_string())
    );
}

#[test]
fn terminal_check_allows_missing_tmux_details() {
    let mut inputs = terminal_inputs();
    inputs.info.multiplexer = Some(Multiplexer::Tmux { version: None });

    let check = terminal_check_from_inputs(inputs);

    assert_eq!(check.status, CheckStatus::Ok);
    assert_eq!(check.summary, "terminal metadata was detected");
}

#[test]
fn color_output_summary_reports_disabled_reasons() {
    let mut inputs = terminal_inputs();
    inputs.no_color_flag = true;
    assert_eq!(color_output_summary(&inputs), "disabled (--no-color)");

    inputs = terminal_inputs();
    set_terminal_env(&mut inputs, "NO_COLOR", "");
    assert_eq!(color_output_summary(&inputs), "disabled (NO_COLOR)");

    inputs = terminal_inputs();
    inputs.info.term = Some("dumb".to_string());
    set_terminal_env(&mut inputs, "TERM", "dumb");
    assert_eq!(color_output_summary(&inputs), "disabled (TERM=dumb)");

    inputs = terminal_inputs();
    inputs.stdout_is_terminal = false;
    assert_eq!(
        color_output_summary(&inputs),
        "disabled (stdout is not a terminal)"
    );
}
