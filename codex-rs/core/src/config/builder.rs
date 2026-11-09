//! `ConfigBuilder` and the `Config::load_*` entry points that drive layered config loading.

use super::*;

impl ConfigBuilder {
    pub fn codex_home(mut self, codex_home: PathBuf) -> Self {
        self.codex_home = Some(codex_home);
        self
    }

    pub fn cli_overrides(mut self, cli_overrides: Vec<(String, TomlValue)>) -> Self {
        self.cli_overrides = Some(cli_overrides);
        self
    }

    pub fn harness_overrides(mut self, harness_overrides: ConfigOverrides) -> Self {
        self.harness_overrides = Some(harness_overrides);
        self
    }

    pub fn loader_overrides(mut self, loader_overrides: LoaderOverrides) -> Self {
        self.loader_overrides = Some(loader_overrides);
        self
    }

    pub fn strict_config(mut self, strict_config: bool) -> Self {
        self.strict_config = strict_config;
        self
    }

    pub fn cloud_config_bundle(mut self, cloud_config_bundle: CloudConfigBundleLoader) -> Self {
        self.cloud_config_bundle = cloud_config_bundle;
        self
    }

    pub fn thread_config_loader(
        mut self,
        thread_config_loader: Arc<dyn ThreadConfigLoader>,
    ) -> Self {
        self.thread_config_loader = Some(thread_config_loader);
        self
    }

    pub fn fallback_cwd(mut self, fallback_cwd: Option<PathBuf>) -> Self {
        self.fallback_cwd = fallback_cwd;
        self
    }

    /// Materializes `Config.cwd` without using it as config-layer context.
    pub fn without_project_context(mut self) -> Self {
        self.without_project_context = true;
        self
    }

    pub async fn build(self) -> std::io::Result<Config> {
        // Keep the large config-loading future off small runtime thread stacks.
        // Avoid an additional async frame for ordinary config loads.
        Box::pin(self.build_inner(|_, _, _| Ok(()))).await
    }

    /// Validates the same loaded configuration that will be materialized, before provider
    /// selection or initialization. Validation cannot alter the loaded configuration.
    pub async fn build_with_validation(
        self,
        validate: impl FnOnce(&ConfigToml, &ConfigOverrides, &ConfigLayerStack) -> std::io::Result<()>,
    ) -> std::io::Result<Config> {
        Box::pin(self.build_inner(validate)).await
    }

    async fn build_inner(
        self,
        validate: impl FnOnce(&ConfigToml, &ConfigOverrides, &ConfigLayerStack) -> std::io::Result<()>,
    ) -> std::io::Result<Config> {
        let Self {
            codex_home,
            cli_overrides,
            harness_overrides,
            loader_overrides,
            strict_config,
            cloud_config_bundle,
            thread_config_loader,
            fallback_cwd,
            without_project_context,
        } = self;
        let codex_home = match codex_home {
            Some(codex_home) => AbsolutePathBuf::from_absolute_path(codex_home)?,
            None => find_codex_home()?,
        };
        let cli_overrides = cli_overrides.unwrap_or_default();
        let mut harness_overrides = harness_overrides.unwrap_or_default();
        let loader_overrides = loader_overrides.unwrap_or_default();
        let cwd_override = harness_overrides
            .cwd
            .as_deref()
            .or(fallback_cwd.as_deref())
            .or(loader_overrides.default_cwd.as_deref());
        let cwd = match cwd_override {
            Some(path) => AbsolutePathBuf::relative_to_current_dir(path)?,
            None => AbsolutePathBuf::current_dir()?,
        };
        harness_overrides.cwd = Some(cwd.to_path_buf());
        let config_layer_stack = load_config_layers_state(
            LOCAL_FS.as_ref(),
            &codex_home,
            (!without_project_context).then_some(cwd),
            &cli_overrides,
            ConfigLoadOptions {
                loader_overrides,
                strict_config,
                cloud_config_bundle,
            },
            thread_config_loader
                .as_deref()
                .unwrap_or(&codex_config::NoopThreadConfigLoader),
        )
        .await?;
        let config_toml = config_toml_from_layers(&config_layer_stack).await?;
        validate(&config_toml, &harness_overrides, &config_layer_stack)?;
        Config::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            config_toml,
            harness_overrides,
            codex_home,
            config_layer_stack,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn without_managed_config_for_tests() -> Self {
        Self::default().loader_overrides(LoaderOverrides::without_managed_config_for_tests())
    }
}

async fn config_toml_from_layers(layers: &ConfigLayerStack) -> std::io::Result<ConfigToml> {
    // The loader resolves paths relative to each layer's file before deserialization.
    match ConfigToml::try_from(layers.effective_config()) {
        Ok(config_toml) => Ok(config_toml),
        Err(err) => {
            if let Some(config_error) = codex_config::first_layer_config_error::<ConfigToml>(
                layers,
                codex_config::CONFIG_TOML_FILE,
            )
            .await
            {
                return Err(codex_config::io_error_from_config_error(
                    std::io::ErrorKind::InvalidData,
                    config_error,
                    Some(err),
                ));
            }
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err))
        }
    }
}

