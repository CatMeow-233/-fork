//! Fresh threads use the destination server's model defaults on every connection.
//! Explicit profile launches carry the profile's resolved settings into the first request.

use super::*;
use crate::app_server_session::StartupLaunchChoices;
use crate::config_update::EffectiveConfig;
use codex_app_server_protocol::GetAccountResponse;

pub(crate) fn uses_server_owned_fresh_bootstrap(
    selection: &SessionSelection,
    loader_overrides: &LoaderOverrides,
) -> bool {
    matches!(
        selection,
        SessionSelection::StartFresh | SessionSelection::Exit
    ) && loader_overrides.user_config_profile.is_none()
}

/// The cwd whose server defaults a fresh start reads before its first thread.
pub(crate) fn fresh_startup_defaults_cwd<'a>(
    config: &'a Config,
    app_server: &'a AppServerSession,
    app_server_target: &AppServerTarget,
) -> &'a Path {
    match app_server_target {
        AppServerTarget::Embedded | AppServerTarget::LocalDaemon { .. } => config.cwd.as_path(),
        AppServerTarget::Remote { .. } | AppServerTarget::Cloud => {
            app_server.remote_cwd_override().unwrap_or(Path::new("."))
        }
    }
}

/// Server defaults read for a fresh start before `App::run`.
pub(crate) struct PrefetchedStartupDefaults {
    cwd: PathBuf,
    defaults: EffectiveConfig,
}

impl PrefetchedStartupDefaults {
    pub(crate) fn new(cwd: PathBuf, defaults: EffectiveConfig) -> Self {
        Self { cwd, defaults }
    }

    /// Returns the defaults only when they answer the same request for `cwd`.
    pub(super) fn for_cwd(self, cwd: &Path) -> Option<EffectiveConfig> {
        (self.cwd.as_os_str() == cwd.as_os_str()).then_some(self.defaults)
    }
}

/// Startup reads that `App::run` may reuse instead of repeating them.
///
/// Callers leave these empty when an interactive screen, a server replacement, or a provider
/// change happened after the read.
#[derive(Default)]
pub(crate) struct StartupReads {
    /// Used only by the server-owned fresh bootstrap.
    pub(crate) account: Option<GetAccountResponse>,
    pub(crate) defaults: Option<PrefetchedStartupDefaults>,
}

pub(super) async fn bootstrap_server_owned_start(
    app_server: &mut AppServerSession,
    config: &mut Config,
    launch_choices: &mut StartupLaunchChoices,
    cli_kv_overrides: &[(String, TomlValue)],
    harness_overrides: &ConfigOverrides,
    account: Option<GetAccountResponse>,
) -> Result<AppServerBootstrap> {
    let new_thread_defaults = launch_choices.model.is_none() && launch_choices.effort.is_none();
    let bootstrap = match (new_thread_defaults, account) {
        (true, Some(account)) => {
            app_server
                .bootstrap_for_new_thread_with_account(config, account)
                .await
        }
        (true, None) => app_server.bootstrap_for_new_thread(config).await,
        (false, Some(account)) => app_server.bootstrap_with_account(config, account).await,
        (false, None) => app_server.bootstrap(config).await,
    }?;
    let previous = (config.model.clone(), config.model_reasoning_effort.clone());
    apply_managed_new_thread_defaults(
        config,
        app_server.managed_new_thread_defaults(),
        cli_kv_overrides,
        harness_overrides,
    );
    if (config.model.clone(), config.model_reasoning_effort.clone()) != previous {
        launch_choices.model = config.model.clone();
        launch_choices.effort = config
            .model_reasoning_effort
            .as_ref()
            .map(|effort| serde_json::json!(effort));
    }
    Ok(bootstrap)
}
