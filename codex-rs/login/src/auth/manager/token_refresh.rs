//! Exchanges ChatGPT refresh tokens with the OAuth authority and persists the results.

use super::*;

// Ordinary auth tolerates legacy metadata loss; EMA grant checks remain strict.
pub(super) fn same_login_for_persistence(expected: &TokenData, stored: &TokenData) -> bool {
    same_account_login(expected, stored)
        || (expected.login_owner_id.is_some()
            && stored.login_owner_id.is_none()
            && stored.account_id == expected.account_id
            && stored.id_token.chatgpt_user_id == expected.id_token.chatgpt_user_id
            && !stored.refresh_token.is_empty()
            && stored.refresh_token == expected.refresh_token)
}

// Persist refreshed tokens into auth storage and update last_refresh.
pub(in crate::auth) fn persist_tokens(
    storage: &Arc<dyn AuthStorageBackend>,
    expected: &TokenData,
    provenance: Option<AccountOAuthProvenance>,
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
) -> std::io::Result<AuthDotJson> {
    let _commit = storage.lock_for_write()?;
    let stored = storage.load()?;
    let mut auth_dot_json = stored.ok_or(std::io::Error::other("Token data is not available."))?;
    let original = auth_dot_json.clone();
    if auth_dot_json.resolved_mode() != AuthMode::Chatgpt {
        return Err(std::io::Error::other(
            "Account credentials changed during refresh.",
        ));
    }
    if !auth_dot_json.tokens.as_ref().is_some_and(|tokens| {
        same_login_for_persistence(expected, tokens)
            && tokens.refresh_token == expected.refresh_token
            && tokens.access_token == expected.access_token
    }) {
        return Err(std::io::Error::other(
            "Account credentials changed during refresh.",
        ));
    }
    let tokens = auth_dot_json
        .tokens
        .as_mut()
        .ok_or(std::io::Error::other("Token data is not available."))?;
    tokens.oauth_provenance = provenance
        .filter(|_| refresh_token.is_some() || tokens.refresh_token == expected.refresh_token);
    if let Some(id_token) = id_token {
        tokens.id_token = parse_chatgpt_jwt_claims(&id_token).map_err(std::io::Error::other)?;
    }
    if let Some(access_token) = access_token {
        tokens.access_token = access_token;
    }
    if let Some(refresh_token) = refresh_token {
        tokens.refresh_token = refresh_token;
    }
    auth_dot_json.last_refresh = Some(Utc::now());
    storage.compare_and_save(&original, &auth_dot_json)?;
    Ok(auth_dot_json)
}

// Requests refreshed ChatGPT OAuth tokens from the auth service using a refresh token.
// The caller is responsible for persisting any returned tokens.
async fn request_chatgpt_token_refresh(
    refresh_token: String,
    endpoint: &str,
    client_id: &str,
    client: &HttpClient,
    refresh_reason: RefreshReason,
) -> Result<RefreshResponse, RefreshTokenError> {
    let oauth = OAuthClient::new(
        client,
        TokenEndpoint {
            url: endpoint,
            client_id,
            encoding: TokenEncoding::Json,
            timeout: Some(Duration::from_secs(45)),
            error_body_limit: ErrorBodyLimit::Unlimited,
        },
    );
    match oauth
        .refresh(RefreshTokenGrant {
            refresh_token: &refresh_token,
            resource: None,
            refresh_reason: Some(refresh_reason),
        })
        .await
    {
        Ok(response) => Ok(response),
        Err(OAuthError::Rejected(rejection)) => {
            if let Some(codex_http_client::HttpError::Policy(denied)) = rejection.body_read_error {
                return Err(denied.into());
            }
            let status = rejection.status;
            // The IdP controls the response body and may echo credential material.
            tracing::error!(%status, "Failed to refresh token");
            let code = rejection.detail.error_code();
            let is_invalid_grant_bad_request = status == StatusCode::BAD_REQUEST
                && code.is_some_and(|code| code.eq_ignore_ascii_case("invalid_grant"));
            let failed = classify_refresh_token_failure(code, is_invalid_grant_bad_request);
            if status == StatusCode::UNAUTHORIZED
                || failed.reason != RefreshTokenFailedReason::Other
                || is_invalid_grant_bad_request
            {
                Err(RefreshTokenError::Permanent(failed))
            } else {
                Err(RefreshTokenError::Transient(std::io::Error::other(
                    format!("Failed to refresh token: {status}"),
                )))
            }
        }
        Err(OAuthError::Transport(error)) => Err(error.into()),
        Err(error @ OAuthError::InvalidResponse) => {
            Err(RefreshTokenError::Transient(std::io::Error::other(error)))
        }
    }
}

