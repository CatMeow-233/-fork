//! Runs the individual doctor checks and assembles them into a `DoctorReport`.

use super::*;

pub(super) async fn build_report(
    command: &DoctorCommand,
    root_config_overrides: CliConfigOverrides,
    interactive: &TuiCli,
    arg0_paths: &Arg0DispatchPaths,
) -> DoctorReport {
    let progress = doctor_progress(command.json);
    if command.network_only {
        let checks =
            network::checks(root_config_overrides, interactive, arg0_paths, &progress).await;
        progress.settle();
        return report_from_checks(checks);
    }
    let mut checks = Vec::new();
    checks.push(run_sync_check("system", progress.clone(), system_check));
    checks.push(run_async_check("endpoint protection", progress.clone(), security::check()).await);
    checks.push(run_sync_check("installation", progress.clone(), || {
        installation_check(!command.summary)
    }));
    checks.push(run_sync_check("runtime", progress.clone(), runtime_check));
    checks.push(run_sync_check("search", progress.clone(), search_check));

    progress.begin("config");
    let config_started = Instant::now();
    let config_result = load_config(root_config_overrides, interactive, arg0_paths).await;
    let config_duration = config_started.elapsed();
    let cwd = config_result
        .as_ref()
        .map(|config| config.cwd.as_path().to_path_buf())
        .unwrap_or_else(|_| {
            let mut cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            if let Some(requested_cwd) = &interactive.cwd {
                cwd.push(requested_cwd);
            }
            cwd
        });
    checks.push(run_sync_check("disk", progress.clone(), || {
        disk::check(config_result.as_ref().ok(), &cwd)
    }));
    #[cfg(target_os = "windows")]
    checks.push(run_sync_check("dev drive", progress.clone(), || {
        windows_dev_drive::check(&cwd)
    }));
    match &config_result {
        Ok(config) => {
            // Other checks below do synchronous work inside join!. Keep the
            // probe deadlines independent of those checks' scheduler delays.
            let filesystem_paths_check = run_async_check(
                "filesystem paths",
                progress.clone(),
                filesystem_paths::check(config),
            )
            .await;
            let auth_manager_result =
                AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ true).await;
            let auth_manager = auth_manager_result.as_ref().ok().cloned();
            let reachability_plan = provider_reachability_plan(config);
            #[cfg(target_os = "macos")]
            let reachability_url = reachability_plan
                .endpoints
                .first()
                .map(|endpoint| endpoint.url.clone());
            let (
                config_check,
                auth_check,
                updates_check,
                network_check,
                websocket_check,
                mcp_check,
                sandbox_check,
                terminal_check,
                git_check,
                terminal_title_check,
                state_check,
                thread_inventory_check,
                background_server_check,
                reachability_check,
            ) = tokio::join!(
                async {
                    run_sync_check("config", progress.clone(), || {
                        config_check(config).detail(format!(
                            "configuration load ms: {}",
                            config_duration.as_millis()
                        ))
                    })
                },
                async {
                    run_sync_check("auth", progress.clone(), || match &auth_manager_result {
                        Ok(_) => auth_check(config),
                        Err(error) => DoctorCheck::new(
                            "auth.load",
                            "auth",
                            CheckStatus::Fail,
                            "authentication could not be initialized",
                        )
                        .detail(error.to_string())
                        .remediation(
                            "Fix the reported authentication error, then rerun codex doctor.",
                        ),
                    })
                },
                run_async_check("updates", progress.clone(), updates_check(config)),
                async {
                    run_sync_check("network", progress.clone(), || network::check(Some(config)))
                },
                run_async_check(
                    "websocket",
                    progress.clone(),
                    websocket_reachability_check(config, auth_manager),
                ),
                run_async_check("MCP", progress.clone(), mcp_check(config)),
                async {
                    run_sync_check("sandbox", progress.clone(), || {
                        sandbox_check(config, arg0_paths)
                    })
                },
                async {
                    run_sync_check("terminal", progress.clone(), || {
                        terminal_check(command.no_color)
                    })
                },
                async {
                    run_sync_check("git", progress.clone(), || git_check(config.cwd.as_path()))
                },
                async {
                    run_sync_check("terminal title", progress.clone(), || {
                        terminal_title_check(config)
                    })
                },
                run_async_check("state", progress.clone(), state_check(config, command)),
                run_async_check(
                    "thread inventory",
                    progress.clone(),
                    thread_inventory_check(config),
                ),
                run_async_check(
                    "app-server",
                    progress.clone(),
                    background_server_check(config)
                ),
                run_async_check(
                    "provider reachability",
                    progress.clone(),
                    provider_reachability_check(reachability_plan),
                ),
            );
            #[cfg(target_os = "macos")]
            let reachability_check = network::with_system_proxy_remediation(
                reachability_check,
                config,
                reachability_url.as_deref(),
            );
            checks.extend([
                config_check,
                auth_check,
                updates_check,
                network_check,
                websocket_check,
                mcp_check,
                sandbox_check,
                filesystem_paths_check,
                terminal_check,
                git_check,
                terminal_title_check,
                state_check,
                thread_inventory_check,
                background_server_check,
                reachability_check,
            ]);
        }
        Err(err) => {
            let reachability_plan = default_reachability_plan();
            let (
                config_check,
                network_check,
                terminal_check,
                git_check,
                state_check,
                reachability_check,
            ) = tokio::join!(
                async { run_sync_check("config", progress.clone(), || config_load_failure(err)) },
                async {
                    run_sync_check("network", progress.clone(), || {
                        network::check(/*config*/ None)
                    })
                },
                async {
                    run_sync_check("terminal", progress.clone(), || {
                        terminal_check(command.no_color)
                    })
                },
                async { run_sync_check("git", progress.clone(), || git_check(&cwd)) },
                async { run_sync_check("state", progress.clone(), fallback_state_check) },
                run_async_check(
                    "provider reachability",
                    progress.clone(),
                    provider_reachability_check(reachability_plan),
                ),
            );
            checks.extend([
                config_check,
                network_check,
                terminal_check,
                git_check,
                state_check,
                reachability_check,
            ]);
        }
    }

    progress.begin("desktop");
    if let Some(desktop) = desktop::collect().await {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(application) = desktop.application.as_ref() {
            updates::append_desktop_update(&mut checks, config_result.as_ref().ok(), application)
                .await;
        }
        progress.finish("desktop", overall_status(&desktop.checks));
        checks.extend(desktop.checks);
    }

    progress.settle();

    report_from_checks(checks)
}

