//! Startup and plugin-list background tasks: curated sync, marketplace upgrades, cache refresh.

use super::*;

impl PluginsManager {
    /// Starts the local curated marketplace sync when the remote catalog is unavailable.
    pub fn maybe_start_curated_repo_sync_for_config(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) {
        if config.plugins_enabled
            && !self.remote_global_catalog_active(config)
            && MarketplacePolicy::from_requirements(config.config_layer_stack.requirements())
                .validate_git_source(OPENAI_PLUGINS_GIT_URL, /*ref_name*/ None)
                .is_ok()
        {
            self.start_curated_repo_sync(
                config.http_client_factory.clone(),
                on_effective_plugins_changed,
            );
        }
    }

    pub fn maybe_start_plugin_list_background_tasks(
        self: &Arc<Self>,
        context: &PluginMarketplaceContext,
        auth: Option<CodexAuth>,
        options: PluginListBackgroundTaskOptions,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) {
        self.maybe_start_non_curated_plugin_cache_refresh(context, &options.local_marketplaces);
        self.maybe_start_remote_catalog_cache_refresh(
            &context.global_config,
            auth.clone(),
            options.remote_catalog_cache_refresh_scopes,
        );
        self.maybe_start_remote_plugin_caches_refresh(
            &context.global_config,
            auth.clone(),
            on_effective_plugins_changed.clone(),
        );
        self.maybe_start_remote_installed_plugin_bundle_sync(
            &context.global_config,
            auth,
            on_effective_plugins_changed,
        );
    }

    /// Starts background tasks and sends marketplace upgrade warnings through the supplied channel.
    pub fn maybe_start_plugin_startup_tasks_for_config(
        self: &Arc<Self>,
        config: &PluginsConfigInput,
        reload_config: ConfigLayerReload,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
        upgrade_warnings: mpsc::UnboundedSender<String>,
    ) {
        if config.plugins_enabled {
            self.maybe_start_curated_repo_sync_for_config(
                config,
                on_effective_plugins_changed.clone(),
            );
            let should_spawn_marketplace_auto_upgrade = {
                let mut state = match self.configured_marketplace_upgrade_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                if state.in_flight {
                    false
                } else {
                    state.in_flight = true;
                    true
                }
            };
            if should_spawn_marketplace_auto_upgrade {
                let manager = Arc::clone(self);
                let config = config.clone();
                let on_effective_plugins_changed = on_effective_plugins_changed.clone();
                let runtime = tokio::runtime::Handle::current();
                let task_warnings = upgrade_warnings.clone();
                if let Err(err) = std::thread::Builder::new()
                    .name("plugins-marketplace-auto-upgrade".to_string())
                    .spawn(move || {
                        let outcome = manager.upgrade_configured_marketplaces_for_config_with_mode(
                            &config,
                            /*marketplace_name*/ None,
                            PluginGitMode::Automatic,
                            &reload_config,
                        );
                        match outcome {
                            Ok(outcome) => {
                                if !outcome.upgraded_roots.is_empty()
                                    && let Some(on_effective_plugins_changed) =
                                        on_effective_plugins_changed
                                {
                                    runtime.spawn(async move {
                                        on_effective_plugins_changed(
                                            EffectivePluginsChange::default(),
                                        );
                                    });
                                }
                                for error in outcome.errors {
                                    warn!(
                                        marketplace = error.marketplace_name,
                                        error = %error.message,
                                        "failed to auto-upgrade configured marketplace"
                                    );
                                    let _ = task_warnings.send(format!(
                                        "Codex could not update marketplace `{}`: {}",
                                        error.marketplace_name, error.message
                                    ));
                                }
                            }
                            Err(err) => {
                                warn!("failed to auto-upgrade configured marketplaces: {err}");
                                let _ = task_warnings
                                    .send(format!("Codex could not update marketplaces: {err}"));
                            }
                        }

                        let mut state = match manager.configured_marketplace_upgrade_state.write() {
                            Ok(state) => state,
                            Err(err) => err.into_inner(),
                        };
                        state.in_flight = false;
                    })
                {
                    let mut state = match self.configured_marketplace_upgrade_state.write() {
                        Ok(state) => state,
                        Err(err) => err.into_inner(),
                    };
                    state.in_flight = false;
                    warn!("failed to start configured marketplace auto-upgrade task: {err}");
                    let _ = upgrade_warnings
                        .send(format!("Codex could not start marketplace updates: {err}"));
                }
            }
            let config_for_remote_sync = config.clone();
            let manager = Arc::clone(self);
            tokio::spawn(async move {
                let auth = manager.auth_manager.auth().await;
                manager.maybe_start_remote_installed_plugins_cache_refresh_with_notify(
                    &config_for_remote_sync,
                    auth.clone(),
                    RemoteInstalledPluginsCacheRefreshNotify::IfCacheChanged,
                    on_effective_plugins_changed.clone(),
                    EffectivePluginsChange::default(),
                );
                manager.maybe_start_remote_installed_plugin_bundle_sync(
                    &config_for_remote_sync,
                    auth,
                    on_effective_plugins_changed,
                );
            });
        }
    }

