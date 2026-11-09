//! Per-turn facts (config, usage, profile, errors, images) and `codex_turn_event` builders.

use super::*;

impl AnalyticsReducer {
    pub(super) async fn ingest_turn_resolved_config(
        &mut self,
        mut input: TurnResolvedConfigFact,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let turn_id = input.turn_id.clone();
        let thread_id = input.thread_id.clone();
        let num_input_images = input.num_input_images;
        let turn_state = self.turn_state(turn_id.clone());
        turn_state.thread_id = Some(thread_id);
        turn_state.num_input_images = Some(num_input_images);
        // Keep the first received plugin inventory, including unknown or empty,
        // while the remaining resolved config continues updating.
        if let Some(initial_config) = &turn_state.resolved_config {
            input.active_plugin_ids_at_turn_start =
                initial_config.active_plugin_ids_at_turn_start.clone();
        }
        turn_state.resolved_config = Some(input);
        self.maybe_emit_turn_event(&turn_id, out).await;
    }

    pub(super) async fn ingest_turn_token_usage(
        &mut self,
        input: TurnTokenUsageFact,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let turn_id = input.turn_id.clone();
        let turn_state = self.turn_state(turn_id.clone());
        turn_state.thread_id = Some(input.thread_id);
        turn_state.token_usage = Some(input.token_usage);
        self.maybe_emit_turn_event(&turn_id, out).await;
    }

    pub(super) async fn ingest_turn_profile(
        &mut self,
        input: TurnProfileFact,
        out: &mut Vec<TrackEventRequest>,
    ) {
        let TurnProfileFact { turn_id, profile } = input;
        let turn_state = self.turn_state(turn_id.clone());
        turn_state.profile = Some(profile);
        self.maybe_emit_turn_event(&turn_id, out).await;
    }

    pub(super) fn ingest_turn_codex_error(&mut self, input: TurnCodexErrorFact) {
        let TurnCodexErrorFact {
            turn_id,
            thread_id,
            error,
        } = input;
        let turn_state = self.turn_state(turn_id);
        turn_state.thread_id.get_or_insert(thread_id);
        turn_state.codex_error = Some(error);
    }

    pub(super) fn ingest_image_preparation(&mut self, input: ImagePreparationFact) {
        let turn_state = self.turn_state(input.turn_id);
        turn_state.image_preparations.push(input.metadata);
    }
}

pub(super) fn accepted_line_event_input(
    turn_id: &str,
    turn_state: &TurnState,
) -> Option<(AcceptedLineFingerprintEventInput, PathBuf)> {
    let latest_diff = turn_state.latest_diff.as_deref()?;
    let summary = accepted_line_counts_from_unified_diff(latest_diff);
    if summary.accepted_added_lines == 0 && summary.accepted_deleted_lines == 0 {
        return None;
    }

    let thread_id = turn_state.thread_id.clone()?;
    let resolved_config = turn_state.resolved_config.clone()?;

    Some((
        AcceptedLineFingerprintEventInput {
            event_type: "codex.accepted_line_fingerprints",
            turn_id: turn_id.to_string(),
            thread_id,
            product_surface: Some("codex".to_string()),
            model_slug: Some(resolved_config.model.clone()),
            completed_at: now_unix_seconds(),
            repo_hash: None,
            accepted_added_lines: summary.accepted_added_lines,
            accepted_deleted_lines: summary.accepted_deleted_lines,
        },
        resolved_config.permission_profile_cwd,
    ))
}

