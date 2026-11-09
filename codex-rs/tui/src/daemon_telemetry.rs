//! Records automatic daemon startup and ensures launch observations survive setup failures.

use super::*;
use crate::terminal_size_telemetry::LaunchTags;

/// Record the connection mode or update handoff once. Other early exits are unconfirmed.
pub(super) struct Launch<F: FnOnce(&'static str) -> LaunchTags>(pub(super) Option<F>);

impl<F: FnOnce(&'static str) -> LaunchTags> Launch<F> {
    pub(super) fn record(mut self, app_server_mode: &'static str) -> LaunchTags {
        self.0
            .take()
            .map(|record| record(app_server_mode))
            .unwrap_or_default()
    }
}

impl<F: FnOnce(&'static str) -> LaunchTags> Drop for Launch<F> {
    fn drop(&mut self) {
        if let Some(record) = self.0.take() {
            record("unconfirmed");
        }
    }
}

/// Outcome of the automatic daemon start, kept until session telemetry exists.
///
/// Startup must not wait on a metrics export. A start that lets startup continue is
/// recorded on the session provider, which exports periodically and at exit. Only a
/// start failure that aborts launch flushes through a short-lived provider.
pub(super) struct StartObservation {
    outcome: &'static str,
    failure_reason: Option<&'static str>,
}

impl StartObservation {
    pub(super) fn new(
        result: &Result<
            codex_app_server_daemon::LifecycleOutput,
            codex_app_server_daemon::telemetry::StartError,
        >,
    ) -> Self {
        use codex_app_server_daemon::LifecycleStatus;
        let outcome = match result {
            Ok(output) => match output.status {
                LifecycleStatus::Started => "started",
                LifecycleStatus::AlreadyRunning => "already_running",
                LifecycleStatus::Restarted => "restarted",
                LifecycleStatus::Stopped
                | LifecycleStatus::NotRunning
                | LifecycleStatus::Running => "unconfirmed",
            },
            Err(_) => "failed",
        };
        Self {
            outcome,
            failure_reason: result
                .as_ref()
                .err()
                .map(codex_app_server_daemon::telemetry::StartError::failure_reason),
        }
    }

    /// Record on the session provider without forcing an export.
    pub(super) fn record(
        self,
        metrics: &codex_otel::MetricsClient,
        settings_tags: &[(&'static str, &'static str)],
    ) {
        let tags = self.tags(settings_tags);
        let _ = metrics.counter("codex.daemon.start", /*inc*/ 1, &tags);
    }

    /// Record a start failure that aborts launch before session telemetry exists.
    pub(super) async fn record_aborted(self, config: &Config) {
        // This short-lived provider uses exactly the same consent and identity
        // construction as TUI startup, so the failure survives the early exit.
        let Ok(Ok(Some(otel))) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            codex_app_server_client::build_otel_provider(
                config,
                env!("CARGO_PKG_VERSION"),
                /*service_name_override*/ None,
                /*default_analytics_enabled*/ true,
            )
        })) else {
            return;
        };
        if let Some(metrics) = otel.metrics() {
            let settings_tags =
                codex_app_server_daemon::telemetry::settings_tags(&config.codex_home).await;
            self.record(metrics, &settings_tags);
        }
        let _ = otel
            .shutdown_with_timeout(INTERACTIVE_OTEL_SHUTDOWN_TIMEOUT)
            .await;
    }

    fn tags(
        &self,
        settings_tags: &[(&'static str, &'static str)],
    ) -> Vec<(&'static str, &'static str)> {
        let mut tags = settings_tags.to_vec();
        tags.extend([
            ("initiation_source", "tui_auto_start"),
            ("outcome", self.outcome),
        ]);
        if let Some(reason) = self.failure_reason {
            tags.push(("failure_reason", reason));
        }
        tags
    }
}
