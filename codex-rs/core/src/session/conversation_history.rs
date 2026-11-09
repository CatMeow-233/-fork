//! Recording, replacing, and windowing the session's conversation history.

use super::*;

impl Session {
    pub(crate) fn response_item_create_time() -> serde_json::Number {
        let create_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        serde_json::Number::from_f64(create_time.as_secs_f64())
            .unwrap_or_else(|| serde_json::Number::from(create_time.as_secs()))
    }

    /// Adds the turn ID and creation time to newly created conversation items.
    pub(crate) fn stamp_response_item_for_history(item: &mut ResponseItem, turn_id: &str) {
        item.set_turn_id_if_missing(turn_id);
        item.set_create_time_if_missing(Self::response_item_create_time());
    }

    /// Prepares media using the originating model and preserves existing item identity.
    pub(crate) async fn prepare_conversation_items_for_history(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        items: &[ResponseItem],
    ) -> (Vec<ResponseItem>, Vec<ImagePreparationMetadata>) {
        let mut items = items.to_vec();
        let image_preparation_mode =
            if unified_image_budget_enabled(&turn_context.config.features, model_info) {
                ImagePreparationMode::UnifiedBudget
            } else {
                ImagePreparationMode::DetailBased
            };
        let image_resize_notice_mode = if turn_context
            .config
            .features
            .enabled(Feature::ImageResizeNotice)
        {
            ImageResizeNoticeMode::Enabled
        } else {
            ImageResizeNoticeMode::Disabled
        };
        // Keep nested image-upload futures out of every caller's future frame.
        let image_preparations = Box::pin(prepare_image_response_items(
            &self.thread_id.to_string(),
            turn_context.config.ephemeral,
            &mut items,
            image_preparation_mode,
            image_resize_notice_mode,
            self.services.image_store.as_ref(),
        ))
        .await;
        prepare_audio_response_items(&mut items);
        // Most response items get their passthrough turn ID at the durable history boundary.
        for item in &mut items {
            Self::stamp_response_item_for_history(item, &turn_context.sub_id);
            self.assign_missing_response_item_id(item);
        }
        (items, image_preparations)
    }

    pub(crate) fn assign_missing_response_item_id(&self, item: &mut ResponseItem) {
        assign_missing_item_id(item, &self.item_id_generator);
    }

    pub(crate) fn response_item_from_user_input(&self, input: Vec<UserInput>) -> ResponseItem {
        self.response_item_from_user_input_with_image_positions(input, &mut HashMap::new())
    }

    pub(super) fn response_item_from_user_input_with_image_positions(
        &self,
        input: Vec<UserInput>,
        user_image_content_indices: &mut HashMap<usize, usize>,
    ) -> ResponseItem {
        let mut item = ResponseItem::from(ResponseInputItem::from_user_input(
            input,
            LocalImagePreparation::Defer,
            user_image_content_indices,
        ));
        if let ResponseItem::Message {
            content,
            internal_chat_message_metadata_passthrough,
            ..
        } = &mut item
        {
            let content_item_kinds = content
                .iter()
                .map(|content| {
                    ContentItemKind(
                        match content {
                            ContentItem::InputText { .. } | ContentItem::OutputText { .. } => {
                                "user.text"
                            }
                            ContentItem::InputImage { .. } => "user.image",
                            ContentItem::InputAudio { .. } => "user.audio",
                        }
                        .to_string(),
                    )
                })
                .collect();
            *internal_chat_message_metadata_passthrough =
                Some(InternalChatMessageMetadataPassthrough {
                    content_item_kinds: Some(content_item_kinds),
                    content_item_metadata: Some(vec![ContentItemMetadata::user(); content.len()]),
                    ..Default::default()
                });
        }
        item
    }

    /// Appends to history, persists the prepared items, then notifies raw-item observers.
    /// Execution callers supply their captured model; standalone callers need no tool runtime.
    #[tracing::instrument(level = "trace", skip_all, fields(item_count = items.len()))]
    pub(crate) async fn record_conversation_items(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        items: &[ResponseItem],
    ) {
        let (items, image_preparations) = self
            .prepare_conversation_items_for_history(turn_context, model_info, items)
            .await;
        let items = items.into_iter().map(ResponseItemEnvelope::new).collect();
        self.record_prepared_conversation_items(
            turn_context,
            model_info,
            items,
            image_preparations,
        )
        .await;
    }

