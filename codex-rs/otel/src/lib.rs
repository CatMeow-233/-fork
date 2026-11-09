pub mod auth_storage;
pub(crate) mod config;
mod events;
pub(crate) mod metrics;
pub(crate) mod provider;
pub(crate) mod trace_context;

mod agent_response;
mod guardian_assessment;
mod network_policy;
mod otlp;
mod skill_invocation;
mod targets;
mod tool_result;

use crate::metrics::Result as MetricsResult;
use codex_protocol::auth::AuthMode;
use serde::Serialize;
use strum_macros::Display;

pub use crate::agent_response::AgentResponseContext;
pub use crate::agent_response::AgentResponseLogger;
pub use crate::config::OtelExporter;
pub use crate::config::OtelHttpProtocol;
pub use crate::config::OtelSettings;
pub use crate::config::OtelTlsConfig;
pub use crate::config::StatsigMetricsSettings;
pub use crate::config::validate_span_attributes;
pub use crate::events::session_telemetry::AuthEnvTelemetryMetadata;
pub use crate::events::session_telemetry::SessionTelemetry;
pub use crate::events::session_telemetry::SessionTelemetryMetadata;
pub use crate::events::session_telemetry::bounded_product_sku;
pub use crate::metrics::runtime_metrics::RuntimeMetricTotals;
pub use crate::metrics::runtime_metrics::RuntimeMetricsSummary;
pub use crate::metrics::timer::Timer;
pub use crate::metrics::*;
pub use crate::provider::OtelProvider;
pub use crate::skill_invocation::SkillInvocationEvent;
pub use crate::skill_invocation::SkillInvocationType;
pub use crate::trace_context::context_from_w3c_trace_context;
pub use crate::trace_context::current_span_trace_id;
pub use crate::trace_context::current_span_w3c_trace_context;
pub use crate::trace_context::inject_span_w3c_trace_headers;
pub use crate::trace_context::set_parent_from_context;
pub use crate::trace_context::set_parent_from_w3c_trace_context;
pub use crate::trace_context::span_w3c_trace_context;
pub use crate::trace_context::traceparent_context_from_env;
pub use crate::trace_context::validate_tracestate_entries;
pub use crate::trace_context::validate_tracestate_member;
pub use codex_utils_string::sanitize_metric_tag_value;

#[derive(Debug, Clone, Serialize, Display)]
#[serde(rename_all = "snake_case")]
pub enum ToolDecisionSource {
    AutomatedReviewer,
    Config,
    User,
}

/// Coarsens the authentication domain into the dimensions used by telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
pub enum TelemetryAuthMode {
    ApiKey,
    Chatgpt,
}

impl From<AuthMode> for TelemetryAuthMode {
    fn from(mode: AuthMode) -> Self {
        match mode {
            AuthMode::ApiKey | AuthMode::BedrockApiKey | AuthMode::BedrockAccessKeys => {
                Self::ApiKey
            }
            AuthMode::Chatgpt
            | AuthMode::ChatgptAuthTokens
            | AuthMode::Headers
            | AuthMode::AgentIdentity
            | AuthMode::PersonalAccessToken => Self::Chatgpt,
        }
    }
}

/// Operating system details, discovered once per process.
///
/// Discovery can launch subprocesses on Linux (`lsb_release`, a Python script on
/// Debian and Ubuntu, and `getconf`), so every caller shares this single result.
/// Call it from a background thread early in startup to keep the discovery off
/// the critical path; later callers then block only for any remaining time.
pub fn os_info() -> &'static ::os_info::Info {
    static OS_INFO: std::sync::LazyLock<::os_info::Info> = std::sync::LazyLock::new(::os_info::get);
    &OS_INFO
}

/// Start OS discovery on a background thread so later callers find it ready.
pub fn prewarm_os_info() {
    let _ = std::thread::Builder::new()
        .name("codex-os-info".to_string())
        .spawn(|| {
            let _ = os_info();
        });
}

/// Install externally managed, non-Statsig process-global metrics.
///
/// Call this once during single-threaded startup, before any instruments are
/// registered. Keep the returned handle to flush and shut down the exporter
/// owned by this installation.
pub fn install_global_metrics(metrics: MetricsClient) -> MetricsClient {
    crate::metrics::install_global(metrics)
}

/// Start a metrics timer using the globally installed metrics client.
pub fn start_global_timer(name: &str, tags: &[(&str, &str)]) -> MetricsResult<Timer> {
    let Some(metrics) = crate::metrics::global() else {
        return Err(MetricsError::ExporterDisabled);
    };
    metrics.start_timer(name, tags)
}

/// Returns the resolved Statsig metrics settings for the globally installed
/// OTEL metrics client, if the active metrics exporter is Statsig.
pub fn global_statsig_metrics_settings() -> Option<StatsigMetricsSettings> {
    crate::metrics::global_statsig_settings()
}
