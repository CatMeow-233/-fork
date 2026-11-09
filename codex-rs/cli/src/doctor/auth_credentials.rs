//! Auth check: provider auth requirements, auth env vars, and stored credential completeness.

use super::*;

pub(super) fn auth_check(config: &Config) -> DoctorCheck {
    let mut details = Vec::new();
    let auth_path = config.codex_home.join("auth.json");
    details.push(format!(
        "auth storage mode: {:?}",
        config.cli_auth_credentials_store_mode
    ));
    details.push(format!("auth file: {}", auth_path.display()));

    let env_auth_vars = [
        OPENAI_API_KEY_ENV_VAR,
        CODEX_API_KEY_ENV_VAR,
        CODEX_ACCESS_TOKEN_ENV_VAR,
    ]
    .into_iter()
    .filter(|name| env_var_present(name))
    .collect::<Vec<_>>();
    if !env_auth_vars.is_empty() {
        details.push(format!(
            "auth env vars present: {}",
            env_auth_vars.join(", ")
        ));
    }
    if let Some(check) = provider_specific_auth_check(
        config.model_provider.requires_openai_auth,
        config.model_provider.env_key.as_deref(),
        config.model_provider.env_key_instructions.as_deref(),
        details.clone(),
        env_var_present,
    ) {
        return check;
    }

    match load_auth_dot_json(
        &config.codex_home,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    ) {
        Ok(Some(auth)) => {
            details.push(format!("stored auth mode: {}", stored_auth_mode(&auth)));
            details.push(format!("stored API key: {}", auth.openai_api_key.is_some()));
            details.push(format!("stored ChatGPT tokens: {}", auth.tokens.is_some()));
            details.push(format!(
                "stored agent identity: {}",
                auth.agent_identity.is_some()
            ));
            let auth_issues = stored_auth_issues(&auth, env_var_present);
            details.extend(
                auth_issues
                    .iter()
                    .map(|issue| format!("stored auth issue: {issue}")),
            );
            let status = if !auth_issues.is_empty() && env_auth_vars.is_empty() {
                CheckStatus::Fail
            } else if !auth_issues.is_empty() || env_auth_vars.len() > 1 {
                CheckStatus::Warning
            } else {
                CheckStatus::Ok
            };
            let summary = match status {
                CheckStatus::Ok => "auth is configured",
                CheckStatus::Warning if !auth_issues.is_empty() => {
                    "auth is provided by environment, but stored credentials are incomplete"
                }
                CheckStatus::Warning => {
                    "auth is configured, but multiple auth env vars are present"
                }
                CheckStatus::Fail => "stored credentials are incomplete",
            };
            let mut check =
                DoctorCheck::new("auth.credentials", "auth", status, summary).details(details);
            if status == CheckStatus::Fail {
                check =
                    check.remediation("Run codex login again or provide a supported auth env var.");
            }
            check
        }
        Ok(None) if !env_auth_vars.is_empty() => DoctorCheck::new(
            "auth.credentials",
            "auth",
            CheckStatus::Ok,
            "auth is provided by environment",
        )
        .details(details),
        Ok(None) => DoctorCheck::new(
            "auth.credentials",
            "auth",
            CheckStatus::Fail,
            "no Codex credentials were found",
        )
        .details(details)
        .remediation("Run codex login or provide an API key through a supported auth env var."),
        Err(err) => DoctorCheck::new(
            "auth.credentials",
            "auth",
            CheckStatus::Fail,
            "stored credentials could not be read",
        )
        .detail(err.to_string())
        .remediation("Fix auth storage access or run codex login again."),
    }
}

pub(super) fn provider_specific_auth_check(
    requires_openai_auth: bool,
    provider_env_key: Option<&str>,
    provider_env_key_instructions: Option<&str>,
    mut details: Vec<String>,
    env_var_present: impl Fn(&str) -> bool,
) -> Option<DoctorCheck> {
    details.push(format!(
        "model provider requires OpenAI auth: {requires_openai_auth}"
    ));
    if requires_openai_auth {
        return None;
    }

    match provider_env_key {
        Some(env_key) if env_var_present(env_key) => {
            details.push(format!("provider auth env var: {env_key} (present)"));
            Some(
                DoctorCheck::new(
                    "auth.credentials",
                    "auth",
                    CheckStatus::Ok,
                    "auth is provided by the active model provider",
                )
                .details(details),
            )
        }
        Some(env_key) => {
            details.push(format!("provider auth env var: {env_key} (missing)"));
            let remediation = provider_env_key_instructions
                .map(str::to_string)
                .unwrap_or_else(|| format!("Set {env_key} for the active model provider."));
            Some(
                DoctorCheck::new(
                    "auth.credentials",
                    "auth",
                    CheckStatus::Fail,
                    "active model provider auth env var is missing",
                )
                .details(details)
                .remediation(remediation),
            )
        }
        None => Some(
            DoctorCheck::new(
                "auth.credentials",
                "auth",
                CheckStatus::Ok,
                "OpenAI auth is not required for the active model provider",
            )
            .details(details),
        ),
    }
}

