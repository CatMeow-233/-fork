use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use codex_config::CONFIG_TOML_FILE;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::HookEventsToml;
use codex_config::HookHandlerConfig;
use codex_config::HookStateToml;
use codex_config::HooksFile;
use codex_config::ManagedHooksRequirementsToml;
use codex_config::MatcherGroup;
use codex_config::RequirementSource;
use codex_config::TomlValue;
use codex_config::version_for_toml;
use codex_plugin::PluginHookSource;
use codex_plugin::is_allowlisted_bundled_cleanup_hook;
use codex_protocol::protocol::HookEventName;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde::Serialize;

use super::ConfiguredHandler;
use super::ConfiguredHandlerKind;
use super::HookListEntry;
use super::HookListEntryHandler;
use super::dispatcher::hook_event_name_label;
use super::plugin_trust::PluginHookTrust;
use crate::config_rules::hook_states_from_stack;
use crate::engine::HookMatcher;
use crate::events::common::matcher_pattern_for_event;
use crate::events::session_end::SESSION_END_DEFAULT_TIMEOUT_SEC;
use crate::events::session_end::SESSION_END_MAX_TIMEOUT_SEC;
use crate::output_spill::AdditionalContextLimit;
use crate::output_spill::DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT;
use codex_protocol::protocol::HookSource;
use codex_protocol::protocol::HookTrustStatus;

#[derive(Default)]
pub(crate) struct DiscoveryResult {
    pub handlers: Vec<ConfiguredHandler>,
    pub hook_entries: Vec<HookListEntry>,
    pub warnings: Vec<String>,
    pub required_load_errors: Vec<String>,
}

pub(crate) fn ensure_required_hooks_loaded(errors: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        errors.is_empty(),
        "failed to load required managed hooks: {}",
        errors.join("; ")
    );
    Ok(())
}

struct HookHandlerSource<'a> {
    path: &'a AbsolutePathBuf,
    key_source: String,
    source: HookSource,
    is_managed: bool,
    requirement: HookRequirement<'a>,
    bypass_hook_trust: bool,
    hook_states: &'a HashMap<String, HookStateToml>,
    env: HashMap<String, String>,
    plugin_id: Option<String>,
    plugin_content_hash: Option<&'a std::io::Result<String>>,
    config_layer_stack: Option<&'a ConfigLayerStack>,
    plugin_root: Option<&'a AbsolutePathBuf>,
}

enum HookRequirement<'a> {
    Required(&'a mut Vec<String>),
    Optional,
}

impl HookHandlerSource<'_> {
    fn record_load_failure(&mut self, warning: String, warnings: &mut Vec<String>) {
        if let HookRequirement::Required(required_load_errors) = &mut self.requirement {
            required_load_errors.push(warning.clone());
        }
        warnings.push(warning);
    }
}

struct NormalizedHandler {
    config: HookHandlerConfig,
    kind: ConfiguredHandlerKind,
    timeout_sec: u64,
    status_message: Option<String>,
    additional_context_limit: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscoveryMode {
    Runtime,
    Listing,
}

#[derive(Clone, Copy)]
struct HookDiscoveryPolicy<'a> {
    config_layer_stack: Option<&'a ConfigLayerStack>,
    allow_managed_hooks_only: bool,
    bypass_hook_trust: bool,
    mode: DiscoveryMode,
}

impl HookDiscoveryPolicy<'_> {
    fn allows(self, source: &HookHandlerSource<'_>) -> bool {
        !self.allow_managed_hooks_only || source.is_managed
    }
}

