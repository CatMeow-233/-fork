//! Background refresh of remote installed-plugin and catalog caches, and bundle reconciliation.

use super::*;

impl PluginsManager {
    fn clear_remote_installed_plugins_cache_if_current(&self, generation: u64) -> Option<bool> {
        let mut cache = match self.remote_installed_plugins_cache.write() {
            Ok(cache) => cache,
            Err(err) => err.into_inner(),
        };
        if cache.generation != generation {
            return None;
        }
        cache.generation = cache.generation.wrapping_add(1);
        cache.auth_identity = None;
        cache.reconciliation_generation = None;
        cache.needs_effective_plugins_refresh = false;
        if cache.plugins.take().is_none() {
            return Some(false);
        }
        drop(cache);
        self.clear_loaded_plugins_cache();
        Some(true)
    }

    pub fn maybe_start_remote_plugin_caches_refresh(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<CodexAuth>,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) {
        self.maybe_start_remote_installed_plugins_cache_refresh_with_notify(
            config,
            auth.clone(),
            RemoteInstalledPluginsCacheRefreshNotify::IfCacheChanged,
            on_effective_plugins_changed,
            EffectivePluginsChange::default(),
        );

        let manager = Arc::clone(self);
        let config = config.clone();
        tokio::spawn(async move {
            manager
                .recommended_plugins_mode_for_config(&config, auth.as_ref())
                .await;
        });
    }

    pub(super) fn maybe_start_remote_installed_plugins_cache_refresh_after_mutation(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<CodexAuth>,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) {
        self.maybe_start_remote_installed_plugins_cache_refresh_with_notify(
            config,
            auth,
            RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh,
            on_effective_plugins_changed,
            EffectivePluginsChange::default(),
        );
    }

    pub(super) fn maybe_start_remote_installed_plugins_cache_refresh_with_notify(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<CodexAuth>,
        notify: RemoteInstalledPluginsCacheRefreshNotify,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
        change: EffectivePluginsChange,
    ) {
        if !config.plugins_enabled {
            return;
        }

        let Some(generation) =
            self.prepare_remote_installed_plugins_cache_generation(auth.as_ref())
        else {
            return;
        };
        self.schedule_remote_installed_plugins_cache_refresh(
            RemoteInstalledPluginsCacheRefreshRequest {
                generation,
                service_config: self.remote_plugin_service_config(config),
                auth,
                notify,
                on_effective_plugins_changed,
                change,
            },
        );
    }

