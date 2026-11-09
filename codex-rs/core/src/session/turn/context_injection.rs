//! Skill, plugin, app, and extension context for a turn, plus MCP servers its mentions need.

use super::*;

pub(super) async fn required_mcp_servers_for_input(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    user_input: &[UserInput],
) -> (Vec<String>, Vec<crate::plugins::PluginCapabilitySummary>) {
    if crate::guardian::is_basic_session_source(&turn_context.session_source) {
        return (Vec::new(), Vec::new());
    }

    // Plugin capabilities depend on authentication, so project them only after
    // the runtime has aligned the plugin manager with its current account.
    sess.refresh_mcp_if_dirty().await;
    let loaded_plugins = sess
        .services
        .plugins_manager
        .plugins_for_config(&turn_context.config.plugins_config_input())
        .await
        .without_plugins(&turn_context.disabled_plugin_ids);
    let current_config = sess.services.mcp_runtime.current_config();
    let mentioned_plugins =
        collect_explicit_plugin_mentions(user_input, loaded_plugins.capability_summaries());
    let mut required_servers = mentioned_plugins
        .iter()
        .flat_map(|plugin| plugin.mcp_server_names.iter().cloned())
        .collect::<HashSet<_>>();

    let messages = user_input
        .iter()
        .filter_map(|input| match input {
            UserInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mentions = collect_tool_mentions_from_messages(&messages);
    let paths = user_input
        .iter()
        .filter_map(|input| match input {
            UserInput::Mention { path, .. } => Some(path.clone()),
            _ => None,
        })
        .chain(mentions.paths);
    required_servers.extend(paths.filter_map(|path| {
        path.strip_prefix("mcp://")
            .filter(|server| !server.is_empty())
            .map(str::to_string)
    }));

    let connector_slug_counts = if turn_context.apps_enabled() && !mentions.plain_names.is_empty() {
        let cached_connectors = connectors::list_cached_accessible_connectors_from_mcp_tools(
            &turn_context.config,
            sess.services.auth_manager.auth().await.as_ref(),
        );
        let accessible_connectors = match cached_connectors {
            Some(connectors) => connectors,
            None => sess
                .services
                .mcp_runtime
                .current_binding()
                .await
                .map(|binding| connectors::accessible_connectors_from_mcp_tools(binding.tools()))
                .unwrap_or_default(),
        };
        let connector_ids = current_config
            .iter()
            .flat_map(|config| config.connector_snapshot.connector_ids())
            .map(|connector_id| connector_id.0.clone());
        build_connector_slug_counts(
            &codex_connectors::merge::merge_plugin_connectors_with_accessible(
                connector_ids,
                accessible_connectors,
            ),
        )
    } else {
        HashMap::new()
    };
    let environments = sess.services.turn_environments.snapshot().await;
    let skills_snapshot = sess.host_skills_for_step(turn_context, &environments).await;
    let skills_outcome = skills_snapshot.outcome();
    let mentioned_skills =
        collect_explicit_skill_mentions(user_input, skills_outcome, &connector_slug_counts);
    for skill in mentioned_skills {
        if let Some(dependencies) = skill.dependencies {
            required_servers.extend(
                dependencies
                    .tools
                    .into_iter()
                    .filter(|tool| tool.r#type.eq_ignore_ascii_case("mcp"))
                    .map(|tool| tool.value),
            );
        }
        if let Some(plugin_id) = skill.plugin_id.as_deref()
            && let Some(plugin) = loaded_plugins
                .capability_summaries()
                .iter()
                .find(|plugin| plugin.config_name == plugin_id)
        {
            required_servers.extend(plugin.mcp_server_names.iter().cloned());
        }
    }

    (required_servers.into_iter().collect(), mentioned_plugins)
}

#[instrument(level = "trace", skip_all)]
pub(super) async fn build_skills_and_plugins(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    mentioned_plugins: &[crate::plugins::PluginCapabilitySummary],
    cancellation_token: &CancellationToken,
) -> Option<(Vec<ResponseItem>, HashSet<String>)> {
    let turn_context = step_context.turn.as_ref();
    // Guardian input embeds the parent transcript as untrusted evidence. Do not interpret skill or
    // plugin mentions from that generated prompt as requests to inject additional instructions.
    if crate::guardian::is_basic_session_source(&turn_context.session_source) {
        return Some((Vec::new(), HashSet::new()));
    }

    let tracking = build_track_events_context(
        turn_context.model_info().slug.clone(),
        sess.thread_id.to_string(),
        turn_context.sub_id.clone(),
        turn_context.originator.clone(),
        Some(turn_context.turn_metadata_state.clone()),
    );
    let connector_snapshot = step_context.mcp.config().connector_snapshot.clone();
    let mcp_tools = if turn_context.apps_enabled() || !mentioned_plugins.is_empty() {
        // Plugin mentions need raw MCP/app inventory even when app tools
        // are normally hidden so we can describe the plugin's currently
        // usable capabilities for this turn.
        step_context.mcp.tools()
    } else {
        &[]
    };
    let available_connectors = if turn_context.apps_enabled() {
        let connectors = codex_connectors::merge::merge_plugin_connectors_with_accessible(
            connector_snapshot
                .connector_ids()
                .iter()
                .map(|connector_id| connector_id.0.clone()),
            connectors::accessible_connectors_from_mcp_tools(mcp_tools),
        );
        AppToolPolicyEvaluator::new(&turn_context.config.config_layer_stack)
            .apply_app_enabled_state(connectors)
    } else {
        Vec::new()
    };
    let skills_snapshot =
        HostSkillsSnapshot::for_step(&step_context.extension_data, &turn_context.extension_data)?;
    let skills_outcome = skills_snapshot.outcome();
    let connector_slug_counts = build_connector_slug_counts(&available_connectors);
    let extension_injection_items =
        build_extension_turn_input_items(sess, step_context, user_input, cancellation_token)
            .await?;
    let skill_name_counts_lower =
        build_skill_name_counts(&skills_outcome.skills, &skills_outcome.disabled_paths).1;
    let mentioned_skills =
        collect_explicit_skill_mentions(user_input, skills_outcome, &connector_slug_counts);
    maybe_prompt_and_install_mcp_dependencies(
        sess,
        turn_context,
        cancellation_token,
        &mentioned_skills,
        Some(sess.mcp_elicitation_reviewer()),
    )
    .await;

    let injected_host_skill_prompts = turn_context
        .extension_data
        .get::<InjectedHostSkillPrompts>();
    let environments = step_context.environments();
    let accessors = environments
        .iter()
        .map(|(_, access)| access as &dyn EnvironmentAccess)
        .collect::<Vec<_>>();
    let HostSkillPrompts {
        fragments,
        injected: injected_host_skills,
        warnings: host_skill_warnings,
    } = skills_snapshot
        .load_skill_prompts(&mentioned_skills, &accessors)
        .await;
    emit_explicit_skill_invocations(
        sess,
        turn_context,
        &mentioned_skills,
        &injected_host_skills,
        tracking.clone(),
    )
    .await;
    for message in host_skill_warnings {
        sess.send_event(turn_context, EventMsg::Warning(WarningEvent { message }))
            .await;
    }
    let skill_items = fragments
        .into_iter()
        .map(ContextualUserFragment::into_boxed_response_item)
        .collect::<Vec<_>>();
    let skill_connector_ids = collect_explicit_app_ids_from_skill_items(
        &skill_items,
        &available_connectors,
        &skill_name_counts_lower,
    );
    let plugin_items = build_plugin_injections(
        mentioned_plugins,
        mcp_tools,
        &available_connectors,
        &turn_context.config.features,
        codex_prompts::ResolvedModelMessages::from_model(&step_context.settings.model_info),
    );
    let mut explicitly_enabled_connectors = collect_explicit_app_ids(user_input);
    explicitly_enabled_connectors.extend(skill_connector_ids);
    let connector_names_by_id = available_connectors
        .iter()
        .map(|connector| (connector.id.as_str(), connector.name.as_str()))
        .collect::<HashMap<&str, &str>>();
    let mentioned_app_invocations = explicitly_enabled_connectors
        .iter()
        .map(|connector_id| AppInvocation {
            connector_id: Some(connector_id.clone()),
            app_name: connector_names_by_id
                .get(connector_id.as_str())
                .map(|name| (*name).to_string()),
            invocation_type: Some(InvocationType::Explicit),
        })
        .collect::<Vec<_>>();
    sess.services
        .analytics_events_client
        .track_app_mentioned(tracking.clone(), mentioned_app_invocations);
    for summary in mentioned_plugins {
        if let Some(plugin) = sess
            .services
            .plugins_manager
            .telemetry_metadata_for_capability_summary(summary)
        {
            sess.services
                .analytics_events_client
                .track_plugin_used(tracking.clone(), plugin);
        }
    }

    let mut injection_items = match injected_host_skill_prompts {
        Some(injected_host_skill_prompts) => skill_items
            .into_iter()
            .zip(injected_host_skills.iter())
            .filter_map(|(item, skill)| {
                (!injected_host_skill_prompts
                    .contains_path(&skill.path_to_skills_md.inferred_native_path_string()))
                .then_some(item)
            })
            .collect(),
        None => skill_items,
    };
    injection_items.extend(plugin_items);
    injection_items.extend(extension_injection_items);
    Some((injection_items, explicitly_enabled_connectors))
}

#[tracing::instrument(
    level = "trace",
    skip_all,
    fields(user_input_count = user_input.len())
)]
async fn build_extension_turn_input_items(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    cancellation_token: &CancellationToken,
) -> Option<Vec<ResponseItem>> {
    let turn_context = step_context.turn.as_ref();
    let contributors = sess.services.extensions.turn_input_contributors().to_vec();
    if contributors.is_empty() {
        return Some(Vec::new());
    }

    let environment_accessors = step_context.environments();
    let environments = environment_accessors
        .iter()
        .enumerate()
        .map(|(index, (environment, fs))| TurnInputEnvironment {
            environment_id: environment.selection.environment_id.clone(),
            cwd: environment.cwd().clone(),
            is_primary: index == 0,
            fs,
        })
        .collect::<Vec<_>>();

    let input = TurnInputContext {
        step_store: &step_context.extension_data,
        turn_id: turn_context.sub_id.to_string(),
        user_input: user_input.to_vec(),
        environments,
    };
    let extension_metrics =
        super::extension_metrics::from_session_telemetry(turn_context.session_telemetry.clone());

    let mut items = Vec::new();
    for contributor in contributors {
        let contributed_fragments = contributor
            .contribute(
                input.clone(),
                Some(Arc::clone(&extension_metrics)),
                &sess.services.session_extension_data,
                &sess.services.thread_extension_data,
                turn_context.extension_data.as_ref(),
            )
            .or_cancel(cancellation_token)
            .await
            .ok()?;
        items.extend(
            contributed_fragments
                .into_iter()
                .map(ContextualUserFragment::into_boxed_response_item),
        );
    }

    Some(items)
}

pub(in crate::session) fn collect_explicit_app_ids_from_skill_items(
    skill_items: &[ResponseItem],
    connectors: &[connectors::AppInfo],
    skill_name_counts_lower: &HashMap<String, usize>,
) -> HashSet<String> {
    if skill_items.is_empty() || connectors.is_empty() {
        return HashSet::new();
    }

    let skill_messages = skill_items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { content, .. } => {
                content.iter().find_map(|content_item| match content_item {
                    ContentItem::InputText { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<String>>();
    if skill_messages.is_empty() {
        return HashSet::new();
    }

    let mentions = collect_tool_mentions_from_messages(&skill_messages);
    let mention_names_lower = mentions
        .plain_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<String>>();
    let mut connector_ids = mentions
        .paths
        .iter()
        .filter(|path| tool_kind_for_path(path) == ToolMentionKind::App)
        .filter_map(|path| app_id_from_path(path).map(str::to_string))
        .collect::<HashSet<String>>();

    let connector_slug_counts = build_connector_slug_counts(connectors);
    for connector in connectors {
        let slug = codex_connectors::metadata::connector_mention_slug(connector);
        let connector_count = connector_slug_counts.get(&slug).copied().unwrap_or(0);
        let skill_count = skill_name_counts_lower.get(&slug).copied().unwrap_or(0);
        if connector_count == 1 && skill_count == 0 && mention_names_lower.contains(&slug) {
            connector_ids.insert(connector.id.clone());
        }
    }

    connector_ids
}
