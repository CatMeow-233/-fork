//! Featured plugin ids and endpoint-backed plugin recommendations, with their caches.

use super::*;

impl PluginsManager {
    pub fn clear_recommended_plugins_cache(&self) {
        let mut refreshes = match self.recommended_plugins_refreshes.write() {
            Ok(refreshes) => refreshes,
            Err(err) => err.into_inner(),
        };
        refreshes.clear();
        let mut cache = match self.recommended_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        cache.clear();
    }

    fn cached_featured_plugin_ids(
        &self,
        cache_key: &FeaturedPluginIdsCacheKey,
    ) -> Option<Vec<String>> {
        {
            let cache = match self.featured_plugin_ids_cache.read() {
                Ok(cache) => cache,
                Err(err) => err.into_inner(),
            };
            let now = Instant::now();
            if let Some(cached) = cache.as_ref()
                && now < cached.expires_at
                && cached.key == *cache_key
            {
                return Some(cached.featured_plugin_ids.clone());
            }
        }

        let mut cache = match self.featured_plugin_ids_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        let now = Instant::now();
        if cache
            .as_ref()
            .is_some_and(|cached| now >= cached.expires_at || cached.key != *cache_key)
        {
            *cache = None;
        }
        None
    }

    fn write_featured_plugin_ids_cache(
        &self,
        cache_key: FeaturedPluginIdsCacheKey,
        featured_plugin_ids: &[String],
    ) {
        let mut cache = match self.featured_plugin_ids_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        *cache = Some(CachedFeaturedPluginIds {
            key: cache_key,
            expires_at: Instant::now() + FEATURED_PLUGIN_IDS_CACHE_TTL,
            featured_plugin_ids: featured_plugin_ids.to_vec(),
        });
    }

    pub async fn featured_plugin_ids_for_config(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
    ) -> Result<Vec<String>, RemotePluginFetchError> {
        if !config.plugins_enabled {
            return Ok(Vec::new());
        }

        let cache_key = featured_plugin_ids_cache_key(config, auth);
        if let Some(featured_plugin_ids) = self.cached_featured_plugin_ids(&cache_key) {
            return Ok(featured_plugin_ids);
        }
        let featured_plugin_ids = crate::remote_legacy::fetch_remote_featured_plugin_ids(
            &self.remote_plugin_service_config(config),
            auth,
            self.restriction_product,
        )
        .await?;
        self.write_featured_plugin_ids_cache(cache_key, &featured_plugin_ids);
        Ok(featured_plugin_ids)
    }

