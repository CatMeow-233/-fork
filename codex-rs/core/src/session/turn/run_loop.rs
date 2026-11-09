//! The turn loop: admits input and repeats sampling requests until no follow-up is needed.

use super::*;

/// Keep the initial input recoverable while startup can block on a host roster read.
async fn preserve_initial_input_during<T>(
    sess: &Session,
    turn_context: &Arc<TurnContext>,
    input: &mut Vec<TurnInput>,
    prepare: impl Future<Output = T>,
) -> CodexResult<T> {
    if input.is_empty() || sess.host_roster_reader(turn_context).is_none() {
        return Ok(prepare.await);
    }

    let exchange_input = async |replacement| {
        let mut active = sess.active_turn.lock().await;
        let task = active
            .as_mut()
            .and_then(|turn| turn.task.as_mut())
            .filter(|task| Arc::ptr_eq(&task.turn_context, turn_context))
            .ok_or_else(|| CodexErr::new(CodexErrorDetails::TurnAborted))?;
        Ok::<_, CodexErr>(std::mem::replace(
            &mut task.pending_initial_input,
            replacement,
        ))
    };

    let previous = exchange_input(Some(std::mem::take(input))).await?;
    debug_assert!(previous.is_none());
    let result = prepare.await;
    // If suspension took the task, its cleanup owns the input.
    *input = exchange_input(/* replacement */ None)
        .await?
        .ok_or_else(|| CodexErr::Fatal("initial input missing after turn preparation".into()))?;
    Ok(result)
}

