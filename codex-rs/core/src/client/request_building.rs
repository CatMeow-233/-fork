//! Responses API request bodies, request headers, and request-scoped client metadata.

use super::*;

pub(super) fn session_telemetry_for_request(
    session_telemetry: &SessionTelemetry,
    request: &ResponsesApiRequest,
) -> SessionTelemetry {
    session_telemetry.clone().with_inference_request(
        request.service_tier.as_deref(),
        request
            .reasoning
            .as_ref()
            .and_then(|reasoning| reasoning.effort.as_ref()),
    )
}

impl ModelClient {
    pub(super) fn build_subagent_headers(&self) -> ApiHeaderMap {
        let mut extra_headers = ApiHeaderMap::new();
        add_originator_header(&mut extra_headers, self.state.originator.as_str());
        if let Some(subagent) = subagent_header_value(&self.state.session_source)
            && let Ok(val) = HeaderValue::from_str(&subagent)
        {
            extra_headers.insert(X_OPENAI_SUBAGENT_HEADER, val);
        }
        if matches!(
            self.state.session_source,
            SessionSource::Internal(InternalSessionSource::MemoryConsolidation)
        ) {
            extra_headers.insert(
                X_OPENAI_MEMGEN_REQUEST_HEADER,
                HeaderValue::from_static("true"),
            );
        }
        extra_headers
    }

    pub(super) fn build_responses_compatibility_headers(
        &self,
        responses_metadata: &CodexResponsesMetadata,
    ) -> ApiHeaderMap {
        let mut extra_headers = responses_metadata.compatibility_headers();
        if matches!(
            self.state.session_source,
            SessionSource::Internal(InternalSessionSource::MemoryConsolidation)
        ) {
            extra_headers.insert(
                X_OPENAI_MEMGEN_REQUEST_HEADER,
                HeaderValue::from_static("true"),
            );
        }
        extra_headers
    }

    pub(super) fn build_ws_client_metadata(
        &self,
        responses_metadata: &CodexResponsesMetadata,
        include_internal: bool,
        use_responses_lite: bool,
    ) -> HashMap<String, String> {
        let mut client_metadata = responses_metadata.client_metadata(include_internal);
        if use_responses_lite {
            client_metadata.insert(
                WS_REQUEST_HEADER_RESPONSES_LITE_CLIENT_METADATA_KEY.to_string(),
                "true".to_string(),
            );
        }
        client_metadata
    }

    pub(super) async fn generate_attestation_header_for(&self) -> Option<HeaderValue> {
        if !self.state.include_attestation {
            return None;
        }

        self.state
            .attestation_provider
            .as_ref()?
            .header_for_request(AttestationContext {
                thread_id: self.state.thread_id,
            })
            .await
    }