pub(crate) fn discover_handlers(
    config_layer_stack: Option<&ConfigLayerStack>,
    plugin_hook_sources: Vec<PluginHookSource>,
    plugin_hook_load_warnings: Vec<String>,
    bypass_hook_trust: bool,
    mode: DiscoveryMode,
) -> DiscoveryResult {
    let mut handlers = Vec::new();
    let mut hook_entries = Vec::new();
    let mut warnings = plugin_hook_load_warnings;
    let mut required_load_errors = Vec::new();
    let mut display_order = 0_i64;
    let mut visited_json_hook_folders = HashSet::new();
    let hook_states = hook_states_from_stack(config_layer_stack);
    let policy = HookDiscoveryPolicy {
        config_layer_stack,
        allow_managed_hooks_only: config_layer_stack.is_some_and(|config_layer_stack| {
            config_layer_stack
                .requirements()
                .allow_managed_hooks_only
                .as_ref()
                .is_some_and(|requirement| requirement.value)
        }),
        bypass_hook_trust,
        mode,
    };

    if let Some(config_layer_stack) = config_layer_stack {
        required_load_errors = append_managed_requirement_handlers(
            &mut handlers,
            &mut hook_entries,
            &mut warnings,
            &mut display_order,
            config_layer_stack,
            &hook_states,
            policy,
        );

        for layer in config_layer_stack.layers_low_to_high() {
            let (hook_source, is_managed) = hook_metadata_for_config_layer_source(&layer.name);
            let policy_path = config_toml_source_path(layer);
            let policy_source = HookHandlerSource {
                path: &policy_path,
                key_source: policy_path.display().to_string(),
                source: hook_source,
                is_managed,
                requirement: HookRequirement::Optional,
                bypass_hook_trust: false,
                hook_states: &hook_states,
                env: HashMap::new(),
                plugin_id: None,
                plugin_content_hash: None,
                config_layer_stack: None,
                plugin_root: None,
            };
            if !policy.allows(&policy_source) {
                continue;
            }
            let json_hooks = match layer.hooks_config_folder() {
                Some(config_folder) if visited_json_hook_folders.insert(config_folder.clone()) => {
                    load_hooks_json(Some(config_folder.as_path()), &mut warnings)
                }
                _ => None,
            };
            let toml_hooks = load_toml_hooks_from_layer(layer, &mut warnings);

            if let (Some((json_source_path, json_events)), Some((toml_source_path, toml_events))) =
                (&json_hooks, &toml_hooks)
                && !json_events.is_empty()
                && !toml_events.is_empty()
            {
                warnings.push(format!(
                    "loading hooks from both {} and {}; prefer a single representation for this layer",
                    json_source_path.display(),
                    toml_source_path.display()
                ));
            }

            for (source_path, hook_events) in [json_hooks, toml_hooks].into_iter().flatten() {
                append_hook_events(
                    &mut handlers,
                    &mut hook_entries,
                    &mut warnings,
                    &mut display_order,
                    HookHandlerSource {
                        path: &source_path,
                        key_source: source_path.display().to_string(),
                        source: hook_source,
                        is_managed,
                        requirement: HookRequirement::Optional,
                        bypass_hook_trust: policy.bypass_hook_trust,
                        hook_states: &hook_states,
                        env: HashMap::new(),
                        plugin_id: None,
                        plugin_content_hash: None,
                        config_layer_stack: None,
                        plugin_root: None,
                    },
                    hook_events,
                    policy,
                );
            }
        }
    }

    append_plugin_hook_sources(
        &mut handlers,
        &mut hook_entries,
        &mut warnings,
        &mut display_order,
        plugin_hook_sources,
        &hook_states,
        policy,
    );

    DiscoveryResult {
        handlers,
        hook_entries,
        warnings,
        required_load_errors,
    }
}

fn append_managed_requirement_handlers(
    handlers: &mut Vec<ConfiguredHandler>,
    hook_entries: &mut Vec<HookListEntry>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    config_layer_stack: &ConfigLayerStack,
    hook_states: &HashMap<String, HookStateToml>,
    policy: HookDiscoveryPolicy<'_>,
) -> Vec<String> {
    let Some(managed_hooks) = config_layer_stack.requirements().managed_hooks.as_ref() else {
        return Vec::new();
    };
    let mut required_load_errors = Vec::new();
    let source_path = managed_hooks_source_path(managed_hooks.get(), managed_hooks.source.as_ref());
    append_hook_events(
        handlers,
        hook_entries,
        warnings,
        display_order,
        HookHandlerSource {
            path: &source_path,
            key_source: source_path.display().to_string(),
            source: hook_source_for_requirement_source(managed_hooks.source.as_ref()),
            is_managed: true,
            requirement: HookRequirement::Required(&mut required_load_errors),
            bypass_hook_trust: false,
            hook_states,
            env: HashMap::new(),
            plugin_id: None,
            plugin_content_hash: None,
            config_layer_stack: None,
            plugin_root: None,
        },
        managed_hooks.get().hooks.as_ref().clone(),
        policy,
    );
    required_load_errors
}