    pub fn upgrade_configured_marketplaces_for_config(
        &self,
        config: &PluginsConfigInput,
        marketplace_name: Option<&str>,
        reload_config: &ConfigLayerReload,
    ) -> Result<ConfiguredMarketplaceUpgradeOutcome, String> {
        self.upgrade_configured_marketplaces_for_config_with_mode(
            config,
            marketplace_name,
            PluginGitMode::Manual,
            reload_config,
        )
    }

    /// Carries the initiating Git trust policy through marketplace and plugin refreshes.
    fn upgrade_configured_marketplaces_for_config_with_mode(
        &self,
        config: &PluginsConfigInput,
        marketplace_name: Option<&str>,
        mode: PluginGitMode,
        reload_config: &ConfigLayerReload,
    ) -> Result<ConfiguredMarketplaceUpgradeOutcome, String> {
        let mut outcome = upgrade_configured_git_marketplaces_with_mode(
            self.codex_home.as_path(),
            &config.config_layer_stack,
            marketplace_name,
            mode,
            reload_config,
        );
        if let Some(marketplace_name) = marketplace_name
            && outcome.selected_marketplaces.is_empty()
        {
            return Err(format!(
                "marketplace `{marketplace_name}` is not configured as a Git marketplace"
            ));
        }
        if !outcome.upgraded_roots.is_empty() {
            let mut configured_plugin_keys = configured_plugins_from_stack(
                &config.config_layer_stack,
                self.codex_home.as_path(),
            )
            .into_keys()
            .collect::<Vec<_>>();
            configured_plugin_keys.sort_unstable();
            match refresh_non_curated_plugin_cache_force_reinstall_detailed(
                self.codex_home.as_path(),
                &outcome.upgraded_roots,
                &configured_plugin_keys,
                mode,
            ) {
                Ok(refresh_outcome) => {
                    self.clear_caches_after_marketplace_source_refresh(
                        refresh_outcome.cache_refreshed,
                        /*on_effective_plugins_changed*/ None,
                    );
                    outcome
                        .errors
                        .extend(refresh_outcome.errors.into_iter().map(|error| {
                            ConfiguredMarketplaceUpgradeError {
                                marketplace_name: error.marketplace_name,
                                message: error.message,
                            }
                        }));
                }
                Err(err) => {
                    self.clear_cache();
                    outcome.errors.push(ConfiguredMarketplaceUpgradeError {
                        marketplace_name: marketplace_name
                            .unwrap_or("all configured marketplaces")
                            .to_string(),
                        message: format!(
                            "failed to refresh installed plugin cache after marketplace upgrade: {err}"
                        ),
                    });
                }
            }
        }
        Ok(outcome)
    }

    pub fn maybe_start_non_curated_plugin_cache_refresh(
        self: &Arc<Self>,
        context: &PluginMarketplaceContext,
        marketplaces: &[ConfiguredMarketplace],
    ) {
        if let Some(request) = context.non_curated_cache_refresh_request(
            self,
            marketplaces,
            NonCuratedCacheRefreshMode::IfVersionChanged,
            PluginGitMode::Automatic,
        ) {
            self.schedule_non_curated_plugin_cache_refresh(request);
        }
    }