fn classify_refresh_token_failure(
    code: Option<&str>,
    is_invalid_grant_bad_request: bool,
) -> RefreshTokenFailedError {
    let normalized_code = code.map(str::to_ascii_lowercase);
    let reason = match normalized_code.as_deref() {
        Some("refresh_token_expired") => RefreshTokenFailedReason::Expired,
        Some("refresh_token_reused") => RefreshTokenFailedReason::Exhausted,
        Some("refresh_token_invalidated") => RefreshTokenFailedReason::Revoked,
        _ => RefreshTokenFailedReason::Other,
    };

    if reason == RefreshTokenFailedReason::Other && !is_invalid_grant_bad_request {
        tracing::warn!("Encountered unknown response while refreshing token");
    }

    let message = match reason {
        RefreshTokenFailedReason::Expired => REFRESH_TOKEN_EXPIRED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Exhausted => REFRESH_TOKEN_REUSED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Revoked => REFRESH_TOKEN_INVALIDATED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Other => REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
    };

    RefreshTokenFailedError::new(reason, message)
}

pub fn oauth_client_id() -> String {
    std::env::var(CLIENT_ID_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|client_id| !client_id.trim().is_empty())
        .unwrap_or_else(|| CLIENT_ID.to_string())
}

pub(super) fn refresh_token_endpoint(provenance: Option<&AccountOAuthProvenance>) -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR).unwrap_or_else(|_| {
        provenance
            .map(|provenance| format!("{}/oauth/token", provenance.issuer.trim_end_matches('/')))
            .unwrap_or_else(|| REFRESH_TOKEN_URL.to_string())
    })
}

impl AuthManager {
    /// Refreshes external auth through its provider. For managed auth, first
    /// performs a guarded reload: if the stored token differs from the cached
    /// token, reuse it; otherwise, ask the token authority to refresh.
    pub async fn refresh_token(&self) -> Result<(), RefreshTokenError> {
        self.refresh_token_with_reason(RefreshReason::Forced, /*expected_auth*/ None)
            .await
    }

    /// Refreshes only if the expected credential is still current after acquiring
    /// the existing refresh guard, so queued callers reuse a completed refresh.
    pub async fn refresh_token_if_unchanged(
        &self,
        expected_auth: &CodexAuth,
    ) -> Result<(), RefreshTokenError> {
        self.refresh_token_with_reason(RefreshReason::Forced, Some(expected_auth))
            .await
    }