fn append_plugin_hook_sources(
    handlers: &mut Vec<ConfiguredHandler>,
    hook_entries: &mut Vec<HookListEntry>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    plugin_hook_sources: Vec<PluginHookSource>,
    hook_states: &HashMap<String, HookStateToml>,
    policy: HookDiscoveryPolicy<'_>,
) {
    if policy.allow_managed_hooks_only {
        return;
    }
    // Listing inspects current contents; runtime discovery defers hashing until execution.
    let mut content_hashes = HashMap::new();
    for source in plugin_hook_sources {
        let PluginHookSource {
            plugin_root,
            plugin_id,
            plugin_data_root,
            source_path,
            source_relative_path,
            hooks,
        } = source;
        let plugin_id = plugin_id.as_key();
        let content_hash = (policy.mode == DiscoveryMode::Listing).then(|| {
            content_hashes
                .entry(plugin_root.clone())
                .or_insert_with(|| {
                    super::plugin_fingerprint::plugin_content_hash(&plugin_root, &|| false)
                })
        });
        let mut env = HashMap::new();
        let plugin_root_value = plugin_root.display().to_string();
        let plugin_data_root_value = plugin_data_root.display().to_string();
        env.insert("PLUGIN_ROOT".to_string(), plugin_root_value.clone());
        // For OOTB compat with existing plugins that use this env var.
        env.insert("CLAUDE_PLUGIN_ROOT".to_string(), plugin_root_value);
        env.insert("PLUGIN_DATA".to_string(), plugin_data_root_value.clone());
        // For OOTB compat with existing plugins that use this env var.
        env.insert("CLAUDE_PLUGIN_DATA".to_string(), plugin_data_root_value);
        append_hook_events(
            handlers,
            hook_entries,
            warnings,
            display_order,
            HookHandlerSource {
                path: &source_path,
                key_source: crate::declarations::plugin_hook_key_source(
                    plugin_id.as_str(),
                    source_relative_path.as_str(),
                ),
                source: HookSource::Plugin,
                is_managed: false,
                requirement: HookRequirement::Optional,
                bypass_hook_trust: policy.bypass_hook_trust,
                hook_states,
                env,
                plugin_id: Some(plugin_id),
                plugin_content_hash: content_hash.as_deref(),
                config_layer_stack: policy.config_layer_stack,
                plugin_root: Some(&plugin_root),
            },
            hooks,
            policy,
        );
    }
}

fn managed_hooks_source_path(
    managed_hooks: &ManagedHooksRequirementsToml,
    requirement_source: Option<&RequirementSource>,
) -> AbsolutePathBuf {
    if let Some(source_path) = managed_hooks.managed_dir_for_current_platform()
        && source_path.is_absolute()
        && let Ok(source_path) = AbsolutePathBuf::from_absolute_path(source_path)
    {
        return source_path;
    }

    fallback_managed_hooks_source_path(requirement_source)
}

fn fallback_managed_hooks_source_path(
    requirement_source: Option<&RequirementSource>,
) -> AbsolutePathBuf {
    match requirement_source {
        Some(RequirementSource::SystemRequirementsToml { file })
        | Some(RequirementSource::LegacyManagedConfigTomlFromFile { file }) => file.clone(),
        Some(RequirementSource::MdmManagedPreferences { domain, key }) => {
            synthetic_layer_path(&format!("<mdm:{domain}:{key}>/requirements.toml"))
        }
        Some(RequirementSource::Composite { .. }) => {
            synthetic_layer_path("<requirements-composition>/requirements.toml")
        }
        Some(RequirementSource::EnterpriseManaged { id, name }) => {
            let name = escape_xml_text(name);
            let id = escape_xml_text(id);
            synthetic_layer_path(&format!(
                "<enterprise-managed:{name}:{id}>/requirements.toml"
            ))
        }
        Some(RequirementSource::LegacyManagedConfigTomlFromMdm) => {
            synthetic_layer_path("<legacy-managed-config.toml-mdm>/managed_config.toml")
        }
        Some(RequirementSource::Unknown) | None => {
            synthetic_layer_path("<managed-requirements>/requirements.toml")
        }
    }
}

