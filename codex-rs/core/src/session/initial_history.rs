//! Installing resumed or forked initial history into a new session.

use super::*;

impl Session {
    pub(super) async fn record_initial_history(&self, conversation_history: InitialHistory) {
        let (is_subagent, is_paginated_subagent) = {
            let state = self.state.lock().await;
            let session_configuration = &state.session_configuration;
            (
                session_configuration.session_source.is_non_root_agent(),
                matches!(
                    session_configuration.history_mode,
                    ThreadHistoryMode::Paginated
                ) && matches!(
                    session_configuration.thread_source.as_ref(),
                    Some(ThreadSource::Subagent | ThreadSource::GuardianReview)
                ),
            )
        };
        let has_prior_user_turns = initial_history_has_prior_user_turns(&conversation_history);
        {
            let mut state = self.state.lock().await;
            state.set_next_turn_is_first(!has_prior_user_turns);
        }
        let turn_context = match conversation_history {
            InitialHistory::New | InitialHistory::Cleared => {
                // Defer initial context insertion until the first real turn starts so
                // turn/start overrides can be merged before we write model-visible context.
                self.set_previous_turn_settings(/*previous_turn_settings*/ None)
                    .await;
                None
            }
            InitialHistory::Resumed(resumed_history) => {
                let turn_context = self.new_default_turn().await;
                let rollout_items = resumed_history.history;
                if matches!(
                    rollout_items.iter().rev().find_map(|item| match item {
                        RolloutItem::EventMsg(event) => agent_status_from_event(event),
                        _ => None,
                    }),
                    Some(AgentStatus::Interrupted)
                ) {
                    self.agent_status.send_replace(AgentStatus::Interrupted);
                }
                let previous_turn_settings = self
                    .apply_rollout_reconstruction(&turn_context, &rollout_items)
                    .await;

                // If resuming, warn when the last recorded model differs from the current one.
                let curr: &str = turn_context.model_info().slug.as_str();
                if let Some(prev) = previous_turn_settings
                    .as_ref()
                    .map(|settings| settings.model.as_str())
                    .filter(|model| *model != curr)
                {
                    warn!("resuming session with different model: previous={prev}, current={curr}");
                    self.send_event(
                        &turn_context,
                        EventMsg::Warning(WarningEvent {
                            message: format!(
                                "This session was recorded with model `{prev}` but is resuming with `{curr}`. \
                         Consider switching back to `{prev}` as it may affect Codex performance."
                            ),
                        }),
                    )
                    .await;
                }

                // Seed usage info from the recorded rollout so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }
                self.state.lock().await.latest_token_usage_record =
                    Self::last_token_usage_record_from_rollout(&rollout_items);

                let thread_id = self.thread_id();
                let current_settings = self.thread_settings_snapshot().await;
                let persisted_settings = rollout_items.iter().rev().find_map(|item| match item {
                    RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
                        if event.thread_id == Some(thread_id) =>
                    {
                        Some(&event.thread_settings)
                    }
                    _ => None,
                });
                // Persist changed effective settings even when no turn follows the resume.
                if persisted_settings != Some(&current_settings) {
                    self.persist_rollout_items(&[RolloutItem::EventMsg(
                        EventMsg::ThreadSettingsApplied(ThreadSettingsAppliedEvent {
                            thread_id: Some(thread_id),
                            thread_settings: current_settings,
                        }),
                    )])
                    .await;
                }

                // Defer seeding the session's initial context until the first turn starts so
                // turn/start overrides can be merged before we write model-visible context.
                if !is_subagent {
                    let _ = self.flush_rollout().await;
                }
                None
            }
            InitialHistory::Forked(mut rollout_items) => {
                let turn_context = self.new_default_turn().await;
                for record in &mut rollout_items {
                    if let RolloutItem::ResponseItem(envelope) = record {
                        self.assign_missing_response_item_id(&mut envelope.item);
                    }
                }
                self.apply_rollout_reconstruction(&turn_context, &rollout_items)
                    .await;

                // Seed usage info from the recorded rollout so UIs can show token counts
                // immediately on resume/fork.
                if let Some(info) = Self::last_token_info_from_rollout(&rollout_items) {
                    let mut state = self.state.lock().await;
                    state.set_token_info(Some(info));
                }
                self.state.lock().await.latest_token_usage_record =
                    Self::last_token_usage_record_from_rollout(&rollout_items);

                let thread_settings_applied =
                    RolloutItem::EventMsg(thread_settings::applied_event(self).await);
                match &self.fork_persistence {
                    ForkPersistence::Referenced {
                        inherited_item_count,
                        ..
                    } => {
                        // Ancestor records remain behind history_base; only effective child
                        // settings and boundaries synthesized by snapshot processing are local.
                        rollout_items.drain(..*inherited_item_count);
                        rollout_items.insert(0, thread_settings_applied);
                    }
                    ForkPersistence::Copied | ForkPersistence::CopiedDeferred
                        if is_paginated_subagent =>
                    {
                        // Paginated subagents already persist inherited context when their live
                        // thread is created.
                        rollout_items.clear();
                        rollout_items.push(thread_settings_applied);
                    }
                    ForkPersistence::Copied | ForkPersistence::CopiedDeferred => {
                        // Keep the copied prefix and effective child settings in one append so a
                        // cold resume cannot observe inherited settings as the latest value.
                        rollout_items.push(thread_settings_applied);
                    }
                }
                self.persist_rollout_items(&rollout_items).await;
                if self.preserve_parent_tool_declarations() {
                    let notice = DeveloperInstructions::new(
                        "<mcp_tools>\nMCP tools are unavailable for this request.\n</mcp_tools>",
                    )
                    .with_metadata(ContentItemMetadata::tool(/*namespace*/ None));
                    self.record_conversation_items(
                        &turn_context,
                        turn_context.model_info(),
                        &[ContextualUserFragment::into(notice)],
                    )
                    .await;
                }

                // Agent spawn owns its final durability barrier so it can overlap the edge write.
                let persist_context = match self.fork_persistence {
                    ForkPersistence::CopiedDeferred => PersistContext::SubagentSpawn,
                    ForkPersistence::Copied | ForkPersistence::Referenced { .. } => {
                        PersistContext::Standard
                    }
                };
                self.ensure_rollout_materialized(persist_context).await;

                // Flush after seeding history and any persisted rollout copy.
                if !is_subagent {
                    let _ = self.flush_rollout().await;
                }
                Some(turn_context)
            }
        };
        if let Some(turn_context) = turn_context
            && turn_context.config.memories.disable_on_external_context
        {
            let history = self.clone_history().await;
            if let Some(item) = history
                .raw_items()
                .find(|item| matches!(item, ResponseItem::FunctionCallOutput { call_id: None, .. }))
            {
                mark_thread_memory_mode_polluted_if_external_context(self, &turn_context, item)
                    .await;
            }
        }
    }

