//! Resolution and validation of feature-scoped sub-configs (code mode, multi-agent, budgets).

use super::*;

impl TokenBudgetConfig {
    pub(crate) fn validate(&self) -> std::io::Result<()> {
        if self
            .reminder_threshold_tokens
            .is_some_and(|tokens| tokens <= 0)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.token_budget.reminder_threshold_tokens must be positive",
            ));
        }

        if self.reminder_message_template.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.token_budget.reminder_message_template must not be empty",
            ));
        }
        if self.reminder_message_template.len() > TOKEN_BUDGET_REMINDER_MESSAGE_TEMPLATE_MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "features.token_budget.reminder_message_template must not exceed {TOKEN_BUDGET_REMINDER_MESSAGE_TEMPLATE_MAX_BYTES} bytes"
                ),
            ));
        }

        if self
            .guidance_message
            .as_ref()
            .is_some_and(|message| message.len() > TOKEN_BUDGET_GUIDANCE_MESSAGE_MAX_BYTES)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "features.token_budget.guidance_message must not exceed {TOKEN_BUDGET_GUIDANCE_MESSAGE_MAX_BYTES} bytes"
                ),
            ));
        }

        if self
            .auto_compact_fallback_prompt
            .as_ref()
            .is_some_and(|prompt| prompt.len() > AUTO_COMPACT_FALLBACK_PROMPT_MAX_BYTES)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "features.token_budget.auto_compact_fallback_prompt must not exceed {AUTO_COMPACT_FALLBACK_PROMPT_MAX_BYTES} bytes"
                ),
            ));
        }
        if self.auto_compact_fallback_prompt.is_some()
            && self.auto_compact_fallback_buffer_tokens.is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.token_budget.auto_compact_fallback_buffer_tokens is required when auto_compact_fallback_prompt is set",
            ));
        }
        if self
            .auto_compact_fallback_buffer_tokens
            .is_some_and(|tokens| tokens <= 0)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "features.token_budget.auto_compact_fallback_buffer_tokens must be positive",
            ));
        }

        Ok(())
    }

    pub(crate) fn fallback_buffer_tokens(&self) -> i64 {
        if self.auto_compact_fallback_prompt.is_some() {
            self.auto_compact_fallback_buffer_tokens.unwrap_or(0)
        } else {
            0
        }
    }
}

impl MultiAgentV2Config {
    pub(super) fn defaults_for_max_concurrency(max_concurrent_threads_per_session: usize) -> Self {
        Self {
            max_concurrent_threads_per_session,
            min_wait_timeout_ms: DEFAULT_MULTI_AGENT_V2_MIN_WAIT_TIMEOUT_MS,
            max_wait_timeout_ms: DEFAULT_MULTI_AGENT_V2_MAX_WAIT_TIMEOUT_MS,
            default_wait_timeout_ms: DEFAULT_MULTI_AGENT_V2_DEFAULT_WAIT_TIMEOUT_MS,
            usage_hint_text: None,
            root_agent_usage_hint_text: None,
            subagent_usage_hint_text: None,
            subagent_developer_instructions: None,
            preserve_fork_prefix: true,
            multi_agent_mode_hint_text: None,
            tool_namespace: Some(DEFAULT_MULTI_AGENT_V2_TOOL_NAMESPACE.to_string()),
            hide_spawn_agent_metadata: true,
            expose_spawn_agent_model_overrides: true,
            wait_agent_enabled: true,
            disable_native_tools: false,
            disable_direct_message: false,
            message_board_in_memory: false,
            message_board_remote: None,
            non_code_mode_only: true,
        }
    }
}

pub(super) fn resolve_code_mode_config(config_toml: &ConfigToml) -> CodeModeConfig {
    let base = code_mode_toml_config(config_toml.features.as_ref());
    let host = config_toml
        .features
        .as_ref()
        .and_then(|features| features.code_mode_host.as_ref())
        .and_then(|feature| match feature {
            FeatureToml::Enabled(_) => None,
            FeatureToml::Config(config) => Some(config),
        });

    CodeModeConfig {
        default_exec_yield_time_ms: base
            .and_then(|config| config.default_exec_yield_time_ms)
            .unwrap_or(DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS),
        experimental_show_cell_overhead: base
            .and_then(|config| config.experimental_show_cell_overhead)
            .unwrap_or_default(),
        tool_input_schema_max_bytes: base
            .and_then(|config| config.tool_input_schema_max_bytes)
            .map(NonZeroUsize::get),
        excluded_tool_namespaces: base
            .and_then(|config| config.excluded_tool_namespaces.as_ref())
            .cloned()
            .unwrap_or_default(),
        direct_only_tool_namespaces: base
            .and_then(|config| config.direct_only_tool_namespaces.as_ref())
            .cloned()
            .unwrap_or_default(),
        disable_in_process_fallback: host
            .and_then(|config| config.disable_in_process_fallback)
            .unwrap_or_default(),
    }
}

