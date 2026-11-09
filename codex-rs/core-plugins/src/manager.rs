//! `PluginsManager` and the request, outcome, and cache types it shares with its child
//! modules. The manager's methods are grouped by responsibility in `manager/`:
//! construction, plugin loading, the remote installed-plugin cache and its background
//! refresh, telemetry, recommendations, installation, marketplace listing, and startup
//! background tasks.

#[path = "manager_errors.rs"]
mod errors;
pub use errors::PluginInstallError;
pub use errors::PluginUninstallError;
use errors::marketplace_error_sub_error_type;
use errors::marketplace_error_type;
use errors::plugin_install_error_type;

#[path = "remote_mutations.rs"]
mod remote_mutations;
pub use remote_mutations::RemotePluginInstallOutcome;
pub use remote_mutations::RemotePluginInstallRequest;
pub use remote_mutations::RemotePluginOperationError;
pub use remote_mutations::RemotePluginOperationErrorKind;
pub use remote_mutations::RemotePluginUninstallOutcome;
#[path = "marketplace_context.rs"]
mod marketplace_context;
pub use marketplace_context::PluginMarketplaceContext;
pub use marketplace_context::PluginMarketplaceScope;

mod background_tasks;
mod construction;
mod installation;
mod marketplace_listing;
mod plugin_loading;
mod recommendations;
mod remote_installed_cache;
mod remote_refresh;
mod telemetry;
pub(crate) use marketplace_listing::remote_plugin_install_required_description;

