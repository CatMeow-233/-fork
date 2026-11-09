//! Implements the `codex doctor` diagnostic report.
//!
//! Doctor is intentionally read-mostly: checks inspect the current installation,
//! configuration, authentication, terminal, state paths, and bounded reachability
//! probes without attempting repair or starting long-lived services. Each check
//! returns a redacted, serializable row so the same data can back the human
//! summary and `--json` support report. PATH entries are untrusted data: checks
//! may inspect them, but must not execute the programs they select.
//!
//! A failing check should describe the problem and remediation, but it should not
//! mutate user state. That keeps the command safe to run before filing a support
//! issue or while diagnosing a broken local installation.
//!
//! This module owns the report types and the CLI entry point. `report_builder`
//! runs the checks and assembles the report, `json_report` produces the `--json`
//! form, and each remaining child module implements one family of checks.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::env;
use std::ffi::OsStr;
use std::future::Future;
use std::io::IsTerminal;
use std::io::Read;
use std::net::IpAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use clap::Parser;
use codex_api::ApiError;
use codex_api::ResponsesWebsocketClient;
use codex_api::TransportError;
use codex_api::is_azure_responses_provider;
use codex_arg0::Arg0DispatchPaths;
use codex_config::ConfigLoadError;
use codex_config::types::McpServerConfig;
use codex_config::types::McpServerTransportConfig;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_core::config::LoaderOverrides;
use codex_core::config::find_codex_home;
use codex_features::FEATURES;
use codex_features::Feature;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::RouteAwareClientPool;
use codex_install_context::CodexPackageLayout;
use codex_install_context::InstallContext;
use codex_install_context::InstallMethod;
use codex_install_context::StandalonePlatform;
use codex_login::AuthDotJson;
use codex_login::AuthManager;
use codex_login::CODEX_ACCESS_TOKEN_ENV_VAR;
use codex_login::CODEX_API_KEY_ENV_VAR;
use codex_login::CodexAuth;
use codex_login::OPENAI_API_KEY_ENV_VAR;
use codex_login::default_client::create_client_without_request_logging;
use codex_login::default_client::default_headers;
use codex_login::load_auth_dot_json;
use codex_model_provider::create_model_provider;
use codex_protocol::auth::AuthMode;
use codex_protocol::protocol::AskForApproval;
use codex_terminal_detection::Multiplexer;
use codex_terminal_detection::TerminalInfo;
use codex_terminal_detection::TerminalName;
use codex_terminal_detection::terminal_info;
use codex_tui::Cli as TuiCli;
use codex_utils_cli::CliConfigOverrides;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde::Serialize;
use supports_color::Stream;

mod auth_credentials;
mod background;
mod configuration;
mod desktop;
mod disk;
mod filesystem_paths;
mod git;
mod installation;
mod json_report;
mod mcp_servers;
mod network;
mod output;
mod progress;
mod reachability;
mod report_builder;
mod runtime;
mod sandbox;
mod security;
mod state_paths;
mod system;
mod terminal_env;
mod thread_inventory;
mod title;
mod updates;
#[cfg(target_os = "windows")]
mod windows_dev_drive;

#[cfg(test)]
#[path = "doctor/desktop_tests.rs"]
mod desktop_tests;

use auth_credentials::auth_check;
use auth_credentials::stored_auth_mode_value;
use background::background_server_check;
use configuration::config_check;
use configuration::config_load_failure;
use configuration::load_config;
use git::git_check;
use installation::describe_install_context;
use installation::doctor_install_context;
use installation::doctor_managed_by_npm;
use installation::installation_check;
use json_report::redacted_json_report;
use mcp_servers::mcp_check;
use output::HumanOutputOptions;
use output::redact_detail;
use output::render_human_report;
use progress::DoctorProgress;
use progress::doctor_progress;
use reachability::default_reachability_plan;
use reachability::provider_reachability_check;
use reachability::provider_reachability_plan;
use reachability::websocket_reachability_check;
use report_builder::build_report;
use report_builder::run_async_check;
use report_builder::run_sync_check;
use runtime::runtime_check;
use runtime::search_check;
use sandbox::sandbox_check;
use state_paths::fallback_state_check;
use state_paths::state_check;
use system::system_check;
use terminal_env::terminal_check;
use thread_inventory::thread_inventory_check;
use title::terminal_title_check;
use updates::updates_check;

