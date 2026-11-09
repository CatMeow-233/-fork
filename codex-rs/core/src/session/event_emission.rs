//! Emitting session events, turn items, and model verification notices to clients.

use super::*;

impl Session {
    pub(crate) async fn emit_turn_started(&self, turn_context: &TurnContext, task_kind: TaskKind) {
        if turn_context
            .extension_data
            .get::<crate::tasks::HostTurnStarted>()
            .is_some()
        {
            return;
        }
        let event = TurnStartedEvent {
            turn_attribution: (task_kind == TaskKind::Regular).then(|| turn_context.attribution()),
            turn_id: turn_context.sub_id.clone(),
            root_turn_id: Some(turn_context.root_turn_id()),
            trace_id: turn_context.trace_id.clone(),
            started_at: turn_context.turn_timing_state.started_at_unix_secs().await,
            model_context_window: turn_context.model_context_window(),
            collaboration_mode_kind: turn_context.mode(),
        };
        self.send_event(turn_context, EventMsg::TurnStarted(event))
            .await;
    }

    /// Persist the event to rollout and send it to clients.
    pub(crate) async fn send_event(&self, turn_context: &TurnContext, mut msg: EventMsg) {
        if let Some((item_id, started_at_ms)) = msg.command_approval_start() {
            // The client can see the command before execution. Keep its first start
            // for the canonical lifecycle events that will be saved in history.
            turn_context
                .turn_timing_state
                .record_item_started(item_id.to_owned(), started_at_ms)
                .await;
        }
        if let EventMsg::TurnComplete(completed) = &msg
            && completed.error.is_some()
        {
            self.state
                .lock()
                .await
                .history
                .record_rejected_file_audio(completed);
        }
        if let EventMsg::Error(error) = &mut msg
            && error
                .codex_error_info
                .as_ref()
                .is_some_and(CodexErrorInfo::affects_turn_status)
        {
            let mut terminal_error = turn_context.terminal_error.lock().await;
            if let Some(previous) = terminal_error.as_ref() {
                for file_id in &previous.rejected_audio_file_ids {
                    if !error.rejected_audio_file_ids.contains(file_id) {
                        error.rejected_audio_file_ids.push(file_id.clone());
                    }
                }
            }
            terminal_error.replace(error.clone());
        }
        let legacy_source = msg.clone();
        self.services
            .rollout_thread_trace
            .record_codex_turn_event(&turn_context.sub_id, &legacy_source);
        self.services
            .rollout_thread_trace
            .record_tool_call_event(turn_context.sub_id.clone(), &legacy_source);
        let event = Event {
            id: turn_context.sub_id.clone(),
            msg,
        };
        // Private reviewers have no app-server listener; publicly resumed Guardian threads do.
        if matches!(
            &turn_context.session_source,
            SessionSource::SubAgent(SubAgentSource::Other(name))
                if name == crate::guardian::GUARDIAN_REVIEWER_NAME
        ) && self.services.analytics_events_client.is_enabled()
            && turn_context.parent_thread_id.is_some()
            && self
                .state
                .lock()
                .await
                .session_configuration
                .trusted_guardian_reviewer
        {
            self.services
                .analytics_events_client
                .track_guardian_session_event(self.thread_id, &event);
        }
        self.send_event_raw(event).await;
        self.maybe_publish_terminal_agent_result(turn_context, &legacy_source)
            .await;
        self.maybe_mirror_event_text_to_realtime(&legacy_source)
            .await;
        self.maybe_clear_realtime_handoff_for_event(&legacy_source)
            .await;

        let show_raw_agent_reasoning = self.show_raw_agent_reasoning();
        for legacy in legacy_source.as_legacy_events(show_raw_agent_reasoning) {
            self.services
                .rollout_thread_trace
                .record_tool_call_event(turn_context.sub_id.clone(), &legacy);
            let legacy_event = Event {
                id: turn_context.sub_id.clone(),
                msg: legacy,
            };
            self.send_event_raw(legacy_event).await;
        }
    }

