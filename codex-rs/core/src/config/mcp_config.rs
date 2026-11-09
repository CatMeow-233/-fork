//! Builds `McpConfig` from `Config` and applies managed MCP server requirements.

use super::*;

impl Config {
    /// Applies managed MCP requirements to servers supplied by one plugin.
    pub fn apply_plugin_mcp_server_requirements(
        &self,
        plugin_id: &str,
        mcp_servers: &mut HashMap<String, McpServerConfig>,
    ) {
        filter_plugin_mcp_servers_by_requirements(
            plugin_id,
            mcp_servers,
            self.config_layer_stack.requirements().plugins.as_ref(),
        );
        let empty_mcp_allowlist = self
            .config_layer_stack
            .requirements()
            .mcp_servers
            .as_ref()
            .filter(|requirements| requirements.value.is_empty());
        filter_mcp_servers_by_requirements(mcp_servers, empty_mcp_allowlist);
    }

    pub async fn to_mcp_config(
        &self,
        plugins_manager: &codex_core_plugins::PluginsManager,
    ) -> McpConfig {
        self.to_mcp_config_with_plugin_registrations(
            plugins_manager,
            std::iter::empty::<McpServerRegistration>(),
        )
        .await
    }

    pub(crate) async fn to_mcp_config_with_plugin_registrations(
        &self,
        plugins_manager: &codex_core_plugins::PluginsManager,
        additional_plugin_registrations: impl IntoIterator<Item = McpServerRegistration>,
    ) -> McpConfig {
        let plugins_input = self.plugins_config_input();
        let loaded_plugins = plugins_manager.plugins_for_config(&plugins_input).await;
        self.to_mcp_config_with_loaded_plugins(&loaded_plugins, additional_plugin_registrations)
    }