    pub(super) async fn refresh_token_with_reason(
        &self,
        reason: RefreshReason,
        expected_auth: Option<&CodexAuth>,
    ) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
            ))
        })?;
        let auth_before_reload = self.auth_cached();
        if let Some(expected_auth) = expected_auth
            && !Self::auths_equal_for_refresh(auth_before_reload.as_ref(), Some(expected_auth))
        {
            return Ok(());
        }
        if self.has_external_auth() {
            return self.refresh_token_from_authority_impl(reason).await;
        }
        if auth_before_reload
            .as_ref()
            .is_some_and(|auth| auth.is_api_key_auth() || auth.is_personal_access_token_auth())
        {
            return Ok(());
        }
        let expected_account_id = auth_before_reload
            .as_ref()
            .and_then(CodexAuth::get_account_id);

        match self
            .reload_if_account_id_matches(expected_account_id.as_deref())
            .await?
        {
            ReloadOutcome::ReloadedChanged => {
                tracing::info!("Skipping token refresh because auth changed after guarded reload.");
                Ok(())
            }
            ReloadOutcome::ReloadedNoChange => self.refresh_token_from_authority_impl(reason).await,
            ReloadOutcome::Skipped => {
                Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                    RefreshTokenFailedReason::Other,
                    REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                )))
            }
        }
    }

    /// Attempt to refresh the current auth token from the authority that issued
    /// it and update the shared cache. If the token refresh fails, returns the
    /// error to the caller.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        self.refresh_token_from_authority_with_reason(RefreshReason::Forced)
            .await
    }

    pub(super) async fn refresh_token_from_authority_with_reason(
        &self,
        reason: RefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
            ))
        })?;
        self.refresh_token_from_authority_impl(reason).await
    }

    async fn refresh_token_from_authority_impl(
        &self,
        reason: RefreshReason,
    ) -> Result<(), RefreshTokenError> {
        tracing::info!("Refreshing token");

        let attempted_auth = self.auth_cached();
        if let Some(error) = attempted_auth
            .as_ref()
            .and_then(|auth| self.refresh_failure_for_auth(auth))
        {
            return Err(RefreshTokenError::Permanent(error));
        }

        let result = if self.has_external_auth() {
            self.refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                .await
        } else {
            match attempted_auth.as_ref() {
                Some(CodexAuth::Chatgpt(chatgpt_auth)) => {
                    let token_data = chatgpt_auth.current_token_data().ok_or_else(|| {
                        RefreshTokenError::Transient(std::io::Error::other(
                            "Token data is not available.",
                        ))
                    })?;
                    Box::pin(self.refresh_and_persist_chatgpt_token(
                        chatgpt_auth,
                        token_data,
                        reason,
                    ))
                    .await
                }
                Some(
                    CodexAuth::ApiKey(_)
                    | CodexAuth::ChatgptAuthTokens(_)
                    | CodexAuth::Headers(_)
                    | CodexAuth::AgentIdentity(_)
                    | CodexAuth::PersonalAccessToken(_)
                    | CodexAuth::BedrockApiKey(_)
                    | CodexAuth::BedrockAccessKeys(_),
                )
                | None => Ok(()),
            }
        };
        if let Some(attempted_auth) = attempted_auth.as_ref()
            && let Err(RefreshTokenError::Permanent(error)) = &result
        {
            self.record_permanent_refresh_failure_if_unchanged(attempted_auth, error);
        }
        result
    }

    pub(super) fn proactive_refresh_reason(auth: &CodexAuth) -> Option<RefreshReason> {
        let chatgpt_auth = match auth {
            CodexAuth::Chatgpt(chatgpt_auth) => chatgpt_auth,
            _ => return None,
        };

        let auth_dot_json = chatgpt_auth.current_auth_json()?;
        if let Some(tokens) = auth_dot_json.tokens.as_ref()
            && let Ok(Some(expires_at)) = parse_jwt_expiration(&tokens.access_token)
        {
            return (expires_at
                <= Utc::now()
                    + chrono::Duration::minutes(CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES))
            .then_some(RefreshReason::ProactiveExpiry);
        }
        let last_refresh = auth_dot_json.last_refresh?;
        (last_refresh < Utc::now() - chrono::Duration::days(TOKEN_REFRESH_INTERVAL))
            .then_some(RefreshReason::Scheduled)
    }

    // Refreshes ChatGPT OAuth tokens, persists the updated auth state, and
    // reloads the in-memory cache so callers immediately observe new tokens.
    async fn refresh_and_persist_chatgpt_token(
        &self,
        auth: &ChatgptAuth,
        tokens: TokenData,
        reason: RefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let endpoint = refresh_token_endpoint(tokens.oauth_provenance.as_ref());
        let client_id = tokens
            .oauth_provenance
            .as_ref()
            .map(|provenance| provenance.client_id.clone())
            .unwrap_or_else(oauth_client_id);
        let previous_unsettled = self
            .unsettled_chatgpt_refresh
            .lock()
            .map_err(|_| std::io::Error::other("Refresh progress is unavailable."))?
            .replace(tokens.clone());
        let result: Result<(), RefreshTokenError> = async {
            let refresh_response = request_chatgpt_token_refresh(
                tokens.refresh_token.clone(),
                &endpoint,
                &client_id,
                auth.client(),
                reason,
            )
            .await?;

            // An explicit override may send refresh to another authority. Retain provenance
            // only when the endpoint used by this request matches the recorded issuer.
            let provenance = tokens.oauth_provenance.clone().filter(|provenance| {
                endpoint == format!("{}/oauth/token", provenance.issuer.trim_end_matches('/'))
            });

            let storage = Arc::clone(auth.storage());
            let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
            tokio::task::spawn_blocking(move || {
                originator.sync_scope(|| {
                    persist_tokens(
                        &storage,
                        &tokens,
                        provenance,
                        refresh_response.id_token,
                        refresh_response.access_token,
                        refresh_response.refresh_token,
                    )
                })
            })
            .await
            .map_err(|_| {
                RefreshTokenError::Transient(std::io::Error::other(
                    "credential persistence task failed",
                ))
            })?
            .map_err(RefreshTokenError::from)?;
            Ok(())
        }
        .await;
        // Dropping this future while exchange or persistence is pending leaves
        // the attempted generation recorded. A known failure cannot settle an
        // earlier cancelled attempt; successful persistence can.
        *self
            .unsettled_chatgpt_refresh
            .lock()
            .map_err(|_| std::io::Error::other("Refresh progress is unavailable."))? =
            if result.is_ok() {
                None
            } else {
                previous_unsettled
            };
        result?;
        self.reload().await;

        Ok(())
    }
}
