mod agent_identity_bootstrap;
mod auth_cache;
mod auth_loading;
mod codex_auth;
mod construction;
mod external_auth;
mod login_storage;
mod token_refresh;
mod unauthorized_recovery;
mod workspace_routing;
mod workspace_switching;
pub use workspace_switching::WorkspaceLogin;
pub use workspace_switching::WorkspaceSwitch;

#[path = "account_grant.rs"]
mod account_grant;
pub use account_grant::AccountOAuthGrant;
pub use account_grant::AccountOAuthGrantLease;
pub use account_grant::AccountOAuthGrantRevision;
use account_grant::same_account_grant;
use account_grant::same_account_login;

use chrono::Utc;
use http::StatusCode;
use serde::Deserialize;
#[cfg(test)]
use serial_test::serial;
use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fmt::Debug;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tracing::instrument;

use codex_agent_identity::ChatGptEnvironment;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;
use codex_workload_identity::ApiTokenMetadata;

use super::access_token::CodexAccessToken;
use super::access_token::classify_codex_access_token;
use super::agent_identity::ManagedChatGptAgentIdentityBinding;
use super::agent_identity::agent_identity_authapi_base_url;
use super::agent_identity::classify_bootstrap_error;
use super::agent_identity::record_matches_managed_chatgpt_binding;
use super::agent_identity::record_needs_task_registration;
use super::agent_identity::register_managed_chatgpt_agent_identity;
use super::agent_identity::require_agent_identity_authapi_base_url;
use super::agent_identity::verified_record_from_jwt;
use super::api_key::is_fedramp_api_key;
use super::change_state::AuthChangeState;
use super::change_state::AuthOwner;
use super::change_state::same_login;
use super::change_state::same_owner;
use super::external_bearer::BearerTokenRefresher;
use super::revoke::revoke_auth_tokens;
use super::workload_identity::WorkloadIdentityExternalAuth;
use super::workload_identity::WorkloadIdentitySessionError;
use crate::auth::AuthHeaders;
pub use crate::auth::agent_identity::AgentIdentityAuth;
pub use crate::auth::agent_identity::AgentIdentityAuthError;
pub use crate::auth::bedrock_access_keys::BedrockAccessKeysAuth;
pub use crate::auth::bedrock_api_key::BedrockApiKeyAuth;
pub use crate::auth::personal_access_token::PersonalAccessTokenAuth;
pub use crate::auth::storage::AgentIdentityAuthRecord;
pub use crate::auth::storage::AgentIdentityStorage;
pub use crate::auth::storage::AuthDotJson;
pub use crate::auth::storage::AuthKeyringBackendKind;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::auth_storage_commit_guard;
use crate::auth::storage::create_auth_storage;
use crate::default_client::create_client;
use crate::default_client::create_default_auth_client;
use crate::oauth::ErrorBodyLimit;
use crate::oauth::OAuthClient;
use crate::oauth::OAuthError;
use crate::oauth::RefreshReason;
use crate::oauth::RefreshTokenGrant;
use crate::oauth::TokenEncoding;
use crate::oauth::TokenEndpoint;
use crate::outbound_proxy::AuthRouteConfig;
use crate::token_data::AccountOAuthProvenance;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_account_user_id;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::token_data::parse_jwt_expiration;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AuthCredentialsStoreMode;
use codex_http_client::CHATGPT_ACCOUNT_ID_HEADER;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::auth::RefreshTokenFailedError;
use codex_protocol::auth::RefreshTokenFailedReason;
use codex_protocol::protocol::SessionSource;
use thiserror::Error;
pub use workspace_routing::WorkspaceRouting;
pub use workspace_routing::WorkspaceRoutingRequest;
pub use workspace_routing::WorkspaceRoutingResolver;
pub use workspace_routing::WorkspaceRoutingSession;

pub use auth_loading::enforce_login_restrictions;
pub use auth_loading::read_codex_access_token_from_env;
pub use auth_loading::read_codex_api_key_from_env;
pub use auth_loading::read_openai_api_key_from_env;
pub use login_storage::load_auth_dot_json;
pub use login_storage::login_with_access_token;
pub use login_storage::login_with_api_key;
pub use login_storage::login_with_chatgpt_auth_tokens;
pub use login_storage::logout;
pub use login_storage::logout_with_revoke;
pub use login_storage::save_auth;
pub use token_refresh::oauth_client_id;

use super::source;
use auth_loading::ensure_auth_workspace_allowed;
use auth_loading::load_auth;
use auth_loading::validate_auth_restrictions;
use login_storage::logout_with_message;
use token_refresh::refresh_token_endpoint;
use token_refresh::same_login_for_persistence;