    fn build_reasoning(
        &self,
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
    ) -> Reasoning {
        Reasoning {
            effort: effort
                .or_else(|| model_info.default_reasoning_level.clone())
                .map(|effort| model_info.resolve_reasoning_effort(effort)),
            summary: (self.state.provider.capabilities().reasoning_summaries
                && model_info.supports_reasoning_summary_parameter
                && summary != ReasoningSummaryConfig::None)
                .then_some(summary),
            // When Responses Lite is disabled, omit context so Responses uses the default,
            // which is currently `current_turn`.
            context: model_info
                .use_responses_lite
                .then_some(ReasoningContext::AllTurns),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_responses_request(
        &self,
        prompt: &Prompt,
        model_info: &ModelInfo,
        effort: Option<ReasoningEffortConfig>,
        summary: ReasoningSummaryConfig,
        service_tier: Option<String>,
        responses_metadata: &CodexResponsesMetadata,
        include_internal: bool,
    ) -> Result<ResponsesApiRequest> {
        let mut input = prompt.get_formatted_input_for_request(model_info);
        if !self.reasoning_effort_override_enabled(model_info) {
            // Unsupported models and disabled overrides must also accept saved history.
            // Filter only the request copy; persisted history remains unchanged.
            input.retain(|item| !matches!(item, ResponseItem::ConfigurationUpdate { .. }));
        } else {
            input.dedup_by(|next, previous| {
                if matches!(next, ResponseItem::ConfigurationUpdate { .. })
                    && matches!(previous, ResponseItem::ConfigurationUpdate { .. })
                {
                    // Keep the latest update; dedup_by removes `next`.
                    std::mem::swap(previous, next);
                    true
                } else {
                    false
                }
            });
        }
        let is_openai = self.state.provider.info().is_openai();
        // These prompt-only items are rebuilt on every request. Hash their visible payloads
        // within the thread so retries and resumed sessions preserve their identity.
        let prefix_namespace = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            self.state.thread_id.to_string().as_bytes(),
        );
        let mut prefix = Vec::new();
        let tools = if model_info.use_responses_lite {
            if !prompt.tools.is_empty() {
                let tools = create_tools_json_for_responses_lite(&prompt.tools)?;
                prefix.push(ResponseItem::AdditionalTools {
                    id: Some(ResponseItemId::with_suffix(
                        "at",
                        Uuid::new_v5(&prefix_namespace, &serde_json::to_vec(&tools)?),
                    )),
                    role: "developer".to_string(),
                    tools: Arc::from(tools),
                });
            }
            None
        } else {
            Some(create_tools_raw_json_for_responses_api(&prompt.tools)?.into())
        };
        if !prompt.base_instructions.text.is_empty() {
            let mut instructions = ContextualUserFragment::into(BaseInstructionsFragment(
                prompt.base_instructions.text.clone(),
            ));
            instructions.set_id(Some(ResponseItemId::with_suffix(
                "msg",
                Uuid::new_v5(&prefix_namespace, prompt.base_instructions.text.as_bytes()),
            )));
            prefix.push(instructions);
        }
        input.splice(0..0, prefix);
        if !model_info.use_responses_lite {
            // Only RLite accepts per-content attribution until the Responses API schema expands.
            for item in &mut input {
                item.clear_content_item_metadata();
            }
        }
        if !is_openai {
            for item in &mut input {
                item.clear_internal_chat_message_metadata_passthrough();
                if !matches!(
                    item,
                    ResponseItem::Reasoning { .. }
                        | ResponseItem::Compaction { .. }
                        | ResponseItem::ContextCompaction { .. }
                ) {
                    item.clear_encrypted_content();
                }
                if let ResponseItem::FunctionCall {
                    encrypted_function_args,
                    ..
                } = item
                {
                    *encrypted_function_args = None;
                }
            }
        }
        let reasoning = self.build_reasoning(model_info, effort, summary);
        let stream_options = (self.state.concurrent_reasoning_summaries_enabled
            && is_openai
            && reasoning.summary.is_some())
        .then_some(StreamOptions {
            reasoning_summary_delivery: codex_api::ReasoningSummaryDelivery::SequentialCutoff,
        });
        let mut include = vec!["reasoning.encrypted_content".to_string()];
        if self.state.output_token_replay_enabled && is_openai {
            include.push("output.encrypted_content".to_string());
        }
        let verbosity = if model_info.support_verbosity {
            self.state.model_verbosity.or(model_info.default_verbosity)
        } else {
            if self.state.model_verbosity.is_some() {
                warn!(
                    "model_verbosity is set but ignored as the model does not support verbosity: {}",
                    model_info.slug
                );
            }
            None
        };
        let text = create_text_param_for_request(
            verbosity,
            &prompt.output_schema,
            prompt.output_schema_strict,
        );
        let service_tier = model_info
            .service_tier_for_request(service_tier)
            .filter(|tier| {
                // Bedrock requires an advertised tier, including for flex, which the
                // generic OpenAI resolver permits without catalog support.
                !self.state.provider.info().is_amazon_bedrock()
                    || model_info
                        .service_tiers
                        .iter()
                        .any(|supported| supported.id == *tier)
            });
        if !include_internal {
            for item in &mut input {
                item.clear_tool_result_metadata();
            }
        }
        let client_metadata = responses_metadata.client_metadata(include_internal);
        let request = ResponsesApiRequest {
            model: model_info.slug.clone(),
            input,
            tools,
            tool_choice: "auto".to_string(),
            parallel_tool_calls: prompt.parallel_tool_calls && !model_info.use_responses_lite,
            reasoning: Some(reasoning),
            store: false,
            stream: true,
            stream_options,
            include,
            service_tier,
            prompt_cache_key: self.prompt_cache_key(responses_metadata),
            prompt_cache_options: prompt
                .prompt_cache_ttl
                .map(|ttl| codex_api::PromptCacheOptions { ttl }),
            text,
            client_metadata: Some(client_metadata),
            access_programs: None,
        };
        Ok(request)
    }

    pub(super) fn prepare_response_items_for_request(&self, input: &mut [ResponseItem]) {
        for item in input {
            if item.id().is_some_and(|id| !id.is_prefixed()) {
                item.set_id(/*new_id*/ None);
            }
            if !self.state.content_item_kinds_enabled {
                item.clear_content_item_kinds();
            }
        }
    }

    pub(super) fn responses_headers(&self, auth: Option<&CodexAuth>, model: &str) -> ApiHeaderMap {
        self.codex_responses_headers
            .as_ref()
            .filter(|config| config.model == model && self.uses_codex_backend(auth))
            .map(|config| config.headers.clone())
            .unwrap_or_default()
    }

    fn uses_codex_backend(&self, auth: Option<&CodexAuth>) -> bool {
        let provider = self.state.provider.info();
        auth.is_some_and(CodexAuth::uses_codex_backend)
            && provider.is_openai()
            && provider.requires_openai_auth
            && provider.env_key.is_none()
            && provider.experimental_bearer_token.is_none()
            && provider.auth.is_none()
            && provider.aws.is_none()
    }

    pub(super) fn set_guardian_metadata(
        &self,
        metadata: &mut Option<HashMap<String, String>>,
        parent_response_id: Option<&str>,
        auth: Option<&CodexAuth>,
        responses_headers: &ApiHeaderMap,
    ) {
        if let Some(metadata) = metadata.as_mut() {
            metadata.remove("guardian_credits_requested");
            metadata.remove("parent_response_id");
        }
        let guardian_reviewer = is_guardian_reviewer(responses_headers);
        if guardian_reviewer && let Some(parent_response_id) = parent_response_id {
            metadata.get_or_insert_with(HashMap::new).insert(
                "parent_response_id".to_owned(),
                parent_response_id.to_owned(),
            );
        }
        if !guardian_reviewer
            && !crate::guardian::is_basic_session_source(&self.state.session_source)
            && matches!(
                auth,
                Some(CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_))
            )
            && self.uses_codex_backend(auth)
        {
            metadata
                .get_or_insert_with(HashMap::new)
                .insert("guardian_credits_requested".to_owned(), "true".to_owned());
        }
    }