fn load_hooks_json(
    config_folder: Option<&Path>,
    warnings: &mut Vec<String>,
) -> Option<(AbsolutePathBuf, HookEventsToml)> {
    let source_path = config_folder?.join("hooks.json");
    if !source_path.as_path().is_file() {
        return None;
    }

    let contents = match fs::read_to_string(source_path.as_path()) {
        Ok(contents) => contents,
        Err(err) => {
            warnings.push(format!(
                "failed to read hooks config {}: {err}",
                source_path.display()
            ));
            return None;
        }
    };

    let parsed: HooksFile = match serde_json::from_str(&contents) {
        Ok(parsed) => parsed,
        Err(err) => {
            warnings.push(format!(
                "failed to parse hooks config {}: {err}",
                source_path.display()
            ));
            return None;
        }
    };

    let source_path = AbsolutePathBuf::from_absolute_path(&source_path)
        .inspect_err(|err| {
            warnings.push(format!(
                "failed to normalize hooks config path {}: {err}",
                source_path.display()
            ));
        })
        .ok()?;

    (!parsed.hooks.is_empty()).then_some((source_path, parsed.hooks))
}

fn load_toml_hooks_from_layer(
    layer: &ConfigLayerEntry,
    warnings: &mut Vec<String>,
) -> Option<(AbsolutePathBuf, HookEventsToml)> {
    let source_path = config_toml_source_path(layer);
    let hook_value = layer.config.get("hooks")?.clone();
    let parsed = match HookEventsToml::deserialize(hook_value) {
        Ok(parsed) => parsed,
        Err(err) => {
            warnings.push(format!(
                "failed to parse TOML hooks in {}: {err}",
                source_path.display()
            ));
            return None;
        }
    };

    (!parsed.is_empty()).then_some((source_path, parsed))
}

fn config_toml_source_path(layer: &ConfigLayerEntry) -> AbsolutePathBuf {
    match &layer.name {
        ConfigLayerSource::PackagedDefaults { file }
        | ConfigLayerSource::System { file }
        | ConfigLayerSource::User { file, .. }
        | ConfigLayerSource::LegacyManagedConfigTomlFromFile { file } => file.clone(),
        ConfigLayerSource::Project { dot_codex_folder } => layer
            .hooks_config_folder()
            .unwrap_or_else(|| dot_codex_folder.clone())
            .join(CONFIG_TOML_FILE),
        ConfigLayerSource::Mdm { domain, key } => {
            synthetic_layer_path(&format!("<mdm:{domain}:{key}>/{CONFIG_TOML_FILE}"))
        }
        ConfigLayerSource::EnterpriseManaged { id, name } => synthetic_layer_path(&format!(
            "<enterprise-managed:{name}:{id}>/{CONFIG_TOML_FILE}"
        )),
        ConfigLayerSource::LegacyManagedConfigTomlFromMdm => {
            synthetic_layer_path("<legacy-managed-config.toml-mdm>/managed_config.toml")
        }
        ConfigLayerSource::SessionFlags => synthetic_layer_path("<session-flags>/config.toml"),
    }
}

fn synthetic_layer_path(path: &str) -> AbsolutePathBuf {
    #[cfg(windows)]
    {
        AbsolutePathBuf::resolve_path_against_base(path, r"C:\")
    }

    #[cfg(not(windows))]
    {
        AbsolutePathBuf::resolve_path_against_base(path, "/")
    }
}

fn escape_xml_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

pub(crate) fn discover_environment_hooks(
    sources: Vec<crate::EnvironmentHookSource>,
    mut display_order: i64,
) -> DiscoveryResult {
    let mut discovered = DiscoveryResult::default();
    for source in sources {
        append_environment_hooks(&mut discovered, source, &mut display_order);
    }
    discovered
}

/// Reject unloadable requirements before accepting an environment's configuration.
pub fn validate_environment_hooks(
    environment_id: &str,
    cwd: &PathUri,
    hooks: &HookEventsToml,
) -> anyhow::Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }
    let source = crate::EnvironmentHookSource {
        environment_id: environment_id.to_owned(),
        cwd: cwd.clone(),
        hooks: hooks.clone(),
    };
    let discovered = discover_environment_hooks(vec![source], /*display_order*/ 0);
    ensure_required_hooks_loaded(&discovered.required_load_errors)
}