pub(super) fn stored_auth_mode(auth: &codex_login::AuthDotJson) -> &'static str {
    match stored_auth_mode_value(auth) {
        AuthMode::ApiKey => "api_key",
        AuthMode::Chatgpt => "chatgpt",
        AuthMode::ChatgptAuthTokens => "chatgpt_auth_tokens",
        AuthMode::Headers => "headers",
        AuthMode::AgentIdentity => "agent_identity",
        AuthMode::PersonalAccessToken => "personal_access_token",
        AuthMode::BedrockApiKey => "bedrock_api_key",
        AuthMode::BedrockAccessKeys => "bedrock_access_keys",
    }
}

pub(super) fn stored_auth_mode_value(auth: &AuthDotJson) -> AuthMode {
    if let Some(mode) = auth.auth_mode {
        return mode;
    }
    if auth.personal_access_token.is_some() {
        AuthMode::PersonalAccessToken
    } else if auth.bedrock_api_key.is_some() {
        AuthMode::BedrockApiKey
    } else if auth.bedrock_access_keys.is_some() {
        AuthMode::BedrockAccessKeys
    } else if auth.openai_api_key.is_some() {
        AuthMode::ApiKey
    } else {
        AuthMode::Chatgpt
    }
}

pub(super) fn stored_auth_issues(
    auth: &AuthDotJson,
    env_var_present: impl Fn(&str) -> bool,
) -> Vec<&'static str> {
    let mut issues = Vec::new();
    match stored_auth_mode_value(auth) {
        AuthMode::ApiKey => {
            let stored_key_present = auth
                .openai_api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty());
            let env_key_present =
                env_var_present(OPENAI_API_KEY_ENV_VAR) || env_var_present(CODEX_API_KEY_ENV_VAR);
            if !stored_key_present && !env_key_present {
                issues.push("API key auth is missing an API key");
            }
        }
        AuthMode::Chatgpt => {
            match auth.tokens.as_ref() {
                Some(tokens) => {
                    if tokens.access_token.trim().is_empty() {
                        issues.push("ChatGPT auth is missing an access token");
                    }
                    if tokens.refresh_token.trim().is_empty() {
                        issues.push("ChatGPT auth is missing a refresh token");
                    }
                }
                None => issues.push("ChatGPT auth is missing token data"),
            }
            if auth.last_refresh.is_none() {
                issues.push("ChatGPT auth is missing refresh metadata");
            }
        }
        AuthMode::ChatgptAuthTokens => {
            match auth.tokens.as_ref() {
                Some(tokens) => {
                    if tokens.access_token.trim().is_empty() {
                        issues.push("external ChatGPT auth is missing an access token");
                    }
                    if tokens.account_id.is_none() && tokens.id_token.chatgpt_account_id.is_none() {
                        issues.push("external ChatGPT auth is missing a ChatGPT account id");
                    }
                }
                None => issues.push("external ChatGPT auth is missing token data"),
            }
            if auth.last_refresh.is_none() {
                issues.push("external ChatGPT auth is missing refresh metadata");
            }
        }
        AuthMode::Headers => {
            issues.push("header auth cannot be loaded from auth storage");
        }
        AuthMode::AgentIdentity => {
            if auth
                .agent_identity
                .as_ref()
                .is_none_or(|agent_identity| !agent_identity.has_auth_material())
            {
                issues.push("agent identity auth is missing an agent identity token");
            }
        }
        AuthMode::PersonalAccessToken => {
            if auth
                .personal_access_token
                .as_deref()
                .is_none_or(|token| token.trim().is_empty())
            {
                issues.push("personal access token auth is missing a personal access token");
            }
        }
        AuthMode::BedrockApiKey => {
            if auth.bedrock_api_key.is_none() {
                issues.push("Bedrock API key auth is missing a Bedrock API key");
            }
        }
        AuthMode::BedrockAccessKeys => match auth.bedrock_access_keys.as_ref() {
            Some(access_keys) => {
                if access_keys.access_key_id.trim().is_empty() {
                    issues.push("Bedrock access key auth is missing an access key ID");
                }
                if access_keys.secret_access_key.trim().is_empty() {
                    issues.push("Bedrock access key auth is missing a secret access key");
                }
            }
            None => issues.push("Bedrock access key auth is missing AWS access keys"),
        },
    }
    issues
}