pub(super) fn resolve_multi_agent_v2_config(config_toml: &ConfigToml) -> MultiAgentV2Config {
    let base = multi_agent_v2_toml_config(config_toml.features.as_ref());
    let max_concurrent_threads_per_session = base
        .and_then(|config| config.max_concurrent_threads_per_session)
        .or_else(|| {
            config_toml
                .agents
                .as_ref()
                .and_then(|agents| agents.max_concurrent_threads_per_session)
                .map(|max_threads| max_threads.saturating_add(1))
        })
        .unwrap_or(DEFAULT_MULTI_AGENT_V2_MAX_CONCURRENT_THREADS_PER_SESSION);
    let default =
        MultiAgentV2Config::defaults_for_max_concurrency(max_concurrent_threads_per_session);
    let min_wait_timeout_ms = base
        .and_then(|config| config.min_wait_timeout_ms)
        .unwrap_or(default.min_wait_timeout_ms);
    let max_wait_timeout_ms = base
        .and_then(|config| config.max_wait_timeout_ms)
        .unwrap_or(default.max_wait_timeout_ms);
    let default_wait_timeout_ms = base
        .and_then(|config| config.default_wait_timeout_ms)
        .unwrap_or(default.default_wait_timeout_ms);
    let usage_hint_text = base
        .and_then(|config| config.usage_hint_text.as_ref())
        .cloned()
        .or(default.usage_hint_text);
    let hide_spawn_agent_metadata = base
        .and_then(|config| config.hide_spawn_agent_metadata)
        .unwrap_or(default.hide_spawn_agent_metadata);
    let expose_spawn_agent_model_overrides = base
        .and_then(|config| config.expose_spawn_agent_model_overrides)
        .unwrap_or(default.expose_spawn_agent_model_overrides);
    let root_agent_usage_hint_text = base
        .and_then(|config| config.root_agent_usage_hint_text.as_ref())
        .cloned();
    let subagent_usage_hint_text = base
        .and_then(|config| config.subagent_usage_hint_text.as_ref())
        .cloned();
    let wait_agent_enabled = base
        .and_then(|config| config.wait_agent_enabled)
        .unwrap_or(default.wait_agent_enabled);
    let disable_native_tools = base
        .and_then(|config| config.disable_native_tools)
        .unwrap_or(default.disable_native_tools);
    let disable_direct_message = base
        .and_then(|config| config.disable_direct_message)
        .unwrap_or(default.disable_direct_message);
    let message_board_in_memory = base
        .and_then(|config| config.message_board_in_memory)
        .unwrap_or(default.message_board_in_memory);
    let subagent_developer_instructions = base
        .and_then(|config| config.subagent_developer_instructions.as_ref())
        .map(|instructions| instructions.trim().to_string());
    let preserve_fork_prefix = base
        .and_then(|config| config.preserve_fork_prefix)
        .unwrap_or(default.preserve_fork_prefix);
    let multi_agent_mode_hint_text = base
        .and_then(|config| config.multi_agent_mode_hint_text.as_ref())
        .cloned()
        .or(default.multi_agent_mode_hint_text);
    let tool_namespace = base
        .and_then(|config| config.tool_namespace.as_ref())
        .cloned()
        .or(default.tool_namespace);
    let non_code_mode_only = base
        .and_then(|config| config.non_code_mode_only)
        .unwrap_or(default.non_code_mode_only);

    MultiAgentV2Config {
        max_concurrent_threads_per_session,
        min_wait_timeout_ms,
        max_wait_timeout_ms,
        default_wait_timeout_ms,
        usage_hint_text,
        root_agent_usage_hint_text,
        subagent_usage_hint_text,
        subagent_developer_instructions,
        preserve_fork_prefix,
        multi_agent_mode_hint_text,
        tool_namespace,
        hide_spawn_agent_metadata,
        expose_spawn_agent_model_overrides,
        wait_agent_enabled,
        disable_native_tools,
        disable_direct_message,
        message_board_in_memory,
        message_board_remote: base.and_then(|config| config.message_board_remote.clone()),
        non_code_mode_only,
    }
}

