//! Prompt and tool router construction for a sampling request, plus its retry loop.

use super::*;

#[instrument(level = "trace", skip_all)]
pub(crate) fn build_prompt(
    input: Vec<ResponseItem>,
    step_context: &StepContext,
    base_instructions: BaseInstructions,
    incremental_tools: bool,
) -> Prompt {
    let turn_context = &step_context.turn;
    Prompt {
        input,
        tools: if incremental_tools {
            // Hosted tools still require top-level Responses configuration.
            step_context
                .tool_router
                .model_visible_specs()
                .iter()
                .filter(|tool| matches!(tool, ToolSpec::WebSearch { .. }))
                .cloned()
                .collect()
        } else {
            step_context.tool_router.model_visible_specs()
        },
        parallel_tool_calls: true,
        base_instructions: if incremental_tools {
            BaseInstructions {
                text: String::new(),
                provenance: None,
            }
        } else {
            base_instructions
        },
        output_schema: turn_context.final_output_json_schema.clone(),
        output_schema_strict: !crate::guardian::is_basic_session_source(
            &turn_context.session_source,
        ),
        prompt_cache_ttl: turn_context.prompt_cache_ttl,
        cyber_access_program: turn_context.cyber_access_program,
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(deprecated)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %step_context.turn.sub_id,
        model = %step_context.settings.model_info.slug,
        cwd = %step_context.turn.cwd.display()
    )
)]
pub(super) async fn run_sampling_request(
    sess: Arc<Session>,
    step_context: Arc<StepContext>,
    turn_store: Arc<codex_extension_api::ExtensionData>,
    turn_diff_tracker: SharedTurnDiffTracker,
    client_session: &mut ModelClientSession,
    input: crate::context_manager::ContextManager,
    cancellation_token: CancellationToken,
) -> CodexResult<(SamplingRequestResult, Vec<String>)> {
    let turn_context = Arc::clone(&step_context.turn);
    let preempt = step_context.preempt.clone().unwrap_or_default();
    let _input_watch = if let Some(preempt) = &step_context.preempt {
        sess.input_queue
            .watch_user_input(&sess.active_turn, &turn_context.sub_id, preempt.clone())
            .await
    } else {
        None
    };
    let base_instructions = sess.get_prompt_base_instructions().await;

    let tool_runtime = ToolCallRuntime::new(
        Arc::clone(&sess),
        Arc::clone(&step_context),
        Arc::clone(&turn_diff_tracker),
    );
    let _code_mode_worker = sess.services.code_mode_service.start_turn_worker(
        &sess,
        Arc::clone(&step_context),
        Arc::clone(&turn_diff_tracker),
    );
    let max_retries = turn_context.provider.info().stream_max_retries();
    let incremental_tools = sess
        .current_window_uses_incremental_tools(&step_context)
        .await;
    record_registration_metrics(
        &step_context.tool_router,
        &step_context.session_telemetry,
        incremental_tools,
    );
    let mut retry_state = ResponsesStreamRetryState::default();
    let mut initial_input = Some(input);
    let mut notify_messages = Vec::new();
    let mut executed_tool_calls_by_output = HashMap::new();
    loop {
        // Running code-mode cells can request review while this response is in flight.
        // Keep the latest received ID until response.created replaces it.
        let first_attempt = initial_input.is_some();
        let history = if let Some(input) = initial_input.take() {
            input
        } else {
            sess.clone_history().await
        };
        let input_item_count = history.raw_items().len();
        let sampling_step_context = Arc::new(StepContext {
            sampled_through_sequence: Some(history.input_sequence()),
            ..step_context.as_ref().clone()
        });
        let mut prompt_input = history
            .for_prompt(
                &step_context.settings.model_info.input_modalities,
                sess.services.image_store.as_ref(),
            )
            .await;
        sess.services
            .executed_tool_calls
            .attach_to_prompt(&mut prompt_input, &mut executed_tool_calls_by_output);
        let mut prompt = build_prompt(
            prompt_input,
            step_context.as_ref(),
            base_instructions.clone(),
            incremental_tools,
        );
        let responses_metadata = sess
            .responses_metadata(step_context.as_ref(), CodexResponsesRequestKind::Turn)
            .await;
        if crate::guardian::is_basic_session_source(&turn_context.session_source) {
            crate::guardian::prepare_guardian_prompt(
                &sess,
                &mut prompt,
                step_context.as_ref(),
                &responses_metadata,
            )
            .await?;
        }
        if first_attempt && turn_context.config.notify.is_some() {
            notify_messages = legacy_notify_input_messages(&prompt.input);
        }
        let _native_search = crate::model_search::SearchOwner::reserve_server_fallback(
            &step_context.extension_data,
            &sess.services.thread_extension_data,
            &mut prompt.tools,
        );
        let (submitted_audio, pending_audio) = {
            let model = &step_context.settings.model_info.slug;
            let provider = &turn_context.config.model_provider_id;
            let history = sess.clone_history().await;
            let accepted = history
                .retained_context()
                .accepted_file_audio(model, provider);
            let pending = rejected_audio::pending_audio_confirmation(
                accepted,
                &turn_context.sub_id,
                model,
                provider,
                &prompt.input,
            );
            let clear_checkpoints = history.annotated_items().iter().filter_map(|envelope| {
                rejected_audio::clear_checkpoint(&envelope.item, envelope.metadata.as_ref())
            });
            let submitted = rejected_audio::SubmittedFileAudio::from_request(
                &prompt.input,
                accepted,
                clear_checkpoints,
            );
            (submitted, pending)
        };
        let mut draining_for_mailbox = false;
        let err = match try_run_sampling_request(
            tool_runtime.clone(),
            Arc::clone(&sess),
            sampling_step_context,
            Arc::clone(&turn_store),
            client_session,
            &responses_metadata,
            Arc::clone(&turn_diff_tracker),
            prompt,
            pending_audio,
            input_item_count,
            cancellation_token.child_token(),
            &mut draining_for_mailbox,
        )
        .await
        {
            Ok(output) => {
                return Ok((output, notify_messages));
            }
            Err(err) => match err.details() {
                CodexErrorDetails::ContextWindowExceeded => {
                    sess.set_total_tokens_full(&turn_context).await;
                    return Err(err);
                }
                CodexErrorDetails::UsageLimitReached(e) => {
                    let rate_limits = e.rate_limits.clone();
                    if let Some(rate_limits) = rate_limits {
                        sess.update_rate_limits(&turn_context, *rate_limits).await;
                    }
                    return Err(err);
                }
                CodexErrorDetails::SessionBudgetExceeded | CodexErrorDetails::Fatal(_) => {
                    return Err(err);
                }
                _ => err,
            },
        };

        // A steer can preempt retry handling. Finish this terminal failure with the
        // identity from the submitted request before draining any new recordings.
        if let Some(audio) = submitted_audio {
            let ids = audio
                .rejected_file_ids(err.details(), sess.services.image_store.as_ref())
                .await;
            if !ids.is_empty() {
                turn_context
                    .extension_data
                    .insert(rejected_audio::RejectedAudioFileIds(ids));
                return Err(err);
            }
        }
        let retry = handle_response_stream_error(
            &mut retry_state,
            max_retries,
            err,
            client_session,
            &sess,
            &step_context,
            ResponsesStreamRequest::Sampling,
        )
        .or_cancel(&preempt)
        .or_cancel(&cancellation_token)
        .await?;
        if cancellation_token.is_cancelled() {
            return Err(CodexErr::TurnAborted);
        }
        // Recover typed errors (including filter guidance) before handing queued mail back
        // to the turn loop. Terminal errors and exhausted retries must still propagate.
        if preempt.is_cancelled() || (draining_for_mailbox && matches!(retry, Ok(Ok(())))) {
            return Ok((
                SamplingRequestResult {
                    needs_follow_up: true,
                    mailbox_preempted: draining_for_mailbox,
                    last_agent_message: None,
                },
                notify_messages,
            ));
        }
        retry??;
        turn_context.turn_timing_state.record_sampling_retry();
    }
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn prepare_tool_recommendations(
    sess: &Session,
    turn_context: &TurnContext,
) -> PreparedToolRecommendations {
    let loaded_plugins = sess
        .services
        .plugins_manager
        .plugins_for_config(&turn_context.config.plugins_config_input())
        .await
        .without_plugins(&turn_context.disabled_plugin_ids);
    let tool_suggest_is_enabled = tool_suggest_enabled(turn_context);
    let auth = if tool_suggest_is_enabled {
        sess.services.auth_manager.auth().await
    } else {
        None
    };
    let endpoint_candidates = if tool_suggest_is_enabled {
        let plugins_config = turn_context.config.plugins_config_input();
        sess.services
            .plugins_manager
            .recommended_plugin_candidates_for_config(RecommendedPluginCandidatesInput {
                plugins_config: &plugins_config,
                loaded_plugins: &loaded_plugins,
                auth: auth.as_ref(),
                disabled_tools: &turn_context.config.tool_suggest.disabled_tools,
                app_server_client_name: turn_context.app_server_client_name.as_deref(),
            })
            .await
    } else {
        None
    };

    PreparedToolRecommendations {
        auth,
        endpoint_candidates,
    }
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %model_info.slug,
        apps_enabled = turn_context.apps_enabled()
    )
)]
pub(crate) async fn built_tools(
    sess: &Session,
    turn_context: &TurnContext,
    model_info: &codex_protocol::openai_models::ModelInfo,
    environments: &TurnEnvironmentSnapshot,
    mcp: &Arc<codex_mcp::McpBinding>,
    step_store: &ExtensionData,
    prepared_recommendations: PreparedToolRecommendations,
    dynamic_tools: &[codex_protocol::dynamic_tools::DynamicToolSpec],
) -> CodexResult<Arc<ToolRouter>> {
    let all_mcp_tools = mcp.tools();
    let connector_snapshot = mcp.config().connector_snapshot.clone();

    let apps_enabled = turn_context.apps_enabled();
    let accessible_connectors =
        apps_enabled.then(|| connectors::accessible_connectors_from_mcp_tools(all_mcp_tools));
    let tool_suggest_is_enabled = tool_suggest_enabled(turn_context);
    let PreparedToolRecommendations {
        auth,
        endpoint_candidates: endpoint_recommended_plugin_candidates,
    } = prepared_recommendations;
    let tool_suggest_candidates =
        if let Some(recommended_plugin_candidates) = endpoint_recommended_plugin_candidates {
            Some(ToolSuggestCandidates {
                tools: recommended_plugin_candidates,
                presentation: ToolSuggestPresentation::RecommendationContext,
            })
        } else {
            let loaded_plugin_app_connector_ids = connector_snapshot
                .connector_ids()
                .iter()
                .map(|connector_id| connector_id.0.clone())
                .collect::<Vec<_>>();
            async {
                if apps_enabled && tool_suggest_is_enabled {
                    if let Some(accessible_connectors) = accessible_connectors.as_ref() {
                        match connectors::list_tool_suggest_discoverable_tools_with_auth(
                            &turn_context.config,
                            sess.services.plugins_manager.as_ref(),
                            auth.as_ref(),
                            accessible_connectors.as_slice(),
                            &loaded_plugin_app_connector_ids,
                        )
                        .await
                        .map(|discoverable_tools| {
                            filter_request_plugin_install_discoverable_tools_for_client(
                                discoverable_tools,
                                turn_context.app_server_client_name.as_deref(),
                            )
                        }) {
                            Ok(discoverable_tools) if discoverable_tools.is_empty() => None,
                            Ok(discoverable_tools) => Some(ToolSuggestCandidates {
                                tools: discoverable_tools,
                                presentation: ToolSuggestPresentation::ListTool,
                            }),
                            Err(err) => {
                                warn!("failed to load discoverable tool suggestions: {err:#}");
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            .instrument(trace_span!("built_tools.load_discoverable_tools"))
            .await
        };
    Ok(Arc::new(build_tool_router(
        sess,
        turn_context,
        model_info,
        environments,
        mcp,
        apps_enabled,
        step_store,
        tool_suggest_candidates.as_ref(),
        dynamic_tools,
    )?))
}
