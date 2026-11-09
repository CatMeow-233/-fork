//! Read-only `Config` accessors and the `AuthManagerConfig` impl.

use super::*;

impl AuthManagerConfig for Config {
    fn codex_home(&self) -> PathBuf {
        self.codex_home.to_path_buf()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        self.cli_auth_credentials_store_mode
    }

    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind {
        Config::auth_keyring_backend_kind(self)
    }

    fn forced_login_method(&self) -> Option<ForcedLoginMethod> {
        self.forced_login_method
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id.clone()
    }

    fn managed_auth_policy(&self) -> ManagedAuthPolicy {
        self.config_layer_stack.requirements().managed_auth_policy()
    }

    fn chatgpt_base_url(&self) -> String {
        self.chatgpt_base_url.clone()
    }

    fn auth_route_config(&self) -> AuthRouteConfig {
        Config::auth_route_config(self)
    }
}

impl Config {
    /// Capture storage and sync preparation use one session-static rollout choice.
    pub(crate) fn guardian_context_budgeting(&self) -> codex_guardian_context::ContextBudgeting {
        if self.guardian_sync_elastic_budget.is_some() {
            codex_guardian_context::ContextBudgeting::Elastic
        } else {
            codex_guardian_context::ContextBudgeting::Fixed
        }
    }

    pub fn sqlite_config(&self) -> &codex_state::SqliteConfig {
        &self.sqlite
    }

    /// Returns `log_dir` when explicitly set by configuration or managed requirements.
    pub fn configured_log_dir(&self) -> Option<&Path> {
        let configured = self
            .config_layer_stack
            .effective_config()
            .as_table()
            .is_some_and(|table| table.contains_key("log_dir"))
            || self
                .config_layer_stack
                .requirements_toml()
                .log_dir
                .is_some();
        configured.then_some(self.log_dir.as_path())
    }