    #[instrument(
        level = "trace",
        skip_all,
        fields(
            plugins_enabled = config.plugins_enabled,
            remote_plugin_enabled = config.remote_plugin_enabled
        )
    )]
    pub async fn recommended_plugins_mode_for_config(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
    ) -> RecommendedPluginsMode {
        if !config.plugins_enabled
            || !config.remote_plugin_enabled
            || !auth.is_some_and(CodexAuth::uses_codex_backend)
        {
            return RecommendedPluginsMode::Legacy;
        }

        let cache_key = recommended_plugins_cache_key(config);
        if let Some(cached) = self.cached_recommended_plugins_mode(&cache_key) {
            return cached;
        }

        let refresh = {
            let mut refreshes = match self.recommended_plugins_refreshes.write() {
                Ok(refreshes) => refreshes,
                Err(err) => err.into_inner(),
            };
            if let Some(cached) = self.cached_recommended_plugins_mode(&cache_key) {
                return cached;
            }
            refreshes
                .entry(cache_key.clone())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let mode = refresh
            .get_or_init(|| async {
                match crate::remote::fetch_recommended_plugins(
                    &self.remote_plugin_service_config(config),
                    auth,
                )
                .await
                {
                    Ok(mode) => {
                        let refreshes = match self.recommended_plugins_refreshes.read() {
                            Ok(refreshes) => refreshes,
                            Err(err) => err.into_inner(),
                        };
                        // Hold the refresh lock through publication, matching cache-clear lock
                        // order. An invalidated initializer must not repopulate the cache.
                        if refreshes
                            .get(&cache_key)
                            .is_some_and(|current| Arc::ptr_eq(current, &refresh))
                        {
                            let mut cache = match self.recommended_plugins_cache.write() {
                                Ok(cache) => cache,
                                Err(err) => err.into_inner(),
                            };
                            cache.insert(cache_key.clone(), mode.clone());
                        }
                        mode
                    }
                    Err(err) => {
                        warn!(error = %err, "failed to load recommended plugins");
                        RecommendedPluginsMode::Legacy
                    }
                }
            })
            .await
            .clone();

        let mut refreshes = match self.recommended_plugins_refreshes.write() {
            Ok(refreshes) => refreshes,
            Err(err) => err.into_inner(),
        };
        if refreshes
            .get(&cache_key)
            .is_some_and(|current| Arc::ptr_eq(current, &refresh))
        {
            refreshes.remove(&cache_key);
        }

        mode
    }

    /// Returns endpoint recommendations eligible for installation in the current client.
    /// `None` selects the legacy discovery workflow.
    #[instrument(level = "trace", skip_all)]
    pub async fn recommended_plugin_candidates_for_config(
        &self,
        input: RecommendedPluginCandidatesInput<'_>,
    ) -> Option<Vec<DiscoverableTool>> {
        let RecommendedPluginsMode::Endpoint { plugins } = self
            .recommended_plugins_mode_for_config(input.plugins_config, input.auth)
            .await
        else {
            return None;
        };
        if plugins.is_empty() {
            return Some(Vec::new());
        }

        let installed_plugin_ids = input
            .loaded_plugins
            .plugins()
            .iter()
            .map(|plugin| plugin.config_name.as_str())
            .collect::<HashSet<_>>();
        let installed_remote_plugin_ids = {
            let cache = match self.remote_installed_plugins_cache.read() {
                Ok(cache) => cache,
                Err(err) => err.into_inner(),
            };
            let plugins = self
                .remote_installed_plugins_cache_matches_current_auth(&cache)
                .then_some(cache.plugins.as_deref())
                .flatten()
                .unwrap_or_default();
            plugins
                .iter()
                .filter(|plugin| plugin.marketplace_name == REMOTE_GLOBAL_MARKETPLACE_NAME)
                .map(|plugin| plugin.id.clone())
                .collect::<HashSet<_>>()
        };
        let disabled_plugin_ids = input
            .disabled_tools
            .iter()
            .filter(|tool| tool.kind == ToolSuggestDiscoverableType::Plugin)
            .map(|tool| tool.id.as_str())
            .collect::<HashSet<_>>();

        let candidates = plugins
            .into_iter()
            .filter(|plugin| {
                !installed_plugin_ids.contains(plugin.config_id.as_str())
                    && !installed_remote_plugin_ids.contains(plugin.remote_plugin_id.as_str())
                    && !disabled_plugin_ids.contains(plugin.config_id.as_str())
            })
            .map(|plugin| {
                DiscoverableTool::from(DiscoverablePluginInfo {
                    id: plugin.config_id,
                    remote_plugin_id: Some(plugin.remote_plugin_id),
                    name: plugin.display_name,
                    description: None,
                    has_skills: false,
                    mcp_server_names: Vec::new(),
                    app_connector_ids: Vec::new(),
                })
            })
            .collect();
        Some(filter_request_plugin_install_discoverable_tools_for_client(
            candidates,
            input.app_server_client_name,
        ))
    }

    pub(super) fn cached_recommended_plugins_mode(
        &self,
        cache_key: &RecommendedPluginsCacheKey,
    ) -> Option<RecommendedPluginsMode> {
        let cache = match self.recommended_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        cache.get(cache_key).cloned()
    }
}