    /// Runs an explicitly requested refresh using the caller's normal Git configuration.
    pub async fn refresh_non_curated_plugin_cache_for_context(
        self: &Arc<Self>,
        context: &PluginMarketplaceContext,
    ) -> bool {
        let Ok(_refresh_permit) = self.non_curated_cache_refresh_lock.acquire().await else {
            return false;
        };
        let mut completion = self.non_curated_cache_refresh_completion.subscribe();
        let changed_sequence = completion.borrow_and_update().changed_sequence;
        let marketplaces =
            match self.list_marketplaces_for_context(context, /*include_openai_curated*/ false) {
                Ok(outcome) => outcome.marketplaces,
                Err(err) => {
                    warn!("failed to prepare non-curated plugin cache refresh: {err}");
                    Vec::new()
                }
            };
        if let Some(request) = context.non_curated_cache_refresh_request(
            self,
            &marketplaces,
            NonCuratedCacheRefreshMode::IfVersionChanged,
            PluginGitMode::Manual,
        ) {
            self.schedule_non_curated_plugin_cache_refresh(request);
        }

        loop {
            let in_flight = match self.non_curated_cache_refresh_state.read() {
                Ok(state) => state.in_flight,
                Err(err) => err.into_inner().in_flight,
            };
            if !in_flight {
                return completion.borrow().changed_sequence != changed_sequence;
            }
            if completion.changed().await.is_err() {
                return false;
            }
        }
    }

