//! MCP check: local validation of configured MCP servers plus bounded HTTP reachability probes.

use super::*;

pub(super) async fn mcp_check(config: &Config) -> DoctorCheck {
    mcp_check_from_servers(config.mcp_servers.get()).await
}

pub(super) async fn mcp_check_from_servers(
    servers: &HashMap<String, McpServerConfig>,
) -> DoctorCheck {
    if servers.is_empty() {
        return DoctorCheck::new(
            "mcp.config",
            "mcp",
            CheckStatus::Ok,
            "no MCP servers configured",
        );
    }

    let mut details = Vec::new();
    let mut transport_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut disabled = 0usize;
    let mut invalid_config = Vec::new();
    let mut missing_env = Vec::new();
    let mut unreachable_required_http = Vec::new();
    let mut unreachable_optional_http = Vec::new();

    for (name, server) in servers {
        let disabled_server = !server.enabled || server.disabled_reason.is_some();
        if disabled_server {
            disabled += 1;
        }
        match &server.transport {
            McpServerTransportConfig::Stdio {
                command,
                env,
                env_vars,
                cwd,
                ..
            } => {
                *transport_counts.entry("stdio").or_default() += 1;
                if disabled_server {
                    continue;
                }
                let command_is_empty = command.trim().is_empty();
                if command_is_empty {
                    invalid_config.push(format!("{name}: stdio command is empty"));
                }
                if server.is_local_environment() {
                    let host_native_cwd = cwd.as_ref().map(|cwd| Path::new(cwd.as_str()));
                    if let Some(cwd) = host_native_cwd
                        && !cwd.exists()
                    {
                        invalid_config
                            .push(format!("{name}: cwd does not exist ({})", cwd.display()));
                    }
                    if !command_is_empty
                        && let Err(err) =
                            stdio_command_resolves(command, host_native_cwd, env.as_ref())
                    {
                        invalid_config.push(format!(
                            "{name}: stdio command {command:?} is not resolvable ({err})"
                        ));
                    }
                } else {
                    match cwd {
                        Some(cwd) if cwd.to_inferred_path_uri().is_none() => {
                            invalid_config
                                .push(format!("{name}: remote stdio cwd is not absolute ({cwd})"));
                        }
                        None => invalid_config
                            .push(format!("{name}: remote stdio requires an explicit cwd")),
                        Some(_) => {}
                    }
                }
                if let Some(env) = env {
                    for key in env.keys().filter(|key| key.trim().is_empty()) {
                        invalid_config.push(format!("{name}: empty env key {key}"));
                    }
                }
                for env_var in env_vars {
                    if env_var.is_remote_source() {
                        if server.is_local_environment() {
                            invalid_config.push(format!(
                                "{name}: env_vars entry `{}` uses source `remote`, which requires remote MCP stdio",
                                env_var.name()
                            ));
                        }
                    } else if !env_var_present(env_var.name()) {
                        missing_env.push(format!("{name}: env var {} is not set", env_var.name()));
                    }
                }
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                bearer_token_env_var,
                env_http_headers,
                ..
            } => {
                *transport_counts.entry("streamable_http").or_default() += 1;
                if disabled_server {
                    continue;
                }
                if let Some(env_var) = bearer_token_env_var
                    && !env_var_present(env_var)
                {
                    missing_env.push(format!("{name}: bearer token env var {env_var} is not set"));
                }
                if let Some(headers) = env_http_headers {
                    for env_var in headers.values() {
                        if !env_var_present(env_var) {
                            missing_env
                                .push(format!("{name}: header env var {env_var} is not set"));
                        }
                    }
                }
                if let Err(err) = mcp_http_probe_url(url).await {
                    let detail = format!("{name}: {url} ({err})");
                    if server.required {
                        unreachable_required_http.push(detail);
                    } else {
                        unreachable_optional_http.push(detail);
                    }
                }
            }
        }
    }

    details.push(format!("configured servers: {}", servers.len()));
    details.push(format!("disabled servers: {disabled}"));
    for (transport, count) in transport_counts {
        details.push(format!("{transport} servers: {count}"));
    }
    let mut hints = Vec::new();
    if !invalid_config.is_empty() {
        hints.push("Correct the listed MCP server configuration errors.");
    }
    if !missing_env.is_empty() {
        hints.push("Set the missing MCP env vars.");
    }
    if !unreachable_required_http.is_empty() || !unreachable_optional_http.is_empty() {
        hints.push("Check that the MCP server is running and its URL is reachable.");
    }
    let required_missing = servers.iter().any(|(name, server)| {
        server.required
            && invalid_config
                .iter()
                .chain(&missing_env)
                .any(|detail| detail.starts_with(&format!("{name}:")))
    });
    details.extend(invalid_config);
    details.extend(missing_env);
    details.extend(
        unreachable_required_http
            .iter()
            .map(|detail| format!("required reachability failed: {detail}")),
    );
    details.extend(
        unreachable_optional_http
            .iter()
            .map(|detail| format!("optional reachability failed: {detail}")),
    );

    let status = if required_missing || !unreachable_required_http.is_empty() {
        CheckStatus::Fail
    } else if !hints.is_empty() {
        CheckStatus::Warning
    } else {
        CheckStatus::Ok
    };
    let summary = match status {
        CheckStatus::Ok => "MCP configuration is locally consistent",
        CheckStatus::Warning => "MCP configuration has optional issues",
        CheckStatus::Fail => "MCP configuration has failing required inputs or reachability",
    };

    let mut check = DoctorCheck::new("mcp.config", "mcp", status, summary).details(details);
    if status != CheckStatus::Ok {
        hints.push("Alternatively, disable the affected server.");
        check = check.remediation(hints.join(" "));
    }
    check
}

