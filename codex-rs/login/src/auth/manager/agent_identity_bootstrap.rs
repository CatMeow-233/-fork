//! Registers or reuses managed ChatGPT Agent Identity records and throttles failed bootstraps.

use super::*;

impl AgentIdentityBootstrapCooldown {
    fn error_for(
        &mut self,
        account_id: &str,
        authapi_base_url: &str,
        now: Instant,
    ) -> Option<AgentIdentityAuthError> {
        let error = self
            .failure
            .as_ref()
            .filter(|failure| {
                failure.account_id == account_id
                    && failure.authapi_base_url == authapi_base_url
                    && failure.retry_at > now
            })
            .map(|failure| failure.error.clone());
        if error.is_none() {
            self.clear();
        }
        error
    }

    fn record_failure(
        &mut self,
        account_id: String,
        authapi_base_url: String,
        error: AgentIdentityAuthError,
        now: Instant,
    ) {
        self.failure = Some(CachedAgentIdentityBootstrapFailure {
            account_id,
            authapi_base_url,
            retry_at: now + AGENT_IDENTITY_BOOTSTRAP_FAILURE_COOLDOWN,
            error,
        });
    }

    fn clear(&mut self) {
        self.failure = None;
    }
}

impl CodexAuth {
    pub(super) fn stored_managed_chatgpt_agent_identity_record(
        &self,
        account_id: &str,
    ) -> Option<AgentIdentityAuthRecord> {
        self.get_current_auth_json()
            .and_then(|auth| auth.agent_identity)
            .and_then(|identity| identity.as_record().cloned())
            .filter(|identity| identity.account_id == account_id)
    }

    pub(super) fn persist_managed_chatgpt_agent_identity_record(
        &self,
        record: AgentIdentityAuthRecord,
    ) -> std::io::Result<()> {
        if let Self::Chatgpt(chatgpt_auth) = self {
            chatgpt_auth.persist_agent_identity_record(record)?;
        }
        Ok(())
    }

    pub(super) async fn agent_identity_auth(
        &self,
        policy: AgentIdentityAuthPolicy,
        agent_identity_authapi_base_url: Option<&str>,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        auth_route_config: &AuthRouteConfig,
        session_source: SessionSource,
    ) -> std::io::Result<Option<AgentIdentityAuth>> {
        match self {
            Self::AgentIdentity(auth) => Ok(Some(auth.clone())),
            Self::ApiKey(_)
            | Self::ChatgptAuthTokens(_)
            | Self::Headers(_)
            | Self::PersonalAccessToken(_)
            | Self::BedrockApiKey(_)
            | Self::BedrockAccessKeys(_) => Ok(None),
            Self::Chatgpt(_) => {
                if policy == AgentIdentityAuthPolicy::JwtOnly {
                    return Ok(None);
                }
                self.ensure_managed_chatgpt_agent_identity(
                    require_agent_identity_authapi_base_url(agent_identity_authapi_base_url)?,
                    forced_chatgpt_workspace_id,
                    auth_route_config,
                    session_source,
                )
                .await
                .map(Some)
            }
        }
    }

    async fn ensure_managed_chatgpt_agent_identity(
        &self,
        agent_identity_authapi_base_url: &str,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        auth_route_config: &AuthRouteConfig,
        session_source: SessionSource,
    ) -> std::io::Result<AgentIdentityAuth> {
        let binding =
            ManagedChatGptAgentIdentityBinding::from_auth(self, forced_chatgpt_workspace_id)
                .ok_or_else(|| std::io::Error::other("ChatGPT auth is unavailable"))?;

        // JWT auth is loaded as CodexAuth::AgentIdentity; this path only reuses
        // records created by the managed ChatGPT Agent Identity bootstrap.
        if let Some(record) = self.stored_managed_chatgpt_agent_identity_record(&binding.account_id)
            && record_matches_managed_chatgpt_binding(&record, &binding)
        {
            let should_persist = record_needs_task_registration(&record);
            let auth = AgentIdentityAuth::from_record(
                record,
                agent_identity_authapi_base_url,
                auth_route_config,
            )
            .await
            .map_err(|err| classify_bootstrap_error("agent task registration", err))?;
            if should_persist {
                self.persist_managed_chatgpt_agent_identity_record(auth.record().clone())?;
            }
            return Ok(auth);
        }

        let auth = register_managed_chatgpt_agent_identity(
            binding,
            agent_identity_authapi_base_url,
            session_source,
            auth_route_config,
        )
        .await?;
        self.persist_managed_chatgpt_agent_identity_record(auth.record().clone())?;
        Ok(auth)
    }
}

