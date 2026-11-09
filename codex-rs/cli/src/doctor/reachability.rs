//! Reachability checks: Responses WebSocket handshake and provider HTTP endpoint probes.

use super::*;

pub(super) async fn websocket_reachability_check(
    config: &Config,
    auth_manager: Option<Arc<AuthManager>>,
) -> DoctorCheck {
    let provider = &config.model_provider;
    let mut details = vec![
        format!("model provider: {}", config.model_provider_id),
        format!("provider name: {}", provider.name),
        format!("wire API: {}", provider.wire_api),
        format!("supports websockets: {}", provider.supports_websockets),
    ];
    push_proxy_env_details(&mut details);

    if !provider.supports_websockets {
        return DoctorCheck::new(
            "network.websocket_reachability",
            "websocket",
            CheckStatus::Ok,
            "Responses WebSocket is not enabled for the active provider",
        )
        .details(details);
    }

    details.push(format!(
        "connect timeout: {} ms",
        provider.websocket_connect_timeout().as_millis()
    ));

    let setup = async {
        let runtime_auth = if config.features.enabled(Feature::RuntimeAgentInstances)
            && let Some(credentials) = &auth_manager
        {
            let installation_id = codex_core::resolve_installation_id(&config.codex_home).await?;
            codex_core::build_runtime_auth_manager(
                config,
                Arc::clone(credentials),
                &installation_id,
            )
            .map(|runtime| (runtime, config.runtime_auth_mode))
        } else {
            None
        };
        let runtime_provider = match auth_manager {
            Some(credentials) => codex_model_provider::ProviderAuthContext {
                credentials,
                runtime_auth,
            }
            .model_provider(provider.clone()),
            None => create_model_provider(provider.clone(), /*auth_manager*/ None),
        };
        codex_model_provider::resolve_request(
            runtime_provider.as_ref(),
            /*scope*/ None,
            Some(&config.workspace_routing_context()),
            config.http_client_factory(),
        )
        .await
    }
    .await;
    let setup = match setup {
        Ok(setup) => setup,
        Err(err) => {
            return websocket_probe_warning(
                "Responses WebSocket provider setup failed",
                details,
                format!("provider setup failed: {err}"),
            );
        }
    };
    details.push(format!(
        "auth mode: {}",
        setup.auth.as_ref().map(auth_mode_name).unwrap_or("none")
    ));
    let api_provider = setup.api_provider;
    match api_provider.websocket_url_for_path("responses") {
        Ok(url) => {
            details.push(format!("endpoint: {url}"));
            if let Some(host) = url.host_str()
                && let Some(port) = url.port_or_known_default()
            {
                details.extend(dns_address_family_details(host, port).await);
            }
        }
        Err(err) => {
            return websocket_probe_warning(
                "Responses WebSocket endpoint could not be built",
                details,
                format!("endpoint build failed: {err}"),
            );
        }
    }

    let mut extra_headers = HeaderMap::new();
    extra_headers.insert(
        OPENAI_BETA_HEADER,
        HeaderValue::from_static(RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE),
    );
    let client = ResponsesWebsocketClient::new(api_provider, setup.api_auth);
    let http_client_factory = setup.http_client_factory;
    match tokio::time::timeout(
        provider.websocket_connect_timeout(),
        client.probe_handshake(
            &http_client_factory,
            extra_headers,
            default_headers(),
            WEBSOCKET_IMMEDIATE_CLOSE_GRACE,
        ),
    )
    .await
    {
        Ok(Ok(probe)) => {
            details.push(format!("handshake result: HTTP {}", probe.status));
            details.push(format!("reasoning header: {}", probe.reasoning_included));
            details.push(format!(
                "server model present: {}",
                probe.server_model_present
            ));
            if let Some(close) = probe.immediate_close {
                details.push(format!("immediate close code: {}", close.code));
                details.push(format!(
                    "immediate close reason: {}",
                    close_reason_detail(&close.reason)
                ));
                return DoctorCheck::new(
                    "network.websocket_reachability",
                    "websocket",
                    CheckStatus::Warning,
                    "Responses WebSocket closed immediately after handshake",
                )
                .details(details)
                .remediation(
                    "Check proxy, VPN, firewall, DNS, custom CA, and WebSocket policy support.",
                );
            }
            DoctorCheck::new(
                "network.websocket_reachability",
                "websocket",
                CheckStatus::Ok,
                "Responses WebSocket handshake succeeded",
            )
            .details(details)
        }
        Ok(Err(err)) => websocket_probe_warning(
            "Responses WebSocket failed; HTTPS fallback may still work",
            details,
            websocket_error_detail(&err.error),
        ),
        Err(_) => websocket_probe_warning(
            "Responses WebSocket timed out; HTTPS fallback may still work",
            details,
            "handshake timed out".to_string(),
        ),
    }
}

