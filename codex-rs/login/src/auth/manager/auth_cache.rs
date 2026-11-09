//! Reads, reloads, and publishes the manager's cached credential snapshot.

use super::*;

impl AuthManager {
    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Option<CodexAuth> {
        self.inner
            .read()
            .ok()
            .and_then(|cached| cached.auth.clone())
    }

    /// Current revisions if the selected credential still belongs to this manager's source.
    pub fn auth_change_state_for(&self, expected: &CodexAuth) -> Option<AuthChangeState> {
        let cached = self.inner.read().ok()?;
        if !super::source::same_source(cached.auth.as_ref(), Some(expected)) {
            return None;
        }
        Some(*self.auth_change_state_tx.borrow())
    }

    /// Identify this credential source's current account owner without refreshing auth.
    pub fn auth_owner(self: &Arc<Self>) -> AuthOwner {
        AuthOwner::new(
            Arc::clone(self),
            self.auth_change_state_tx.borrow().owner_generation,
        )
    }

    /// Retain the owner of a selected credential, rejecting a changed source.
    pub fn auth_owner_for(self: &Arc<Self>, expected: &CodexAuth) -> Option<AuthOwner> {
        let state = self.auth_change_state_for(expected)?;
        Some(AuthOwner::new(Arc::clone(self), state.owner_generation))
    }

    /// Subscribes to cached auth changes that can affect request recovery.
    pub fn auth_change_receiver(&self) -> watch::Receiver<u64> {
        self.auth_change_tx.subscribe()
    }

    /// Subscribes to credential and owner revisions published together, including when changes coalesce.
    pub fn auth_change_state_receiver(&self) -> watch::Receiver<AuthChangeState> {
        self.auth_change_state_tx.subscribe()
    }

    pub fn refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError> {
        self.inner.read().ok().and_then(|cached| {
            cached
                .permanent_refresh_failure
                .as_ref()
                .filter(|failure| Self::auths_equal_for_refresh(Some(auth), Some(&failure.auth)))
                .map(|failure| failure.error.clone())
        })
    }

    /// Whether this credential's refresh exchange or persistence started but did not finish.
    /// Cancellation retains the record so callers can withhold an uncertain generation.
    /// Waiting for the refresh guard or reloading storage does not mark a credential.
    pub fn is_chatgpt_refresh_unsettled(&self, auth: &CodexAuth) -> std::io::Result<bool> {
        let unsettled = self
            .unsettled_chatgpt_refresh
            .lock()
            .map_err(|_| std::io::Error::other("Refresh progress is unavailable."))?;
        // Match the stored generation; get_token_data() rewrites workspace claims.
        Ok(unsettled.as_ref().is_some_and(|tokens| {
            auth.get_current_auth_json()
                .and_then(|auth| auth.tokens)
                .as_ref()
                == Some(tokens)
        }))
    }

    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    /// For managed ChatGPT auth that needs a proactive refresh, first performs
    /// a guarded reload and then refreshes only if the on-disk auth is unchanged.
    #[instrument(level = "trace", skip_all)]
    pub async fn auth(&self) -> Option<CodexAuth> {
        if self.has_external_auth() {
            self.reload_inner().await.ok()?;
            return self.auth_cached();
        }

        let auth = self.auth_cached()?;
        if let Some(reason) = Self::proactive_refresh_reason(&auth)
            && let Err(err) = self.refresh_token_with_reason(reason, Some(&auth)).await
        {
            tracing::error!("Failed to refresh token: {}", err);
            return Some(auth);
        }
        self.auth_cached()
    }

    /// Refreshes auth, then captures credentials and their account-bound factory together.
    /// The auth read lock prevents an identity change between the two snapshots.
    pub async fn auth_with_http_client_factory(&self) -> Option<(CodexAuth, HttpClientFactory)> {
        self.auth().await?;
        let cached = self.inner.read().ok()?;
        Some((cached.auth.clone()?, self.http_client_factory()))
    }

