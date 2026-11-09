//! Writes and removes stored credentials for login and logout flows.

use super::*;

/// Delete credentials for `codex_home` through the selected storage backend.
/// Returns `Ok(true)` if credentials were removed, `Ok(false)` if none were present.
pub fn logout(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::File && !codex_home.try_exists()? {
        return Ok(false);
    }
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _commit = storage.lock_for_write()?;
    storage.delete()
}

pub async fn logout_with_revoke(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<bool> {
    let auth_dot_json = match load_auth_dot_json(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    ) {
        Ok(auth_dot_json) => auth_dot_json,
        Err(err) => {
            tracing::warn!("failed to load stored auth during logout: {err}");
            None
        }
    };
    if let Err(err) = revoke_auth_tokens(auth_dot_json.as_ref(), auth_route_config).await {
        tracing::warn!("failed to revoke auth tokens during logout: {err}");
    }
    logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

/// Store API-key credentials through the selected storage backend.
pub fn login_with_api_key(
    codex_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

/// Validate and store personal-access-token or Agent Identity credentials through
/// the selected storage backend.
pub async fn login_with_access_token(
    codex_home: &Path,
    access_token: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<()> {
    let auth_dot_json = match classify_codex_access_token(access_token) {
        CodexAccessToken::PersonalAccessToken(access_token) => {
            let auth = PersonalAccessTokenAuth::load(access_token, auth_route_config).await?;
            ensure_auth_workspace_allowed(forced_chatgpt_workspace_id, auth.account_id())?;
            AuthDotJson {
                // Infer PAT auth from the credential field so older Codex builds can still
                // deserialize auth.json after a rollback.
                auth_mode: None,
                openai_api_key: None,
                tokens: None,
                last_refresh: None,
                agent_identity: None,
                personal_access_token: Some(access_token.to_string()),
                bedrock_api_key: None,
                bedrock_access_keys: None,
            }
        }
        CodexAccessToken::AgentIdentityJwt(jwt) => {
            let record = AgentIdentityAuthRecord::from_agent_identity_jwt(jwt)?;
            ensure_auth_workspace_allowed(forced_chatgpt_workspace_id, &record.account_id)?;
            let base_url = chatgpt_base_url
                .unwrap_or(ChatGptEnvironment::default().chatgpt_base_url())
                .trim_end_matches('/')
                .to_string();
            verified_record_from_jwt(jwt, &base_url, auth_route_config).await?;
            AuthDotJson {
                auth_mode: Some(AuthMode::AgentIdentity),
                openai_api_key: None,
                tokens: None,
                last_refresh: None,
                agent_identity: Some(AgentIdentityStorage::Jwt(jwt.to_string())),
                personal_access_token: None,
                bedrock_api_key: None,
                bedrock_access_keys: None,
            }
        }
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

/// Writes an in-memory auth payload for externally managed ChatGPT tokens.
pub fn login_with_chatgpt_auth_tokens(
    codex_home: &Path,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson::from_external_access_token(
        access_token,
        chatgpt_account_id,
        chatgpt_plan_type,
    )?;
    save_auth(
        codex_home,
        &auth_dot_json,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
}

/// Persist the provided auth payload using the specified backend.
pub fn save_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let _commit = storage.lock_for_write()?;
    storage.save(auth)
}

/// Load the raw stored auth payload without applying environment overrides.
///
/// Returns `None` when no credentials are stored. Prefer `AuthManager` for
/// ordinary production reads; this helper is for tests and write-side
/// maintenance that must inspect the exact payload in storage.
pub fn load_auth_dot_json(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<Option<AuthDotJson>> {
    let _commit = auth_storage_commit_guard()?;
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    storage.load()
}

pub(super) fn logout_with_message(
    codex_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    // External auth tokens live in the ephemeral store, but persistent auth may still exist
    // from earlier logins. Clear both so a forced logout truly removes all active auth.
    let removal_result = logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let error_message = match removal_result {
        Ok(_) => message,
        Err(err) => format!("{message}. Failed to remove stored credentials: {err}"),
    };
    Err(std::io::Error::other(error_message))
}

fn logout_all_stores(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return logout(
            codex_home,
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        );
    }
    let removed_ephemeral = logout(
        codex_home,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )?;
    let removed_managed = logout(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )?;
    Ok(removed_ephemeral || removed_managed)
}

impl AuthManager {
    /// Log out by deleting credentials from the selected storage backend and the
    /// ephemeral store. Returns `Ok(true)` if credentials were removed, `Ok(false)`
    /// if none were present. On success, reloads the in-memory auth cache so
    /// callers immediately observe the unauthenticated state.
    pub async fn logout(&self) -> std::io::Result<bool> {
        self.ensure_logout_allowed()?;
        let removed = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?;
        // Always reload to clear cached auth, even if no stored credentials were present.
        self.invalidate_pending_auth_reads();
        self.clear_external_auth();
        self.reload().await;
        Ok(removed)
    }

    pub async fn logout_with_revoke(&self) -> std::io::Result<bool> {
        self.ensure_logout_allowed()?;
        let auth_dot_json = self
            .auth_cached()
            .and_then(|auth| auth.get_current_auth_json());
        if let Err(err) = revoke_auth_tokens(auth_dot_json.as_ref(), &self.auth_route_config).await
        {
            tracing::warn!("failed to revoke auth tokens during logout: {err}");
        }
        let result = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?;
        // Always reload to clear cached auth, even if no stored credentials were present.
        self.invalidate_pending_auth_reads();
        self.clear_external_auth();
        self.reload().await;
        Ok(result)
    }

    fn ensure_logout_allowed(&self) -> std::io::Result<()> {
        if self.workload_identity_selected {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "workload identity auth is managed by the host and cannot be logged out",
            ));
        }
        Ok(())
    }
}
