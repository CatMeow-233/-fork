//! Step context capture and model-visible initial context assembly.

use super::*;

impl Session {
    pub(crate) async fn record_step_world_state_if_changed(
        &self,
        step_context: &step_context::StepContext,
    ) -> CodexResult<Arc<WorldState>> {
        let turn_context = step_context.turn.as_ref();
        // Render model-visible state from the same step used to build and run tools.
        let world_state = self
            .build_world_state_for_step(step_context, /*new_window*/ false)
            .await?;
        let (world_state_snapshot, mut fragments, world_state_item) = self
            .state
            .lock()
            .await
            .history
            .render_step_world_state(&world_state);
        world_state.resolve_host_roster(&mut fragments).await;
        let items = crate::context_manager::updates::merge_world_state_updates(fragments);
        if !items.is_empty() {
            self.record_conversation_items(turn_context, &step_context.settings.model_info, &items)
                .await;
        }

        self.state
            .lock()
            .await
            .history
            .set_world_state_baseline(world_state_snapshot);
        // Record the patch after the context it describes is present in model history.
        if let Some(world_state_item) = world_state_item {
            self.persist_rollout_items(&[RolloutItem::WorldState(world_state_item)])
                .await;
        }
        Ok(Arc::new(world_state))
    }

    /// Retains the step captured for execution.
    pub(crate) async fn set_last_known_step_context(&self, step_context: &Arc<StepContext>) {
        let turn_state = {
            let active_turn = self.active_turn.lock().await;
            active_turn.as_ref().and_then(|active_turn| {
                active_turn
                    .task
                    .as_ref()
                    .filter(|task| task.turn_context.sub_id == step_context.turn.sub_id)
                    .map(|_| Arc::clone(&active_turn.turn_state))
            })
        };
        if let Some(turn_state) = turn_state {
            turn_state.lock().await.last_known_step_context = Some(Arc::clone(step_context));
        }
    }

    /// Captures one request-scoped view of dynamic state and retains it for the active turn.
    ///
    /// This may refresh filesystem-derived state. Normal turns should call it only from
    /// `run_turn` and pass the result down; standalone request or history boundaries may capture
    /// their own step. Use speculative capture for a step that may not execute.
    pub(crate) async fn capture_step_context(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: &CancellationToken,
    ) -> CodexResult<Arc<StepContext>> {
        self.capture_step_context_with_required_mcp_servers(
            turn_context,
            cancellation_token,
            /*required_servers*/ &[],
            /*required_plugins*/ &HashSet::new(),
        )
        .await
    }

    /// Captures and retains a step with the required MCP startup dependencies.
    pub(crate) async fn capture_step_context_with_required_mcp_servers(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: &CancellationToken,
        required_servers: &[String],
        required_plugins: &HashSet<String>,
    ) -> CodexResult<Arc<StepContext>> {
        let step_context = self
            .capture_step_context_inner(
                turn_context,
                cancellation_token,
                required_servers,
                required_plugins,
            )
            .await?;
        self.set_last_known_step_context(&step_context).await;
        Ok(step_context)
    }

    /// Prepares a candidate step without replacing the active turn's retained context.
    /// The caller must retain it explicitly if it is selected for execution.
    pub(crate) async fn capture_speculative_step_context(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: &CancellationToken,
    ) -> CodexResult<Arc<StepContext>> {
        self.capture_step_context_inner(
            turn_context,
            cancellation_token,
            /*required_servers*/ &[],
            /*required_plugins*/ &HashSet::new(),
        )
        .await
    }

