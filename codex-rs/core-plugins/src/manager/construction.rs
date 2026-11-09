//! Construction, auth-context accessors, and shared policy helpers for `PluginsManager`.

use super::*;

impl PluginsManager {
    /// Build remote service state from the request's effective mode and this harness's runtime.
    pub fn remote_plugin_service_config(
        &self,
        config: &PluginsConfigInput,
    ) -> RemotePluginServiceConfig {
        config.remote_plugin_service_config(
            config
                .runtime_auth_mode
                .map(|mode| (Arc::clone(&self.runtime_auth), mode)),
        )
    }

    pub fn auth_manager(&self) -> Arc<AuthManager> {
        Arc::clone(&self.auth_manager)
    }

    pub fn runtime_auth_slot(&self) -> RuntimeAuthSlot {
        Arc::clone(&self.runtime_auth)
    }

    /// Reuse host configuration without sharing another actor's remote caches or credentials.
    pub fn for_auth_context(
        &self,
        auth_manager: Arc<AuthManager>,
        runtime_auth: RuntimeAuthSlot,
    ) -> Self {
        Self::new_with_options(
            self.codex_home.clone(),
            self.restriction_product,
            auth_manager,
            runtime_auth,
            Arc::clone(&self.skill_root_loader),
        )
        .with_plugin_install_source(self.plugin_install_source)
    }

    pub fn new(
        codex_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        skill_root_loader: Arc<dyn SkillRootLoader<PluginSkillRoot>>,
    ) -> Self {
        Self::new_with_options(
            codex_home,
            Some(Product::Codex),
            auth_manager,
            RuntimeAuthSlot::default(),
            skill_root_loader,
        )
    }

    pub fn new_with_options(
        codex_home: PathBuf,
        restriction_product: Option<Product>,
        auth_manager: Arc<AuthManager>,
        runtime_auth: RuntimeAuthSlot,
        skill_root_loader: Arc<dyn SkillRootLoader<PluginSkillRoot>>,
    ) -> Self {
        // Product restrictions are enforced at marketplace admission time for a given CODEX_HOME:
        // listing, install, and curated refresh all consult this restriction context before new
        // plugins enter local config or cache. After admission, runtime plugin loading trusts the
        // contents of that CODEX_HOME and does not re-filter configured plugins by product, so
        // already-admitted plugins may continue exposing MCP servers/tools from shared local state.
        //
        // This assumes a single CODEX_HOME is only used by one product.
        let remote_installed_plugin_bundle_sync_gate =
            crate::remote::remote_installed_plugin_bundle_sync_gate(&codex_home);
        let store = PluginStore::new(codex_home.clone());
        let tool_suggest_metadata_cache =
            ToolSuggestMetadataCache::new(Arc::clone(&store.manifest_cache));
        Self {
            codex_home,
            store,
            featured_plugin_ids_cache: RwLock::new(None),
            recommended_plugins_cache: RwLock::new(HashMap::new()),
            recommended_plugins_refreshes: RwLock::new(HashMap::new()),
            configured_marketplace_upgrade_state: RwLock::new(
                ConfiguredMarketplaceUpgradeState::default(),
            ),
            non_curated_cache_refresh_lock: Semaphore::new(/*permits*/ 1),
            non_curated_cache_refresh_state: RwLock::new(NonCuratedCacheRefreshState::default()),
            non_curated_cache_refresh_completion: watch::channel(
                NonCuratedCacheRefreshCompletion::default(),
            )
            .0,
            loaded_plugins_cache: Mutex::new(LoadedPluginsCache::default()),
            loaded_plugins_load_semaphore: Semaphore::new(/*permits*/ 1),
            skill_root_loader,
            tool_suggest_metadata_cache,
            remote_installed_plugins_cache: RwLock::new(RemoteInstalledPluginsCache::default()),
            remote_installed_plugin_bundle_sync_gate,
            remote_installed_plugins_cache_refresh_state: RwLock::new(
                RemoteInstalledPluginsCacheRefreshState::default(),
            ),
            remote_catalog_cache_refresh_state: RwLock::new(
                RemoteCatalogCacheRefreshState::default(),
            ),
            restriction_product,
            auth_manager,
            runtime_auth,
            analytics_events_client: RwLock::new(None),
            plugin_install_source: PluginInstallSource::Manual,
        }
    }

    pub fn with_plugin_install_source(mut self, source: PluginInstallSource) -> Self {
        self.plugin_install_source = source;
        self
    }

    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.auth_manager.get_api_auth_mode()
    }

    pub(super) fn remote_global_catalog_active(&self, config: &PluginsConfigInput) -> bool {
        config.remote_plugin_enabled && self.auth_mode().is_some_and(AuthMode::uses_codex_backend)
    }

    pub fn set_analytics_events_client(&self, analytics_events_client: AnalyticsEventsClient) {
        let mut stored_client = match self.analytics_events_client.write() {
            Ok(client_guard) => client_guard,
            Err(err) => err.into_inner(),
        };
        *stored_client = Some(analytics_events_client);
    }

    pub(super) fn restriction_product_matches(&self, products: Option<&[Product]>) -> bool {
        match products {
            None => true,
            Some([]) => false,
            Some(products) => self
                .restriction_product
                .is_some_and(|product| product.matches_product_restriction(products)),
        }
    }
}