fn report_from_checks(checks: Vec<DoctorCheck>) -> DoctorReport {
    let overall_status = overall_status(&checks);
    DoctorReport {
        schema_version: 1,
        generated_at: generated_at(),
        overall_status,
        codex_version: env!("CARGO_PKG_VERSION").to_string(),
        checks,
    }
}

pub(super) fn run_sync_check(
    label: &'static str,
    progress: Arc<dyn DoctorProgress>,
    f: impl FnOnce() -> DoctorCheck,
) -> DoctorCheck {
    progress.begin(label);
    let start = Instant::now();
    let mut check = f();
    check.duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    progress.finish(label, check.status);
    check
}

pub(super) async fn run_async_check<Fut>(
    label: &'static str,
    progress: Arc<dyn DoctorProgress>,
    future: Fut,
) -> DoctorCheck
where
    Fut: Future<Output = DoctorCheck>,
{
    progress.begin(label);
    let start = Instant::now();
    tokio::pin!(future);
    let mut progress_interval = tokio::time::interval(SLOW_CHECK_PROGRESS_INTERVAL);
    loop {
        tokio::select! {
            mut check = &mut future => {
                check.duration_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                progress.finish(label, check.status);
                return check;
            }
            _ = progress_interval.tick() => {
                let elapsed = start.elapsed();
                if elapsed >= SLOW_CHECK_PROGRESS_THRESHOLD {
                    progress.heartbeat(label, elapsed);
                }
            }
        }
    }
}

pub(super) fn overall_status(checks: &[DoctorCheck]) -> CheckStatus {
    if checks.iter().any(|check| check.status == CheckStatus::Fail) {
        CheckStatus::Fail
    } else if checks
        .iter()
        .any(|check| check.status == CheckStatus::Warning)
    {
        CheckStatus::Warning
    } else {
        CheckStatus::Ok
    }
}

fn generated_at() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => {
            let seconds = duration.as_secs();
            format!("{seconds}s since unix epoch")
        }
        Err(_) => "unknown".to_string(),
    }
}