use super::LoadedPlugin;
use super::PluginLoadOutcome;
use crate::PluginGitMode;
use crate::app_mcp_routing::apply_app_mcp_routing_policy;
use crate::app_mcp_routing::plugin_supports_apps;
use crate::http_client_selector::HttpClientSelector;
use crate::installed_marketplaces::installed_marketplace_roots_from_layer_stack;
use crate::is_openai_curated_marketplace_name;
use crate::loaded_cache_metrics;
use crate::loaded_cache_metrics::RequestOutcome;
use crate::loader::PluginHookLoadOutcome;
use crate::loader::TargetCuratedMarketplace;
use crate::loader::configured_curated_plugin_ids_from_codex_home;
use crate::loader::curated_plugin_cache_version;
use crate::loader::load_plugin_apps_from_manifest;
use crate::loader::load_plugin_hooks;
use crate::loader::load_plugin_hooks_from_layer_stack;
use crate::loader::load_plugin_mcp_servers_from_manifest_with_format;
use crate::loader::load_plugin_skill_inventory;
use crate::loader::load_plugins_from_layer_stack;
use crate::loader::log_plugin_load_errors;
use crate::loader::materialize_marketplace_plugin_source;
use crate::loader::plugin_capability_summary_from_root;
use crate::loader::plugin_is_eligible_for_target_marketplace;
use crate::loader::refresh_curated_plugin_cache;
use crate::loader::refresh_non_curated_plugin_cache_detailed;
use crate::loader::refresh_non_curated_plugin_cache_force_reinstall_detailed;
use crate::loader::remote_installed_plugins_to_config;
use crate::manifest::PluginManifestFormat;
use crate::manifest::PluginManifestInterface;
use crate::marketplace::MarketplaceError;
use crate::marketplace::MarketplaceInterface;
use crate::marketplace::MarketplaceListError;
use crate::marketplace::MarketplaceListOutcome;
use crate::marketplace::MarketplacePluginAuthPolicy;
use crate::marketplace::MarketplacePluginManifestFallback;
use crate::marketplace::MarketplacePluginPolicy;
use crate::marketplace::MarketplacePluginSource;
use crate::marketplace::ResolvedMarketplacePlugin;
use crate::marketplace::find_installable_marketplace_plugin;
use crate::marketplace::find_marketplace_plugin;
use crate::marketplace::home_dir;
use crate::marketplace::list_marketplaces_with_cache;
use crate::marketplace::plugin_interface_with_marketplace_category;
use crate::marketplace_policy::MarketplacePolicy;
use crate::marketplace_policy::configured_plugins_from_stack;
use crate::marketplace_upgrade::ConfigLayerReload;
use crate::marketplace_upgrade::ConfiguredMarketplaceUpgradeError;
use crate::marketplace_upgrade::ConfiguredMarketplaceUpgradeOutcome;
use crate::marketplace_upgrade::upgrade_configured_git_marketplaces_with_mode;
use crate::remote::CODEX_PRODUCT_SKU;
use crate::remote::InstalledPluginDisplayAssets;
use crate::remote::REMOTE_GLOBAL_MARKETPLACE_NAME;
use crate::remote::RecommendedPluginsMode;
use crate::remote::RemoteInstalledPlugin;
use crate::remote::RemoteInstalledPluginBundleSyncError;
use crate::remote::RemoteInstalledPluginBundleSyncOutcome;
use crate::remote::RemotePluginCapabilities;
use crate::remote::RemotePluginCatalogError;
use crate::remote::RemotePluginChange;
use crate::remote::RemotePluginMaterialization;
use crate::remote::RemotePluginScope;
use crate::remote::RemotePluginServiceConfig;
use crate::remote_legacy::RemotePluginFetchError;
use crate::remote_plugin_id_resolver::RemoteInstalledPluginsSnapshot;
use crate::remote_plugin_id_resolver::RemotePluginIdResolver;
use crate::remote_plugin_id_resolver::persisted_remote_plugin_id_for_installation;
use crate::skill_snapshots::new_plugin_skill_snapshots;
use crate::startup_sync::OPENAI_PLUGINS_GIT_URL;
use crate::startup_sync::curated_plugins_api_marketplace_path;
use crate::startup_sync::curated_plugins_repo_path;
use crate::startup_sync::read_curated_plugins_sha;
use crate::startup_sync::sync_openai_plugins_repo;
use crate::store::PluginInstallResult as StorePluginInstallResult;
use crate::store::PluginStore;
use crate::store::PluginStoreError;
use crate::tool_suggest_metadata::ToolSuggestMetadataCache;
use codex_analytics::AnalyticsEventsClient;
use codex_analytics::PluginInstallSource;
use codex_config::ConfigLayerStack;
use codex_config::SkillConfigRules;
use codex_config::clear_user_plugin;
use codex_config::set_user_plugin_enabled;
use codex_config::skill_config_rules_from_stack;
use codex_config::types::PluginConfig;
use codex_config::types::PluginsConfigToml;
use codex_config::types::ToolSuggestDisabledTool;
use codex_config::types::ToolSuggestDiscoverableType;
use codex_connectors::ConnectorSnapshot;
use codex_connectors::PluginConnectorSource;
use codex_features::RuntimeAuthMode;
use codex_hooks::plugin_hook_declarations;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_plugin::AppConnectorId;
use codex_plugin::PluginCapabilitySummary;
use codex_plugin::PluginId;
use codex_plugin::PluginIdError;
use codex_plugin::PluginTelemetryMetadata;
use codex_plugin::app_connector_ids_from_declarations;
use codex_plugin::prompt_safe_plugin_description;
use codex_protocol::auth::AuthMode;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::Product;
use codex_runtime_agent_instance::RuntimeAuthSlot;
use codex_skills::SkillMetadata;
use codex_skills::SkillRootLoader;
use codex_skills::SkillRootSnapshots;
use codex_tools::DiscoverablePluginInfo;
use codex_tools::DiscoverableTool;
use codex_tools::filter_request_plugin_install_discoverable_tools_for_client;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use codex_utils_plugins::PluginIdentity;
use codex_utils_plugins::PluginSkillRoot;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::OnceCell;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tracing::instrument;
use tracing::warn;

static CURATED_REPO_SYNC_STARTED: AtomicBool = AtomicBool::new(false);
const FEATURED_PLUGIN_IDS_CACHE_TTL: std::time::Duration =
    std::time::Duration::from_secs(60 * 60 * 3);
const REMOTE_INSTALLED_PLUGIN_SYNC_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const LOADED_PLUGINS_CACHE_CAPACITY: usize = 8;

type EffectivePluginsChangedCallback = Arc<dyn Fn(EffectivePluginsChange) + Send + Sync + 'static>;