pub(super) fn websocket_probe_warning(
    summary: &'static str,
    mut details: Vec<String>,
    error_detail: String,
) -> DoctorCheck {
    details.push(error_detail);
    DoctorCheck::new(
        "network.websocket_reachability",
        "websocket",
        CheckStatus::Warning,
        summary,
    )
    .details(details)
    .remediation("Check proxy, VPN, firewall, DNS, custom CA, and WebSocket policy support.")
}

pub(super) fn websocket_error_detail(err: &ApiError) -> String {
    match err {
        ApiError::Transport(transport) => {
            let detail = match transport {
                // Policy denials contain only fixed descriptions, without wire values.
                TransportError::Policy(denied) => denied.to_string(),
                TransportError::Http { status, .. } => format!("http {status}: [redacted]"),
                TransportError::RetryLimit => "retry limit reached".to_string(),
                TransportError::Timeout => "timeout".to_string(),
                TransportError::Connection(_) => "connection failed: [redacted]".to_string(),
                TransportError::Network(_) => "network error: [redacted]".to_string(),
                TransportError::Build(_) => "request build error: [redacted]".to_string(),
                TransportError::ResponseTooLarge { max_bytes } => {
                    format!("response body exceeds the {max_bytes} byte limit")
                }
            };
            format!("handshake transport error: {detail}")
        }
        ApiError::Api { status, .. } => format!("handshake API error: {status} [redacted]"),
        ApiError::Stream(_) => "handshake stream error: [redacted]".to_string(),
        ApiError::ResponseIncomplete(_) => {
            "handshake error: incomplete response: [redacted]".to_string()
        }
        // These variants format fixed text; retry metadata is typed and never printed.
        ApiError::ContentFilter
        | ApiError::ContextWindowExceeded
        | ApiError::QuotaExceeded
        | ApiError::UsageNotIncluded
        | ApiError::SessionBudgetExceeded
        | ApiError::FlexUnavailable
        | ApiError::ServerOverloaded { .. }
        | ApiError::PolicyCapacityUnavailable { .. } => format!("handshake error: {err}"),
        ApiError::Retryable { .. } => "handshake error: retryable error: [redacted]".to_string(),
        ApiError::RateLimitExceeded { .. } => {
            "handshake error: rate limit exceeded: [redacted]".to_string()
        }
        ApiError::RateLimit(_) => "handshake error: rate limit: [redacted]".to_string(),
        ApiError::InvalidRequest { .. } => {
            "handshake error: invalid request: [redacted]".to_string()
        }
        ApiError::InvalidAudioInput { .. } => {
            "handshake error: invalid audio input: [redacted]".to_string()
        }
        ApiError::ModelNotFound { .. } => {
            "handshake error: model not found: [redacted]".to_string()
        }
        ApiError::InvalidPrompt { .. } => "handshake error: invalid prompt: [redacted]".to_string(),
        ApiError::CyberPolicy { .. } => "handshake error: cyber policy: [redacted]".to_string(),
        ApiError::BioPolicy { .. } => "handshake error: bio policy: [redacted]".to_string(),
        ApiError::MisalignmentPolicyViolation { .. } => {
            "handshake error: misalignment policy violation: [redacted]".to_string()
        }
    }
}