    pub(super) async fn record_prepared_conversation_items(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        mut items: Vec<ResponseItemEnvelope>,
        image_preparations: Vec<ImagePreparationMetadata>,
    ) {
        // Save the originating history budget for replay.
        // Preserve any existing tool-specific override.
        let policy: codex_utils_output_truncation::TruncationPolicy =
            model_info.truncation_policy.into();
        crate::model_search::SearchOwner::checkpoint(
            &self.services.thread_extension_data,
            &mut items,
        );
        for envelope in &mut items {
            if matches!(
                envelope.item,
                ResponseItem::FunctionCallOutput { .. } | ResponseItem::CustomToolCallOutput { .. }
            ) {
                envelope
                    .metadata
                    .get_or_insert_default()
                    .history_truncation_token_limit
                    .get_or_insert_with(|| with_serialization_allowance(policy).token_budget());
            }
        }
        // Last-N-turn forks retain a suffix starting at a user turn boundary. Repeat the
        // cumulative checkpoint there so the suffix remains self-contained even when the fork
        // happens mid-turn; dirty checkpoints preserve new sources between boundaries.
        let force_mcp_checkpoint = items
            .iter()
            .any(|envelope| crate::context_manager::is_user_turn_boundary(&envelope.item));
        let mcp_revision = self
            .services
            .executed_tool_calls
            .mcp_attribution_checkpoint(force_mcp_checkpoint)
            .and_then(|(attribution, revision)| {
                let first_persisted = items
                    .iter()
                    .position(|envelope| should_persist_response_item(&envelope.item))?;
                for (index, envelope) in items.iter_mut().enumerate() {
                    // Rollout batches may be partially written. Checkpoint the first persisted
                    // item, and repeat the checkpoint at turn boundaries retained by forks.
                    if index == first_persisted
                        || crate::context_manager::is_user_turn_boundary(&envelope.item)
                    {
                        envelope.metadata.get_or_insert_default().mcp_attribution =
                            Some(attribution.clone());
                    }
                }
                Some(revision)
            });
        let response_items = items
            .iter()
            .map(|envelope| envelope.item.clone())
            .collect::<Vec<_>>();
        {
            let mut state = self.state.lock().await;
            state
                .current_time_reminder
                .note_recorded_items(&response_items);
            let pending_orders = turn_context
                .extension_data
                .get::<retained_context::PendingAssistantMessageOrders>();
            for envelope in &mut items {
                if envelope
                    .metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.compaction_output)
                {
                    continue;
                }
                if matches!(&envelope.item, ResponseItem::Message { role, .. } if role == "assistant")
                    || matches!(&envelope.item, ResponseItem::FunctionCall { .. })
                    || crate::context::is_user_authorization_message(&envelope.item)
                {
                    let message_order = pending_orders.as_ref().and_then(|orders| {
                        orders
                            .0
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(envelope.item.id()?.as_str())
                    });
                    // Preserve input acceptance and source-message start order.
                    // Synthetic messages still receive their order here.
                    envelope
                        .metadata
                        .get_or_insert_default()
                        .user_input_order
                        .get_or_insert_with(|| {
                            message_order.unwrap_or_else(|| state.history.reserve_input_order())
                        });
                }
            }
            state
                .pending_audio_duration_ids
                .extend(file_audio::file_ids(&items).cloned());
            state
                .history
                .record_annotated_items(&mut items, model_info.truncation_policy.into());
        }
        for image in image_preparations {
            self.services
                .analytics_events_client
                .track_image_preparation(ImagePreparationFact {
                    turn_id: turn_context.sub_id.clone(),
                    metadata: image,
                });
        }
        let settings_guard =
            if force_mcp_checkpoint && self.reference_context_item().await.is_none() {
                Some(thread_settings::acquire_persistence_lock(self).await)
            } else {
                None
            };
        let initial_settings = if settings_guard.is_some()
            && self.reference_context_item().await.is_none()
            && self.state.lock().await.take_initial_settings_pending()
        {
            Some(thread_settings::applied_event(self).await)
        } else {
            None
        };
        let rollout_items = initial_settings
            .into_iter()
            .map(RolloutItem::EventMsg)
            .chain(items.into_iter().map(RolloutItem::ResponseItem))
            .collect::<Vec<_>>();
        let persisted = self.persist_rollout_items(&rollout_items).await;
        drop(settings_guard);
        if persisted && let Some(revision) = mcp_revision {
            self.services
                .executed_tool_calls
                .mark_mcp_attribution_persisted(revision);
        }
        if turn_context.config.memories.disable_on_external_context
            && let Some(item) = response_items
                .iter()
                .find(|item| matches!(item, ResponseItem::FunctionCallOutput { call_id: None, .. }))
        {
            mark_thread_memory_mode_polluted_if_external_context(self, turn_context, item).await;
        }
        self.send_raw_response_items(turn_context, &response_items)
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn replace_history(
        &self,
        items: Vec<ResponseItem>,
        reference_context_item: Option<TurnContextItem>,
    ) {
        let mut state = self.state.lock().await;
        state.replace_history(items, reference_context_item);
    }