    pub(super) fn inherits_root_service_tier(&self, session_source: &SessionSource) -> bool {
        match &self.agent_runtime {
            crate::agent::host::AgentRuntimeServices::Controller => matches!(
                session_source,
                SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
            ),
            crate::agent::host::AgentRuntimeServices::Host(_) => {
                crate::agent::host::host_child_uses_shared_service_tier(self)
            }
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "step settings and both environment snapshots must be captured under active_turn then state"
    )]
    #[tracing::instrument(name = "step_context.capture", level = "info", skip_all)]
    async fn capture_step_context_inner(
        self: &Arc<Self>,
        turn_context: Arc<TurnContext>,
        cancellation_token: &CancellationToken,
        required_servers: &[String],
        required_plugins: &HashSet<String>,
    ) -> CodexResult<Arc<StepContext>> {
        // Read the step's model and record its environments together so an update cannot split them.
        // Match apply_turn_settings and activate_turn_environments: active_turn before state.
        // Wait for executor startup below, after releasing both locks.
        let (mut settings, environments, inherited_environments) = {
            let _active = self
                .active_turn
                .lock()
                .or_cancel(cancellation_token)
                .await?;
            let _state = self.state.lock().or_cancel(cancellation_token).await?;
            (
                turn_context.next_step_settings.load_full(),
                self.services.turn_environments.snapshot(),
                self.features
                    .enabled(Feature::SubagentThreadEnvironments)
                    .then(|| self.services.turn_environments.configured_snapshot_now()),
            )
        };
        if self.inherits_root_service_tier(&turn_context.session_source) {
            let root_service_tier = tokio::select! {
                biased;
                tier = crate::agent::host::service_tier(self, settings.selected().service_tier.clone()) => tier,
                _ = cancellation_token.cancelled() => return Err(CodexErr::TurnAborted),
            };
            if settings.selected().service_tier != root_service_tier {
                let mut selected = settings.selected().clone();
                selected.service_tier = root_service_tier;
                let mut inherited_settings = ResolvedStepSettings::new(
                    Arc::new(selected),
                    Arc::clone(&settings.model_info),
                    &self.features,
                );
                inherited_settings.mcp_approvals_reviewer_override =
                    settings.mcp_approvals_reviewer_override;
                settings = Arc::new(inherited_settings);
            }
        }
        let dynamic_tools = self.dynamic_tools().await;
        let token_budget = token_budget::resolve_token_budget(
            turn_context.configured_token_budget.as_ref(),
            turn_context.use_model_token_budget_defaults,
            settings.model_info.as_ref(),
        );
        let session_telemetry = settings.telemetry(&turn_context.session_telemetry);
        let environments = environments.or_cancel(cancellation_token).await?;
        let inherited_environments = inherited_environments.unwrap_or_else(|| environments.clone());
        // Keep both preparation futures off caller stacks while they are live together.
        let load_agents_md = Box::pin(async {
            let (loaded_agents_md, warnings) = self
                .services
                .agents_md_manager
                .refresh(&turn_context.config, &environments)
                .or_cancel(cancellation_token)
                .await?;
            self.emit_instruction_warnings(warnings).await;
            loaded_agents_md
        });
        let prepare_tools = Box::pin(async {
            let selected_capability_roots = self
                .resolve_selected_capability_roots_for_step(&environments)
                .await;
            let ready_selected_capability_roots =
                Self::ready_selected_capability_roots(&selected_capability_roots);
            let (executor_capability_discovery_v2, inventory_changed) = self
                .executor_capability_discovery_for_step_v2(&turn_context.config, &environments)
                .await;
            let v2_snapshots = executor_capability_discovery_v2.as_ref();
            let (executor_capability_discovery, discovery_recovered) = if v2_snapshots.is_some() {
                (None, false)
            } else {
                self.executor_capability_discovery_for_step(
                    &turn_context.config,
                    &selected_capability_roots,
                    &environments,
                    executor_capability_discovery_v2.as_deref(),
                )
                .await
            };
            if inventory_changed || discovery_recovered {
                self.mark_mcp_runtime_dirty();
            }
            let extension_data =
                codex_extension_api::ExtensionData::new(turn_context.sub_id.clone());
            extension_data.insert(extension_metrics::from_session_telemetry(
                session_telemetry.clone(),
            ));
            extension_data.insert_arc(Arc::clone(&settings.model_info.model));
            extension_data.insert_arc(
                self.host_skills_for_step(&turn_context, &environments)
                    .await,
            );
            if turn_context
                .config
                .features
                .enabled(Feature::SkillCatalogDeltas)
                && let Some(grants) = turn_context.turn_skill_filesystem_grants(&environments)
            {
                extension_data.insert(grants);
            }
            if let Some(messages) = settings
                .model_info
                .model_messages
                .as_ref()
                .and_then(|messages| messages.tools.as_ref())
                .and_then(|tools| tools.multi_agent.as_ref())
            {
                extension_data.insert(messages.clone());
            }
            let model_messages = ResolvedModelMessages::from_model(&settings.model_info);
            if let Some(messages) = model_messages.history() {
                extension_data.insert(messages.clone());
            }
            if let Some(messages) = model_messages.notes() {
                extension_data.insert(messages.clone());
            }
            let mut required_servers = required_servers.to_vec();
            if settings.model_info.web_search_provider
                == Some(codex_protocol::openai_models::WebSearchProvider::SearchMcp)
                && turn_context.apps_enabled()
                && !turn_context.config.requires_native_search()
                && (turn_context.config.web_search_mode.value()
                    != codex_protocol::config_types::WebSearchMode::Disabled
                    || turn_context
                        .config
                        .features
                        .enabled(codex_features::Feature::HostedSearchPolicy))
            {
                required_servers.push(codex_mcp::CODEX_APPS_MCP_SERVER_NAME.to_owned());
            }
            let (mcp, prepared_recommendations) = tokio::join!(
                // MCP refresh can be large; keep it off the sampling request's stack.
                Box::pin(self.mcp_runtime_for_step(
                    turn_context.as_ref(),
                    &selected_capability_roots,
                    &required_servers,
                    required_plugins,
                )),
                turn::prepare_tool_recommendations(self.as_ref(), turn_context.as_ref()),
            );
            let search = crate::model_search::resolve(
                turn_context.as_ref(),
                &settings,
                &mcp,
                &self.services,
            )
            .await;
            extension_data.insert(match &search {
                Some(crate::model_search::ModelSearch::Native { mode, .. }) => *mode,
                _ => turn_context.config.web_search_mode.value(),
            });
            let mcp = if let Some(search) = search {
                let binding = match &search {
                    crate::model_search::ModelSearch::Hosted { binding, .. } => Arc::clone(binding),
                    _ => mcp,
                };
                extension_data.insert(search);
                binding
            } else {
                mcp
            };
            extension_data.insert(selected_capability_roots);
            // A step keeps the plugins from the environments it captured, even if shared MCP
            // moves on to another environment. Its skill tools and the model use this same copy.
            // Policy follows this step's MCP binding, which may have refreshed mid-turn.
            let selected_plugins = self
                .services
                .mcp_manager
                .selected_plugins_for_step(
                    codex_extension_api::McpServerContributionContext::for_step(
                        turn_context.config.as_ref(),
                        &self.services.mcp_thread_init,
                        &self.services.thread_extension_data,
                        &turn_context.originator,
                        &ready_selected_capability_roots,
                        executor_capability_discovery.as_deref(),
                        executor_capability_discovery_v2.as_deref(),
                    )
                    .with_session_source(&turn_context.session_source)
                    .with_selected_environments(&environments.all_selections()),
                    &mcp.config().plugins,
                    &turn_context.disabled_plugin_ids,
                )
                .await;
            let mut filesystem_access = HashMap::new();
            for environment in environments.turn_environments() {
                let environment_id = &environment.selection.environment_id;
                let grants = turn_context.granted_permissions(environment_id);
                filesystem_access.insert(environment_id, environment.fs_accessor(grants));
            }
            let accessed_roots = ready_selected_capability_roots
                .iter()
                .filter_map(|root| {
                    let codex_protocol::capabilities::CapabilityRootLocation::Environment {
                        environment_id,
                        ..
                    } = &root.location;
                    Some(codex_extension_api::AccessedSelectedCapabilityRoot {
                        root,
                        fs: filesystem_access.get(environment_id)?,
                    })
                })
                .collect::<Vec<_>>();
            for contributor in self.services.extensions.tool_contributors() {
                contributor
                    .prepare_for_step(codex_extension_api::StepPreparationInput {
                        turn_id: &turn_context.sub_id,
                        ready_selected_capability_roots: &accessed_roots,
                        executor_capability_discovery: executor_capability_discovery.as_deref(),
                        executor_capability_discovery_v2: executor_capability_discovery_v2
                            .as_deref(),
                        selected_plugins: &selected_plugins,
                        session_store: &self.services.session_extension_data,
                        thread_store: &self.services.thread_extension_data,
                        turn_store: turn_context.extension_data.as_ref(),
                        step_store: &extension_data,
                    })
                    .or_cancel(cancellation_token)
                    .await?;
            }
            let executor_hook_sources = crate::hook_runtime::prepare_executor_hook_sources(
                &mcp,
                &selected_plugins,
                executor_capability_discovery.as_deref(),
                executor_capability_discovery_v2.as_deref(),
            );
            extension_data.insert(selected_plugins);
            let tool_router = turn::built_tools(
                self.as_ref(),
                turn_context.as_ref(),
                &settings.model_info,
                &environments,
                &mcp,
                &extension_data,
                prepared_recommendations,
                &dynamic_tools,
            )
            .await?;
            Ok::<_, CodexErr>((executor_hook_sources, mcp, tool_router, extension_data))
        });
        // Returned warnings must finish delivery even if tools fail or preparation is cancelled.
        let (loaded_agents_md, prepared_tools) =
            tokio::join!(load_agents_md, prepare_tools.or_cancel(cancellation_token));
        let loaded_agents_md = loaded_agents_md?;
        if cancellation_token.is_cancelled() {
            return Err(CodexErr::TurnAborted);
        }
        let (executor_hook_sources, mcp, tool_router, extension_data) = prepared_tools??;
        Ok(Arc::new(StepContext {
            sampled_through_sequence: None,
            serving_assessment: None,
            preempt: turn_context
                .config
                .features
                .enabled(Feature::InstantInterrupt)
                .then(CancellationToken::new),
            realtime: self.conversation.snapshot().await,
            settings,
            token_budget,
            session_telemetry,
            turn: turn_context,
            extension_data: Arc::new(extension_data),
            environments,
            inherited_environments,
            executor_hook_sources,
            mcp,
            tool_router,
            loaded_agents_md,
        }))
    }