pub(super) fn close_reason_detail(reason: &str) -> &'static str {
    if reason.is_empty() {
        "empty"
    } else {
        "present (redacted)"
    }
}

fn auth_mode_name(auth: &CodexAuth) -> &'static str {
    match auth.auth_mode() {
        AuthMode::ApiKey => "api_key",
        AuthMode::Chatgpt => "chatgpt",
        AuthMode::ChatgptAuthTokens => "chatgpt_auth_tokens",
        AuthMode::Headers => "headers",
        AuthMode::AgentIdentity => "agent_identity",
        AuthMode::PersonalAccessToken => "personal_access_token",
        AuthMode::BedrockApiKey => "bedrock_api_key",
        AuthMode::BedrockAccessKeys => "bedrock_access_keys",
    }
}

async fn dns_address_family_details(host: &str, port: u16) -> Vec<String> {
    match tokio::net::lookup_host((host, port)).await {
        Ok(addresses) => {
            let addresses = addresses.collect::<Vec<_>>();
            let ipv4_count = addresses
                .iter()
                .filter(|address| matches!(address.ip(), IpAddr::V4(_)))
                .count();
            let ipv6_count = addresses
                .iter()
                .filter(|address| matches!(address.ip(), IpAddr::V6(_)))
                .count();
            let first_family = addresses
                .first()
                .map(|address| match address.ip() {
                    IpAddr::V4(_) => "IPv4",
                    IpAddr::V6(_) => "IPv6",
                })
                .unwrap_or("none");
            vec![format!(
                "DNS: {ipv4_count} IPv4, {ipv6_count} IPv6, first {first_family}"
            )]
        }
        Err(err) => vec![format!("DNS: lookup failed ({err})")],
    }
}

impl ProviderAuthReachabilityMode {
    fn description(self) -> &'static str {
        match self {
            Self::NotRequired => "provider auth",
            Self::ApiKey => "API key auth",
            Self::Chatgpt => "ChatGPT auth",
        }
    }
}

pub(super) fn provider_reachability_plan(config: &Config) -> ReachabilityPlan {
    let query_params = config.model_provider.query_params.as_ref().map(|params| {
        params
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().to_owned()))
            .collect::<HashMap<_, _>>()
    });
    let stored_auth = load_auth_dot_json(
        &config.codex_home,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    )
    .ok()
    .flatten();
    let mode = provider_auth_reachability_mode_from_auth(
        config.model_provider.requires_openai_auth,
        config.model_provider.env_key.as_deref(),
        config.model_provider.base_url.as_deref(),
        env_var_present,
        stored_auth.as_ref(),
    );
    let mut plan = provider_reachability_plan_from_parts(
        mode,
        &config.model_provider_id,
        &config.model_provider.name,
        config.model_provider.base_url.as_deref(),
        query_params.as_ref(),
        config.model_provider.is_amazon_bedrock(),
        &config.chatgpt_base_url,
    );
    plan.http_client_factory = config.http_client_factory();
    plan
}

pub(super) fn default_reachability_plan() -> ReachabilityPlan {
    provider_reachability_plan_from_parts(
        ProviderAuthReachabilityMode::Chatgpt,
        "openai",
        "OpenAI",
        /*provider_base_url*/ None,
        /*provider_query_params*/ None,
        /*is_amazon_bedrock*/ false,
        "https://chatgpt.com/backend-api/",
    )
}

pub(super) fn provider_auth_reachability_mode_from_auth(
    requires_openai_auth: bool,
    provider_env_key: Option<&str>,
    provider_base_url: Option<&str>,
    env_var_present: impl Fn(&str) -> bool,
    stored_auth: Option<&AuthDotJson>,
) -> ProviderAuthReachabilityMode {
    if !requires_openai_auth {
        return ProviderAuthReachabilityMode::NotRequired;
    }
    if provider_base_url.is_some_and(|url| !url.trim().is_empty())
        && provider_env_key
            .is_some_and(|env_key| !env_key.trim().is_empty() && env_var_present(env_key))
        || env_var_present(CODEX_API_KEY_ENV_VAR)
    {
        return ProviderAuthReachabilityMode::ApiKey;
    }
    if env_var_present(CODEX_ACCESS_TOKEN_ENV_VAR) {
        return ProviderAuthReachabilityMode::Chatgpt;
    }
    match stored_auth.map(stored_auth_mode_value) {
        Some(AuthMode::ApiKey | AuthMode::BedrockApiKey | AuthMode::BedrockAccessKeys) => {
            ProviderAuthReachabilityMode::ApiKey
        }
        Some(
            AuthMode::Chatgpt
            | AuthMode::ChatgptAuthTokens
            | AuthMode::Headers
            | AuthMode::AgentIdentity
            | AuthMode::PersonalAccessToken,
        )
        | None => ProviderAuthReachabilityMode::Chatgpt,
    }
}