    pub(crate) async fn replace_compacted_history<'a>(
        &self,
        mut items: Vec<ResponseItemEnvelope>,
        reference_context_item: TurnContextItem,
        mut world_state_baseline: WorldStateSnapshot,
        rejected_audio_file_ids: std::collections::BTreeSet<String>,
        installation: impl Into<CompactedHistoryInstallation<'a>>,
    ) -> bool {
        let installation = installation.into();
        let compaction_model_hash = match &installation {
            CompactedHistoryInstallation::Complete(metadata) => &metadata.compaction_model_hash,
            CompactedHistoryInstallation::Prefix { model_info, .. } => &model_info.comp_hash,
        };
        crate::model_search::SearchOwner::checkpoint(
            &self.services.thread_extension_data,
            &mut items,
        );
        let mcp_revision = self
            .services
            .executed_tool_calls
            .mcp_attribution_checkpoint(/*force*/ true)
            .and_then(|(attribution, revision)| {
                items.last_mut().map(|envelope| {
                    envelope.metadata.get_or_insert_default().mcp_attribution = Some(attribution);
                    revision
                })
            });
        if let Some(checkpoint) = items.iter_mut().rev().find(|envelope| {
            matches!(
                envelope.item,
                ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
            )
        }) {
            checkpoint
                .metadata
                .get_or_insert_default()
                .compaction_model_hash = compaction_model_hash.clone();
        }
        // Wait for accepted updates to finish persisting, then keep later updates from
        // overtaking the current settings snapshot while its checkpoint is written.
        let _settings_guard = thread_settings::acquire_persistence_lock(self).await;
        let (compacted_item, started_item) = {
            let mut state = self.state.lock().await;
            let complete_replacement =
                matches!(&installation, CompactedHistoryInstallation::Complete(_));
            let (metadata, started_item) = match installation {
                CompactedHistoryInstallation::Complete(metadata) => (metadata, None),
                CompactedHistoryInstallation::Prefix {
                    prefix,
                    prefix_context,
                    initial_context,
                    reset_world_state_sections,
                    model_info,
                    compaction_response_id,
                    reviewer_compaction_hash,
                    turn_context,
                    started_item,
                } => {
                    // Validate immediately before slicing and replacing the captured prefix.
                    if state.history.history_version() != prefix.generation
                        || state.history.annotated_items().len() < prefix.len
                        || self.services.model_client.auth_owner() != prefix.auth_owner
                    {
                        return false;
                    }
                    let current = state.history.annotated_items();
                    let suffix = &current[prefix.len..];
                    if suffix.iter().any(|envelope| {
                        crate::compact_retained_tool_outputs::is_retained_tool_output(envelope)
                            || compact::is_real_user_or_agent_message(&envelope.item)
                    }) {
                        items.extend(initial_context);
                    } else {
                        items = compact::insert_initial_context_before_last_real_user_or_summary(
                            items,
                            initial_context,
                        );
                    }
                    items.splice(
                        0..0,
                        prefix_context.into_iter().map(ResponseItemEnvelope::new),
                    );
                    items.extend_from_slice(suffix);
                    // The unchanged suffix already contains updates through this baseline.
                    if let Some(checkpoint) = state.history.world_state_checkpoint() {
                        world_state_baseline = WorldStateSnapshot::from(&checkpoint.state);
                    }
                    // Volatile sections must refresh in the new window even if their
                    // previous messages and comparison state survived in the suffix.
                    for section_id in reset_world_state_sections {
                        world_state_baseline.remove_section(section_id);
                    }
                    // Prefix replacement already retains all edits in the live suffix.
                    let input_goal_ids = Default::default();
                    let (window_number, window_ids) = state.advance_auto_compact_window();
                    (
                        CompactedHistoryMetadata {
                            input_goal_ids,
                            message: String::new(),
                            window_number,
                            window_ids,
                            compaction_response_id: Some(compaction_response_id),
                            compaction_model_hash: model_info.comp_hash.clone(),
                            reviewer_compaction_hash,
                        },
                        started_item.map(|item| (turn_context, item)),
                    )
                }
            };
            for envelope in &mut items {
                self.assign_missing_response_item_id(&mut envelope.item);
            }
            // Goal edits are published outside the running task. Keep edits accepted after
            // the compaction input snapshot, in their original order, after its replacement.
            if complete_replacement {
                let replacement_goal_ids = crate::context::UserGoalUpdate::message_ids(
                    items.iter().map(|envelope| &envelope.item),
                );
                items.extend(
                    state
                        .history
                        .annotated_items()
                        .iter()
                        .filter(|envelope| {
                            crate::context::UserGoalUpdate::message_text(&envelope.item).is_some()
                                && envelope.item.id().is_some_and(|id| {
                                    !metadata.input_goal_ids.contains(id)
                                        && !replacement_goal_ids.contains(id)
                                })
                        })
                        .cloned(),
                );
            }
            state.history.checkpoint_sampling(&mut items);
            let replacement_history = items.clone();
            state
                .history
                .extend_rejected_audio_file_ids(rejected_audio_file_ids);
            state.replace_annotated_history(
                items,
                Some(reference_context_item.clone()),
                HistoryReplacement::Compaction {
                    reviewer_compaction_hash: metadata.reviewer_compaction_hash,
                },
            );
            state.reasoning_effort_pin = ReasoningEffortPin::Compacted;
            state
                .history
                .set_world_state_baseline(world_state_baseline.clone());
            let compacted_item = CompactedItem {
                message: metadata.message,
                replacement_history: Some(replacement_history),
                guardian_history: state.history.guardian_history_checkpoint(),
                retained_context: Some(state.history.retained_context().clone()),
                mcp_resource_origins: self.services.mcp_runtime.resource_origin_checkpoint(),
                window_number: Some(metadata.window_number),
                first_window_id: Some(metadata.window_ids.first_window_id.to_string()),
                previous_window_id: metadata
                    .window_ids
                    .previous_window_id
                    .map(|id| id.to_string()),
                window_id: Some(metadata.window_ids.window_id.to_string()),
                compaction_response_id: metadata.compaction_response_id,
                latest_token_usage_record: state.latest_token_usage_record.clone(),
                resume_metadata: Some(CompactionResumeMetadata {
                    rejected_audio_file_ids: state.history.rejected_audio_file_ids(),
                    dynamic_tools: Some(Box::new(DynamicToolsSnapshot {
                        thread_id: self.thread_id(),
                        tools: state.persisted_dynamic_tools.to_vec(),
                    })),
                    multi_agent_version: self.multi_agent_version(),
                    last_started_turn_id: state.last_started_turn_id.clone(),
                    turn_attribution: state.turn_attribution.clone(),
                    previous_turn_settings: state.previous_turn_settings(),
                }),
            };
            (compacted_item, started_item)
        };

        if let Some((turn_context, item)) = started_item {
            // The prefix is now known to be valid; announce the install before checkpoint I/O.
            self.emit_turn_item_started(turn_context, item).await;
        }
        let world_state_item = WorldStateItem::full(world_state_baseline.into_object());
        let rollout_items = [
            RolloutItem::Compacted(Box::new(compacted_item)),
            RolloutItem::WorldState(world_state_item),
            RolloutItem::TurnContext(reference_context_item),
            // The frozen turn context must not override current settings in persisted metadata.
            RolloutItem::EventMsg(thread_settings::applied_event(self).await),
        ];
        if self.persist_rollout_items(&rollout_items).await
            && let Some(revision) = mcp_revision
        {
            self.services
                .executed_tool_calls
                .mark_mcp_attribution_persisted(revision);
        }
        {
            let mut state = self.state.lock().await;
            state.queue_pending_session_start_source(codex_hooks::SessionStartSource::Compact);
        }
        true
    }

    pub(super) fn preserve_parent_tool_declarations(&self) -> bool {
        // Only prediction forks and thread/sample set this marker when they disable MCP.
        self.enabled(Feature::PredictionPreserveParentTools)
            && self
                .services
                .thread_extension_data
                .get::<crate::McpServersDisabled>()
                .is_some()
    }

    #[tracing::instrument(level = "trace", skip_all, fields(item_count = items.len()))]
    pub(crate) async fn persist_rollout_items(&self, items: &[RolloutItem]) -> bool {
        if let Some(live_thread) = self.live_thread()
            && let Err(e) = live_thread.append_items(items).await
        {
            error!("failed to record rollout items: {e:#}");
            return false;
        }
        true
    }

    pub(crate) async fn clone_history(&self) -> ContextManager {
        let state = self.state.lock().await;
        state.clone_history()
    }

    /// Captures the history lifetime without exposing mutable session state.
    pub(crate) async fn history_reset(&self) -> (u64, CancellationToken) {
        let state = self.state.lock().await;
        (
            state.history.reset_version,
            state.history_reset.child_token(),
        )
    }

    pub(crate) async fn conversation_history_snapshot(
        &self,
    ) -> Arc<dyn ConversationHistorySnapshot> {
        let state = self.state.lock().await;
        state.history.conversation_history_snapshot()
    }

    pub(crate) async fn current_window_id(&self) -> String {
        self.current_window().await.0
    }

    pub(crate) async fn current_window(&self) -> (String, u64, ContextWindowId) {
        let state = self.state.lock().await;
        self.current_window_from_state(&state)
    }

    pub(crate) fn current_window_from_state(
        &self,
        state: &SessionState,
    ) -> (String, u64, ContextWindowId) {
        let thread_id = self.thread_id;
        let window_number = state.auto_compact_window_number();
        let context_window_id = state.auto_compact_window_ids().window_id.clone();
        (
            format!("{thread_id}:{window_number}"),
            window_number,
            context_window_id,
        )
    }

    pub(crate) async fn advance_auto_compact_window(&self) -> (u64, AutoCompactWindowIds) {
        let mut state = self.state.lock().await;
        state.advance_auto_compact_window()
    }

    pub(crate) async fn request_new_context_window(&self) {
        let mut state = self.state.lock().await;
        state.request_new_context_window();
    }

    pub(crate) async fn take_new_context_window_request(&self) -> bool {
        let mut state = self.state.lock().await;
        state.take_new_context_window_request()
    }

    pub(crate) async fn start_new_context_window(
        &self,
        step_context: &StepContext,
        world_state: Arc<WorldState>,
    ) {
        let turn_context = step_context.turn.as_ref();
        let history = self.clone_history().await;
        let input_goal_ids = crate::context::UserGoalUpdate::message_ids(history.raw_items());
        let retained_client_items =
            crate::compact_remote_v2::truncate_retained_messages_for_remote_compaction(
                history
                    .annotated_items()
                    .iter()
                    .filter(|item| {
                        (self.enabled(Feature::RetainClientToolOutputs)
                            && crate::compact_retained_tool_outputs::is_retained_tool_output(item))
                            || (self.enabled(Feature::RetainClientDeveloperMessages)
                                && crate::compact_remote_v2::is_client_authored_developer_message(
                                    item,
                                ))
                    })
                    .cloned()
                    .collect(),
                crate::compact_remote_v2::RETAINED_MESSAGE_TOKEN_BUDGET,
                &history.file_audio_durations,
            );
        // Resolve the roster before allocating a new context window.
        let rendered = world_state.render_full_with_host_roster().await;
        let (window_number, window_ids) = self.state.lock().await.start_new_context_window();
        let (context_updates, world_state_snapshot) =
            self.assemble_initial_context(step_context, rendered).await;
        let context_items =
            crate::context_manager::updates::merge_world_state_updates(context_updates)
                .into_iter()
                .map(ResponseItemEnvelope::new)
                .chain(retained_client_items)
                .collect();
        let turn_context_item = step_context.to_turn_context_item();
        self.replace_compacted_history(
            context_items,
            turn_context_item,
            world_state_snapshot,
            /*rejected_audio_file_ids*/ Default::default(),
            CompactedHistoryMetadata {
                input_goal_ids,
                message: String::new(),
                window_number,
                window_ids,
                compaction_response_id: None,
                compaction_model_hash: None,
                reviewer_compaction_hash: None,
            },
        )
        .await;
        self.recompute_token_usage(turn_context).await;
    }
}