    pub fn maybe_start_remote_installed_plugin_bundle_sync(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<CodexAuth>,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) {
        if !config.plugins_enabled {
            return;
        }

        let Some(auth) = auth else {
            return;
        };
        let Ok(permit) =
            Arc::clone(&self.remote_installed_plugin_bundle_sync_gate).try_acquire_owned()
        else {
            return;
        };
        let manager = Arc::clone(self);
        let config = config.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = manager
                .reconcile_remote_installed_plugins_after_acquiring_gate(&config, Some(&auth))
                .await;
            match result {
                Ok((outcome, effective_plugins_changed)) => {
                    tracing::info!(
                        materialized_remote_plugins = ?outcome.materialized_remote_plugins,
                        changed_plugins = ?outcome.changed_plugins,
                        failed_remote_plugin_ids = ?outcome.failed_remote_plugin_ids,
                        failed_materialization_remote_plugin_ids = ?outcome.failed_materialization_remote_plugin_ids,
                        "completed remote installed plugin bundle sync"
                    );
                    if (effective_plugins_changed || !outcome.changed_plugins.is_empty())
                        && let Some(on_effective_plugins_changed) = on_effective_plugins_changed
                    {
                        on_effective_plugins_changed(EffectivePluginsChange {
                            materialized_remote_plugins: outcome.materialized_remote_plugins,
                        });
                    }
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "remote installed plugin bundle sync failed"
                    );
                    manager.maybe_start_remote_installed_plugins_cache_refresh_with_notify(
                        &config,
                        Some(auth),
                        RemoteInstalledPluginsCacheRefreshNotify::IfCacheChanged,
                        on_effective_plugins_changed,
                        EffectivePluginsChange::default(),
                    );
                }
            }
        });
    }

    /// Acquires the cache-root gate shared by full installed-bundle sync, reconciliation, and
    /// direct remote plugin mutations.
    pub(super) async fn acquire_remote_installed_plugin_sync_guard(
        &self,
    ) -> Result<RemoteInstalledPluginSyncGuard, RemoteInstalledPluginBundleSyncError> {
        let permit = tokio::time::timeout(
            REMOTE_INSTALLED_PLUGIN_SYNC_WAIT_TIMEOUT,
            Arc::clone(&self.remote_installed_plugin_bundle_sync_gate).acquire_owned(),
        )
        .await
        .map_err(|_| RemoteInstalledPluginBundleSyncError::LockTimeout)?
        .map_err(|_| RemoteInstalledPluginBundleSyncError::Superseded)?;
        Ok(RemoteInstalledPluginSyncGuard { _permit: permit })
    }

    /// Synchronizes remote bundles and publishes the same installed-plugin snapshot.
    ///
    /// The shared gate orders this blocking path with background bundle synchronization. A
    /// reconciled publication advances the cache generation so metadata fetches that started
    /// before or during the bundle pass cannot overwrite its snapshot afterward.
    pub async fn reconcile_remote_installed_plugins(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
    ) -> Result<RemoteInstalledPluginBundleSyncOutcome, RemoteInstalledPluginBundleSyncError> {
        let _guard = self.acquire_remote_installed_plugin_sync_guard().await?;
        let (outcome, _) = self
            .reconcile_remote_installed_plugins_after_acquiring_gate(config, auth)
            .await?;
        Ok(outcome)
    }

    async fn reconcile_remote_installed_plugins_after_acquiring_gate(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
    ) -> Result<(RemoteInstalledPluginBundleSyncOutcome, bool), RemoteInstalledPluginBundleSyncError>
    {
        let generation = self
            .begin_remote_installed_plugins_reconcile(auth)
            .ok_or(RemoteInstalledPluginBundleSyncError::Superseded)?;
        let mut reconciliation = RemoteInstalledPluginsReconciliationGuard {
            manager: self,
            generation,
            committed: false,
        };
        let previous_enabled = {
            let cache = match self.remote_installed_plugins_cache.read() {
                Ok(cache) => cache,
                Err(err) => err.into_inner(),
            };
            cache.plugins.as_ref().map(|plugins| {
                plugins
                    .iter()
                    .filter_map(|plugin| {
                        PluginId::new(plugin.name.clone(), plugin.marketplace_name.clone())
                            .ok()
                            .map(|plugin_id| (plugin_id, plugin.enabled))
                    })
                    .collect::<HashMap<_, _>>()
            })
        };
        let previous_plugin_ids = previous_enabled
            .iter()
            .flat_map(HashMap::keys)
            .cloned()
            .collect::<Vec<_>>();
        let result = crate::remote::sync_remote_installed_plugin_bundles_once_with_snapshot(
            self.codex_home.clone(),
            &self.remote_plugin_service_config(config),
            auth,
            &previous_plugin_ids,
        )
        .await?;
        let mut outcome = result.outcome;
        // The generation fence keeps this comparison on the snapshot replaced by this pass.
        // In a known snapshot, absence means inactive: cached reinstalls need the same hints
        // as re-enablement, without becoming materializations or triggering hook-trust writes.
        if let Some(previous_enabled) = previous_enabled {
            for plugin in &result.installed_plugins {
                let plugin_id = PluginId::new(plugin.name.clone(), plugin.marketplace_name.clone())
                    .map_err(|err| RemotePluginCatalogError::UnexpectedResponse(err.to_string()))?;
                if previous_enabled
                    .get(&plugin_id)
                    .copied()
                    .unwrap_or_default()
                    == plugin.enabled
                {
                    continue;
                }
                let plugin_key = plugin_id.as_key();
                if outcome
                    .changed_plugins
                    .iter()
                    .any(|change| change.plugin_id == plugin_key)
                    || self.store.active_plugin_root(&plugin_id).is_none()
                {
                    continue;
                }
                let mut capabilities = RemotePluginCapabilities::default();
                capabilities
                    .include_active_bundle(&self.store, &plugin_id)
                    .await;
                outcome.changed_plugins.push(RemotePluginChange {
                    plugin_id: plugin_key,
                    capabilities,
                });
            }
        }
        outcome
            .changed_plugins
            .sort_unstable_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        let Some(effective_plugins_changed) = self.write_remote_installed_plugins_cache_snapshot(
            generation,
            result.installed_plugins,
            auth,
            RemoteInstalledPluginsCachePublication::Reconcile,
        ) else {
            return Err(RemoteInstalledPluginBundleSyncError::Superseded);
        };
        reconciliation.committed = true;
        if !effective_plugins_changed && !outcome.changed_plugins.is_empty() {
            self.clear_loaded_plugins_cache();
        }
        Ok((outcome, effective_plugins_changed))
    }

    pub(super) fn maybe_start_remote_catalog_cache_refresh(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        auth: Option<CodexAuth>,
        scopes: BTreeSet<RemotePluginScope>,
    ) {
        if !config.plugins_enabled || scopes.is_empty() {
            return;
        }

        self.schedule_remote_catalog_cache_refresh(RemoteCatalogCacheRefreshRequest {
            service_config: self.remote_plugin_service_config(config),
            auth,
            scopes,
        });
    }

    pub(super) fn schedule_remote_installed_plugins_cache_refresh(
        self: &Arc<Self>,
        mut request: RemoteInstalledPluginsCacheRefreshRequest,
    ) {
        if self.remote_installed_plugins_cache_generation_if_current(request.auth.as_ref())
            != Some(request.generation)
        {
            return;
        }
        let should_spawn = {
            let mut state = match self.remote_installed_plugins_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            if self.remote_installed_plugins_cache_generation_if_current(request.auth.as_ref())
                != Some(request.generation)
            {
                return;
            }
            if let Some(existing_request) = state.requested.as_ref()
                && existing_request.generation == request.generation
            {
                if matches!(
                    existing_request.notify,
                    RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh
                ) {
                    request.notify =
                        RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh;
                }
                if !existing_request
                    .change
                    .materialized_remote_plugins
                    .is_empty()
                    && let Some(existing_callback) =
                        existing_request.on_effective_plugins_changed.as_ref()
                {
                    request.on_effective_plugins_changed = Some(Arc::clone(existing_callback));
                } else if request.on_effective_plugins_changed.is_none() {
                    request.on_effective_plugins_changed =
                        existing_request.on_effective_plugins_changed.clone();
                }
                for materialization in &existing_request.change.materialized_remote_plugins {
                    if !request
                        .change
                        .materialized_remote_plugins
                        .iter()
                        .any(|pending| pending.plugin_id == materialization.plugin_id)
                    {
                        request
                            .change
                            .materialized_remote_plugins
                            .push(materialization.clone());
                    }
                }
                request
                    .change
                    .materialized_remote_plugins
                    .sort_by_key(|materialization| materialization.plugin_id.as_key());
            }
            state.requested = Some(request);
            if state.in_flight {
                false
            } else {
                state.in_flight = true;
                true
            }
        };
        if !should_spawn {
            return;
        }

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager
                .run_remote_installed_plugins_cache_refresh_loop()
                .await;
        });
    }

    fn schedule_remote_catalog_cache_refresh(
        self: &Arc<Self>,
        request: RemoteCatalogCacheRefreshRequest,
    ) {
        let should_spawn = {
            let mut state = match self.remote_catalog_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            if let Some(pending) = state
                .requests
                .iter_mut()
                .find(|pending| pending.has_same_cache_identity(&request))
            {
                pending.scopes.extend(request.scopes);
                pending.auth = request.auth;
            } else {
                state.requests.push_back(request);
            }
            if state.in_flight {
                false
            } else {
                state.in_flight = true;
                true
            }
        };
        if !should_spawn {
            return;
        }

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager.run_remote_catalog_cache_refresh_loop().await;
        });
    }

    async fn run_remote_installed_plugins_cache_refresh_loop(self: Arc<Self>) {
        loop {
            let request = {
                let mut state = match self.remote_installed_plugins_cache_refresh_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                match state.requested.take() {
                    Some(request) => request,
                    None => {
                        state.in_flight = false;
                        return;
                    }
                }
            };

            let plugins = crate::remote::fetch_remote_installed_plugins(
                &request.service_config,
                request.auth.as_ref(),
            )
            .await;
            match plugins {
                Ok(plugins) => {
                    let Some(changed) = self.write_remote_installed_plugins_cache_snapshot(
                        request.generation,
                        plugins,
                        request.auth.as_ref(),
                        RemoteInstalledPluginsCachePublication::Refresh(
                            InstalledPluginDisplayAssets::Include,
                        ),
                    ) else {
                        continue;
                    };
                    let should_notify = changed
                        || !request.change.materialized_remote_plugins.is_empty()
                        || matches!(
                            request.notify,
                            RemoteInstalledPluginsCacheRefreshNotify::AfterSuccessfulRefresh
                        );
                    if should_notify
                        && let Some(on_effective_plugins_changed) =
                            request.on_effective_plugins_changed
                    {
                        on_effective_plugins_changed(request.change);
                    }
                }
                Err(
                    RemotePluginCatalogError::AuthRequired
                    | RemotePluginCatalogError::UnsupportedAuthMode,
                ) => {
                    let Some(changed) =
                        self.clear_remote_installed_plugins_cache_if_current(request.generation)
                    else {
                        continue;
                    };
                    if changed
                        && let Some(on_effective_plugins_changed) =
                            request.on_effective_plugins_changed
                    {
                        on_effective_plugins_changed(EffectivePluginsChange::default());
                    }
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        materialized_remote_plugin_count = request
                            .change
                            .materialized_remote_plugins
                            .len(),
                        "failed to refresh remote installed plugins cache"
                    );
                }
            }
        }
    }

    async fn run_remote_catalog_cache_refresh_loop(self: Arc<Self>) {
        loop {
            let request = {
                let mut state = match self.remote_catalog_cache_refresh_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                match state.requests.pop_front() {
                    Some(request) => request,
                    None => {
                        state.in_flight = false;
                        return;
                    }
                }
            };

            for scope in request.scopes {
                if crate::remote::has_fresh_cached_remote_plugin_catalog(
                    self.codex_home.as_path(),
                    &request.service_config,
                    request.auth.as_ref(),
                    scope,
                ) {
                    continue;
                }

                match crate::remote::fetch_and_cache_remote_plugin_catalog(
                    self.codex_home.as_path(),
                    &request.service_config,
                    request.auth.as_ref(),
                    scope,
                )
                .await
                {
                    Ok(()) => {}
                    Err(
                        RemotePluginCatalogError::AuthRequired
                        | RemotePluginCatalogError::UnsupportedAuthMode,
                    ) => {}
                    Err(err) => {
                        warn!(
                            error = %err,
                            scope = ?scope,
                            "failed to refresh cached remote plugin catalog"
                        );
                    }
                }
            }
        }
    }
}