    pub(super) async fn build_turn_context_contribution_items(
        &self,
        step_context: &StepContext,
    ) -> Vec<ResponseItem> {
        let turn_context = step_context.turn.as_ref();
        let mut developer_sections = Vec::new();
        let context_contributors = self.services.extensions.context_contributors().to_vec();

        for contributor in &context_contributors {
            for fragment in contributor
                .contribute_turn_context(TurnContextContributionInput {
                    thread_id: self.thread_id(),
                    turn_id: turn_context.sub_id.as_str(),
                    session_store: &self.services.session_extension_data,
                    thread_store: &self.services.thread_extension_data,
                    turn_store: turn_context.extension_data.as_ref(),
                    model_context_window: step_context.settings.model_info.usable_context_window(),
                })
                .await
            {
                developer_sections.push(fragment.into());
            }
        }

        crate::context_manager::updates::build_rendered_message(developer_sections)
            .into_iter()
            .collect()
    }

    /// Normally both inputs come from the same step. Async compaction uses the prefix's
    /// world state so later state updates can stay in history unchanged.
    /// Returns rendered items with placement preserved for the caller's history layout.
    pub(crate) async fn build_initial_context_with_world_state(
        &self,
        step_context: &StepContext,
        world_state: &WorldState,
    ) -> (Vec<WorldStateUpdate>, WorldStateSnapshot) {
        let rendered = world_state.render_full_with_host_roster().await;
        self.assemble_initial_context(step_context, rendered).await
    }