pub(super) fn provider_reachability_plan_from_parts(
    mode: ProviderAuthReachabilityMode,
    provider_id: &str,
    provider_name: &str,
    provider_base_url: Option<&str>,
    provider_query_params: Option<&HashMap<String, String>>,
    is_amazon_bedrock: bool,
    chatgpt_base_url: &str,
) -> ReachabilityPlan {
    let provider_route_probe_url = provider_base_url
        .or_else(|| {
            (mode == ProviderAuthReachabilityMode::ApiKey).then_some("https://api.openai.com/v1")
        })
        .and_then(|url| {
            should_probe_models_route(provider_name, url, is_amazon_bedrock)
                .then(|| provider_url_for_path(url, "models", provider_query_params))
        });
    let endpoints = match (mode, provider_base_url) {
        (ProviderAuthReachabilityMode::ApiKey, _) | (_, Some(_)) => vec![ReachabilityEndpoint {
            label: format!("{provider_id} API"),
            url: provider_url_for_path(
                provider_base_url.unwrap_or("https://api.openai.com/v1"),
                "responses",
                provider_query_params,
            ),
            required: true,
            route_probe_url: provider_route_probe_url,
        }],
        (ProviderAuthReachabilityMode::Chatgpt, None) => vec![ReachabilityEndpoint {
            label: "ChatGPT".to_string(),
            url: provider_url_for_path(chatgpt_base_url, "codex/responses", provider_query_params),
            required: true,
            route_probe_url: None,
        }],
        (ProviderAuthReachabilityMode::NotRequired, None) => Vec::new(),
    };
    ReachabilityPlan {
        description: mode.description().to_string(),
        endpoints,
        http_client_factory: HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    }
}

fn should_probe_models_route(provider_name: &str, base_url: &str, is_amazon_bedrock: bool) -> bool {
    !is_amazon_bedrock && !is_azure_responses_provider(provider_name, Some(base_url))
}

fn provider_url_for_path(
    base_url: &str,
    path: &str,
    query_params: Option<&HashMap<String, String>>,
) -> String {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let mut url = if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    };

    if let Some(params) = query_params
        && !params.is_empty()
    {
        let separator = if url.contains('?') { '&' } else { '?' };
        url.push(separator);
        url.push_str(
            &params
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }

    url
}

