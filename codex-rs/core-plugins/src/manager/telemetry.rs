//! Plugin telemetry metadata for installed plugins and capability summaries.

use super::*;

impl PluginsManager {
    pub async fn telemetry_metadata_for_installed_plugin(
        &self,
        plugin_id: &PluginId,
    ) -> PluginTelemetryMetadata {
        let mut metadata = self.telemetry_metadata_for_plugin_id(plugin_id);
        metadata.capability_summary = match self.store.active_plugin_root(plugin_id) {
            Some(plugin_root) => {
                plugin_capability_summary_from_root(
                    plugin_id,
                    &plugin_root,
                    self.skill_root_loader.as_ref(),
                    &self.store.manifest_cache,
                )
                .await
            }
            None => None,
        };
        metadata
    }

    pub async fn telemetry_metadata_for_installed_plugin_with_remote_id(
        &self,
        plugin_id: &PluginId,
        remote_plugin_id: &str,
    ) -> PluginTelemetryMetadata {
        let mut metadata =
            self.telemetry_metadata_for_plugin_id_with_remote_id(plugin_id, remote_plugin_id);
        metadata.capability_summary = match self.store.active_plugin_root(plugin_id) {
            Some(plugin_root) => {
                plugin_capability_summary_from_root(
                    plugin_id,
                    &plugin_root,
                    self.skill_root_loader.as_ref(),
                    &self.store.manifest_cache,
                )
                .await
            }
            None => None,
        };
        metadata
    }

    pub fn telemetry_metadata_for_plugin_id(
        &self,
        plugin_id: &PluginId,
    ) -> PluginTelemetryMetadata {
        PluginTelemetryMetadata {
            plugin_id: Some(plugin_id.clone()),
            remote_plugin_id: self.remote_plugin_id_for(plugin_id),
            capability_summary: None,
        }
    }

    pub fn telemetry_metadata_for_plugin_id_with_remote_id(
        &self,
        plugin_id: &PluginId,
        remote_plugin_id: &str,
    ) -> PluginTelemetryMetadata {
        PluginTelemetryMetadata {
            remote_plugin_id: Some(remote_plugin_id.to_string()),
            ..self.telemetry_metadata_for_plugin_id(plugin_id)
        }
    }

    pub fn telemetry_metadata_for_capability_summary(
        &self,
        summary: &PluginCapabilitySummary,
    ) -> Option<PluginTelemetryMetadata> {
        let plugin_id = PluginId::parse(&summary.config_name).ok()?;
        Some(PluginTelemetryMetadata {
            remote_plugin_id: self.remote_plugin_id_for(&plugin_id),
            plugin_id: Some(plugin_id),
            capability_summary: Some(summary.clone()),
        })
    }
}