#[cfg(test)]
use auth_credentials::provider_specific_auth_check;
#[cfg(test)]
use auth_credentials::stored_auth_issues;
#[cfg(test)]
use auth_credentials::stored_auth_mode;
#[cfg(test)]
use configuration::config_overrides_from_interactive;
#[cfg(test)]
use configuration::push_startup_warning_counts;
#[cfg(test)]
use json_report::redacted_json_check;
#[cfg(test)]
use mcp_servers::executable_path_exists;
#[cfg(test)]
use mcp_servers::http_probe_url_with_timeout;
#[cfg(test)]
use mcp_servers::mcp_check_from_servers;
#[cfg(test)]
use mcp_servers::mcp_http_probe_url_with_timeout;
#[cfg(test)]
use reachability::close_reason_detail;
#[cfg(test)]
use reachability::provider_auth_reachability_mode_from_auth;
#[cfg(test)]
use reachability::provider_reachability_outcome;
#[cfg(test)]
use reachability::provider_reachability_plan_from_parts;
#[cfg(test)]
use reachability::websocket_error_detail;
#[cfg(test)]
use reachability::websocket_probe_warning;
#[cfg(test)]
use report_builder::overall_status;
#[cfg(test)]
use state_paths::rollout_stats_details;
#[cfg(test)]
use terminal_env::color_output_summary;
#[cfg(test)]
use terminal_env::terminal_check_from_inputs;

const OPENAI_BETA_HEADER: &str = "OpenAI-Beta";
const RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE: &str = "responses_websockets=2026-02-06";
const WEBSOCKET_IMMEDIATE_CLOSE_GRACE: Duration = Duration::from_millis(250);
const SLOW_CHECK_PROGRESS_THRESHOLD: Duration = Duration::from_secs(2);
const SLOW_CHECK_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
const PROXY_ENV_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];
const COLOR_ENV_VARS: &[&str] = &[
    "COLORTERM",
    "NO_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "FORCE_COLOR",
    "COLORFGBG",
];
const TERMINAL_DIMENSION_ENV_VARS: &[&str] = &["COLUMNS", "LINES"];
const TERMINFO_ENV_VARS: &[&str] = &["TERMINFO", "TERMINFO_DIRS"];
const LOCALE_ENV_VARS: &[&str] = &["LC_ALL", "LC_CTYPE", "LANG"];
const REMOTE_TERMINAL_ENV_VARS: &[&str] = &[
    "SSH_TTY",
    "SSH_CONNECTION",
    "SSH_CLIENT",
    "MOSH_IP",
    "WSL_DISTRO_NAME",
    "WSL_INTEROP",
    "VSCODE_INJECTION",
    "VSCODE_IPC_HOOK_CLI",
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "WT_SESSION",
];
const NARROW_TERMINAL_COLUMNS: u16 = 80;
const NARROW_TERMINAL_ROWS: u16 = 24;

/// Options for building a local Codex diagnostic report.
///
/// The command runs the full diagnostic set unless `--network-only` is selected.
/// Human output includes detailed diagnostics by default; --summary keeps the terminal output
/// compact.
#[derive(Debug, Parser)]
pub struct DoctorCommand {
    /// Internal isolated filesystem probe; exits before loading configuration.
    #[arg(long, hide = true)]
    probe_filesystem_path: Option<PathBuf>,

    /// Emit a redacted machine-readable report.
    #[arg(long, default_value_t = false)]
    json: bool,

    /// Run config, network environment, provider HTTP, and Responses WebSocket checks.
    #[arg(long, default_value_t = false)]
    network_only: bool,

