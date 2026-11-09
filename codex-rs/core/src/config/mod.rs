use crate::config::edit::ConfigEdit;
use crate::config::edit::ConfigEditsBuilder;
use crate::context::world_state::validate_managed_developer_instructions;
use crate::path_utils::normalize_for_native_workdir;
use crate::unified_exec::DEFAULT_MAX_BACKGROUND_TERMINAL_TIMEOUT_MS;
use crate::unified_exec::MIN_EMPTY_YIELD_TIME_MS;
use crate::windows_sandbox::WindowsSandboxLevelExt;
use crate::windows_sandbox::resolve_windows_sandbox_mode;
use crate::windows_sandbox::windows_sandbox_level_for_legacy_checks;
use codex_agent_roles::load_agent_roles;
use codex_config::CloudConfigBundleLoader;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigPathContext;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_config::ConstrainedWithSource;
use codex_config::FeatureRequirementsToml;
use codex_config::ManagedAuthPolicy;
use codex_config::McpEnterpriseManagedAuthConfig;
use codex_config::McpServerRequirement;
use codex_config::PluginRequirementsToml;
use codex_config::ProfileV2Name;
use codex_config::ResidencyRequirement;
use codex_config::SandboxModeRequirement;
use codex_config::Sourced;
use codex_config::ThreadConfigLoader;
use codex_config::apply_default_gcp_auth;
use codex_config::config_toml::CircuitBreakAction;
use codex_config::config_toml::ConfigToml;
use codex_config::config_toml::DEFAULT_PROJECT_DOC_MAX_BYTES;
use codex_config::config_toml::ProjectConfig;
use codex_config::config_toml::RealtimeAudioConfig;
use codex_config::config_toml::RealtimeConfig;
use codex_config::config_toml::ThreadStoreToml;
use codex_config::config_toml::validate_model_providers;
use codex_config::loader::load_config_layers_state;
use codex_config::loader::project_trust_key;
use codex_config::permissions_toml::PermissionProfileToml;
use codex_config::permissions_toml::PermissionsToml;
use codex_config::sandbox_mode_requirement_for_permission_profile;
use codex_config::types::ApprovalsReviewer;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthKeyringBackendKind;
use codex_config::types::History;
use codex_config::types::McpServerConfig;
use codex_config::types::MemoriesConfig;
use codex_config::types::ModelAvailabilityNuxConfig;
use codex_config::types::Notice;
use codex_config::types::OAuthCredentialsStoreMode;
use codex_config::types::QuickDial;
use codex_config::types::ResumeCwdMode;
use codex_config::types::SessionPickerViewMode;
use codex_config::types::ToolSuggestConfig;
use codex_config::types::ToolSuggestDisabledTool;
use codex_config::types::ToolSuggestDiscoverable;
use codex_config::types::TuiKeymap;
use codex_config::types::TuiNotificationSettings;
use codex_config::types::TuiPetAnchor;
use codex_config::types::UriBasedFileOpener;
use codex_config::types::WindowsSandboxModeToml;
use codex_core_plugins::PluginLoadOutcome;
use codex_core_plugins::PluginsConfigInput;
use codex_exec_server::ExecutorFileSystem;
use codex_exec_server::LOCAL_FS;
use codex_exec_server::ReadFileOptions;
use codex_features::CodeModeConfigToml;
use codex_features::CurrentTimeReminderConfigToml;
use codex_features::CurrentTimeReminderDeliveryMode;
use codex_features::CurrentTimeSource;
use codex_features::Feature;
use codex_features::FeatureConfigSource;
use codex_features::FeatureOverrides;
use codex_features::FeatureToml;
use codex_features::Features;
use codex_features::FeaturesToml;
use codex_features::MultiAgentV2ConfigToml;
use codex_features::NetworkProxyConfigToml;
use codex_features::RuntimeAuthMode;
use codex_features::SleepToolMode;
use codex_features::TokenBudgetConfigToml;
use codex_git_utils::resolve_root_git_project_for_trust;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManagerConfig;
use codex_login::AuthRouteConfig;
use codex_mcp::DEFAULT_OPTIONAL_MCP_STARTUP_GRACE;
use codex_mcp::McpConfig;
use codex_mcp::McpPluginAttribution;
use codex_mcp::McpProtocolMode;
use codex_mcp::McpServerRegistration;
use codex_mcp::ResolvedMcpCatalog;
use codex_model_provider::ChatgptBaseUrl;
use codex_model_provider::DEFAULT_CHATGPT_BASE_URL;
use codex_model_provider::ProviderCapabilities;
use codex_model_provider_info::GCP_PROVIDER_ID;
use codex_model_provider_info::GatewayOAuthConfig;
use codex_model_provider_info::LEGACY_OLLAMA_CHAT_PROVIDER_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OLLAMA_CHAT_PROVIDER_REMOVED_ERROR;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_provider_info::built_in_model_providers;
use codex_model_provider_info::merge_configured_model_providers;
use codex_models_manager::ModelsManagerConfig;
use codex_prompts::ResolvedModelMessages;
use codex_protocol::MemoryVersion;
use codex_protocol::config_types::AltScreenMode;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::config_types::SERVICE_TIER_DEFAULT_REQUEST_VALUE;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_protocol::config_types::TrustLevel;
use codex_protocol::config_types::Verbosity;
use codex_protocol::config_types::WebSearchConfig;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::PermissionProfile;
pub use codex_protocol::models::PermissionProfileSnapshot;
use codex_protocol::models::ProfileWorkspaceRoot;
use codex_protocol::models::SandboxEnforcement;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::permissions::DenyReadValidator;
use codex_protocol::permissions::DenyReadViolation;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSandboxPolicyContext;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::SandboxPolicy;
use codex_rmcp_client::McpOAuthRefreshMode;
use codex_sandboxing::SandboxType;
pub use codex_thread_store::ExtraConfig;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::AbsolutePathBufGuard;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use http::HeaderValue;
use rmcp::model::ElicitationCapability;
use rmcp::model::FormElicitationCapability;
use rmcp::model::UrlElicitationCapability;
use serde::Deserialize;
use serde::Serialize;
use serde::de::IntoDeserializer;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::config::permissions::BUILT_IN_READ_ONLY_PROFILE;
use crate::config::permissions::BUILT_IN_WORKSPACE_PROFILE;
use crate::config::permissions::default_builtin_permission_profile_name;
use crate::config::permissions::network_proxy_config_for_profile_selection;
use crate::config::permissions::validate_user_permission_profile_names;
use crate::responses_metadata::validate_extra_metadata;
use codex_network_proxy::NetworkProxyConfig;
use toml::Value as TomlValue;
use toml_edit::DocumentMut;