#[derive(Debug, Clone)]
pub struct PluginsConfigInput {
    pub config_layer_stack: ConfigLayerStack,
    pub plugins: PluginsConfigToml,
    pub model_provider_id: String,
    pub plugins_enabled: bool,
    pub remote_plugin_enabled: bool,
    pub chatgpt_base_url: String,
    pub product_sku: Option<String>,
    http_client_factory: HttpClientFactory,
    remote_http_clients: Arc<OnceLock<Arc<dyn HttpClientSelector>>>,
    runtime_http_clients: Arc<OnceLock<Arc<dyn HttpClientSelector>>>,
    runtime_auth_mode: Option<RuntimeAuthMode>,
}

impl PluginsConfigInput {
    // Require validated policy at construction; a default followed by a setter could fail open.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config_layer_stack: ConfigLayerStack,
        plugins: PluginsConfigToml,
        model_provider_id: String,
        plugins_enabled: bool,
        remote_plugin_enabled: bool,
        chatgpt_base_url: String,
        http_client_factory: HttpClientFactory,
        product_sku: Option<String>,
    ) -> Self {
        Self {
            config_layer_stack,
            plugins,
            model_provider_id,
            plugins_enabled,
            remote_plugin_enabled,
            chatgpt_base_url,
            http_client_factory,
            product_sku,
            remote_http_clients: Arc::new(OnceLock::new()),
            runtime_http_clients: Arc::new(OnceLock::new()),
            runtime_auth_mode: None,
        }
    }

    pub fn with_runtime_auth_mode(mut self, mode: Option<RuntimeAuthMode>) -> Self {
        self.runtime_auth_mode = mode;
        self
    }

    /// Shares lazy route-aware connection pools across this input and its clones.
    /// Runtime mode strips sensitive headers on cross-origin redirects.
    pub fn remote_plugin_service_config(
        &self,
        runtime_auth: Option<(RuntimeAuthSlot, RuntimeAuthMode)>,
    ) -> RemotePluginServiceConfig {
        let runtime_auth = runtime_auth.filter(|_| {
            codex_http_client::classify_openai_api_url(&self.chatgpt_base_url)
                != codex_http_client::OpenAiDestination::Other
        });
        let (http_clients, factory) = if runtime_auth.is_some() {
            (
                &self.runtime_http_clients,
                self.http_client_factory
                    .clone()
                    .with_sensitive_header_protection(),
            )
        } else {
            (&self.remote_http_clients, self.http_client_factory.clone())
        };
        let http_clients = http_clients.get_or_init(|| {
            Arc::new(
                RouteAwareClientPool::with_chatgpt_cloudflare_cookies_without_request_logging(
                    factory,
                    ClientRouteClass::Api,
                ),
            )
        });
        RemotePluginServiceConfig {
            chatgpt_base_url: self.chatgpt_base_url.clone(),
            product_sku: self
                .product_sku
                .clone()
                .unwrap_or_else(|| CODEX_PRODUCT_SKU.to_string()),
            http_clients: Arc::clone(http_clients),
            runtime_auth,
        }
    }
}

/// Effective-plugin changes that downstream composition layers may act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectivePluginsChange {
    /// Remote bundles installed or updated by background installed-plugin sync.
    pub materialized_remote_plugins: Vec<RemotePluginMaterialization>,
}

/// Inputs used to select endpoint-backed plugin install candidates.
pub struct RecommendedPluginCandidatesInput<'a> {
    pub plugins_config: &'a PluginsConfigInput,
    pub loaded_plugins: &'a PluginLoadOutcome,
    pub auth: Option<&'a CodexAuth>,
    pub disabled_tools: &'a [ToolSuggestDisabledTool],
    pub app_server_client_name: Option<&'a str>,
}