async fn mcp_http_probe_url(url: &str) -> Result<String, String> {
    mcp_http_probe_url_with_timeout(url, Duration::from_secs(3)).await
}

pub(super) async fn mcp_http_probe_url_with_timeout(
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
    match http_probe_url_with_timeout(url, timeout).await {
        Ok(status) => Ok(status),
        Err(head_err) => match http_get_probe_url_with_timeout(url, timeout).await {
            Ok(status) => Ok(status),
            Err(get_err) => Err(format!("HEAD {head_err}; GET {get_err}")),
        },
    }
}

pub(super) async fn http_probe_url_with_timeout(
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
    let response = create_client_without_request_logging()
        .head(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                "request timed out".to_string()
            } else if err.is_connect() {
                "connect failed".to_string()
            } else if err.is_builder() {
                "request could not be built".to_string()
            } else {
                err.to_string()
            }
        })?;
    Ok(format!("HTTP {}", response.status().as_u16()))
}

async fn http_get_probe_url_with_timeout(url: &str, timeout: Duration) -> Result<String, String> {
    http_get_probe_status_with_timeout(url, timeout)
        .await
        .map(|status| format!("HTTP {status}"))
}

async fn http_get_probe_status_with_timeout(url: &str, timeout: Duration) -> Result<u16, String> {
    let response = create_client_without_request_logging()
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| {
            if err.is_timeout() {
                "request timed out".to_string()
            } else if err.is_connect() {
                "connect failed".to_string()
            } else if err.is_builder() {
                "request could not be built".to_string()
            } else {
                err.to_string()
            }
        })?;
    Ok(response.status().as_u16())
}

fn stdio_command_resolves(
    command: &str,
    cwd: Option<&Path>,
    server_env: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    let command_path = Path::new(command);
    if command_path.is_absolute() {
        return executable_path_exists(command_path);
    }

    if command_path.components().count() > 1 {
        let base = cwd
            .map(Path::to_path_buf)
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        return executable_path_exists(&base.join(command_path));
    }

    let Some(path_env) = server_env
        .and_then(|env| env.get("PATH").map(String::as_str))
        .map(std::ffi::OsString::from)
        .or_else(|| env::var_os("PATH"))
    else {
        return Err("PATH is not set".to_string());
    };

    for dir in env::split_paths(&path_env) {
        let candidate = dir.join(command);
        if executable_path_exists(&candidate).is_ok() {
            return Ok(());
        }
        #[cfg(windows)]
        {
            let pathext = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
            for extension in pathext.split(';').filter(|extension| !extension.is_empty()) {
                let candidate = dir.join(format!("{command}{extension}"));
                if executable_path_exists(&candidate).is_ok() {
                    return Ok(());
                }
            }
        }
    }
    Err("not found on PATH".to_string())
}

pub(super) fn executable_path_exists(path: &Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => executable_file_permission(path, &metadata),
        Ok(_) => Err("path is not a file".to_string()),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(unix)]
fn executable_file_permission(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o111 == 0 {
        Err(format!("{} is not executable", path.display()))
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn executable_file_permission(_path: &Path, _metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}
