//! The account-scoped remote installed-plugins cache: lookups, generation fencing, publication.

use super::*;

impl PluginsManager {
    /// Applies plugin exclusions and canonical ownership from the current account's installed cache.
    /// A canonical owner's bundle need not be installed on this host.
    pub fn connector_snapshot(
        &self,
        sources: impl IntoIterator<Item = PluginConnectorSource>,
        disabled_plugin_ids: &[String],
    ) -> ConnectorSnapshot {
        let mut canonical_app_ids = HashSet::new();
        if !disabled_plugin_ids.is_empty() {
            let cache = match self.remote_installed_plugins_cache.read() {
                Ok(cache) => cache,
                Err(err) => err.into_inner(),
            };
            if self.remote_installed_plugins_cache_matches_current_auth(&cache) {
                canonical_app_ids = cache
                    .plugins
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|plugin| {
                        let app_id = plugin.canonical_app_id.as_ref()?;
                        let plugin_id =
                            PluginId::new(plugin.name.clone(), plugin.marketplace_name.clone())
                                .ok()?;
                        disabled_plugin_ids
                            .contains(&plugin_id.as_key())
                            .then(|| app_id.clone())
                    })
                    .collect();
            }
        }
        ConnectorSnapshot::from_plugin_sources(sources, disabled_plugin_ids, canonical_app_ids)
    }

    pub(super) fn remote_installed_plugin_configs(&self) -> HashMap<String, PluginConfig> {
        let cache = match self.remote_installed_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if !self.remote_installed_plugins_cache_matches_current_auth(&cache) {
            return HashMap::new();
        }
        let Some(plugins) = cache.plugins.as_ref() else {
            return HashMap::new();
        };

        remote_installed_plugins_to_config(plugins, &self.store)
    }

    pub(super) fn remote_installed_plugins_snapshot(&self) -> RemoteInstalledPluginsSnapshot {
        let cache = match self.remote_installed_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if !self.remote_installed_plugins_cache_matches_current_auth(&cache) {
            return RemoteInstalledPluginsSnapshot::default();
        }
        let Some(plugins) = cache.plugins.as_ref() else {
            return RemoteInstalledPluginsSnapshot::default();
        };

        RemoteInstalledPluginsSnapshot {
            configs: remote_installed_plugins_to_config(plugins, &self.store),
            remote_plugin_id_resolver: RemotePluginIdResolver::new(plugins),
        }
    }

    pub(super) fn remote_plugin_id_for(&self, plugin_id: &PluginId) -> Option<String> {
        let cache = match self.remote_installed_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if self.remote_installed_plugins_cache_matches_current_auth(&cache)
            && let Some(plugins) = cache.plugins.as_ref()
        {
            return plugins.iter().find_map(|plugin| {
                (plugin.name == plugin_id.plugin_name
                    && plugin.marketplace_name == plugin_id.marketplace_name)
                    .then(|| plugin.id.clone())
            });
        }
        drop(cache);

        let installation = self.store.active_plugin_installation(plugin_id)?;
        persisted_remote_plugin_id_for_installation(&installation)
    }

    pub(super) fn remote_installed_plugins_cache_matches_current_auth(
        &self,
        cache: &RemoteInstalledPluginsCache,
    ) -> bool {
        let auth = self.auth_manager.auth_cached();
        cache.auth_identity.as_ref()
            == Some(&RemoteInstalledPluginsAuthIdentity::from_auth(
                auth.as_ref(),
            ))
    }

    pub(super) fn remote_installed_plugins_auth_is_current(
        &self,
        auth_identity: &RemoteInstalledPluginsAuthIdentity,
    ) -> bool {
        let auth = self.auth_manager.auth_cached();
        RemoteInstalledPluginsAuthIdentity::from_auth(auth.as_ref()) == *auth_identity
    }

    pub fn build_remote_installed_plugin_marketplaces_from_cache(
        &self,
        visible_marketplaces: &[&str],
    ) -> Option<Vec<crate::remote::RemoteMarketplace>> {
        self.build_remote_installed_plugin_marketplaces_from_cache_with_display_assets(
            visible_marketplaces,
            InstalledPluginDisplayAssets::Include,
        )
    }

    /// A runtime snapshot cannot satisfy a caller that requires display enrichment.
    pub fn build_remote_installed_plugin_marketplaces_from_cache_with_display_assets(
        &self,
        visible_marketplaces: &[&str],
        display_assets: InstalledPluginDisplayAssets,
    ) -> Option<Vec<crate::remote::RemoteMarketplace>> {
        let auth = self.auth_manager.auth_cached();
        self.build_remote_installed_plugin_marketplaces_from_cache_for_auth(
            auth.as_ref(),
            visible_marketplaces,
            display_assets,
        )
    }

    fn build_remote_installed_plugin_marketplaces_from_cache_for_auth(
        &self,
        auth: Option<&CodexAuth>,
        visible_marketplaces: &[&str],
        display_assets: InstalledPluginDisplayAssets,
    ) -> Option<Vec<crate::remote::RemoteMarketplace>> {
        let cache = match self.remote_installed_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.auth_identity.as_ref()
            != Some(&RemoteInstalledPluginsAuthIdentity::from_auth(auth))
        {
            return None;
        }
        if display_assets == InstalledPluginDisplayAssets::Include
            && cache.display_assets == InstalledPluginDisplayAssets::Skip
        {
            return None;
        }
        let plugins = cache.plugins.as_ref()?;
        Some(
            crate::remote::group_remote_installed_plugins_by_marketplaces(
                plugins,
                visible_marketplaces,
            ),
        )
    }

    pub fn cached_global_remote_discoverable_plugins_for_config(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
    ) -> Vec<crate::remote::RemoteDiscoverablePlugin> {
        if !config.plugins_enabled || !config.remote_plugin_enabled {
            return Vec::new();
        }
        let Some(auth) = auth.filter(|auth| auth.uses_codex_backend()) else {
            return Vec::new();
        };
        let Some(account_id) = auth.get_account_id() else {
            return Vec::new();
        };
        if account_id.is_empty() {
            return Vec::new();
        }

        crate::remote::cached_global_remote_discoverable_plugins(
            self.codex_home.as_path(),
            &self.remote_plugin_service_config(config),
            auth,
        )
    }

    pub async fn build_and_cache_remote_installed_plugin_marketplaces(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
        visible_marketplaces: &[&str],
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) -> Result<Vec<crate::remote::RemoteMarketplace>, RemotePluginCatalogError> {
        self.build_and_cache_remote_installed_plugin_marketplaces_with_display_assets(
            config,
            auth,
            visible_marketplaces,
            on_effective_plugins_changed,
            InstalledPluginDisplayAssets::Include,
        )
        .await
    }

    /// Fetch installed runtime metadata without optional display enrichment.
    pub async fn build_and_cache_remote_installed_plugin_marketplaces_with_display_assets(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
        visible_marketplaces: &[&str],
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
        display_assets: InstalledPluginDisplayAssets,
    ) -> Result<Vec<crate::remote::RemoteMarketplace>, RemotePluginCatalogError> {
        let generation = self
            .prepare_remote_installed_plugins_cache_generation(auth)
            .ok_or(RemotePluginCatalogError::AuthRequired)?;
        let plugins = crate::remote::fetch_remote_installed_plugins_with_display_assets(
            &self.remote_plugin_service_config(config),
            auth,
            display_assets,
        )
        .await?;
        let marketplaces = crate::remote::group_remote_installed_plugins_by_marketplaces(
            &plugins,
            visible_marketplaces,
        );
        let Some(changed) = self.write_remote_installed_plugins_cache_snapshot(
            generation,
            plugins,
            auth,
            RemoteInstalledPluginsCachePublication::Refresh(display_assets),
        ) else {
            let auth_identity = RemoteInstalledPluginsAuthIdentity::from_auth(auth);
            if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
                return Err(RemotePluginCatalogError::AuthRequired);
            }
            return Ok(self
                .build_remote_installed_plugin_marketplaces_from_cache_for_auth(
                    auth,
                    visible_marketplaces,
                    display_assets,
                )
                .unwrap_or(marketplaces));
        };
        if changed && let Some(on_effective_plugins_changed) = on_effective_plugins_changed {
            on_effective_plugins_changed(EffectivePluginsChange::default());
        }
        Ok(self
            .build_remote_installed_plugin_marketplaces_from_cache_for_auth(
                auth,
                visible_marketplaces,
                display_assets,
            )
            .unwrap_or(marketplaces))
    }

    pub(super) fn prepare_remote_installed_plugins_cache_generation(
        &self,
        auth: Option<&CodexAuth>,
    ) -> Option<u64> {
        let auth_identity = RemoteInstalledPluginsAuthIdentity::from_auth(auth);
        if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
            return None;
        }
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        let cache_changed = if cache.auth_identity.as_ref() != Some(&auth_identity) {
            cache.generation = cache.generation.wrapping_add(1);
            cache.auth_identity = Some(auth_identity);
            cache.reconciliation_generation = None;
            cache.needs_effective_plugins_refresh = false;
            cache.plugins.take().is_some()
        } else {
            false
        };
        let generation = cache.generation;
        drop(cache);
        if cache_changed {
            self.clear_loaded_plugins_cache();
        }
        Some(generation)
    }

    pub(super) fn abandon_remote_installed_plugins_reconcile(&self, generation: u64) {
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.reconciliation_generation != Some(generation) {
            return;
        }
        cache.reconciliation_generation = None;
        cache.generation = cache.generation.wrapping_add(1);
        cache.needs_effective_plugins_refresh = true;
        drop(cache);
        self.clear_loaded_plugins_cache();
    }

    pub(super) fn remote_installed_plugins_cache_generation_if_current(
        &self,
        auth: Option<&CodexAuth>,
    ) -> Option<u64> {
        let auth_identity = RemoteInstalledPluginsAuthIdentity::from_auth(auth);
        if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
            return None;
        }
        let cache = match self.remote_installed_plugins_cache.read() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        (cache.auth_identity.as_ref() == Some(&auth_identity)).then_some(cache.generation)
    }

    pub(super) fn write_remote_installed_plugins_cache_snapshot(
        &self,
        generation: u64,
        plugins: Vec<RemoteInstalledPlugin>,
        auth: Option<&CodexAuth>,
        publication: RemoteInstalledPluginsCachePublication,
    ) -> Option<bool> {
        let auth_identity = RemoteInstalledPluginsAuthIdentity::from_auth(auth);
        if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
            return None;
        }
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.generation != generation
            || cache
                .auth_identity
                .as_ref()
                .is_some_and(|current| current != &auth_identity)
        {
            return None;
        }
        let is_reconcile = matches!(
            publication,
            RemoteInstalledPluginsCachePublication::Reconcile
        );
        if !is_reconcile && cache.reconciliation_generation == Some(generation) {
            return None;
        }
        if cache.auth_identity.is_none() {
            cache.auth_identity = Some(auth_identity);
        }
        if is_reconcile {
            cache.reconciliation_generation = None;
            cache.generation = cache.generation.wrapping_add(1);
        }
        let needs_effective_plugins_refresh =
            is_reconcile && std::mem::take(&mut cache.needs_effective_plugins_refresh);
        cache.display_assets = match publication {
            RemoteInstalledPluginsCachePublication::Refresh(display_assets) => display_assets,
            RemoteInstalledPluginsCachePublication::Reconcile => {
                InstalledPluginDisplayAssets::Include
            }
        };
        if cache.plugins.as_ref() == Some(&plugins) {
            drop(cache);
            if needs_effective_plugins_refresh {
                self.clear_loaded_plugins_cache();
            }
            return Some(needs_effective_plugins_refresh);
        }
        let metadata_changed = cache.plugins.as_ref().is_none_or(|previous| {
            !crate::remote_metadata::installed_plugin_metadata_eq(previous, &plugins)
        });
        cache.plugins = Some(plugins);
        drop(cache);
        let changed = needs_effective_plugins_refresh || metadata_changed;
        if changed {
            self.clear_loaded_plugins_cache();
        }
        Some(changed)
    }

    pub fn clear_remote_installed_plugins_cache(&self) -> bool {
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        cache.generation = cache.generation.wrapping_add(1);
        cache.auth_identity = None;
        cache.reconciliation_generation = None;
        cache.needs_effective_plugins_refresh = false;
        if cache.plugins.take().is_none() {
            return false;
        }
        drop(cache);
        self.clear_loaded_plugins_cache();
        true
    }
}
