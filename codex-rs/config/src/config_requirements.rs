use crate::ApplicationRequirementsToml;
use codex_features::FeatureToml;
use codex_features::RuntimeAgentInstancesConfigToml;
use codex_features::RuntimeAuthMode;
use codex_model_provider_info::ModelProviderInfo;
pub use codex_model_provider_info::ResidencyRequirement;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::ToolResultLogConfig;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde::de::Error as _;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use wildmatch::WildMatchPattern;

use super::requirements_exec_policy::RequirementsExecPolicyToml;
use crate::Constrained;
use crate::ConstraintError;
use crate::InAppBrowserRequirementsToml;
use crate::ManagedAuthPolicy;
use crate::ManagedHooksRequirementsToml;
use crate::McpServerRequirement;
use crate::PluginRequirementsToml;
use crate::RequirementsExecPolicy;
use crate::browser_computer_use_requirements::BrowserUseRequirementsToml;
use crate::browser_computer_use_requirements::ComputerUseRequirementsToml;
use crate::config_toml::ConfigToml;
use crate::mcp_requirements::validate_mcp_server_requirement;
use crate::mcp_types::AppToolApproval;
use crate::permissions_toml::PermissionProfileToml;
use crate::types::AuthCredentialsStoreMode;
use crate::types::FeedbackConfigToml;
use crate::types::OtelConfigToml;
use crate::types::OtelExporterKind;
use crate::types::WindowsSandboxModeToml;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementSource {
    Unknown,
    MdmManagedPreferences {
        domain: String,
        key: String,
    },
    /// Multiple requirements layers contributed to the final value. Sources are
    /// stored highest-priority first, matching the order surfaced in errors.
    Composite {
        sources: Vec<RequirementSource>,
    },
    /// A backend-delivered enterprise-managed layer. `id` is the stable backend
    /// identifier; `name` is the admin-facing display name.
    EnterpriseManaged {
        id: String,
        name: String,
    },
    SystemRequirementsToml {
        file: AbsolutePathBuf,
    },
    LegacyManagedConfigTomlFromFile {
        file: AbsolutePathBuf,
    },
    LegacyManagedConfigTomlFromMdm,
}

impl RequirementSource {
    pub fn composite(sources: impl IntoIterator<Item = RequirementSource>) -> Self {
        let mut flattened = Vec::new();
        for source in sources {
            source.append_to_composite(&mut flattened);
        }

        match flattened.len() {
            0 => RequirementSource::Unknown,
            1 => flattened.remove(0),
            _ => RequirementSource::Composite { sources: flattened },
        }
    }

    fn append_to_composite(self, flattened: &mut Vec<RequirementSource>) {
        match self {
            RequirementSource::Composite { sources } => {
                for source in sources {
                    source.append_to_composite(flattened);
                }
            }
            source => {
                if !flattened.contains(&source) {
                    flattened.push(source);
                }
            }
        }
    }
}

impl fmt::Display for RequirementSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequirementSource::Unknown => write!(f, "<unspecified>"),
            RequirementSource::MdmManagedPreferences { domain, key } => {
                write!(f, "MDM {domain}:{key}")
            }
            RequirementSource::Composite { sources } => {
                write!(f, "requirements layers: ")?;
                for (index, source) in sources.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{source}")?;
                }
                Ok(())
            }
            RequirementSource::EnterpriseManaged { id, name } => {
                write!(f, "enterprise-managed requirements {name} ({id})")
            }
            RequirementSource::SystemRequirementsToml { file } => {
                write!(f, "{}", file.as_path().display())
            }
            RequirementSource::LegacyManagedConfigTomlFromFile { file } => {
                write!(f, "{}", file.as_path().display())
            }
            RequirementSource::LegacyManagedConfigTomlFromMdm => {
                write!(f, "MDM managed_config.toml (legacy)")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstrainedWithSource<T> {
    pub value: Constrained<T>,
    pub source: Option<RequirementSource>,
}

impl<T> ConstrainedWithSource<T> {
    pub fn new(value: Constrained<T>, source: Option<RequirementSource>) -> Self {
        Self { value, source }
    }
}

impl<T> std::ops::Deref for ConstrainedWithSource<T> {
    type Target = Constrained<T>;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> std::ops::DerefMut for ConstrainedWithSource<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

/// Normalized version of [`ConfigRequirementsToml`] after deserialization and
/// normalization.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigRequirements {
    pub allowed_login_methods: Option<Sourced<Vec<ForcedLoginMethod>>>,
    pub allowed_chatgpt_workspaces: Option<Sourced<Vec<String>>>,
    pub cli_auth_credentials_store: Option<Sourced<AuthCredentialsStoreMode>>,
    pub chatgpt_base_url: Option<Sourced<String>>,
    pub sqlite_home: Option<Sourced<AbsolutePathBuf>>,
    pub unsafe_enforce_sqlite_journal_mode: Option<Sourced<bool>>,
    pub log_dir: Option<Sourced<AbsolutePathBuf>>,
    pub model_catalog_json: Option<Sourced<AbsolutePathBuf>>,
    pub model_provider: Option<Sourced<String>>,
    pub model_providers: Option<Sourced<HashMap<String, ModelProviderInfo>>>,
    pub check_for_update_on_startup: Option<Sourced<bool>>,
    pub allow_login_shell: Option<Sourced<bool>>,
    pub feedback: Option<Sourced<FeedbackConfigToml>>,
    pub approval_policy: ConstrainedWithSource<AskForApproval>,
    pub approvals_reviewer: ConstrainedWithSource<ApprovalsReviewer>,
    pub auto_review_required_models: Option<Sourced<BTreeSet<String>>>,
    pub permission_profile: ConstrainedWithSource<PermissionProfile>,
    pub windows_sandbox_mode: ConstrainedWithSource<Option<WindowsSandboxModeToml>>,
    pub web_search_mode: ConstrainedWithSource<WebSearchMode>,
    pub allow_managed_hooks_only: Option<Sourced<bool>>,
    pub allow_appshots: Option<Sourced<bool>>,
    pub allow_remote_control: Option<Sourced<bool>>,
    pub computer_use: Option<Sourced<ComputerUseRequirementsToml>>,
    pub feature_requirements: Option<Sourced<FeatureRequirementsToml>>,
    pub managed_hooks: Option<ConstrainedWithSource<ManagedHooksRequirementsToml>>,
    pub mcp_servers: Option<Sourced<BTreeMap<String, McpServerRequirement>>>,
    pub plugins: Option<Sourced<BTreeMap<String, PluginRequirementsToml>>>,
    pub marketplaces: Option<Sourced<MarketplaceRequirementsToml>>,
    // Keep OTEL boxed in both forms so unset values do not inflate ConfigLayerStack/Config stack frames.
    pub otel: Option<Box<Sourced<OtelRequirementsToml>>>,
    pub exec_policy: Option<Sourced<RequirementsExecPolicy>>,
    pub enforce_residency: ConstrainedWithSource<Option<ResidencyRequirement>>,
    /// Managed network constraints derived from requirements.
    pub network: Option<Sourced<NetworkConstraints>>,
    pub application: Option<Sourced<ApplicationRequirementsToml>>,
    /// Managed filesystem constraints derived from requirements.
    pub filesystem: Option<Sourced<FilesystemConstraints>>,
    /// Managed instructions included independently of ordinary developer instructions.
    pub additional_developer_instructions: Option<Sourced<String>>,
    /// Source for the managed guardian policy config, when one is configured.
    pub guardian_policy_config_source: Option<RequirementSource>,
    /// Source for the managed extra guardian policy, when one is configured.
    pub guardian_extra_policy_source: Option<RequirementSource>,
}

impl Default for ConfigRequirements {
    fn default() -> Self {
        Self {
            allowed_login_methods: None,
            allowed_chatgpt_workspaces: None,
            cli_auth_credentials_store: None,
            chatgpt_base_url: None,
            sqlite_home: None,
            unsafe_enforce_sqlite_journal_mode: None,
            log_dir: None,
            model_catalog_json: None,
            model_provider: None,
            model_providers: None,
            check_for_update_on_startup: None,
            allow_login_shell: None,
            feedback: None,
            approval_policy: ConstrainedWithSource::new(
                Constrained::allow_any_from_default(),
                /*source*/ None,
            ),
            approvals_reviewer: ConstrainedWithSource::new(
                Constrained::allow_any_from_default(),
                /*source*/ None,
            ),
            auto_review_required_models: None,
            permission_profile: ConstrainedWithSource::new(
                Constrained::allow_any(PermissionProfile::read_only()),
                /*source*/ None,
            ),
            windows_sandbox_mode: ConstrainedWithSource::new(
                Constrained::allow_any(/*initial_value*/ None),
                /*source*/ None,
            ),
            web_search_mode: ConstrainedWithSource::new(
                Constrained::allow_any(WebSearchMode::Cached),
                /*source*/ None,
            ),
            allow_managed_hooks_only: None,
            allow_appshots: None,
            allow_remote_control: None,
            computer_use: None,
            feature_requirements: None,
            managed_hooks: None,
            mcp_servers: None,
            plugins: None,
            marketplaces: None,
            otel: None,
            exec_policy: None,
            enforce_residency: ConstrainedWithSource::new(
                Constrained::allow_any(/*initial_value*/ None),
                /*source*/ None,
            ),
            network: None,
            application: None,
            filesystem: None,
            additional_developer_instructions: None,
            guardian_policy_config_source: None,
            guardian_extra_policy_source: None,
        }
    }
}

impl ConfigRequirements {
    /// Returns whether a model slug or its supported provider alias requires auto-review.
    pub fn auto_review_required_for_model(&self, model: &str) -> bool {
        let Some(protected_models) = self.auto_review_required_models.as_ref() else {
            return false;
        };

        let model = match model.split_once('/') {
            Some((namespace, suffix))
                if !namespace.is_empty()
                    && !suffix.contains('/')
                    && namespace.chars().all(|character| {
                        character.is_ascii_alphanumeric() || character == '_' || character == '-'
                    }) =>
            {
                suffix
            }
            Some(_) => return false,
            None => model,
        };

        protected_models.value.contains(model)
    }

    pub fn managed_auth_policy(&self) -> ManagedAuthPolicy {
        ManagedAuthPolicy {
            allowed_login_methods: self
                .allowed_login_methods
                .as_ref()
                .map(|allowed| allowed.value.clone()),
            allowed_chatgpt_workspaces: self.allowed_chatgpt_workspaces.as_ref().map(|allowed| {
                allowed
                    .value
                    .iter()
                    .map(|workspace| workspace.trim())
                    .filter(|workspace| !workspace.is_empty())
                    .map(str::to_string)
                    .collect()
            }),
        }
    }

    pub fn exec_policy_source(&self) -> Option<&RequirementSource> {
        self.exec_policy.as_ref().map(|policy| &policy.source)
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceRequirementsToml {
    pub restrict_to_allowed_sources: Option<bool>,
    #[serde(default)]
    pub allowed_sources: BTreeMap<String, MarketplaceAllowedSourceToml>,
}

impl MarketplaceRequirementsToml {
    pub fn is_empty(&self) -> bool {
        self.restrict_to_allowed_sources.is_none() && self.allowed_sources.is_empty()
    }
}

/// Raw marketplace source rule whose active fields are interpreted after
/// requirements composition.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MarketplaceAllowedSourceToml {
    pub source: Option<MarketplaceAllowedSourceKind>,
    pub url: Option<String>,
    #[serde(rename = "ref")]
    pub ref_name: Option<String>,
    pub host_pattern: Option<String>,
    pub path: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceAllowedSourceKind {
    Git,
    HostPattern,
    Local,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkDomainPermissionsToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, NetworkDomainPermissionToml>,
}

impl NetworkDomainPermissionsToml {
    pub fn allowed_domains(&self) -> Option<Vec<String>> {
        let allowed_domains: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, permission)| matches!(permission, NetworkDomainPermissionToml::Allow))
            .map(|(pattern, _)| pattern.clone())
            .collect();
        (!allowed_domains.is_empty()).then_some(allowed_domains)
    }

    pub fn denied_domains(&self) -> Option<Vec<String>> {
        let denied_domains: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, permission)| matches!(permission, NetworkDomainPermissionToml::Deny))
            .map(|(pattern, _)| pattern.clone())
            .collect();
        (!denied_domains.is_empty()).then_some(denied_domains)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum NetworkDomainPermissionToml {
    Allow,
    Deny,
}

impl std::fmt::Display for NetworkDomainPermissionToml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let permission = match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        };
        f.write_str(permission)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkUnixSocketPermissionsToml {
    #[serde(flatten)]
    pub entries: BTreeMap<String, NetworkUnixSocketPermissionToml>,
}

