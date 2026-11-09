//! Config loading for doctor plus the config check (config.toml, feature flags, startup warnings).

use super::*;

pub(super) fn config_load_failure(err: &anyhow::Error) -> DoctorCheck {
    let check = DoctorCheck::new(
        "config.load",
        "config",
        CheckStatus::Fail,
        "config could not be loaded",
    )
    .remediation("Fix the reported config error, then rerun codex doctor.");
    // Error messages can echo config values. Report only typed metadata,
    // including errors wrapped by io::Error, whose source skips the wrapper.
    let config_error = err.chain().find_map(|cause| {
        cause.downcast_ref::<ConfigLoadError>().or_else(|| {
            cause
                .downcast_ref::<std::io::Error>()?
                .get_ref()?
                .downcast_ref::<ConfigLoadError>()
        })
    });
    if let Some(error) = config_error {
        let error = error.config_error();
        return check
            .detail("error: invalid configuration")
            .detail(format!("file: {}", error.path.display()))
            .detail(format!("line: {}", error.range.start.line))
            .detail(format!("column: {}", error.range.start.column));
    }
    let io_error = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>());
    match io_error {
        Some(error) => check.detail(format!("error: {}", error.kind())),
        None => check.detail("error: configuration load failed"),
    }
}

pub(super) async fn load_config(
    mut root_config_overrides: CliConfigOverrides,
    interactive: &TuiCli,
    arg0_paths: &Arg0DispatchPaths,
) -> anyhow::Result<Config> {
    if interactive.web_search {
        root_config_overrides
            .raw_overrides
            .push("web_search=\"live\"".to_string());
    }

    let overrides = ConfigOverrides {
        ephemeral: Some(true),
        ..config_overrides_from_interactive(interactive, arg0_paths)
    };

    let config = crate::cloud_config::config_builder(
        &root_config_overrides,
        LoaderOverrides::default(),
        overrides,
    )
    .await?
    .build()
    .await
    .context("failed to load Codex config")?;
    config.config_layer_stack.ensure_config_server_applied()?;
    Ok(config)
}

pub(super) fn config_overrides_from_interactive(
    interactive: &TuiCli,
    arg0_paths: &Arg0DispatchPaths,
) -> ConfigOverrides {
    let approval_policy = if interactive.dangerously_bypass_approvals_and_sandbox {
        Some(AskForApproval::Never)
    } else {
        interactive.approval_policy.map(Into::into)
    };
    let sandbox_mode = if interactive.dangerously_bypass_approvals_and_sandbox {
        Some(codex_protocol::config_types::SandboxMode::DangerFullAccess)
    } else {
        interactive.sandbox_mode.map(Into::into)
    };
    ConfigOverrides {
        model: interactive.model.clone(),
        approval_policy,
        sandbox_mode,
        cwd: interactive.cwd.clone(),
        model_provider: interactive
            .oss
            .then(|| interactive.oss_provider.clone())
            .flatten(),
        codex_self_exe: arg0_paths.codex_self_exe.clone(),
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
        show_raw_agent_reasoning: interactive.oss.then_some(true),
        additional_writable_roots: interactive.add_dir.clone(),
        ..Default::default()
    }
}

fn display_list<T: AsRef<str>>(items: &[T]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub(super) fn config_check(config: &Config) -> DoctorCheck {
    let mut details = Vec::new();
    details
        .push("configuration scope: invocation config, including cloud-managed policy".to_string());
    details.push("active thread overrides: not inspected".to_string());
    details.push(format!("CODEX_HOME: {}", config.codex_home.display()));
    details.push(format!("cwd: {}", config.cwd.display()));
    details.push(format!(
        "model: {}",
        config.model.as_deref().unwrap_or("<default>")
    ));
    details.push(format!("model provider: {}", config.model_provider_id));
    details.push(format!(
        "configured TUI mode: {}",
        if config.tui_fullscreen_transcript {
            "fullscreen"
        } else {
            "scrollback"
        }
    ));
    details.push(format!("log dir: {}", config.log_dir.display()));
    details.push(format!(
        "sqlite home: {}",
        config.sqlite_config().home().display()
    ));
    details.push(format!("mcp servers: {}", config.mcp_servers.get().len()));
    feature_flag_details(config, &mut details);
    config_toml_details(config, &mut details);

    let status = if config.startup_warnings.is_empty() {
        CheckStatus::Ok
    } else {
        push_startup_warning_counts(&mut details, &config.startup_warnings);
        details.extend(
            config
                .startup_warnings
                .iter()
                .map(|warning| format!("startup warning: {warning}")),
        );
        CheckStatus::Warning
    };

    DoctorCheck::new("config.load", "config", status, "config loaded").details(details)
}

pub(super) fn push_startup_warning_counts(details: &mut Vec<String>, warnings: &[String]) {
    details.push(format!("startup warnings: {}", warnings.len()));
    for (label, needle) in [
        ("startup warning skills", "skill"),
        ("startup warning hooks", "hook"),
        ("startup warning plugins", "plugin"),
        ("startup warning MCP", "mcp"),
        ("startup warning deprecated", "deprecated"),
    ] {
        let count = warnings
            .iter()
            .filter(|warning| warning.to_ascii_lowercase().contains(needle))
            .count();
        details.push(format!("{label}: {count}"));
    }
}

fn feature_flag_details(config: &Config, details: &mut Vec<String>) {
    let features = config.features.get();
    let enabled_features = FEATURES
        .iter()
        .filter(|spec| features.enabled(spec.id))
        .map(|spec| spec.key)
        .collect::<Vec<_>>();
    let overrides = FEATURES
        .iter()
        .filter(|spec| features.enabled(spec.id) != spec.default_enabled)
        .map(|spec| format!("{}={}", spec.key, features.enabled(spec.id)))
        .collect::<Vec<_>>();
    details.push(format!("feature flags enabled: {}", enabled_features.len()));
    details.push(format!(
        "enabled feature flags: {}",
        display_list(&enabled_features)
    ));
    details.push(format!(
        "feature flag overrides: {}",
        display_list(&overrides)
    ));
    for usage in features.legacy_feature_usages() {
        details.push(format!(
            "legacy feature flag: {} -> {}",
            usage.alias,
            usage.feature.key()
        ));
    }
}

fn config_toml_details(config: &Config, details: &mut Vec<String>) {
    let config_path = config.codex_home.join(codex_config::CONFIG_TOML_FILE);
    details.push(format!("config.toml: {}", config_path.display()));
    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str::<toml::Value>(&contents) {
            Ok(_) => details.push("config.toml parse: ok".to_string()),
            Err(err) => details.push(format!("config.toml parse: {err}")),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            details.push("config.toml: missing".to_string());
        }
        Err(err) => details.push(format!("config.toml read: {err}")),
    }
}