fn append_environment_hooks(
    discovered: &mut DiscoveryResult,
    mut source: crate::EnvironmentHookSource,
    display_order: &mut i64,
) {
    let path = synthetic_layer_path(&format!(
        "<environment:{}>/requirements.toml",
        source.environment_id
    ));
    // Resolve overrides against the executor platform before normal local discovery.
    for (_, groups) in source.hooks.matcher_groups_mut() {
        for handler in groups.iter_mut().flat_map(|group| &mut group.hooks) {
            if let HookHandlerConfig::Command {
                command,
                command_windows,
                ..
            } = handler
                && let Some(windows) = command_windows.take()
                && source.cwd.infer_path_convention() == Some(PathConvention::Windows)
            {
                *command = windows;
            }
        }
    }
    let handler_start = discovered.handlers.len();
    let entry_start = discovered.hook_entries.len();
    append_hook_events(
        &mut discovered.handlers,
        &mut discovered.hook_entries,
        &mut discovered.warnings,
        display_order,
        HookHandlerSource {
            path: &path,
            key_source: path.display().to_string(),
            source: HookSource::System,
            is_managed: true,
            requirement: HookRequirement::Required(&mut discovered.required_load_errors),
            bypass_hook_trust: false,
            hook_states: &HashMap::new(),
            env: HashMap::new(),
            plugin_id: None,
            plugin_content_hash: None,
            config_layer_stack: None,
            plugin_root: None,
        },
        source.hooks,
        HookDiscoveryPolicy {
            config_layer_stack: None,
            mode: DiscoveryMode::Runtime,
            allow_managed_hooks_only: true,
            bypass_hook_trust: false,
        },
    );
    for entry in &mut discovered.hook_entries[entry_start..] {
        entry.environment_id = Some(source.environment_id.clone());
    }
    for handler in &mut discovered.handlers[handler_start..] {
        handler.source_path = super::HandlerSourcePath::Environment {
            environment_id: source.environment_id.clone(),
            cwd: source.cwd.clone(),
            source_path: path.clone().into(),
        };
    }
}

fn append_hook_events(
    handlers: &mut Vec<ConfiguredHandler>,
    hook_entries: &mut Vec<HookListEntry>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    mut source: HookHandlerSource<'_>,
    hook_events: HookEventsToml,
    policy: HookDiscoveryPolicy<'_>,
) {
    if !policy.allows(&source) {
        return;
    }

    for (event_name, groups) in hook_events.into_matcher_groups() {
        append_matcher_groups(
            handlers,
            hook_entries,
            warnings,
            display_order,
            &mut source,
            event_name,
            groups,
        );
    }
}