    /// Classifies terminal results for native parent delivery and opted-in hosts.
    async fn maybe_publish_terminal_agent_result(
        &self,
        turn_context: &TurnContext,
        msg: &EventMsg,
    ) {
        if !matches!(msg, EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)) {
            return;
        }

        let notify_native_parent = turn_context.multi_agent_version == MultiAgentVersion::V2
            && matches!(
                &turn_context.session_source,
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    agent_path: Some(_),
                    ..
                })
            );
        let reporter = self
            .outcome_reporting
            .as_ref()
            .map(|reporting| &reporting.reporter);
        if !notify_native_parent && reporter.is_none() {
            return;
        }

        let terminal_error_message = {
            let mut terminal_error = turn_context.terminal_error.lock().await;
            if notify_native_parent {
                terminal_error.take().map(|error| error.message)
            } else {
                terminal_error.as_ref().map(|error| error.message.clone())
            }
        };
        let mut error_info = None;
        let status = match terminal_error_message {
            Some(message) => {
                let status = AgentStatus::Errored(message);
                if notify_native_parent {
                    self.agent_status.send_replace(status.clone());
                }
                status
            }
            None => {
                if let EventMsg::TurnAborted(event) = msg
                    && event.reason == TurnAbortReason::Interrupted
                    && let Some(error) = &event.error
                    && error.codex_error_info == Some(CodexErrorInfo::TooManyDenials)
                {
                    // Report the safety stop to the parent without changing the child's
                    // interrupted status or notifying for ordinary interruptions.
                    error_info = error.codex_error_info.clone();
                    AgentStatus::Errored(error.message.clone())
                } else {
                    let Some(status) = agent_status_from_event(msg) else {
                        return;
                    };
                    status
                }
            }
        };
        if !is_final(&status) {
            return;
        }

        let outcome = AgentTurnOutcome {
            thread_id: self.thread_id,
            turn_id: turn_context.sub_id.clone(),
            source: turn_context.session_source.clone(),
            parent_turn_id: turn_context.turn_metadata_state.parent_turn_id(),
            initiating_agent_path: turn_context
                .turn_metadata_state
                .initiating_agent_path()
                .cloned(),
            status,
            error_info,
        };
        let host_outcome = if notify_native_parent {
            let host_outcome = reporter.is_some().then(|| outcome.clone());
            self.services
                .agent_control
                .turn_finished(outcome, &self.services.rollout_thread_trace)
                .await;
            host_outcome
        } else {
            Some(outcome)
        };
        if let Some((reporter, outcome)) = reporter.zip(host_outcome) {
            reporter.report(outcome).await;
        }
    }

    async fn maybe_mirror_event_text_to_realtime(&self, msg: &EventMsg) {
        if self.conversation.running_state().await.is_none() {
            return;
        }
        match msg {
            EventMsg::ItemStarted(event) => {
                if let TurnItem::AgentMessage(item) = &event.item {
                    self.conversation
                        .register_handoff_stream_item(
                            item.id.clone(),
                            item.phase.clone(),
                            agent_message_text(item),
                        )
                        .await;
                }
                return;
            }
            EventMsg::AgentMessageContentDelta(event) => {
                if let Err(err) = self
                    .conversation
                    .stream_handoff_delta(&event.item_id, event.delta.clone())
                    .await
                {
                    debug!("failed to stream event text to realtime conversation: {err}");
                }
                return;
            }
            EventMsg::ItemCompleted(event) => {
                if let TurnItem::AgentMessage(item) = &event.item
                    && self.conversation.finish_handoff_stream_item(&item.id).await
                {
                    return;
                }
            }
            _ => {}
        }
        let result = match realtime_text_for_event(msg) {
            Some(RealtimeEventText::Handoff(text, phase)) => {
                self.conversation.handoff_out(text, phase).await
            }
            Some(RealtimeEventText::QuietReasoning(text)) => {
                self.conversation.send_reasoning_status(&text).await
            }
            None => return,
        };
        if let Err(err) = result {
            debug!("failed to mirror event text to realtime conversation: {err}");
        }
    }

    async fn maybe_clear_realtime_handoff_for_event(&self, msg: &EventMsg) {
        if !matches!(msg, EventMsg::TurnComplete(_)) {
            return;
        }
        if let Err(err) = self.conversation.handoff_complete().await {
            debug!("failed to finalize realtime handoff output: {err}");
        }
        self.conversation.clear_active_handoff().await;
    }

    pub(crate) async fn send_event_raw(&self, event: Event) {
        self.send_event_raw_with_persistence(event, /*persist*/ true)
            .await;
    }

    /// Delivers an event without creating a local rollout for a thread that has not materialized.
    pub(crate) async fn send_event_raw_without_materializing_rollout(&self, event: Event) {
        let persist = match self.current_rollout_path().await {
            Ok(Some(path)) => codex_rollout::existing_rollout_path(&path).await.is_some(),
            Ok(None) => true,
            Err(err) => {
                warn!("failed to check whether thread persistence is materialized: {err}");
                true
            }
        };
        self.send_event_raw_with_persistence(event, persist).await;
    }

    async fn send_event_raw_with_persistence(&self, event: Event, persist: bool) {
        let flush_guardian_completion = persist
            && matches!(event.msg, EventMsg::TurnComplete(_))
            && self.is_private_guardian_reviewer().await;
        // Keep realtime reduction, canonical append, and delivery in the same order.
        // This lock must not acquire SessionState or ActiveTurn: event producers can
        // already hold those locks. Host presentation policies are synchronous.
        let mut realtime_history = match &self.realtime_history {
            Some(history) => {
                let history = history.lock().await;
                history.should_observe(&event.msg).then_some(history)
            }
            None => None,
        };
        self.services.mcp_runtime.observe_event(&event.msg);
        let (before_event, after_event) = match realtime_history.as_mut() {
            Some(history) => {
                let effects = history.observe(&event.msg);
                match effects.order {
                    RealtimeEventOrder::BeforeEvent => (Some(effects), None),
                    RealtimeEventOrder::AfterEvent => (None, Some(effects)),
                }
            }
            None => (None, None),
        };
        if let Some(effects) = before_event
            && let Err(error) = self.send_realtime_history_effects(&event.id, effects).await
        {
            warn!("failed to persist realtime history: {error}");
        }
        // Persist the event into rollout storage; the store applies its persistence policy.
        if persist {
            let rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
            self.persist_rollout_items(&rollout_items).await;
        }
        self.services
            .rollout_thread_trace
            .record_protocol_event(&event.msg);
        if let Some(effects) = after_event
            && let Err(error) = self.send_realtime_history_effects(&event.id, effects).await
        {
            warn!("failed to persist realtime history: {error}");
        }
        // Save the completed review and its terminal event together before the parent sees
        // the decision. Turn completion skips its separate before/after barriers for this case.
        if flush_guardian_completion && let Err(err) = self.flush_rollout().await {
            warn!("failed to flush completed Guardian review: {err}");
        }
        self.deliver_event_raw(event).await;
    }

    pub(super) async fn deliver_event_raw(&self, event: Event) {
        if let Some(parent_tx) = self.parent_observer_event_tx.get() {
            let forwarded = match &event.msg {
                EventMsg::RawResponseItem(item) => {
                    Some(PrivateSessionEventPayload::RecordedItem(item.clone()))
                }
                EventMsg::TurnAborted(aborted) => {
                    Some(PrivateSessionEventPayload::TurnAborted(aborted.clone()))
                }
                _ => None,
            };
            if let Some(forwarded) = forwarded {
                // Queue observer events before the private consumer can finish the review.
                // They bypass the parent's history and retain the reviewer's identity.
                let _ = parent_tx
                    .send(Event {
                        id: event.id.clone(),
                        msg: EventMsg::PrivateSessionEvent(PrivateSessionEvent {
                            thread_id: self.thread_id,
                            event: forwarded,
                        }),
                    })
                    .await;
            }
        }
        // Record the last known agent status.
        if let Some(status) = agent_status_from_event(&event.msg) {
            self.agent_status.send_replace(status);
        }
        if let Err(e) = self.tx_event.send(event).await {
            debug!("dropping event because channel is closed: {e}");
        }
    }

    pub(crate) async fn emit_turn_item_started(&self, turn_context: &TurnContext, item: &TurnItem) {
        if matches!(
            item,
            TurnItem::WebSearch(_)
                | TurnItem::Extension(codex_extension_items::ExtensionItem::WebSearch(_))
        ) && let Some(usage) = turn_context
            .extension_data
            .get::<crate::model_search::SearchTurnUsage>()
        {
            // This turn selected Search MCP. Pin native only when a fallback call starts.
            crate::model_search::SearchOwner::admit(
                &self.services.thread_extension_data,
                codex_protocol::openai_models::WebSearchProvider::Standalone,
            );
            usage.record(
                turn_context,
                codex_protocol::openai_models::WebSearchProvider::Standalone,
            );
        }
        let started_at_ms = turn_context
            .turn_timing_state
            .record_item_started(item.id(), now_unix_timestamp_ms())
            .await;
        self.send_event(
            turn_context,
            EventMsg::ItemStarted(ItemStartedEvent {
                thread_id: self.thread_id,
                turn_id: turn_context.sub_id.clone(),
                item: item.clone(),
                started_at_ms,
            }),
        )
        .await;
    }

    pub(crate) async fn emit_turn_item_completed(
        &self,
        turn_context: &TurnContext,
        item: TurnItem,
    ) {
        self.emit_turn_item_completed_with_metadata(
            turn_context,
            item,
            /*content_token_count*/ None,
            /*user_input_extension_data*/ None,
        )
        .await;
    }

    pub(crate) async fn emit_turn_item_completed_with_metadata(
        &self,
        turn_context: &TurnContext,
        item: TurnItem,
        content_token_count: Option<u64>,
        user_input_extension_data: Option<&BTreeMap<String, Value>>,
    ) {
        record_turn_ttfm_metric(turn_context, &item).await;
        for contributor in self.services.extensions.turn_lifecycle_contributors() {
            contributor
                .on_item_completed_with_metadata(
                    &self.services.thread_extension_data,
                    turn_context.extension_data.as_ref(),
                    &item,
                    user_input_extension_data,
                )
                .await;
        }
        let completed_at_ms = now_unix_timestamp_ms();
        let item_id = item.id();
        let started_at_ms = turn_context
            .turn_timing_state
            .take_item_started(&item_id)
            .await
            .unwrap_or_else(|| {
                warn!(
                    thread_id = %self.thread_id,
                    turn_id = %turn_context.sub_id,
                    item_id = %item_id,
                    "item completed without a recorded start timestamp"
                );
                completed_at_ms
            });
        self.send_event(
            turn_context,
            EventMsg::ItemCompleted(ItemCompletedEvent {
                thread_id: self.thread_id,
                turn_id: turn_context.sub_id.clone(),
                item,
                content_token_count,
                started_at_ms: Some(started_at_ms),
                completed_at_ms,
            }),
        )
        .await;
    }

    pub(crate) async fn record_inter_agent_communication(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        communication: impl Into<crate::PendingAgentMessage>,
    ) {
        let communication = communication.into();
        let mut response_item = communication.to_model_input_item();
        if let ResponseItem::AgentMessage {
            content,
            internal_chat_message_metadata_passthrough,
            ..
        } = &mut response_item
        {
            internal_chat_message_metadata_passthrough
                .get_or_insert_default()
                .content_item_metadata
                .get_or_insert_with(|| {
                    vec![
                        ContentItemMetadata::tool(Some(
                            turn_context
                                .config
                                .multi_agent_v2
                                .tool_namespace
                                .clone()
                                .map(ContentItemNamespace::from)
                                .unwrap_or(ContentItemNamespace::Functions),
                        ));
                        content.len()
                    ]
                });
        }
        let (items, _) = self
            .prepare_conversation_items_for_history(
                turn_context,
                model_info,
                std::slice::from_ref(&response_item),
            )
            .await;
        let mut response_item = ResponseItemEnvelope::new(items[0].clone());
        // A send confirmed after the pending snapshot must not reach the rollout
        // before this boundary; older readers assign deliveries by physical order.
        let boundary = self
            .code_mode_message_tasks
            .communication_boundary
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("communication boundary remains open"));
        // Older readers assign delivered assistant messages to the next physical
        // communication boundary, so persist earlier confirmed sends first.
        let (order, pending) = {
            let mut state = self.state.lock().await;
            (
                state.history.reserve_input_order(),
                self.pending_code_mode_message_recordings(),
            )
        };
        response_item
            .metadata
            .get_or_insert_default()
            .user_input_order = Some(order);
        for mut recording in pending {
            let _ = recording.changed().await;
        }
        {
            let mut state = self.state.lock().await;
            state.current_time_reminder.note_recorded_items(&items);
            state.history.record_annotated_items(
                std::slice::from_mut(&mut response_item),
                model_info.truncation_policy.into(),
            );
        }
        if let Some(delivery) = &communication.delivery {
            delivery.consumed(self.thread_id);
        }
        self.persist_rollout_items(&[
            RolloutItem::InterAgentCommunicationMetadata {
                trigger_turn: communication.trigger_turn,
            },
            RolloutItem::ResponseItem(response_item),
        ])
        .await;
        drop(boundary);
        self.send_raw_response_items(turn_context, &items).await;
    }

    pub(super) async fn maybe_warn_on_server_model_mismatch(
        self: &Arc<Self>,
        step_context: &StepContext,
        server_model: String,
    ) -> bool {
        let turn_context = &step_context.turn;
        let requested_model = step_context.settings.model_info.slug.clone();
        let server_model_normalized = server_model.to_ascii_lowercase();
        let requested_model_normalized = requested_model.to_ascii_lowercase();
        if server_model_normalized == requested_model_normalized {
            info!("server reported model {server_model} (matches requested model)");
            return false;
        }

        warn!("server reported model {server_model} while requested model was {requested_model}");

        let warning_message = format!(
            "Your account was flagged for potentially high-risk cyber activity and this request was routed to gpt-5.2 as a fallback. To regain access to gpt-5.3-codex, apply for trusted access: {CYBER_VERIFY_URL} or learn more: {CYBER_SAFETY_URL}"
        );

        self.send_event(
            turn_context,
            EventMsg::ModelReroute(ModelRerouteEvent {
                from_model: requested_model.clone(),
                to_model: server_model.clone(),
                reason: ModelRerouteReason::HighRiskCyberActivity,
            }),
        )
        .await;

        self.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: warning_message.clone(),
            }),
        )
        .await;
        true
    }

    pub(crate) async fn emit_model_verification(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        verifications: Vec<ModelVerification>,
    ) {
        self.send_event(
            turn_context,
            EventMsg::ModelVerification(ModelVerificationEvent { verifications }),
        )
        .await;
    }

    pub(crate) async fn emit_turn_moderation_metadata(
        self: &Arc<Self>,
        turn_context: &Arc<TurnContext>,
        metadata: TurnModerationMetadataEvent,
    ) {
        self.send_event(turn_context, EventMsg::TurnModerationMetadata(metadata))
            .await;
    }

    pub(crate) async fn record_response_item_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        response_item: ResponseItem,
    ) {
        // Add to conversation history and persist response item to rollout.
        self.record_conversation_items(
            turn_context,
            model_info,
            std::slice::from_ref(&response_item),
        )
        .await;

        // Derive a turn item and emit lifecycle events if applicable.
        if let Some(item) = parse_turn_item(&response_item) {
            self.emit_turn_item_started(turn_context, &item).await;
            self.emit_turn_item_completed(turn_context, item).await;
        }
    }

    pub(crate) async fn record_user_prompt_and_emit_turn_item(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        input: &[UserInput],
        client_id: Option<String>,
        metadata: UserInputMetadata,
        persist_context: PersistContext,
    ) {
        // Persist the user message to history, but emit the turn item from `UserInput` so
        // UI-only `text_elements` are preserved. `ResponseItem::Message` does not carry
        // those spans, and `record_response_item_and_emit_turn_item` would drop them.
        let mut user_image_content_indices = HashMap::new();
        let mut response_item = self.response_item_from_user_input_with_image_positions(
            input.to_vec(),
            &mut user_image_content_indices,
        );
        if let Some(uuid) = metadata.response_item_uuid {
            response_item.set_id(Some(ResponseItemId::for_admitted_user_input(uuid)));
        }
        if metadata.origin == codex_history::UserInputOrigin::Heartbeat
            && let ResponseItem::Message {
                content,
                internal_chat_message_metadata_passthrough: Some(metadata),
                ..
            } = &mut response_item
            && matches!(content.as_slice(), [ContentItem::InputText { .. }])
        {
            metadata.content_item_kinds = Some(vec![ContentItemKind(
                codex_history::HEARTBEAT_CONTENT_KIND.to_owned(),
            )]);
        }
        let (prepared_items, image_preparations) = self
            .prepare_annotated_conversation_items_for_history(
                turn_context,
                model_info,
                vec![ResponseItemEnvelope {
                    item: response_item,
                    metadata: metadata.acceptance_order.map(|order| CodexHarnessMetadata {
                        user_input_order: Some(order),
                        ..Default::default()
                    }),
                }],
            )
            .await;
        let mut user_message_item = UserMessageItem::new(input);
        apply_prepared_image_file_ids(
            &mut user_message_item,
            &prepared_items,
            &user_image_content_indices,
        );
        self.record_prepared_conversation_items(
            turn_context,
            model_info,
            prepared_items,
            image_preparations,
        )
        .await;
        user_message_item.client_id = client_id;
        let turn_item = TurnItem::UserMessage(user_message_item);
        self.emit_turn_item_started(turn_context, &turn_item).await;
        self.emit_turn_item_completed_with_metadata(
            turn_context,
            turn_item,
            /*content_token_count*/ None,
            Some(&metadata.extension_data),
        )
        .await;
        self.ensure_rollout_materialized(persist_context).await;
    }

    pub(crate) async fn notify_stream_error(
        &self,
        turn_context: &TurnContext,
        message: impl Into<String>,
        codex_error: CodexErr,
    ) {
        let additional_details = codex_error.to_string();
        let codex_error_info = CodexErrorInfo::ResponseStreamDisconnected {
            http_status_code: codex_error.http_status_code_value(),
        };
        let event = EventMsg::StreamError(StreamErrorEvent {
            message: message.into(),
            codex_error_info: Some(codex_error_info),
            additional_details: Some(additional_details),
        });
        self.send_event(turn_context, event).await;
    }

    fn show_raw_agent_reasoning(&self) -> bool {
        self.services.show_raw_agent_reasoning
    }
}

fn apply_prepared_image_file_ids(
    user_message_item: &mut UserMessageItem,
    prepared_items: &[ResponseItemEnvelope],
    user_image_content_indices: &HashMap<usize, usize>,
) {
    // Preparation replaces image slots in place; resize notices are separate messages.
    // Read the references from the original message while retaining UI-only input spans.
    if let Some(ResponseItemEnvelope {
        item: ResponseItem::Message { content, .. },
        ..
    }) = prepared_items.first()
    {
        for (&input_index, &content_index) in user_image_content_indices {
            if let Some(ContentItem::InputImage {
                image: ImageReference::File { file_id },
                ..
            }) = content.get(content_index)
                && let UserInput::Image { detail, .. } | UserInput::LocalImage { detail, .. } =
                    &user_message_item.content[input_index]
            {
                user_message_item.content[input_index] = UserInput::Image {
                    image: ImageReference::File {
                        file_id: file_id.clone(),
                    },
                    detail: *detail,
                };
            }
        }
    }
}