    /// Limit database integrity scans when collecting a feedback attachment.
    #[arg(long, hide = true, default_value_t = false)]
    feedback: bool,

    /// Only show grouped check rows and the final count summary.
    #[arg(long, default_value_t = false)]
    summary: bool,

    /// Expand long lists in detailed human output.
    #[arg(long, default_value_t = false)]
    all: bool,

    /// Disable ANSI color in human output.
    #[arg(long, default_value_t = false)]
    no_color: bool,

    /// Use ASCII status labels and separators in human output.
    #[arg(long, default_value_t = false)]
    ascii: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Ok,
    Warning,
    Fail,
}

/// Machine-readable doctor output shared by human and JSON renderers.
///
/// The schema is intentionally flat: each check carries its own category,
/// status, details, remediation, and duration so support tooling can filter or
/// redact individual rows without understanding the renderer's section layout.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorReport {
    schema_version: u32,
    generated_at: String,
    overall_status: CheckStatus,
    codex_version: String,
    checks: Vec<DoctorCheck>,
}

/// One diagnostic result in the doctor report.
///
/// Summaries are safe for compact human output. Details may include local paths
/// or command output and are redacted before rendering or JSON serialization.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorCheck {
    id: String,
    category: String,
    status: CheckStatus,
    summary: String,
    details: Vec<String>,
    issues: Vec<DoctorIssue>,
    remediation: Option<String>,
    duration_ms: u64,
}

/// Structured cause/remedy metadata for a non-ok doctor check.
///
/// Human output uses issues to make warnings and failures self-explanatory:
/// the row headline says what is wrong, matching detail rows show measured vs.
/// expected values, and remedies are printed as explicit next actions.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DoctorIssue {
    severity: CheckStatus,
    cause: String,
    measured: Option<String>,
    expected: Option<String>,
    remedy: Option<String>,
    fields: Vec<String>,
}

impl DoctorIssue {
    fn new(severity: CheckStatus, cause: impl Into<String>) -> Self {
        Self {
            severity,
            cause: cause.into(),
            measured: None,
            expected: None,
            remedy: None,
            fields: Vec::new(),
        }
    }

    fn measured(mut self, measured: impl Into<String>) -> Self {
        self.measured = Some(measured.into());
        self
    }

    fn expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    fn remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = Some(remedy.into());
        self
    }

    fn field(mut self, field: impl Into<String>) -> Self {
        self.fields.push(field.into());
        self
    }
}

impl DoctorCheck {
    fn new(
        id: impl Into<String>,
        category: impl Into<String>,
        status: CheckStatus,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category: category.into(),
            status,
            summary: summary.into(),
            details: Vec::new(),
            issues: Vec::new(),
            remediation: None,
            duration_ms: 0,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.details.push(detail.into());
        self
    }

    fn details(mut self, details: Vec<String>) -> Self {
        self.details.extend(details);
        self
    }

    fn remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }

    fn issue(mut self, issue: DoctorIssue) -> Self {
        self.issues.push(issue);
        self
    }
}

/// Builds, renders, and exits according to the current doctor report.
///
/// This is the CLI entry point for codex doctor. It does not repair issues;
/// failures are represented in the report and cause a non-zero process exit so
/// scripts can distinguish a clean environment from one that needs attention.
pub async fn run_doctor(
    command: DoctorCommand,
    root_config_overrides: CliConfigOverrides,
    interactive: &TuiCli,
    arg0_paths: &Arg0DispatchPaths,
) -> anyhow::Result<()> {
    if let Some(path) = &command.probe_filesystem_path {
        std::process::exit(filesystem_paths::probe_exit_code(path));
    }
    let report = build_report(&command, root_config_overrides, interactive, arg0_paths).await;

    if command.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&redacted_json_report(&report))?
        );
    } else {
        print!(
            "{}",
            render_human_report(&report, human_output_options(&command))
        );
    }

    if report.overall_status == CheckStatus::Fail {
        std::process::exit(1);
    }

    Ok(())
}