fn append_matcher_groups(
    handlers: &mut Vec<ConfiguredHandler>,
    hook_entries: &mut Vec<HookListEntry>,
    warnings: &mut Vec<String>,
    display_order: &mut i64,
    source: &mut HookHandlerSource<'_>,
    event_name: codex_protocol::protocol::HookEventName,
    groups: Vec<MatcherGroup>,
) {
    for (group_index, group) in groups.into_iter().enumerate() {
        let matcher = matcher_pattern_for_event(event_name, group.matcher.as_deref());
        let compiled_matcher = match matcher.map(HookMatcher::new).transpose() {
            Ok(matcher) => matcher,
            Err(err) => {
                let matcher = matcher.unwrap_or_default();
                let warning = format!(
                    "invalid matcher {matcher:?} in {}: {err}",
                    source.path.display()
                );
                if group.hooks.is_empty() {
                    warnings.push(warning);
                } else {
                    source.record_load_failure(warning, warnings);
                }
                continue;
            }
        };
        for (handler_index, handler) in group.hooks.iter().cloned().enumerate() {
            let normalized = match handler {
                HookHandlerConfig::Command {
                    command,
                    command_windows,
                    timeout_sec,
                    r#async,
                    status_message,
                    additional_context_limit,
                } => {
                    let command = if cfg!(windows) {
                        command_windows.unwrap_or(command)
                    } else {
                        command
                    };
                    if command.trim().is_empty() {
                        source.record_load_failure(
                            format!("skipping empty hook command in {}", source.path.display()),
                            warnings,
                        );
                        continue;
                    }
                    let timeout_sec = normalize_command_hook(
                        event_name,
                        timeout_sec,
                        source.path.as_path(),
                        warnings,
                    );
                    let runs_async = r#async && event_name != HookEventName::SessionEnd;
                    if r#async && !runs_async {
                        warnings.push(format!(
                            "running async {} hook synchronously in {}",
                            hook_event_name_label(event_name),
                            source.path.display()
                        ));
                    }
                    let additional_context_limit = if matches!(
                        event_name,
                        codex_protocol::protocol::HookEventName::PreToolUse
                            | codex_protocol::protocol::HookEventName::PostToolUse
                            | codex_protocol::protocol::HookEventName::SessionStart
                            | codex_protocol::protocol::HookEventName::UserPromptSubmit
                            | codex_protocol::protocol::HookEventName::SubagentStart
                    ) {
                        additional_context_limit
                    } else {
                        if additional_context_limit.is_some() {
                            warnings.push(format!(
                                "ignoring additionalContextLimit for {event_name:?} hook in {}: this event cannot emit additionalContext",
                                source.path.display()
                            ));
                        }
                        None
                    };
                    let normalized_additional_context_limit = additional_context_limit
                        .filter(|limit| *limit != DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT);
                    let config = HookHandlerConfig::Command {
                        command: command.clone(),
                        command_windows: None,
                        timeout_sec: Some(timeout_sec),
                        r#async,
                        status_message: status_message.clone(),
                        additional_context_limit: normalized_additional_context_limit,
                    };
                    let command = source.env.iter().fold(command, |command, (key, value)| {
                        command.replace(&format!("${{{key}}}"), value)
                    });
                    NormalizedHandler {
                        config,
                        kind: ConfiguredHandlerKind::Command {
                            command,
                            env: source.env.clone(),
                            r#async: runs_async,
                        },
                        timeout_sec,
                        status_message,
                        additional_context_limit,
                    }
                }
                HookHandlerConfig::McpTool {
                    server,
                    tool,
                    input,
                    timeout_sec,
                    status_message,
                } => {
                    if event_name == HookEventName::SessionEnd {
                        source.record_load_failure(
                            format!(
                                "skipping MCP tool hook in {}: {} MCP hooks are not supported",
                                source.path.display(),
                                hook_event_name_label(event_name),
                            ),
                            warnings,
                        );
                        continue;
                    }
                    if server.trim().is_empty() || tool.trim().is_empty() {
                        source.record_load_failure(
                            format!(
                                "skipping MCP tool hook in {}: server and tool must not be empty",
                                source.path.display()
                            ),
                            warnings,
                        );
                        continue;
                    }
                    let timeout_sec = normalize_command_hook(
                        event_name,
                        timeout_sec,
                        source.path.as_path(),
                        warnings,
                    );
                    let config = HookHandlerConfig::McpTool {
                        server: server.clone(),
                        tool: tool.clone(),
                        input: input.clone(),
                        timeout_sec: Some(timeout_sec),
                        status_message: status_message.clone(),
                    };
                    NormalizedHandler {
                        config,
                        kind: ConfiguredHandlerKind::McpTool {
                            server,
                            tool,
                            input,
                        },
                        timeout_sec,
                        status_message,
                        additional_context_limit: None,
                    }
                }
                HookHandlerConfig::Prompt {} => {
                    source.record_load_failure(
                        format!(
                            "skipping prompt hook in {}: prompt hooks are not supported yet",
                            source.path.display()
                        ),
                        warnings,
                    );
                    continue;
                }
                HookHandlerConfig::Agent {} => {
                    source.record_load_failure(
                        format!(
                            "skipping agent hook in {}: agent hooks are not supported yet",
                            source.path.display()
                        ),
                        warnings,
                    );
                    continue;
                }
            };

            let NormalizedHandler {
                config,
                kind,
                timeout_sec,
                status_message,
                additional_context_limit,
            } = normalized;
            let key = crate::hook_key(&source.key_source, event_name, group_index, handler_index);
            let state = source.hook_states.get(&key);
            let builtin = source.plugin_id.as_deref().is_some_and(|plugin_id| {
                is_allowlisted_bundled_cleanup_hook(
                    plugin_id,
                    event_name,
                    group.matcher.as_deref(),
                    &config,
                    /*app_connector_id*/ None,
                )
            });
            let plugin_content_hash = match source.plugin_content_hash {
                Some(Ok(hash)) => Some(hash.as_str()),
                Some(Err(err)) if !builtin && !source.bypass_hook_trust => {
                    source.record_load_failure(
                        format!(
                            "skipping plugin hook in {}: failed to fingerprint plugin contents: {err}",
                            source.path.display()
                        ),
                        warnings,
                    );
                    continue;
                }
                Some(Err(_)) | None => None,
            };
            let declaration_hash = hook_hash(event_name, matcher, &group, &config);
            // The config writer persists these two approval hashes separately.
            let current_hash = plugin_content_hash.map_or_else(
                || declaration_hash.clone(),
                |hash| format!("{declaration_hash}/{hash}"),
            );
            let enabled = hook_enabled(source.is_managed, builtin, state);
            let trusted_hash = hook_trusted_hash(source.is_managed, state);
            let plugin_trust = if !builtin
                && (!source.bypass_hook_trust || source.plugin_content_hash.is_some())
                && let Some(root) = source.plugin_root
                && let Some(plugin_id) = source.plugin_id.as_deref()
                && let Some(trusted_hash) = trusted_hash
            {
                Some(PluginHookTrust {
                    root: root.clone(),
                    legacy_path: source
                        .config_layer_stack
                        .and_then(|stack| super::plugin_trust::legacy_trust_path(stack, plugin_id)),
                    declaration_hash,
                    trusted_hash: trusted_hash.to_owned(),
                    trusted_content_hash: state
                        .and_then(|state| state.trusted_content_hash.clone()),
                })
            } else {
                None
            };
            let trust_status = if let Some(content_hash) = plugin_content_hash
                && let Some(trust) = plugin_trust.as_ref()
            {
                match trust.matches_hash(content_hash) {
                    Ok(true) => HookTrustStatus::Trusted,
                    Ok(false) => HookTrustStatus::Modified,
                    Err(err) => {
                        source.record_load_failure(
                            format!(
                                "skipping plugin hook in {}: failed to migrate hook trust: {err}",
                                source.path.display()
                            ),
                            warnings,
                        );
                        continue;
                    }
                }
            } else {
                hook_trust_status(source.is_managed, builtin, &current_hash, trusted_hash)
            };
            let handler = match &kind {
                ConfiguredHandlerKind::Command {
                    command, r#async, ..
                } => HookListEntryHandler::Command {
                    command: command.clone(),
                    r#async: *r#async,
                },
                ConfiguredHandlerKind::McpTool { server, tool, .. } => {
                    HookListEntryHandler::McpTool {
                        server: server.clone(),
                        tool: tool.clone(),
                    }
                }
            };

            hook_entries.push(HookListEntry {
                builtin,
                key,
                event_name,
                handler,
                matcher: matcher.map(ToOwned::to_owned),
                timeout_sec,
                status_message: status_message.clone(),
                additional_context_limit,
                source_path: source.path.clone(),
                source: source.source,
                plugin_id: source.plugin_id.clone(),
                environment_id: None,
                display_order: *display_order,
                enabled,
                is_managed: source.is_managed,
                current_hash,
                trust_status,
            });
            if enabled
                && (source.bypass_hook_trust
                    || (source.plugin_content_hash.is_none() && plugin_trust.is_some())
                    || matches!(
                        trust_status,
                        HookTrustStatus::Managed | HookTrustStatus::Trusted
                    ))
            {
                handlers.push(ConfiguredHandler {
                    builtin,
                    plugin_trust,
                    event_name,
                    matcher: compiled_matcher.clone(),
                    timeout_sec,
                    status_message,
                    additional_context_limit: AdditionalContextLimit::from_config(
                        additional_context_limit,
                    ),
                    source_path: source.path.clone().into(),
                    source: source.source,
                    display_order: *display_order,
                    kind,
                });
            }
            *display_order += 1;
        }
    }
}