    /// Resolves the configured, reviewer-catalog, or bundled Guardian policy.
    pub fn resolve_guardian_policy<'a>(
        &'a self,
        model_messages: ResolvedModelMessages<'a>,
    ) -> &'a str {
        self.guardian_policy_config
            .as_deref()
            .unwrap_or(model_messages.auto_review().policy)
    }

    /// Native Guardian agent history requires the reviewed thread's OpenAI provider.
    pub fn guardian_native_multi_agent_history_enabled(&self) -> bool {
        self.model_provider_id == OPENAI_PROVIDER_ID
            && self
                .features
                .enabled(Feature::GuardianNativeMultiAgentHistory)
    }

    pub(crate) fn multi_agent_version_override(&self) -> Option<MultiAgentVersion> {
        if self.features.enabled(Feature::MultiAgentV2) {
            Some(MultiAgentVersion::V2)
        } else if !self.agents_enabled {
            Some(MultiAgentVersion::Disabled)
        } else {
            None
        }
    }

    pub(crate) fn multi_agent_version_from_features(&self) -> MultiAgentVersion {
        self.multi_agent_version_override().unwrap_or_else(|| {
            if self.features.enabled(Feature::Collab) {
                MultiAgentVersion::V1
            } else {
                MultiAgentVersion::Disabled
            }
        })
    }

    pub(crate) fn multi_agent_version_for_model(
        &self,
        model_multi_agent_version: Option<MultiAgentVersion>,
    ) -> MultiAgentVersion {
        self.multi_agent_version_override()
            .or(model_multi_agent_version)
            .unwrap_or_else(|| self.multi_agent_version_from_features())
    }

    pub(crate) fn effective_agent_max_threads(
        &self,
        multi_agent_version: MultiAgentVersion,
    ) -> Option<usize> {
        match multi_agent_version {
            MultiAgentVersion::V2 => Some(
                self.multi_agent_v2
                    .max_concurrent_threads_per_session
                    .saturating_sub(1),
            ),
            MultiAgentVersion::Disabled | MultiAgentVersion::V1 => {
                self.agent_max_threads.or(DEFAULT_AGENT_MAX_THREADS)
            }
        }
    }

    pub fn to_models_manager_config(&self) -> ModelsManagerConfig {
        ModelsManagerConfig {
            model_context_window: self.model_context_window,
            model_auto_compact_token_limit: self.model_auto_compact_token_limit,
            tool_output_token_limit: self.tool_output_token_limit,
            base_instructions: self.base_instructions.clone().filter(|_| {
                !matches!(
                    self.base_instructions_provenance,
                    Some(BaseInstructionsProvenance::Model { .. })
                )
            }),
            personality: self.personality,
            model_catalog: self.model_catalog.clone(),
        }
    }

    /// Returns backend routing that can resolve defaults against the request's auth snapshot.
    pub fn chatgpt_backend_url(&self) -> ChatgptBaseUrl {
        if !self.chatgpt_base_url_is_explicit && self.chatgpt_base_url == DEFAULT_CHATGPT_BASE_URL {
            ChatgptBaseUrl::Default
        } else {
            ChatgptBaseUrl::Configured(self.chatgpt_base_url.clone())
        }
    }

    /// Returns auth routing resolved from the effective feature configuration.
    pub fn auth_route_config(&self) -> AuthRouteConfig {
        self.application_auth_route_config
            .clone()
            .unwrap_or_else(|| {
                AuthRouteConfig::from_http_client_factory(
                    self.http_client_factory()
                        .with_network_policy(self.application_network_policy.clone()),
                )
            })
    }

    /// Creates the HTTP client factory resolved from the effective feature configuration.
    pub fn http_client_factory(&self) -> HttpClientFactory {
        let outbound_proxy_policy = if self.respect_system_proxy {
            OutboundProxyPolicy::RespectSystemProxy
        } else {
            OutboundProxyPolicy::ReqwestDefault
        };
        let mut factory = HttpClientFactory::new(outbound_proxy_policy)
            .with_network_policy(self.application_network_policy.clone());
        if self
            .model_provider
            .gateway_oauth
            .as_ref()
            .is_some_and(GatewayOAuthConfig::uses_credential_export)
        {
            factory = factory.with_sensitive_header_protection();
        }
        if !self.respect_system_proxy && self.features.enabled(Feature::SystemProxyFallback) {
            factory = factory.with_system_proxy_fallback();
        }
        if self.features.enabled(Feature::Psp) {
            factory.with_chatgpt_cookies([HeaderValue::from_static("oai-chat-psp=true")])
        } else {
            factory
        }
    }

    /// Build the plugin-manager input from the effective config.
    pub fn plugins_config_input(&self) -> PluginsConfigInput {
        PluginsConfigInput::new(
            self.config_layer_stack.clone(),
            self.plugins.clone(),
            self.model_provider_id.clone(),
            self.features.enabled(Feature::Plugins),
            self.features.enabled(Feature::RemotePlugin),
            self.chatgpt_base_url.clone(),
            self.http_client_factory(),
            self.apps_mcp_product_sku.clone(),
        )
        .with_runtime_auth_mode(
            (self.features.enabled(Feature::RuntimeAgentInstances)
                && codex_runtime_agent_instance::is_openai_api_url(&self.chatgpt_base_url))
            .then_some(self.runtime_auth_mode),
        )
    }

    pub fn workspace_routing_context(&self) -> codex_model_provider::WorkspaceRoutingContext {
        codex_model_provider::WorkspaceRoutingContext::new(self.chatgpt_base_url.clone())
            .with_session(codex_login::WorkspaceRoutingSession {
                cwd: self.cwd.to_path_buf(),
                config_layer_stack: self.config_layer_stack.clone(),
            })
    }
}

/// Returns the path to the folder where Codex logs are stored. Does not verify
/// that the directory exists.
pub fn log_dir(cfg: &Config) -> std::io::Result<PathBuf> {
    Ok(cfg.log_dir.clone())
}