mod accessors;
mod auth_keyring;
mod builder;
pub mod edit;
mod feature_configs;
mod guardian_policy;
mod managed_features;
mod materialize;
mod mcp_config;
mod metrics;
mod network_config;
mod network_proxy_spec;
mod otel;
mod permission_path;
mod permission_profile_catalog;
mod permission_profile_selection;
mod permission_resolution;
mod permissions;
mod requirements;
mod resolved_permission_profile;
mod runtime_permissions;
mod setting_resolvers;
mod toml_io;
pub(crate) use guardian_policy::resolve_guardian_policy_text;
pub(crate) use guardian_policy::resolve_guardian_reviewer;
mod runtime_refresh;
pub(crate) use runtime_refresh::RuntimeConfigRefresh;
#[cfg(test)]
mod schema;
mod token_budget_startup;
mod windows_sandbox_config;
pub use accessors::log_dir;
pub use auth_keyring::bootstrap_auth_config;
pub use auth_keyring::resolve_bootstrap_auth_keyring_backend_kind;
pub use builder::find_codex_home;
pub use codex_agent_roles::AgentRoleConfig;
pub use codex_config::ConfigLoadOptions;
pub use codex_config::Constrained;
pub use codex_config::ConstraintError;
pub use codex_config::ConstraintResult;
pub use codex_config::LoaderOverrides;
pub use codex_network_proxy::NetworkProxyAuditMetadata;
use codex_sandboxing::compatibility_sandbox_policy_for_permission_profile;
pub use codex_sandboxing::system_bwrap_warning;
use feature_configs::resolve_code_mode_config;
use feature_configs::resolve_current_time_reminder_config;
use feature_configs::resolve_multi_agent_v2_config;
use feature_configs::resolve_rollout_budget_config;
pub(crate) use feature_configs::resolve_token_budget_config;
use feature_configs::validate_multi_agent_v2_wait_timeout;
pub use managed_features::ManagedFeatures;
use mcp_config::constrain_mcp_servers;
#[cfg(test)]
use mcp_config::filter_mcp_servers_by_requirements;
#[cfg(test)]
use mcp_config::filter_plugin_mcp_servers_by_requirements;
pub use mcp_config::load_global_mcp_servers;
pub(crate) use metrics::emit_session_start_metrics;
pub(crate) use metrics::legacy_multi_agent_disabled_sources;
pub use network_config::EnvironmentNetworkConfigError;
pub use network_config::NetworkConfigInputs;
pub use network_config::PreparedNetworkConfig;
pub use network_config::project_environment_profile_network;
pub use network_config::validate_environment_network_policy;
#[cfg(test)]
pub(crate) use network_proxy_spec::McpNetworkRoute;
pub use network_proxy_spec::NetworkProxySpec;
pub use network_proxy_spec::StartedNetworkProxy;
pub use permission_profile_catalog::PermissionProfileCatalogEntry;
pub use permission_profile_catalog::permission_profile_catalog;
use permission_profile_catalog::permission_profile_catalog_from_permissions;
use permission_profile_catalog::permission_profile_is_allowed;
pub use permission_profile_catalog::validate_permission_profile_for_deny_read;
pub use permission_profile_selection::ResolvedPermissionProfileSelection;
pub use permission_profile_selection::resolve_permission_profile_selection;
use permission_resolution::is_permission_allowed;
use permission_resolution::merge_managed_permission_profiles;
use permission_resolution::resolve_effective_permission_selection;
use permission_resolution::resolve_permission_config_syntax;
use permission_resolution::validate_required_permission_profile_catalog;
pub use permissions::CompiledPermissionProfile;
pub use permissions::WorkspaceWriteSettings;
pub use permissions::compile_permission_profile;
pub(crate) use permissions::is_builtin_permission_profile_name;
pub use permissions::network_proxy_config_from_profile_network;
pub use permissions::resolve_permission_profile;
pub(crate) use resolved_permission_profile::PermissionProfileState;
use runtime_permissions::build_network_proxy_spec;
use setting_resolvers::apply_requirement_constrained_value;
use setting_resolvers::dedupe_absolute_paths;
use setting_resolvers::guardian_policy_config_from_requirements;
use setting_resolvers::normalize_guardian_policy_config;
pub use setting_resolvers::resolve_bootstrap_auth_route_config;
pub use setting_resolvers::resolve_bootstrap_http_client_factory;
pub use setting_resolvers::resolve_bootstrap_respect_system_proxy;
use setting_resolvers::resolve_cli_auth_credentials_store_mode;
use setting_resolvers::resolve_experimental_request_user_input_enabled;
use setting_resolvers::resolve_mcp_oauth_credentials_store_mode;
pub use setting_resolvers::resolve_oss_provider;
use setting_resolvers::resolve_sqlite_home_env;
use setting_resolvers::resolve_tool_suggest_config;
pub(crate) use setting_resolvers::resolve_tool_suggest_config_from_layer_stack;
use setting_resolvers::resolve_update_plan_enabled;
use setting_resolvers::resolve_web_search_config;
pub(crate) use setting_resolvers::resolve_web_search_mode_for_turn;
use setting_resolvers::thread_store_config;
pub use token_budget_startup::TokenBudgetStartupConfig;
pub use toml_io::deserialize_config_toml_with_base;
pub use toml_io::load_config_as_toml_with_cli_and_load_options;
pub use toml_io::load_config_as_toml_with_cli_and_loader_overrides;
pub use toml_io::load_config_as_toml_with_cli_overrides;
pub use toml_io::load_config_toml_with_layer_stack;
use toml_io::load_model_catalog;
pub use toml_io::resolve_profile_v2_config_path;
pub use toml_io::set_default_oss_provider;
pub use toml_io::set_project_trust_level;
pub(crate) use toml_io::set_project_trust_level_inner;
pub use toml_io::validate_feature_requirements_for_config_toml;
pub use windows_sandbox_config::PreparedWindowsSandboxConfig;
use windows_sandbox_config::config_allows_mxc;
pub use windows_sandbox_config::prepare_windows_sandbox_config;
use windows_sandbox_config::resolve_windows_sandbox_type;
pub use windows_sandbox_config::windows_mxc_allowed_by_config;

const DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS: i64 = 200;
const DEFAULT_IGNORE_LARGE_UNTRACKED_FILES: i64 = 10 * 1024 * 1024;
const DEFAULT_ASYNC_COMPACTION_THRESHOLD_PERCENT: u8 = 65;

/// Signals that a public config selected the retired `untrusted` approval policy.
#[derive(Debug, thiserror::Error)]
#[error("approval_policy = \"untrusted\" is no longer supported; remove this setting")]
pub struct UnsupportedUntrustedApprovalPolicyError;

/// Compatibility-only config retained so legacy `ghost_snapshot` settings
/// continue to load even though snapshots are no longer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhostSnapshotConfig {
    pub ignore_large_untracked_files: Option<i64>,
    pub ignore_large_untracked_dirs: Option<i64>,
    pub disable_warnings: bool,
}