    pub(super) fn schedule_non_curated_plugin_cache_refresh(
        self: &Arc<Self>,
        mut request: NonCuratedCacheRefreshRequest,
    ) {
        let should_spawn = {
            let mut state = match self.non_curated_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            if request.mode == NonCuratedCacheRefreshMode::IfVersionChanged
                && state.last_refreshed.as_ref().is_some_and(|last_refreshed| {
                    request.configured_plugin_sources.iter().any(|source| {
                        last_refreshed
                            .configured_plugin_sources
                            .iter()
                            .any(|previous_source| {
                                previous_source.plugin_key == source.plugin_key
                                    && previous_source.local_version == source.local_version
                                    && (previous_source.marketplace_path != source.marketplace_path
                                        || previous_source.source != source.source)
                            })
                    })
                })
            {
                request.mode = NonCuratedCacheRefreshMode::ForceReinstall;
            }
            if request.mode == NonCuratedCacheRefreshMode::IfVersionChanged
                && state.requested.as_ref().is_some_and(|requested| {
                    requested.mode == NonCuratedCacheRefreshMode::ForceReinstall
                        && requested.roots == request.roots
                })
            {
                request.mode = NonCuratedCacheRefreshMode::ForceReinstall;
            }
            // Reconcile each canonical plugin generation once before publishing its resource.
            if state.requested.as_ref().is_some_and(|requested| {
                requested == &request
                    || (requested.git_mode == PluginGitMode::Manual
                        && request.git_mode == PluginGitMode::Automatic
                        && requested.roots == request.roots
                        && requested.configured_plugin_keys == request.configured_plugin_keys
                        && requested.configured_plugin_sources == request.configured_plugin_sources
                        && requested.mode == request.mode)
            }) || (request.mode == NonCuratedCacheRefreshMode::IfVersionChanged
                && !state.in_flight
                && state.last_refreshed.as_ref().is_some_and(|last_refreshed| {
                    last_refreshed.roots == request.roots
                        && last_refreshed.configured_plugin_keys == request.configured_plugin_keys
                        && last_refreshed.configured_plugin_sources
                            == request.configured_plugin_sources
                        && (last_refreshed.git_mode == request.git_mode
                            || last_refreshed.git_mode == PluginGitMode::Manual)
                }))
            {
                return;
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
        if let Err(err) = std::thread::Builder::new()
            .name("plugins-non-curated-cache-refresh".to_string())
            .spawn(move || manager.run_non_curated_plugin_cache_refresh_loop())
        {
            let mut state = match self.non_curated_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            state.in_flight = false;
            state.requested = None;
            self.non_curated_cache_refresh_completion
                .send_modify(|completion| {
                    completion.sequence = completion.sequence.wrapping_add(1);
                });
            warn!("failed to start non-curated plugin cache refresh task: {err}");
        }
    }

    fn start_curated_repo_sync(
        self: &Arc<Self>,
        http_client_factory: HttpClientFactory,
        on_effective_plugins_changed: Option<EffectivePluginsChangedCallback>,
    ) {
        if CURATED_REPO_SYNC_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let on_effective_plugins_changed =
            on_effective_plugins_changed.map(|on_effective_plugins_changed| {
                let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                    return on_effective_plugins_changed;
                };
                let callback: EffectivePluginsChangedCallback = Arc::new(move |change| {
                    let on_effective_plugins_changed = Arc::clone(&on_effective_plugins_changed);
                    runtime.spawn(async move {
                        on_effective_plugins_changed(change);
                    });
                });
                callback
            });
        let manager = Arc::clone(self);
        let codex_home = self.codex_home.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("plugins-curated-repo-sync".to_string())
            .spawn(move || {
                match sync_openai_plugins_repo(codex_home.as_path(), http_client_factory) {
                    Ok(curated_plugin_version) => {
                        let configured_curated_plugin_ids =
                            configured_curated_plugin_ids_from_codex_home(codex_home.as_path());
                        match refresh_curated_plugin_cache(
                            codex_home.as_path(),
                            &curated_plugin_version,
                            &configured_curated_plugin_ids,
                        ) {
                            Ok(cache_refreshed) => {
                                manager.clear_caches_after_marketplace_source_refresh(
                                    cache_refreshed,
                                    on_effective_plugins_changed.as_ref(),
                                );
                            }
                            Err(err) => {
                                manager.clear_cache();
                                CURATED_REPO_SYNC_STARTED.store(false, Ordering::SeqCst);
                                warn!("failed to refresh curated plugin cache after sync: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        CURATED_REPO_SYNC_STARTED.store(false, Ordering::SeqCst);
                        warn!("failed to sync curated plugins repo: {err}");
                    }
                }
            })
        {
            CURATED_REPO_SYNC_STARTED.store(false, Ordering::SeqCst);
            warn!("failed to start curated plugins repo sync task: {err}");
        }
    }

    fn run_non_curated_plugin_cache_refresh_loop(self: Arc<Self>) {
        loop {
            let request = {
                let state = match self.non_curated_cache_refresh_state.read() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                state.requested.clone()
            };

            let Some(request) = request else {
                let mut state = match self.non_curated_cache_refresh_state.write() {
                    Ok(state) => state,
                    Err(err) => err.into_inner(),
                };
                state.in_flight = false;
                self.non_curated_cache_refresh_completion
                    .send_modify(|completion| {
                        completion.sequence = completion.sequence.wrapping_add(1);
                    });
                return;
            };

            let refresh_result = match request.mode {
                NonCuratedCacheRefreshMode::IfVersionChanged => {
                    refresh_non_curated_plugin_cache_detailed(
                        self.codex_home.as_path(),
                        &request.roots,
                        &request.configured_plugin_keys,
                        request.git_mode,
                    )
                }
                NonCuratedCacheRefreshMode::ForceReinstall => {
                    refresh_non_curated_plugin_cache_force_reinstall_detailed(
                        self.codex_home.as_path(),
                        &request.roots,
                        &request.configured_plugin_keys,
                        request.git_mode,
                    )
                }
            };
            let (refreshed, cache_changed) = match refresh_result {
                Ok(refresh_outcome) => {
                    if refresh_outcome.cache_refreshed {
                        self.clear_cache();
                    }
                    for error in &refresh_outcome.errors {
                        warn!(
                            marketplace = error.marketplace_name,
                            error = %error.message,
                            "failed to refresh configured plugin cache"
                        );
                    }
                    (
                        refresh_outcome.errors.is_empty(),
                        refresh_outcome.cache_refreshed,
                    )
                }
                Err(err) => {
                    self.clear_cache();
                    warn!("failed to refresh non-curated plugin cache: {err}");
                    (false, false)
                }
            };

            let mut state = match self.non_curated_cache_refresh_state.write() {
                Ok(state) => state,
                Err(err) => err.into_inner(),
            };
            if refreshed {
                state.last_refreshed = Some(request.clone());
            }
            let complete = state.requested.as_ref() == Some(&request);
            if complete {
                state.requested = None;
                state.in_flight = false;
            }
            self.non_curated_cache_refresh_completion
                .send_modify(|completion| {
                    completion.sequence = completion.sequence.wrapping_add(1);
                    if cache_changed {
                        completion.changed_sequence = completion.changed_sequence.wrapping_add(1);
                    }
                });
            if complete {
                return;
            }
        }
    }
}
