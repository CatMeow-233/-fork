//! Resolves credentials from the environment and auth storage, applying login restrictions.

use super::*;

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_codex_api_key_from_env() -> Option<String> {
    read_non_empty_env_var(CODEX_API_KEY_ENV_VAR)
}

pub fn read_codex_access_token_from_env() -> Option<String> {
    read_non_empty_env_var(CODEX_ACCESS_TOKEN_ENV_VAR)
}

fn read_non_empty_env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn ensure_auth_workspace_allowed(
    expected_workspace_ids: Option<&[String]>,
    account_id: &str,
) -> std::io::Result<()> {
    crate::server::ensure_workspace_account_allowed(expected_workspace_ids, account_id)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::PermissionDenied, message))
}

fn ensure_agent_identity_workspace_allowed(
    expected_workspace_ids: Option<&[String]>,
    agent_identity: &AgentIdentityStorage,
) -> std::io::Result<()> {
    let Some(expected_workspace_ids) = expected_workspace_ids else {
        return Ok(());
    };

    match agent_identity {
        AgentIdentityStorage::Jwt(jwt) => {
            let record = AgentIdentityAuthRecord::from_agent_identity_jwt(jwt)?;
            ensure_auth_workspace_allowed(Some(expected_workspace_ids), &record.account_id)
        }
        AgentIdentityStorage::Record(record) => {
            ensure_auth_workspace_allowed(Some(expected_workspace_ids), &record.account_id)
        }
    }
}

impl AuthConfig {
    pub fn is_login_method_allowed(&self, method: ForcedLoginMethod) -> bool {
        self.managed_auth_policy.allows_login_method(
            method,
            self.forced_login_method,
            self.forced_chatgpt_workspace_id.as_deref(),
        )
    }

    pub fn effective_chatgpt_workspaces(&self) -> Option<Vec<String>> {
        self.managed_auth_policy
            .effective_chatgpt_workspaces(self.forced_chatgpt_workspace_id.as_deref())
    }

    pub fn validate(&self) -> std::io::Result<()> {
        if self.is_login_method_allowed(ForcedLoginMethod::Api)
            || self.is_login_method_allowed(ForcedLoginMethod::Chatgpt)
        {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "authentication requirements do not permit any usable login method",
            ))
        }
    }

    pub fn allows_auth(&self, auth: &CodexAuth) -> bool {
        let allowed_login_methods = self.allowed_login_methods();
        let workspaces = self.effective_chatgpt_workspaces();
        validate_auth_restrictions(Some(&allowed_login_methods), workspaces.as_deref(), auth)
            .is_ok()
    }

    pub async fn load_auth(
        &self,
        enable_codex_api_key_env: bool,
    ) -> std::io::Result<Option<CodexAuth>> {
        self.load_auth_with_modes(enable_codex_api_key_env, /*allowed_auth_modes*/ None)
            .await
    }

    pub(super) async fn load_auth_with_modes(
        &self,
        enable_codex_api_key_env: bool,
        allowed_auth_modes: Option<&[AuthMode]>,
    ) -> std::io::Result<Option<CodexAuth>> {
        let allowed_login_methods = self.allowed_login_methods();
        let workspaces = self.effective_chatgpt_workspaces();
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(self.chatgpt_base_url.as_deref()).ok();
        let auth = load_auth(
            &self.codex_home,
            enable_codex_api_key_env,
            self.auth_credentials_store_mode,
            Some(&allowed_login_methods),
            workspaces.as_deref(),
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            agent_identity_authapi_base_url.as_deref(),
            &self.auth_route_config,
            /*cached_auth*/ None,
            allowed_auth_modes,
        )
        .await?;
        Ok(auth.filter(|auth| self.allows_auth(auth)))
    }

    fn allowed_login_methods(&self) -> Vec<ForcedLoginMethod> {
        self.managed_auth_policy.allowed_login_methods(
            self.forced_login_method,
            self.forced_chatgpt_workspace_id.as_deref(),
        )
    }
}

fn auth_mode_is_allowed(
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    mode: AuthMode,
) -> bool {
    let method = if mode.uses_codex_backend() {
        ForcedLoginMethod::Chatgpt
    } else {
        ForcedLoginMethod::Api
    };
    allowed_login_methods.is_none_or(|allowed| allowed.contains(&method))
}