/// Takes initial turn input and runs a loop where, at each sampling request,
/// the model replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single sampling request, in practice, we generally one item per sampling request:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next sampling request.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the turn complete.
///
pub(crate) async fn run_turn(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    mut input: Vec<TurnInput>,
    mcp_startup_requirements: &mut McpStartupRequirements,
    prewarmed_client_session: Option<ModelClientSession>,
    cancellation_token: CancellationToken,
) -> CodexResult<Option<String>> {
    let model_info = turn_context.capture_current_model_info();
    if !model_info.input_modalities.contains(&InputModality::Audio)
        && input.iter().any(turn_input_has_uploaded_audio_file)
    {
        let result = run_hooks_and_record_inputs(
            &sess,
            &turn_context,
            &model_info,
            &input,
            PersistContext::TurnStart,
        )
        .await;
        return match result {
            Err(err) => Err(err),
            Ok(_) => Err(unsupported_uploaded_audio_error()),
        };
    }
    let auto_compaction_enabled = sess
        .services
        .thread_extension_data
        .get::<AutoCompactionDisabled>()
        .is_none();
    let auto_summarization_enabled =
        auto_compaction_enabled && !turn_context.config.features.enabled(Feature::TokenBudget);
    if crate::guardian::is_basic_session_source(&turn_context.session_source) {
        crate::guardian::check_pending_guardian_input(&sess, &turn_context).await?;
    }
    // Record results from hooks that finished after the previous turn before this turn's user prompt.
    drain_async_hook_results(&sess, &turn_context, /*before_user_prompt*/ true).await;

    let mut client_session =
        prewarmed_client_session.unwrap_or_else(|| sess.services.model_client.new_session());
    // TODO(ccunningham): Pre-turn compaction runs before context updates and the
    // new user message are recorded. Estimate pending incoming items (context
    // diffs/full reinjection + user input) and trigger compaction preemptively
    // when they would push the thread over the compaction threshold.
    if auto_compaction_enabled
        && let Err(err) = preserve_initial_input_during(
            &sess,
            &turn_context,
            &mut input,
            run_pre_sampling_compact(
                &sess,
                &turn_context,
                &mut client_session,
                &cancellation_token,
            ),
        )
        .await?
    {
        // Compaction runs before the new input is recorded, so preserve it on every failure.
        let _ = run_hooks_and_record_inputs(
            &sess,
            &turn_context,
            &turn_context.capture_current_model_info(),
            &input,
            PersistContext::Standard,
        )
        .await;
        if matches!(err.details(), CodexErrorDetails::TurnAborted) {
            return Err(err);
        }
        if matches!(err.details(), CodexErrorDetails::ToolCollision(_)) {
            return Err(err);
        }
        let error = err.to_codex_protocol_error();
        sess.emit_turn_error_lifecycle(turn_context.as_ref(), error.clone(), err.details())
            .await;
        // Publish the failure only after prompt hooks finish, so clients cannot react to
        // an error by steering follow-up input into a turn still preserving its prompt.
        let message_prefix = match turn_context.provider.capabilities().remote_compaction {
            RemoteCompactionSupport::V2 => Some("Error running remote compact task".to_string()),
            RemoteCompactionSupport::Unsupported => None,
        };
        sess.send_event(
            turn_context.as_ref(),
            EventMsg::Error(err.to_error_event(message_prefix)),
        )
        .await;
        error!("Failed to run pre-sampling compact");
        return Ok(None);
    }

    let user_input = turn_user_input(&input);
    let allow_plugin_mentions =
        !crate::guardian::is_basic_session_source(&turn_context.session_source);
    let McpStartupRequirements {
        required_servers,
        required_plugins,
    } = mcp_startup_requirements;
    if allow_plugin_mentions {
        required_plugins.extend(crate::plugins::collect_explicit_plugin_ids(&user_input));
    }
    let (input_required_servers, mentioned_plugins) =
        match required_mcp_servers_for_input(&sess, turn_context.as_ref(), &user_input)
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(requirements) => requirements,
            Err(err) => {
                let _ = run_hooks_and_record_inputs(
                    &sess,
                    &turn_context,
                    &turn_context.capture_current_model_info(),
                    &input,
                    PersistContext::Standard,
                )
                .await;
                return Err(err.into());
            }
        };

    required_servers.extend(input_required_servers);
    required_servers.sort_unstable();
    required_servers.dedup();

    // run_turn owns the step used to seed context and make the first sampling request.
    let first_step_context = match sess
        .capture_step_context_with_required_mcp_servers(
            Arc::clone(&turn_context),
            &cancellation_token,
            required_servers,
            required_plugins,
        )
        .await
    {
        Ok(step_context) => step_context,
        Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
            let _ = run_hooks_and_record_inputs(
                &sess,
                &turn_context,
                &turn_context.capture_current_model_info(),
                &input,
                PersistContext::Standard,
            )
            .await;
            return Err(err);
        }
        Err(err) => return Err(err),
    };
    // Record initial context while preparing diff display roots.
    let (record_context, display_roots) = tokio::join!(
        preserve_initial_input_during(
            &sess,
            &turn_context,
            &mut input,
            sess.record_context_updates_and_set_reference_context_item(first_step_context.as_ref()),
        ),
        async {
            // Guardian must not wait for remote Git discovery just to display diff paths.
            if crate::guardian::is_basic_session_source(&turn_context.session_source)
                || first_step_context
                    .turn
                    .config
                    .features
                    .enabled(Feature::CwdRelativeTurnDiffs)
            {
                first_step_context
                    .environments
                    .turn_environments()
                    .map(|environment| {
                        (
                            environment.selection().environment_id,
                            environment.cwd().clone(),
                        )
                    })
                    .collect()
            } else {
                turn_diff_display_roots(first_step_context.as_ref()).await
            }
        },
    );
    let mut world_state = record_context??;

    let Some((injection_items, explicitly_enabled_connectors)) = build_skills_and_plugins(
        &sess,
        first_step_context.as_ref(),
        &user_input,
        &mentioned_plugins,
        &cancellation_token,
    )
    .await
    else {
        return Ok(None);
    };

    if run_pending_session_start_hooks(&sess, &turn_context).await {
        return Ok(None);
    }
    if crate::guardian::is_basic_session_source(&turn_context.session_source)
        && let Err(error) = crate::guardian::finalize_guardian_input(
            &sess,
            &first_step_context,
            &mut input,
            codex_guardian_context::HistoryTruncation::Preserve,
        )
        .await
    {
        // Token-budget compaction resets history, which can discard the evidence
        // referenced by a pending delta review. Leave budget failures unreusable.
        if !auto_summarization_enabled
            || !matches!(error.details(), CodexErrorDetails::ContextWindowExceeded)
        {
            return Err(error);
        }
        // Incoming evidence can overflow even below the normal history
        // threshold. Keep it pending while compacting, then select once more.
        sess.services
            .thread_extension_data
            .insert(crate::guardian::ExhaustedReviewBudget::Compacting);
        run_auto_compact(
            &sess,
            Arc::clone(&first_step_context),
            Arc::clone(&first_step_context),
            &mut client_session,
            CompactionReason::ContextLimit,
            CompactionPhase::PreTurn,
            /*candidate*/ None,
        )
        .await?;
        world_state = sess
            .record_context_updates_and_set_reference_context_item(first_step_context.as_ref())
            .await?;
        crate::guardian::finalize_guardian_input(
            &sess,
            &first_step_context,
            &mut input,
            codex_guardian_context::HistoryTruncation::Allow,
        )
        .await?;
    }
    let mut can_drain_pending_input = input.is_empty();
    if run_hooks_and_record_inputs(
        &sess,
        &turn_context,
        &first_step_context.settings.model_info,
        &input,
        PersistContext::TurnStart,
    )
    .await?
    {
        return Ok(None);
    }

    // Guardian can replace its submitted prompt with response items during finalization.
    // Those skip UserInput telemetry above, so emit the saved prompt after acceptance.
    if crate::guardian::is_basic_session_source(&turn_context.session_source)
        && !user_input.is_empty()
        && input
            .iter()
            .all(|item| matches!(item, TurnInput::ResponseItem(_)))
    {
        turn_context.session_telemetry.user_prompt(&user_input);
    }

    // Only speculate after hooks accept the turn, using its finalized tools and permissions.
    {
        let mut state = sess.state.lock().await;
        if state.shell_snapshot_prewarm.is_none() {
            state.shell_snapshot_prewarm =
                sess.prewarm_shell_snapshots(first_step_context.as_ref());
        }
    }

    sess.merge_connector_selection(explicitly_enabled_connectors.clone())
        .await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: turn_context.model_info().slug.clone(),
        cyber_access_program: turn_context.cyber_access_program,
        comp_hash: turn_context.model_info().comp_hash.clone(),
        realtime_active: Some(turn_context.realtime_active),
    }))
    .await;
    for response_item in injection_items {
        sess.record_conversation_items(
            &turn_context,
            &first_step_context.settings.model_info,
            std::slice::from_ref(&response_item),
        )
        .await;
    }

    if super::async_compaction::enabled(&turn_context) {
        let token_status = super::context_window::context_window_token_status(
            sess.as_ref(),
            turn_context.as_ref(),
        )
        .await;
        super::async_compaction::maybe_start(
            &sess,
            &first_step_context,
            &world_state,
            CompactionPhase::PreTurn,
            &cancellation_token,
            &token_status,
            client_session.turn_state_with_auth_owner(),
        )
        .await;
    }
    track_turn_resolved_config_analytics(&sess, &first_step_context, &input).await;

    let mut last_agent_message: Option<String> = None;
    let mut stop_hook_active = false;
    // Although from the perspective of codex.rs, TurnDiffTracker has the lifecycle of a Task which contains
    // many turns, from the perspective of the user, it is a single turn.
    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(
        TurnDiffTracker::with_environment_display_roots(display_roots),
    ));

    // `ModelClientSession` is turn-scoped and caches WebSocket + sticky routing state, so we reuse
    // one instance across retries within this turn.
    // Pending input is drained into history before building the next model request.
    // However, we defer that drain until after sampling in two cases:
    // 1. At the start of a turn, so the fresh turn input in `input` gets sampled first.
    // 2. After auto-compact, while a model/tool continuation is pending.

    let mut next_step_context = Some(first_step_context);
    let mut guardian_budget_compacted = false;
    loop {
        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_input = if can_drain_pending_input {
            sess.input_queue
                .get_pending_input(&sess.active_turn)
                .await
                .0
        } else {
            Vec::new()
        };

        if run_hooks_and_record_inputs(
            &sess,
            &turn_context,
            &turn_context.capture_current_model_info(),
            &pending_input,
            PersistContext::SteeredUserInput,
        )
        .await?
        {
            break;
        }

        // Input and turn-start injections are recorded before recovery can continue this turn.
        turn_context.extension_data.insert(RecordedTurnInput);
        let window_id = sess.current_window_id().await;
        super::rollout_budget::maybe_record_reminder(
            sess.as_ref(),
            turn_context.as_ref(),
            &window_id,
        )
        .await;

        // Capture once so context, advertised tools, and tool calls share one request view.
        let step_context = match next_step_context.take() {
            Some(step_context) if pending_input.is_empty() => step_context,
            None if pending_input.is_empty() => {
                sess.capture_step_context_with_required_mcp_servers(
                    Arc::clone(&turn_context),
                    &cancellation_token,
                    required_servers,
                    required_plugins,
                )
                .await?
            }
            Some(_) | None => {
                let pending_user_input = turn_user_input(&pending_input);
                if allow_plugin_mentions {
                    required_plugins.extend(crate::plugins::collect_explicit_plugin_ids(
                        &pending_user_input,
                    ));
                }
                let (pending_required_servers, _) = required_mcp_servers_for_input(
                    &sess,
                    turn_context.as_ref(),
                    &pending_user_input,
                )
                .or_cancel(&cancellation_token)
                .await?;
                required_servers.extend(pending_required_servers);
                required_servers.sort_unstable();
                required_servers.dedup();
                sess.capture_step_context_with_required_mcp_servers(
                    Arc::clone(&turn_context),
                    &cancellation_token,
                    required_servers,
                    required_plugins,
                )
                .await?
            }
        };
        let sampling_request_result: CodexResult<_> = async {
            super::time_reminder::maybe_record_current_time_reminder(
                sess.as_ref(),
                turn_context.as_ref(),
                &window_id,
            )
            .await?;

            world_state = sess
                .record_step_world_state_if_changed(step_context.as_ref())
                .await?;

            // Isolated Guardian reviewers deliberately have no skill catalog.
            if !crate::guardian::is_basic_session_source(&turn_context.session_source) {
                validate_required_skills(
                    &step_context.extension_data,
                    step_context.environments.required_skills(),
                )
                .map_err(CodexErr::Fatal)?;
            }

            // Keep the override after accepted input so history truncation removes them together.
            sess.record_reasoning_effort_override(step_context.as_ref())
                .await;

            // Construct the input that we will send to the model.
            let sampling_request_input = sess
                .clone_history()
                .instrument(trace_span!("run_turn.prepare_sampling_request_input"))
                .await;

            run_sampling_request(
                Arc::clone(&sess),
                Arc::clone(&step_context),
                Arc::clone(&turn_context.extension_data),
                Arc::clone(&turn_diff_tracker),
                &mut client_session,
                sampling_request_input,
                cancellation_token.child_token(),
            )
            .await
        }
        .await;
        match sampling_request_result {
            Ok((sampling_request_output, notify_messages)) => {
                guardian_budget_compacted = false;
                let SamplingRequestResult {
                    needs_follow_up: model_needs_follow_up,
                    mailbox_preempted,
                    last_agent_message: sampling_request_last_agent_message,
                } = sampling_request_output;
                if model_needs_follow_up {
                    sess.input_queue
                        .accept_mailbox_delivery_for_current_turn(
                            &sess.active_turn,
                            &turn_context.sub_id,
                        )
                        .await;
                }
                can_drain_pending_input = true;
                // Process async hooks only after sampling and its tools have finished.
                drain_async_hook_results(&sess, &turn_context, /*before_user_prompt*/ false).await;
                let (has_pending_input, token_status) = async {
                    let has_pending_input =
                        sess.input_queue.has_pending_input(&sess.active_turn).await;
                    let token_status = super::context_window::context_window_token_status(
                        sess.as_ref(),
                        turn_context.as_ref(),
                    )
                    .await;
                    (has_pending_input, token_status)
                }
                .instrument(trace_span!("run_turn.collect_post_sampling_state"))
                .await;
                let needs_follow_up = model_needs_follow_up || has_pending_input;
                let token_limit_reached = token_status.token_limit_reached;

                super::async_compaction::maybe_start(
                    &sess,
                    &step_context,
                    &world_state,
                    if needs_follow_up {
                        CompactionPhase::MidTurn
                    } else {
                        CompactionPhase::PostTurn
                    },
                    &cancellation_token,
                    &token_status,
                    client_session.turn_state_with_auth_owner(),
                )
                .await;

                trace!(
                    turn_id = %turn_context.sub_id,
                    total_usage_tokens = token_status.active_context_tokens,
                    auto_compact_scope_tokens = token_status.auto_compact_scope_tokens,
                    auto_compact_scope_limit = ?token_status.auto_compact_scope_limit,
                    auto_compact_limit_scope = ?turn_context.config.model_auto_compact_token_limit_scope,
                    auto_compact_window_prefill_tokens = ?token_status.auto_compact_window_prefill_tokens,
                    full_context_window_limit = ?token_status.full_context_window_limit,
                    full_context_window_limit_reached = token_status.full_context_window_limit_reached,
                    token_limit_reached,
                    model_needs_follow_up,
                    has_pending_input,
                    needs_follow_up,
                    "post sampling token usage"
                );
                if tracing::event_enabled!(
                    target: POST_SAMPLING_TOKEN_ESTIMATE_TARGET,
                    tracing::Level::TRACE,
                    turn_id,
                    estimated_token_count,
                    message
                ) {
                    let estimated_token_count =
                        sess.get_estimated_token_count(turn_context.as_ref()).await;
                    trace!(
                        target: POST_SAMPLING_TOKEN_ESTIMATE_TARGET,
                        turn_id = %turn_context.sub_id,
                        estimated_token_count = ?estimated_token_count,
                        "post sampling token estimate"
                    );
                }

                let candidate = if needs_follow_up {
                    super::async_compaction::take_ready(&sess, &turn_context).await
                } else {
                    None
                };
                let should_roll_over = auto_compaction_enabled
                    && needs_follow_up
                    && (sess.take_new_context_window_request().await
                        || token_limit_reached
                        || candidate.is_some());
                if auto_compaction_enabled {
                    let allow_auto_compact_fallback = !should_roll_over && !token_limit_reached;
                    super::token_budget::maybe_record(
                        sess.as_ref(),
                        turn_context.as_ref(),
                        token_status.base_window_tokens_remaining,
                        allow_auto_compact_fallback,
                    )
                    .await;
                }

                // as long as compaction works well in getting us way below the token limit, we shouldn't worry about being in an infinite loop.
                if should_roll_over {
                    let compacted = run_auto_compact(
                        &sess,
                        Arc::clone(&step_context),
                        Arc::clone(&step_context),
                        &mut client_session,
                        CompactionReason::ContextLimit,
                        CompactionPhase::MidTurn,
                        candidate,
                    )
                    .await;
                    match compacted {
                        Ok(true) => {
                            if run_pending_session_start_hooks(&sess, &turn_context).await {
                                return Ok(None);
                            }
                            // After interrupted sampling, drain pending input even if compaction
                            // leaves the estimated context above the token limit.
                            can_drain_pending_input = !model_needs_follow_up
                                || mailbox_preempted
                                || step_context
                                    .preempt
                                    .as_ref()
                                    .is_some_and(CancellationToken::is_cancelled);
                            continue;
                        }
                        Ok(false) => {}
                        Err(err) => {
                            if matches!(err.details(), CodexErrorDetails::TurnAborted) {
                                return Err(err);
                            }
                            let error = err.to_codex_protocol_error();
                            sess.emit_turn_error_lifecycle(
                                turn_context.as_ref(),
                                error.clone(),
                                err.details(),
                            )
                            .await;
                            return Ok(None);
                        }
                    }
                }

                if !needs_follow_up {
                    last_agent_message = sampling_request_last_agent_message;
                    let stop_outcome = run_turn_stop_hooks(
                        &sess,
                        &step_context,
                        stop_hook_active,
                        last_agent_message.clone(),
                    )
                    .await;
                    if matches!(
                        turn_context.session_source,
                        SessionSource::Internal(InternalSessionSource::MemoryConsolidation)
                    ) && (stop_outcome.should_block || stop_outcome.should_stop)
                    {
                        // Do not feed managed rejections back into an unattended memory loop.
                        return Err(CodexErr::InvalidRequest(
                            "Memory consolidation was rejected by a Stop hook.".to_string(),
                        ));
                    }
                    if stop_outcome.should_block {
                        if let Some(hook_prompt_message) = build_hook_prompt_message(
                            &stop_outcome.continuation_fragments,
                            &sess.item_id_generator,
                        ) {
                            sess.record_response_item_and_emit_turn_item(
                                &turn_context,
                                &step_context.settings.model_info,
                                hook_prompt_message,
                            )
                            .await;
                            sess.input_queue
                                .accept_mailbox_delivery_for_current_turn(
                                    &sess.active_turn,
                                    &turn_context.sub_id,
                                )
                                .await;
                            stop_hook_active = true;
                            continue;
                        } else {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Stop hook requested continuation without a prompt; ignoring the block.".to_string(),
                                }),
                            )
                            .await;
                        }
                    }
                    if stop_outcome.should_stop {
                        break;
                    }
                    if run_legacy_after_agent_hook(
                        &sess,
                        &turn_context,
                        notify_messages,
                        last_agent_message.clone(),
                    )
                    .await
                    {
                        return Ok(None);
                    }
                    // Token-budget resets do not summarize, so preserve their existing rollover
                    // policy. Keep summarizing compaction in this task to serialize history updates.
                    let config = &turn_context.config;
                    if auto_summarization_enabled
                        && config.model_post_turn_compact_threshold_percent > 0
                        && !sess.input_queue.has_pending_input(&sess.active_turn).await
                        && !cancellation_token.is_cancelled()
                    {
                        let token_status = super::context_window::context_window_token_status(
                            sess.as_ref(),
                            turn_context.as_ref(),
                        )
                        .await;
                        let foreground_due = token_status.turn_end_compaction_threshold_reached;
                        let candidate =
                            super::async_compaction::take_ready(&sess, &turn_context).await;
                        if (foreground_due || candidate.is_some())
                            && let Err(err) = run_auto_compact(
                                &sess,
                                Arc::clone(&step_context),
                                Arc::clone(&step_context),
                                &mut client_session,
                                CompactionReason::ContextLimit,
                                CompactionPhase::PostTurn,
                                candidate,
                            )
                            .await
                        {
                            if matches!(
                                err.details(),
                                CodexErrorDetails::Interrupted | CodexErrorDetails::TurnAborted
                            ) {
                                return Err(err);
                            }
                            let error = err.to_codex_protocol_error();
                            if matches!(error, CodexErrorInfo::UsageLimitExceeded) {
                                // Preserve the completed answer while stopping automatic work.
                                sess.emit_turn_error_lifecycle(
                                    turn_context.as_ref(),
                                    error,
                                    err.details(),
                                )
                                .await;
                            }
                            warn!(error = %err, "Post-turn compaction failed; preserving the completed turn");
                        }
                    }
                    break;
                }
                continue;
            }
            Err(err)
                if auto_summarization_enabled
                    && matches!(err.details(), CodexErrorDetails::ContextWindowExceeded)
                    && !guardian_budget_compacted
                    && sess
                        .services
                        .thread_extension_data
                        .get::<crate::guardian::ExhaustedReviewBudget>()
                        .is_some() =>
            {
                // Tool continuations can also cross the complete-request limit.
                // Only summarizing compaction preserves the action and evidence;
                // token-budget resets must fail closed and retire the reviewer.
                // Retry once per model step, so ineffective compaction cannot loop.
                guardian_budget_compacted = true;
                sess.services
                    .thread_extension_data
                    .insert(crate::guardian::ExhaustedReviewBudget::Compacting);
                run_auto_compact(
                    &sess,
                    Arc::clone(&step_context),
                    Arc::clone(&step_context),
                    &mut client_session,
                    CompactionReason::ContextLimit,
                    CompactionPhase::MidTurn,
                    /*candidate*/ None,
                )
                .await?;
                can_drain_pending_input = false;
                continue;
            }
            Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
                return Err(err);
            }
            Err(codex_error)
                if matches!(
                    codex_error.details(),
                    CodexErrorDetails::InvalidImageRequest()
                ) =>
            {
                sess.track_turn_codex_error(turn_context.as_ref(), &codex_error);
                let error = CodexErrorInfo::BadRequest;
                sess.emit_turn_error_lifecycle(
                    turn_context.as_ref(),
                    error.clone(),
                    codex_error.details(),
                )
                .await;
                let event = EventMsg::Error(ErrorEvent {
                    misalignment: None,
                    message: "Invalid image in your last message. Please remove it and try again."
                        .to_string(),
                    codex_error_info: Some(error),
                    rejected_audio_file_ids: Vec::new(),
                });
                sess.send_event(&turn_context, event).await;
                break;
            }
            Err(e) => {
                info!("Turn error: {e:#}");
                if matches!(
                    e.details(),
                    CodexErrorDetails::MisalignmentPolicyViolation { .. }
                ) {
                    sess.conversation.retire_handoffs_for_misalignment().await;
                }
                let error = e.to_codex_protocol_error();
                sess.emit_turn_error_lifecycle(turn_context.as_ref(), error.clone(), e.details())
                    .await;
                sess.track_turn_codex_error(turn_context.as_ref(), &e);
                let mut error_event = e.to_error_event(/*message_prefix*/ None);
                if let Some(rejected) = turn_context
                    .extension_data
                    .remove::<rejected_audio::RejectedAudioFileIds>()
                {
                    error_event
                        .rejected_audio_file_ids
                        .extend(Arc::unwrap_or_clone(rejected).0);
                }
                if matches!(e.details(), CodexErrorDetails::InvalidAudioInput { .. })
                    || !error_event.rejected_audio_file_ids.is_empty()
                {
                    error_event.message.push_str(
                        " Please remove or replace the affected audio attachment and try again.",
                    );
                }
                let event = EventMsg::Error(error_event);
                sess.send_event(&turn_context, event).await;
                // let the user continue the conversation
                break;
            }
        }
    }

    Ok(last_agent_message)
}

