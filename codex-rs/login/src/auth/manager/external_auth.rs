//! Installs, resolves, and refreshes credentials supplied by an `ExternalAuth` provider.

use super::*;

fn permanent_external_auth_error(message: impl Into<String>) -> RefreshTokenError {
    RefreshTokenError::Permanent(RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Other,
        message,
    ))
}

impl AuthManager {
    pub async fn set_external_auth(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> Result<(), RefreshTokenError> {
        if self.workload_identity_selected {
            return Err(permanent_external_auth_error(
                "workload identity auth cannot be replaced at runtime",
            ));
        }
        self.install_external_auth(external_auth).await
    }

    pub(super) async fn install_external_auth(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> Result<(), RefreshTokenError> {
        let auth = self.resolve_external_auth(external_auth.as_ref()).await?;
        let mut external_auth_slot = self.external_auth.write().map_err(|_| {
            RefreshTokenError::Transient(std::io::Error::other("external auth lock is poisoned"))
        })?;
        *external_auth_slot = Some(external_auth);
        drop(external_auth_slot);
        if let Ok(mut guard) = self.inner.write() {
            guard.permanent_refresh_failure = None;
        }
        self.commit_external_auth(auth)
    }

    pub fn clear_external_auth(&self) {
        if self.workload_identity_selected {
            return;
        }
        if let Ok(mut external_auth) = self.external_auth.write()
            && external_auth.take().is_some()
        {
            self.set_cached_auth(/*new_auth*/ None);
        }
    }

    pub fn has_external_auth(&self) -> bool {
        self.external_auth_provider().is_some()
    }

    pub(super) fn external_auth_provider(&self) -> Option<Arc<dyn ExternalAuth>> {
        self.external_auth
            .read()
            .ok()
            .and_then(|external_auth| external_auth.clone())
    }

    pub(super) fn has_refreshable_external_auth(&self) -> bool {
        self.has_external_auth()
            && self
                .auth_cached()
                .as_ref()
                .is_none_or(|auth| auth.is_api_key_auth() || auth.supports_unauthorized_recovery())
    }

    pub(super) async fn resolve_external_auth(
        &self,
        external_auth: &dyn ExternalAuth,
    ) -> Result<CodexAuth, RefreshTokenError> {
        let auth = external_auth
            .resolve()
            .await
            .map_err(|error| external_auth.classify_error(error))?;
        self.validate_external_auth(&auth, external_auth)?;
        Ok(auth)
    }

    pub(super) async fn refresh_external_auth(
        &self,
        reason: ExternalAuthRefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let Some(external_auth) = self.external_auth_provider() else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth is not configured",
            )));
        };
        let previous_account_id = self
            .auth_cached()
            .as_ref()
            .and_then(CodexAuth::get_account_id);
        let context = ExternalAuthRefreshContext {
            reason,
            previous_account_id,
        };

        let refreshed = external_auth
            .refresh(context)
            .await
            .map_err(|error| external_auth.classify_error(error))?;
        self.validate_external_auth(&refreshed, external_auth.as_ref())?;
        self.commit_external_auth(refreshed)?;
        Ok(())
    }

    fn commit_external_auth(&self, auth: CodexAuth) -> Result<(), RefreshTokenError> {
        if auth.is_external_chatgpt_tokens() {
            let auth_dot_json = auth.get_current_auth_json().ok_or_else(|| {
                RefreshTokenError::Transient(std::io::Error::other(
                    "external ChatGPT auth tokens are missing auth state",
                ))
            })?;
            // Independent AuthManagers share external ChatGPT auth through the process-local store.
            save_auth(
                &self.codex_home,
                &auth_dot_json,
                AuthCredentialsStoreMode::Ephemeral,
                AuthKeyringBackendKind::default(),
            )
            .map_err(RefreshTokenError::Transient)?;
        }

        self.set_cached_auth(Some(auth));
        Ok(())
    }

    fn validate_external_auth(
        &self,
        auth: &CodexAuth,
        external_auth: &dyn ExternalAuth,
    ) -> Result<(), RefreshTokenError> {
        let allowed_login_methods = self.allowed_login_methods();
        validate_auth_restrictions(
            Some(&allowed_login_methods),
            self.effective_chatgpt_workspaces().as_deref(),
            auth,
        )
        .map_err(|error| external_auth.classify_error(std::io::Error::other(error)))
    }
}