impl ManagedChatGptAgentIdentityBinding {
    fn from_auth(auth: &CodexAuth, forced_workspace_id: Option<Vec<String>>) -> Option<Self> {
        if !auth.is_chatgpt_auth() {
            return None;
        }

        let token_data = auth.get_token_data().ok()?;
        let forced_workspace_id =
            forced_workspace_id
                .as_deref()
                .and_then(|workspace_ids| match workspace_ids {
                    [workspace_id] if !workspace_id.is_empty() => Some(workspace_id.clone()),
                    _ => None,
                });
        let account_id = forced_workspace_id
            .or(token_data
                .account_id
                .clone()
                .filter(|value| !value.is_empty()))
            .or(token_data.id_token.chatgpt_account_id.clone())?;
        let chatgpt_user_id = token_data
            .id_token
            .chatgpt_user_id
            .clone()
            .filter(|value| !value.is_empty())?;

        Some(Self {
            account_id,
            chatgpt_user_id,
            email: token_data.id_token.email.clone(),
            plan_type: auth.account_plan_type().unwrap_or(AccountPlanType::Unknown),
            chatgpt_account_is_fedramp: auth.is_fedramp_account(),
            access_token: token_data.access_token,
        })
    }
}

impl ChatgptAuth {
    fn persist_agent_identity_record(
        &self,
        record: AgentIdentityAuthRecord,
    ) -> std::io::Result<()> {
        persist_agent_identity_record(&self.state.auth_dot_json, &self.storage, record)
    }
}

fn persist_agent_identity_record(
    auth_dot_json: &Arc<Mutex<Option<AuthDotJson>>>,
    storage: &Arc<dyn AuthStorageBackend>,
    record: AgentIdentityAuthRecord,
) -> std::io::Result<()> {
    let _commit = storage.lock_for_write()?;
    let mut guard = auth_dot_json
        .lock()
        .map_err(|_| std::io::Error::other("failed to lock auth state"))?;
    let stored = storage.load()?;
    if let Some(previous) = guard.as_ref().and_then(|auth| auth.tokens.as_ref())
        && !stored
            .as_ref()
            .and_then(|auth| auth.tokens.as_ref())
            .is_some_and(|current| same_login_for_persistence(previous, current))
    {
        return Err(std::io::Error::other(
            "Account changed during agent identity registration.",
        ));
    }
    let mut auth = stored.ok_or_else(|| std::io::Error::other("auth data is not available"))?;
    auth.agent_identity = Some(AgentIdentityStorage::Record(record));
    storage.save(&auth)?;
    // Publishing credentials belongs to AuthManager. Persisting identity metadata must
    // not reload different on-disk credentials into already selected auth values.
    if let Some(cached) = guard.as_mut() {
        cached.agent_identity = auth.agent_identity;
    }
    Ok(())
}

impl AuthManager {
    pub async fn agent_identity_auth(
        &self,
        policy: AgentIdentityAuthPolicy,
        session_source: SessionSource,
    ) -> std::io::Result<Option<AgentIdentityAuth>> {
        let Some(auth) = self.auth().await else {
            return Ok(None);
        };
        if policy == AgentIdentityAuthPolicy::ChatGptAuth && matches!(auth, CodexAuth::Chatgpt(_)) {
            let _bootstrap_permit = self
                .agent_identity_lock
                .acquire()
                .await
                .map_err(std::io::Error::other)?;
            let effective_chatgpt_workspaces = self.effective_chatgpt_workspaces();
            let cooldown_key = ManagedChatGptAgentIdentityBinding::from_auth(
                &auth,
                effective_chatgpt_workspaces.clone(),
            )
            .and_then(|binding| {
                self.agent_identity_authapi_base_url
                    .as_ref()
                    .map(|base_url| (binding.account_id, base_url.clone()))
            });
            if let Some((account_id, authapi_base_url)) = cooldown_key.as_ref()
                && let Ok(mut cooldown) = self.agent_identity_bootstrap_cooldown.lock()
                && let Some(error) =
                    cooldown.error_for(account_id, authapi_base_url, Instant::now())
            {
                tracing::warn!("agent identity bootstrap retry suppressed during shared cooldown");
                return Err(std::io::Error::other(error));
            }

            let result = auth
                .agent_identity_auth(
                    policy,
                    self.agent_identity_authapi_base_url.as_deref(),
                    effective_chatgpt_workspaces,
                    &self.auth_route_config,
                    session_source,
                )
                .await;
            if let Ok(mut cooldown) = self.agent_identity_bootstrap_cooldown.lock() {
                if let (Err(err), Some((account_id, authapi_base_url))) = (&result, cooldown_key)
                    && let Some(error) = AgentIdentityAuthError::bootstrap_unavailable(err).cloned()
                {
                    cooldown.record_failure(account_id, authapi_base_url, error, Instant::now());
                } else {
                    cooldown.clear();
                }
            }
            return result;
        }
        auth.agent_identity_auth(
            policy,
            self.agent_identity_authapi_base_url.as_deref(),
            self.effective_chatgpt_workspaces(),
            &self.auth_route_config,
            session_source,
        )
        .await
    }
}