/// JSON support report emitted by `codex doctor --json`.
///
/// The report is keyed by check id so support tooling can fetch paths like
/// `checks["terminal.metadata"]` without scanning arrays. Human rendering can
/// reorder or group rows independently, but this JSON shape should stay stable
/// across cosmetic output changes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonDoctorReport {
    schema_version: u32,
    generated_at: String,
    overall_status: CheckStatus,
    codex_version: String,
    checks: BTreeMap<String, JsonDoctorCheck>,
}

/// One redacted check in the JSON support report.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonDoctorCheck {
    id: String,
    category: String,
    status: CheckStatus,
    summary: String,
    details: BTreeMap<String, JsonDetailValue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    issues: Vec<JsonDoctorIssue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
    remediation: Option<String>,
    duration_ms: u64,
}

/// One redacted issue in the JSON support report.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonDoctorIssue {
    severity: CheckStatus,
    cause: String,
    measured: Option<String>,
    expected: Option<String>,
    remedy: Option<String>,
    fields: Vec<String>,
}

/// JSON detail value that preserves repeated detail keys without inventing names.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum JsonDetailValue {
    One(String),
    Many(Vec<String>),
}

fn push_proxy_env_details(details: &mut Vec<String>) {
    let present_proxy_vars = PROXY_ENV_VARS
        .iter()
        .copied()
        .filter(|name| env_var_present(name))
        .collect::<Vec<_>>();
    if present_proxy_vars.is_empty() {
        details.push("proxy env vars: none".to_string());
    } else {
        details.push(format!(
            "proxy env vars present: {}",
            present_proxy_vars.join(", ")
        ));
    }
}

fn read_probe_file(path: &Path) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0_u8; 1];
    let _ = file.read(&mut buffer)?;
    Ok(())
}

#[derive(Clone, Debug)]
struct TerminalCheckInputs {
    info: TerminalInfo,
    env: BTreeMap<String, String>,
    present_env: BTreeSet<String>,
    no_color_flag: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
    stream_supports_color: bool,
    terminal_size: Result<(u16, u16), String>,
    tmux_details: Vec<String>,
    windows_console_details: Vec<String>,
}

#[derive(Default)]
struct RolloutStats {
    files: u64,
    total_bytes: u64,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReachabilityPlan {
    description: String,
    endpoints: Vec<ReachabilityEndpoint>,
    http_client_factory: HttpClientFactory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReachabilityEndpoint {
    label: String,
    url: String,
    required: bool,
    route_probe_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderAuthReachabilityMode {
    NotRequired,
    ApiKey,
    Chatgpt,
}

enum RouteProbeOutcome {
    Ok(String),
    Warning(String),
    Fail(String),
    TransportError(String),
}

fn push_path_detail(details: &mut Vec<String>, label: &str, path: Option<&Path>) {
    match path {
        Some(path) => details.push(format!("{label}: {}", path.display())),
        None => details.push(format!("{label}: none")),
    }
}

fn env_var_present(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn human_output_options(command: &DoctorCommand) -> HumanOutputOptions {
    let term = env::var("TERM").ok();
    let color_enabled = should_enable_color(
        command.no_color,
        env::var_os("NO_COLOR").is_some(),
        term.as_deref(),
        std::io::stdout().is_terminal(),
        supports_color::on(Stream::Stdout).is_some(),
    );
    HumanOutputOptions {
        show_details: !command.summary,
        show_all: command.all,
        ascii: command.ascii,
        color_enabled,
    }
}

fn should_enable_color(
    no_color_flag: bool,
    no_color_env: bool,
    term: Option<&str>,
    stdout_is_tty: bool,
    stream_supports_color: bool,
) -> bool {
    !no_color_flag
        && !no_color_env
        && term != Some("dumb")
        && stdout_is_tty
        && stream_supports_color
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
