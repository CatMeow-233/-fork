//! Pre-sampling and mid-turn auto-compaction, including against the previous model.

use super::*;

#[instrument(level = "trace", skip_all)]
pub(super) async fn run_pre_sampling_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    cancellation_token: &CancellationToken,
) -> CodexResult<()> {
    maybe_run_previous_model_inline_compact(sess, turn_context, client_session, cancellation_token)
        .await?;
    let token_status =
        super::context_window::context_window_token_status(sess.as_ref(), turn_context.as_ref())
            .await;
    let candidate = super::async_compaction::take_ready(sess, turn_context).await;
    // Compact if the configured auto-compaction budget or usable context window is exhausted.
    if token_status.token_limit_reached || candidate.is_some() {
        // Pre-turn compaction runs before run_turn creates the normal sampling step.
        let step_context = sess
            .capture_step_context(Arc::clone(turn_context), cancellation_token)
            .await?;
        run_auto_compact(
            sess,
            Arc::clone(&step_context),
            step_context,
            client_session,
            CompactionReason::ContextLimit,
            CompactionPhase::PreTurn,
            candidate,
        )
        .await?;
    }
    Ok(())
}

/// Returns true only when both turns declare compaction compatibility hashes and they differ.
/// A missing hash does not provide enough information to trigger compaction.
fn comp_hash_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    previous
        .zip(current)
        .is_some_and(|(previous, current)| previous != current)
}

/// Runs pre-sampling compaction against the previous model when its compaction compatibility
/// hash changed or when switching to a smaller context-window model.
///
/// Returns `Err(_)` only when compaction was attempted and failed.
async fn maybe_run_previous_model_inline_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    cancellation_token: &CancellationToken,
) -> CodexResult<()> {
    let Some(previous_turn_settings) = sess.previous_turn_settings().await else {
        return Ok(());
    };
    let should_compact_for_comp_hash_change = comp_hash_changed(
        previous_turn_settings.comp_hash.as_deref(),
        turn_context.model_info().comp_hash.as_deref(),
    );
    let previous_model = previous_turn_settings.model;
    if !should_compact_for_comp_hash_change && previous_model == turn_context.model_info().slug {
        return Ok(());
    }
    let mut previous_model_turn_context = turn_context
        .with_model(previous_model.clone(), &sess.services.models_manager)
        .await;
    // `with_model` preserves the current turn's access program. Restore the previous
    // turn's program so compaction uses the same model/cyber_access_program pair as that turn.
    // Combining the previous model with the current turn's program can produce a pair
    // that the server rejects.
    previous_model_turn_context.cyber_access_program = cyber_access_program::for_provider(
        &previous_model_turn_context.config,
        previous_turn_settings.cyber_access_program,
    );
    let previous_model_turn_context = Arc::new(previous_model_turn_context);

    let reason = if should_compact_for_comp_hash_change {
        CompactionReason::CompHashChanged
    } else {
        let Some(old_context_window) = previous_model_turn_context.model_context_window() else {
            return Ok(());
        };
        let Some(new_context_window) = turn_context.model_context_window() else {
            return Ok(());
        };
        let active_context_tokens = sess.get_total_token_usage().await;
        let previous_model_limit_reached =
            match turn_context.config.model_auto_compact_token_limit_scope {
                AutoCompactTokenLimitScope::Total => {
                    let new_auto_compact_limit = turn_context
                        .model_info()
                        .auto_compact_token_limit()
                        .unwrap_or(i64::MAX);
                    active_context_tokens > new_auto_compact_limit
                        || active_context_tokens >= new_context_window
                }
                AutoCompactTokenLimitScope::BodyAfterPrefix => {
                    active_context_tokens >= new_context_window
                }
            };
        if !previous_model_limit_reached
            || previous_model_turn_context.model_info().slug == turn_context.model_info().slug
            || old_context_window <= new_context_window
        {
            return Ok(());
        }
        CompactionReason::ModelDownshift
    };

    let step_context = sess
        .capture_step_context(previous_model_turn_context, cancellation_token)
        .await?;
    // The previous model summarizes the old window; the current model owns its replacement.
    // Reuse the replacement step if the compaction request needs to fall back to that model.
    let replacement_step_context = sess
        .capture_speculative_step_context(Arc::clone(turn_context), cancellation_token)
        .await?;
    run_auto_compact(
        sess,
        step_context,
        replacement_step_context,
        client_session,
        reason,
        CompactionPhase::PreTurn,
        /*candidate*/ None,
    )
    .await?;
    Ok(())
}