impl Default for GhostSnapshotConfig {
    fn default() -> Self {
        Self {
            ignore_large_untracked_files: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_FILES),
            ignore_large_untracked_dirs: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS),
            disable_warnings: false,
        }
    }
}

/// Maximum number of bytes of the documentation that will be embedded. Larger
/// files are *silently truncated* to this size so we do not take up too much of
/// the context window.
pub(crate) const AGENTS_MD_MAX_BYTES: usize = DEFAULT_PROJECT_DOC_MAX_BYTES; // 32 KiB
pub(crate) const DEFAULT_AGENT_MAX_THREADS: Option<usize> = Some(6);
pub(crate) const DEFAULT_MULTI_AGENT_V2_MAX_CONCURRENT_THREADS_PER_SESSION: usize = 4;
pub(crate) const DEFAULT_MULTI_AGENT_V2_MIN_WAIT_TIMEOUT_MS: i64 = 10_000;
pub(crate) const DEFAULT_MULTI_AGENT_V2_MAX_WAIT_TIMEOUT_MS: i64 = 3600 * 1000;
pub(crate) const DEFAULT_MULTI_AGENT_V2_DEFAULT_WAIT_TIMEOUT_MS: i64 = 30_000;
const DEFAULT_MULTI_AGENT_V2_TOOL_NAMESPACE: &str = "collaboration";

pub(crate) const HARD_MIN_MULTI_AGENT_V2_TIMEOUT_MS: i64 = 0;
pub(crate) const HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS: i64 =
    DEFAULT_MULTI_AGENT_V2_MAX_WAIT_TIMEOUT_MS;
pub(crate) const DEFAULT_AGENT_MAX_DEPTH: i32 = 1;
const LOCAL_DEV_BUILD_VERSION: &str = "0.0.0";

pub const CONFIG_TOML_FILE: &str = "config.toml";
const CONFIG_PROFILE_V2_SUFFIX: &str = ".config.toml";

#[cfg(test)]
pub(crate) async fn test_config() -> Config {
    let codex_home = tempfile::tempdir().expect("create temp dir");
    Config::load_from_base_config_with_overrides(
        ConfigToml {
            model: Some("gpt-5.5".to_string()),
            ..Default::default()
        },
        ConfigOverrides::default(),
        AbsolutePathBuf::from_absolute_path(codex_home.path()).expect("temp dir should resolve"),
    )
    .await
    .expect("load default test config")
}

/// Application configuration loaded from disk and merged with overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct Permissions {
    /// Approval policy for executing commands.
    pub approval_policy: Constrained<AskForApproval>,
    /// Constrained permission profile plus its selected profile identity, if
    /// the profile came from a built-in or named config profile.
    permission_profile_state: PermissionProfileState,
    /// Managed deny-read rules retained independently from user-defined denies.
    managed_deny_read_policy: Option<Arc<FileSystemSandboxPolicy>>,
    /// Thread-scoped runtime workspace roots. Symbolic `:workspace_roots`
    /// entries in the permission profile are materialized against these roots.
    workspace_roots: Vec<AbsolutePathBuf>,
    /// Effective network configuration applied to all spawned processes.
    pub network: Option<NetworkProxySpec>,
    /// Whether the model may request a login shell for shell-based tools.
    /// Default to `true`
    ///
    /// If `true`, the model may request a login shell (`login = true`), and
    /// omitting `login` defaults to using a login shell.
    /// If `false`, the model can never use a login shell: `login = true`
    /// requests are rejected, and omitting `login` defaults to a non-login
    /// shell.
    pub allow_login_shell: bool,
    /// Policy used to build process environments for shell/unified exec.
    pub shell_environment_policy: ShellEnvironmentPolicy,
    /// Effective Windows sandbox mode derived from `[windows].sandbox` or
    /// legacy feature keys.
    pub windows_sandbox_mode: Option<WindowsSandboxModeToml>,
    /// Configured Windows backend; use `Config::effective_local_windows_sandbox_type()` for local selection.
    pub windows_sandbox_type: SandboxType,
}

// A profile override only inherits the selected profile's proxy/allowlist config
// when Codex is still responsible for the network policy. `Disabled` means no
// outer sandbox, so starting the managed proxy would narrow the override.
fn profile_allows_configured_network_proxy(permission_profile: &PermissionProfile) -> bool {
    match permission_profile {
        PermissionProfile::Managed { network, .. } | PermissionProfile::External { network } => {
            network.is_enabled()
        }
        PermissionProfile::Disabled => false,
    }
}

/// Configured thread persistence backend.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ThreadStoreConfig {
    /// Persist threads locally using rollout JSONL files and sqlite metadata.
    #[default]
    Local,
    /// In-memory thread store for test and debug configurations.
    InMemory { id: String },
}

