//! Installing and uninstalling marketplace plugins, including install-failure analytics.

use super::*;

impl PluginsManager {
    pub async fn install_plugin(
        &self,
        config_layer_stack: &ConfigLayerStack,
        request: PluginInstallRequest,
    ) -> Result<PluginInstallOutcome, PluginInstallError> {
        let resolved = self.resolve_installable_plugin(config_layer_stack, &request)?;
        let plugin_id = resolved.plugin_id.clone();
        match self.install_resolved_plugin(resolved).await {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                self.track_plugin_install_failed(
                    &plugin_id,
                    plugin_install_error_type(&err),
                    err.sub_error_type(),
                    err.to_string(),
                );
                Err(err)
            }
        }
    }

    fn resolve_installable_plugin(
        &self,
        config_layer_stack: &ConfigLayerStack,
        request: &PluginInstallRequest,
    ) -> Result<ResolvedMarketplacePlugin, PluginInstallError> {
        let resolved = match find_installable_marketplace_plugin(
            &request.marketplace_path,
            &request.plugin_name,
            self.restriction_product,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                self.track_plugin_install_resolution_failed(&err);
                return Err(err.into());
            }
        };
        if let Err(message) =
            MarketplacePolicy::from_requirements(config_layer_stack.requirements())
                .validate_install(
                    config_layer_stack,
                    self.codex_home.as_path(),
                    &request.marketplace_path,
                    &resolved.plugin_id.marketplace_name,
                )
        {
            let err = MarketplaceError::InvalidMarketplaceFile {
                path: request.marketplace_path.to_path_buf(),
                message,
            };
            self.track_plugin_install_resolution_failed(&err);
            return Err(err.into());
        }
        Ok(resolved)
    }

    pub async fn install_plugin_with_remote_sync(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
        request: PluginInstallRequest,
    ) -> Result<PluginInstallOutcome, PluginInstallError> {
        let resolved = self.resolve_installable_plugin(&config.config_layer_stack, &request)?;
        let plugin_id = resolved.plugin_id.as_key();
        // This only forwards the backend mutation before the local install flow.
        if let Err(err) = crate::remote_legacy::enable_remote_plugin(
            &self.remote_plugin_service_config(config),
            auth,
            &plugin_id,
        )
        .await
        {
            let err = PluginInstallError::from(err);
            self.track_plugin_install_failed(
                &resolved.plugin_id,
                plugin_install_error_type(&err),
                err.sub_error_type(),
                err.to_string(),
            );
            return Err(err);
        }
        let plugin_id = resolved.plugin_id.clone();
        match self.install_resolved_plugin(resolved).await {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                self.track_plugin_install_failed(
                    &plugin_id,
                    plugin_install_error_type(&err),
                    err.sub_error_type(),
                    err.to_string(),
                );
                Err(err)
            }
        }
    }

    fn track_plugin_install_resolution_failed(&self, err: &MarketplaceError) {
        let sub_error_type = marketplace_error_sub_error_type(err);
        let plugin_id = match err {
            MarketplaceError::PluginNotFound {
                plugin_name,
                marketplace_name,
            }
            | MarketplaceError::PluginNotAvailable {
                plugin_name,
                marketplace_name,
            } => PluginId::new(plugin_name.clone(), marketplace_name.clone()).ok(),
            MarketplaceError::Io { .. }
            | MarketplaceError::MarketplaceNotFound { .. }
            | MarketplaceError::InvalidMarketplaceFile { .. }
            | MarketplaceError::PluginsDisabled
            | MarketplaceError::InvalidPlugin(_) => None,
        };
        if let Some(plugin_id) = plugin_id {
            self.track_plugin_install_failed(
                &plugin_id,
                marketplace_error_type(err),
                sub_error_type,
                err.to_string(),
            );
        } else {
            tracing::warn!(
                error_type = %marketplace_error_type(err),
                sub_error_type = sub_error_type.as_deref(),
                error = %err,
                "plugin install failed while resolving marketplace plugin"
            );
            self.emit_plugin_install_failed(
                PluginTelemetryMetadata {
                    plugin_id: None,
                    remote_plugin_id: None,
                    capability_summary: None,
                },
                marketplace_error_type(err),
                sub_error_type,
            );
        }
    }

    fn track_plugin_install_failed(
        &self,
        plugin_id: &PluginId,
        error_type: &'static str,
        sub_error_type: Option<String>,
        error_message: String,
    ) {
        tracing::warn!(
            plugin_id = %plugin_id.as_key(),
            error_type = %error_type,
            sub_error_type = sub_error_type.as_deref(),
            error = %error_message,
            "plugin install failed"
        );
        self.emit_plugin_install_failed(
            self.telemetry_metadata_for_plugin_id(plugin_id),
            error_type,
            sub_error_type,
        );
    }

    fn emit_plugin_install_failed(
        &self,
        plugin: PluginTelemetryMetadata,
        error_type: &'static str,
        sub_error_type: Option<String>,
    ) {
        let analytics_events_client = match self.analytics_events_client.read() {
            Ok(client) => client.clone(),
            Err(err) => err.into_inner().clone(),
        };
        if let Some(analytics_events_client) = analytics_events_client {
            analytics_events_client.track_plugin_install_failed(
                plugin,
                self.plugin_install_source,
                error_type.to_string(),
                sub_error_type,
            );
        }
    }

    async fn install_resolved_plugin(
        &self,
        resolved: ResolvedMarketplacePlugin,
    ) -> Result<PluginInstallOutcome, PluginInstallError> {
        let auth_policy = resolved.policy.authentication;
        let plugin_version =
            if is_openai_curated_marketplace_name(&resolved.plugin_id.marketplace_name) {
                let curated_plugin_version = read_curated_plugins_sha(self.codex_home.as_path())
                    .ok_or_else(|| {
                        PluginStoreError::Invalid(
                            "local curated marketplace sha is not available".to_string(),
                        )
                    })?;
                Some(curated_plugin_cache_version(&curated_plugin_version))
            } else {
                None
            };
        let store = self.store.clone();
        let codex_home = self.codex_home.clone();
        let manifest_fallback_contents = resolved
            .manifest_fallback
            .contents_if_has_metadata()
            .map(str::to_string);
        let result: StorePluginInstallResult = tokio::task::spawn_blocking(move || {
            let materialized =
                materialize_marketplace_plugin_source(codex_home.as_path(), &resolved.source)
                    .map_err(PluginStoreError::Invalid)?;
            let source_path = materialized.path;
            match (plugin_version, manifest_fallback_contents.as_deref()) {
                (Some(plugin_version), Some(manifest_contents)) => store
                    .install_with_version_and_fallback_manifest(
                        source_path,
                        resolved.plugin_id,
                        plugin_version,
                        manifest_contents,
                    ),
                (Some(plugin_version), None) => {
                    store.install_with_version(source_path, resolved.plugin_id, plugin_version)
                }
                (None, Some(manifest_contents)) => store.install_with_fallback_manifest(
                    source_path,
                    resolved.plugin_id,
                    manifest_contents,
                ),
                (None, None) => store.install(source_path, resolved.plugin_id),
            }
        })
        .await
        .map_err(PluginInstallError::join)??;

        set_user_plugin_enabled(
            &self.codex_home,
            result.plugin_id.as_key(),
            /*enabled*/ true,
        )
        .await
        .map_err(anyhow::Error::from)?;

        let analytics_events_client = match self.analytics_events_client.read() {
            Ok(client) => client.clone(),
            Err(err) => err.into_inner().clone(),
        };
        if let Some(analytics_events_client) = analytics_events_client {
            analytics_events_client.track_plugin_installed(
                self.telemetry_metadata_for_installed_plugin(&result.plugin_id)
                    .await,
            );
        }

        Ok(PluginInstallOutcome {
            plugin_id: result.plugin_id,
            plugin_version: result.plugin_version,
            installed_path: result.installed_path,
            auth_policy,
        })
    }

    pub async fn uninstall_plugin(&self, plugin_id: String) -> Result<(), PluginUninstallError> {
        let plugin_id = PluginId::parse(&plugin_id)?;
        self.uninstall_plugin_id(plugin_id).await
    }

    pub async fn uninstall_plugin_with_remote_sync(
        &self,
        config: &PluginsConfigInput,
        auth: Option<&CodexAuth>,
        plugin_id: String,
    ) -> Result<(), PluginUninstallError> {
        // TODO: Remove this legacy remote-sync path once remote plugins have
        // their own manager and installed-state API.
        let plugin_id = PluginId::parse(&plugin_id)?;
        let plugin_key = plugin_id.as_key();
        // This only forwards the backend mutation before the local uninstall flow.
        crate::remote_legacy::uninstall_remote_plugin(
            &self.remote_plugin_service_config(config),
            auth,
            &plugin_key,
        )
        .await
        .map_err(PluginUninstallError::from)?;
        self.uninstall_plugin_id(plugin_id).await
    }

    async fn uninstall_plugin_id(&self, plugin_id: PluginId) -> Result<(), PluginUninstallError> {
        let plugin_telemetry = if self.store.active_plugin_root(&plugin_id).is_some() {
            Some(
                self.telemetry_metadata_for_installed_plugin(&plugin_id)
                    .await,
            )
        } else {
            None
        };
        let store = self.store.clone();
        let plugin_id_for_store = plugin_id.clone();
        tokio::task::spawn_blocking(move || store.uninstall(&plugin_id_for_store))
            .await
            .map_err(PluginUninstallError::join)??;

        clear_user_plugin(&self.codex_home, plugin_id.as_key())
            .await
            .map_err(anyhow::Error::from)?;

        let analytics_events_client = match self.analytics_events_client.read() {
            Ok(client) => client.clone(),
            Err(err) => err.into_inner().clone(),
        };
        if let Some(plugin_telemetry) = plugin_telemetry
            && let Some(analytics_events_client) = analytics_events_client
        {
            analytics_events_client.track_plugin_uninstalled(plugin_telemetry);
        }

        Ok(())
    }
}
