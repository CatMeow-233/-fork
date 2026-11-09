//! Token usage and rate-limit accounting for a session.

use super::*;

impl Session {
    pub(crate) async fn get_total_token_usage(&self) -> i64 {
        let state = self.state.lock().await;
        state.get_total_token_usage()
    }

    pub(crate) async fn auto_compact_window_snapshot(&self) -> AutoCompactWindowSnapshot {
        let state = self.state.lock().await;
        state.auto_compact_window_snapshot()
    }

    pub(crate) async fn estimated_tokens_since_last_usage(&self) -> i64 {
        let state = self.state.lock().await;
        state.history.estimated_tokens_since_last_usage()
    }

    pub(crate) async fn total_token_usage(&self) -> Option<TokenUsage> {
        let state = self.state.lock().await;
        state.token_info().map(|info| info.total_token_usage)
    }

    /// Returns the complete token usage snapshot currently cached for this session.
    ///
    /// Resume and fork reconstruction seed this state from the last persisted rollout
    /// `TokenCount` event. Callers that need to replay restored usage to a client
    /// should use this accessor instead of `total_token_usage`, because the app-server
    /// notification includes both total and last-turn usage.
    pub(crate) async fn token_usage_info(&self) -> Option<TokenUsageInfo> {
        let state = self.state.lock().await;
        state.token_info()
    }

    pub(crate) async fn get_estimated_token_count(
        &self,
        turn_context: &TurnContext,
    ) -> Option<i64> {
        let history = self.clone_history().await;
        history.estimate_token_count(turn_context)
    }

    /// Record reported spending without changing the context baseline or estimating usage.
    pub(crate) async fn record_incomplete_response_usage(
        &self,
        turn_context: &TurnContext,
        settings: &ResolvedStepSettings,
        incomplete: &ResponseIncompleteError,
    ) {
        if let Some(response_id) = &incomplete.response_id {
            self.record_response_token_usage(
                turn_context,
                response_id,
                incomplete.token_usage.as_ref(),
            )
            .await;
        }
        // Accounting may exhaust the budget; keep the original incomplete-response reason.
        let _ = self
            .record_token_usage_info(
                turn_context,
                settings,
                incomplete.token_usage.as_ref(),
                TokenUsageContext::SpendingOnly,
            )
            .await;
    }

    pub(crate) async fn update_token_usage_info(
        &self,
        turn_context: &TurnContext,
        token_usage: Option<&TokenUsage>,
        includes_reasoning: bool,
    ) -> CodexResult<()> {
        let result = self
            .record_token_usage_info(
                turn_context,
                &turn_context.initial_settings,
                token_usage,
                TokenUsageContext::Sampling {
                    includes_reasoning,
                    input_item_count: 0,
                },
            )
            .await;
        self.send_token_count_event(turn_context).await;
        result
    }

    pub(crate) async fn record_observed_response_completed(
        &self,
        turn_context: &TurnContext,
        response_id: &str,
        usage: Option<&TokenUsage>,
        usage_metadata: Option<&ResponseUsageMetadata>,
    ) {
        self.send_event(
            turn_context,
            EventMsg::RawResponseCompleted(RawResponseCompletedEvent {
                response_id: response_id.to_string(),
                token_usage: usage.cloned(),
                usage_metadata: usage_metadata.cloned(),
            }),
        )
        .await;
        self.record_response_token_usage(turn_context, response_id, usage)
            .await;
    }

    pub(crate) async fn record_response_token_usage(
        &self,
        turn_context: &TurnContext,
        response_id: &str,
        usage: Option<&TokenUsage>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        let record = self.state.lock().await.record_token_usage(
            self.thread_id,
            &turn_context.sub_id,
            self.session_id(),
            turn_context
                .turn_metadata_state
                .root_turn_id()
                .unwrap_or_else(|| turn_context.sub_id.clone()),
            response_id.to_string(),
            usage,
        );
        self.persist_rollout_items(&[RolloutItem::TokenUsageRecord(record)])
            .await;
    }

