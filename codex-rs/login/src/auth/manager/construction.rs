//! Builds `AuthManager` instances and exposes the login policy they enforce.

use super::*;

fn default_agent_identity_authapi_base_url() -> Option<String> {
    agent_identity_authapi_base_url(/*chatgpt_base_url*/ None).ok()
}

impl AuthManager {
    /// Returns the application policy associated with this account owner.
    pub fn application_network_policy(&self) -> codex_http_client::NetworkPolicy {
        self.auth_route_config.application_network_policy().clone()
    }

    /// Creates content clients bound to the account currently owned by this manager.
    pub fn http_client_factory(&self) -> HttpClientFactory {
        self.auth_route_config
            .http_client_factory()
            .clone()
            .with_network_policy(self.application_network_policy().for_current_account())
    }

    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth are swallowed; `auth()` will
    /// simply return `None` in that case so callers can treat it as an
    /// unauthenticated state.
    pub async fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Self {
        Self::new_from_auth_config(
            AuthConfig {
                codex_home,
                auth_credentials_store_mode,
                keyring_backend_kind,
                forced_login_method: None,
                chatgpt_base_url,
                forced_chatgpt_workspace_id,
                managed_auth_policy: ManagedAuthPolicy::default(),
                auth_route_config,
            },
            enable_codex_api_key_env,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    pub(super) async fn new_from_auth_config(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<Vec<AuthMode>>,
    ) -> Self {
        let managed_auth = auth_config
            .load_auth_with_modes(enable_codex_api_key_env, allowed_auth_modes.as_deref())
            .await
            .ok()
            .flatten();
        let AuthConfig {
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            chatgpt_base_url,
            forced_chatgpt_workspace_id,
            managed_auth_policy,
            auth_route_config,
        } = auth_config;
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(chatgpt_base_url.as_deref()).ok();
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Self {
            codex_home,
            inner: RwLock::new(CachedAuth {
                auth: managed_auth,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_login_method,
            forced_chatgpt_workspace_id: RwLock::new(forced_chatgpt_workspace_id),
            managed_auth_policy,
            chatgpt_base_url,
            agent_identity_authapi_base_url,
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            unsettled_chatgpt_refresh: Mutex::new(None),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            workload_identity_selected: false,
            auth_route_config,
            owns_network_policy: true,
            allowed_auth_modes: RwLock::new(allowed_auth_modes),
        }
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        Self::from_optional_auth_for_testing(Some(auth))
    }

    /// Create an AuthManager with an optional CodexAuth, for testing only.
    pub(crate) fn from_optional_auth_for_testing(auth: Option<CodexAuth>) -> Arc<Self> {
        Self::from_cached_auth(
            auth,
            /*external_auth*/ None,
            AuthRuntimeConfig {
                codex_home: PathBuf::from("non-existent"),
                auth_route_config: crate::test_support::transport_default_auth_route_config(),
            },
            /*owns_network_policy*/ true,
        )
    }

    /// An explicitly selected API key with no ambient auth-storage or refresh fallback.
    pub fn from_api_key(api_key: &str, runtime: AuthRuntimeConfig) -> Arc<Self> {
        let auth = CodexAuth::from_api_key(api_key);
        Self::from_cached_auth(
            Some(auth.clone()),
            Some(Arc::new(FixedApiKeyAuth(auth))),
            runtime,
            /*owns_network_policy*/ false,
        )
    }

    /// Give a command-backed credential source the caller's storage and network context.
    pub fn external_bearer_with_runtime(
        config: ModelProviderAuthInfo,
        runtime: AuthRuntimeConfig,
    ) -> Arc<Self> {
        Self::from_cached_auth(
            /*auth*/ None,
            Some(Arc::new(BearerTokenRefresher::new(config, HashMap::new()))),
            runtime,
            /*owns_network_policy*/ false,
        )
    }

    fn from_cached_auth(
        auth: Option<CodexAuth>,
        external_auth: Option<Arc<dyn ExternalAuth>>,
        runtime: AuthRuntimeConfig,
        owns_network_policy: bool,
    ) -> Arc<Self> {
        let cached = CachedAuth {
            auth,
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);

        Arc::new(Self {
            codex_home: runtime.codex_home,
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: if external_auth.is_some() {
                AuthCredentialsStoreMode::Ephemeral
            } else {
                AuthCredentialsStoreMode::File
            },
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: default_agent_identity_authapi_base_url(),
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            unsettled_chatgpt_refresh: Mutex::new(None),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(external_auth),
            workload_identity_selected: false,
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: runtime.auth_route_config,
            owns_network_policy,
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home,
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: default_agent_identity_authapi_base_url(),
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            unsettled_chatgpt_refresh: Mutex::new(None),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            workload_identity_selected: false,
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
            owns_network_policy: true,
        })
    }

    /// Create an AuthManager with a specific CodexAuth and Agent Identity AuthAPI base URL, for testing only.
    #[doc(hidden)]
    pub fn from_auth_for_testing_with_agent_identity_authapi_base_url(
        auth: CodexAuth,
        agent_identity_authapi_base_url: String,
    ) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(cached),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: Some(
                agent_identity_authapi_base_url
                    .trim_end_matches('/')
                    .to_string(),
            ),
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            unsettled_chatgpt_refresh: Mutex::new(None),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            workload_identity_selected: false,
            allowed_auth_modes: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
            owns_network_policy: true,
        })
    }

    pub fn external_bearer_only(config: ModelProviderAuthInfo) -> Arc<Self> {
        Self::external_bearer_only_with_env(config, HashMap::new())
    }

    /// Creates a provider-scoped token cache whose command environment overrides are
    /// retained for both initial authentication and every subsequent refresh.
    pub fn external_bearer_only_with_env(
        config: ModelProviderAuthInfo,
        env: HashMap<String, OsString>,
    ) -> Arc<Self> {
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(CachedAuth {
                auth: None,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            auth_change_state_tx: watch::channel(AuthChangeState::default()).0,
            workspace_routing_resolver: OnceLock::new(),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            forced_chatgpt_workspace_id: RwLock::new(None),
            managed_auth_policy: ManagedAuthPolicy::default(),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: default_agent_identity_authapi_base_url(),
            refresh_lock: Arc::new(Semaphore::new(/*permits*/ 1)),
            unsettled_chatgpt_refresh: Mutex::new(None),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(Some(Arc::new(BearerTokenRefresher::new(config, env)))),
            workload_identity_selected: false,
            allowed_auth_modes: RwLock::new(None),
            owns_network_policy: false,
            // External bearer auth refreshes by running the provider's command and never makes
            // auth-owned HTTP requests, so this route is intentionally inert.
            auth_route_config: AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
        })
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<Vec<String>>) {
        // Fence reloads that validated credentials before this policy change.
        let Ok(_auth_guard) = self.inner.write() else {
            return;
        };
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write()
            && *guard != workspace_id
        {
            *guard = workspace_id;
            self.auth_change_state_tx
                .send_modify(|state| state.generation += 1);
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn effective_chatgpt_workspaces(&self) -> Option<Vec<String>> {
        self.managed_auth_policy
            .effective_chatgpt_workspaces(self.forced_chatgpt_workspace_id().as_deref())
    }

    pub fn is_login_method_allowed(&self, method: ForcedLoginMethod) -> bool {
        self.managed_auth_policy.allows_login_method(
            method,
            self.forced_login_method,
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    /// Returns the login methods permitted by the current effective authentication policy.
    pub fn allowed_login_methods(&self) -> Vec<ForcedLoginMethod> {
        self.managed_auth_policy.allowed_login_methods(
            self.forced_login_method,
            self.forced_chatgpt_workspace_id().as_deref(),
        )
    }

    pub fn is_workload_identity_selected(&self) -> bool {
        self.workload_identity_selected
    }

    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.enable_codex_api_key_env
    }

    /// Returns policy only; independent credential managers own their own state and lifecycle.
    pub fn runtime_config(&self) -> AuthRuntimeConfig {
        AuthRuntimeConfig {
            codex_home: self.codex_home.clone(),
            auth_route_config: self.auth_route_config.clone(),
        }
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub async fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Arc<Self> {
        Arc::new(
            Self::new(
                codex_home,
                enable_codex_api_key_env,
                auth_credentials_store_mode,
                forced_chatgpt_workspace_id,
                chatgpt_base_url,
                keyring_backend_kind,
                auth_route_config,
            )
            .await,
        )
    }

    /// Builds a shared manager and activates process-configured workload identity when selected.
    pub async fn shared_from_config(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config(auth_config_from(config), enable_codex_api_key_env).await
    }

    /// Activates workload identity against an auth config resolved before full runtime config.
    pub async fn shared_from_auth_config(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_modes(
            auth_config,
            enable_codex_api_key_env,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    /// Restricts credential modes before loading or hydrating the initial credential.
    pub async fn shared_from_config_with_auth_modes(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Vec<AuthMode>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_auth_modes(
            auth_config_from(config),
            enable_codex_api_key_env,
            allowed_auth_modes,
        )
        .await
    }

    /// Restricts credential modes during bootstrap, before loading runtime configuration.
    pub async fn shared_from_auth_config_with_auth_modes(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Vec<AuthMode>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        Self::shared_from_auth_config_with_modes(
            auth_config,
            enable_codex_api_key_env,
            Some(allowed_auth_modes),
        )
        .await
    }

    async fn shared_from_auth_config_with_modes(
        auth_config: AuthConfig,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<Vec<AuthMode>>,
    ) -> Result<Arc<Self>, AuthManagerInitializationError> {
        let external_auth = WorkloadIdentityExternalAuth::from_process_config(&auth_config)?;
        let mut manager =
            Self::new_from_auth_config(auth_config, enable_codex_api_key_env, allowed_auth_modes)
                .await;
        manager.workload_identity_selected = external_auth.is_some();
        let manager = Arc::new(manager);
        if let Some(external_auth) = external_auth {
            manager
                .install_external_auth(Arc::new(external_auth))
                .await?;
        }
        Ok(manager)
    }
}

pub(super) fn auth_config_from(config: &impl AuthManagerConfig) -> AuthConfig {
    AuthConfig {
        codex_home: config.codex_home(),
        auth_credentials_store_mode: config.cli_auth_credentials_store_mode(),
        keyring_backend_kind: config.auth_keyring_backend_kind(),
        forced_login_method: config.forced_login_method(),
        chatgpt_base_url: Some(config.chatgpt_base_url()),
        forced_chatgpt_workspace_id: config.forced_chatgpt_workspace_id(),
        managed_auth_policy: config.managed_auth_policy(),
        auth_route_config: config.auth_route_config(),
    }
}