impl NetworkUnixSocketPermissionsToml {
    pub fn allow_unix_sockets(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, permission)| matches!(permission, NetworkUnixSocketPermissionToml::Allow))
            .map(|(path, _)| path.clone())
            .collect()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum NetworkUnixSocketPermissionToml {
    Allow,
    Deny,
}

impl std::fmt::Display for NetworkUnixSocketPermissionToml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let permission = match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        };
        f.write_str(permission)
    }
}

#[derive(Serialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkRequirementsToml {
    pub enabled: Option<bool>,
    pub http_port: Option<u16>,
    pub socks_port: Option<u16>,
    pub allow_upstream_proxy: Option<bool>,
    pub dangerously_allow_non_loopback_proxy: Option<bool>,
    pub dangerously_allow_all_unix_sockets: Option<bool>,
    pub domains: Option<NetworkDomainPermissionsToml>,
    /// Host patterns routed to the existing network approval flow before ordinary allow rules.
    /// Explicit denials take precedence; approval caching, hooks, and reviewer routing still apply.
    pub review_domains: Option<Vec<String>>,
    /// When true, only managed `allowed_domains` are respected while managed
    /// network enforcement is active. User allowlist entries are ignored.
    pub managed_allowed_domains_only: Option<bool>,
    pub unix_sockets: Option<NetworkUnixSocketPermissionsToml>,
    /// MXC requires true when managed networking is enabled; an explicit false is rejected.
    pub allow_local_binding: Option<bool>,
    /// Requirements-only header injections. These annotate matching requests
    /// without changing whether non-matching requests are allowed.
    pub header_injections: Option<Vec<NetworkHeaderInjectionToml>>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct NetworkHeaderInjectionToml {
    pub host: String,
    pub methods: Vec<String>,
    pub path_prefixes: Vec<String>,
    pub headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for NetworkHeaderInjectionToml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHeaderInjectionToml")
            .field("host", &self.host)
            .field("methods", &self.methods)
            .field("path_prefixes", &self.path_prefixes)
            .field("header_names", &self.headers.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Deserialize)]
struct RawNetworkRequirementsToml {
    enabled: Option<bool>,
    http_port: Option<u16>,
    socks_port: Option<u16>,
    allow_upstream_proxy: Option<bool>,
    dangerously_allow_non_loopback_proxy: Option<bool>,
    dangerously_allow_all_unix_sockets: Option<bool>,
    domains: Option<NetworkDomainPermissionsToml>,
    review_domains: Option<Vec<String>>,
    #[serde(default)]
    allowed_domains: Option<Vec<String>>,
    /// When true, only managed `allowed_domains` are respected while managed
    /// network enforcement is active. User allowlist entries are ignored.
    managed_allowed_domains_only: Option<bool>,
    #[serde(default)]
    denied_domains: Option<Vec<String>>,
    unix_sockets: Option<NetworkUnixSocketPermissionsToml>,
    #[serde(default)]
    allow_unix_sockets: Option<Vec<String>>,
    allow_local_binding: Option<bool>,
    header_injections: Option<Vec<NetworkHeaderInjectionToml>>,
}

impl<'de> Deserialize<'de> for NetworkRequirementsToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawNetworkRequirementsToml::deserialize(deserializer)?;
        let RawNetworkRequirementsToml {
            enabled,
            http_port,
            socks_port,
            allow_upstream_proxy,
            dangerously_allow_non_loopback_proxy,
            dangerously_allow_all_unix_sockets,
            domains,
            review_domains,
            allowed_domains,
            managed_allowed_domains_only,
            denied_domains,
            unix_sockets,
            allow_unix_sockets,
            allow_local_binding,
            header_injections,
        } = raw;

        if domains.is_some() && (allowed_domains.is_some() || denied_domains.is_some()) {
            return Err(D::Error::custom(
                "`domains` cannot be combined with legacy `allowed_domains` or `denied_domains` in managed network requirements",
            ));
        }

        if unix_sockets.is_some() && allow_unix_sockets.is_some() {
            return Err(D::Error::custom(
                "`unix_sockets` cannot be combined with legacy `allow_unix_sockets` in managed network requirements",
            ));
        }

        Ok(Self {
            enabled,
            http_port,
            socks_port,
            allow_upstream_proxy,
            dangerously_allow_non_loopback_proxy,
            dangerously_allow_all_unix_sockets,
            domains: domains
                .or_else(|| legacy_domain_permissions_from_lists(allowed_domains, denied_domains)),
            review_domains,
            managed_allowed_domains_only,
            unix_sockets: unix_sockets
                .or_else(|| legacy_unix_socket_permissions_from_list(allow_unix_sockets)),
            allow_local_binding,
            header_injections,
        })
    }
}

/// Legacy list normalization is intentionally lossy: explicit empty legacy
/// lists are treated as unset when converted to the canonical network
/// permission shape.
fn legacy_domain_permissions_from_lists(
    allowed_domains: Option<Vec<String>>,
    denied_domains: Option<Vec<String>>,
) -> Option<NetworkDomainPermissionsToml> {
    let mut entries = BTreeMap::new();

    for pattern in allowed_domains.unwrap_or_default() {
        entries.insert(pattern, NetworkDomainPermissionToml::Allow);
    }

    for pattern in denied_domains.unwrap_or_default() {
        entries.insert(pattern, NetworkDomainPermissionToml::Deny);
    }

    (!entries.is_empty()).then_some(NetworkDomainPermissionsToml { entries })
}

fn legacy_unix_socket_permissions_from_list(
    allow_unix_sockets: Option<Vec<String>>,
) -> Option<NetworkUnixSocketPermissionsToml> {
    let entries = allow_unix_sockets
        .unwrap_or_default()
        .into_iter()
        .map(|path| (path, NetworkUnixSocketPermissionToml::Allow))
        .collect::<BTreeMap<_, _>>();

    (!entries.is_empty()).then_some(NetworkUnixSocketPermissionsToml { entries })
}

/// Normalized network constraints derived from requirements TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NetworkConstraints {
    pub enabled: Option<bool>,
    pub http_port: Option<u16>,
    pub socks_port: Option<u16>,
    pub allow_upstream_proxy: Option<bool>,
    pub dangerously_allow_non_loopback_proxy: Option<bool>,
    pub dangerously_allow_all_unix_sockets: Option<bool>,
    pub domains: Option<NetworkDomainPermissionsToml>,
    pub review_domains: Option<Vec<String>>,
    /// When true, only managed `allowed_domains` are respected while managed
    /// network enforcement is active. User allowlist entries are ignored.
    pub managed_allowed_domains_only: Option<bool>,
    pub unix_sockets: Option<NetworkUnixSocketPermissionsToml>,
    pub allow_local_binding: Option<bool>,
    pub header_injections: Option<Vec<NetworkHeaderInjectionToml>>,
}

impl<'de> Deserialize<'de> for NetworkConstraints {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let requirements = NetworkRequirementsToml::deserialize(deserializer)?;
        Ok(requirements.into())
    }
}