pub(super) fn validate_auth_restrictions(
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    expected_workspaces: Option<&[String]>,
    auth: &CodexAuth,
) -> Result<(), String> {
    if !auth_mode_is_allowed(allowed_login_methods, auth.auth_mode()) {
        return Err(match allowed_login_methods {
            Some(methods) if methods.contains(&ForcedLoginMethod::Api) => {
                "API key login is required".to_string()
            }
            Some(_) => "ChatGPT login is required".to_string(),
            None => unreachable!("unrestricted login methods accept every auth mode"),
        });
    }

    let Some(expected_workspaces) = expected_workspaces else {
        return Ok(());
    };
    if matches!(
        auth,
        CodexAuth::ApiKey(_) | CodexAuth::BedrockApiKey(_) | CodexAuth::BedrockAccessKeys(_)
    ) {
        return Ok(());
    }

    let actual_workspace = auth.get_account_id().or_else(|| {
        auth.get_token_data()
            .ok()
            .and_then(|tokens| tokens.id_token.chatgpt_account_id)
    });
    if actual_workspace
        .as_ref()
        .is_some_and(|workspace| expected_workspaces.contains(workspace))
    {
        Ok(())
    } else {
        let actual = actual_workspace.unwrap_or_else(|| "unknown".to_string());
        Err(format!(
            "Login is restricted to workspace(s) {}, but current credentials belong to {actual}",
            expected_workspaces.join(", ")
        ))
    }
}

/// Enforces configured login restrictions using auth-owned HTTP settings.
pub async fn enforce_login_restrictions(config: &AuthConfig) -> std::io::Result<()> {
    let agent_identity_authapi_base_url =
        agent_identity_authapi_base_url(config.chatgpt_base_url.as_deref()).ok();
    enforce_login_restrictions_with_agent_identity_authapi_base_url(
        config,
        agent_identity_authapi_base_url.as_deref(),
    )
    .await
}