pub(crate) fn resolve_token_budget_config(
    config_toml: &ConfigToml,
    features: &ManagedFeatures,
) -> std::io::Result<Option<TokenBudgetConfig>> {
    if !features.enabled(Feature::TokenBudget) {
        return Ok(None);
    }

    let token_budget_config = token_budget_toml_config(config_toml.features.as_ref());
    let use_history_notes_extension = token_budget_config
        .and_then(|config| config.use_history_notes_extension)
        .unwrap_or_default();
    let reminder_threshold_tokens =
        token_budget_config.and_then(|config| config.reminder_threshold_tokens);
    let reminder_message_template = token_budget_config
        .and_then(|config| config.reminder_message_template.clone())
        .unwrap_or_else(|| {
            ResolvedModelMessages::bundled()
                .token_budget_reminder_template()
                .to_owned()
        });
    let guidance_message = token_budget_config
        .and_then(|config| config.guidance_message.clone())
        .filter(|message| !message.trim().is_empty());
    let auto_compact_fallback_prompt = token_budget_config
        .and_then(|config| config.auto_compact_fallback_prompt.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let auto_compact_fallback_buffer_tokens =
        token_budget_config.and_then(|config| config.auto_compact_fallback_buffer_tokens);

    let token_budget = TokenBudgetConfig {
        use_history_notes_extension,
        reminder_threshold_tokens,
        reminder_message_template,
        guidance_message,
        auto_compact_fallback_prompt,
        auto_compact_fallback_buffer_tokens,
    };
    token_budget.validate()?;
    Ok(Some(token_budget))
}

pub(super) fn resolve_rollout_budget_config(
    config_toml: &ConfigToml,
    features: &ManagedFeatures,
) -> std::io::Result<Option<RolloutBudgetConfig>> {
    if !features.enabled(Feature::RolloutBudget) {
        return Ok(None);
    }
    let missing_limit_error = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.rollout_budget.limit_tokens is required when rollout_budget is enabled",
        )
    };
    let Some(FeatureToml::Config(config)) = config_toml
        .features
        .as_ref()
        .and_then(|features| features.rollout_budget.as_ref())
    else {
        return Err(missing_limit_error());
    };
    let Some(limit_tokens) = config.limit_tokens else {
        return Err(missing_limit_error());
    };
    if limit_tokens <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.rollout_budget.limit_tokens must be positive",
        ));
    }
    let reminder_at_remaining_tokens =
        config
            .reminder_at_remaining_tokens
            .clone()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "features.rollout_budget.reminder_at_remaining_tokens is required when rollout_budget is enabled",
                )
            })?;
    if reminder_at_remaining_tokens
        .iter()
        .any(|&tokens| tokens <= 0 || tokens >= limit_tokens)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "features.rollout_budget.reminder_at_remaining_tokens must contain only positive values below limit_tokens",
        ));
    }
    let sampling_token_weight = config.sampling_token_weight.unwrap_or(1.0);
    let prefill_token_weight = config.prefill_token_weight.unwrap_or(1.0);
    for (field, weight) in [
        ("sampling_token_weight", sampling_token_weight),
        ("prefill_token_weight", prefill_token_weight),
    ] {
        if !weight.is_finite() || weight < 0.0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("features.rollout_budget.{field} must be finite and non-negative"),
            ));
        }
    }
    Ok(Some(RolloutBudgetConfig {
        enforce_limit: config.enforce_limit.unwrap_or(true),
        limit_tokens,
        reminder_at_remaining_tokens,
        sampling_token_weight,
        prefill_token_weight,
    }))
}

pub(super) fn resolve_current_time_reminder_config(
    config_toml: &ConfigToml,
    features: &ManagedFeatures,
) -> std::io::Result<Option<CurrentTimeReminderConfig>> {
    if !features.enabled(Feature::CurrentTimeReminder) {
        return Ok(None);
    }

    let base = current_time_reminder_toml_config(config_toml.features.as_ref());
    let default = CurrentTimeReminderConfig::default();
    let reminder_interval_seconds = base
        .and_then(|config| config.reminder_interval_seconds)
        .unwrap_or(default.reminder_interval_seconds);

    Ok(Some(CurrentTimeReminderConfig {
        reminder_interval_seconds,
        clock_source: base
            .and_then(|config| config.clock_source)
            .unwrap_or(default.clock_source),
        delivery_mode: base
            .and_then(|config| config.delivery_mode)
            .unwrap_or(default.delivery_mode),
        sleep_tool: base
            .and_then(|config| config.sleep_tool)
            .unwrap_or(default.sleep_tool),
    }))
}

fn code_mode_toml_config(features: Option<&FeaturesToml>) -> Option<&CodeModeConfigToml> {
    match features?.code_mode.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn multi_agent_v2_toml_config(features: Option<&FeaturesToml>) -> Option<&MultiAgentV2ConfigToml> {
    match features?.multi_agent_v2.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn token_budget_toml_config(features: Option<&FeaturesToml>) -> Option<&TokenBudgetConfigToml> {
    match features?.token_budget.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

fn current_time_reminder_toml_config(
    features: Option<&FeaturesToml>,
) -> Option<&CurrentTimeReminderConfigToml> {
    match features?.current_time_reminder.as_ref()? {
        FeatureToml::Enabled(_) => None,
        FeatureToml::Config(config) => Some(config),
    }
}

pub(super) fn validate_multi_agent_v2_wait_timeout(label: &str, value: i64) -> std::io::Result<()> {
    if value < HARD_MIN_MULTI_AGENT_V2_TIMEOUT_MS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} must be at least {HARD_MIN_MULTI_AGENT_V2_TIMEOUT_MS}"),
        ));
    }
    if value > HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} must be at most {HARD_MAX_MULTI_AGENT_V2_TIMEOUT_MS}"),
        ));
    }
    Ok(())
}