#[cfg(test)]
use auth_loading::enforce_login_restrictions_with_agent_identity_authapi_base_url;
#[cfg(test)]
use construction::auth_config_from;
#[cfg(test)]
pub(super) use token_refresh::persist_tokens;

/// Authentication mechanism used by the current user.
#[derive(Debug, Clone)]
pub enum CodexAuth {
    ApiKey(ApiKeyAuth),
    Chatgpt(ChatgptAuth),
    ChatgptAuthTokens(ChatgptAuthTokens),
    Headers(AuthHeaders),
    AgentIdentity(AgentIdentityAuth),
    PersonalAccessToken(PersonalAccessTokenAuth),
    BedrockApiKey(BedrockApiKeyAuth),
    BedrockAccessKeys(BedrockAccessKeysAuth),
}

/// Policy for resolving Agent Identity auth from a broader Codex auth snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentIdentityAuthPolicy {
    /// Use Agent Identity auth only when the current auth is already Agent Identity.
    JwtOnly,
    /// Allow managed ChatGPT auth to register or reuse Agent Identity auth.
    ChatGptAuth,
}

const AGENT_IDENTITY_BOOTSTRAP_FAILURE_COOLDOWN: Duration = Duration::from_secs(60 * 60);

#[derive(Debug)]
struct CachedAgentIdentityBootstrapFailure {
    account_id: String,
    authapi_base_url: String,
    retry_at: Instant,
    error: AgentIdentityAuthError,
}

#[derive(Debug, Default)]
struct AgentIdentityBootstrapCooldown {
    failure: Option<CachedAgentIdentityBootstrapFailure>,
}

impl PartialEq for CodexAuth {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Headers(a), Self::Headers(b)) => a == b,
            (Self::PersonalAccessToken(a), Self::PersonalAccessToken(b)) => a == b,
            (Self::BedrockApiKey(a), Self::BedrockApiKey(b)) => a == b,
            (Self::BedrockAccessKeys(a), Self::BedrockAccessKeys(b)) => a == b,
            _ => self.api_auth_mode() == other.api_auth_mode(),
        }
    }
}

#[derive(Clone)]
pub struct ApiKeyAuth {
    pub(super) api_key: String,
    pub(super) workload_identity: Option<ApiTokenMetadata>,
}