impl Config {
    pub async fn rebuild_with_session_layers(
        session_layers: &ConfigLayerStack,
        cwd: PathBuf,
        refreshed_layers: &ConfigLayerStack,
        codex_home: AbsolutePathBuf,
    ) -> std::io::Result<Self> {
        let config_layer_stack =
            Self::layer_stack_preserving_session(session_layers, refreshed_layers)?;
        let cfg = config_toml_from_layers(&config_layer_stack).await?;
        Self::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            cfg,
            ConfigOverrides {
                cwd: Some(cwd),
                ..Default::default()
            },
            codex_home,
            config_layer_stack,
        )
        .await
    }

    pub(super) fn layer_stack_preserving_session(
        session_layers: &ConfigLayerStack,
        refreshed_layers: &ConfigLayerStack,
    ) -> std::io::Result<ConfigLayerStack> {
        let mut layers = refreshed_layers
            .all_layers_low_to_high()
            .filter(|layer| !is_session_layer(&layer.name))
            .cloned()
            .collect::<Vec<_>>();
        layers.extend(
            session_layers
                .all_layers_low_to_high()
                .filter(|layer| is_session_layer(&layer.name))
                .cloned(),
        );
        layers.sort_by_key(|layer| layer.name.precedence());

        Ok(ConfigLayerStack::new(
            layers,
            refreshed_layers.requirements().clone(),
            refreshed_layers.requirements_toml().clone(),
        )?
        .with_cloud_config_binding(refreshed_layers.cloud_config_binding().cloned())
        .with_applied_config_server(refreshed_layers.applied_config_server().cloned())
        .with_user_and_project_exec_policy_rules_ignored(
            refreshed_layers.ignore_user_and_project_exec_policy_rules(),
        ))
    }

    /// This is the preferred way to create an instance of [Config].
    pub async fn load_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        ConfigBuilder::default()
            .cli_overrides(cli_overrides)
            .build()
            .await
    }

    /// Load a default configuration when user config files are invalid.
    pub async fn load_default_with_cli_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        let codex_home = find_codex_home()?;
        Self::load_default_with_cli_overrides_for_codex_home(
            codex_home.to_path_buf(),
            cli_overrides,
        )
        .await
    }

    /// Load a default configuration for a specific Codex home without reading
    /// user, project, or system config layers.
    pub async fn load_default_with_cli_overrides_for_codex_home(
        codex_home: PathBuf,
        cli_overrides: Vec<(String, TomlValue)>,
    ) -> std::io::Result<Self> {
        let mut merged = toml::Value::try_from(ConfigToml::default()).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to serialize default config: {e}"),
            )
        })?;
        let cli_layer = codex_config::build_cli_overrides_layer(&cli_overrides);
        codex_config::merge_toml_values(&mut merged, &cli_layer);
        let codex_home = AbsolutePathBuf::from_absolute_path_checked(codex_home)?;
        let config_toml = deserialize_config_toml_with_base(merged, &codex_home)?;
        Self::load_config_with_layer_stack(
            LOCAL_FS.as_ref(),
            config_toml,
            ConfigOverrides::default(),
            codex_home,
            ConfigLayerStack::default(),
        )
        .await
    }
    /// This is a secondary way of creating [Config], which is appropriate when
    /// the harness is meant to be used with a specific configuration that
    /// ignores user settings. For example, the `codex exec` subcommand is
    /// designed to use [AskForApproval::Never] exclusively.
    ///
    /// Further, [ConfigOverrides] contains some options that are not supported
    /// in [ConfigToml], such as `cwd`, `codex_self_exe`, and `codex_linux_sandbox_exe`.
    pub async fn load_with_cli_overrides_and_harness_overrides(
        cli_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
    ) -> std::io::Result<Self> {
        ConfigBuilder::default()
            .cli_overrides(cli_overrides)
            .harness_overrides(harness_overrides)
            .build()
            .await
    }
}

fn is_session_layer(source: &ConfigLayerSource) -> bool {
    matches!(source, ConfigLayerSource::SessionFlags)
}

/// Returns the path to the Codex configuration directory, which can be
/// specified by the `CODEX_HOME` environment variable. If not set, defaults to
/// `~/.codex`.
///
/// - If `CODEX_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `CODEX_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    codex_utils_home_dir::find_codex_home()
}
