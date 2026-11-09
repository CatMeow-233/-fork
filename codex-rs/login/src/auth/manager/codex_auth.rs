//! Constructs `CodexAuth` values and reads credential details from them.

use super::*;

impl CodexAuth {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn from_auth_dot_json(
        codex_home: &Path,
        auth_dot_json: AuthDotJson,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        agent_identity_authapi_base_url: Option<&str>,
        auth_route_config: &AuthRouteConfig,
        cached_auth: Option<&CodexAuth>,
    ) -> std::io::Result<Self> {
        let auth_mode = auth_dot_json.resolved_mode();
        if auth_mode == AuthMode::ApiKey {
            let Some(api_key) = auth_dot_json.openai_api_key.as_deref() else {
                return Err(std::io::Error::other("API key auth is missing a key."));
            };
            return Ok(Self::from_api_key(api_key));
        }
        if auth_mode == AuthMode::AgentIdentity {
            let Some(agent_identity) = auth_dot_json.agent_identity.clone() else {
                return Err(std::io::Error::other(
                    "agent identity auth is missing agent identity auth material.",
                ));
            };
            let base_url = chatgpt_base_url
                .unwrap_or(ChatGptEnvironment::default().chatgpt_base_url())
                .trim_end_matches('/')
                .to_string();
            let agent_identity_authapi_base_url =
                require_agent_identity_authapi_base_url(agent_identity_authapi_base_url)?;
            match agent_identity {
                AgentIdentityStorage::Jwt(jwt) => {
                    let auth = AgentIdentityAuth::from_jwt(
                        &jwt,
                        &base_url,
                        agent_identity_authapi_base_url,
                        auth_route_config,
                    )
                    .await?;
                    return Ok(Self::AgentIdentity(auth));
                }
                AgentIdentityStorage::Record(record) => {
                    let auth = AgentIdentityAuth::from_record(
                        record,
                        agent_identity_authapi_base_url,
                        auth_route_config,
                    )
                    .await?;
                    return Ok(Self::AgentIdentity(auth));
                }
            }
        }
        if auth_mode == AuthMode::PersonalAccessToken {
            let Some(personal_access_token) = auth_dot_json.personal_access_token.as_deref() else {
                return Err(std::io::Error::other(
                    "personal access token auth is missing a personal access token.",
                ));
            };
            return Ok(Self::PersonalAccessToken(
                PersonalAccessTokenAuth::load_with_cached(
                    personal_access_token,
                    auth_route_config,
                    cached_auth,
                )
                .await?,
            ));
        }
        if auth_mode == AuthMode::BedrockApiKey {
            let Some(auth) = auth_dot_json.bedrock_api_key else {
                return Err(std::io::Error::other(
                    "Bedrock API key auth is missing a Bedrock API key.",
                ));
            };
            return Ok(Self::BedrockApiKey(auth));
        }
        if auth_mode == AuthMode::BedrockAccessKeys {
            let Some(auth) = auth_dot_json.bedrock_access_keys else {
                return Err(std::io::Error::other(
                    "Bedrock access keys auth is missing AWS access keys.",
                ));
            };
            return Ok(Self::BedrockAccessKeys(auth));
        }
        if auth_mode == AuthMode::Headers {
            return Err(std::io::Error::other(
                "externally provided auth cannot be loaded from auth storage.",
            ));
        }

        let storage_mode = auth_dot_json.storage_mode(auth_credentials_store_mode);
        let provenance = auth_dot_json
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.oauth_provenance.as_ref());
        // Reuse only the transport, not the credential state or storage backend: the
        // selected source can change even when its credential document is identical.
        let cached_client = cached_auth
            .filter(|auth| auth.get_current_auth_json().as_ref() == Some(&auth_dot_json))
            .and_then(|auth| match auth {
                Self::Chatgpt(auth) => Some(auth.state.client.clone()),
                Self::ChatgptAuthTokens(auth) => Some(auth.state.client.clone()),
                _ => None,
            });
        let client = match cached_client {
            Some(client) => client,
            None => {
                create_default_auth_client(&refresh_token_endpoint(provenance), auth_route_config)?
            }
        };
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client,
        };

        match auth_mode {
            AuthMode::Chatgpt => {
                let storage = create_auth_storage(
                    codex_home.to_path_buf(),
                    storage_mode,
                    keyring_backend_kind,
                );
                Ok(Self::Chatgpt(ChatgptAuth { state, storage }))
            }
            AuthMode::ChatgptAuthTokens => Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens { state })),
            AuthMode::ApiKey => unreachable!("api key mode is handled above"),
            AuthMode::Headers => {
                unreachable!("externally provided auth is never loaded from auth storage")
            }
            AuthMode::AgentIdentity => unreachable!("agent identity mode is handled above"),
            AuthMode::PersonalAccessToken => {
                unreachable!("personal access token mode is handled above")
            }
            AuthMode::BedrockApiKey => unreachable!("bedrock api key mode is handled above"),
            AuthMode::BedrockAccessKeys => {
                unreachable!("bedrock access keys mode is handled above")
            }
        }
    }

    pub async fn from_auth_storage(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Option<Self>> {
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(chatgpt_base_url).ok();
        load_auth(
            codex_home,
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
            /*allowed_login_methods*/ None,
            /*forced_chatgpt_workspace_id*/ None,
            chatgpt_base_url,
            keyring_backend_kind,
            agent_identity_authapi_base_url.as_deref(),
            auth_route_config,
            /*cached_auth*/ None,
            /*allowed_auth_modes*/ None,
        )
        .await
    }

    pub async fn from_agent_identity_jwt(
        jwt: &str,
        chatgpt_base_url: Option<&str>,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        let agent_identity_authapi_base_url = agent_identity_authapi_base_url(chatgpt_base_url)?;
        Self::from_agent_identity_jwt_with_authapi_base_url(
            jwt,
            chatgpt_base_url,
            &agent_identity_authapi_base_url,
            auth_route_config,
        )
        .await
    }

    pub(super) async fn from_agent_identity_jwt_with_authapi_base_url(
        jwt: &str,
        chatgpt_base_url: Option<&str>,
        agent_identity_authapi_base_url: &str,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        let base_url = chatgpt_base_url
            .unwrap_or(ChatGptEnvironment::default().chatgpt_base_url())
            .trim_end_matches('/')
            .to_string();
        Ok(Self::AgentIdentity(
            AgentIdentityAuth::from_jwt(
                jwt,
                &base_url,
                agent_identity_authapi_base_url,
                auth_route_config,
            )
            .await?,
        ))
    }

    pub async fn from_personal_access_token(
        access_token: &str,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        Ok(Self::PersonalAccessToken(
            PersonalAccessTokenAuth::load(access_token, auth_route_config).await?,
        ))
    }

    /// Returns the effective backend auth mode.
    ///
    /// Externally managed ChatGPT tokens are normalized to [`AuthMode::Chatgpt`].
    pub fn auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => AuthMode::Chatgpt,
            Self::Headers(_) => AuthMode::Headers,
            Self::AgentIdentity(_) => AuthMode::AgentIdentity,
            Self::PersonalAccessToken(_) => AuthMode::PersonalAccessToken,
            Self::BedrockApiKey(_) => AuthMode::BedrockApiKey,
            Self::BedrockAccessKeys(_) => AuthMode::BedrockAccessKeys,
        }
    }

    /// Returns the precise kind of credentials backing this authentication.
    pub fn api_auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) => AuthMode::Chatgpt,
            Self::ChatgptAuthTokens(_) => AuthMode::ChatgptAuthTokens,
            Self::Headers(_) => AuthMode::Headers,
            Self::AgentIdentity(_) => AuthMode::AgentIdentity,
            Self::PersonalAccessToken(_) => AuthMode::PersonalAccessToken,
            Self::BedrockApiKey(_) => AuthMode::BedrockApiKey,
            Self::BedrockAccessKeys(_) => AuthMode::BedrockAccessKeys,
        }
    }

    pub fn is_api_key_auth(&self) -> bool {
        self.auth_mode() == AuthMode::ApiKey
    }

    pub fn is_personal_access_token_auth(&self) -> bool {
        self.auth_mode() == AuthMode::PersonalAccessToken
    }

    pub fn is_chatgpt_auth(&self) -> bool {
        self.api_auth_mode().has_chatgpt_account()
    }

    pub fn uses_codex_backend(&self) -> bool {
        self.api_auth_mode().uses_codex_backend()
    }

    pub fn is_external_chatgpt_tokens(&self) -> bool {
        matches!(self, Self::ChatgptAuthTokens(_))
    }

    pub(super) fn supports_unauthorized_recovery(&self) -> bool {
        matches!(
            self,
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) | Self::Headers(_)
        )
    }

    /// Returns `None` if `auth_mode() != AuthMode::ApiKey`.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(auth) => Some(auth.api_key.as_str()),
            Self::Chatgpt(_)
            | Self::ChatgptAuthTokens(_)
            | Self::Headers(_)
            | Self::AgentIdentity(_)
            | Self::PersonalAccessToken(_)
            | Self::BedrockApiKey(_)
            | Self::BedrockAccessKeys(_) => None,
        }
    }

    pub(in crate::auth) fn api_workload_identity(&self) -> Option<&ApiTokenMetadata> {
        if let Self::ApiKey(auth) = self {
            auth.workload_identity.as_ref()
        } else {
            None
        }
    }

    /// Returns `Err` if token-backed ChatGPT auth is unavailable.
    pub fn get_token_data(&self) -> Result<TokenData, std::io::Error> {
        let auth_dot_json: Option<AuthDotJson> = self.get_current_auth_json();
        match auth_dot_json {
            Some(AuthDotJson {
                tokens: Some(tokens),
                last_refresh: Some(_),
                ..
            }) => Ok(workspace_switching::selected_workspace_tokens(tokens)),
            _ => Err(std::io::Error::other("Token data is not available.")),
        }
    }

    /// Returns the token string used for bearer authentication.
    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => {
                let access_token = self.get_token_data()?.access_token;
                Ok(access_token)
            }
            Self::AgentIdentity(_) => Err(std::io::Error::other(
                "agent identity auth does not expose a bearer token",
            )),
            Self::Headers(_) => Err(std::io::Error::other(
                "header auth does not expose a bearer token",
            )),
            Self::PersonalAccessToken(auth) => Ok(auth.access_token().to_string()),
            Self::BedrockApiKey(_) | Self::BedrockAccessKeys(_) => Err(std::io::Error::other(
                "Bedrock API key auth does not expose a Codex bearer token",
            )),
        }
    }

    /// Returns `None` if Codex backend auth does not expose an account id.
    pub fn get_account_id(&self) -> Option<String> {
        match self {
            Self::Headers(headers) => headers
                .headers()
                .get(CHATGPT_ACCOUNT_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .filter(|account_id| !account_id.is_empty() && account_id.trim() == *account_id)
                .map(ToOwned::to_owned),
            Self::AgentIdentity(auth) => Some(auth.account_id().to_string()),
            Self::PersonalAccessToken(auth) => Some(auth.account_id().to_string()),
            _ => self.get_current_token_data().and_then(|t| t.account_id),
        }
    }

    /// Returns whether auth carries a FedRAMP routing hint from a claim or API-key prefix.
    pub fn is_fedramp_account(&self) -> bool {
        match self {
            Self::ApiKey(auth) => is_fedramp_api_key(&auth.api_key),
            Self::Headers(_) => false,
            Self::AgentIdentity(auth) => auth.is_fedramp_account(),
            Self::PersonalAccessToken(auth) => auth.is_fedramp_account(),
            _ => self
                .get_current_token_data()
                .is_some_and(|t| t.id_token.is_fedramp_account()),
        }
    }

    /// Returns `None` if Codex backend auth does not expose an account email.
    pub fn get_account_email(&self) -> Option<String> {
        match self {
            Self::Headers(_) => None,
            Self::AgentIdentity(auth) => auth.email().map(str::to_string),
            Self::PersonalAccessToken(auth) => auth.email().map(str::to_string),
            _ => self.get_current_token_data().and_then(|t| t.id_token.email),
        }
    }

    /// Returns `None` if Codex backend auth does not expose a ChatGPT user id.
    pub fn get_chatgpt_user_id(&self) -> Option<String> {
        match self {
            Self::Headers(_) => None,
            Self::AgentIdentity(auth) => Some(auth.chatgpt_user_id().to_string()),
            Self::PersonalAccessToken(auth) => Some(auth.chatgpt_user_id().to_string()),
            _ => self
                .get_current_token_data()
                .and_then(|t| t.id_token.chatgpt_user_id),
        }
    }

    /// Returns the access token's opaque account-user identity only when its workspace
    /// matches the selected account. Missing claims never fall back to a user id.
    /// Unlike `get_chatgpt_user_id`, this identifies one workspace membership, so keys
    /// are not shared across a person's workspaces. Workload-identity exchange claims
    /// describe an agent identity and cannot select a human verification credential.
    /// This is local identity selection, not access-token or proof validation.
    pub fn get_chatgpt_account_user_id(&self) -> Option<String> {
        let tokens = self.get_current_token_data()?;
        parse_chatgpt_account_user_id(&tokens.access_token, tokens.account_id.as_deref()?)
            .ok()
            .flatten()
    }

    /// Account-facing plan classification derived from the current auth.
    /// Returns a high-level `AccountPlanType` (e.g., Free/Plus/Pro/Team/…)
    /// for UI or product decisions based on the user's subscription.
    pub fn account_plan_type(&self) -> Option<AccountPlanType> {
        if matches!(self, Self::Headers(_)) {
            return None;
        }
        if let Self::AgentIdentity(auth) = self {
            return Some(auth.plan_type());
        }
        if let Self::PersonalAccessToken(auth) = self {
            return Some(auth.plan_type());
        }

        self.get_current_token_data().map(|t| {
            t.id_token
                .chatgpt_plan_type
                .map(AccountPlanType::from)
                .unwrap_or(AccountPlanType::Unknown)
        })
    }

    pub fn is_workspace_account(&self) -> bool {
        self.account_plan_type()
            .is_some_and(AccountPlanType::is_workspace_account)
    }

    /// Returns `None` if token-backed ChatGPT auth is unavailable.
    pub(super) fn get_current_auth_json(&self) -> Option<AuthDotJson> {
        let state = match self {
            Self::Chatgpt(auth) => &auth.state,
            Self::ChatgptAuthTokens(auth) => &auth.state,
            Self::ApiKey(_)
            | Self::Headers(_)
            | Self::AgentIdentity(_)
            | Self::PersonalAccessToken(_)
            | Self::BedrockApiKey(_)
            | Self::BedrockAccessKeys(_) => return None,
        };
        #[expect(clippy::unwrap_used)]
        state.auth_dot_json.lock().unwrap().clone()
    }

    /// Returns `None` if token-backed ChatGPT auth is unavailable.
    pub(super) fn get_current_token_data(&self) -> Option<TokenData> {
        self.get_current_auth_json()
            .and_then(|t| t.tokens)
            .map(workspace_switching::selected_workspace_tokens)
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: Default::default(),
                access_token: "Access Token".to_string(),
                refresh_token: "test".to_string(),
                oauth_provenance: None,
                login_owner_id: None,
                account_id: Some("account_id".to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
            bedrock_access_keys: None,
        };

        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client: create_client(),
        };
        let dummy_auth_id = NEXT_DUMMY_AUTH_ID.fetch_add(1, Ordering::Relaxed);
        let storage = create_auth_storage(
            PathBuf::from(format!("dummy-chatgpt-auth-{dummy_auth_id}")),
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        );
        Self::Chatgpt(ChatgptAuth { state, storage })
    }

    /// Constructs in-memory ChatGPT auth from externally managed tokens.
    pub fn from_external_chatgpt_tokens(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
    ) -> std::io::Result<Self> {
        let auth_dot_json = AuthDotJson::from_external_access_token(
            access_token,
            chatgpt_account_id,
            chatgpt_plan_type,
        )?;
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client: create_client(),
        };
        Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens { state }))
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
            workload_identity: None,
        })
    }
}

impl ChatgptAuth {
    pub(super) fn current_auth_json(&self) -> Option<AuthDotJson> {
        #[expect(clippy::unwrap_used)]
        self.state.auth_dot_json.lock().unwrap().clone()
    }

    pub(super) fn current_token_data(&self) -> Option<TokenData> {
        self.current_auth_json().and_then(|auth| auth.tokens)
    }

    pub(super) fn storage(&self) -> &Arc<dyn AuthStorageBackend> {
        &self.storage
    }

    pub(super) fn client(&self) -> &HttpClient {
        &self.state.client
    }
}