pub(super) async fn provider_reachability_check(plan: ReachabilityPlan) -> DoctorCheck {
    let mut details = vec![format!("reachability mode: {}", plan.description)];
    if plan.endpoints.is_empty() {
        details.push("active provider endpoint: none configured".to_string());
        return DoctorCheck::new(
            "network.provider_reachability",
            "reachability",
            CheckStatus::Ok,
            "active provider has no HTTP endpoint to probe",
        )
        .details(details);
    }

    let client = RouteAwareClientPool::with_chatgpt_cloudflare_cookies_without_request_logging(
        plan.http_client_factory,
        ClientRouteClass::Api,
    )
    .with_legacy_custom_ca_fallback();
    let mut failures = Vec::new();
    let mut optional_failures = Vec::new();
    let mut route_failures = Vec::new();
    let mut route_warnings = Vec::new();
    let mut issues = Vec::new();
    for endpoint in plan.endpoints {
        match network::probe_status(&client, &endpoint.url, Method::HEAD, default_headers()).await {
            Ok(status) => details.push(format!(
                "{} inference URL: {} reachable (HTTP {status})",
                endpoint.label, endpoint.url
            )),
            Err(err) => {
                let requirement = if endpoint.required {
                    "required"
                } else {
                    "optional"
                };
                details.push(format!(
                    "{} inference URL: {} {err} ({requirement})",
                    endpoint.label, endpoint.url
                ));
                if endpoint.required {
                    failures.push(endpoint.url);
                } else {
                    optional_failures.push(endpoint.url);
                }
                continue;
            }
        }

        let Some(route_probe_url) = endpoint.route_probe_url.as_deref() else {
            continue;
        };
        match provider_route_probe_url(&client, route_probe_url).await {
            RouteProbeOutcome::Ok(status) => {
                details.push(format!(
                    "{} route probe: {route_probe_url} route exists ({status})",
                    endpoint.label,
                ));
            }
            RouteProbeOutcome::Warning(status) => {
                details.push(format!(
                    "{} route probe: {route_probe_url} returned {status} (warning)",
                    endpoint.label,
                ));
                route_warnings.push(route_probe_url.to_string());
            }
            RouteProbeOutcome::Fail(status) => {
                details.push(format!(
                    "{} route probe: {route_probe_url} returned {status} (required)",
                    endpoint.label,
                ));
                route_failures.push(route_probe_url.to_string());
                issues.push(
                    DoctorIssue::new(
                        CheckStatus::Fail,
                        "provider base URL route returned 404 - verify the configured API prefix",
                    )
                    .measured(format!("{route_probe_url} returned {status}"))
                    .expected("GET /models returns 2xx, 401, or 403")
                    .remedy("Set base_url to the provider API root, for example https://api.openai.com/v1")
                    .field("route probe"),
                );
            }
            RouteProbeOutcome::TransportError(err) => {
                details.push(format!(
                    "{} route probe: {route_probe_url} {err} (required)",
                    endpoint.label,
                ));
                route_failures.push(route_probe_url.to_string());
                issues.push(
                    DoctorIssue::new(
                        CheckStatus::Fail,
                        "provider route probe could not connect - verify network access to the provider API",
                    )
                    .measured(format!("{route_probe_url} {err}"))
                    .expected("GET /models completes")
                    .remedy("Check proxy, VPN, firewall, DNS, and custom CA configuration.")
                    .field("route probe"),
                );
            }
        }
    }

    let (status, summary) = provider_reachability_outcome(
        failures.len() + route_failures.len(),
        optional_failures.len() + route_warnings.len(),
    );
    let mut check = DoctorCheck::new(
        "network.provider_reachability",
        "reachability",
        status,
        summary,
    )
    .details(details);
    for issue in issues {
        check = check.issue(issue);
    }
    if status != CheckStatus::Ok {
        check = check.remediation("Check proxy, VPN, firewall, DNS, and custom CA configuration.");
    }
    check
}

async fn provider_route_probe_url(client: &RouteAwareClientPool, url: &str) -> RouteProbeOutcome {
    match network::probe_status(client, url, Method::GET, default_headers()).await {
        Ok(status) if (200..300).contains(&status) || matches!(status, 401 | 403) => {
            RouteProbeOutcome::Ok(format!("HTTP {status}"))
        }
        Ok(404) => RouteProbeOutcome::Fail("HTTP 404".to_string()),
        Ok(status) => RouteProbeOutcome::Warning(format!("HTTP {status}")),
        Err(err) => RouteProbeOutcome::TransportError(err),
    }
}

pub(super) fn provider_reachability_outcome(
    required_failures: usize,
    warnings: usize,
) -> (CheckStatus, &'static str) {
    match (required_failures, warnings) {
        (0, 0) => (
            CheckStatus::Ok,
            "active provider endpoints are reachable over HTTP",
        ),
        (0, _) => (
            CheckStatus::Warning,
            "provider endpoint checks returned warnings",
        ),
        (_, _) => (
            CheckStatus::Fail,
            "one or more required provider endpoints are unreachable over HTTP",
        ),
    }
}
