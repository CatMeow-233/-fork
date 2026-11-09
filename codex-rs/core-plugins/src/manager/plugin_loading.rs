//! Loading effective plugins and hooks for a config, plus the loaded-plugins cache.

use super::*;

impl PluginsManager {
    /// Returns skill snapshots parsed while loading the matching plugin cache entry.
    pub fn plugin_skill_snapshots_for_config(
        &self,
        config: &PluginsConfigInput,
    ) -> Option<SkillRootSnapshots<PluginSkillRoot>> {
        if !config.plugins_enabled {
            return None;
        }
        let auth = self.auth_manager.auth_cached();
        let key = PluginLoadCacheKey::from_config(
            config,
            self.codex_home.as_path(),
            self.remote_global_catalog_active(config),
            RemoteInstalledPluginsAuthIdentity::from_auth(auth.as_ref()),
        );
        LoadedPluginsCache::get(&self.loaded_plugins_cache, &key, &self.store)
            .map(|cached| cached.plugin_skill_snapshots.clone())
    }

    #[instrument(
        name = "plugins_for_config",
        level = "info",
        skip_all,
        fields(
            otel.name = "plugins_for_config",
            plugins_enabled = config.plugins_enabled
        )
    )]
    pub async fn plugins_for_config(&self, config: &PluginsConfigInput) -> PluginLoadOutcome {
        if !config.plugins_enabled {
            return PluginLoadOutcome::default();
        }

        // Both the semaphore wait and skill loading can outlive the auth state captured at the
        // start of this load. Account switches require a restart, so discard those passes;
        // same-account revisions also cover ordinary token refreshes, where returning no plugins
        // is wrong, so retry those instead.
        let auth_change_receiver = self.auth_manager.auth_change_receiver();
        let mut cache_outcome = RequestOutcome::Hit;

        loop {
            let auth_revision = *auth_change_receiver.borrow();
            let remote_global_catalog_active = self.remote_global_catalog_active(config);
            let auth = self.auth_manager.auth_cached();
            let auth_mode = auth.as_ref().map(CodexAuth::api_auth_mode);
            let auth_identity = RemoteInstalledPluginsAuthIdentity::from_auth(auth.as_ref());
            let cache_key = PluginLoadCacheKey::from_config(
                config,
                self.codex_home.as_path(),
                remote_global_catalog_active,
                auth_identity.clone(),
            );
            if let Some(plugins) = self.cached_loaded_plugins(&cache_key) {
                let outcome = self.resolve_loaded_plugins_for_auth(plugins, auth_mode);
                if *auth_change_receiver.borrow() == auth_revision {
                    cache_outcome.record();
                    return outcome;
                }
                if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
                    return PluginLoadOutcome::default();
                }
                continue;
            }

            if matches!(cache_outcome, RequestOutcome::Hit) {
                cache_outcome = RequestOutcome::HitAfterWait;
            }
            let wait_started = Instant::now();
            let load_permit = self.loaded_plugins_load_semaphore.acquire().await;
            loaded_cache_metrics::record_duration(
                loaded_cache_metrics::WAIT_DURATION,
                wait_started.elapsed(),
            );
            let Ok(_load_permit) = load_permit else {
                warn!("plugin load semaphore closed");
                return PluginLoadOutcome::default();
            };
            if *auth_change_receiver.borrow() != auth_revision {
                if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
                    return PluginLoadOutcome::default();
                }
                continue;
            }
            if let Some(plugins) = self.cached_loaded_plugins(&cache_key) {
                let outcome = self.resolve_loaded_plugins_for_auth(plugins, auth_mode);
                if *auth_change_receiver.borrow() == auth_revision {
                    cache_outcome.record();
                    return outcome;
                }
                if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
                    return PluginLoadOutcome::default();
                }
                continue;
            }
            let cache_generation = self.loaded_plugins_cache_generation();
            let plugin_skill_snapshots = new_plugin_skill_snapshots();
            cache_outcome = RequestOutcome::Load;
            let load_started = Instant::now();
            let plugins = load_plugins_from_layer_stack(
                &config.config_layer_stack,
                &config.plugins,
                self.remote_installed_plugins_snapshot(),
                &self.store,
                Some(&plugin_skill_snapshots),
                self.restriction_product,
                remote_global_catalog_active,
                self.skill_root_loader.as_ref(),
            )
            .await;
            loaded_cache_metrics::record_duration(
                loaded_cache_metrics::LOAD_DURATION,
                load_started.elapsed(),
            );
            if *auth_change_receiver.borrow() != auth_revision {
                if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
                    return PluginLoadOutcome::default();
                }
                continue;
            }
            log_plugin_load_errors(&plugins);
            self.cache_loaded_plugins_if_current(
                cache_generation,
                cache_key,
                plugins.clone(),
                plugin_skill_snapshots,
            );
            let outcome = self.resolve_loaded_plugins_for_auth(plugins, auth_mode);
            if *auth_change_receiver.borrow() == auth_revision {
                cache_outcome.record();
                return outcome;
            }
            if !self.remote_installed_plugins_auth_is_current(&auth_identity) {
                return PluginLoadOutcome::default();
            }
        }
    }

    fn resolve_loaded_plugins_for_auth(
        &self,
        mut plugins: Vec<LoadedPlugin>,
        auth_mode: Option<AuthMode>,
    ) -> PluginLoadOutcome {
        let target_curated_marketplace = target_curated_marketplace(auth_mode);
        plugins.retain(|plugin| {
            plugin_is_eligible_for_target_marketplace(
                &plugin.config_name,
                target_curated_marketplace,
            )
        });
        for plugin in &mut plugins {
            let plugin_active = plugin.is_active();
            apply_app_mcp_routing_policy(
                &mut plugin.apps,
                &mut plugin.mcp_servers,
                auth_mode,
                plugin_active,
            );
        }
        PluginLoadOutcome::from_plugins(plugins)
    }

    pub fn clear_cache(&self) {
        self.clear_loaded_plugins_cache();
        let mut featured_plugin_ids_cache = match self.featured_plugin_ids_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        *featured_plugin_ids_cache = None;
    }

    pub(super) fn clear_loaded_plugins_cache(&self) {
        self.tool_suggest_metadata_cache.clear();
        let mut cache = match self.loaded_plugins_cache.lock() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        cache.generation = cache.generation.wrapping_add(1);
        cache.entries.clear();
        drop(cache);
        loaded_cache_metrics::record_event("clear");
    }

    pub(super) fn clear_caches_after_marketplace_source_refresh(
        &self,
        installed_plugin_cache_refreshed: bool,
        on_effective_plugins_changed: Option<&EffectivePluginsChangedCallback>,
    ) {
        if installed_plugin_cache_refreshed {
            self.clear_cache();
            if let Some(on_effective_plugins_changed) = on_effective_plugins_changed {
                on_effective_plugins_changed(EffectivePluginsChange::default());
            }
        } else {
            self.tool_suggest_metadata_cache.clear();
        }
    }

    /// Resolve plugin hooks for a config layer stack without loading other plugin capabilities.
    pub async fn plugin_hooks_for_layer_stack(
        &self,
        config_layer_stack: &ConfigLayerStack,
        config: &PluginsConfigInput,
    ) -> PluginHookLoadOutcome {
        if !config.plugins_enabled {
            return PluginHookLoadOutcome::default();
        }
        let target_curated_marketplace = target_curated_marketplace(self.auth_mode());
        load_plugin_hooks_from_layer_stack(
            config_layer_stack,
            &config.plugins,
            self.remote_installed_plugin_configs(),
            &self.store,
            target_curated_marketplace,
            self.remote_global_catalog_active(config),
        )
        .await
    }

    pub(super) fn loaded_plugins_cache_generation(&self) -> u64 {
        match self.loaded_plugins_cache.lock() {
            Ok(cache) => cache.generation,
            Err(err) => err.into_inner().generation,
        }
    }

    pub(super) fn cache_loaded_plugins_if_current(
        &self,
        generation: u64,
        key: PluginLoadCacheKey,
        plugins: Vec<LoadedPlugin>,
        plugin_skill_snapshots: SkillRootSnapshots<PluginSkillRoot>,
    ) {
        let mut cache = match self.loaded_plugins_cache.lock() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.generation != generation {
            return;
        }
        cache.entries.retain(|entry| entry.key != key);
        cache.entries.push_front(Arc::new(LoadedPluginsCacheEntry {
            key,
            plugins,
            plugin_skill_snapshots,
        }));
        let evicted = cache.entries.len() > LOADED_PLUGINS_CACHE_CAPACITY;
        cache.entries.truncate(LOADED_PLUGINS_CACHE_CAPACITY);
        drop(cache);
        if evicted {
            loaded_cache_metrics::record_event("capacity_eviction");
        }
    }
}