    /// Borrows a persisted account grant while excluding this manager's token refreshes.
    /// Release the lease after the IdP exchange, then check the grant before using its result.
    pub async fn lease_account_oauth_grant(
        self: &Arc<Self>,
    ) -> std::io::Result<Option<AccountOAuthGrantLease>> {
        let permit = Arc::clone(&self.refresh_lock)
            .acquire_owned()
            .await
            .map_err(|_| std::io::Error::other("Account refresh coordination is unavailable."))?;
        Ok(self
            .account_oauth_grant()
            .await?
            .map(|grant| AccountOAuthGrantLease { grant, permit }))
    }

    /// Reads an owner-checked grant without waiting for or excluding account refresh.
    /// Use this for inspection; lease the grant again before exchanging its refresh token.
    pub async fn account_oauth_grant(
        self: &Arc<Self>,
    ) -> std::io::Result<Option<AccountOAuthGrant>> {
        let login_generation = self.auth_change_state_tx.borrow().login_generation;
        let Some(tokens) = self.read_account_grant_tokens().await? else {
            return Ok(None);
        };
        let grant = AccountOAuthGrant {
            manager: Arc::clone(self),
            tokens,
            login_generation,
        };
        if !grant.owner_is_current() {
            return Ok(None);
        }
        Ok(Some(grant))
    }

    /// Checks the persisted account owner without requiring an EMA-eligible account grant.
    /// Dedicated enterprise credentials still belong to this account login.
    pub async fn account_login_is_current(&self, expected: &CodexAuth) -> std::io::Result<bool> {
        if !matches!(expected, CodexAuth::Chatgpt(_)) {
            return Ok(self.auth_cached().is_some_and(|current| {
                !matches!(current, CodexAuth::Chatgpt(_))
                    && current.get_account_id() == expected.get_account_id()
                    && current.get_chatgpt_user_id() == expected.get_chatgpt_user_id()
            }));
        }
        let expected = expected.get_token_data()?;
        if expected
            .login_owner_id
            .as_ref()
            .is_none_or(String::is_empty)
        {
            return Ok(false);
        }
        let matches_owner = || {
            self.auth_cached().is_some_and(|auth| {
                matches!(auth, CodexAuth::Chatgpt(_))
                    && auth
                        .get_token_data()
                        .is_ok_and(|tokens| account_grant::same_account_login(&expected, &tokens))
            })
        };
        if !matches_owner() {
            return Ok(false);
        }
        let stored = self.read_persisted_account_tokens().await?;
        Ok(matches_owner()
            && stored
                .as_ref()
                .is_some_and(|tokens| account_grant::same_account_login(&expected, tokens)))
    }

    async fn read_persisted_account_tokens(&self) -> std::io::Result<Option<TokenData>> {
        let home = self.codex_home.clone();
        let mode = self.auth_credentials_store_mode;
        let backend = self.keyring_backend_kind;
        let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
        let stored = tokio::task::spawn_blocking(move || {
            originator.sync_scope(|| {
                let _commit = auth_storage_commit_guard()?;
                create_auth_storage(home, mode, backend).load_account_grant()
            })
        })
        .await
        .map_err(|_| std::io::Error::other("Account credential read failed."))?
        .map_err(|_: std::io::Error| {
            std::io::Error::other("Account credential storage is unavailable.")
        })?;
        Ok(stored
            .filter(|auth| auth.resolved_mode() == AuthMode::Chatgpt)
            .and_then(|auth| auth.tokens))
    }