/// Normalizes hook timeouts. SessionEnd and Interrupt default to one second and are capped at three
/// seconds; all other hooks keep the standard ten-minute default.
fn normalize_command_hook(
    event_name: HookEventName,
    timeout_sec: Option<u64>,
    source_path: &Path,
    warnings: &mut Vec<String>,
) -> u64 {
    match event_name {
        HookEventName::SessionEnd | HookEventName::Interrupt => {
            let max_timeout_sec = SESSION_END_MAX_TIMEOUT_SEC;
            if timeout_sec.is_some_and(|timeout_sec| timeout_sec > max_timeout_sec) {
                warnings.push(format!(
                    "clamping {} hook timeout to {max_timeout_sec}s in {}",
                    hook_event_name_label(event_name),
                    source_path.display()
                ));
            }
            timeout_sec
                .unwrap_or(SESSION_END_DEFAULT_TIMEOUT_SEC)
                .clamp(1, max_timeout_sec)
        }
        _ => timeout_sec.unwrap_or(600).max(1),
    }
}

/// Hash a normalized, config-derived identity instead of source text so equivalent
/// hooks from config TOML and hooks.json converge on the same trust identity.
#[derive(Serialize)]
struct NormalizedHookIdentity {
    event_name: &'static str,
    #[serde(flatten)]
    group: MatcherGroup,
}