pub(super) fn codex_turn_event_params(
    app_server_client: CodexAppServerClientMetadata,
    runtime: CodexRuntimeMetadata,
    turn_id: String,
    turn_state: &TurnState,
    thread_metadata: &ThreadMetadataState,
) -> CodexTurnEventParams {
    let (
        Some(thread_id),
        Some(num_input_images),
        Some(resolved_config),
        Some(profile),
        Some(completed),
    ) = (
        turn_state.thread_id.clone(),
        turn_state.num_input_images,
        turn_state.resolved_config.clone(),
        turn_state.profile.clone(),
        turn_state.completed.clone(),
    )
    else {
        unreachable!("turn event params require a fully populated turn state");
    };
    let started_at = turn_state.started_at;
    let TurnResolvedConfigFact {
        turn_id: _resolved_turn_id,
        thread_id: _resolved_thread_id,
        turn_metadata,
        active_plugin_ids_at_turn_start,
        num_input_images: _resolved_num_input_images,
        submission_type,
        ephemeral,
        session_source: _session_source,
        model,
        model_provider,
        permission_profile,
        permission_profile_cwd,
        reasoning_effort,
        reasoning_summary,
        service_tier,
        approval_policy,
        approvals_reviewer,
        guardian_v2_enabled,
        multi_agent_version,
        sandbox_network_access,
        collaboration_mode,
        personality,
        workspace_kind,
        is_first_turn,
    } = resolved_config;
    let TurnProfile {
        critical_path,
        before_first_sampling_ms,
        sampling_ms,
        compaction_ms,
        between_sampling_overhead_ms,
        tool_blocking_ms,
        after_last_sampling_ms,
        sampling_request_count,
        sampling_retry_count,
        tools_change_count,
    } = profile;
    let token_usage = turn_state.token_usage.clone();
    let codex_error = turn_state.codex_error.as_ref();
    CodexTurnEventParams {
        critical_path: critical_path.into(),
        thread_id,
        session_id: thread_metadata.session_id.clone(),
        turn_id,
        active_plugin_ids_at_turn_start,
        voice_session_id: turn_state.voice_session_id.clone(),
        root_turn_id: turn_metadata.root_turn_id(),
        turn_trigger: turn_metadata.turn_trigger(),
        codex_turn_source: turn_metadata.codex_turn_source(),
        app_server_client,
        runtime,
        submission_type,
        ephemeral,
        thread_source: thread_metadata.thread_source.clone(),
        initialization_mode: thread_metadata.initialization_mode,
        subagent_source: thread_metadata.subagent_source.clone(),
        parent_thread_id: thread_metadata.parent_thread_id.clone(),
        model: Some(model),
        model_provider,
        sandbox_policy: Some(sandbox_policy_mode(
            &permission_profile,
            permission_profile_cwd.as_path(),
        )),
        reasoning_effort: reasoning_effort.map(|value| value.to_string()),
        reasoning_summary: reasoning_summary_mode(reasoning_summary),
        service_tier: service_tier
            .map(|value| value.to_string())
            .unwrap_or_else(|| "default".to_string()),
        approval_policy: approval_policy.to_string(),
        approvals_reviewer: approvals_reviewer.to_string(),
        guardian_v2_enabled,
        multi_agent_version,
        sandbox_network_access,
        collaboration_mode: Some(collaboration_mode_mode(collaboration_mode)),
        personality: personality_mode(personality),
        workspace_kind,
        num_input_images,
        image_preparations: turn_state.image_preparations.clone(),
        is_first_turn,
        status: completed.status,
        explicit_client_interrupt_requested_at_ms: turn_state
            .explicit_client_interrupt_requested_at_ms,
        turn_error: completed.turn_error,
        codex_error_kind: codex_error.map(|error| error.kind),
        codex_error_http_status_code: codex_error.and_then(|error| error.http_status_code),
        usage_limit_window_minutes: codex_error.and_then(|error| error.usage_limit_window_minutes),
        steer_count: Some(turn_state.steer_count),
        total_tool_call_count: Some(turn_state.tool_counts.total),
        shell_command_count: Some(turn_state.tool_counts.shell_command),
        file_change_count: Some(turn_state.tool_counts.file_change),
        mcp_tool_call_count: Some(turn_state.tool_counts.mcp_tool_call),
        dynamic_tool_call_count: Some(turn_state.tool_counts.dynamic_tool_call),
        subagent_tool_call_count: Some(turn_state.tool_counts.subagent_tool_call),
        web_search_count: Some(turn_state.tool_counts.web_search),
        image_generation_count: Some(turn_state.tool_counts.image_generation),
        input_tokens: token_usage
            .as_ref()
            .map(|token_usage| token_usage.input_tokens),
        cached_input_tokens: token_usage
            .as_ref()
            .map(|token_usage| token_usage.cached_input_tokens),
        cache_write_input_tokens: token_usage
            .as_ref()
            .map(|token_usage| token_usage.cache_write_input_tokens),
        output_tokens: token_usage
            .as_ref()
            .map(|token_usage| token_usage.output_tokens),
        reasoning_output_tokens: token_usage
            .as_ref()
            .map(|token_usage| token_usage.reasoning_output_tokens),
        total_tokens: token_usage
            .as_ref()
            .map(|token_usage| token_usage.total_tokens),
        before_first_sampling_ms,
        sampling_ms,
        compaction_ms,
        between_sampling_overhead_ms,
        tool_blocking_ms,
        after_last_sampling_ms,
        sampling_request_count,
        sampling_retry_count,
        tools_change_count,
        duration_ms: completed.duration_ms,
        started_at,
        completed_at: Some(completed.completed_at),
    }
}

fn collaboration_mode_mode(mode: ModeKind) -> &'static str {
    match mode {
        ModeKind::Plan => "plan",
        ModeKind::Default => "default",
    }
}

fn reasoning_summary_mode(summary: Option<ReasoningSummary>) -> Option<String> {
    match summary {
        Some(ReasoningSummary::None) | None => None,
        Some(summary) => Some(summary.to_string()),
    }
}

fn personality_mode(personality: Option<Personality>) -> Option<String> {
    match personality {
        Some(Personality::None) | None => None,
        Some(personality) => Some(personality.to_string()),
    }
}

pub(super) fn num_input_images(input: &[UserInput]) -> usize {
    input
        .iter()
        .filter(|item| matches!(item, UserInput::Image { .. } | UserInput::LocalImage { .. }))
        .count()
}
