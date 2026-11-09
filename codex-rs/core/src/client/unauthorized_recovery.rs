//! Recovery from unauthorized responses via provider recovery or token refresh.

use super::*;

impl PendingUnauthorizedRetry {
    pub(super) fn from_recovery(recovery: UnauthorizedRecoveryExecution) -> Self {
        Self {
            retry_after_unauthorized: true,
            recovery_mode: Some(recovery.mode),
            recovery_phase: Some(recovery.phase),
        }
    }
}

fn emit_auth_recovery_event(
    event_sender: Option<&Sender<ProtocolEvent>>,
    turn_id: Option<&str>,
    provider: &SharedModelProvider,
    message: &str,
    event: fn(AuthRecoveryEvent) -> EventMsg,
) {
    if let (Some(sender), Some(turn_id)) = (event_sender, turn_id) {
        let _ = sender.try_send(ProtocolEvent {
            id: turn_id.to_string(),
            msg: event(AuthRecoveryEvent {
                provider: provider.info().name.clone(),
                message: message.to_string(),
            }),
        });
    }
}

pub(super) async fn handle_unauthorized(
    transport: TransportError,
    auth_recovery: &mut Option<UnauthorizedRecovery>,
    provider_auth_recovery_attempted: &mut bool,
    session_telemetry: &SessionTelemetry,
    provider: &SharedModelProvider,
    event_sender: Option<&Sender<ProtocolEvent>>,
    turn_id: Option<&str>,
) -> Result<UnauthorizedRecoveryExecution> {
    let debug = extract_response_debug_context(&transport);
    if !*provider_auth_recovery_attempted {
        *provider_auth_recovery_attempted = true;
        let messages = provider.auth_recovery_messages();
        if let Some(messages) = messages {
            emit_auth_recovery_event(
                event_sender,
                turn_id,
                provider,
                messages.started,
                EventMsg::AuthRecoveryStarted,
            );
        }
        match provider.recover_from_unauthorized().await {
            Ok(ProviderUnauthorizedRecovery::Recovered) => {
                if let Some(messages) = messages {
                    emit_auth_recovery_event(
                        event_sender,
                        turn_id,
                        provider,
                        messages.succeeded,
                        EventMsg::AuthRecoveryCompleted,
                    );
                }
                return Ok(UnauthorizedRecoveryExecution {
                    mode: "provider",
                    phase: "provider_refresh",
                });
            }
            Ok(ProviderUnauthorizedRecovery::NotConfigured) => {}
            Err(error) => {
                let original = provider.map_api_error(ApiError::Transport(transport));
                warn!(
                    error = %error,
                    original_error = %original,
                    "provider authentication recovery failed"
                );
                return Err(if error.retry_delay(/*retry_count*/ 1).is_some() {
                    original
                } else {
                    error
                });
            }
        }
    }

    if let Some(recovery) = auth_recovery
        && recovery.has_next()
    {
        let mode = recovery.mode_name();
        let phase = recovery.step_name();
        return match recovery.next().await {
            Ok(step_result) => {
                session_telemetry.record_auth_recovery(
                    mode,
                    phase,
                    "recovery_succeeded",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                    /*recovery_reason*/ None,
                    step_result.auth_state_changed(),
                );
                emit_feedback_auth_recovery_tags(
                    mode,
                    phase,
                    "recovery_succeeded",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                );
                Ok(UnauthorizedRecoveryExecution { mode, phase })
            }
            Err(RefreshTokenError::Policy(error)) => Err(CodexErr::Fatal(error.to_string())),
            Err(RefreshTokenError::Permanent(failed)) => {
                session_telemetry.record_auth_recovery(
                    mode,
                    phase,
                    "recovery_failed_permanent",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                    /*recovery_reason*/ None,
                    /*auth_state_changed*/ None,
                );
                emit_feedback_auth_recovery_tags(
                    mode,
                    phase,
                    "recovery_failed_permanent",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                );
                Err(CodexErr::RefreshTokenFailed(failed))
            }
            Err(RefreshTokenError::Transient(other)) => {
                session_telemetry.record_auth_recovery(
                    mode,
                    phase,
                    "recovery_failed_transient",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                    /*recovery_reason*/ None,
                    /*auth_state_changed*/ None,
                );
                emit_feedback_auth_recovery_tags(
                    mode,
                    phase,
                    "recovery_failed_transient",
                    debug.request_id.as_deref(),
                    debug.cf_ray.as_deref(),
                    debug.auth_error.as_deref(),
                    debug.auth_error_code.as_deref(),
                );
                Err(CodexErr::Io(other))
            }
        };
    }

    let (mode, phase, recovery_reason) = match auth_recovery.as_ref() {
        Some(recovery) => (
            recovery.mode_name(),
            recovery.step_name(),
            Some(recovery.unavailable_reason()),
        ),
        None => ("none", "none", Some("auth_manager_missing")),
    };
    session_telemetry.record_auth_recovery(
        mode,
        phase,
        "recovery_not_run",
        debug.request_id.as_deref(),
        debug.cf_ray.as_deref(),
        debug.auth_error.as_deref(),
        debug.auth_error_code.as_deref(),
        recovery_reason,
        /*auth_state_changed*/ None,
    );
    emit_feedback_auth_recovery_tags(
        mode,
        phase,
        "recovery_not_run",
        debug.request_id.as_deref(),
        debug.cf_ray.as_deref(),
        debug.auth_error.as_deref(),
        debug.auth_error_code.as_deref(),
    );

    Err(provider.map_api_error(ApiError::Transport(transport)))
}