#[instrument(level = "trace", skip_all)]
async fn turn_diff_display_roots(step_context: &StepContext) -> Vec<(String, PathUri)> {
    let mut display_roots = Vec::new();
    for turn_environment in step_context.environments.turn_environments() {
        let cwd = turn_environment.cwd();
        // A turn cwd is expected to be a directory. If it is a file, the failed `<cwd>/.git` probe
        // is ignored and ancestor search continues from its parent.
        let root = find_nearest_ancestor_with_markers(
            turn_environment.environment().get_filesystem().as_ref(),
            cwd,
            vec![".git".to_string()],
            FindUpErrorPolicy::Ignore,
            /*sandbox*/ None,
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| cwd.clone());
        display_roots.push((turn_environment.selection.environment_id.clone(), root));
    }
    display_roots
}

#[tracing::instrument(
    level = "trace",
    skip_all,
    fields(input_count = input.len())
)]
async fn track_turn_resolved_config_analytics(
    sess: &Session,
    first_step: &StepContext,
    input: &[TurnInput],
) {
    let turn_context = &first_step.turn;
    let thread_config = sess.thread_config_snapshot().await;
    let is_first_turn = {
        let mut state = sess.state.lock().await;
        state.take_next_turn_is_first()
    };
    sess.services
        .analytics_events_client
        .track_turn_resolved_config(TurnResolvedConfigFact {
            turn_id: turn_context.sub_id.clone(),
            thread_id: sess.thread_id.to_string(),
            turn_metadata: turn_context.turn_metadata_state.clone(),
            active_plugin_ids_at_turn_start: turn_context.active_plugin_ids_for_telemetry(
                first_step
                    .extension_data
                    .get::<codex_extension_api::SelectedPluginSnapshot>()
                    .as_deref(),
            ),
            num_input_images: input
                .iter()
                .filter_map(|item| match item {
                    TurnInput::UserInput { content, .. } => Some(content.as_slice()),
                    TurnInput::ResponseItem(_)
                    | TurnInput::FunctionCallOutput(_)
                    | TurnInput::InterAgentCommunication(_) => None,
                })
                .flatten()
                .filter(|item| {
                    matches!(item, UserInput::Image { .. } | UserInput::LocalImage { .. })
                })
                .count(),
            submission_type: None,
            ephemeral: thread_config.ephemeral,
            session_source: thread_config.session_source,
            model: turn_context.model_info().slug.clone(),
            model_provider: turn_context.config.model_provider_id.clone(),
            permission_profile: turn_context.permission_profile(),
            #[allow(deprecated)]
            permission_profile_cwd: turn_context.cwd.to_path_buf(),
            reasoning_effort: turn_context.reasoning_effort().cloned(),
            reasoning_summary: Some(turn_context.reasoning_summary()),
            service_tier: turn_context
                .config
                .service_tier
                .as_deref()
                .and_then(ServiceTier::from_request_value),
            approval_policy: turn_context.approval_policy(),
            approvals_reviewer: turn_context.config.approvals_reviewer,
            guardian_v2_enabled: sess
                .services
                .thread_extension_data
                .get::<codex_extension_api::GuardianV2Enabled>()
                .is_some(),
            sandbox_network_access: turn_context.network_sandbox_policy().is_enabled(),
            multi_agent_version: turn_context.multi_agent_version,
            collaboration_mode: turn_context.mode(),
            personality: turn_context.personality(),
            workspace_kind: turn_context.turn_metadata_state.workspace_kind(),
            is_first_turn,
        });
}