pub(super) async fn enforce_login_restrictions_with_agent_identity_authapi_base_url(
    config: &AuthConfig,
    agent_identity_authapi_base_url: Option<&str>,
) -> std::io::Result<()> {
    // Managed-only restrictions are enforced by AuthManager.
    if config.forced_login_method.is_none() && config.forced_chatgpt_workspace_id.is_none() {
        return Ok(());
    }

    let Some(auth) = load_auth(
        &config.codex_home,
        /*enable_codex_api_key_env*/ true,
        config.auth_credentials_store_mode,
        /*allowed_login_methods*/ None,
        /*forced_chatgpt_workspace_id*/ None,
        config.chatgpt_base_url.as_deref(),
        config.keyring_backend_kind,
        agent_identity_authapi_base_url,
        &config.auth_route_config,
        /*cached_auth*/ None,
        /*allowed_auth_modes*/ None,
    )
    .await?
    else {
        return Ok(());
    };

    if let Some(required_method) = config.forced_login_method {
        let method_violation = match (required_method, auth.auth_mode()) {
            (ForcedLoginMethod::Api, AuthMode::ApiKey)
            | (ForcedLoginMethod::Api, AuthMode::BedrockApiKey)
            | (ForcedLoginMethod::Api, AuthMode::BedrockAccessKeys) => None,
            (ForcedLoginMethod::Chatgpt, AuthMode::Chatgpt)
            | (ForcedLoginMethod::Chatgpt, AuthMode::ChatgptAuthTokens)
            | (ForcedLoginMethod::Chatgpt, AuthMode::Headers)
            | (ForcedLoginMethod::Chatgpt, AuthMode::AgentIdentity)
            | (ForcedLoginMethod::Chatgpt, AuthMode::PersonalAccessToken) => None,
            (ForcedLoginMethod::Api, AuthMode::Chatgpt)
            | (ForcedLoginMethod::Api, AuthMode::ChatgptAuthTokens)
            | (ForcedLoginMethod::Api, AuthMode::Headers)
            | (ForcedLoginMethod::Api, AuthMode::AgentIdentity)
            | (ForcedLoginMethod::Api, AuthMode::PersonalAccessToken) => Some(
                "API key login is required, but ChatGPT is currently being used. Logging out."
                    .to_string(),
            ),
            (ForcedLoginMethod::Chatgpt, AuthMode::ApiKey)
            | (ForcedLoginMethod::Chatgpt, AuthMode::BedrockApiKey)
            | (ForcedLoginMethod::Chatgpt, AuthMode::BedrockAccessKeys) => Some(
                "ChatGPT login is required, but an API key is currently being used. Logging out."
                    .to_string(),
            ),
        };

        if let Some(message) = method_violation {
            return logout_with_message(
                &config.codex_home,
                message,
                config.auth_credentials_store_mode,
                config.keyring_backend_kind,
            );
        }
    }

    if let Some(expected_account_ids) = config.forced_chatgpt_workspace_id.as_deref() {
        let chatgpt_account_id = match &auth {
            CodexAuth::ApiKey(_)
            | CodexAuth::BedrockApiKey(_)
            | CodexAuth::BedrockAccessKeys(_) => {
                return Ok(());
            }
            CodexAuth::Headers(_)
            | CodexAuth::AgentIdentity(_)
            | CodexAuth::PersonalAccessToken(_) => auth.get_account_id(),
            CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_) => {
                let token_data = match auth.get_token_data() {
                    Ok(data) => data,
                    Err(err) => {
                        return logout_with_message(
                            &config.codex_home,
                            format!(
                                "Failed to load ChatGPT credentials while enforcing workspace restrictions: {err}. Logging out."
                            ),
                            config.auth_credentials_store_mode,
                            config.keyring_backend_kind,
                        );
                    }
                };
                token_data.id_token.chatgpt_account_id
            }
        };

        // workspace is the external identifier for account id.
        let chatgpt_account_id = chatgpt_account_id.as_deref();
        if !chatgpt_account_id.is_some_and(|actual| {
            expected_account_ids
                .iter()
                .any(|expected| expected == actual)
        }) {
            let expected_workspaces = expected_account_ids.join(", ");
            let message = match chatgpt_account_id {
                Some(actual) => format!(
                    "Login is restricted to workspace(s) {expected_workspaces}, but current credentials belong to {actual}. Logging out."
                ),
                None => format!(
                    "Login is restricted to workspace(s) {expected_workspaces}, but current credentials lack a workspace identifier. Logging out."
                ),
            };
            return logout_with_message(
                &config.codex_home,
                message,
                config.auth_credentials_store_mode,
                config.keyring_backend_kind,
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn load_auth(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    allowed_login_methods: Option<&[ForcedLoginMethod]>,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    keyring_backend_kind: AuthKeyringBackendKind,
    agent_identity_authapi_base_url: Option<&str>,
    auth_route_config: &AuthRouteConfig,
    cached_auth: Option<&CodexAuth>,
    allowed_auth_modes: Option<&[AuthMode]>,
) -> std::io::Result<Option<CodexAuth>> {
    let mode_allowed = |mode| allowed_auth_modes.is_none_or(|modes| modes.contains(&mode));
    // API key via env var takes precedence over any other auth method.
    if enable_codex_api_key_env
        && mode_allowed(AuthMode::ApiKey)
        && auth_mode_is_allowed(allowed_login_methods, AuthMode::ApiKey)
        && let Some(api_key) = read_codex_api_key_from_env()
    {
        return Ok(Some(CodexAuth::from_api_key(api_key.as_str())));
    }

    // External ChatGPT auth tokens live in the in-memory (ephemeral) store. Always check this
    // first so external auth takes precedence over any persisted credentials.
    let ephemeral_storage = create_auth_storage(
        codex_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    );
    if let Some(auth_dot_json) = load_stored_auth(ephemeral_storage).await?
        && auth_mode_is_allowed(allowed_login_methods, auth_dot_json.resolved_mode())
    {
        if !mode_allowed(auth_dot_json.resolved_mode()) {
            return Ok(None);
        }
        if let Some(agent_identity) = auth_dot_json.agent_identity.as_ref() {
            ensure_agent_identity_workspace_allowed(forced_chatgpt_workspace_id, agent_identity)?;
        }
        let auth = CodexAuth::from_auth_dot_json(
            codex_home,
            auth_dot_json,
            AuthCredentialsStoreMode::Ephemeral,
            chatgpt_base_url,
            keyring_backend_kind,
            agent_identity_authapi_base_url,
            auth_route_config,
            cached_auth,
        )
        .await?;
        if let CodexAuth::PersonalAccessToken(auth) = &auth {
            ensure_auth_workspace_allowed(forced_chatgpt_workspace_id, auth.account_id())?;
        }
        return Ok(Some(auth));
    }

    if auth_mode_is_allowed(allowed_login_methods, AuthMode::AgentIdentity)
        && let Some(access_token) = read_codex_access_token_from_env()
    {
        return match classify_codex_access_token(&access_token) {
            CodexAccessToken::PersonalAccessToken(access_token) => {
                if !mode_allowed(AuthMode::PersonalAccessToken) {
                    return Ok(None);
                }
                let auth = PersonalAccessTokenAuth::load_with_cached(
                    access_token,
                    auth_route_config,
                    cached_auth,
                )
                .await?;
                ensure_auth_workspace_allowed(forced_chatgpt_workspace_id, auth.account_id())?;
                Ok(Some(CodexAuth::PersonalAccessToken(auth)))
            }
            CodexAccessToken::AgentIdentityJwt(jwt) => {
                if !mode_allowed(AuthMode::AgentIdentity) {
                    return Ok(None);
                }
                let record = AgentIdentityAuthRecord::from_agent_identity_jwt(jwt)?;
                ensure_auth_workspace_allowed(forced_chatgpt_workspace_id, &record.account_id)?;
                CodexAuth::from_agent_identity_jwt_with_authapi_base_url(
                    jwt,
                    chatgpt_base_url,
                    require_agent_identity_authapi_base_url(agent_identity_authapi_base_url)?,
                    auth_route_config,
                )
            }
            .await
            .map(Some),
        };
    }

    // If the caller explicitly requested ephemeral auth, there is no persisted fallback.
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(None);
    }

    // Fall back to the configured persistent store (file/keyring/auto) for managed auth.
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let auth_dot_json = match load_stored_auth(storage).await? {
        Some(auth) => auth,
        None => return Ok(None),
    };
    if !mode_allowed(auth_dot_json.resolved_mode())
        || !auth_mode_is_allowed(allowed_login_methods, auth_dot_json.resolved_mode())
    {
        return Ok(None);
    }
    if let Some(agent_identity) = auth_dot_json.agent_identity.as_ref() {
        ensure_agent_identity_workspace_allowed(forced_chatgpt_workspace_id, agent_identity)?;
    }

    let auth = CodexAuth::from_auth_dot_json(
        codex_home,
        auth_dot_json,
        auth_credentials_store_mode,
        chatgpt_base_url,
        keyring_backend_kind,
        agent_identity_authapi_base_url,
        auth_route_config,
        cached_auth,
    )
    .await?;
    if let CodexAuth::PersonalAccessToken(auth) = &auth {
        ensure_auth_workspace_allowed(forced_chatgpt_workspace_id, auth.account_id())?;
    }
    Ok(Some(auth))
}

// Blocking keyring access must not prevent an async caller's deadline from firing.
async fn load_stored_auth(
    storage: Arc<dyn AuthStorageBackend>,
) -> std::io::Result<Option<AuthDotJson>> {
    let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
    tokio::task::spawn_blocking(move || originator.sync_scope(|| storage.load()))
        .await
        .map_err(|_| std::io::Error::other("credential storage read failed"))?
}

impl AuthDotJson {
    pub(super) fn from_external_access_token(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
    ) -> std::io::Result<Self> {
        let mut token_info =
            parse_chatgpt_jwt_claims(access_token).map_err(std::io::Error::other)?;
        token_info.chatgpt_account_id = Some(chatgpt_account_id.to_string());
        token_info.chatgpt_plan_type = chatgpt_plan_type
            .map(InternalPlanType::from_raw_value)
            .or(token_info.chatgpt_plan_type)
            .or(Some(InternalPlanType::Unknown("unknown".to_string())));
        let tokens = TokenData {
            id_token: token_info,
            access_token: access_token.to_string(),
            refresh_token: String::new(),
            oauth_provenance: None,
            login_owner_id: None,
            account_id: Some(chatgpt_account_id.to_string()),
        };

        Ok(Self {
            auth_mode: Some(AuthMode::ChatgptAuthTokens),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        })
    }

    pub(in crate::auth) fn resolved_mode(&self) -> AuthMode {
        if let Some(mode) = self.auth_mode {
            return mode;
        }
        if self.personal_access_token.is_some() {
            return AuthMode::PersonalAccessToken;
        }
        if self.bedrock_api_key.is_some() {
            return AuthMode::BedrockApiKey;
        }
        if self.bedrock_access_keys.is_some() {
            return AuthMode::BedrockAccessKeys;
        }
        if self.openai_api_key.is_some() {
            return AuthMode::ApiKey;
        }
        AuthMode::Chatgpt
    }

    pub(super) fn storage_mode(
        &self,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> AuthCredentialsStoreMode {
        if self.resolved_mode() == AuthMode::ChatgptAuthTokens {
            AuthCredentialsStoreMode::Ephemeral
        } else {
            auth_credentials_store_mode
        }
    }
}