    pub(crate) async fn assemble_initial_context(
        &self,
        step_context: &StepContext,
        (snapshot, updates): (WorldStateSnapshot, Vec<WorldStateUpdate>),
    ) -> (Vec<WorldStateUpdate>, WorldStateSnapshot) {
        let turn_context = step_context.turn.as_ref();
        let mut developer_sections = Vec::<RenderedFragment>::with_capacity(8);
        let mut contextual_user_sections = Vec::<RenderedFragment>::with_capacity(2);
        let mut separate_developer_messages = Vec::<ResponseItem>::new();
        let mut context_window_hints = Vec::new();
        let (session_source, auto_compact_window_ids) = {
            let state = self.state.lock().await;
            (
                state.session_configuration.session_source.clone(),
                state.auto_compact_window_ids().clone(),
            )
        };
        let separate_guardian_developer_message =
            crate::guardian::is_basic_session_source(&session_source);
        // Keep the guardian policy prompt out of the aggregated developer bundle so it
        // stays isolated as its own top-level developer message for guardian subagents.
        if !separate_guardian_developer_message
            && let Some(developer_instructions) = turn_context.developer_instructions.as_deref()
            && !developer_instructions.is_empty()
        {
            developer_sections
                .push(DeveloperInstructions::new(developer_instructions).render_fragment());
        }
        let loaded_plugins = self
            .services
            .plugins_manager
            .plugins_for_config(&turn_context.config.plugins_config_input())
            .await
            .without_plugins(&turn_context.disabled_plugin_ids);
        let recommended_plugin_candidates = if turn_context
            .config
            .features
            .plugin_recommendations_enabled()
        {
            let auth = self.services.auth_manager.auth().await;
            let plugins_config = turn_context.config.plugins_config_input();
            self.services
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
        let context_contributors = self.services.extensions.context_contributors().to_vec();
        for contributor in &context_contributors {
            for fragment in contributor
                .contribute_thread_context(
                    &self.services.session_extension_data,
                    &self.services.thread_extension_data,
                )
                .await
            {
                match fragment.slot() {
                    PromptSlot::ContextWindow => {
                        context_window_hints.push(fragment.into_content());
                    }
                    PromptSlot::DeveloperPolicy | PromptSlot::DeveloperCapabilities => {
                        developer_sections.push(fragment.into());
                    }
                }
            }
        }
        for contributor in &context_contributors {
            for fragment in contributor
                .contribute_turn_context(TurnContextContributionInput {
                    thread_id: self.thread_id(),
                    turn_id: turn_context.sub_id.as_str(),
                    session_store: &self.services.session_extension_data,
                    thread_store: &self.services.thread_extension_data,
                    turn_store: turn_context.extension_data.as_ref(),
                    model_context_window: step_context.settings.model_info.usable_context_window(),
                })
                .await
            {
                developer_sections.push(fragment.into());
            }
        }
        // This is full-context metadata. Steady-state context diffs should not re-emit it.
        if turn_context.config.features.enabled(Feature::TokenBudget)
            && step_context
                .settings
                .model_info
                .resolved_context_window()
                .is_some()
        {
            // Keep the legacy bridge hint when native Notes is disabled. A failed
            // native request must not fall back to the bridge.
            if !turn_context
                .config
                .token_budget
                .as_ref()
                .is_some_and(|config| config.use_history_notes_extension)
                && let Some((text, source_tool_namespace)) = self
                    .services
                    .mcp_runtime
                    .latest_call_tool(
                        "notes",
                        "thread_hint",
                        /*environment_id*/ None,
                        /*arguments*/ None,
                        Some(serde_json::json!({
                            "threadId": self.thread_id().to_string(),
                        })),
                        /*requested_timeout*/ None,
                        /*wait_for_server*/ true,
                    )
                    .await
                    .ok()
                    .and_then(|call| {
                        let text = call
                            .result
                            .content
                            .iter()
                            .filter_map(|content| {
                                content.get("text").and_then(serde_json::Value::as_str)
                            })
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n");
                        (!text.is_empty()).then_some((text, call.source_tool_namespace))
                    })
            {
                context_window_hints.push(codex_context_fragments::AnnotatedContent::text(
                    text,
                    ContentItemKind("notes.thread_hint".to_string()),
                    ContentItemMetadata::tool(source_tool_namespace.map(Into::into)),
                ));
            }
            separate_developer_messages.push(
                crate::context::TokenBudgetContext::new(
                    session_source
                        .get_agent_path()
                        .unwrap_or_else(codex_protocol::AgentPath::root),
                    auto_compact_window_ids.first_window_id,
                    auto_compact_window_ids.previous_window_id,
                    auto_compact_window_ids.window_id,
                )
                .render_with_hints(context_window_hints),
            );
        }
        let (mut prefix, context) = split_prefix_updates(updates);
        let mut items = Vec::with_capacity(4);
        let mut context = context.into_iter();
        // Keep the initial-context grouping within each fragment run, but never across an item.
        loop {
            let mut next_item = None;
            // Render the active mode after the usage hint so it can override that hint.
            let mut initial_multi_agent_mode = None;
            let mut managed_developer_instructions = None;
            for update in context.by_ref() {
                let fragment = match update.content.into_fragment() {
                    Ok(fragment) => fragment,
                    Err(item) => {
                        next_item = Some(*item);
                        break;
                    }
                };
                match fragment.role() {
                    "developer"
                        if fragment.markers().0 == ModelSwitchInstructions::type_markers().0 =>
                    {
                        // New-model instructions must precede the rest of the developer context.
                        developer_sections.insert(0, fragment.render_fragment());
                    }
                    "developer" if fragment.markers().0 == MULTI_AGENT_MODE_OPEN_TAG => {
                        initial_multi_agent_mode = Some(fragment);
                    }
                    "developer"
                        if fragment.markers().0
                            == ManagedDeveloperInstructions::type_markers().0 =>
                    {
                        managed_developer_instructions = Some(fragment);
                    }
                    "developer"
                        if fragment.markers().0 == MultiAgentRoleInstructions::type_markers().0 =>
                    {
                        separate_developer_messages.push(fragment.render_fragment().into());
                    }
                    "developer"
                        if update.placement == Placement::Standalone
                            && fragment.markers().0.is_empty() =>
                    {
                        separate_developer_messages.push(fragment.render_fragment().into());
                    }
                    "developer" => developer_sections.push(fragment.render_fragment()),
                    "user" => contextual_user_sections.push(fragment.render_fragment()),
                    _ => {}
                }
            }

            if next_item.is_none()
                && let Some(recommended_plugins) = recommended_plugin_candidates
                    .as_deref()
                    .and_then(|plugins| {
                        RecommendedPluginsInstructions::from_plugins(
                            plugins,
                            &ResolvedModelMessages::from_model(&step_context.settings.model_info)
                                .plugin_recommendations(),
                        )
                    })
            {
                developer_sections.push(recommended_plugins.render_fragment());
            }

            if let Some(developer_message) = crate::context_manager::updates::build_rendered_message(
                std::mem::take(&mut developer_sections),
            ) {
                items.push(developer_message);
            }
            items.append(&mut separate_developer_messages);
            if let Some(initial_multi_agent_mode) = initial_multi_agent_mode
                && let Some(message) =
                    crate::context_manager::updates::build_rendered_message(vec![
                        initial_multi_agent_mode.render_fragment(),
                    ])
            {
                items.push(message);
            }
            if let Some(contextual_user_message) =
                crate::context_manager::updates::build_rendered_message(std::mem::take(
                    &mut contextual_user_sections,
                ))
            {
                items.push(contextual_user_message);
            }
            // Emit the guardian policy prompt as a separate developer item so the guardian
            // subagent sees a distinct, easy-to-audit instruction block.
            if next_item.is_none()
                && separate_guardian_developer_message
                && let Some(developer_instructions) = turn_context.developer_instructions.as_deref()
                && !developer_instructions.is_empty()
                && let Some(guardian_developer_message) =
                    crate::context_manager::updates::build_rendered_message(vec![
                        GuardianPolicy::new(developer_instructions).render_fragment(),
                    ])
            {
                items.push(guardian_developer_message);
            }
            if let Some(managed_developer_instructions) = managed_developer_instructions
                && let Some(message) =
                    crate::context_manager::updates::build_rendered_message(vec![
                        managed_developer_instructions.render_fragment(),
                    ])
            {
                items.push(message);
            }

            if let Some(item) = next_item {
                items.push(item);
            } else {
                break;
            }
        }
        // New context windows and compaction install these items directly into replacement history.
        for item in prefix.iter_mut().chain(&mut items) {
            item.set_turn_id_if_missing(&turn_context.sub_id);
        }
        let updates = prefix
            .into_iter()
            .map(WorldStateUpdate::prefix_item)
            .chain(items.into_iter().map(|item| WorldStateUpdate {
                placement: Placement::Standalone,
                content: WorldStateUpdateContent::Item(Box::new(item)),
            }))
            .collect();
        (updates, snapshot)
    }

    pub(crate) async fn reference_context_item(&self) -> Option<TurnContextItem> {
        let state = self.state.lock().await;
        state.reference_context_item()
    }

    /// Persist the latest turn context snapshot for the first real user turn and for
    /// steady-state turns that emit model-visible context updates.
    ///
    /// When the reference snapshot is missing, this injects full initial context. Otherwise, it
    /// emits only context diffs.
    ///
    /// If full context is injected and a model switch occurred, this prepends the
    /// `<model_switch>` developer message so model-specific instructions are not lost.
    ///
    /// This is the normal runtime path that establishes a new `reference_context_item`.
    /// Compaction also establishes that reference when it installs full initial context
    /// into replacement history. Live world-state changes may
    /// independently advance their in-memory baseline within a turn.
    #[instrument(level = "trace", skip_all)]
    pub(crate) async fn record_context_updates_and_set_reference_context_item(
        &self,
        step_context: &StepContext,
    ) -> CodexResult<Arc<WorldState>> {
        let turn_context = step_context.turn.as_ref();
        let reference_context_item = {
            let state = self.state.lock().await;
            state.reference_context_item()
        };
        let turn_context_item = step_context.to_turn_context_item();
        let turn_context_changed = reference_context_item.as_ref() != Some(&turn_context_item);
        let should_inject_full_context = reference_context_item.is_none();
        let world_state = self
            .build_world_state_for_step(step_context, should_inject_full_context)
            .await?;
        // Full initial context resets the baseline; later turns persist only its changes.
        let (mut context_items, world_state_item) = if should_inject_full_context {
            let (context_updates, snapshot) = self
                .build_initial_context_with_world_state(step_context, &world_state)
                .await;
            let context_items =
                crate::context_manager::updates::merge_world_state_updates(context_updates);
            self.state
                .lock()
                .await
                .history
                .set_world_state_baseline(snapshot.clone());
            (
                context_items,
                Some(WorldStateItem::full(snapshot.into_object())),
            )
        } else {
            let (snapshot, mut fragments, world_state_item) = self
                .state
                .lock()
                .await
                .history
                .render_step_world_state(&world_state);
            world_state.resolve_host_roster(&mut fragments).await;
            let items = crate::context_manager::updates::merge_world_state_updates(fragments);
            self.state
                .lock()
                .await
                .history
                .set_world_state_baseline(snapshot);
            (items, world_state_item)
        };
        if !should_inject_full_context && turn_context_changed {
            context_items.extend(
                self.build_turn_context_contribution_items(step_context)
                    .await,
            );
        }
        // A snapshot can change without producing model-visible or TurnContext updates.
        let only_world_state_changed = !turn_context_changed && context_items.is_empty();
        if only_world_state_changed && world_state_item.is_none() {
            return Ok(Arc::new(world_state));
        }
        if !context_items.is_empty() {
            self.record_conversation_items(
                turn_context,
                &step_context.settings.model_info,
                &context_items,
            )
            .await;
        }
        // Persist state only after any model-visible context generated from it.
        if let Some(world_state_item) = world_state_item {
            self.persist_rollout_items(&[RolloutItem::WorldState(world_state_item)])
                .await;
        }
        // A snapshot-only change does not require a duplicate TurnContext record.
        if only_world_state_changed {
            return Ok(Arc::new(world_state));
        }
        // Persist one `TurnContextItem` per real user turn so resume/lazy replay can recover the
        // latest durable baseline even when this turn emitted no model-visible context diffs.
        // Checkpoint current thread-owned settings after the first turn's context:
        // the step may predate an acknowledged update or use executor-owned permissions.
        // Serialize with settings updates so an older checkpoint cannot supersede a newer one.
        let _settings_guard = if should_inject_full_context {
            Some(thread_settings::acquire_persistence_lock(self).await)
        } else {
            None
        };
        let mut rollout_items = vec![RolloutItem::TurnContext(turn_context_item.clone())];
        if should_inject_full_context {
            rollout_items.push(RolloutItem::EventMsg(
                thread_settings::applied_event(self).await,
            ));
        }
        self.persist_rollout_items(&rollout_items).await;

        // Advance the persisted-settings baseline even when this turn emitted no model-visible
        // context items.
        let mut state = self.state.lock().await;
        state.set_reference_context_item(Some(turn_context_item));
        Ok(Arc::new(world_state))
    }
}