fn hook_hash(
    event_name: codex_protocol::protocol::HookEventName,
    matcher: Option<&str>,
    group: &MatcherGroup,
    normalized_handler: &HookHandlerConfig,
) -> String {
    let mut group = group.clone();
    group.matcher = matcher.map(ToOwned::to_owned);
    group.hooks = vec![normalized_handler.clone()];
    let identity = NormalizedHookIdentity {
        event_name: crate::hook_event_key_label(event_name),
        group,
    };
    let Ok(value) = TomlValue::try_from(identity) else {
        unreachable!("normalized hook identity should serialize to TOML");
    };
    version_for_toml(&value)
}

fn hook_trust_status(
    is_managed: bool,
    is_builtin: bool,
    current_hash: &str,
    trusted_hash: Option<&str>,
) -> HookTrustStatus {
    if is_builtin {
        HookTrustStatus::Trusted
    } else if is_managed {
        HookTrustStatus::Managed
    } else {
        match trusted_hash {
            Some(trusted_hash) if trusted_hash == current_hash => HookTrustStatus::Trusted,
            Some(_) => HookTrustStatus::Modified,
            None => HookTrustStatus::Untrusted,
        }
    }
}

fn hook_enabled(is_managed: bool, is_builtin: bool, state: Option<&HookStateToml>) -> bool {
    is_builtin || is_managed || state.and_then(|state| state.enabled) != Some(false)
}

fn hook_trusted_hash(is_managed: bool, state: Option<&HookStateToml>) -> Option<&str> {
    (!is_managed)
        .then(|| state.and_then(|state| state.trusted_hash.as_deref()))
        .flatten()
}

fn hook_metadata_for_config_layer_source(source: &ConfigLayerSource) -> (HookSource, bool) {
    match source {
        ConfigLayerSource::PackagedDefaults { .. } => (HookSource::Unknown, false),
        ConfigLayerSource::System { .. } => (HookSource::System, true),
        ConfigLayerSource::User { .. } => (HookSource::User, false),
        ConfigLayerSource::Project { .. } => (HookSource::Project, false),
        ConfigLayerSource::Mdm { .. } => (HookSource::Mdm, true),
        ConfigLayerSource::EnterpriseManaged { .. } => (HookSource::CloudManagedConfig, true),
        ConfigLayerSource::SessionFlags => (HookSource::SessionFlags, false),
        ConfigLayerSource::LegacyManagedConfigTomlFromFile { .. } => {
            (HookSource::LegacyManagedConfigFile, true)
        }
        ConfigLayerSource::LegacyManagedConfigTomlFromMdm => {
            (HookSource::LegacyManagedConfigMdm, true)
        }
    }
}

fn hook_source_for_requirement_source(source: Option<&RequirementSource>) -> HookSource {
    match source {
        Some(RequirementSource::MdmManagedPreferences { .. }) => HookSource::Mdm,
        Some(RequirementSource::SystemRequirementsToml { .. }) => HookSource::System,
        Some(RequirementSource::LegacyManagedConfigTomlFromFile { .. }) => {
            HookSource::LegacyManagedConfigFile
        }
        Some(RequirementSource::LegacyManagedConfigTomlFromMdm) => {
            HookSource::LegacyManagedConfigMdm
        }
        Some(RequirementSource::Composite { sources }) => {
            // Requirements hook composition preserves contributing sources in
            // priority order, but discovery only carries one source for the
            // whole merged hooks field. Use the primary contributor as the best
            // available coarse attribution.
            hook_source_for_requirement_source(sources.first())
        }
        Some(RequirementSource::EnterpriseManaged { .. }) => HookSource::CloudRequirements,
        Some(RequirementSource::Unknown) | None => HookSource::Unknown,
    }
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
