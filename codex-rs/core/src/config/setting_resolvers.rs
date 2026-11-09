//! Resolvers that derive individual `Config` settings from `ConfigToml` and requirements.

use super::*;

pub(super) fn resolve_sqlite_home_env(resolved_cwd: &Path) -> Option<AbsolutePathBuf> {
    let raw = std::env::var(codex_state::SQLITE_HOME_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(AbsolutePathBuf::resolve_path_against_base(
        trimmed,
        resolved_cwd,
    ))
}

pub(super) fn resolve_cli_auth_credentials_store_mode(
    configured: AuthCredentialsStoreMode,
    package_version: &str,
) -> AuthCredentialsStoreMode {
    match (package_version, configured) {
        (
            LOCAL_DEV_BUILD_VERSION,
            AuthCredentialsStoreMode::Keyring | AuthCredentialsStoreMode::Auto,
        ) => AuthCredentialsStoreMode::File,
        (_, mode) => mode,
    }
}

pub(super) fn resolve_mcp_oauth_credentials_store_mode(
    configured: OAuthCredentialsStoreMode,
    package_version: &str,
) -> OAuthCredentialsStoreMode {
    match (package_version, configured) {
        (
            LOCAL_DEV_BUILD_VERSION,
            OAuthCredentialsStoreMode::Keyring | OAuthCredentialsStoreMode::Auto,
        ) => OAuthCredentialsStoreMode::File,
        (_, mode) => mode,
    }
}

pub(super) fn apply_requirement_constrained_value<T>(
    field_name: &'static str,
    configured_value: T,
    constrained_value: &mut ConstrainedWithSource<T>,
    startup_warnings: &mut Vec<String>,
) -> std::io::Result<bool>
where
    T: Clone + std::fmt::Debug + Send + Sync,
{
    if let Err(err) = constrained_value.set(configured_value) {
        let fallback_value = constrained_value.get().clone();
        tracing::warn!(
            error = %err,
            ?fallback_value,
            requirement_source = ?constrained_value.source,
            "configured value is disallowed by requirements; falling back to required value for {field_name}"
        );
        let message = format!(
            "Configured value for `{field_name}` is disallowed by requirements; falling back to required value {fallback_value:?}. Details: {err}"
        );
        startup_warnings.push(message);

        constrained_value.set(fallback_value).map_err(|fallback_err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "configured value for `{field_name}` is disallowed by requirements ({err}); fallback to a requirement-compliant value also failed ({fallback_err})"
                ),
            )
        })?;
        return Ok(true);
    }

    Ok(false)
}

pub(super) fn resolve_tool_suggest_config(
    config_toml: &ConfigToml,
    config_layer_stack: &ConfigLayerStack,
) -> ToolSuggestConfig {
    resolve_tool_suggest_config_from_config(config_toml.tool_suggest.as_ref(), config_layer_stack)
}

pub(crate) fn resolve_tool_suggest_config_from_layer_stack(
    config_layer_stack: &ConfigLayerStack,
) -> ToolSuggestConfig {
    let tool_suggest = config_layer_stack
        .effective_config()
        .get("tool_suggest")
        .cloned()
        .and_then(|value| value.try_into::<ToolSuggestConfig>().ok());
    resolve_tool_suggest_config_from_config(tool_suggest.as_ref(), config_layer_stack)
}

fn resolve_tool_suggest_config_from_config(
    tool_suggest: Option<&ToolSuggestConfig>,
    config_layer_stack: &ConfigLayerStack,
) -> ToolSuggestConfig {
    let discoverables = tool_suggest
        .into_iter()
        .flat_map(|tool_suggest| tool_suggest.discoverables.iter())
        .filter_map(|discoverable| {
            let trimmed = discoverable.id.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(ToolSuggestDiscoverable {
                    kind: discoverable.kind,
                    id: trimmed.to_string(),
                })
            }
        })
        .collect();
    let mut seen_disabled_tools = HashSet::new();
    let mut disabled_tools = Vec::new();
    let mut add_disabled_tool = |disabled_tool: ToolSuggestDisabledTool| {
        if let Some(disabled_tool) = disabled_tool.normalized()
            && seen_disabled_tools.insert(disabled_tool.clone())
        {
            disabled_tools.push(disabled_tool);
        }
    };

    let mut layers = config_layer_stack.layers_low_to_high().peekable();
    if layers.peek().is_none() {
        for disabled_tool in tool_suggest
            .into_iter()
            .flat_map(|tool_suggest| tool_suggest.disabled_tools.iter().cloned())
        {
            add_disabled_tool(disabled_tool);
        }
    } else {
        for layer in layers {
            let Some(tool_suggest) = layer
                .config
                .get("tool_suggest")
                .cloned()
                .and_then(|value| value.try_into::<ToolSuggestConfig>().ok())
            else {
                continue;
            };
            for disabled_tool in tool_suggest.disabled_tools {
                add_disabled_tool(disabled_tool);
            }
        }
    }

    ToolSuggestConfig {
        discoverables,
        disabled_tools,
    }
}

pub(super) fn thread_store_config(thread_store: Option<ThreadStoreToml>) -> ThreadStoreConfig {
    match thread_store {
        Some(ThreadStoreToml::Local {}) => ThreadStoreConfig::Local,
        Some(ThreadStoreToml::InMemory { id }) => ThreadStoreConfig::InMemory { id },
        None => ThreadStoreConfig::Local,
    }
}