/// The replacement context may use a different model than the compactor.
#[allow(clippy::too_many_arguments)]
#[instrument(
    level = "trace",
    skip_all,
    fields(reason = ?reason, phase = ?phase)
)]
pub(super) async fn run_auto_compact(
    sess: &Arc<Session>,
    step_context: Arc<StepContext>,
    replacement_step_context: Arc<StepContext>,
    client_session: &mut ModelClientSession,
    reason: CompactionReason,
    phase: CompactionPhase,
    candidate: Option<Box<BackgroundCompactionCandidate>>,
) -> CodexResult<bool> {
    let turn_context = &step_context.turn;
    if matches!(phase, CompactionPhase::PreTurn)
        && crate::guardian::is_basic_session_source(&turn_context.session_source)
        && !crate::guardian::should_compact_guardian_input(sess)?
    {
        return Ok(false);
    }
    let world_state = Arc::new(
        sess.build_world_state_for_step(&replacement_step_context, /*new_window*/ true)
            .await?,
    );
    let _profile_guard = turn_context.turn_timing_state.begin_compaction();
    let _compaction_span = trace_span!(
        "codex.compaction",
        codex.turn.phase = "compaction",
        conversation.id = %sess.thread_id,
        turn.id = %turn_context.sub_id,
    );
    if turn_context.config.features.enabled(Feature::TokenBudget) {
        // Compaction is the reset request, so force a new context window
        // instead of consuming a pending `new_context` tool request.
        crate::compact_token_budget::run_inline_auto_compact_task(
            Arc::clone(sess),
            replacement_step_context,
            world_state,
        )
        .await?;
        return Ok(true);
    }

    match turn_context.provider.capabilities().remote_compaction {
        RemoteCompactionSupport::V2 => {
            let has_candidate = candidate.is_some();
            if !has_candidate {
                emit_compact_metric(
                    &sess.services.session_telemetry,
                    "remote_v2",
                    /*manual*/ false,
                );
            }
            let fallback = if has_candidate && matches!(reason, CompactionReason::ContextLimit) {
                let status =
                    super::context_window::context_window_token_status(sess, turn_context).await;
                let required = match phase {
                    CompactionPhase::PostTurn => status.turn_end_compaction_threshold_reached,
                    CompactionPhase::PreTurn
                    | CompactionPhase::MidTurn
                    | CompactionPhase::StandaloneTurn => status.token_limit_reached,
                };
                if required {
                    crate::compact_remote_v2::CompactionFallback::Foreground
                } else {
                    crate::compact_remote_v2::CompactionFallback::Skip
                }
            } else {
                crate::compact_remote_v2::CompactionFallback::Foreground
            };
            let installed = run_inline_remote_auto_compact_task_v2(
                Arc::clone(sess),
                step_context,
                replacement_step_context,
                client_session,
                world_state,
                reason,
                phase,
                candidate,
                fallback,
            )
            .await?;
            if has_candidate && installed {
                emit_compact_metric(
                    &sess.services.session_telemetry,
                    "remote_v2",
                    /*manual*/ false,
                );
            }
            Ok(installed)
        }
        RemoteCompactionSupport::Unsupported => {
            emit_compact_metric(
                &sess.services.session_telemetry,
                "local",
                /*manual*/ false,
            );
            run_inline_auto_compact_task(
                Arc::clone(sess),
                Arc::clone(turn_context),
                replacement_step_context,
                world_state,
                reason,
                phase,
            )
            .await?;
            Ok(true)
        }
    }
}