impl Debug for ApiKeyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyAuth")
            .field("api_key", &"[redacted]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct ChatgptAuth {
    state: ChatgptAuthState,
    storage: Arc<dyn AuthStorageBackend>,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuthTokens {
    state: ChatgptAuthState,
}

#[derive(Debug, Clone)]
struct ChatgptAuthState {
    auth_dot_json: Arc<Mutex<Option<AuthDotJson>>>,
    client: HttpClient,
}

const TOKEN_REFRESH_INTERVAL: i64 = 8;
const CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES: i64 = 5;

const REFRESH_TOKEN_EXPIRED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.";
const REFRESH_TOKEN_REUSED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.";
const REFRESH_TOKEN_INVALIDATED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
const REFRESH_TOKEN_UNKNOWN_MESSAGE: &str =
    "Your access token could not be refreshed. Please log out and sign in again.";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(super) const REVOKE_TOKEN_URL: &str = "https://auth.openai.com/oauth/revoke";
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
pub const REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REVOKE_TOKEN_URL_OVERRIDE";
pub const CLIENT_ID_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_LOGIN_CLIENT_ID";
static NEXT_DUMMY_AUTH_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum RefreshTokenError {
    #[error("{0}")]
    Permanent(#[from] RefreshTokenFailedError),
    #[error(transparent)]
    Transient(#[from] std::io::Error),
    /// Denied by application policy; neither retry nor cache as a credential failure.
    #[error(transparent)]
    Policy(#[from] codex_http_client::NetworkPolicyDenied),
}

/// Error returned when constructing an [`AuthManager`] from resolved configuration.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct AuthManagerInitializationError(AuthManagerInitializationErrorSource);

#[derive(Debug, Error)]
enum AuthManagerInitializationErrorSource {
    #[error(transparent)]
    WorkloadIdentityConfiguration(WorkloadIdentitySessionError),
    #[error(transparent)]
    InitialAuth(RefreshTokenError),
}

impl From<WorkloadIdentitySessionError> for AuthManagerInitializationError {
    fn from(error: WorkloadIdentitySessionError) -> Self {
        Self(AuthManagerInitializationErrorSource::WorkloadIdentityConfiguration(error))
    }
}

impl From<RefreshTokenError> for AuthManagerInitializationError {
    fn from(error: RefreshTokenError) -> Self {
        Self(AuthManagerInitializationErrorSource::InitialAuth(error))
    }
}

impl From<AuthManagerInitializationError> for std::io::Error {
    fn from(error: AuthManagerInitializationError) -> Self {
        Self::other(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalAuthRefreshReason {
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthRefreshContext {
    pub reason: ExternalAuthRefreshReason,
    pub previous_account_id: Option<String>,
}

/// Pluggable auth provider used by `AuthManager` for externally managed auth flows.
///
/// Implementations own the current auth value and any source-specific refresh mechanism.
pub trait ExternalAuth: Send + Sync {
    /// Returns the provider's current auth value.
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Refreshes auth and makes the returned value current for future `resolve()` calls.
    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Maps a provider error into the retry policy used by external-auth reload and recovery.
    fn classify_error(&self, error: std::io::Error) -> RefreshTokenError {
        RefreshTokenError::Transient(error)
    }
}

struct FixedApiKeyAuth(CodexAuth);

impl ExternalAuth for FixedApiKeyAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        self.resolve()
    }
}

pub type ExternalAuthFuture<'a, T> = Pin<Box<dyn Future<Output = std::io::Result<T>> + Send + 'a>>;

impl RefreshTokenError {
    pub fn failed_reason(&self) -> Option<RefreshTokenFailedReason> {
        match self {
            Self::Permanent(error) => Some(error.reason),
            Self::Transient(_) | Self::Policy(_) => None,
        }
    }
}

impl From<codex_http_client::HttpError> for RefreshTokenError {
    fn from(error: codex_http_client::HttpError) -> Self {
        match error {
            codex_http_client::HttpError::Policy(denied) => Self::Policy(denied),
            error => Self::Transient(std::io::Error::other(error)),
        }
    }
}

impl From<RefreshTokenError> for std::io::Error {
    fn from(err: RefreshTokenError) -> Self {
        match err {
            RefreshTokenError::Permanent(failed) => std::io::Error::other(failed),
            RefreshTokenError::Transient(inner) => inner,
            RefreshTokenError::Policy(error) => {
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, error)
            }
        }
    }
}

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CODEX_API_KEY_ENV_VAR: &str = "CODEX_API_KEY";
pub const CODEX_ACCESS_TOKEN_ENV_VAR: &str = "CODEX_ACCESS_TOKEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub codex_home: PathBuf,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub keyring_backend_kind: AuthKeyringBackendKind,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub chatgpt_base_url: Option<String>,
    pub forced_chatgpt_workspace_id: Option<Vec<String>>,
    pub managed_auth_policy: ManagedAuthPolicy,
    pub auth_route_config: AuthRouteConfig,
}

#[derive(Deserialize, Clone)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

// Shared constant for token refresh (client id used for oauth token refresh flow)
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

struct AuthReload {
    changed: bool,
    changed_for_refresh: bool,
}

/// Internal cached auth state.
#[derive(Clone)]
struct CachedAuth {
    auth: Option<CodexAuth>,
    /// Permanent refresh failure cached for the current auth snapshot so
    /// later refresh attempts for the same credentials fail fast without network.
    permanent_refresh_failure: Option<AuthScopedRefreshFailure>,
}

#[derive(Clone)]
struct AuthScopedRefreshFailure {
    auth: CodexAuth,
    error: RefreshTokenFailedError,
}

impl Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field(
                "auth_mode",
                &self.auth.as_ref().map(CodexAuth::api_auth_mode),
            )
            .field(
                "permanent_refresh_failure",
                &self
                    .permanent_refresh_failure
                    .as_ref()
                    .map(|failure| failure.error.reason),
            )
            .finish()
    }
}

enum UnauthorizedRecoveryStep {
    Reload,
    RefreshToken,
    ExternalRefresh,
    Done,
}

enum ReloadOutcome {
    /// Reload was performed and the cached auth changed
    ReloadedChanged,
    /// Reload was performed and the cached auth remained the same
    ReloadedNoChange,
    /// Reload was skipped (missing or mismatched account id)
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnauthorizedRecoveryMode {
    Managed,
    External,
}

// UnauthorizedRecovery is a state machine that handles an attempt to refresh the authentication when requests
// to API fail with 401 status code.
// The client calls next() every time it encounters a 401 error, one time per retry.
// For API key based authentication, we don't do anything and let the error bubble to the user.
//
// For ChatGPT based authentication, we:
// 1. Attempt to reload the auth data from disk. We only reload if the account id matches the one the current process is running as.
// 2. Attempt to refresh the token using OAuth token refresh flow.
// If after both steps the server still responds with 401 we let the error bubble to the user.
//
// For external auth sources, UnauthorizedRecovery retries once by asking the
// configured provider to refresh and caching the returned auth through the same
// path used by other auth sources.
pub struct UnauthorizedRecovery {
    manager: Arc<AuthManager>,
    step: UnauthorizedRecoveryStep,
    expected_account_id: Option<String>,
    mode: UnauthorizedRecoveryMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnauthorizedRecoveryStepResult {
    auth_state_changed: Option<bool>,
}

/// Central manager providing a single source of truth for stored
/// authentication data. It loads once (or on preference change) and then
/// hands out cloned `CodexAuth` values so the rest of the program has a
/// consistent snapshot.
///
/// External modifications to stored credentials will NOT be observed until
/// `reload()` is called explicitly. This matches the design goal of avoiding
/// different parts of the program seeing inconsistent auth data mid‑run.
pub struct AuthManager {
    codex_home: PathBuf,
    inner: RwLock<CachedAuth>,
    auth_change_tx: watch::Sender<u64>,
    auth_change_state_tx: watch::Sender<AuthChangeState>,
    workspace_routing_resolver: OnceLock<Weak<dyn WorkspaceRoutingResolver>>,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: RwLock<Option<Vec<String>>>,
    managed_auth_policy: ManagedAuthPolicy,
    chatgpt_base_url: Option<String>,
    agent_identity_authapi_base_url: Option<String>,
    refresh_lock: Arc<Semaphore>,
    // Writers hold refresh_lock; cancellation retains the attempted generation.
    unsettled_chatgpt_refresh: Mutex<Option<TokenData>>,
    agent_identity_lock: Semaphore,
    agent_identity_bootstrap_cooldown: Mutex<AgentIdentityBootstrapCooldown>,
    external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
    workload_identity_selected: bool,
    auth_route_config: AuthRouteConfig,
    // Provider credentials follow the primary account's policy without owning its lifecycle.
    owns_network_policy: bool,
    allowed_auth_modes: RwLock<Option<Vec<AuthMode>>>,
}

/// Configuration view required to construct a shared [`AuthManager`].
///
/// Implementations should return the auth-related config values for the
/// already-resolved runtime configuration. The primary implementation is
/// `codex_core::config::Config`, but this trait keeps `codex-login` independent
/// from `codex-core`.
pub trait AuthManagerConfig {
    /// Returns the Codex home directory used for auth storage.
    fn codex_home(&self) -> PathBuf;

    /// Returns the CLI auth credential storage mode for auth loading.
    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode;

    /// Returns the backend to use when CLI auth keyring storage is selected.
    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind;

    /// Returns the resolved login-method restriction, if any.
    fn forced_login_method(&self) -> Option<ForcedLoginMethod>;

    /// Returns the workspace IDs that ChatGPT auth should be restricted to, if any.
    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>>;

    /// Returns administrator-managed authentication restrictions.
    fn managed_auth_policy(&self) -> ManagedAuthPolicy;

    /// Returns the ChatGPT backend base URL used for first-party backend authorization.
    fn chatgpt_base_url(&self) -> String;

    /// Returns route-selection settings for auth-owned clients.
    fn auth_route_config(&self) -> AuthRouteConfig;
}

/// Runtime storage and network policy shared by independent credential managers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRuntimeConfig {
    pub codex_home: PathBuf,
    pub auth_route_config: AuthRouteConfig,
}

impl Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("codex_home", &self.codex_home)
            .field("inner", &self.inner)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .field("keyring_backend_kind", &self.keyring_backend_kind)
            .field("forced_login_method", &self.forced_login_method)
            .field(
                "forced_chatgpt_workspace_id",
                &self.forced_chatgpt_workspace_id,
            )
            .field("managed_auth_policy", &self.managed_auth_policy)
            .field("chatgpt_base_url", &self.chatgpt_base_url)
            .field("auth_route_config", &self.auth_route_config)
            .field("has_external_auth", &self.has_external_auth())
            .field(
                "workload_identity_selected",
                &self.workload_identity_selected,
            )
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "change_state_tests.rs"]
mod change_state_tests;

#[cfg(test)]
#[path = "account_user_id_tests.rs"]
mod account_user_id_tests;

#[cfg(test)]
#[path = "source_tests.rs"]
mod source_tests;