/// Application configuration loaded from disk and merged with overrides.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// App-server-owned destination policy; other runtimes remain unmanaged.
    pub application_network_policy: codex_http_client::NetworkPolicy,
    /// Auth bootstrap routing installed by the app-server configuration owner.
    pub application_auth_route_config: Option<AuthRouteConfig>,
    /// Provenance for how this [`Config`] was derived (merged layers + enforced
    /// requirements).
    pub config_layer_stack: ConfigLayerStack,

    /// Plugin settings resolved with this snapshot, before capability admission.
    pub plugins: codex_config::types::PluginsConfigToml,

    /// Warnings collected during config load that should be shown on startup.
    pub startup_warnings: Vec<String>,

    /// Optional override of model selection.
    pub model: Option<String>,

    /// Default Daybreak preference for new threads and non-interactive turns.
    pub daybreak_enabled: bool,

    /// Effective service tier request id preference for new turns.
    /// `default` means the user explicitly selected standard routing.
    pub service_tier: Option<String>,

    /// Model used specifically for review sessions.
    pub review_model: Option<String>,

    /// Size of the context window for the model, in tokens.
    pub model_context_window: Option<i64>,

    /// Token usage threshold triggering auto-compaction of conversation history.
    pub model_auto_compact_token_limit: Option<i64>,

    /// Controls whether `model_auto_compact_token_limit` applies to the full
    /// active context or only tokens after the carried compaction-window prefix.
    pub model_auto_compact_token_limit_scope: AutoCompactTokenLimitScope,

    /// Percentage of the usable context window that triggers turn-end compaction.
    /// Zero disables turn-end compaction.
    pub model_post_turn_compact_threshold_percent: u8,

    /// Percentage of the model's full context window at which background compaction starts.
    pub async_compaction_threshold_percent: u8,

    /// Key into the model_providers map that specifies which provider to use.
    pub model_provider_id: String,

    /// Info needed to make an API request to the model.
    pub model_provider: ModelProviderInfo,

    /// Deprecated: `friendly` and `pragmatic` no longer select a style.
    pub personality: Option<Personality>,

    /// Effective permission configuration for shell tool execution.
    pub permissions: Permissions,

    /// Whether config explicitly selected named permissions profiles instead
    /// of the legacy `sandbox_mode` syntax.
    pub explicit_permission_profile_mode: bool,

    /// User-defined permission profiles available from effective config.
    pub custom_permission_profiles: Vec<PermissionProfileCatalogEntry>,

    /// Configures who approval requests are routed to for review once they have
    /// been escalated. This does not disable separate safety checks such as
    /// ARC.
    pub approvals_reviewer: ApprovalsReviewer,
    /// Requested reviewer when managed requirements replace the effective choice.
    /// `None` uses `approvals_reviewer`, including programmatically constructed configs.
    pub requested_approvals_reviewer: Option<ApprovalsReviewer>,

    /// enforce_residency means web traffic cannot be routed outside of a
    /// particular geography. HTTP clients should direct their requests
    /// using backend-specific headers or URLs to enforce this.
    pub enforce_residency: Constrained<Option<ResidencyRequirement>>,

    /// When `true`, `AgentReasoning` events emitted by the backend will be
    /// suppressed from the frontend output. This can reduce visual noise when
    /// users are only interested in the final agent responses.
    pub hide_agent_reasoning: bool,

    /// When set to `true`, `AgentReasoningRawContentEvent` events will be shown in the UI/output.
    /// Defaults to `false`.
    pub show_raw_agent_reasoning: bool,

    /// Base instructions override.
    pub base_instructions: Option<String>,

    /// Origin of the configured base instructions when supplied by another session or lockfile.
    pub base_instructions_provenance: Option<BaseInstructionsProvenance>,

    /// Developer instructions override injected as a separate message.
    pub developer_instructions: Option<String>,

    /// Guardian-specific policy config override from requirements.toml or config.toml.
    /// This is inserted into the fixed guardian prompt template under the
    /// `# Policy Configuration` section rather than replacing the whole
    /// guardian developer prompt.
    pub guardian_policy_config: Option<String>,
    /// Guardian publication observed before startup loading or last policy refresh.
    pub guardian_policy_revision: u64,

    /// Additional Guardian policy from requirements.toml or config.toml.
    /// Rendered into `{{ extra_policy }}` alongside the resolved tenant policy.
    pub guardian_extra_policy: Option<String>,

    /// Guardian prompt template override from config.toml.
    /// The resolved policy config replaces its `{{ tenant_policy_config }}`
    /// placeholder when a review session is built.
    pub guardian_policy_template: Option<String>,
    /// Transcript encoding shared by Guardian review and scoring.
    pub guardian_transcript_mode: codex_protocol::TranscriptFormat,
    /// Frozen section policy for sync Guardian; absence keeps fixed budgeting.
    pub guardian_sync_elastic_budget: Option<codex_guardian_context::ElasticBudgetConfig>,

    /// Optional replacement for the gated history-retrieval instructions.
    /// Blank config values are treated as unset, like other Guardian policy overrides.
    pub guardian_conversation_history_prompt: Option<String>,

    /// Optional per-response history-tool budget. Guardian defaults to 4,000 tokens and
    /// preserves stricter parent tool limits.
    pub guardian_conversation_history_max_output_tokens: Option<NonZeroUsize>,

    /// Include a structured error when Guardian's circuit breaker interrupts a turn.
    pub guardian_circuit_break_action: CircuitBreakAction,

    /// Overall Guardian review budget. None means unlimited; cancellation still applies.
    pub guardian_review_timeout: Option<Duration>,

    /// Whether to inject the `<permissions instructions>` developer block.
    pub include_permissions_instructions: bool,

    /// Whether to inject the `<apps_instructions>` developer block.
    pub include_apps_instructions: bool,

    /// Whether to inject the `<collaboration_mode>` developer block.
    pub include_collaboration_mode_instructions: bool,

    /// Whether to inject the `<skills_instructions>` developer block.
    pub include_skill_instructions: bool,

    /// Optional token budget override for the available-skills catalog.
    pub skill_max_context_tokens: Option<NonZeroUsize>,

    /// Whether cloud skills are discovered and exposed to the model.
    pub cloud_skill_enabled: bool,

    /// Whether orchestrator-owned MCP tools are exposed to the model.
    pub orchestrator_mcp_enabled: bool,

    /// Whether to inject the `<environment_context>` user block.
    pub include_environment_context: bool,

    /// Whether environment context includes the current date and timezone.
    pub include_environment_context_time: bool,

    /// Compact prompt override.
    pub compact_prompt: Option<String>,

    /// Optional external notifier command. When set, Codex will spawn this
    /// program after each completed *turn* (i.e. when the agent finishes
    /// processing a user submission). The value must be the full command
    /// broken into argv tokens **without** the trailing JSON argument - Codex
    /// appends one extra argument containing a JSON payload describing the
    /// event.
    ///
    /// Example `~/.codex/config.toml` snippet:
    ///
    /// ```toml
    /// notify = ["notify-send", "Codex"]
    /// ```
    ///
    /// which will be invoked as:
    ///
    /// ```shell
    /// notify-send Codex '{"type":"agent-turn-complete","turn-id":"12345"}'
    /// ```
    ///
    /// If unset the feature is disabled.
    pub notify: Option<Vec<String>>,

    /// TUI notification settings, including enabled events, delivery method, and focus condition.
    pub tui_notifications: TuiNotificationSettings,

    /// Enable ASCII animations and shimmer effects in the TUI.
    pub animations: bool,

    /// Individual TUI effects, subordinate to the animation master switch.
    pub tui_effects: codex_config::types::TuiEffects,

    /// Rich content rendering preferences, independent of animations.
    pub tui_rendering: codex_config::types::TuiRendering,

    /// Show startup tooltips in the TUI welcome screen.
    pub show_tooltips: bool,

    /// Show a TUI notice when the connected app server is an older stable release.
    pub tui_show_server_version_notice: bool,

    /// Generate automatic TUI recaps. Manual `/recap` remains available when disabled.
    pub tui_auto_recap: bool,

    /// Persisted startup availability NUX state for model tooltips.
    pub model_availability_nux: ModelAvailabilityNuxConfig,

    /// Start the composer in Vim mode (`Normal`) by default.
    pub tui_vim_mode_default: bool,
    pub tui_question_esc_back: bool,

    /// Start the TUI in raw scrollback mode for copy-friendly transcript output.
    pub tui_raw_output_mode: bool,

    /// Own the fullscreen transcript when the alternate screen is enabled.
    pub tui_fullscreen_transcript: bool,

    /// Preferred docked sidebar width as a percentage of the terminal.
    pub tui_sidebar_width_percent: Option<u8>,

    /// Mouse wheel speed multiplier for transcript scrolling; defaults to one row per event.
    pub tui_mouse_scroll_speed: Option<f64>,

    /// Override the terminal default for copying transcript, footer, and composer selections.
    pub tui_copy_on_select: codex_config::types::CopyOnSelect,

    /// Whether to deselect read-only transcript or footer text after confirmed automatic copy.
    pub tui_copy_on_select_clear_selection: bool,

    /// Right-click text paste fallback for the fullscreen TUI.
    pub tui_right_click_paste: codex_config::types::RightClickPaste,

    /// Start the TUI in the specified collaboration mode (plan/default).

    /// Controls whether the TUI uses the terminal's alternate screen buffer.
    ///
    /// This is the same `tui.alternate_screen` value from `config.toml`.
    /// - `auto` (default): Use alternate screen.
    /// - `always`: Always use alternate screen.
    /// - `never`: Never use alternate screen (inline mode, preserves scrollback).
    pub tui_alternate_screen: AltScreenMode,
    /// Ordered list of status line item identifiers for the TUI.
    ///
    /// When unset, the TUI defaults to: `model-with-reasoning` and `current-dir`.
    pub tui_status_line: Option<Vec<String>>,

    /// Whether to color status line items with colors from the active syntax theme.
    pub tui_status_line_use_colors: bool,

    /// Ordered list of terminal title item identifiers for the TUI.
    ///
    /// When unset, the TUI defaults to: `activity` and `project`.
    /// The `activity` item spins while working and shows an action-required
    /// message when blocked on the user.
    pub tui_terminal_title: Option<Vec<String>>,

    /// Syntax highlighting theme override (kebab-case name).
    pub tui_theme: Option<String>,

    /// Pet id preselected by the terminal pet picker.
    pub tui_pet: Option<String>,

    /// Vertical anchor used by terminal pet rendering.
    pub tui_pet_anchor: TuiPetAnchor,

    /// Preferred layout for resume/fork session picker results.
    pub tui_session_picker_view: SessionPickerViewMode,

    /// Last selected grouping in Agent Command Center.
    pub tui_agents_overview_grouping: codex_config::types::AgentsOverviewGrouping,

    /// Working directory to use when resuming or forking a session.
    /// When unset, prompt if the current and session directories differ.
    pub tui_resume_cwd: Option<ResumeCwdMode>,

    /// Terminal resize-reflow tuning knobs.
    pub terminal_resize_reflow: TerminalResizeReflowConfig,

    /// Keybinding overrides for the TUI.
    ///
    /// Precedence is:
    ///
    /// 1. context table (`tui.keymap.chat`, `tui.keymap.composer`, etc.)
    /// 2. `tui.keymap.global`
    /// 3. built-in defaults
    pub tui_keymap: TuiKeymap,

    /// Saved TUI inputs keyed by a single digit (`0` through `9`).
    pub tui_quick_dials: BTreeMap<String, QuickDial>,

    /// The absolute directory that should be treated as the current working
    /// directory for the session. All relative paths inside the business-logic
    /// layer are resolved against this path.
    pub cwd: AbsolutePathBuf,

    /// Absolute runtime workspace roots for the session. Symbolic
    /// `:workspace_roots` permission entries are materialized against these
    /// roots while profile-defined workspace roots remain encoded directly in
    /// the permission profile.
    pub workspace_roots: Vec<AbsolutePathBuf>,
    /// Whether runtime workspace roots were supplied explicitly by the caller
    /// or legacy config, rather than defaulting to `cwd`.
    pub workspace_roots_explicit: bool,

    /// Preferred store for CLI auth credentials.
    /// file (default): Use a file in the Codex home directory.
    /// keyring: Use an OS-specific keyring service.
    /// auto: Use the OS-specific keyring service if available, otherwise use a file.
    pub cli_auth_credentials_store_mode: AuthCredentialsStoreMode,

    /// Definition for MCP servers that Codex can reach out to for tool calls.
    pub mcp_servers: Constrained<HashMap<String, McpServerConfig>>,

    /// Trusted IdP shared by all permitted EMA MCP registrations.
    pub mcp_enterprise_managed_auth: Option<McpEnterpriseManagedAuthConfig>,

    /// When present, only these MCP servers omit the legacy `mcp__` namespace prefix.
    pub non_prefixed_mcp_tool_servers: Option<Vec<String>>,

    /// Preferred store for MCP OAuth credentials.
    /// keyring: Use an OS-specific keyring service.
    ///          Credentials stored in the keyring will only be readable by Codex unless the user explicitly grants access via OS-level keyring access.
    ///          https://github.com/openai/codex/blob/main/codex-rs/rmcp-client/src/oauth.rs#L2
    /// file: CODEX_HOME/.credentials.json
    ///       This file will be readable to Codex and other applications running as the same user.
    /// auto (default): keyring if available, otherwise file.
    pub mcp_oauth_credentials_store_mode: OAuthCredentialsStoreMode,

    /// Optional fixed port to use for the local HTTP callback server used during MCP OAuth login.
    ///
    /// When unset, Codex will bind to an ephemeral port chosen by the OS.
    pub mcp_oauth_callback_port: Option<u16>,

    /// Optional redirect URI to use during MCP OAuth login.
    ///
    /// When set, this URI is used in the OAuth authorization request instead
    /// of the local listener address. The local callback listener still binds
    /// to 127.0.0.1 (using `mcp_oauth_callback_port` when provided).
    pub mcp_oauth_callback_url: Option<String>,

    /// How long to wait for optional MCP servers while building the initial tool catalog.
    pub mcp_optional_startup_grace: Duration,

    /// Combined provider map (defaults plus user-defined providers).
    pub model_providers: HashMap<String, ModelProviderInfo>,

    /// Maximum total bytes of project instruction content across all selected environments.
    pub project_doc_max_bytes: usize,

    /// Additional filenames to try when looking for project-level docs.
    pub project_doc_fallback_filenames: Vec<String>,

    /// Token budget applied when storing tool/function outputs in the context manager.
    pub tool_output_token_limit: Option<usize>,

    /// Whether multi-agent tools are enabled through `[agents]`.
    pub agents_enabled: bool,

    /// User-configured maximum number of spawned agent threads per session.
    pub agent_max_threads: Option<usize>,

    /// Default model for spawned subagents when the spawn call does not select one.
    pub agent_default_subagent_model: Option<String>,

    /// Default reasoning effort for spawned subagents when the spawn call does not select one.
    pub agent_default_subagent_reasoning_effort: Option<ReasoningEffort>,

    /// Whether to record a model-visible message when an agent turn is interrupted.
    pub agent_interrupt_message_enabled: bool,

    /// Maximum nesting depth for V1 agent threads. Ignored by V2.
    pub agent_max_depth: i32,

    /// User-defined role declarations keyed by role name.
    pub agent_roles: BTreeMap<String, AgentRoleConfig>,

    /// Maximum token budget allowed for a goal and default budget for new goals.
    pub max_goal_token_budget: Option<i64>,

    /// Memories subsystem settings.
    pub memories: MemoriesConfig,

    /// Directory containing all Codex state (defaults to `~/.codex` but can be
    /// overridden by the `CODEX_HOME` environment variable).
    pub codex_home: AbsolutePathBuf,

    /// Resolved configuration shared by all Codex SQLite databases.
    pub sqlite: codex_state::SqliteConfig,

    /// Directory for log files (defaults to `$CODEX_HOME/log`).
    /// SQLite logs use this directory only when `log_dir` is explicitly configured
    /// and no managed `sqlite_home` requirement applies.
    pub log_dir: PathBuf,

    /// Settings that govern if and what will be written to `~/.codex/history.jsonl`.
    pub history: History,

    /// When true, session is not persisted on disk. Default to `false`
    pub ephemeral: bool,

    /// Enable persistent dynamic tool updates only when every reader, including rollback
    /// versions, can restore the updated tools and preserve them through compaction.
    pub allow_persistent_dynamic_tool_updates: bool,

    /// Optional extra configuration fields for the thread.
    pub extra_config: Option<ExtraConfig>,

    /// Whether enabled hooks should run without requiring persisted hook trust for this session.
    ///
    /// This is a runtime-only knob populated from invocation overrides, not from config files.
    pub bypass_hook_trust: bool,

    /// Optional URI-based file opener. If set, citations to files in the model
    /// output will be hyperlinked using the specified URI scheme.
    pub file_opener: UriBasedFileOpener,

    /// Path to the current Codex executable. This cannot be set in the config
    /// file: it must be set in code via [`ConfigOverrides`].
    pub codex_self_exe: Option<PathBuf>,

    /// Path to the `codex-linux-sandbox` executable. This must be set if
    /// [`codex_sandboxing::SandboxType::LinuxSeccomp`] is used. Note that this
    /// cannot be set in the config file: it must be set in code via
    /// [`ConfigOverrides`].
    ///
    /// When this program is invoked, arg0 will be set to `codex-linux-sandbox`.
    pub codex_linux_sandbox_exe: Option<PathBuf>,

    /// Ignored compatibility field for clients that still pass the retired wrapper path.
    pub main_execve_wrapper_exe: Option<PathBuf>,

    /// Ignored compatibility field for clients that still pass a patched zsh path.
    pub zsh_path: Option<PathBuf>,

    /// Value to use for `reasoning.effort` when making a request using the
    /// Responses API.
    pub model_reasoning_effort: Option<ReasoningEffort>,
    /// Optional Plan-mode-specific reasoning effort override used by the TUI.
    ///
    /// When unset, Plan mode uses the built-in Plan preset default (currently
    /// `medium`). When explicitly set (including `none`), this overrides the
    /// Plan preset. The `none` value means "no reasoning" (not "inherit the
    /// global default").
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,

    /// Optional value to use for `reasoning.summary` when making a request
    /// using the Responses API. When unset, the model catalog default is used.
    pub model_reasoning_summary: Option<ReasoningSummary>,

    /// Optional full model catalog loaded from `model_catalog_json`.
    /// When set, this replaces the bundled catalog for the current process.
    pub model_catalog: Option<ModelsResponse>,

    /// Optional verbosity control for GPT-5 models (Responses API `text.verbosity`).
    pub model_verbosity: Option<Verbosity>,

    /// Base URL for requests to ChatGPT (as opposed to the OpenAI API).
    pub chatgpt_base_url: String,

    /// Whether configuration or managed requirements explicitly selected the ChatGPT base URL.
    pub chatgpt_base_url_is_explicit: bool,

    /// Whether Codex-owned clients should respect host system proxy settings.
    pub respect_system_proxy: bool,

    /// Optional product SKU forwarded to the host-owned apps MCP server.
    pub apps_mcp_product_sku: Option<String>,

    /// Bounded, product-owned metadata attached to every Responses API request.
    pub responses_api_metadata: BTreeMap<String, String>,

    /// Machine-local realtime audio device preferences used by realtime voice.
    pub realtime_audio: RealtimeAudioConfig,

    /// Experimental / do not use. Overrides only the realtime conversation
    /// websocket transport base URL (the `Op::RealtimeConversation`
    /// `/v1/realtime`
    /// connection) without changing normal provider HTTP requests.
    pub experimental_realtime_ws_base_url: Option<String>,
    /// Experimental / do not use. Overrides only the WebRTC realtime call
    /// creation base URL.
    pub experimental_realtime_webrtc_call_base_url: Option<String>,
    /// Experimental / do not use. Selects the realtime websocket model/snapshot
    /// used for the `Op::RealtimeConversation` connection.
    pub experimental_realtime_ws_model: Option<String>,
    /// Experimental / do not use. Realtime websocket session selection.
    /// `version` controls v1/v2 and `type` controls conversational/transcription.
    pub realtime: RealtimeConfig,
    /// Experimental / do not use. Overrides only the realtime conversation
    /// websocket transport instructions (the `Op::RealtimeConversation`
    /// `/ws` session.update instructions) without changing normal prompts.
    pub experimental_realtime_ws_backend_prompt: Option<String>,
    /// Experimental / do not use. Replaces the synthesized realtime startup
    /// context appended to websocket session instructions. An empty string
    /// disables startup context injection entirely.
    pub experimental_realtime_ws_startup_context: Option<String>,
    /// Experimental / do not use. Replaces the built-in realtime start
    /// instructions inserted into developer messages when realtime becomes
    /// active.
    pub experimental_realtime_start_instructions: Option<String>,
    /// Experimental / do not use. Selects the thread persistence backend.
    pub experimental_thread_store: ThreadStoreConfig,
    /// When set, restricts ChatGPT login to one or more workspace identifiers.
    pub forced_chatgpt_workspace_id: Option<Vec<String>>,

    /// When set, restricts the login mechanism users may use.
    pub forced_login_method: Option<ForcedLoginMethod>,

    /// Explicit or feature-derived web search mode.
    pub web_search_mode: Constrained<WebSearchMode>,

    /// Additional parameters for the web search tool when it is enabled.
    pub web_search_config: Option<WebSearchConfig>,

    /// Whether to register the experimental request_user_input tool.
    pub experimental_request_user_input_enabled: bool,

    /// Whether to register the update_plan tool.
    pub update_plan_enabled: bool,

    /// Policy for collecting and validating tool runtimes.
    pub tool_registry: ToolRegistryConfig,

    /// Configuration for the experimental code-mode tool surface.
    pub code_mode: CodeModeConfig,

    /// Maximum poll window for background terminal output (`write_stdin`), in milliseconds.
    /// Default: `300000` (5 minutes).
    pub background_terminal_max_timeout: u64,

    /// Idle timeout for unsubscribed app-server threads.
    pub thread_unload_delay: Duration,

    /// Compatibility-only settings retained for legacy `ghost_snapshot`
    /// config loading.
    pub ghost_snapshot: GhostSnapshotConfig,

    /// Settings specific to the task-path-based multi-agent tool surface.
    pub multi_agent_v2: MultiAgentV2Config,

    /// Context-window token budget configuration, when enabled.
    pub token_budget: Option<TokenBudgetConfig>,
    /// Runtime snapshot of configured token-budget preferences before startup activation.
    pub token_budget_startup_config: Option<TokenBudgetStartupConfig>,
    /// Shared token budget for the root thread and its sub-agents.
    pub rollout_budget: Option<RolloutBudgetConfig>,
    /// Current-time reminder and clock tool configuration, when enabled.
    pub current_time_reminder: Option<CurrentTimeReminderConfig>,
    /// How the sleep tool is selected when its feature gate is enabled.
    pub sleep_tool_mode: SleepToolMode,
    /// Authentication mode used when runtime agent instances are enabled.
    pub runtime_auth_mode: RuntimeAuthMode,

    /// Centralized feature flags; source of truth for feature gating.
    pub features: ManagedFeatures,

    /// Local rollout preference after checking network restrictions and native availability.
    pub prefer_mxc: bool,

    /// Host feature defaults retained beneath explicit configuration overrides.
    pub runtime_feature_defaults: BTreeMap<Feature, bool>,

    /// When `true`, suppress warnings about unstable (under development) features.
    pub suppress_unstable_features_warning: bool,

    /// The currently active project config, resolved by checking if cwd:
    /// is (1) part of a git repo, (2) a git worktree, or (3) just using the cwd
    pub active_project: ProjectConfig,

    /// Collection of various notices we show the user
    pub notices: Notice,

    /// When `true`, checks for Codex updates on startup and surfaces update prompts.
    /// Set to `false` only if your Codex updates are centrally managed.
    /// Defaults to `true`.
    pub check_for_update_on_startup: bool,

    /// When true, disables burst-paste detection for typed input entirely.
    /// All characters are inserted as they are received, and no buffering
    /// or placeholder replacement will occur for fast keypress bursts.
    pub disable_paste_burst: bool,

    /// When `false`, disables OpenAI analytics across Codex product surfaces on this machine.
    /// Custom OTLP metrics exporters are controlled by `otel.metrics_exporter`.
    /// Voluntarily left as Optional because the default value might depend on the client.
    pub analytics_enabled: Option<bool>,

    /// When `false`, disables feedback uploads and background Guardian review retention. Explicit local exports remain available.
    /// Defaults to `true`.
    pub feedback_enabled: bool,

    /// Configured discoverable tools for tool suggestions.
    pub tool_suggest: ToolSuggestConfig,

    /// OTEL configuration (exporter type, endpoint, headers, etc.).
    pub otel: codex_config::types::OtelConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ToolRegistryConfig {
    /// Fail the turn when multiple tools share the same effective name.
    pub error_on_tool_collisions: bool,
    /// Include authoritative tool information in per-turn request metadata.
    pub turn_metadata_includes_tool_info: bool,
}

const DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeModeConfig {
    pub default_exec_yield_time_ms: u64,
    /// Show handler duration, code-mode host duration, and harness overhead
    /// in each code-mode cell response.
    /// Experimental: this option and the response format may change or be removed.
    pub experimental_show_cell_overhead: bool,
    pub tool_input_schema_max_bytes: Option<usize>,
    pub excluded_tool_namespaces: Vec<String>,
    pub direct_only_tool_namespaces: Vec<String>,
    /// Keep code mode fail-closed when the standalone host is unavailable.
    pub disable_in_process_fallback: bool,
}

impl Default for CodeModeConfig {
    fn default() -> Self {
        Self {
            default_exec_yield_time_ms: DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS,
            experimental_show_cell_overhead: false,
            tool_input_schema_max_bytes: None,
            excluded_tool_namespaces: Vec::new(),
            direct_only_tool_namespaces: Vec::new(),
            disable_in_process_fallback: false,
        }
    }
}

const TOKEN_BUDGET_REMINDER_MESSAGE_TEMPLATE_MAX_BYTES: usize = 2000;
const TOKEN_BUDGET_GUIDANCE_MESSAGE_MAX_BYTES: usize = 2000;
const AUTO_COMPACT_FALLBACK_PROMPT_MAX_BYTES: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TokenBudgetConfig {
    pub use_history_notes_extension: bool,
    pub reminder_threshold_tokens: Option<i64>,
    pub reminder_message_template: String,
    pub guidance_message: Option<String>,
    pub auto_compact_fallback_prompt: Option<String>,
    pub auto_compact_fallback_buffer_tokens: Option<i64>,
}