impl From<NetworkRequirementsToml> for NetworkConstraints {
    fn from(value: NetworkRequirementsToml) -> Self {
        let NetworkRequirementsToml {
            enabled,
            http_port,
            socks_port,
            allow_upstream_proxy,
            dangerously_allow_non_loopback_proxy,
            dangerously_allow_all_unix_sockets,
            domains,
            review_domains,
            managed_allowed_domains_only,
            unix_sockets,
            allow_local_binding,
            header_injections,
        } = value;
        Self {
            enabled,
            http_port,
            socks_port,
            allow_upstream_proxy,
            dangerously_allow_non_loopback_proxy,
            dangerously_allow_all_unix_sockets,
            domains,
            review_domains,
            managed_allowed_domains_only,
            unix_sockets,
            allow_local_binding,
            header_injections,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilesystemRequirementsToml {
    pub deny_read: Option<Vec<FilesystemDenyReadPattern>>,
}

#[derive(Deserialize)]
struct RawFilesystemRequirementsToml {
    deny_read: Option<Vec<FilesystemDenyReadPattern>>,
    description: Option<serde::de::IgnoredAny>,
    extends: Option<serde::de::IgnoredAny>,
    workspace_roots: Option<serde::de::IgnoredAny>,
    filesystem: Option<serde::de::IgnoredAny>,
    network: Option<serde::de::IgnoredAny>,
}

impl<'de> Deserialize<'de> for FilesystemRequirementsToml {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawFilesystemRequirementsToml::deserialize(deserializer)?;
        let RawFilesystemRequirementsToml {
            deny_read,
            description,
            extends,
            workspace_roots,
            filesystem,
            network,
        } = raw;

        if description.is_some()
            || extends.is_some()
            || workspace_roots.is_some()
            || filesystem.is_some()
            || network.is_some()
        {
            return Err(D::Error::custom(
                "`permissions.filesystem` is reserved for requirements-level filesystem constraints and cannot define a profile",
            ));
        }

        Ok(Self { deny_read })
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionsRequirementsToml {
    pub filesystem: Option<FilesystemRequirementsToml>,
    /// Shared network constraints. Policy publishers must use `experimental_network`
    /// until all clients that can read the policy support the chosen replacement.
    // Keep the optional raw payload out of config-loading futures' inline storage.
    #[serde(default, deserialize_with = "deserialize_permissions_network")]
    pub network: Option<Box<NetworkRequirementsToml>>,
    // `filesystem` and `network` are reserved for requirements-level
    // constraints and cannot name profiles.
    #[serde(default, flatten)]
    pub profiles: BTreeMap<String, PermissionProfileToml>,
}

impl PermissionsRequirementsToml {
    /// Split out the independent network policy without leaving an empty profile table behind.
    pub(crate) fn take_network(table: &mut Option<Self>) -> Option<Box<NetworkRequirementsToml>> {
        let permissions = table.as_mut()?;
        let network = permissions.network.take()?;
        if permissions.filesystem.is_none() && permissions.profiles.is_empty() {
            *table = None;
        }
        Some(network)
    }
}

fn deserialize_permissions_network<'de, D>(
    deserializer: D,
) -> Result<Option<Box<NetworkRequirementsToml>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let table = toml::Table::deserialize(deserializer)?;
    if table.is_empty()
        || [
            "description",
            "extends",
            "workspace_roots",
            "filesystem",
            "network",
        ]
        .iter()
        .any(|key| table.contains_key(*key))
    {
        return Err(D::Error::custom(
            "`permissions.network` in requirements.toml is reserved for shared network constraints; rename the managed profile (for example, to `network-access`) and update `default_permissions`, `allowed_permission_profiles`, and `extends`; check saved session profile references. Profiles named `network` in config.toml remain valid",
        ));
    }
    toml::Value::Table(table)
        .try_into()
        .map(Some)
        .map_err(D::Error::custom)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemConstraints {
    pub deny_read: Vec<FilesystemDenyReadPattern>,
}

impl From<PermissionsRequirementsToml> for FilesystemConstraints {
    fn from(value: PermissionsRequirementsToml) -> Self {
        let deny_read = value
            .filesystem
            .and_then(|filesystem| filesystem.deny_read)
            .unwrap_or_default();
        Self { deny_read }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct FilesystemDenyReadPattern(String);

impl FilesystemDenyReadPattern {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn contains_glob(&self) -> bool {
        self.0.chars().any(is_glob_metacharacter)
    }

    pub fn from_input(input: &str) -> Result<Self, String> {
        codex_utils_path_uri::PathUri::validate_config_path_text(
            input,
            crate::path_context::convention(),
        )
        .map_err(|error| error.to_string())?;
        if !input.chars().any(is_glob_metacharacter) {
            let path = deserialize_absolute_path(input)?;
            validate_literal_denial_path(&path)?;
            return Ok(Self(path));
        }

        let (directory_prefix, suffix) = split_glob_pattern(input);
        if crate::path_context::convention() == codex_utils_path_uri::PathConvention::Windows
            && matches!(input.as_bytes(), [b'/' | b'\\', b'/' | b'\\', ..])
            && !matches!(
                directory_prefix.as_bytes(),
                [b'/' | b'\\', b'/' | b'\\', ..]
            )
        {
            return Err(
                "filesystem denial glob requires a literal UNC server and share".to_string(),
            );
        }
        let normalized_prefix = if directory_prefix.is_empty() {
            deserialize_absolute_path(".")?
        } else {
            deserialize_absolute_path(directory_prefix)?
        };
        // The prefix is literal even when supplied home/base facts contain
        // glob syntax. Reject it before appending the user's pattern suffix.
        validate_literal_denial_path(&normalized_prefix)?;
        let normalized = if suffix.is_empty() {
            normalized_prefix
        } else if normalized_prefix == "/" {
            format!("/{suffix}")
        } else {
            format!("{normalized_prefix}/{suffix}")
        };
        codex_utils_path_uri::PathUri::validate_config_path_text(
            &normalized,
            crate::path_context::convention(),
        )
        .map_err(|error| error.to_string())?;
        Ok(Self(normalized))
    }
}

impl From<AbsolutePathBuf> for FilesystemDenyReadPattern {
    fn from(value: AbsolutePathBuf) -> Self {
        Self(value.to_string_lossy().into_owned())
    }
}

impl<'de> Deserialize<'de> for FilesystemDenyReadPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = String::deserialize(deserializer)?;
        Self::from_input(&input).map_err(D::Error::custom)
    }
}

fn validate_literal_denial_path(path: &str) -> Result<(), String> {
    let convention = crate::path_context::convention();
    codex_utils_path_uri::LegacyAppPathString::from_string(path)
        .to_path_uri(convention)
        .and_then(|path| path.validate_glob_directory(convention))
        .map_err(|error| error.to_string())
}

fn deserialize_absolute_path(input: &str) -> Result<String, String> {
    crate::path_context::resolve(input)
}

fn split_glob_pattern(input: &str) -> (&str, &str) {
    let Some(first_glob) = input.find(is_glob_metacharacter) else {
        return ("", input);
    };
    let separator_index = input[..first_glob]
        .char_indices()
        .rev()
        .find(|(_, ch)| is_path_separator(*ch))
        .map(|(index, _)| index);

    match separator_index {
        Some(0) => ("/", &input[1..]),
        Some(index)
            if crate::path_context::convention()
                == codex_utils_path_uri::PathConvention::Windows
                && index == 2
                && input.as_bytes().get(1) == Some(&b':')
                && input.as_bytes().get(2).is_some() =>
        {
            (&input[..=index], &input[index + 1..])
        }
        Some(index) => (&input[..index], &input[index + 1..]),
        None => ("", input),
    }
}

fn is_path_separator(ch: char) -> bool {
    if crate::path_context::convention() == codex_utils_path_uri::PathConvention::Windows {
        ch == '/' || ch == '\\'
    } else {
        ch == '/'
    }
}

fn is_glob_metacharacter(ch: char) -> bool {
    matches!(ch, '*' | '?' | '[')
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchModeRequirement {
    Disabled,
    Cached,
    Indexed,
    Live,
}

impl From<WebSearchMode> for WebSearchModeRequirement {
    fn from(mode: WebSearchMode) -> Self {
        match mode {
            WebSearchMode::Disabled => WebSearchModeRequirement::Disabled,
            WebSearchMode::Cached => WebSearchModeRequirement::Cached,
            WebSearchMode::Indexed => WebSearchModeRequirement::Indexed,
            WebSearchMode::Live => WebSearchModeRequirement::Live,
        }
    }
}

impl From<WebSearchModeRequirement> for WebSearchMode {
    fn from(mode: WebSearchModeRequirement) -> Self {
        match mode {
            WebSearchModeRequirement::Disabled => WebSearchMode::Disabled,
            WebSearchModeRequirement::Cached => WebSearchMode::Cached,
            WebSearchModeRequirement::Indexed => WebSearchMode::Indexed,
            WebSearchModeRequirement::Live => WebSearchMode::Live,
        }
    }
}

impl fmt::Display for WebSearchModeRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebSearchModeRequirement::Disabled => write!(f, "disabled"),
            WebSearchModeRequirement::Cached => write!(f, "cached"),
            WebSearchModeRequirement::Indexed => write!(f, "indexed"),
            WebSearchModeRequirement::Live => write!(f, "live"),
        }
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsRequirementsToml {
    pub allowed_sandbox_implementations: Option<Vec<WindowsSandboxImplementationToml>>,
    /// False blocks both explicit MXC configuration and automatic selection.
    pub allow_mxc: Option<bool>,
    /// Require MXC as the selected local Windows backend.
    pub require_mxc: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WindowsSandboxImplementationToml {
    Elevated,
    Unelevated,
}

impl WindowsRequirementsToml {
    pub fn is_empty(&self) -> bool {
        self.allowed_sandbox_implementations.is_none()
            && self.allow_mxc.is_none()
            && self.require_mxc.is_none()
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
#[serde(from = "FeatureRequirementsInput")]
pub struct FeatureRequirementsToml {
    pub entries: BTreeMap<String, bool>,
    pub runtime_auth_mode: Option<RuntimeAuthMode>,
}

// Keep boolean requirements unchanged while accepting the same typed runtime setting as config.
#[derive(Deserialize)]
struct FeatureRequirementsInput {
    runtime_agent_instances: Option<FeatureToml<RuntimeAgentInstancesConfigToml>>,
    #[serde(flatten)]
    entries: BTreeMap<String, bool>,
}

impl From<FeatureRequirementsInput> for FeatureRequirementsToml {
    fn from(input: FeatureRequirementsInput) -> Self {
        let mut entries = input.entries;
        if let Some(enabled) = input
            .runtime_agent_instances
            .as_ref()
            .and_then(FeatureToml::enabled)
        {
            entries.insert("runtime_agent_instances".to_string(), enabled);
        }
        let runtime_auth_mode = input
            .runtime_agent_instances
            .and_then(|feature| match feature {
                FeatureToml::Enabled(_) => None,
                FeatureToml::Config(config) => config.mode,
            });
        Self {
            entries,
            runtime_auth_mode,
        }
    }
}

impl FeatureRequirementsToml {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.runtime_auth_mode.is_none()
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AppToolRequirementToml {
    pub approval_mode: Option<AppToolApproval>,
    /// Opt-in analytics extraction for this exact tool, not a tool argument.
    /// The highest-priority rule wins as a whole, including unsupported formats.
    pub analytics_result_source: Option<AppToolResultSourceRequirementToml>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppToolResultSourceRequirementToml {
    /// Result format to parse; currently only `detailed_message_search_v1` is supported.
    pub format: AppToolResultSourceFormat,
    /// Source kind emitted alongside each extracted ID.
    #[serde(rename = "type")]
    pub source_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppToolResultSourceFormat {
    DetailedMessageSearchV1,
    /// Keep unknown formats so higher-priority rules still override lower ones.
    Unknown(String),
}

impl FromStr for AppToolResultSourceFormat {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "detailed_message_search_v1" => Self::DetailedMessageSearchV1,
            _ => Self::Unknown(value.to_string()),
        })
    }
}

impl<'de> Deserialize<'de> for AppToolResultSourceFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

impl AppToolRequirementToml {
    pub fn is_empty(&self) -> bool {
        self.approval_mode.is_none() && self.analytics_result_source.is_none()
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AppToolsRequirementsToml {
    #[serde(default, flatten)]
    pub tools: BTreeMap<String, AppToolRequirementToml>,
}

impl AppToolsRequirementsToml {
    pub fn is_empty(&self) -> bool {
        self.tools.values().all(AppToolRequirementToml::is_empty)
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AppRequirementToml {
    pub enabled: Option<bool>,
    pub tools: Option<AppToolsRequirementsToml>,
}

impl AppRequirementToml {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self
                .tools
                .as_ref()
                .is_none_or(AppToolsRequirementsToml::is_empty)
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AppsRequirementsToml {
    #[serde(default, flatten)]
    pub apps: BTreeMap<String, AppRequirementToml>,
}

impl AppsRequirementsToml {
    pub fn is_empty(&self) -> bool {
        self.apps.values().all(AppRequirementToml::is_empty)
    }
}

/// Merge app requirements from a lower-precedence source into an existing higher-precedence set.
/// This lets managed sources (for example Cloud/MDM) enforce setting disablement across layers,
/// while exact tool approval settings keep the higher-precedence value when present.
pub(crate) fn merge_app_requirements_descending(
    base: &mut AppsRequirementsToml,
    incoming: AppsRequirementsToml,
) {
    for (app_id, incoming_requirement) in incoming.apps {
        let base_requirement = base.apps.entry(app_id).or_default();
        let higher_precedence = base_requirement.enabled;
        let lower_precedence = incoming_requirement.enabled;
        base_requirement.enabled =
            if higher_precedence == Some(false) || lower_precedence == Some(false) {
                Some(false)
            } else {
                higher_precedence.or(lower_precedence)
            };

        let Some(incoming_tools) = incoming_requirement.tools else {
            continue;
        };
        let base_tools = base_requirement.tools.get_or_insert_with(Default::default);
        for (tool_name, incoming_tool) in incoming_tools.tools {
            let base_tool = base_tools.tools.entry(tool_name).or_default();
            if base_tool.approval_mode.is_none() {
                base_tool.approval_mode = incoming_tool.approval_mode;
            }
            if base_tool.analytics_result_source.is_none() {
                base_tool.analytics_result_source = incoming_tool.analytics_result_source;
            }
        }
    }
}

/// Base config deserialized from system `requirements.toml` or MDM.
#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ConfigRequirementsToml {
    /// Required customer-hosted configuration server. Only local managed requirements may configure it.
    pub config_server: Option<crate::ConfigServer>,
    pub allowed_login_methods: Option<Vec<ForcedLoginMethod>>,
    pub allowed_chatgpt_workspaces: Option<Vec<String>>,
    pub cli_auth_credentials_store: Option<AuthCredentialsStoreMode>,
    pub chatgpt_base_url: Option<String>,
    pub sqlite_home: Option<AbsolutePathBuf>,
    /// Require migration of existing WAL databases on NFS before opening writable pools.
    /// Unsafe: other clients on all hosts must remain stopped during replacement.
    /// Allows accessible shared files; mode bits are preserved, but ownership and ACLs are not.
    pub unsafe_enforce_sqlite_journal_mode: Option<bool>,
    pub log_dir: Option<AbsolutePathBuf>,
    pub model_catalog_json: Option<AbsolutePathBuf>,
    /// Exact provider selection, overriding local and session configuration.
    pub model_provider: Option<String>,
    /// Complete provider definitions; each entry replaces the configured provider.
    pub model_providers: Option<HashMap<String, ModelProviderInfo>>,
    pub check_for_update_on_startup: Option<bool>,
    pub allow_login_shell: Option<bool>,
    pub feedback: Option<FeedbackConfigToml>,
    pub allowed_approval_policies: Option<Vec<AskForApproval>>,
    pub allowed_approvals_reviewers: Option<Vec<ApprovalsReviewer>>,
    pub allowed_sandbox_modes: Option<Vec<SandboxModeRequirement>>,
    pub allowed_permission_profiles: Option<BTreeMap<String, bool>>,
    pub default_permissions: Option<String>,
    pub remote_sandbox_config: Option<Vec<RemoteSandboxConfigToml>>,
    pub allowed_web_search_modes: Option<Vec<WebSearchModeRequirement>>,
    pub allow_managed_hooks_only: Option<bool>,
    pub allow_browser_and_computer_use: Option<bool>,
    pub allow_appshots: Option<bool>,
    pub allow_remote_control: Option<bool>,
    pub computer_use: Option<ComputerUseRequirementsToml>,
    pub browser_use: Option<BrowserUseRequirementsToml>,
    pub in_app_browser: Option<InAppBrowserRequirementsToml>,
    pub windows: Option<WindowsRequirementsToml>,
    #[serde(rename = "features", alias = "feature_requirements")]
    pub feature_requirements: Option<FeatureRequirementsToml>,
    pub hooks: Option<ManagedHooksRequirementsToml>,
    pub mcp_servers: Option<BTreeMap<String, McpServerRequirement>>,
    pub plugins: Option<BTreeMap<String, PluginRequirementsToml>>,
    pub marketplaces: Option<MarketplaceRequirementsToml>,
    pub otel: Option<Box<OtelRequirementsToml>>,
    pub apps: Option<AppsRequirementsToml>,
    pub rules: Option<RequirementsExecPolicyToml>,
    pub enforce_residency: Option<ResidencyRequirement>,
    /// Shared managed network constraints; `experimental_network` remains an alias.
    #[serde(rename = "network_proxy", alias = "experimental_network")]
    pub network: Option<NetworkRequirementsToml>,
    pub application: Option<ApplicationRequirementsToml>,
    pub permissions: Option<PermissionsRequirementsToml>,
    pub auto_review: Option<AutoReviewRequirementsToml>,
    pub models: Option<ModelsRequirementsToml>,
    pub additional_developer_instructions: Option<String>,
    pub guardian_policy_config: Option<String>,
    pub guardian_extra_policy: Option<String>,
}

/// OTEL settings that may be managed through `requirements.toml`.
///
/// Tool-result limits are optional here because `config.toml` defaults them,
/// while requirements must distinguish unset from managed-to-default.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OtelRequirementsToml {
    pub tool_result: Option<ToolResultLogConfig>,
    pub log_sse_deltas: Option<bool>,
    pub log_user_prompt: Option<bool>,
    pub log_agent_responses: Option<bool>,
    pub log_guardian_assessments: Option<bool>,
    pub environment: Option<String>,
    pub exporter: Option<OtelExporterKind>,
    pub trace_exporter: Option<OtelExporterKind>,
    pub metrics_exporter: Option<OtelExporterKind>,
    pub span_attributes: Option<HashMap<String, String>>,
    pub tracestate: Option<BTreeMap<String, BTreeMap<String, String>>>,
}

impl OtelRequirementsToml {
    pub(crate) fn is_empty(&self) -> bool {
        self.tool_result.is_none()
            && self.log_sse_deltas.is_none()
            && self.log_user_prompt.is_none()
            && self.log_agent_responses.is_none()
            && self.log_guardian_assessments.is_none()
            && self.environment.is_none()
            && self.exporter.is_none()
            && self.trace_exporter.is_none()
            && self.metrics_exporter.is_none()
            && !self.has_metadata()
    }

    fn has_metadata(&self) -> bool {
        self.span_attributes
            .as_ref()
            .is_some_and(|fields| !fields.is_empty())
            || self
                .tracestate
                .as_ref()
                .is_some_and(|members| members.values().any(|fields| !fields.is_empty()))
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoReviewRequirementsToml {
    pub required_on_models: Option<Vec<String>>,
    pub ignore_rules: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelsRequirementsToml {
    pub new_thread: Option<NewThreadModelDefaultsToml>,
}

impl ModelsRequirementsToml {
    fn is_empty(&self) -> bool {
        self.new_thread
            .as_ref()
            .is_none_or(NewThreadModelDefaultsToml::is_empty)
    }
}

#[derive(Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct NewThreadModelDefaultsToml {
    pub model: Option<String>,
    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub service_tier: Option<String>,
}

impl NewThreadModelDefaultsToml {
    fn is_empty(&self) -> bool {
        self.model.is_none() && self.model_reasoning_effort.is_none() && self.service_tier.is_none()
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct RemoteSandboxConfigToml {
    pub hostname_patterns: Vec<String>,
    pub allowed_sandbox_modes: Vec<SandboxModeRequirement>,
}

/// Value paired with the requirement source it came from, for better error
/// messages.
#[derive(Debug, Clone, PartialEq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: RequirementSource,
}

impl<T> Sourced<T> {
    pub fn new(value: T, source: RequirementSource) -> Self {
        Self { value, source }
    }
}

impl<T> std::ops::Deref for Sourced<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigRequirementsWithSources {
    pub config_server: Option<Sourced<crate::ConfigServer>>,
    pub allowed_login_methods: Option<Sourced<Vec<ForcedLoginMethod>>>,
    pub allowed_chatgpt_workspaces: Option<Sourced<Vec<String>>>,
    pub cli_auth_credentials_store: Option<Sourced<AuthCredentialsStoreMode>>,
    pub chatgpt_base_url: Option<Sourced<String>>,
    pub sqlite_home: Option<Sourced<AbsolutePathBuf>>,
    pub unsafe_enforce_sqlite_journal_mode: Option<Sourced<bool>>,
    pub log_dir: Option<Sourced<AbsolutePathBuf>>,
    pub model_catalog_json: Option<Sourced<AbsolutePathBuf>>,
    pub model_provider: Option<Sourced<String>>,
    pub model_providers: Option<Sourced<HashMap<String, ModelProviderInfo>>>,
    pub check_for_update_on_startup: Option<Sourced<bool>>,
    pub allow_login_shell: Option<Sourced<bool>>,
    pub feedback: Option<Sourced<FeedbackConfigToml>>,
    pub allowed_approval_policies: Option<Sourced<Vec<AskForApproval>>>,
    pub allowed_approvals_reviewers: Option<Sourced<Vec<ApprovalsReviewer>>>,
    pub allowed_sandbox_modes: Option<Sourced<Vec<SandboxModeRequirement>>>,
    pub allowed_permission_profiles: Option<Sourced<BTreeMap<String, bool>>>,
    pub default_permissions: Option<Sourced<String>>,
    pub allowed_web_search_modes: Option<Sourced<Vec<WebSearchModeRequirement>>>,
    pub allow_managed_hooks_only: Option<Sourced<bool>>,
    pub allow_browser_and_computer_use: Option<Sourced<bool>>,
    pub allow_appshots: Option<Sourced<bool>>,
    pub allow_remote_control: Option<Sourced<bool>>,
    pub computer_use: Option<Sourced<ComputerUseRequirementsToml>>,
    pub browser_use: Option<Sourced<BrowserUseRequirementsToml>>,
    pub in_app_browser: Option<Sourced<InAppBrowserRequirementsToml>>,
    pub windows: Option<Sourced<WindowsRequirementsToml>>,
    pub feature_requirements: Option<Sourced<FeatureRequirementsToml>>,
    pub hooks: Option<Sourced<ManagedHooksRequirementsToml>>,
    pub mcp_servers: Option<Sourced<BTreeMap<String, McpServerRequirement>>>,
    pub plugins: Option<Sourced<BTreeMap<String, PluginRequirementsToml>>>,
    pub marketplaces: Option<Sourced<MarketplaceRequirementsToml>>,
    pub otel: Option<Box<Sourced<OtelRequirementsToml>>>,
    pub apps: Option<Sourced<AppsRequirementsToml>>,
    pub rules: Option<Sourced<RequirementsExecPolicyToml>>,
    pub enforce_residency: Option<Sourced<ResidencyRequirement>>,
    pub network: Option<Sourced<NetworkRequirementsToml>>,
    pub application: Option<Sourced<ApplicationRequirementsToml>>,
    pub permissions: Option<Sourced<PermissionsRequirementsToml>>,
    /// Kept separate from profiles and filesystem so each retains its own precedence and source.
    pub permissions_network: Option<Box<Sourced<NetworkRequirementsToml>>>,
    pub auto_review: Option<Sourced<AutoReviewRequirementsToml>>,
    pub models: Option<Sourced<ModelsRequirementsToml>>,
    pub additional_developer_instructions: Option<Sourced<String>>,
    pub guardian_policy_config: Option<Sourced<String>>,
    pub guardian_extra_policy: Option<Sourced<String>>,
}

impl ConfigRequirementsWithSources {
    pub(crate) fn validate_network_spelling(&self) -> Result<(), ConstraintError> {
        if let (Some(nested), Some(top_level)) = (&self.permissions_network, &self.network) {
            return Err(ConstraintError::InvalidValue {
                field_name: "permissions.network",
                candidate: format!(
                    "configured together with top-level network requirements from {}",
                    top_level.source
                ),
                allowed: "specify managed network requirements through exactly one surface"
                    .to_string(),
                requirement_source: nested.source.clone(),
            });
        }
        Ok(())
    }

    pub fn merge_unset_fields(&mut self, source: RequirementSource, other: ConfigRequirementsToml) {
        // For every field in `other` that is `Some`, if the corresponding field
        // in `self` is `None`, copy the value from `other` into `self`.
        macro_rules! fill_missing_take {
            ($base:expr, $other:expr, $source:expr, { $($field:ident),+ $(,)? }) => {
                $(
                    if $base.$field.is_none()
                        && let Some(value) = $other.$field.take()
                    {
                        $base.$field = Some(Sourced::new(value, $source.clone()));
                    }
                )+
            };
        }

        // Destructure without `..` so adding fields to `ConfigRequirementsToml`
        // forces this merge logic to be updated.
        let ConfigRequirementsToml {
            config_server: _,
            allowed_login_methods: _,
            allowed_chatgpt_workspaces: _,
            cli_auth_credentials_store: _,
            chatgpt_base_url: _,
            sqlite_home: _,
            unsafe_enforce_sqlite_journal_mode: _,
            log_dir: _,
            model_catalog_json: _,
            model_provider: _,
            model_providers: _,
            check_for_update_on_startup: _,
            allow_login_shell: _,
            feedback: _,
            allowed_approval_policies: _,
            allowed_approvals_reviewers: _,
            allowed_sandbox_modes: _,
            allowed_permission_profiles: _,
            default_permissions: _,
            remote_sandbox_config: _,
            allowed_web_search_modes: _,
            allow_managed_hooks_only: _,
            allow_browser_and_computer_use: _,
            allow_appshots: _,
            allow_remote_control: _,
            computer_use: _,
            browser_use: _,
            in_app_browser: _,
            windows: _,
            feature_requirements: _,
            hooks: _,
            mcp_servers: _,
            plugins: _,
            marketplaces: _,
            otel: _,
            apps: _,
            rules: _,
            enforce_residency: _,
            network: _,
            application: _,
            permissions: _,
            auto_review: _,
            models: _,
            additional_developer_instructions: _,
            guardian_policy_config: _,
            guardian_extra_policy: _,
        } = &other;

        let mut other = other;
        if let Some(network) = PermissionsRequirementsToml::take_network(&mut other.permissions)
            && self.permissions_network.is_none()
        {
            self.permissions_network = Some(Box::new(Sourced::new(*network, source.clone())));
        }
        if other
            .guardian_policy_config
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            other.guardian_policy_config = None;
        }
        if other
            .guardian_extra_policy
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            other.guardian_extra_policy = None;
        }
        fill_missing_take!(
            self,
            other,
            source,
            {
                config_server,
                allowed_login_methods,
                allowed_chatgpt_workspaces,
                cli_auth_credentials_store,
                chatgpt_base_url,
                sqlite_home,
                unsafe_enforce_sqlite_journal_mode,
                log_dir,
                model_catalog_json,
                model_provider,
                model_providers,
                check_for_update_on_startup,
                allow_login_shell,
                feedback,
                allowed_approval_policies,
                allowed_approvals_reviewers,
                allowed_sandbox_modes,
                allowed_permission_profiles,
                default_permissions,
                allowed_web_search_modes,
                allow_managed_hooks_only,
                allow_browser_and_computer_use,
                allow_appshots,
                allow_remote_control,
                computer_use,
                browser_use,
                in_app_browser,
                windows,
                feature_requirements,
                hooks,
                mcp_servers,
                plugins,
                marketplaces,
                rules,
                enforce_residency,
                network,
                application,
                permissions,
                models,
                additional_developer_instructions,
                guardian_policy_config,
                guardian_extra_policy,
            }
        );

        if self.otel.is_none()
            && let Some(value) = other.otel.take()
        {
            self.otel = Some(Box::new(Sourced::new(*value, source.clone())));
        }

        if let Some(incoming_auto_review) = other.auto_review.take() {
            if let Some(existing_auto_review) = self.auto_review.as_mut() {
                let mut source_contributed = false;
                if let Some(incoming_slugs) = incoming_auto_review.required_on_models {
                    let protected_slugs = existing_auto_review
                        .value
                        .required_on_models
                        .get_or_insert_default();
                    for slug in incoming_slugs {
                        if !protected_slugs.contains(&slug) {
                            protected_slugs.push(slug);
                            source_contributed = true;
                        }
                    }
                }
                if existing_auto_review.value.ignore_rules.is_none()
                    && let Some(ignore_rules) = incoming_auto_review.ignore_rules
                {
                    existing_auto_review.value.ignore_rules = Some(ignore_rules);
                    source_contributed = true;
                }
                if source_contributed && existing_auto_review.source != source {
                    existing_auto_review.source = RequirementSource::composite([
                        existing_auto_review.source.clone(),
                        source.clone(),
                    ]);
                }
            } else {
                self.auto_review = Some(Sourced::new(incoming_auto_review, source.clone()));
            }
        }

        if let Some(incoming_apps) = other.apps.take() {
            if let Some(existing_apps) = self.apps.as_mut() {
                merge_app_requirements_descending(&mut existing_apps.value, incoming_apps);
            } else {
                self.apps = Some(Sourced::new(incoming_apps, source));
            }
        }
    }

    pub fn into_toml(self) -> ConfigRequirementsToml {
        let ConfigRequirementsWithSources {
            config_server,
            allowed_login_methods,
            allowed_chatgpt_workspaces,
            cli_auth_credentials_store,
            chatgpt_base_url,
            sqlite_home,
            unsafe_enforce_sqlite_journal_mode,
            log_dir,
            model_catalog_json,
            model_provider,
            model_providers,
            check_for_update_on_startup,
            allow_login_shell,
            feedback,
            allowed_approval_policies,
            allowed_approvals_reviewers,
            allowed_sandbox_modes,
            allowed_permission_profiles,
            default_permissions,
            allowed_web_search_modes,
            allow_managed_hooks_only,
            allow_browser_and_computer_use,
            allow_appshots,
            allow_remote_control,
            computer_use,
            browser_use,
            in_app_browser,
            windows,
            feature_requirements,
            hooks,
            mcp_servers,
            plugins,
            marketplaces,
            otel,
            apps,
            rules,
            enforce_residency,
            network,
            application,
            permissions,
            permissions_network,
            auto_review,
            models,
            additional_developer_instructions,
            guardian_policy_config,
            guardian_extra_policy,
        } = self;
        let mut permissions = permissions.map(|sourced| sourced.value);
        if let Some(network) = permissions_network {
            permissions.get_or_insert_with(Default::default).network =
                Some(Box::new(network.value));
        }
        ConfigRequirementsToml {
            config_server: config_server.map(|sourced| sourced.value),
            allowed_login_methods: allowed_login_methods.map(|sourced| sourced.value),
            allowed_chatgpt_workspaces: allowed_chatgpt_workspaces.map(|sourced| sourced.value),
            cli_auth_credentials_store: cli_auth_credentials_store.map(|sourced| sourced.value),
            chatgpt_base_url: chatgpt_base_url.map(|sourced| sourced.value),
            sqlite_home: sqlite_home.map(|sourced| sourced.value),
            unsafe_enforce_sqlite_journal_mode: unsafe_enforce_sqlite_journal_mode
                .map(|sourced| sourced.value),
            log_dir: log_dir.map(|sourced| sourced.value),
            model_catalog_json: model_catalog_json.map(|sourced| sourced.value),
            model_provider: model_provider.map(|sourced| sourced.value),
            model_providers: model_providers.map(|sourced| sourced.value),
            check_for_update_on_startup: check_for_update_on_startup.map(|sourced| sourced.value),
            allow_login_shell: allow_login_shell.map(|sourced| sourced.value),
            feedback: feedback.map(|sourced| sourced.value),
            allowed_approval_policies: allowed_approval_policies.map(|sourced| sourced.value),
            allowed_approvals_reviewers: allowed_approvals_reviewers.map(|sourced| sourced.value),
            allowed_sandbox_modes: allowed_sandbox_modes.map(|sourced| sourced.value),
            allowed_permission_profiles: allowed_permission_profiles.map(|sourced| sourced.value),
            default_permissions: default_permissions.map(|sourced| sourced.value),
            remote_sandbox_config: None,
            allowed_web_search_modes: allowed_web_search_modes.map(|sourced| sourced.value),
            allow_managed_hooks_only: allow_managed_hooks_only.map(|sourced| sourced.value),
            allow_browser_and_computer_use: allow_browser_and_computer_use
                .map(|sourced| sourced.value),
            allow_appshots: allow_appshots.map(|sourced| sourced.value),
            allow_remote_control: allow_remote_control.map(|sourced| sourced.value),
            computer_use: computer_use.map(|sourced| sourced.value),
            browser_use: browser_use.map(|sourced| sourced.value),
            in_app_browser: in_app_browser.map(|sourced| sourced.value),
            windows: windows.map(|sourced| sourced.value),
            feature_requirements: feature_requirements.map(|sourced| sourced.value),
            hooks: hooks.map(|sourced| sourced.value),
            mcp_servers: mcp_servers.map(|sourced| sourced.value),
            plugins: plugins.map(|sourced| sourced.value),
            marketplaces: marketplaces.map(|sourced| sourced.value),
            otel: otel.map(|sourced| Box::new(sourced.value)),
            apps: apps.map(|sourced| sourced.value),
            rules: rules.map(|sourced| sourced.value),
            enforce_residency: enforce_residency.map(|sourced| sourced.value),
            network: network.map(|sourced| sourced.value),
            application: application.map(|sourced| sourced.value),
            permissions,
            auto_review: auto_review.map(|sourced| sourced.value),
            models: models.map(|sourced| sourced.value),
            additional_developer_instructions: additional_developer_instructions
                .map(|sourced| sourced.value),
            guardian_policy_config: guardian_policy_config.map(|sourced| sourced.value),
            guardian_extra_policy: guardian_extra_policy.map(|sourced| sourced.value),
        }
    }
}

fn normalize_hostname(hostname: &str) -> Option<String> {
    let hostname = hostname.trim().trim_end_matches('.');
    (!hostname.is_empty()).then(|| hostname.to_ascii_lowercase())
}

fn hostname_matches_any_pattern(hostname: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        normalize_hostname(pattern)
            .map(|pattern| WildMatchPattern::<'*', '?'>::new_case_insensitive(&pattern))
            .is_some_and(|pattern| pattern.matches(hostname))
    })
}

/// Currently, `external-sandbox` is not supported in config.toml, but it is
/// supported through programmatic use.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum SandboxModeRequirement {
    #[serde(rename = "read-only")]
    ReadOnly,

    #[serde(rename = "workspace-write")]
    WorkspaceWrite,

    #[serde(rename = "danger-full-access")]
    DangerFullAccess,

    #[serde(rename = "external-sandbox")]
    ExternalSandbox,
}

impl From<SandboxMode> for SandboxModeRequirement {
    fn from(mode: SandboxMode) -> Self {
        match mode {
            SandboxMode::ReadOnly => SandboxModeRequirement::ReadOnly,
            SandboxMode::WorkspaceWrite => SandboxModeRequirement::WorkspaceWrite,
            SandboxMode::DangerFullAccess => SandboxModeRequirement::DangerFullAccess,
        }
    }
}

impl ConfigRequirementsToml {
    /// Whether local administration requires a remotely delivered managed bundle.
    pub fn requires_remote_config(&self) -> bool {
        self.config_server.is_some()
    }

    pub fn apply_remote_sandbox_config(&mut self, hostname: Option<&str>) {
        let Some(remote_sandbox_config) = self.remote_sandbox_config.as_ref() else {
            return;
        };
        let Some(hostname) = hostname.and_then(normalize_hostname) else {
            return;
        };
        let Some(matched_config) = remote_sandbox_config
            .iter()
            .find(|config| hostname_matches_any_pattern(&hostname, &config.hostname_patterns))
        else {
            return;
        };
        self.allowed_sandbox_modes = Some(matched_config.allowed_sandbox_modes.clone());
    }

    pub fn is_empty(&self) -> bool {
        self.config_server.is_none()
            && self.allowed_login_methods.is_none()
            && self.allowed_chatgpt_workspaces.is_none()
            && self.cli_auth_credentials_store.is_none()
            && self.chatgpt_base_url.is_none()
            && self.sqlite_home.is_none()
            && self.unsafe_enforce_sqlite_journal_mode.is_none()
            && self.log_dir.is_none()
            && self.model_catalog_json.is_none()
            && self.model_provider.is_none()
            && self.model_providers.as_ref().is_none_or(HashMap::is_empty)
            && self.check_for_update_on_startup.is_none()
            && self.allow_login_shell.is_none()
            && self
                .feedback
                .as_ref()
                .is_none_or(|feedback| feedback == &FeedbackConfigToml::default())
            && self.allowed_approval_policies.is_none()
            && self.allowed_approvals_reviewers.is_none()
            && self.allowed_sandbox_modes.is_none()
            && self.allowed_permission_profiles.is_none()
            && self.default_permissions.is_none()
            && self.remote_sandbox_config.is_none()
            && self.allowed_web_search_modes.is_none()
            && self.allow_managed_hooks_only.is_none()
            && self.allow_browser_and_computer_use.is_none()
            && self.allow_appshots.is_none()
            && self.allow_remote_control.is_none()
            && self
                .computer_use
                .as_ref()
                .is_none_or(ComputerUseRequirementsToml::is_empty)
            && self
                .browser_use
                .as_ref()
                .is_none_or(BrowserUseRequirementsToml::is_empty)
            && self
                .in_app_browser
                .as_ref()
                .is_none_or(|requirements| requirements == &InAppBrowserRequirementsToml::default())
            && self
                .windows
                .as_ref()
                .is_none_or(WindowsRequirementsToml::is_empty)
            && self
                .feature_requirements
                .as_ref()
                .is_none_or(FeatureRequirementsToml::is_empty)
            && self
                .hooks
                .as_ref()
                .is_none_or(ManagedHooksRequirementsToml::is_empty)
            && self.mcp_servers.is_none()
            && self
                .plugins
                .as_ref()
                .is_none_or(|plugins| plugins.values().all(PluginRequirementsToml::is_empty))
            && self
                .marketplaces
                .as_ref()
                .is_none_or(MarketplaceRequirementsToml::is_empty)
            && self
                .otel
                .as_deref()
                .is_none_or(OtelRequirementsToml::is_empty)
            && self
                .apps
                .as_ref()
                .is_none_or(AppsRequirementsToml::is_empty)
            && self.rules.is_none()
            && self.enforce_residency.is_none()
            && self.network.is_none()
            && self.application.as_ref().is_none_or(|application| {
                application.network.is_none()
                    && application
                        .storage
                        .as_deref()
                        .is_none_or(crate::ApplicationStorageRequirementsToml::is_empty)
            })
            && self.permissions.is_none()
            && self.auto_review.as_ref().is_none_or(|auto_review| {
                auto_review.ignore_rules.as_ref().is_none_or(Vec::is_empty)
                    && auto_review
                        .required_on_models
                        .as_ref()
                        .is_none_or(Vec::is_empty)
            })
            && self
                .models
                .as_ref()
                .is_none_or(ModelsRequirementsToml::is_empty)
            && self.additional_developer_instructions.is_none()
            && self
                .guardian_policy_config
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            && self
                .guardian_extra_policy
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
    }

    /// Applies the requirements whose values replace config values.
    ///
    /// This projection keeps config/read aligned with the final runtime config.
    pub fn apply_exact_to_config(&self, config: &mut ConfigToml) {
        macro_rules! apply_exact {
            ($field:ident) => {
                if let Some(value) = self.$field.as_ref() {
                    config.$field = Some(value.clone());
                }
            };
        }

        apply_exact!(cli_auth_credentials_store);
        apply_exact!(chatgpt_base_url);
        apply_exact!(sqlite_home);
        apply_exact!(log_dir);
        if let Some(storage) = self
            .application
            .as_ref()
            .and_then(|application| application.storage.as_ref())
        {
            storage.apply_to_config(config);
        }
        apply_exact!(model_catalog_json);
        apply_exact!(model_provider);
        if let Some(providers) = &self.model_providers {
            config.model_providers.extend(providers.clone());
        }
        apply_exact!(check_for_update_on_startup);
        apply_exact!(allow_login_shell);

        if self
            .allowed_approvals_reviewers
            .as_ref()
            .is_some_and(|reviewers| !reviewers.contains(&ApprovalsReviewer::User))
        {
            config.features.get_or_insert_default().guardianv2 = Some(FeatureToml::Enabled(false));
        }

        if let Some(enabled) = self.feedback.as_ref().and_then(|feedback| feedback.enabled) {
            config.feedback.get_or_insert_default().enabled = Some(enabled);
        }
        if let Some(requirements) = &self.otel
            && !requirements.is_empty()
        {
            apply_otel_requirements(config.otel.get_or_insert_default(), requirements);
        }
    }

    /// Returns the exact managed field affected by editing `segments`.
    pub fn exact_requirement_for_config_path(&self, segments: &[String]) -> Option<&'static str> {
        if let Some(field) = self
            .application
            .as_ref()
            .and_then(|application| application.storage.as_ref())
            .and_then(|storage| storage.exact_requirement_for_config_path(segments))
        {
            return Some(field);
        }
        if self.model_providers.as_ref().is_some_and(|providers| {
            providers
                .keys()
                .any(|id| config_paths_overlap(segments, &["model_providers", id]))
        }) {
            return Some("model_providers");
        }
        let otel = self.otel.as_ref();
        let managed_fields: [(bool, &[&str], &'static str); 18] = [
            (
                self.model_provider.is_some(),
                &["model_provider"],
                "model_provider",
            ),
            (self.sqlite_home.is_some(), &["sqlite_home"], "sqlite_home"),
            (self.log_dir.is_some(), &["log_dir"], "log_dir"),
            (
                self.model_catalog_json.is_some(),
                &["model_catalog_json"],
                "model_catalog_json",
            ),
            (
                self.check_for_update_on_startup.is_some(),
                &["check_for_update_on_startup"],
                "check_for_update_on_startup",
            ),
            (
                self.allow_login_shell.is_some(),
                &["allow_login_shell"],
                "allow_login_shell",
            ),
            (
                self.feedback
                    .as_ref()
                    .and_then(|feedback| feedback.enabled)
                    .is_some(),
                &["feedback", "enabled"],
                "feedback.enabled",
            ),
            (
                self.cli_auth_credentials_store.is_some(),
                &["cli_auth_credentials_store"],
                "cli_auth_credentials_store",
            ),
            (
                self.chatgpt_base_url.is_some(),
                &["chatgpt_base_url"],
                "chatgpt_base_url",
            ),
            (
                otel.is_some_and(|otel| otel.tool_result.is_some()),
                &["otel", "tool_result"],
                "otel.tool_result",
            ),
            (
                otel.is_some_and(|otel| otel.log_user_prompt.is_some()),
                &["otel", "log_user_prompt"],
                "otel.log_user_prompt",
            ),
            (
                otel.is_some_and(|otel| otel.log_sse_deltas.is_some()),
                &["otel", "log_sse_deltas"],
                "otel.log_sse_deltas",
            ),
            (
                otel.is_some_and(|otel| otel.log_agent_responses.is_some()),
                &["otel", "log_agent_responses"],
                "otel.log_agent_responses",
            ),
            (
                otel.is_some_and(|otel| otel.log_guardian_assessments.is_some()),
                &["otel", "log_guardian_assessments"],
                "otel.log_guardian_assessments",
            ),
            (
                otel.is_some_and(|otel| otel.environment.is_some()),
                &["otel", "environment"],
                "otel.environment",
            ),
            (
                otel.is_some_and(|otel| otel.exporter.is_some()),
                &["otel", "exporter"],
                "otel.exporter",
            ),
            (
                otel.is_some_and(|otel| otel.trace_exporter.is_some()),
                &["otel", "trace_exporter"],
                "otel.trace_exporter",
            ),
            (
                otel.is_some_and(|otel| otel.metrics_exporter.is_some()),
                &["otel", "metrics_exporter"],
                "otel.metrics_exporter",
            ),
        ];

        managed_fields
            .into_iter()
            .find_map(|(is_managed, managed_path, field)| {
                (is_managed && config_paths_overlap(segments, managed_path)).then_some(field)
            })
            .or_else(|| {
                otel.and_then(|otel| {
                    exact_otel_metadata_requirement_for_config_path(otel, segments)
                })
            })
    }
}

macro_rules! otel_merge_functions {
    ($($field:ident),+ $(,)?) => {
        /// Merges the supplied fields and reports whether this layer contributed a requirement.
        pub(crate) fn merge_otel_requirements(
            requirements: &mut Option<OtelRequirementsToml>,
            higher_priority: &OtelRequirementsToml,
        ) -> bool {
            let mut contributed = false;
            $(if let Some(value) = &higher_priority.$field {
                requirements.get_or_insert_default().$field = Some(value.clone());
                contributed = true;
            })+
            if higher_priority.has_metadata() {
                let target = requirements.get_or_insert_default();
                merge_otel_metadata(
                    &mut target.span_attributes,
                    &mut target.tracestate,
                    higher_priority,
                );
                contributed = true;
            }
            contributed
        }

        /// Applies managed OTEL values and reports whether any configured value was overridden.
        pub fn apply_otel_requirements(
            config: &mut OtelConfigToml,
            requirements: &OtelRequirementsToml,
        ) -> bool {
            let mut conflict = false;
            $(if let Some(value) = &requirements.$field {
                conflict |= config.$field.as_ref().is_some_and(|current| current != value);
                config.$field = Some(value.clone());
            })+
            conflict |= merge_otel_metadata(
                &mut config.span_attributes,
                &mut config.tracestate,
                requirements,
            );
            conflict
        }
    };
}

otel_merge_functions!(
    tool_result,
    log_sse_deltas,
    log_user_prompt,
    log_agent_responses,
    log_guardian_assessments,
    environment,
    exporter,
    trace_exporter,
    metrics_exporter,
);

fn merge_otel_metadata(
    span_attributes: &mut Option<HashMap<String, String>>,
    tracestate: &mut Option<BTreeMap<String, BTreeMap<String, String>>>,
    requirements: &OtelRequirementsToml,
) -> bool {
    let mut conflict = false;
    if let Some(attributes) = &requirements.span_attributes
        && !attributes.is_empty()
    {
        let configured = span_attributes.get_or_insert_default();
        for (key, value) in attributes {
            conflict |= configured
                .insert(key.clone(), value.clone())
                .is_some_and(|configured| configured != *value);
        }
    }
    if let Some(members) = &requirements.tracestate {
        for (member, fields) in members.iter().filter(|(_, fields)| !fields.is_empty()) {
            let configured = tracestate
                .get_or_insert_default()
                .entry(member.clone())
                .or_default();
            for (key, value) in fields {
                conflict |= configured
                    .insert(key.clone(), value.clone())
                    .is_some_and(|configured| configured != *value);
            }
        }
    }
    conflict
}

fn exact_otel_metadata_requirement_for_config_path(
    requirements: &OtelRequirementsToml,
    segments: &[String],
) -> Option<&'static str> {
    if requirements
        .span_attributes
        .as_ref()
        .is_some_and(|attributes| {
            attributes.keys().any(|key| {
                config_paths_overlap(segments, &["otel", "span_attributes", key.as_str()])
            })
        })
    {
        return Some("otel.span_attributes");
    }

    requirements
        .tracestate
        .as_ref()
        .is_some_and(|members| {
            members.iter().any(|(member, fields)| {
                fields.keys().any(|field| {
                    config_paths_overlap(
                        segments,
                        &["otel", "tracestate", member.as_str(), field.as_str()],
                    )
                })
            })
        })
        .then_some("otel.tracestate")
}

fn config_paths_overlap(segments: &[String], managed_path: &[&str]) -> bool {
    segments
        .iter()
        .zip(managed_path)
        .all(|(segment, managed_segment)| segment == managed_segment)
}

fn validate_mcp_server_requirements(
    requirements: &BTreeMap<String, McpServerRequirement>,
    source: &RequirementSource,
    plugin_name: Option<&str>,
) -> Result<(), ConstraintError> {
    for (server_name, requirement) in requirements {
        validate_mcp_server_requirement(requirement).map_err(|reason| {
            ConstraintError::McpServerRequirementParse {
                server_name: plugin_name
                    .map(|plugin_name| format!("{plugin_name}/{server_name}"))
                    .unwrap_or_else(|| server_name.clone()),
                requirement_source: source.clone(),
                reason,
            }
        })?;
    }
    Ok(())
}

impl TryFrom<ConfigRequirementsWithSources> for ConfigRequirements {
    type Error = ConstraintError;

    fn try_from(toml: ConfigRequirementsWithSources) -> Result<Self, Self::Error> {
        toml.validate_network_spelling()?;
        // Profile catalog selection remains on ConfigRequirementsToml for
        // config loading and requirements API projection. Managed new-thread
        // defaults also remain there because they are initialization values;
        // model-specific auto-review requirements are runtime constraints.
        let ConfigRequirementsWithSources {
            config_server: _,
            allowed_login_methods,
            allowed_chatgpt_workspaces,
            cli_auth_credentials_store,
            chatgpt_base_url,
            sqlite_home,
            unsafe_enforce_sqlite_journal_mode,
            log_dir,
            model_catalog_json,
            model_provider,
            model_providers,
            check_for_update_on_startup,
            allow_login_shell,
            feedback,
            allowed_approval_policies,
            allowed_approvals_reviewers,
            allowed_sandbox_modes,
            allowed_permission_profiles: _,
            default_permissions: _,
            allowed_web_search_modes,
            allow_managed_hooks_only,
            allow_browser_and_computer_use: _,
            allow_appshots,
            allow_remote_control,
            computer_use,
            browser_use: _,
            in_app_browser: _,
            windows,
            feature_requirements,
            hooks,
            mcp_servers,
            plugins,
            marketplaces,
            otel,
            apps: _apps,
            rules,
            enforce_residency,
            network,
            application,
            permissions,
            permissions_network,
            auto_review,
            models: _,
            additional_developer_instructions,
            guardian_policy_config,
            guardian_extra_policy,
        } = toml;

        let auto_review_required_models = auto_review
            .and_then(|auto_review| {
                auto_review
                    .value
                    .required_on_models
                    .map(|slugs| Sourced::new(slugs, auto_review.source))
            })
            .filter(|models| !models.value.is_empty())
            .map(|models| {
                let Sourced { value, source } = models;
                let mut protected_models = BTreeSet::new();
                for slug in value {
                    if slug.trim().is_empty() || slug.trim() != slug || slug.contains('/') {
                        return Err(ConstraintError::InvalidValue {
                            field_name: "auto_review.required_on_models",
                            candidate: format!("{slug:?}"),
                            allowed: "non-empty model slugs without surrounding whitespace or provider namespaces"
                                .to_string(),
                            requirement_source: source,
                        });
                    }
                    protected_models.insert(slug);
                }
                Ok(Sourced::new(protected_models, source))
            })
            .transpose()?;

        if let Some(requirements) = &mcp_servers {
            validate_mcp_server_requirements(
                &requirements.value,
                &requirements.source,
                /*plugin_name*/ None,
            )?;
        }
        if let Some(plugin_requirements) = &plugins {
            for (plugin_name, plugin) in &plugin_requirements.value {
                if let Some(requirements) = &plugin.mcp_servers {
                    validate_mcp_server_requirements(
                        requirements,
                        &plugin_requirements.source,
                        Some(plugin_name),
                    )?;
                }
            }
        }

        let approval_policy = match allowed_approval_policies {
            Some(Sourced {
                value: policies,
                source: requirement_source,
            }) => {
                let Some(initial_value) = policies.first().copied() else {
                    return Err(ConstraintError::empty_field("allowed_approval_policies"));
                };

                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(initial_value, move |candidate| {
                    if policies.contains(candidate) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "approval_policy",
                            candidate: format!("{candidate:?}"),
                            allowed: format!("{policies:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any_from_default(),
                /*source*/ None,
            ),
        };

        let approvals_reviewer = match allowed_approvals_reviewers {
            Some(Sourced {
                value: reviewers,
                source: requirement_source,
            }) => {
                let Some(initial_value) = reviewers.first().copied() else {
                    return Err(ConstraintError::empty_field("allowed_approvals_reviewers"));
                };

                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(initial_value, move |candidate| {
                    if reviewers.contains(candidate) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "approvals_reviewer",
                            candidate: format!("{candidate:?}"),
                            allowed: format!("{reviewers:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any_from_default(),
                /*source*/ None,
            ),
        };

        let default_permission_profile = PermissionProfile::read_only();
        let permission_profile = match allowed_sandbox_modes {
            Some(Sourced {
                value: modes,
                source: requirement_source,
            }) => {
                if !modes.contains(&SandboxModeRequirement::ReadOnly) {
                    return Err(ConstraintError::InvalidValue {
                        field_name: "allowed_sandbox_modes",
                        candidate: format!("{modes:?}"),
                        allowed: "must include 'read-only' to allow any PermissionProfile"
                            .to_string(),
                        requirement_source,
                    });
                };

                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(default_permission_profile, move |candidate| {
                    let mode = sandbox_mode_requirement_for_permission_profile(candidate);
                    if modes.contains(&mode) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "sandbox_mode",
                            candidate: format!("{mode:?}"),
                            allowed: format!("{modes:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any(default_permission_profile),
                /*source*/ None,
            ),
        };
        let mxc_requirement_source = windows
            .as_ref()
            .filter(|windows| windows.value.allow_mxc == Some(false))
            .map(|windows| windows.source.clone());
        let mut windows_sandbox_mode = match windows {
            Some(Sourced {
                value:
                    WindowsRequirementsToml {
                        allowed_sandbox_implementations: Some(implementations),
                        allow_mxc: _,
                        require_mxc: _,
                    },
                source: requirement_source,
            }) => {
                if implementations.is_empty() {
                    return Err(ConstraintError::empty_field(
                        "windows.allowed_sandbox_implementations",
                    ));
                }
                // Prefer elevated when both Windows sandbox implementations are allowed.
                let initial_value =
                    if implementations.contains(&WindowsSandboxImplementationToml::Elevated) {
                        WindowsSandboxModeToml::Elevated
                    } else {
                        WindowsSandboxModeToml::Unelevated
                    };

                let requirement_source_for_error = requirement_source.clone();
                let constrained =
                    Constrained::new(Some(initial_value), move |candidate| match candidate {
                        Some(WindowsSandboxModeToml::Mxc) => Ok(()),
                        Some(WindowsSandboxModeToml::Elevated)
                            if implementations
                                .contains(&WindowsSandboxImplementationToml::Elevated) =>
                        {
                            Ok(())
                        }
                        Some(WindowsSandboxModeToml::Unelevated)
                            if implementations
                                .contains(&WindowsSandboxImplementationToml::Unelevated) =>
                        {
                            Ok(())
                        }
                        _ => Err(ConstraintError::InvalidValue {
                            field_name: "windows.sandbox",
                            candidate: format!("{candidate:?}"),
                            allowed: format!("{implementations:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        }),
                    })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            Some(Sourced {
                value:
                    WindowsRequirementsToml {
                        allowed_sandbox_implementations: None,
                        allow_mxc: _,
                        require_mxc: _,
                    },
                ..
            })
            | None => ConstrainedWithSource::new(
                Constrained::allow_any(/*initial_value*/ None),
                /*source*/ None,
            ),
        };
        if let Some(requirement_source) = mxc_requirement_source {
            windows_sandbox_mode.add_validator(move |candidate| match candidate {
                Some(WindowsSandboxModeToml::Mxc) => Err(ConstraintError::InvalidValue {
                    field_name: "windows.sandbox",
                    candidate: format!("{candidate:?}"),
                    allowed: "windows.allow_mxc = false".to_string(),
                    requirement_source: requirement_source.clone(),
                }),
                Some(WindowsSandboxModeToml::Elevated | WindowsSandboxModeToml::Unelevated)
                | None => Ok(()),
            })?;
        }
        let exec_policy = match rules {
            Some(Sourced { value, source }) => {
                let policy = value.to_requirements_policy().map_err(|err| {
                    ConstraintError::ExecPolicyParse {
                        requirement_source: source.clone(),
                        reason: err.to_string(),
                    }
                })?;
                Some(Sourced::new(policy, source))
            }
            None => None,
        };
        let web_search_mode = match allowed_web_search_modes {
            Some(Sourced {
                value: modes,
                source: requirement_source,
            }) => {
                let mut accepted = modes.into_iter().collect::<std::collections::BTreeSet<_>>();
                accepted.insert(WebSearchModeRequirement::Disabled);
                let allowed_for_error = format!(
                    "{:?}",
                    accepted
                        .iter()
                        .copied()
                        .map(WebSearchMode::from)
                        .collect::<Vec<_>>()
                );

                let initial_value = if accepted.contains(&WebSearchModeRequirement::Cached) {
                    WebSearchMode::Cached
                } else if accepted.contains(&WebSearchModeRequirement::Indexed) {
                    WebSearchMode::Indexed
                } else if accepted.contains(&WebSearchModeRequirement::Live) {
                    WebSearchMode::Live
                } else {
                    WebSearchMode::Disabled
                };
                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(initial_value, move |candidate| {
                    if accepted.contains(&(*candidate).into()) {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "web_search_mode",
                            candidate: format!("{candidate:?}"),
                            allowed: allowed_for_error.clone(),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any(WebSearchMode::Cached),
                /*source*/ None,
            ),
        };
        let feature_requirements =
            feature_requirements.filter(|requirements| !requirements.value.is_empty());
        let managed_hooks = hooks
            .filter(|managed_hooks| managed_hooks.value.handler_count() > 0)
            .map(|sourced_hooks| {
                let Sourced {
                    value,
                    source: requirement_source,
                } = sourced_hooks;
                let allowed = value;
                let allowed_for_error = format!("{allowed:?}");
                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(allowed.clone(), move |candidate| {
                    if candidate == &allowed {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "hooks",
                            candidate: format!("{candidate:?}"),
                            allowed: allowed_for_error.clone(),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                Ok(ConstrainedWithSource::new(
                    constrained,
                    Some(requirement_source),
                ))
            })
            .transpose()?;

        let enforce_residency = match enforce_residency {
            Some(Sourced {
                value: residency,
                source: requirement_source,
            }) => {
                let required = Some(residency);
                let requirement_source_for_error = requirement_source.clone();
                let constrained = Constrained::new(required, move |candidate| {
                    if candidate == &required {
                        Ok(())
                    } else {
                        Err(ConstraintError::InvalidValue {
                            field_name: "enforce_residency",
                            candidate: format!("{candidate:?}"),
                            allowed: format!("{required:?}"),
                            requirement_source: requirement_source_for_error.clone(),
                        })
                    }
                })?;
                ConstrainedWithSource::new(constrained, Some(requirement_source))
            }
            None => ConstrainedWithSource::new(
                Constrained::allow_any(/*initial_value*/ None),
                /*source*/ None,
            ),
        };
        let network = permissions_network
            .map(|network| *network)
            .or(network)
            .map(|Sourced { value, source }| Sourced::new(NetworkConstraints::from(value), source));
        let filesystem = permissions
            .filter(|permissions| permissions.value.filesystem.is_some())
            .map(|Sourced { value, source }| {
                Sourced::new(FilesystemConstraints::from(value), source)
            });
        let guardian_policy_config_source = guardian_policy_config.map(|sourced| sourced.source);
        let guardian_extra_policy_source = guardian_extra_policy.map(|sourced| sourced.source);
        Ok(ConfigRequirements {
            allowed_login_methods,
            allowed_chatgpt_workspaces,
            cli_auth_credentials_store,
            chatgpt_base_url,
            sqlite_home,
            unsafe_enforce_sqlite_journal_mode,
            log_dir,
            model_catalog_json,
            model_provider,
            model_providers,
            check_for_update_on_startup,
            allow_login_shell,
            feedback,
            approval_policy,
            approvals_reviewer,
            auto_review_required_models,
            permission_profile,
            windows_sandbox_mode,
            web_search_mode,
            allow_managed_hooks_only,
            allow_appshots,
            allow_remote_control,
            computer_use,
            feature_requirements,
            managed_hooks,
            mcp_servers,
            plugins,
            marketplaces,
            otel,
            exec_policy,
            enforce_residency,
            network,
            application,
            filesystem,
            additional_developer_instructions,
            guardian_policy_config_source,
            guardian_extra_policy_source,
        })
    }
}

pub fn sandbox_mode_requirement_for_permission_profile(
    permission_profile: &PermissionProfile,
) -> SandboxModeRequirement {
    match permission_profile {
        PermissionProfile::Disabled => SandboxModeRequirement::DangerFullAccess,
        PermissionProfile::External { .. } => SandboxModeRequirement::ExternalSandbox,
        PermissionProfile::Managed { .. } => {
            let file_system_policy = permission_profile.file_system_sandbox_policy();
            if file_system_policy.has_full_disk_write_access() {
                SandboxModeRequirement::DangerFullAccess
            } else if file_system_policy
                .entries
                .iter()
                .any(|entry| entry.access.can_write())
            {
                SandboxModeRequirement::WorkspaceWrite
            } else {
                SandboxModeRequirement::ReadOnly
            }
        }
    }
}

#[cfg(test)]
#[path = "config_requirements_tests.rs"]
mod tests;