#[derive(Clone, PartialEq, Eq)]
struct FeaturedPluginIdsCacheKey {
    chatgpt_base_url: String,
    account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    is_workspace_account: bool,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct RecommendedPluginsCacheKey {
    chatgpt_base_url: String,
}

#[derive(Clone)]
struct CachedFeaturedPluginIds {
    key: FeaturedPluginIdsCacheKey,
    expires_at: Instant,
    featured_plugin_ids: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RemoteInstalledPluginsAuthIdentity {
    auth_mode: Option<AuthMode>,
    account_id: Option<String>,
    chatgpt_user_id: Option<String>,
    is_workspace_account: Option<bool>,
}

impl RemoteInstalledPluginsAuthIdentity {
    pub(crate) fn from_auth(auth: Option<&CodexAuth>) -> Self {
        Self {
            auth_mode: auth.map(CodexAuth::api_auth_mode),
            account_id: auth.and_then(CodexAuth::get_account_id),
            chatgpt_user_id: auth.and_then(CodexAuth::get_chatgpt_user_id),
            is_workspace_account: auth.map(CodexAuth::is_workspace_account),
        }
    }
}

#[derive(Default)]
struct RemoteInstalledPluginsCache {
    generation: u64,
    auth_identity: Option<RemoteInstalledPluginsAuthIdentity>,
    reconciliation_generation: Option<u64>,
    // A cancelled pass may mutate bundles before publication; retry must notify consumers.
    needs_effective_plugins_refresh: bool,
    plugins: Option<Vec<RemoteInstalledPlugin>>,
    display_assets: InstalledPluginDisplayAssets,
}

enum RemoteInstalledPluginsCachePublication {
    Refresh(InstalledPluginDisplayAssets),
    Reconcile,
}

/// Holds the cache-root gate shared by full installed-bundle sync, reconciliation, and direct
/// remote plugin mutations.
///
/// Callers should keep this guard only while mutating the remote plugin cache and backend
/// installed state, then release it before unrelated analytics, OAuth, or runtime refresh work.
struct RemoteInstalledPluginSyncGuard {
    _permit: OwnedSemaphorePermit,
}

struct RemoteInstalledPluginsReconciliationGuard<'a> {
    manager: &'a PluginsManager,
    generation: u64,
    committed: bool,
}

impl Drop for RemoteInstalledPluginsReconciliationGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.manager
                .abandon_remote_installed_plugins_reconcile(self.generation);
        }
    }
}

struct RemoteInstalledPluginsCacheRefreshRequest {
    generation: u64,
    service_config: RemotePluginServiceConfig,
    auth: Option<CodexAuth>,
    notify: RemoteInstalledPluginsCacheRefreshNotify,
    // App-server attaches side effects such as skills metadata invalidation and MCP refreshes when
    // remote installed state changes.
    on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    change: EffectivePluginsChange,
}

#[derive(Clone, Copy)]
enum RemoteInstalledPluginsCacheRefreshNotify {
    IfCacheChanged,
    // Remote mutations may change local bundles or active MCP state even when the installed set is
    // unchanged. Notify after `/installed` succeeds so MCP refreshes are ordered after the remote
    // installed cache.
    AfterSuccessfulRefresh,
}

#[derive(Default)]
struct RemoteInstalledPluginsCacheRefreshState {
    requested: Option<RemoteInstalledPluginsCacheRefreshRequest>,
    in_flight: bool,
}

struct RemoteCatalogCacheRefreshRequest {
    service_config: RemotePluginServiceConfig,
    auth: Option<CodexAuth>,
    scopes: BTreeSet<RemotePluginScope>,
}

impl RemoteCatalogCacheRefreshRequest {
    fn has_same_cache_identity(&self, other: &Self) -> bool {
        self.service_config == other.service_config
            && self.auth.as_ref().and_then(CodexAuth::get_account_id)
                == other.auth.as_ref().and_then(CodexAuth::get_account_id)
            && self.auth.as_ref().and_then(CodexAuth::get_chatgpt_user_id)
                == other.auth.as_ref().and_then(CodexAuth::get_chatgpt_user_id)
            && self.auth.as_ref().map(CodexAuth::is_workspace_account)
                == other.auth.as_ref().map(CodexAuth::is_workspace_account)
    }
}