impl Default for TokenBudgetConfig {
    fn default() -> Self {
        Self {
            use_history_notes_extension: false,
            reminder_threshold_tokens: None,
            reminder_message_template: ResolvedModelMessages::bundled()
                .token_budget_reminder_template()
                .to_owned(),
            guidance_message: None,
            auto_compact_fallback_prompt: None,
            auto_compact_fallback_buffer_tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RolloutBudgetConfig {
    pub enforce_limit: bool,
    pub limit_tokens: i64,
    pub reminder_at_remaining_tokens: Vec<i64>,
    pub sampling_token_weight: f64,
    pub prefill_token_weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CurrentTimeReminderConfig {
    pub reminder_interval_seconds: u64,
    pub clock_source: CurrentTimeSource,
    pub delivery_mode: CurrentTimeReminderDeliveryMode,
    /// Whether to expose the input-interruptible `clock.sleep` tool.
    pub sleep_tool: bool,
}

impl Default for CurrentTimeReminderConfig {
    fn default() -> Self {
        Self {
            reminder_interval_seconds: 1,
            clock_source: CurrentTimeSource::System,
            delivery_mode: CurrentTimeReminderDeliveryMode::AnyInference,
            sleep_tool: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MultiAgentV2Config {
    pub max_concurrent_threads_per_session: usize,
    pub min_wait_timeout_ms: i64,
    pub max_wait_timeout_ms: i64,
    pub default_wait_timeout_ms: i64,
    pub usage_hint_text: Option<String>,
    pub root_agent_usage_hint_text: Option<String>,
    pub subagent_usage_hint_text: Option<String>,
    pub subagent_developer_instructions: Option<String>,
    pub preserve_fork_prefix: bool,
    pub multi_agent_mode_hint_text: Option<String>,
    pub tool_namespace: Option<String>,
    pub hide_spawn_agent_metadata: bool,
    pub expose_spawn_agent_model_overrides: bool,
    pub wait_agent_enabled: bool,
    pub disable_native_tools: bool,
    pub disable_direct_message: bool,
    pub message_board_in_memory: bool,
    pub message_board_remote: Option<codex_features::RemoteMessageBoardConfigToml>,
    pub non_code_mode_only: bool,
}

impl Default for MultiAgentV2Config {
    fn default() -> Self {
        Self::defaults_for_max_concurrency(
            DEFAULT_MULTI_AGENT_V2_MAX_CONCURRENT_THREADS_PER_SESSION,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalResizeReflowMaxRows {
    /// Use the runtime terminal detector to choose a scrollback-sized cap.
    #[default]
    Auto,
    /// Keep all rendered transcript rows during resize reflow.
    Disabled,
    /// Keep at most this many rendered transcript rows during resize reflow.
    Limit(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalResizeReflowConfig {
    pub max_rows: TerminalResizeReflowMaxRows,
}

#[derive(Clone, Default)]
pub struct ConfigBuilder {
    codex_home: Option<PathBuf>,
    cli_overrides: Option<Vec<(String, TomlValue)>>,
    harness_overrides: Option<ConfigOverrides>,
    loader_overrides: Option<LoaderOverrides>,
    strict_config: bool,
    cloud_config_bundle: CloudConfigBundleLoader,
    thread_config_loader: Option<Arc<dyn ThreadConfigLoader>>,
    fallback_cwd: Option<PathBuf>,
    without_project_context: bool,
}

/// Partially loaded config plus the layer stack used to derive it.
///
/// This is intended for startup paths that must inspect raw config before a
/// full [`Config`] can be constructed, but still need access to managed
/// requirements loaded with the config layers.
pub struct ConfigTomlLoadResult {
    pub config_toml: ConfigToml,
    pub config_layer_stack: ConfigLayerStack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermissionConfigSyntax {
    Legacy,
    Profiles,
}

#[derive(Debug, Deserialize, Default)]
struct PermissionSelectionToml {
    default_permissions: Option<String>,
    sandbox_mode: Option<SandboxMode>,
}

// Resolve the named-profile catalog and selected profile id together. Runtime
// profile constraints are applied later after this selection compiles into a
// concrete `PermissionProfile`.
#[derive(Debug)]
struct EffectivePermissionSelection<'a> {
    profiles: Option<PermissionsToml>,
    selected_profile_id: Option<&'a str>,
    persisted_profile_id_was_provided: bool,
    requirements_force_profile_selection: bool,
}

/// Optional overrides for user configuration (e.g., from CLI flags).
#[derive(Default, Debug, Clone)]
pub struct ConfigOverrides {
    pub model: Option<String>,
    pub review_model: Option<String>,
    pub cwd: Option<PathBuf>,
    pub approval_policy: Option<AskForApproval>,
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub sandbox_mode: Option<SandboxMode>,
    pub permission_profile: Option<PermissionProfile>,
    pub default_permissions: Option<String>,
    /// Permission profile ID recovered from persisted thread state. Explicit
    /// permission overrides take precedence, and stale profile IDs fall back
    /// to the configured default.
    pub persisted_permission_profile_id: Option<String>,
    /// Unnamed permissions from a thread-owned rollout snapshot. These are
    /// fallback state: explicit overrides and current requirements take priority.
    pub persisted_permission_profile: Option<PermissionProfile>,
    pub model_provider: Option<String>,
    pub service_tier: Option<Option<String>>,
    pub codex_self_exe: Option<PathBuf>,
    pub codex_linux_sandbox_exe: Option<PathBuf>,
    /// Ignored compatibility field for clients that still pass the retired wrapper path.
    pub main_execve_wrapper_exe: Option<PathBuf>,
    pub base_instructions: Option<String>,
    pub developer_instructions: Option<String>,
    /// Deprecated: `friendly` and `pragmatic` no longer select a style.
    pub personality: Option<Personality>,
    pub compact_prompt: Option<String>,
    pub show_raw_agent_reasoning: Option<bool>,
    pub tools_web_search_request: Option<bool>,
    pub ephemeral: Option<bool>,
    pub bypass_hook_trust: Option<bool>,
    /// Additional directories that should be treated as writable roots for this session.
    pub additional_writable_roots: Vec<PathBuf>,
    /// Explicit absolute runtime workspace roots for this session. When set,
    /// this is the full runtime root list rather than an additive override.
    pub workspace_roots: Option<Vec<AbsolutePathBuf>>,
}

/// Resolve the web search mode from explicit config and feature flags.
fn resolve_web_search_mode(config_toml: &ConfigToml, features: &Features) -> Option<WebSearchMode> {
    if let Some(mode) = config_toml.web_search {
        return Some(mode);
    }
    if features.enabled(Feature::WebSearchCached) {
        return Some(WebSearchMode::Cached);
    }
    if features.enabled(Feature::WebSearchRequest) {
        return Some(WebSearchMode::Live);
    }
    None
}

fn resolve_feature_enabled(feature: Option<&codex_config::config_toml::FeatureToggleToml>) -> bool {
    feature.and_then(|feature| feature.enabled).unwrap_or(true)
}

fn resolve_terminal_resize_reflow_config(config_toml: &ConfigToml) -> TerminalResizeReflowConfig {
    let Some(tui) = config_toml.tui.as_ref() else {
        return TerminalResizeReflowConfig::default();
    };

    TerminalResizeReflowConfig {
        max_rows: match tui.terminal_resize_reflow_max_rows {
            Some(0) => TerminalResizeReflowMaxRows::Disabled,
            Some(rows) => TerminalResizeReflowMaxRows::Limit(rows),
            None => TerminalResizeReflowMaxRows::Auto,
        },
    }
}

fn network_proxy_toml_config(features: Option<&FeaturesToml>) -> Option<&NetworkProxyConfigToml> {
    match features?.network_proxy.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn validate_multi_agent_v2_tool_namespace(namespace: Option<&str>) -> std::io::Result<()> {
    const LABEL: &str = "features.multi_agent_v2.tool_namespace";
    const MAX_LEN: usize = 64;
    const RESERVED_RESPONSES_NAMESPACES: &[&str] = &[
        "api_tool",
        "browser",
        "computer",
        "container",
        "file_search",
        "functions",
        "image_gen",
        "multi_tool_use",
        "python",
        "python_user_visible",
        "submodel_delegator",
        "terminal",
        "tool_search",
        "web",
    ];

    let Some(namespace) = namespace else {
        return Ok(());
    };
    if namespace.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must not be empty"),
        ));
    }
    if namespace.trim() != namespace {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must not have leading or trailing whitespace"),
        ));
    }
    if !namespace
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must match ^[a-zA-Z0-9_-]+$"),
        ));
    }
    if namespace.chars().count() > MAX_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} must be at most {MAX_LEN} characters"),
        ));
    }
    if namespace == "mcp"
        || namespace.starts_with("mcp__")
        || RESERVED_RESPONSES_NAMESPACES.contains(&namespace)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{LABEL} uses a reserved namespace: {namespace}"),
        ));
    }

    Ok(())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "config_loader_tests.rs"]
mod config_loader_tests;

#[cfg(test)]
#[path = "runtime_auth_tests.rs"]
mod runtime_auth_tests;