    pub(super) fn build_routing_hint_header(
        &self,
        auth: Option<&CodexAuth>,
        responses_headers: &ApiHeaderMap,
        model: &str,
        service_tier: Option<&str>,
    ) -> Option<HeaderValue> {
        if !self.uses_codex_backend(auth) || is_guardian_reviewer(responses_headers) {
            return None;
        }

        let routing_hint = match service_tier {
            Some(tier) => format!("model={model};tier={tier}"),
            None => format!("model={model}"),
        };
        HeaderValue::from_str(&routing_hint).ok()
    }
}

pub(super) fn is_guardian_reviewer(responses_headers: &ApiHeaderMap) -> bool {
    responses_headers
        .get("x-codex-guardian")
        .is_some_and(|value| value == "reviewer")
}

/// Builds the extra headers attached to Responses API requests.
///
/// These headers implement Codex-specific conventions:
///
/// - `x-codex-beta-features`: comma-separated beta feature keys enabled for the session.
/// - `x-codex-turn-state`: sticky routing token captured earlier in the turn.
pub(super) fn build_responses_headers(
    beta_features_header: Option<&str>,
    turn_state: Option<&Arc<OnceLock<String>>>,
) -> ApiHeaderMap {
    let mut headers = ApiHeaderMap::new();
    if let Some(value) = beta_features_header
        && !value.is_empty()
        && let Ok(header_value) = HeaderValue::from_str(value)
    {
        headers.insert("x-codex-beta-features", header_value);
    }
    if let Some(turn_state) = turn_state
        && let Some(state) = turn_state.get()
        && let Ok(header_value) = HeaderValue::from_str(state)
    {
        headers.insert(X_CODEX_TURN_STATE_HEADER, header_value);
    }
    headers
}

pub(super) fn add_responses_lite_header(headers: &mut ApiHeaderMap, use_responses_lite: bool) {
    if use_responses_lite {
        headers.insert(
            X_OPENAI_INTERNAL_CODEX_RESPONSES_LITE_HEADER,
            HeaderValue::from_static("true"),
        );
    }
}