    #[instrument(
        level = "trace",
        skip_all,
        fields(
            thread_id = %self.thread_id(),
            rollout_item_count = rollout_items.len()
        )
    )]
    async fn apply_rollout_reconstruction(
        &self,
        turn_context: &Arc<TurnContext>,
        rollout_items: &[RolloutItem],
    ) -> Option<PreviousTurnSettings> {
        let rollout_reconstruction::RolloutReconstruction {
            mut history,
            rejected_audio_file_ids,
            retained_context,
            guardian_history,
            last_started_turn_id,
            previous_turn_settings,
            reference_context_item,
            turn_attribution,
            world_state_baseline,
            window_number,
            first_window_id,
            previous_window_id,
            window_id,
        } = self
            .reconstruct_history_from_rollout(turn_context, rollout_items)
            .await;
        // An automatic turn can have a persisted routing override without establishing the
        // user-turn context baseline. Recover its own latest context by surviving turn ID.
        let restored_turn_routing = turn_attribution.as_ref().and_then(|turn| {
            rollout_items
                .iter()
                .rev()
                .find_map(|item| match item {
                    RolloutItem::TurnContext(context)
                        if context.turn_id.as_deref() == Some(turn.turn_id.as_str()) =>
                    {
                        Some(context)
                    }
                    _ => None,
                })
                .and_then(|context| context.direct_only_tool_namespaces.clone())
                .map(|namespaces| (turn.turn_id.clone(), namespaces))
        });
        // Keep the recorded rollout unchanged. Prepare its reconstructed history before
        // installing it, so legacy media is processed once for this resume or fork and
        // will be processed again if the rollout is reconstructed in a future session.
        // Replay disables image-resize notices, so media preparation remains one-to-one. Keep
        // the prior batch behavior and carry history-only metadata in a positional sidecar.
        let (mut prepared_history, metadata): (Vec<_>, Vec<_>) = history
            .into_iter()
            .map(|envelope| (envelope.item, envelope.metadata))
            .unzip();
        // Replay must not upload or migrate recorded history. The inline store returns prepared
        // inline bytes, while existing file references bypass preparation and remain unchanged.
        // Bound replay future size now that image preparation can await storage.
        let _ = Box::pin(prepare_image_response_items(
            &self.thread_id.to_string(),
            turn_context.config.ephemeral,
            &mut prepared_history,
            ImagePreparationMode::DetailBased,
            ImageResizeNoticeMode::Disabled,
            &InlineAttachmentStore,
        ))
        .await;
        prepare_audio_response_items(&mut prepared_history);
        assert_eq!(
            prepared_history.len(),
            metadata.len(),
            "replay media preparation must remain one-to-one when resize notices are disabled"
        );
        history = prepared_history
            .into_iter()
            .zip(metadata)
            .map(|(item, metadata)| ResponseItemEnvelope { item, metadata })
            .collect();
        file_audio::resolve(self, file_audio::file_ids(&history).cloned().collect()).await;
        // Ordinary replay needs no reviewer refresh. Only an opaque
        // checkpoint can make reconstruction depend on reviewer compatibility.
        let reviewer_compaction_hash = if codex_history::CompactionCheckpoint::latest(&history)
            .is_some_and(codex_history::CompactionCheckpoint::is_usable)
            && turn_context
                .config
                .features
                .enabled(Feature::GuardianReuseParentCompaction)
        {
            let context = crate::guardian::GuardianReviewContext::from(turn_context);
            let (_, reviewer) = crate::guardian::resolve_review_model(self, &context).await;
            reviewer.comp_hash.clone()
        } else {
            None
        };
        {
            let mut state = self.state.lock().await;
            state.replace_annotated_history(
                history,
                reference_context_item,
                HistoryReplacement::Reset,
            );
            state
                .history
                .restore_rejected_audio_file_ids(rejected_audio_file_ids);
            state.history.restore_review_context(
                Some(&retained_context),
                guardian_history.as_ref(),
                reviewer_compaction_hash.as_deref(),
            );
            state.last_started_turn_id = last_started_turn_id;
            state.turn_attribution = turn_attribution;
            state.restored_turn_routing = restored_turn_routing;
            if let Some(mut world_state) = world_state_baseline {
                // Handles belong to this runtime; a saved checkpoint can also lag its fragment.
                world_state.invalidate_section("command_sessions");
                state.history.set_world_state_baseline(world_state);
            }
            let fallback_ids = state.auto_compact_window_ids();
            let window_id = window_id.unwrap_or_else(|| fallback_ids.window_id.clone());
            state.restore_auto_compact_window(
                window_number,
                AutoCompactWindowIds {
                    first_window_id: first_window_id.unwrap_or_else(|| window_id.clone()),
                    previous_window_id,
                    window_id,
                },
            );
            state.set_previous_turn_settings(previous_turn_settings.clone());
        }
        let prefix_tokens = if matches!(
            turn_context.config.model_auto_compact_token_limit_scope,
            AutoCompactTokenLimitScope::BodyAfterPrefix
        ) {
            let history = self.clone_history().await;
            let base_instructions = self.get_base_instructions().await;
            history.estimate_token_count_with_base_instructions(&base_instructions)
        } else {
            None
        };
        if let Some(prefix_tokens) = prefix_tokens {
            self.set_auto_compact_window_estimated_prefill_for_scope(turn_context, prefix_tokens)
                .await;
        }
        previous_turn_settings
    }

    pub(super) async fn set_auto_compact_window_estimated_prefill_for_scope(
        &self,
        turn_context: &TurnContext,
        tokens: i64,
    ) {
        if !matches!(
            turn_context.config.model_auto_compact_token_limit_scope,
            AutoCompactTokenLimitScope::BodyAfterPrefix
        ) {
            return;
        }

        let mut state = self.state.lock().await;
        state.set_auto_compact_window_estimated_prefill(tokens);
    }

    fn last_token_info_from_rollout(rollout_items: &[RolloutItem]) -> Option<TokenUsageInfo> {
        rollout_items.iter().rev().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TokenCount(ev)) => ev.info.clone(),
            _ => None,
        })
    }

    pub(super) fn last_token_usage_record_from_rollout(
        rollout_items: &[RolloutItem],
    ) -> Option<TokenUsageRecord> {
        for item in rollout_items.iter().rev() {
            match item {
                RolloutItem::TokenUsageRecord(record) => return Some(record.clone()),
                RolloutItem::Compacted(compacted) => {
                    return compacted.latest_token_usage_record.clone();
                }
                _ => {}
            }
        }
        None
    }
}