pub(super) fn dedupe_absolute_paths(paths: &mut Vec<AbsolutePathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

/// Resolves the OSS provider from CLI override or global config.
/// Returns `None` if no provider is configured at any level.
pub fn resolve_oss_provider(
    explicit_provider: Option<&str>,
    config_toml: &ConfigToml,
) -> Option<String> {
    if let Some(provider) = explicit_provider {
        // Explicit provider specified (e.g., via --local-provider)
        Some(provider.to_string())
    } else {
        config_toml.oss_provider.clone()
    }
}

pub(super) fn resolve_web_search_config(config_toml: &ConfigToml) -> Option<WebSearchConfig> {
    config_toml
        .tools
        .as_ref()
        .and_then(|tools| tools.web_search.as_ref())
        .cloned()
        .map(Into::into)
}

pub(super) fn resolve_experimental_request_user_input_enabled(config_toml: &ConfigToml) -> bool {
    config_toml
        .tools
        .as_ref()
        .and_then(|tools| tools.experimental_request_user_input.as_ref())
        .is_none_or(|config| config.enabled)
}

pub(super) fn resolve_update_plan_enabled(config_toml: &ConfigToml) -> bool {
    config_toml
        .tools
        .as_ref()
        .and_then(|tools| tools.update_plan.as_ref())
        .is_some_and(|config| config.enabled)
}

/// Bootstrap-only resolver for the cloud-config fetch.
///
/// Call before a cloud-config bundle is available. Final [`Config`] loading
/// resolves the effective feature value after all layers are available.
pub fn resolve_bootstrap_respect_system_proxy(
    cfg: &ConfigToml,
    feature_requirements: Option<&Sourced<FeatureRequirementsToml>>,
) -> std::io::Result<bool> {
    resolve_bootstrap_http_client_factory(cfg, feature_requirements)
        .map(|factory| factory.outbound_proxy_policy() == OutboundProxyPolicy::RespectSystemProxy)
}

/// Resolves auth route settings for the initial cloud-config bootstrap.
pub fn resolve_bootstrap_auth_route_config(
    cfg: &ConfigToml,
    feature_requirements: Option<&Sourced<FeatureRequirementsToml>>,
) -> std::io::Result<AuthRouteConfig> {
    resolve_bootstrap_http_client_factory(cfg, feature_requirements)
        .map(AuthRouteConfig::from_http_client_factory)
}

/// Resolves shared HTTP routing for startup work that runs before final [`Config`] loading.
pub fn resolve_bootstrap_http_client_factory(
    cfg: &ConfigToml,
    feature_requirements: Option<&Sourced<FeatureRequirementsToml>>,
) -> std::io::Result<HttpClientFactory> {
    let configured_features = Features::from_sources(
        FeatureConfigSource {
            features: cfg.features.as_ref(),
            experimental_use_unified_exec_tool: cfg.experimental_use_unified_exec_tool,
        },
        FeatureConfigSource::default(),
        FeatureOverrides::default(),
    );
    let features =
        ManagedFeatures::from_configured(configured_features, feature_requirements.cloned())?;
    let outbound_proxy_policy = if features.enabled(Feature::RespectSystemProxy) {
        OutboundProxyPolicy::RespectSystemProxy
    } else {
        OutboundProxyPolicy::ReqwestDefault
    };
    let mut factory = HttpClientFactory::new(outbound_proxy_policy);
    if outbound_proxy_policy == OutboundProxyPolicy::ReqwestDefault
        && features.enabled(Feature::SystemProxyFallback)
    {
        factory = factory.with_system_proxy_fallback();
    }
    Ok(factory)
}

pub(crate) fn resolve_web_search_mode_for_turn(
    web_search_mode: &Constrained<WebSearchMode>,
    permission_profile: &PermissionProfile,
    provider_capabilities: ProviderCapabilities,
) -> WebSearchMode {
    let preferred = web_search_mode.value();
    let is_allowed = |mode: WebSearchMode| {
        let provider_supports_mode = match mode {
            WebSearchMode::Live | WebSearchMode::Indexed => {
                provider_capabilities.external_web_access
            }
            WebSearchMode::Cached | WebSearchMode::Disabled => true,
        };

        provider_supports_mode && web_search_mode.can_set(&mode).is_ok()
    };

    if matches!(permission_profile, PermissionProfile::Disabled)
        && !matches!(preferred, WebSearchMode::Disabled | WebSearchMode::Indexed)
    {
        for mode in [
            WebSearchMode::Live,
            WebSearchMode::Cached,
            WebSearchMode::Disabled,
        ] {
            if is_allowed(mode) {
                return mode;
            }
        }
    } else {
        if is_allowed(preferred) {
            return preferred;
        }
        for mode in [
            WebSearchMode::Cached,
            WebSearchMode::Live,
            WebSearchMode::Disabled,
        ] {
            if is_allowed(mode) {
                return mode;
            }
        }
    }

    WebSearchMode::Disabled
}

pub(super) fn guardian_policy_config_from_requirements(
    requirements_toml: &ConfigRequirementsToml,
) -> Option<String> {
    normalize_guardian_policy_config(requirements_toml.guardian_policy_config.as_deref())
}

pub(super) fn normalize_guardian_policy_config(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}