    pub(crate) async fn record_token_usage_info(
        &self,
        turn_context: &TurnContext,
        settings: &ResolvedStepSettings,
        token_usage: Option<&TokenUsage>,
        context: TokenUsageContext,
    ) -> CodexResult<()> {
        if let Some(token_usage) = token_usage {
            let token_info = {
                let mut state = self.state.lock().await;
                match context {
                    TokenUsageContext::Sampling {
                        includes_reasoning,
                        input_item_count,
                    } => {
                        state.update_token_info_from_usage(
                            token_usage,
                            turn_context.model_context_window(),
                            includes_reasoning,
                            input_item_count,
                        );
                        if matches!(
                            turn_context.config.model_auto_compact_token_limit_scope,
                            AutoCompactTokenLimitScope::BodyAfterPrefix
                        ) {
                            state.ensure_auto_compact_window_server_prefill_from_usage(token_usage);
                        }
                        state.token_info()
                    }
                    TokenUsageContext::SpendingOnly => {
                        let mut info = state.token_info().unwrap_or(TokenUsageInfo {
                            total_token_usage: TokenUsage::default(),
                            last_usage_includes_reasoning: None,
                            last_token_usage: TokenUsage::default(),
                            model_context_window: turn_context.model_context_window(),
                        });
                        info.total_token_usage.add_assign(token_usage);
                        state.set_token_info(Some(info.clone()));

                        // Contributors receive the reported usage; stored context stays unchanged.
                        info.last_token_usage = token_usage.clone();
                        info.last_usage_includes_reasoning = None;
                        info.model_context_window = turn_context
                            .model_context_window()
                            .or(info.model_context_window);
                        Some(info)
                    }
                }
            };
            let turn_state = self
                .input_queue
                .turn_state_for_sub_id(&self.active_turn, &turn_context.sub_id)
                .await;
            if let Some(turn_state) = turn_state {
                turn_state.lock().await.token_usage_by_model.record(
                    settings.selected_collaboration_mode().model(),
                    settings.telemetry(&turn_context.session_telemetry),
                    token_usage,
                );
            }
            let budget_result = self
                .record_rollout_budget_usage(&turn_context.sub_id, token_usage)
                .await;
            if let Some(token_info) = token_info.as_ref() {
                for contributor in self.services.extensions.token_usage_contributors() {
                    contributor
                        .on_token_usage(
                            &self.services.session_extension_data,
                            &self.services.thread_extension_data,
                            turn_context.extension_data.as_ref(),
                            token_info,
                        )
                        .await;
                }
            }
            budget_result?;
        }
        Ok(())
    }

    pub(crate) async fn recompute_token_usage(&self, turn_context: &TurnContext) {
        let history = self.clone_history().await;
        let base_instructions = self.get_base_instructions().await;
        let Some(estimated_total_tokens) =
            history.estimate_token_count_with_base_instructions(&base_instructions)
        else {
            return;
        };
        {
            let mut state = self.state.lock().await;
            let mut info = state.token_info().unwrap_or(TokenUsageInfo {
                total_token_usage: TokenUsage::default(),
                last_usage_includes_reasoning: None,
                last_token_usage: TokenUsage::default(),
                model_context_window: None,
            });

            info.last_token_usage = TokenUsage {
                input_tokens: 0,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: 0,
                reasoning_output_tokens: 0,
                total_tokens: estimated_total_tokens.max(0),
                codex_rollout_budget_units: None,
            };

            if let Some(model_context_window) = turn_context.model_context_window() {
                info.model_context_window = Some(model_context_window);
            }

            // The local estimate already includes every retained reasoning item.
            info.last_usage_includes_reasoning = Some(true);
            state
                .history
                .set_token_info_with_boundary(Some(info), history.raw_items().len());
        }
        self.set_auto_compact_window_estimated_prefill_for_scope(
            turn_context,
            estimated_total_tokens,
        )
        .await;
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn update_rate_limits(
        &self,
        turn_context: &TurnContext,
        new_rate_limits: RateLimitSnapshot,
    ) {
        self.record_rate_limits_info(new_rate_limits).await;
        self.send_token_count_event(turn_context).await;
    }

    pub(crate) async fn record_rate_limits_info(&self, new_rate_limits: RateLimitSnapshot) {
        {
            let mut state = self.state.lock().await;
            state.set_rate_limits(new_rate_limits);
        }
    }

    pub(crate) async fn send_token_count_event(&self, turn_context: &TurnContext) {
        let (info, rate_limits) = {
            let state = self.state.lock().await;
            state.token_info_and_rate_limits()
        };
        let event = EventMsg::TokenCount(TokenCountEvent { info, rate_limits });
        self.send_event(turn_context, event).await;
    }

    pub(crate) async fn set_total_tokens_full(&self, turn_context: &TurnContext) {
        if let Some(context_window) = turn_context.model_context_window() {
            let mut state = self.state.lock().await;
            state.set_token_usage_full(context_window);
        }
        self.send_token_count_event(turn_context).await;
    }
}