    pub(super) async fn read_account_grant_tokens(&self) -> std::io::Result<Option<TokenData>> {
        if self.has_external_auth()
            || self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral
        {
            return Ok(None);
        }
        let Some(auth @ CodexAuth::Chatgpt(_)) = self.auth_cached() else {
            return Ok(None);
        };
        let allowed = self.allowed_login_methods();
        if validate_auth_restrictions(
            Some(&allowed),
            self.effective_chatgpt_workspaces().as_deref(),
            &auth,
        )
        .is_err()
        {
            return Ok(None);
        }
        let cached = auth.get_token_data()?;
        let stored = self.read_persisted_account_tokens().await?;
        Ok(stored.filter(|tokens| {
            tokens.oauth_provenance.as_ref().is_some_and(|provenance| {
                !provenance.issuer.trim().is_empty() && !provenance.client_id.trim().is_empty()
            }) && tokens
                .login_owner_id
                .as_ref()
                .is_some_and(|id| !id.is_empty())
                && !tokens.refresh_token.is_empty()
                && tokens
                    .id_token
                    .chatgpt_user_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty())
                && tokens.account_id.as_ref().is_some_and(|id| !id.is_empty())
                && same_account_grant(tokens, &cached)
        }))
    }

    /// Reloads auth from the active source. Returns whether the auth value changed.
    pub async fn reload(&self) -> bool {
        self.reload_inner().await.unwrap_or(false)
    }

    async fn reload_inner(&self) -> Result<bool, RefreshTokenError> {
        tracing::info!("Reloading auth");
        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            let new_auth = self.load_auth(/*cached_auth*/ None).await?;
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return Ok(publication.changed);
            }
            if self.inner.is_poisoned() {
                return Ok(false);
            }
        }
    }

    /// Pins the stored credential kind before any subsequent load or hydration.
    /// This prevents a running client from silently acquiring a different kind of secret.
    pub fn restrict_auth_modes(&self, mut modes: Vec<AuthMode>) -> std::io::Result<()> {
        let mut allowed = self
            .allowed_auth_modes
            .write()
            .map_err(|_| std::io::Error::other("credential policy is unavailable"))?;
        if let Some(previous) = allowed.as_ref() {
            modes.retain(|mode| previous.contains(mode));
        }
        *allowed = Some(modes);
        Ok(())
    }

    /// Reloads stored credentials without hiding storage or hydration failures.
    /// Failed loads retain the last known owner so a temporary outage does not
    /// invalidate application network policy as if the user had logged out.
    /// Leaves host-provided credentials alone and serializes with token refresh.
    /// Callers must reject cached headers on error.
    pub async fn reload_with_cached_auth(&self) -> std::io::Result<bool> {
        let _refresh_guard = self
            .refresh_lock
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            if self.has_external_auth() {
                return Ok(false);
            }
            let cached = self.auth_cached();
            let new_auth = self.try_load_managed_auth(cached.as_ref()).await?;
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return Ok(publication.changed);
            }
            if self.inner.is_poisoned() {
                return Err(std::io::Error::other("credential cache is unavailable"));
            }
        }
    }

    pub(super) async fn reload_if_account_id_matches(
        &self,
        expected_account_id: Option<&str>,
    ) -> std::io::Result<ReloadOutcome> {
        let expected_account_id = match expected_account_id {
            Some(account_id) => account_id,
            None => {
                tracing::info!("Skipping auth reload because no account id is available.");
                return Ok(ReloadOutcome::Skipped);
            }
        };

        loop {
            let generation = self.auth_change_state_tx.borrow().generation;
            let new_auth = self.load_auth(/*cached_auth*/ None).await?;
            let new_account_id = new_auth.as_ref().and_then(CodexAuth::get_account_id);

            if new_account_id.as_deref() != Some(expected_account_id) {
                let found_account_id = new_account_id.as_deref().unwrap_or("unknown");
                tracing::info!(
                    "Skipping auth reload due to account id mismatch (expected: {expected_account_id}, found: {found_account_id})"
                );
                return Ok(ReloadOutcome::Skipped);
            }

            tracing::info!("Reloading auth for account {expected_account_id}");
            if let Some(publication) = self.publish_reloaded_auth(generation, new_auth) {
                return Ok(if publication.changed_for_refresh {
                    ReloadOutcome::ReloadedChanged
                } else {
                    ReloadOutcome::ReloadedNoChange
                });
            }
            if self.inner.is_poisoned() {
                return Ok(ReloadOutcome::Skipped);
            }
        }
    }

    pub(super) fn auths_equal_for_refresh(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => match (a.api_auth_mode(), b.api_auth_mode()) {
                (AuthMode::ApiKey, AuthMode::ApiKey) => {
                    a.api_key() == b.api_key()
                        && a.api_workload_identity() == b.api_workload_identity()
                }
                (AuthMode::Chatgpt, AuthMode::Chatgpt) => {
                    a.get_current_auth_json() == b.get_current_auth_json()
                }
                (AuthMode::ChatgptAuthTokens, AuthMode::ChatgptAuthTokens) => {
                    a.get_current_token_data() == b.get_current_token_data()
                }
                (AuthMode::Headers, AuthMode::Headers) => a == b,
                (AuthMode::AgentIdentity, AuthMode::AgentIdentity) => match (a, b) {
                    (CodexAuth::AgentIdentity(a), CodexAuth::AgentIdentity(b)) => {
                        a.record() == b.record()
                    }
                    _ => false,
                },
                (AuthMode::PersonalAccessToken, AuthMode::PersonalAccessToken) => a == b,
                (AuthMode::BedrockApiKey, AuthMode::BedrockApiKey) => a == b,
                (AuthMode::BedrockAccessKeys, AuthMode::BedrockAccessKeys) => a == b,
                _ => false,
            },
            _ => false,
        }
    }

    fn auths_equal(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Records a permanent refresh failure only if the failed refresh was
    /// attempted against the auth snapshot that is still cached.
    pub(super) fn record_permanent_refresh_failure_if_unchanged(
        &self,
        attempted_auth: &CodexAuth,
        error: &RefreshTokenFailedError,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let current_auth_matches =
                Self::auths_equal_for_refresh(Some(attempted_auth), guard.auth.as_ref());
            if current_auth_matches {
                guard.permanent_refresh_failure = Some(AuthScopedRefreshFailure {
                    auth: attempted_auth.clone(),
                    error: error.clone(),
                });
            }
        }
    }

    pub(super) async fn load_auth(
        &self,
        cached_auth: Option<&CodexAuth>,
    ) -> Result<Option<CodexAuth>, RefreshTokenError> {
        if let Some(external_auth) = self.external_auth_provider() {
            let cached_auth = self.auth_cached();
            if cached_auth
                .as_ref()
                .is_some_and(|auth| self.refresh_failure_for_auth(auth).is_some())
            {
                return Ok(cached_auth);
            }
            return match self.resolve_external_auth(external_auth.as_ref()).await {
                Ok(auth) => Ok(Some(auth)),
                Err(err) => {
                    tracing::error!("Failed to resolve external auth: {err}");
                    match err {
                        RefreshTokenError::Permanent(error) => {
                            if let Some(auth) = cached_auth.as_ref() {
                                self.record_permanent_refresh_failure_if_unchanged(auth, &error);
                            }
                            Ok(cached_auth)
                        }
                        // A temporary provider outage is not a logout. Preserve the identity
                        // only while its cached credentials still satisfy the current policy.
                        RefreshTokenError::Transient(_) => Ok(cached_auth.filter(|auth| {
                            validate_auth_restrictions(
                                Some(&self.allowed_login_methods()),
                                self.effective_chatgpt_workspaces().as_deref(),
                                auth,
                            )
                            .is_ok()
                        })),
                        RefreshTokenError::Policy(_) => Ok(cached_auth),
                    }
                }
            };
        }

        self.try_load_managed_auth(cached_auth)
            .await
            .map_err(RefreshTokenError::Transient)
    }

    async fn try_load_managed_auth(
        &self,
        cached_auth: Option<&CodexAuth>,
    ) -> std::io::Result<Option<CodexAuth>> {
        let allowed_auth_modes = self
            .allowed_auth_modes
            .read()
            .map_err(|_| std::io::Error::other("credential policy is unavailable"))?
            .clone();
        let allowed_login_methods = self.allowed_login_methods();
        let effective_chatgpt_workspaces = self.effective_chatgpt_workspaces();
        let auth = load_auth(
            &self.codex_home,
            self.enable_codex_api_key_env,
            self.auth_credentials_store_mode,
            Some(&allowed_login_methods),
            effective_chatgpt_workspaces.as_deref(),
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            self.agent_identity_authapi_base_url.as_deref(),
            &self.auth_route_config,
            cached_auth,
            allowed_auth_modes.as_deref(),
        )
        .await?;
        Ok(auth.filter(|auth| {
            validate_auth_restrictions(
                Some(&allowed_login_methods),
                effective_chatgpt_workspaces.as_deref(),
                auth,
            )
            .is_ok()
        }))
    }

    pub(super) fn set_cached_auth(&self, new_auth: Option<CodexAuth>) -> bool {
        let Ok(mut guard) = self.inner.write() else {
            return false;
        };
        self.set_cached_auth_locked(&mut guard, new_auth)
    }

    pub(super) fn publish_reloaded_auth(
        &self,
        generation: u64,
        auth: Option<CodexAuth>,
    ) -> Option<AuthReload> {
        let mut guard = self.inner.write().ok()?;
        if self.auth_change_state_tx.borrow().generation != generation {
            return None;
        }
        let changed_for_refresh =
            !Self::auths_equal_for_refresh(guard.auth.as_ref(), auth.as_ref());
        let changed = self.set_cached_auth_locked(&mut guard, auth);
        Some(AuthReload {
            changed,
            changed_for_refresh,
        })
    }

    pub(super) fn invalidate_pending_auth_reads(&self) {
        if let Ok(_guard) = self.inner.write() {
            self.auth_change_state_tx
                .send_modify(|state| state.generation += 1);
        }
    }

    fn set_cached_auth_locked(&self, guard: &mut CachedAuth, new_auth: Option<CodexAuth>) -> bool {
        let previous = guard.auth.as_ref();
        let changed = !AuthManager::auths_equal(previous, new_auth.as_ref());
        let auth_changed_for_refresh = !Self::auths_equal_for_refresh(previous, new_auth.as_ref());
        let owner_changed = auth_changed_for_refresh && !same_owner(previous, new_auth.as_ref());
        let source_changed =
            auth_changed_for_refresh && !super::source::same_source(previous, new_auth.as_ref());
        let login_changed = auth_changed_for_refresh && !same_login(previous, new_auth.as_ref());
        if owner_changed && self.owns_network_policy {
            self.auth_route_config
                .application_network_policy()
                .invalidate();
        }
        if auth_changed_for_refresh {
            guard.permanent_refresh_failure = None;
        }
        guard.auth = new_auth;
        if auth_changed_for_refresh {
            self.auth_change_state_tx.send_modify(|state| {
                state.generation += 1;
                if owner_changed {
                    state.owner_generation += 1;
                }
                if source_changed {
                    state.source_generation += 1;
                }
                if login_changed {
                    state.login_generation += 1;
                }
            });
            self.auth_change_tx.send_modify(|revision| *revision += 1);
        }
        tracing::info!(
            auth_changed_for_refresh,
            "Reloaded auth, changed: {changed}"
        );
        changed
    }

    pub fn is_external_chatgpt_auth_active(&self) -> bool {
        self.auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_external_chatgpt_tokens)
    }

    /// Returns the precise kind of credentials backing the current authentication.
    pub fn get_api_auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::api_auth_mode)
    }

    /// Returns the effective backend auth mode for the current authentication.
    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::auth_mode)
    }

    pub fn current_auth_uses_codex_backend(&self) -> bool {
        self.get_api_auth_mode()
            .is_some_and(AuthMode::uses_codex_backend)
    }
}