    pub(crate) fn to_mcp_config_with_loaded_plugins(
        &self,
        loaded_plugins: &PluginLoadOutcome,
        additional_plugin_registrations: impl IntoIterator<Item = McpServerRegistration>,
    ) -> McpConfig {
        let mut catalog = ResolvedMcpCatalog::builder();
        if self.features.enabled(Feature::UseXaa)
            && let Some(auth) = &self.mcp_enterprise_managed_auth
        {
            catalog.enable_ema(auth.idp.clone());
        }
        for (plugin_order, plugin) in loaded_plugins
            .plugins()
            .iter()
            .filter(|plugin| plugin.is_active())
            .enumerate()
        {
            let mut plugin_mcp_servers = plugin.mcp_servers.clone();
            self.apply_plugin_mcp_server_requirements(&plugin.config_name, &mut plugin_mcp_servers);
            let attribution = if plugin.is_agent_plugin() {
                McpPluginAttribution::agent_plugin(
                    plugin.config_name.clone(),
                    plugin.display_name().to_string(),
                )
            } else {
                McpPluginAttribution::new(
                    plugin.config_name.clone(),
                    plugin.display_name().to_string(),
                )
            }
            .with_host_root(PathUri::from_abs_path(&plugin.root));
            for (name, plugin_server) in plugin_mcp_servers {
                catalog.register(McpServerRegistration::from_plugin(
                    name,
                    attribution.clone(),
                    plugin_order,
                    plugin_server,
                ));
            }
        }
        for registration in additional_plugin_registrations {
            catalog.register(registration);
        }
        for (name, server) in self.mcp_servers.get() {
            catalog.register(McpServerRegistration::from_config(
                name.clone(),
                server.clone(),
            ));
        }

        McpConfig {
            chatgpt_base_url: self.chatgpt_base_url.clone(),
            apps_mcp_product_sku: self.apps_mcp_product_sku.clone(),
            requires_read_only_mcp_tools: false,
            codex_home: self.codex_home.to_path_buf(),
            mcp_enterprise_managed_auth: self.mcp_enterprise_managed_auth.clone(),
            ema_state: Default::default(),
            mcp_ema_cancellation: self.config_layer_stack.mcp_ema_cancellation().clone(),
            xaa_enabled: self.features.enabled(Feature::UseXaa)
                && self.mcp_enterprise_managed_auth.is_some(),
            mcp_oauth_credentials_store_mode: self.mcp_oauth_credentials_store_mode,
            mcp_tool_list_changed: self.features.enabled(Feature::McpToolListChanged),
            oauth_refresh_mode: if self.features.enabled(Feature::McpOAuthRefreshCoordination) {
                McpOAuthRefreshMode::Coordinated
            } else {
                McpOAuthRefreshMode::Legacy
            },
            auth_keyring_backend_kind: self.auth_keyring_backend_kind(),
            mcp_oauth_callback_port: self.mcp_oauth_callback_port,
            mcp_oauth_callback_url: self.mcp_oauth_callback_url.clone(),
            optional_mcp_startup_grace: self.mcp_optional_startup_grace,
            skill_mcp_dependency_install_enabled: self
                .features
                .enabled(Feature::SkillMcpDependencyInstall),
            approval_policy: self.permissions.approval_policy.clone(),
            permission_profile: self.permissions.permission_profile().clone(),
            host_cwd: self.cwd.clone(),
            config_layer_stack: self.config_layer_stack.clone(),
            plugins: self.plugins.clone(),
            approvals_reviewer: self.approvals_reviewer,
            environment_cwds: HashMap::new(),
            environment_use_mxc: HashMap::new(),
            server_permissions: HashMap::new(),
            codex_self_exe: self.codex_self_exe.clone(),
            codex_linux_sandbox_exe: self.codex_linux_sandbox_exe.clone(),
            use_legacy_landlock: self.features.use_legacy_landlock(),
            apps_enabled: self.features.enabled(Feature::Apps),
            apps_early_resource_discovery: self
                .features
                .enabled(Feature::AppsEarlyResourceDiscovery),
            prefix_mcp_tool_names: self.prefix_mcp_tool_names(),
            non_prefixed_mcp_tool_servers: if self
                .features
                .enabled(Feature::NonPrefixedMcpToolNames)
            {
                self.non_prefixed_mcp_tool_servers
                    .clone()
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            protocol_mode: self.mcp_protocol_mode(),
            discover_local_stdio: self.features.enabled(Feature::McpConnectionSharing),
            host_owned_apps_protocol_mode: if self.features.enabled(Feature::CodexAppsMcp20260728) {
                McpProtocolMode::V20260728
            } else {
                McpProtocolMode::Legacy
            },
            client_elicitation_capability: if self.features.enabled(Feature::AuthElicitation) {
                ElicitationCapability::new()
                    .with_form(FormElicitationCapability::new())
                    .with_url(UrlElicitationCapability::new())
            } else {
                // https://modelcontextprotocol.io/specification/2025-06-18/client/elicitation#capabilities
                // indicates this should be an empty object.
                ElicitationCapability::default()
            },
            mcp_server_catalog: catalog.build(),
            connector_snapshot:
                codex_connectors::ConnectorSnapshot::from_plugin_capability_summaries(
                    loaded_plugins.capability_summaries(),
                ),
        }
    }

    pub(crate) fn prefix_mcp_tool_names(&self) -> bool {
        !self.features.enabled(Feature::NonPrefixedMcpToolNames)
            || self.non_prefixed_mcp_tool_servers.is_some()
    }

    pub fn mcp_protocol_mode(&self) -> McpProtocolMode {
        if self.features.enabled(Feature::Mcp20260728) {
            McpProtocolMode::V20260728
        } else {
            McpProtocolMode::Legacy
        }
    }
}

pub(super) fn filter_mcp_servers_by_requirements(
    mcp_servers: &mut HashMap<String, McpServerConfig>,
    mcp_requirements: Option<&Sourced<BTreeMap<String, McpServerRequirement>>>,
) {
    let Some(allowlist) = mcp_requirements else {
        return;
    };

    let source = allowlist.source.clone();
    for (name, server) in mcp_servers.iter_mut() {
        let allowed = allowlist
            .value
            .get(name)
            .is_some_and(|requirement| server.matches_requirement(requirement));
        if allowed {
            server.disabled_reason = None;
        } else {
            server.disable_by_requirements(source.clone());
        }
    }
}

pub(super) fn filter_plugin_mcp_servers_by_requirements(
    plugin_config_name: &str,
    mcp_servers: &mut HashMap<String, McpServerConfig>,
    plugin_requirements: Option<&Sourced<BTreeMap<String, PluginRequirementsToml>>>,
) {
    let Some(requirements) = plugin_requirements else {
        return;
    };
    if !requirements
        .value
        .values()
        .any(|plugin| plugin.mcp_servers.is_some())
    {
        return;
    }
    let source = requirements.source.clone();
    let plugin_mcp_requirements = requirements
        .value
        .get(plugin_config_name)
        .and_then(|plugin| plugin.mcp_servers.as_ref());

    for (name, server) in mcp_servers.iter_mut() {
        let allowed = plugin_mcp_requirements
            .and_then(|mcp_requirements| mcp_requirements.get(name))
            .is_some_and(|requirement| server.matches_requirement(requirement));
        if allowed {
            server.disabled_reason = None;
        } else {
            server.disable_by_requirements(source.clone());
        }
    }
}

pub(super) fn constrain_mcp_servers(
    mcp_servers: HashMap<String, McpServerConfig>,
    mcp_requirements: Option<&Sourced<BTreeMap<String, McpServerRequirement>>>,
) -> ConstraintResult<Constrained<HashMap<String, McpServerConfig>>> {
    if mcp_requirements.is_none() {
        return Ok(Constrained::allow_any(mcp_servers));
    }

    let mcp_requirements = mcp_requirements.cloned();
    Constrained::normalized(mcp_servers, move |mut servers| {
        filter_mcp_servers_by_requirements(&mut servers, mcp_requirements.as_ref());
        servers
    })
}

pub async fn load_global_mcp_servers(
    codex_home: &Path,
) -> std::io::Result<BTreeMap<String, McpServerConfig>> {
    // In general, Config::load_with_cli_overrides() should be used to load the
    // full config with requirements.toml applied, but in this case, we need
    // access to the raw TOML in order to warn the user about deprecated fields.
    //
    // Note that a more precise way to do this would be to audit the individual
    // config layers for deprecated fields rather than reporting on the merged
    // result.
    let cli_overrides = Vec::<(String, TomlValue)>::new();
    // There is no cwd/project context for this query, so this will not include
    // MCP servers defined in in-repo .codex/ folders.
    let cwd: Option<AbsolutePathBuf> = None;
    let config_layer_stack = load_config_layers_state(
        LOCAL_FS.as_ref(),
        codex_home,
        cwd,
        &cli_overrides,
        LoaderOverrides::default(),
        &codex_config::NoopThreadConfigLoader,
    )
    .await?;
    let merged_toml = config_layer_stack.effective_config();
    let Some(servers_value) = merged_toml.get("mcp_servers") else {
        return Ok(BTreeMap::new());
    };

    ensure_no_inline_bearer_tokens(servers_value)?;

    servers_value
        .clone()
        .try_into()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// We briefly allowed plain text bearer_token fields in MCP server configs.
/// We want to warn people who recently added these fields but can remove this after a few months.
fn ensure_no_inline_bearer_tokens(value: &TomlValue) -> std::io::Result<()> {
    let Some(servers_table) = value.as_table() else {
        return Ok(());
    };

    for (server_name, server_value) in servers_table {
        if let Some(server_table) = server_value.as_table()
            && server_table.contains_key("bearer_token")
        {
            let message = format!(
                "mcp_servers.{server_name} uses unsupported `bearer_token`; set `bearer_token_env_var`."
            );
            return Err(std::io::Error::new(ErrorKind::InvalidData, message));
        }
    }

    Ok(())
}
