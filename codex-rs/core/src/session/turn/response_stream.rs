//! Consuming one model response stream: output items, deltas, and in-flight tool calls.

use super::*;

#[instrument(level = "trace", skip_all)]
async fn drain_in_flight(
    in_flight: &mut FuturesOrdered<InFlightFuture<'static>>,
    sess: Arc<Session>,
    step_context: &StepContext,
) -> CodexResult<()> {
    let turn_context = &step_context.turn;
    while let Some(res) = in_flight.next().await {
        match res {
            Ok(envelope) => {
                mark_thread_memory_mode_polluted_if_external_context(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &envelope.item,
                )
                .await;
                sess.record_annotated_conversation_items(
                    turn_context,
                    &step_context.settings.model_info,
                    vec![envelope],
                )
                .await;
            }
            Err(err) => {
                error_or_panic(format!("in-flight tool future failed during drain: {err}"));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %step_context.turn.sub_id,
        model = %step_context.settings.model_info.slug
    )
)]
pub(super) async fn try_run_sampling_request(
    tool_runtime: ToolCallRuntime,
    sess: Arc<Session>,
    step_context: Arc<StepContext>,
    turn_store: Arc<codex_extension_api::ExtensionData>,
    client_session: &mut ModelClientSession,
    responses_metadata: &CodexResponsesMetadata,
    turn_diff_tracker: SharedTurnDiffTracker,
    prompt: Prompt,
    mut pending_audio: Option<codex_history::AcceptedFileAudio>,
    input_item_count: usize,
    cancellation_token: CancellationToken,
    draining_for_mailbox: &mut bool,
) -> CodexResult<SamplingRequestResult> {
    let turn_context = Arc::clone(&step_context.turn);
    feedback_tags!(
        model = step_context.settings.model_info.slug.clone(),
        approval_policy = turn_context.approval_policy(),
        sandbox_policy = &turn_context.sandbox_policy(),
        effort = step_context.settings.reasoning_effort(),
        auth_mode = sess.services.auth_manager.auth_mode(),
        tags_json = tracing::field::display(serde_json::json!(crate::feedback_config::usage_tags(
            &turn_context.config,
            &sess.features,
            &step_context.settings.model_info,
            step_context.settings.service_tier.as_deref(),
        ))),
    );
    let inference_trace = sess.services.rollout_thread_trace.inference_trace_context(
        turn_context.sub_id.as_str(),
        step_context.settings.model_info.slug.as_str(),
        turn_context.provider.info().name.as_str(),
    );
    if client_session.inference_tools_changed(&step_context.tool_router.model_visible_specs()) {
        turn_context.turn_timing_state.record_tools_change();
    }
    // Do not enter this span: overlapping tools must not retain it past sampling.
    let sampling_span = trace_span!(
        "codex.sampling",
        codex.turn.phase = "sampling",
        conversation.id = %sess.thread_id,
        turn.id = %turn_context.sub_id,
    );
    let uses_sequential_cutoff_reasoning_summaries = turn_context
        .config
        .features
        .enabled(Feature::ConcurrentReasoningSummaries)
        && turn_context.provider.info().is_openai();
    let mut preempt = step_context.preempt.clone().unwrap_or_default();
    let effort = sess
        .reasoning_effort_for_request(&step_context.settings, super::RequestEffortUsage::Sampling)
        .await;
    let hooks = hooks_for_model_request(&sess, &step_context);
    let post_mode = hooks.execution_mode_for_event(HookEventName::PostModelRequest);
    let defer_tool_dispatch = post_mode == Some(HookExecutionMode::Sync);
    let finish_post_request = post_mode.map(|_| {
        let (post_request, completed) = tokio::sync::oneshot::channel();
        let (hook_done, wait_for_hook) = tokio::sync::oneshot::channel();
        hooks.schedule_async_task({
            let hooks = Arc::clone(&hooks);
            let session = Arc::clone(&sess);
            let step = Arc::clone(&step_context);
            async move {
                let event = completed.await.unwrap_or(PostModelRequest {
                    status: ModelRequestStatus::Cancelled,
                    error: None,
                    token_usage: None,
                });
                run_model_request_hooks(&session, &step, &hooks, event).await;
                let _ = hook_done.send(());
            }
        });
        let cancellation_token = cancellation_token.clone();
        async move |event| {
            let _ = post_request.send(event);
            if defer_tool_dispatch {
                // Interrupt the wait, not the session-owned hook or final token accounting.
                let _ = wait_for_hook.or_cancel(&cancellation_token).await;
            }
        }
    });
    if hooks
        .execution_mode_for_event(HookEventName::PreModelRequest)
        .is_some()
    {
        run_model_request_hooks(&sess, &step_context, &hooks, PreModelRequest).await;
    }
    let sampling_timing_guard = turn_context.turn_timing_state.begin_sampling();
    let mut response_id_for_assessment = None;
    let mut answer_moderation: Option<codex_protocol::protocol::TargetedAnswerModeration> = None;
    let mut pending_assessment: Option<codex_api::GuardianAssessment> = None;
    let mut response_completed = false;
    let mut request_token_usage: Option<codex_protocol::protocol::TokenUsage> = None;
    let mut includes_reasoning = step_context.settings.model_info.use_responses_lite;
    // Catalog policies can enable adaptive review independently of the legacy scorer flag.
    // Capture evidence whenever Guardian is available; the reviewer resolves the policy.
    let assessment_authorization = if turn_context
        .config
        .features
        .enabled(Feature::GuardianApproval)
    {
        Some(crate::guardian::ScoreAuthorization::for_session(&sess, &Default::default()).await)
    } else {
        None
    };
    let mut resampling = crate::guardian::ResponseResampling::new(&sess);
    let mut stream_request = async |prompt: &Prompt| {
        client_session
            .stream(
                prompt,
                &step_context.settings.model_info,
                &step_context.session_telemetry,
                effort.clone(),
                step_context.settings.reasoning_summary,
                step_context.settings.service_tier.clone(),
                responses_metadata,
                &inference_trace,
            )
            .instrument(trace_span!("stream_request"))
            .await
    };
    let stream = stream_request(&prompt)
        .or_cancel(&cancellation_token)
        .await?;
    let mut stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            if let Some(finish_post_request) = finish_post_request {
                finish_post_request(PostModelRequest {
                    status: ModelRequestStatus::Failed,
                    error: Some(error.to_string()),
                    token_usage: None,
                })
                .await;
            }
            return Err(error);
        }
    };
    let can_interrupt = turn_context
        .config
        .features
        .enabled(Feature::InterruptOnCancel)
        && step_context.settings.model_info.use_responses_lite
        && stream.interrupt.is_some();
    if cancellation_token.is_cancelled() && !can_interrupt {
        return Err(CodexErr::TurnAborted);
    }
    let mut in_flight: FuturesOrdered<InFlightFuture<'static>> = FuturesOrdered::new();
    let mut needs_follow_up = false;
    let mut last_agent_message: Option<String> = None;
    let mut active_item: Option<TurnItem> = None;
    let mut active_tool_argument_diff_consumer: Option<(
        String,
        Box<dyn ToolArgumentDiffConsumer>,
    )> = None;
    let mut should_emit_turn_diff = false;
    let mut should_emit_token_count = false;
    const MAX_ANALYTICS_TOOL_CALL_IDS_PER_RESPONSE: usize = 256;
    let mut analytics_tool_call_ids = Vec::new();
    let reasoning_effort = step_context
        .settings
        .reasoning_effort()
        .or(step_context
            .settings
            .model_info
            .default_reasoning_level
            .as_ref())
        .map(std::string::ToString::to_string)
        .unwrap_or_else(|| "default".to_string());
    let plan_mode = turn_context.mode() == ModeKind::Plan;
    let mut assistant_message_stream_parsers = AssistantMessageStreamParsers::new(plan_mode);
    let mut plan_mode_state = plan_mode.then(|| PlanModeStreamState::new(&turn_context.sub_id));
    let defer_streamed_turn_items_for_contributors =
        !sess.services.extensions.turn_item_contributors().is_empty();
    let mut active_item_is_streaming_to_client = false;
    let receiving_span = trace_span!("receiving_stream");
    let outcome: CodexResult<SamplingRequestResult> = loop {
        if response_completed && resampling.accepted_output.is_empty() {
            if resampling.update_usage_boundary {
                sess.state.lock().await.history.reset_last_usage_boundary();
            }
            break Ok(SamplingRequestResult {
                needs_follow_up,
                mailbox_preempted: *draining_for_mailbox,
                last_agent_message,
            });
        }
        let handle_responses = trace_span!(
            parent: &receiving_span,
            "handle_responses",
            otel.name = field::Empty,
            tool_name = field::Empty,
            from = field::Empty,
            codex.request.reasoning_effort = %reasoning_effort,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.cache_read.input_tokens = field::Empty,
            gen_ai.usage.cache_write.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            codex.usage.reasoning_output_tokens = field::Empty,
            codex.usage.total_tokens = field::Empty,
            codex.response.receive_wait_ms = field::Empty,
        );

        let receive_started = Instant::now();
        let event = resampling
            .next(&mut stream)
            .instrument(trace_span!(parent: &handle_responses, "receiving"))
            .or_cancel(&preempt)
            .or_cancel(&cancellation_token)
            .await;
        // Split the existing iteration span without adding another span per streamed delta.
        handle_responses.record(
            "codex.response.receive_wait_ms",
            receive_started.elapsed().as_secs_f64() * 1000.0,
        );
        // Account for an already-received terminal error even if cancellation raced it.
        // Cancellation still stops ordinary stream events before they can run tools.
        let event = match event {
            Ok(Ok(event)) => event,
            Ok(Err(_)) => {
                if *draining_for_mailbox {
                    preempt = CancellationToken::new();
                    continue;
                }
                if let Some(interrupt) = stream.interrupt.take() {
                    if step_context.settings.model_info.use_responses_lite {
                        let _ = interrupt.send(codex_api::ResponseInterrupt::Drain);
                    }
                    // Drain the response normally before reusing its connection and history.
                    preempt = CancellationToken::new();
                    needs_follow_up = true;
                    continue;
                }
                // TODO: Reconcile any response item already being presented to the client.
                break Ok(SamplingRequestResult {
                    needs_follow_up: true,
                    mailbox_preempted: false,
                    last_agent_message,
                });
            }
            Err(_) => break Err(CodexErr::TurnAborted),
        };

        if cancellation_token.is_cancelled()
            && !matches!(event.as_ref(), Some(Err(err))
                if matches!(err.details(), CodexErrorDetails::ResponseIncomplete(_)))
        {
            break Err(CodexErr::TurnAborted);
        }

        let event = match event.unwrap_or_else(|| {
            Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
            ))
        }) {
            Ok(event) => event,
            Err(err) => {
                if let CodexErrorDetails::ResponseIncomplete(incomplete) = err.details() {
                    if let Some(usage) = &incomplete.token_usage {
                        request_token_usage
                            .get_or_insert_default()
                            .add_assign(usage);
                    }
                    sess.record_incomplete_response_usage(
                        &turn_context,
                        &step_context.settings,
                        incomplete,
                    )
                    .await;
                    should_emit_token_count |= incomplete.token_usage.is_some();
                }
                if *draining_for_mailbox && matches!(err.details(), CodexErrorDetails::Stream(_)) {
                    // Mail already requires a follow-up, even if the producer exited before
                    // receiving the interrupt. Do not retry without that pending input.
                    break Ok(SamplingRequestResult {
                        needs_follow_up: true,
                        mailbox_preempted: true,
                        last_agent_message,
                    });
                }
                break Err(err);
            }
        };

        record_turn_ttft_metric(&turn_context, &event).await;
        let Some(event) = resampling.filter(event, &step_context.tool_router) else {
            continue;
        };
        sess.services
            .session_telemetry
            .record_responses(&handle_responses, &event);

        match event {
            ResponseEvent::Created { response_id } => {
                answer_moderation = None;
                response_id_for_assessment.clone_from(&response_id);
                if let Some(response_id) = response_id {
                    turn_context
                        .extension_data
                        .insert(crate::guardian::ParentResponse {
                            id: response_id.clone(),
                            routing_affinity: stream.routing_affinity.clone(),
                        });
                    turn_context
                        .extension_data
                        .insert(codex_api::ResponseId(response_id));
                }
            }
            ResponseEvent::OutputItemDone(mut item) => {
                // Serving emits one final value immediately before this item's completion.
                // After this handoff, the invocation/cell owns it independently of the stream.
                let assessment = pending_assessment
                    .take()
                    .filter(|value| item.id().is_some_and(|id| id.as_str() == value.item_id))
                    .zip(assessment_authorization.as_ref())
                    .map(|(value, authorization)| {
                        Arc::new(crate::guardian::ServingAssessment {
                            requires_review: value.requires_review,
                            authorization: authorization.clone(),
                        })
                    });
                if item.id().is_none_or(|id| id.is_empty())
                    && let Some(active_item) = &active_item
                {
                    item.set_id(Some(ResponseItemId::from_server(active_item.id())));
                }
                sess.assign_missing_response_item_id(&mut item);
                sess.reserve_assistant_message_order(&turn_context, &item)
                    .await;
                if analytics_tool_call_ids.len() < MAX_ANALYTICS_TOOL_CALL_IDS_PER_RESPONSE {
                    let call_id = match &item {
                        ResponseItem::FunctionCall { call_id, .. }
                        | ResponseItem::CustomToolCall { call_id, .. } => Some(call_id.as_str()),
                        ResponseItem::ToolSearchCall { call_id, .. }
                        | ResponseItem::LocalShellCall { call_id, .. } => call_id.as_deref(),
                        ResponseItem::WebSearchCall { id, .. }
                        | ResponseItem::ImageGenerationCall { id, .. } => {
                            id.as_ref().map(codex_protocol::ResponseItemId::as_str)
                        }
                        _ => None,
                    };
                    if let Some(call_id) = call_id {
                        analytics_tool_call_ids.push(call_id.to_string());
                    }
                }
                if let Some((_, mut consumer)) = active_tool_argument_diff_consumer.take()
                    && let Ok(Some(event)) = consumer.finish()
                {
                    sess.send_event(&turn_context, event).await;
                }
                let previously_active_item = active_item.take();
                let previously_streamed_item = if active_item_is_streaming_to_client {
                    previously_active_item
                } else {
                    None
                };
                active_item_is_streaming_to_client = false;
                if let Some(previous) = previously_streamed_item.as_ref()
                    && matches!(previous, TurnItem::AgentMessage(_))
                {
                    let item_id = previous.id();
                    flush_assistant_text_segments_for_item(
                        &sess,
                        &turn_context,
                        plan_mode_state.as_mut(),
                        &mut assistant_message_stream_parsers,
                        &item_id,
                    )
                    .await;
                }
                if let Some(state) = plan_mode_state.as_mut()
                    && handle_assistant_item_done_in_plan_mode(
                        &sess,
                        &step_context,
                        turn_store.as_ref(),
                        &item,
                        state,
                        previously_streamed_item.as_ref(),
                        &mut last_agent_message,
                        answer_moderation.as_ref(),
                    )
                    .await
                {
                    continue;
                }

                let mut tool_runtime = tool_runtime.clone();
                if assessment.is_some() {
                    Arc::make_mut(&mut tool_runtime.step_context).serving_assessment = assessment;
                }
                let mut ctx = HandleOutputCtx {
                    sess: sess.clone(),
                    step_context: Arc::clone(&step_context),
                    turn_store: Arc::clone(&turn_store),
                    tool_runtime,
                    defer_tool_dispatch: defer_tool_dispatch || *draining_for_mailbox,
                    cancellation_token: cancellation_token.child_token(),
                };
                if *draining_for_mailbox {
                    // Retain buffered calls in history, but use the existing pre-dispatch
                    // cancellation path so they cannot run ahead of the queued mail.
                    ctx.cancellation_token.cancel();
                }

                let preempt_for_mailbox_mail = match &item {
                    ResponseItem::Message { role, phase, .. } => {
                        role == "assistant"
                            && matches!(
                                phase,
                                Some(MessagePhase::Commentary | MessagePhase::PartialAnswer)
                            )
                    }
                    ResponseItem::Reasoning { .. } => true,
                    ResponseItem::AgentMessage { .. } => false,
                    ResponseItem::AdditionalTools { .. }
                    | ResponseItem::LocalShellCall { .. }
                    | ResponseItem::FunctionCall { .. }
                    | ResponseItem::ToolSearchCall { .. }
                    | ResponseItem::FunctionCallOutput { .. }
                    | ResponseItem::CustomToolCall { .. }
                    | ResponseItem::CustomToolCallOutput { .. }
                    | ResponseItem::ToolSearchOutput { .. }
                    | ResponseItem::WebSearchCall { .. }
                    | ResponseItem::ImageGenerationCall { .. }
                    | ResponseItem::Compaction { .. }
                    | ResponseItem::ConfigurationUpdate { .. }
                    | ResponseItem::CompactionTrigger { .. }
                    | ResponseItem::ContextCompaction { .. }
                    | ResponseItem::Other => false,
                };

                let output_result = match handle_output_item_done(
                    &mut ctx,
                    item,
                    previously_streamed_item,
                    answer_moderation.as_ref(),
                )
                .instrument(handle_responses)
                .await
                {
                    Ok(output_result) => output_result,
                    Err(err) => break Err(err),
                };
                if let Some(tool_future) = output_result.tool_future {
                    in_flight.push_back(tool_future);
                }
                if let Some(agent_message) = output_result.last_agent_message {
                    last_agent_message = Some(agent_message);
                }
                needs_follow_up |= output_result.needs_follow_up;
                // Hosts can keep the current response intact and deliver mail before the next
                // model request instead of cutting off its remaining tool calls.
                if preempt_for_mailbox_mail
                    && !*draining_for_mailbox
                    && !turn_context
                        .config
                        .features
                        .enabled(Feature::DeferMailboxPreemption)
                    && sess
                        .input_queue
                        .should_preempt_sampling_for_mail(&sess.active_turn, &turn_context.sub_id)
                        .await
                {
                    tracing::event!(
                        name: "codex.mailbox_preemption",
                        target: "codex_otel.trace_safe",
                        tracing::Level::INFO,
                        event.name = "codex.mailbox_preemption",
                        conversation.id = %sess.thread_id,
                        turn.id = %turn_context.sub_id,
                    );
                    if step_context.settings.model_info.use_responses_lite
                        && let Some(interrupt) = stream.interrupt.take()
                    {
                        // A closed receiver can mean completion is already buffered. Drain it
                        // too; a transport failure below still takes the mailbox follow-up.
                        let _ = interrupt.send(codex_api::ResponseInterrupt::Drain);
                        // Retain completed items and the response ID for incremental continuation.
                        // Further item boundaries must not drop the stream while it drains.
                        *draining_for_mailbox = true;
                        needs_follow_up = true;
                        continue;
                    }
                    break Ok(SamplingRequestResult {
                        needs_follow_up: true,
                        mailbox_preempted: true,
                        last_agent_message,
                    });
                }
            }
            ResponseEvent::OutputItemAdded(mut item) => {
                sess.assign_missing_response_item_id(&mut item);
                sess.reserve_assistant_message_order(&turn_context, &item)
                    .await;
                if !*draining_for_mailbox
                    && let ResponseItem::CustomToolCall {
                        call_id,
                        name,
                        namespace,
                        ..
                    } = &item
                {
                    let tool_name = ToolName::new(namespace.clone(), name.as_str());
                    active_tool_argument_diff_consumer = tool_runtime
                        .create_diff_consumer(&tool_name)
                        .map(|consumer| (call_id.clone(), consumer));
                } else if matches!(&item, ResponseItem::FunctionCall { .. }) {
                    active_tool_argument_diff_consumer = None;
                }
                if let Some(turn_item) = handle_non_tool_response_item(
                    sess.as_ref(),
                    TurnItemContributorPolicy::Skip,
                    &item,
                    plan_mode,
                )
                .await
                {
                    let mut turn_item = turn_item;
                    // An interrupted response may discard its partial item. Only expose
                    // items received during this drain once OutputItemDone confirms them.
                    let stream_item_to_client =
                        !*draining_for_mailbox && !defer_streamed_turn_items_for_contributors;
                    let mut seeded_parsed: Option<ParsedAssistantTextDelta> = None;
                    let mut seeded_item_id: Option<String> = None;
                    if stream_item_to_client
                        && matches!(turn_item, TurnItem::AgentMessage(_))
                        && let Some(raw_text) = raw_assistant_output_text_from_item(&item)
                    {
                        let item_id = turn_item.id();
                        let mut seeded =
                            assistant_message_stream_parsers.seed_item_text(&item_id, &raw_text);
                        if let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                            agent_message.content =
                                vec![codex_protocol::items::AgentMessageContent::Text {
                                    text: if plan_mode {
                                        String::new()
                                    } else {
                                        std::mem::take(&mut seeded.visible_text)
                                    },
                                }];
                        }
                        seeded_parsed = plan_mode.then_some(seeded);
                        seeded_item_id = Some(item_id);
                    }
                    if stream_item_to_client {
                        if let Some(state) = plan_mode_state.as_mut()
                            && matches!(turn_item, TurnItem::AgentMessage(_))
                        {
                            let item_id = turn_item.id();
                            state
                                .pending_agent_message_items
                                .insert(item_id, turn_item.clone());
                        } else {
                            sess.emit_turn_item_started(&turn_context, &turn_item).await;
                        }
                        if let (Some(state), Some(item_id), Some(parsed)) = (
                            plan_mode_state.as_mut(),
                            seeded_item_id.as_deref(),
                            seeded_parsed,
                        ) {
                            emit_streamed_assistant_text_delta(
                                &sess,
                                &turn_context,
                                Some(state),
                                item_id,
                                parsed,
                            )
                            .await;
                        }
                    }
                    active_item = Some(turn_item);
                    active_item_is_streaming_to_client = stream_item_to_client;
                }
            }
            ResponseEvent::ServerModel(server_model) => {
                if !turn_context
                    .server_model_warning_emitted
                    .load(Ordering::Relaxed)
                    && sess
                        .maybe_warn_on_server_model_mismatch(&step_context, server_model)
                        .await
                {
                    turn_context
                        .server_model_warning_emitted
                        .store(true, Ordering::Relaxed);
                }
            }
            ResponseEvent::ModelVerifications(verifications) => {
                if !turn_context
                    .model_verification_emitted
                    .swap(true, Ordering::Relaxed)
                {
                    sess.emit_model_verification(&turn_context, verifications)
                        .await;
                }
            }
            ResponseEvent::TurnModerationMetadata(metadata) => {
                answer_moderation = metadata
                    .metadata
                    .get("final_answer_moderation")
                    .and_then(|marker| serde::Deserialize::deserialize(marker).ok());
                sess.emit_turn_moderation_metadata(&turn_context, metadata)
                    .await;
            }
            ResponseEvent::GuardianAssessment(assessment) => {
                if response_id_for_assessment.as_deref() == Some(assessment.response_id.as_str()) {
                    pending_assessment = Some(assessment);
                }
            }
            ResponseEvent::SafetyBuffering(buffering) => {
                sess.send_event(
                    &turn_context,
                    EventMsg::SafetyBuffering(SafetyBufferingEvent {
                        model: step_context.settings.model_info.slug.clone(),
                        use_cases: buffering.use_cases,
                        reasons: buffering.reasons,
                        show_buffering_ui: buffering.show_buffering_ui,
                        faster_model: buffering.faster_model,
                    }),
                )
                .await;
            }
            ResponseEvent::ServerReasoningIncluded(included) => {
                includes_reasoning = included;
            }
            ResponseEvent::RateLimits(snapshot) => {
                // Update internal state with latest rate limits, but defer sending until
                // token usage is available to avoid duplicate TokenCount events.
                sess.record_rate_limits_info(snapshot).await;
                should_emit_token_count = true;
            }
            ResponseEvent::ModelsEtag(etag) => {
                // Update internal state with latest models etag
                sess.services
                    .models_manager
                    .refresh_if_new_etag(etag, turn_context.config.http_client_factory())
                    .await;
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
                usage_metadata,
                end_turn,
            } => {
                if let Some(accepted) = pending_audio.take() {
                    sess.record_retained_context(
                        codex_history::RetainedContextEvent::AcceptedFileAudio {
                            evidence: Arc::new(accepted),
                        },
                    )
                    .await;
                }
                response_completed = true;
                if let Some(usage) = &token_usage {
                    request_token_usage
                        .get_or_insert_default()
                        .add_assign(usage);
                }
                sess.services
                    .analytics_events_client
                    .track_code_mode_tool_call(
                        codex_analytics::CodeModeToolCallFact::SamplingResponseCompleted {
                            thread_id: sess.thread_id.to_string(),
                            turn_id: turn_context.sub_id.clone(),
                            response_id: response_id.clone(),
                            tool_call_ids: std::mem::take(&mut analytics_tool_call_ids),
                        },
                    );
                flush_assistant_text_segments_all(
                    &sess,
                    &turn_context,
                    plan_mode_state.as_mut(),
                    &mut assistant_message_stream_parsers,
                )
                .await;
                sess.record_observed_response_completed(
                    &turn_context,
                    &response_id,
                    token_usage.as_ref(),
                    usage_metadata.as_ref(),
                )
                .await;
                let budget_result = sess
                    .record_token_usage_info(
                        &turn_context,
                        &step_context.settings,
                        token_usage.as_ref(),
                        if resampling.retry_pending {
                            super::TokenUsageContext::SpendingOnly
                        } else {
                            super::TokenUsageContext::Sampling {
                                includes_reasoning,
                                input_item_count,
                            }
                        },
                    )
                    .await;
                should_emit_token_count = true;
                should_emit_turn_diff = true;
                if let Err(err) = budget_result {
                    break Err(err);
                }
                if resampling.retry_pending {
                    response_completed = false;
                    response_id_for_assessment = None;
                    pending_assessment = None;
                    includes_reasoning = step_context.settings.model_info.use_responses_lite;
                    stream = match resampling
                        .retry(stream_request(&prompt), &cancellation_token)
                        .await
                    {
                        Ok(stream) => stream,
                        Err(error) => break Err(error),
                    };
                    continue;
                }
                if let Some(false) = end_turn {
                    needs_follow_up = true;
                }
            }
            ResponseEvent::OutputTextDelta(delta) => {
                // In review child threads, suppress assistant text deltas; the
                // UI will show a selection popup from the final ReviewOutput.
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let item_id = active.id();
                    if matches!(active, TurnItem::AgentMessage(_)) {
                        let parsed = assistant_message_stream_parsers.parse_delta(&item_id, &delta);
                        emit_streamed_assistant_text_delta(
                            &sess,
                            &turn_context,
                            plan_mode_state.as_mut(),
                            &item_id,
                            parsed,
                        )
                        .await;
                    } else {
                        let event = AgentMessageContentDeltaEvent {
                            thread_id: sess.thread_id.to_string(),
                            turn_id: turn_context.sub_id.clone(),
                            item_id,
                            delta,
                        };
                        sess.send_event(&turn_context, EventMsg::AgentMessageContentDelta(event))
                            .await;
                    }
                } else {
                    error_or_panic("OutputTextDelta without active item".to_string());
                }
            }
            ResponseEvent::ToolCallInputDelta {
                item_id: _,
                call_id,
                delta,
            } => {
                let Some((active_call_id, consumer)) = active_tool_argument_diff_consumer.as_mut()
                else {
                    continue;
                };
                let call_id = match call_id {
                    Some(call_id) if call_id.as_str() != active_call_id.as_str() => continue,
                    Some(call_id) => call_id,
                    None => active_call_id.clone(),
                };
                if let Some(event) = consumer.consume_diff(turn_context.as_ref(), call_id, &delta) {
                    sess.send_event(&turn_context, event).await;
                }
            }
            ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => {
                if uses_sequential_cutoff_reasoning_summaries {
                    continue;
                }
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let event = ReasoningContentDeltaEvent {
                        thread_id: sess.thread_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        summary_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningSummaryDelta without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
                if uses_sequential_cutoff_reasoning_summaries {
                    continue;
                }
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let event =
                        EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {
                            item_id: active.id(),
                            summary_index,
                        });
                    sess.send_event(&turn_context, event).await;
                } else {
                    error_or_panic("ReasoningSummaryPartAdded without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryDone {
                item_id,
                text,
                summary_index,
            } => {
                if !uses_sequential_cutoff_reasoning_summaries {
                    continue;
                }
                let Some(active) = active_item.as_ref() else {
                    continue;
                };
                if !active_item_is_streaming_to_client || active.id() != item_id {
                    continue;
                }
                if summary_index > 0 {
                    sess.send_event(
                        &turn_context,
                        EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {
                            item_id: item_id.clone(),
                            summary_index,
                        }),
                    )
                    .await;
                }
                let event = ReasoningContentDeltaEvent {
                    thread_id: sess.thread_id.to_string(),
                    turn_id: turn_context.sub_id.clone(),
                    item_id,
                    delta: text,
                    summary_index,
                };
                sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                    .await;
            }
            ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let event = ReasoningRawContentDeltaEvent {
                        thread_id: sess.thread_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        content_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningRawContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningRawContentDelta without active item".to_string());
                }
            }
        }
    };
    if cancellation_token.is_cancelled() && !response_completed && can_interrupt {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        if let Some(interrupt) = stream.interrupt.take() {
            let _ = interrupt.send(codex_api::ResponseInterrupt::Cancel { deadline });
        }
    }
    drop(stream);
    drop(sampling_span);
    drop(sampling_timing_guard);
    if let Some(finish_post_request) = finish_post_request {
        let (status, error) = match (response_completed, &outcome) {
            (true, _) => (ModelRequestStatus::Completed, None),
            (false, Err(error)) if !matches!(error.details(), CodexErrorDetails::TurnAborted) => {
                (ModelRequestStatus::Failed, Some(error.to_string()))
            }
            _ => (ModelRequestStatus::Cancelled, None),
        };
        finish_post_request(PostModelRequest {
            status,
            error,
            token_usage: request_token_usage,
        })
        .await;
    }
    flush_assistant_text_segments_all(
        &sess,
        &turn_context,
        plan_mode_state.as_mut(),
        &mut assistant_message_stream_parsers,
    )
    .await;

    if !in_flight.is_empty() {
        let _tool_blocking_timing_guard = turn_context
            .turn_timing_state
            .begin_tool_blocking(tool_runtime.batch_timing());
        let _tool_blocking_span = trace_span!(
            "codex.tool_blocking",
            codex.turn.phase = "tool_blocking",
            conversation.id = %sess.thread_id,
            turn.id = %turn_context.sub_id,
        );
        drain_in_flight(&mut in_flight, sess.clone(), &step_context).await?;
    }

    if should_emit_token_count {
        // A tool call such as request_user_input can intentionally pause the turn. Emit token
        // counts only after pending tools resolve so clients do not see progress events while the
        // turn is waiting on the user. This also needs to happen before returning cancellation so
        // token usage already recorded from the completed response is still persisted.
        sess.send_token_count_event(&turn_context).await;
    }

    if cancellation_token.is_cancelled() {
        return Err(CodexErr::TurnAborted);
    }

    if should_emit_turn_diff {
        let unified_diff = {
            let tracker = turn_diff_tracker.lock().await;
            tracker.get_unified_diff()
        };
        if let Some(unified_diff) = unified_diff {
            let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
            sess.clone().send_event(&turn_context, msg).await;
        }
    }

    outcome
}