#[derive(Default)]
struct RemoteCatalogCacheRefreshState {
    requests: VecDeque<RemoteCatalogCacheRefreshRequest>,
    in_flight: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginListBackgroundTaskOptions {
    pub local_marketplaces: Vec<ConfiguredMarketplace>,
    pub remote_catalog_cache_refresh_scopes: BTreeSet<RemotePluginScope>,
}

#[derive(Clone, PartialEq, Eq)]
struct NonCuratedCacheRefreshRequest {
    roots: Vec<AbsolutePathBuf>,
    configured_plugin_keys: Vec<String>,
    configured_plugin_sources: Vec<NonCuratedPluginSource>,
    mode: NonCuratedCacheRefreshMode,
    git_mode: PluginGitMode,
}

#[derive(Clone, PartialEq, Eq)]
struct NonCuratedPluginSource {
    marketplace_path: AbsolutePathBuf,
    plugin_key: String,
    source: MarketplacePluginSource,
    local_version: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NonCuratedCacheRefreshMode {
    IfVersionChanged,
    ForceReinstall,
}

#[derive(Default)]
struct NonCuratedCacheRefreshState {
    requested: Option<NonCuratedCacheRefreshRequest>,
    last_refreshed: Option<NonCuratedCacheRefreshRequest>,
    in_flight: bool,
}

#[derive(Clone, Copy, Default)]
struct NonCuratedCacheRefreshCompletion {
    sequence: u64,
    changed_sequence: u64,
}

#[derive(Default)]
struct ConfiguredMarketplaceUpgradeState {
    in_flight: bool,
}

fn featured_plugin_ids_cache_key(
    config: &PluginsConfigInput,
    auth: Option<&CodexAuth>,
) -> FeaturedPluginIdsCacheKey {
    FeaturedPluginIdsCacheKey {
        chatgpt_base_url: config.chatgpt_base_url.clone(),
        account_id: auth.and_then(CodexAuth::get_account_id),
        chatgpt_user_id: auth.and_then(CodexAuth::get_chatgpt_user_id),
        is_workspace_account: auth.is_some_and(CodexAuth::is_workspace_account),
    }
}

fn recommended_plugins_cache_key(config: &PluginsConfigInput) -> RecommendedPluginsCacheKey {
    RecommendedPluginsCacheKey {
        chatgpt_base_url: config.chatgpt_base_url.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstallRequest {
    pub plugin_name: String,
    pub marketplace_path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReadRequest {
    pub plugin_name: String,
    pub marketplace_path: AbsolutePathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstallOutcome {
    pub plugin_id: PluginId,
    pub plugin_version: String,
    pub installed_path: AbsolutePathBuf,
    pub auth_policy: MarketplacePluginAuthPolicy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginReadOutcome {
    pub marketplace_name: String,
    pub marketplace_path: Option<AbsolutePathBuf>,
    pub plugin: PluginDetail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginDetail {
    pub id: String,
    pub name: String,
    pub local_version: Option<String>,
    pub description: Option<String>,
    pub source: MarketplacePluginSource,
    pub policy: MarketplacePluginPolicy,
    pub interface: Option<PluginManifestInterface>,
    pub keywords: Vec<String>,
    pub installed: bool,
    pub enabled: bool,
    pub skills: Vec<SkillMetadata>,
    pub disabled_skill_paths: HashSet<PathUri>,
    /// Packaged onboarding path; callers apply visibility and enablement.
    pub onboarding_skill: Option<AbsolutePathBuf>,
    pub hooks: Vec<PluginHookSummary>,
    pub apps: Vec<AppConnectorId>,
    pub app_category_by_id: HashMap<String, String>,
    pub mcp_server_names: Vec<String>,
    pub details_unavailable_reason: Option<PluginDetailsUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginHookSummary {
    pub key: String,
    pub event_name: HookEventName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDetailsUnavailableReason {
    InstallRequiredForRemoteSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredMarketplace {
    pub name: String,
    pub path: AbsolutePathBuf,
    pub interface: Option<MarketplaceInterface>,
    pub plugins: Vec<ConfiguredMarketplacePlugin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredMarketplacePlugin {
    pub id: String,
    pub name: String,
    pub local_version: Option<String>,
    pub installed_version: Option<String>,
    pub source: MarketplacePluginSource,
    pub policy: MarketplacePluginPolicy,
    pub interface: Option<PluginManifestInterface>,
    pub keywords: Vec<String>,
    pub manifest_fallback: Option<MarketplacePluginManifestFallback>,
    pub installed: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfiguredMarketplaceListOutcome {
    pub marketplaces: Vec<ConfiguredMarketplace>,
    pub errors: Vec<MarketplaceListError>,
}

#[derive(Default)]
struct ConfiguredPluginStates {
    installed: HashSet<String>,
    enabled: HashSet<String>,
}

impl From<PluginDetail> for PluginCapabilitySummary {
    fn from(value: PluginDetail) -> Self {
        let has_skills = value.skills.iter().any(|skill| {
            !value
                .disabled_skill_paths
                .contains(&skill.path_to_skills_md)
        });
        Self {
            config_name: value.id,
            display_name: value.name,
            plugin_namespace: None,
            description: prompt_safe_plugin_description(value.description.as_deref()),
            has_skills,
            mcp_server_names: value.mcp_server_names,
            app_connector_ids: value.apps,
        }
    }
}

pub struct PluginsManager {
    codex_home: PathBuf,
    store: PluginStore,
    featured_plugin_ids_cache: RwLock<Option<CachedFeaturedPluginIds>>,
    recommended_plugins_cache: RwLock<HashMap<RecommendedPluginsCacheKey, RecommendedPluginsMode>>,
    recommended_plugins_refreshes:
        RwLock<HashMap<RecommendedPluginsCacheKey, Arc<OnceCell<RecommendedPluginsMode>>>>,
    configured_marketplace_upgrade_state: RwLock<ConfiguredMarketplaceUpgradeState>,
    non_curated_cache_refresh_lock: Semaphore,
    non_curated_cache_refresh_state: RwLock<NonCuratedCacheRefreshState>,
    non_curated_cache_refresh_completion: watch::Sender<NonCuratedCacheRefreshCompletion>,
    // Loaded capabilities vary by effective configuration and, for remote plugins, account.
    loaded_plugins_cache: Mutex<LoadedPluginsCache>,
    loaded_plugins_load_semaphore: Semaphore,
    skill_root_loader: Arc<dyn SkillRootLoader<PluginSkillRoot>>,
    tool_suggest_metadata_cache: ToolSuggestMetadataCache,
    remote_installed_plugins_cache: RwLock<RemoteInstalledPluginsCache>,
    remote_installed_plugin_bundle_sync_gate: Arc<Semaphore>,
    remote_installed_plugins_cache_refresh_state: RwLock<RemoteInstalledPluginsCacheRefreshState>,
    remote_catalog_cache_refresh_state: RwLock<RemoteCatalogCacheRefreshState>,
    restriction_product: Option<Product>,
    auth_manager: Arc<AuthManager>,
    runtime_auth: RuntimeAuthSlot,
    analytics_events_client: RwLock<Option<AnalyticsEventsClient>>,
    plugin_install_source: PluginInstallSource,
}

#[derive(Clone)]
struct LoadedPluginsCacheEntry {
    key: PluginLoadCacheKey,
    plugins: Vec<LoadedPlugin>,
    plugin_skill_snapshots: SkillRootSnapshots<PluginSkillRoot>,
}

#[derive(Default)]
struct LoadedPluginsCache {
    generation: u64,
    // Most recently used first.
    entries: VecDeque<Arc<LoadedPluginsCacheEntry>>,
}

impl LoadedPluginsCache {
    fn get(
        cache: &Mutex<Self>,
        key: &PluginLoadCacheKey,
        store: &PluginStore,
    ) -> Option<Arc<LoadedPluginsCacheEntry>> {
        let (generation, entry) = {
            let cache = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = cache.entries.iter().find(|entry| &entry.key == key)?;
            (cache.generation, Arc::clone(entry))
        };
        // Another process can replace installed versions without invalidating this manager.
        // Keep the parsed skills paired with the installation whose paths they advertise.
        // Validate outside the cache lock so filesystem work does not block other cache keys.
        let stale = entry.plugins.iter().any(|plugin| {
            let Ok(plugin_id) = PluginId::parse(&plugin.config_name) else {
                return false;
            };
            let installed_root = store
                .active_plugin_root(&plugin_id)
                .unwrap_or_else(|| store.plugin_base_root(&plugin_id));
            installed_root != plugin.root
        });
        let mut cache = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.generation != generation {
            return None;
        }
        // A load can replace this key without changing the cache generation.
        let index = cache
            .entries
            .iter()
            .position(|current| Arc::ptr_eq(current, &entry))?;
        let current = cache.entries.remove(index)?;
        if stale {
            return None;
        }
        cache.entries.push_front(current);
        Some(entry)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PluginLoadCacheKey {
    configured_plugins: HashMap<String, PluginConfig>,
    plugin_policy: PluginsConfigToml,
    skill_config_rules: SkillConfigRules,
    remote_global_catalog_active: bool,
    auth_identity: Option<RemoteInstalledPluginsAuthIdentity>,
}

impl PluginLoadCacheKey {
    fn from_config(
        config: &PluginsConfigInput,
        codex_home: &Path,
        remote_global_catalog_active: bool,
        auth_identity: RemoteInstalledPluginsAuthIdentity,
    ) -> Self {
        let mut configured_plugins =
            configured_plugins_from_stack(&config.config_layer_stack, codex_home);
        let mut plugin_policy = config.plugins.clone();
        // Apps policy is resolved separately and does not affect loaded plugin contents.
        for plugin in configured_plugins
            .values_mut()
            .chain(plugin_policy.plugins.values_mut())
        {
            plugin.apps.clear();
        }
        Self {
            configured_plugins,
            plugin_policy,
            skill_config_rules: skill_config_rules_from_stack(&config.config_layer_stack),
            remote_global_catalog_active,
            // Local curated loads are auth-independent; only remote snapshots vary by account.
            auth_identity: remote_global_catalog_active.then_some(auth_identity),
        }
    }
}

fn target_curated_marketplace(auth_mode: Option<AuthMode>) -> TargetCuratedMarketplace {
    if auth_mode.is_some_and(AuthMode::uses_codex_backend) {
        TargetCuratedMarketplace::OpenAiWithRemote
    } else {
        TargetCuratedMarketplace::OpenAiApi
    }
}

impl PluginsManager {
    fn cached_loaded_plugins(&self, key: &PluginLoadCacheKey) -> Option<Vec<LoadedPlugin>> {
        LoadedPluginsCache::get(&self.loaded_plugins_cache, key, &self.store)
            .map(|cached| cached.plugins.clone())
    }

    fn begin_remote_installed_plugins_reconcile(&self, auth: Option<&CodexAuth>) -> Option<u64> {
        let generation = self.prepare_remote_installed_plugins_cache_generation(auth)?;
        let auth_identity = RemoteInstalledPluginsAuthIdentity::from_auth(auth);
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.generation != generation || cache.auth_identity.as_ref() != Some(&auth_identity) {
            return None;
        }
        cache.generation = cache.generation.wrapping_add(1);
        cache.reconciliation_generation = Some(cache.generation);
        Some(cache.generation)
    }

    #[cfg(test)]
    fn write_remote_installed_plugins_cache(&self, plugins: Vec<RemoteInstalledPlugin>) -> bool {
        let auth = self.auth_manager.auth_cached();
        let generation = self
            .prepare_remote_installed_plugins_cache_generation(auth.as_ref())
            .expect("test cache identity should remain current");
        self.write_remote_installed_plugins_cache_snapshot(
            generation,
            plugins,
            auth.as_ref(),
            RemoteInstalledPluginsCachePublication::Refresh(InstalledPluginDisplayAssets::Include),
        )
        .expect("test cache generation should remain current")
    }

    fn configured_plugin_states(&self, config: &PluginsConfigInput) -> ConfiguredPluginStates {
        let configured_plugins =
            configured_plugins_from_stack(&config.config_layer_stack, self.codex_home.as_path());
        let installed = configured_plugins
            .keys()
            .filter(|plugin_key| {
                PluginId::parse(plugin_key)
                    .ok()
                    .is_some_and(|plugin_id| self.store.is_installed(&plugin_id))
            })
            .cloned()
            .collect::<HashSet<_>>();
        let enabled = configured_plugins
            .into_iter()
            .filter_map(|(plugin_key, plugin)| {
                (plugin.enabled.unwrap_or(true) && config.plugins.allows_plugin(&plugin_key))
                    .then_some(plugin_key)
            })
            .collect::<HashSet<_>>();
        ConfiguredPluginStates { installed, enabled }
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plugins_config_input_tests.rs"]
mod plugins_config_input_tests;
