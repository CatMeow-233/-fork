use super::HookMatcher;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::HookEventsToml;
use codex_config::RequirementSource;
use codex_protocol::protocol::HookEventName;
use codex_protocol::protocol::HookExecutionMode;
use codex_protocol::protocol::HookSource;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use pretty_assertions::assert_eq;

use super::ConfiguredHandler;
use super::ConfiguredHandlerKind;
use super::HookListEntry;
use super::HookListEntryHandler;
use super::append_matcher_groups;
use super::normalize_command_hook;
use crate::output_spill::AdditionalContextLimit;
use crate::output_spill::DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT;
use codex_config::HookHandlerConfig;
use codex_config::HookStateToml;
use codex_config::MatcherGroup;
use codex_config::TomlValue;
use codex_protocol::protocol::HookTrustStatus;

fn source_path() -> AbsolutePathBuf {
    test_path_buf("/tmp/hooks.json").abs()
}

fn hook_source() -> HookSource {
    HookSource::System
}

fn hook_handler_source<'a>(
    path: &'a AbsolutePathBuf,
    hook_states: &'a std::collections::HashMap<String, HookStateToml>,
) -> super::HookHandlerSource<'a> {
    super::HookHandlerSource {
        path,
        key_source: path.display().to_string(),
        source: hook_source(),
        is_managed: true,
        requirement: super::HookRequirement::Optional,
        bypass_hook_trust: false,
        hook_states,
        env: std::collections::HashMap::new(),
        plugin_id: None,
        plugin_content_hash: None,
        config_layer_stack: None,
        plugin_root: None,
    }
}

fn unmanaged_hook_handler_source<'a>(
    path: &'a AbsolutePathBuf,
    hook_states: &'a std::collections::HashMap<String, HookStateToml>,
    bypass_hook_trust: bool,
) -> super::HookHandlerSource<'a> {
    super::HookHandlerSource {
        path,
        key_source: path.display().to_string(),
        source: HookSource::User,
        is_managed: false,
        requirement: super::HookRequirement::Optional,
        bypass_hook_trust,
        hook_states,
        env: std::collections::HashMap::new(),
        plugin_id: None,
        plugin_content_hash: None,
        config_layer_stack: None,
        plugin_root: None,
    }
}

#[test]
fn composite_requirement_hook_source_uses_primary_source() {
    let source = RequirementSource::Composite {
        sources: vec![
            RequirementSource::SystemRequirementsToml {
                file: test_path_buf("/etc/codex/requirements.toml").abs(),
            },
            RequirementSource::EnterpriseManaged {
                id: "layer-1".to_string(),
                name: "Engineering".to_string(),
            },
        ],
    };

    assert_eq!(
        super::hook_source_for_requirement_source(Some(&source)),
        HookSource::System
    );
}

#[test]
fn enterprise_managed_synthetic_path_escapes_display_fields() {
    let source = RequirementSource::EnterpriseManaged {
        id: "id<&>".to_string(),
        name: "Name <Admin> & \"Ops\"".to_string(),
    };

    let source_path = super::fallback_managed_hooks_source_path(Some(&source));
    let source_path = source_path.display().to_string();

    assert!(source_path.contains("Name &lt;Admin&gt; &amp; &quot;Ops&quot;"));
    assert!(source_path.contains("id&lt;&amp;&gt;"));
    assert!(!source_path.contains("Name <Admin>"));
}

fn command_group(matcher: Option<&str>) -> MatcherGroup {
    MatcherGroup {
        matcher: matcher.map(str::to_string),
        hooks: vec![HookHandlerConfig::Command {
            command: "echo hello".to_string(),
            command_windows: None,
            timeout_sec: None,
            r#async: false,
            status_message: None,
            additional_context_limit: None,
        }],
    }
}

fn command_group_with_additional_context_limit(additional_context_limit: usize) -> MatcherGroup {
    MatcherGroup {
        matcher: None,
        hooks: vec![HookHandlerConfig::Command {
            command: "echo hello".to_string(),
            command_windows: None,
            timeout_sec: None,
            r#async: false,
            status_message: None,
            additional_context_limit: Some(additional_context_limit),
        }],
    }
}

#[test]
fn mcp_tool_hooks_preserve_argument_templates_and_list_their_target() {
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();
    let input = serde_json::from_value(serde_json::json!({
        "file_path": "${tool_input.file_path}",
        "optional": "${tool_input.optional}",
    }))
    .expect("MCP hook input should be an object");
    let mut handlers = Vec::new();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;

    append_matcher_groups(
        &mut handlers,
        &mut entries,
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::PostToolUse,
        vec![MatcherGroup {
            matcher: Some("Write|Edit".to_string()),
            hooks: vec![HookHandlerConfig::McpTool {
                server: "security".to_string(),
                tool: "scan".to_string(),
                input,
                timeout_sec: Some(30),
                status_message: Some("Scanning file".to_string()),
            }],
        }],
    );

    assert!(warnings.is_empty());
    assert_eq!(handlers.len(), 1);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].handler,
        HookListEntryHandler::McpTool {
            server: "security".to_string(),
            tool: "scan".to_string(),
        }
    );
    assert!(entries[0].current_hash.starts_with("sha256:"));
}

#[test]
fn session_end_mcp_tool_hooks_are_warned_and_skipped() {
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();
    let mut handlers = Vec::new();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;

    append_matcher_groups(
        &mut handlers,
        &mut entries,
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::SessionEnd,
        vec![MatcherGroup {
            matcher: None,
            hooks: vec![HookHandlerConfig::McpTool {
                server: "security".to_string(),
                tool: "scan".to_string(),
                input: serde_json::Map::new(),
                timeout_sec: None,
                status_message: None,
            }],
        }],
    );

    assert!(handlers.is_empty());
    assert!(entries.is_empty());
    assert_eq!(
        warnings,
        vec![format!(
            "skipping MCP tool hook in {}: SessionEnd MCP hooks are not supported",
            source_path.display()
        )]
    );
}

#[test]
fn interrupt_mcp_tool_hooks_are_supported_and_timeout_is_clamped() {
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();
    let mut handlers = Vec::new();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;

    append_matcher_groups(
        &mut handlers,
        &mut entries,
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::Interrupt,
        vec![MatcherGroup {
            matcher: None,
            hooks: vec![
                HookHandlerConfig::McpTool {
                    server: "security".to_string(),
                    tool: "scan".to_string(),
                    input: serde_json::Map::new(),
                    timeout_sec: None,
                    status_message: None,
                },
                HookHandlerConfig::McpTool {
                    server: "security".to_string(),
                    tool: "report".to_string(),
                    input: serde_json::Map::new(),
                    timeout_sec: Some(600),
                    status_message: None,
                },
            ],
        }],
    );

    assert_eq!(
        handlers
            .iter()
            .map(|handler| handler.timeout_sec)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.timeout_sec)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(
        entries
            .iter()
            .all(|entry| matches!(entry.handler, HookListEntryHandler::McpTool { .. }))
    );
    assert_eq!(
        warnings,
        vec![format!(
            "clamping Interrupt hook timeout to 3s in {}",
            source_path.display()
        )]
    );
}

fn discover_command(
    event_name: HookEventName,
    additional_context_limit: Option<usize>,
) -> (ConfiguredHandler, HookListEntry, Vec<String>) {
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();
    let mut handlers = Vec::new();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    append_matcher_groups(
        &mut handlers,
        &mut entries,
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        event_name,
        vec![match additional_context_limit {
            Some(limit) => command_group_with_additional_context_limit(limit),
            None => command_group(/*matcher*/ None),
        }],
    );
    (handlers.remove(0), entries.remove(0), warnings)
}

#[test]
fn supported_events_retain_per_handler_additional_context_limit_and_hash_it() {
    for event_name in [
        HookEventName::PreToolUse,
        HookEventName::PostToolUse,
        HookEventName::SessionStart,
        HookEventName::UserPromptSubmit,
        HookEventName::SubagentStart,
    ] {
        let (_, default_entry, _) =
            discover_command(event_name, /*additional_context_limit*/ None);
        let (explicit_default_handler, explicit_default_entry, _) = discover_command(
            event_name,
            /*additional_context_limit*/ Some(DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT),
        );
        let (custom_handler, custom_entry, _) =
            discover_command(event_name, /*additional_context_limit*/ Some(20_000));
        let (unlimited_handler, unlimited_entry, _) =
            discover_command(event_name, /*additional_context_limit*/ Some(0));

        assert_eq!(
            custom_handler.additional_context_limit,
            AdditionalContextLimit::from_config(Some(20_000))
        );
        assert_eq!(custom_entry.additional_context_limit, Some(20_000));
        assert_ne!(default_entry.current_hash, custom_entry.current_hash);
        assert_eq!(
            explicit_default_handler.additional_context_limit,
            AdditionalContextLimit::from_config(Some(DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT))
        );
        assert_eq!(
            explicit_default_entry.additional_context_limit,
            Some(DEFAULT_HOOK_OUTPUT_TOKEN_LIMIT)
        );
        assert_eq!(
            default_entry.current_hash,
            explicit_default_entry.current_hash
        );
        assert_eq!(
            unlimited_handler.additional_context_limit,
            AdditionalContextLimit::from_config(Some(0))
        );
        assert_eq!(unlimited_entry.additional_context_limit, Some(0));
        assert_ne!(default_entry.current_hash, unlimited_entry.current_hash);
    }
}

#[test]
fn unsupported_event_warns_and_ignores_additional_context_limit() {
    let source_path = source_path();
    let (handler, _, warnings) = discover_command(
        HookEventName::Stop,
        /*additional_context_limit*/ Some(4_096),
    );

    assert_eq!(handler.additional_context_limit, Default::default());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("ignoring additionalContextLimit for Stop hook"));
    assert!(warnings[0].contains(&source_path.display().to_string()));
}

#[test]
fn user_prompt_submit_ignores_invalid_matcher_during_discovery() {
    let mut handlers = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut Vec::new(),
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::UserPromptSubmit,
        vec![command_group(Some("["))],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        handlers,
        vec![ConfiguredHandler {
            builtin: false,
            plugin_trust: None,
            event_name: HookEventName::UserPromptSubmit,
            matcher: None,
            timeout_sec: 600,
            status_message: None,
            additional_context_limit: Default::default(),
            source_path: source_path.clone().into(),
            source: hook_source(),
            display_order: 0,
            kind: ConfiguredHandlerKind::Command {
                command: "echo hello".to_string(),
                r#async: false,
                env: std::collections::HashMap::new(),
            },
        }]
    );
}

#[test]
fn pre_tool_use_keeps_valid_matcher_during_discovery() {
    let mut handlers = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut Vec::new(),
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::PreToolUse,
        vec![command_group(Some("^Bash$"))],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        handlers,
        vec![ConfiguredHandler {
            builtin: false,
            plugin_trust: None,
            event_name: HookEventName::PreToolUse,
            matcher: Some(HookMatcher::new("^Bash$").expect("valid matcher")),
            timeout_sec: 600,
            status_message: None,
            additional_context_limit: Default::default(),
            source_path: source_path.clone().into(),
            source: hook_source(),
            display_order: 0,
            kind: ConfiguredHandlerKind::Command {
                command: "echo hello".to_string(),
                r#async: false,
                env: std::collections::HashMap::new(),
            },
        }]
    );
}

#[test]
fn session_end_normalizes_timeout() {
    let mut handlers = Vec::new();
    let mut hook_entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut hook_entries,
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::SessionEnd,
        vec![MatcherGroup {
            matcher: Some("other".to_string()),
            hooks: vec![
                HookHandlerConfig::Command {
                    command: "echo default".to_string(),
                    command_windows: None,
                    timeout_sec: None,
                    r#async: false,
                    status_message: None,
                    additional_context_limit: None,
                },
                HookHandlerConfig::Command {
                    command: "echo clamped".to_string(),
                    command_windows: None,
                    timeout_sec: Some(600),
                    r#async: true,
                    status_message: None,
                    additional_context_limit: None,
                },
            ],
        }],
    );

    assert_eq!(
        handlers
            .iter()
            .map(|handler| handler.timeout_sec)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(
        handlers
            .iter()
            .all(ConfiguredHandler::can_apply_control_effects)
    );
    assert_eq!(
        handlers
            .iter()
            .map(|handler| handler.matcher.as_ref().map(HookMatcher::as_str))
            .collect::<Vec<_>>(),
        vec![Some("other"), Some("other")]
    );
    assert_eq!(
        hook_entries
            .iter()
            .map(|entry| entry.timeout_sec)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(hook_entries.iter().all(|entry| matches!(
        entry.handler,
        HookListEntryHandler::Command { r#async: false, .. }
    )));
    assert_eq!(
        hook_entries
            .iter()
            .map(|entry| entry.matcher.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("other"), Some("other")]
    );
    assert_eq!(
        warnings,
        vec![
            format!(
                "clamping SessionEnd hook timeout to 3s in {}",
                source_path.display()
            ),
            format!(
                "running async SessionEnd hook synchronously in {}",
                source_path.display()
            ),
        ]
    );
}

#[test]
fn interrupt_normalizes_timeout_and_supports_async_execution() {
    let mut handlers = Vec::new();
    let mut hook_entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut hook_entries,
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::Interrupt,
        vec![MatcherGroup {
            matcher: Some("ignored".to_string()),
            hooks: vec![HookHandlerConfig::Command {
                command: "echo interrupt".to_string(),
                command_windows: None,
                timeout_sec: Some(600),
                r#async: true,
                status_message: None,
                additional_context_limit: None,
            }],
        }],
    );

    assert_eq!(
        normalize_command_hook(
            HookEventName::Interrupt,
            /*timeout_sec*/ None,
            source_path.as_path(),
            &mut Vec::new(),
        ),
        1
    );
    assert_eq!(
        handlers
            .iter()
            .map(|handler| (
                handler.timeout_sec,
                handler.matcher.as_ref().map(HookMatcher::as_str),
                handler.execution_mode()
            ))
            .collect::<Vec<_>>(),
        vec![(3, None, HookExecutionMode::Async)]
    );
    assert_eq!(
        hook_entries
            .iter()
            .map(|entry| (entry.timeout_sec, entry.matcher.as_deref()))
            .collect::<Vec<_>>(),
        vec![(3, None)]
    );
    assert!(hook_entries.iter().all(|entry| matches!(
        entry.handler,
        HookListEntryHandler::Command { r#async: true, .. }
    )));
    assert_eq!(
        warnings,
        vec![format!(
            "clamping Interrupt hook timeout to 3s in {}",
            source_path.display()
        )]
    );
}

#[test]
fn bypass_hook_trust_allows_enabled_untrusted_handlers() {
    let mut handlers = Vec::new();
    let mut hook_entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut hook_entries,
        &mut warnings,
        &mut display_order,
        &mut unmanaged_hook_handler_source(
            &source_path,
            &hook_states,
            /*bypass_hook_trust*/ true,
        ),
        HookEventName::PreToolUse,
        vec![command_group(Some("Bash"))],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(handlers.len(), 1);
    assert_eq!(hook_entries.len(), 1);
    assert_eq!(hook_entries[0].trust_status, HookTrustStatus::Untrusted);
    assert_eq!(hook_entries[0].enabled, true);
}

#[test]
fn bypass_hook_trust_respects_disabled_handlers() {
    let mut handlers = Vec::new();
    let mut hook_entries = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::from([(
        format!("{}:pre_tool_use:0:0", source_path.display()),
        HookStateToml {
            enabled: Some(false),
            trusted_hash: None,
            trusted_content_hash: None,
        },
    )]);

    append_matcher_groups(
        &mut handlers,
        &mut hook_entries,
        &mut warnings,
        &mut display_order,
        &mut unmanaged_hook_handler_source(
            &source_path,
            &hook_states,
            /*bypass_hook_trust*/ true,
        ),
        HookEventName::PreToolUse,
        vec![command_group(Some("Bash"))],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(handlers, Vec::<ConfiguredHandler>::new());
    assert_eq!(hook_entries.len(), 1);
    assert_eq!(hook_entries[0].trust_status, HookTrustStatus::Untrusted);
    assert_eq!(hook_entries[0].enabled, false);
}

#[test]
fn pre_tool_use_treats_star_matcher_as_match_all() {
    let mut handlers = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut Vec::new(),
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::PreToolUse,
        vec![command_group(Some("*"))],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(handlers.len(), 1);
    assert_eq!(
        handlers[0].matcher.as_ref().map(HookMatcher::as_str),
        Some("*")
    );
}

#[test]
fn post_tool_use_keeps_valid_matcher_during_discovery() {
    let mut handlers = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut Vec::new(),
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::PostToolUse,
        vec![command_group(Some("Edit|Write"))],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].event_name, HookEventName::PostToolUse);
    assert_eq!(
        handlers[0].matcher.as_ref().map(HookMatcher::as_str),
        Some("Edit|Write")
    );
}

#[test]
fn toml_hook_discovery_ignores_malformed_state_entries() {
    let layer = ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: test_path_buf("/tmp/config.toml").abs(),
            profile: None,
        },
        config_with_malformed_state_and_session_start_hook(),
    );
    let mut warnings = Vec::new();

    let (_, hooks) = super::load_toml_hooks_from_layer(&layer, &mut warnings)
        .expect("valid hook events should still load");

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(
        hooks,
        HookEventsToml {
            session_start: vec![MatcherGroup {
                matcher: None,
                hooks: vec![HookHandlerConfig::Command {
                    command: "echo hello".to_string(),
                    command_windows: None,
                    timeout_sec: None,
                    r#async: false,
                    status_message: None,
                    additional_context_limit: None,
                }],
            }],
            ..Default::default()
        }
    );
}

#[test]
fn pre_tool_use_resolves_windows_command_override_during_discovery() {
    let mut handlers = Vec::new();
    let mut warnings = Vec::new();
    let mut display_order = 0;
    let source_path = source_path();
    let hook_states = std::collections::HashMap::new();

    append_matcher_groups(
        &mut handlers,
        &mut Vec::new(),
        &mut warnings,
        &mut display_order,
        &mut hook_handler_source(&source_path, &hook_states),
        HookEventName::PreToolUse,
        vec![MatcherGroup {
            matcher: Some("^Bash$".to_string()),
            hooks: vec![HookHandlerConfig::Command {
                command: "echo unix".to_string(),
                command_windows: Some("echo windows".to_string()),
                timeout_sec: None,
                r#async: false,
                status_message: None,
                additional_context_limit: None,
            }],
        }],
    );

    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(handlers.len(), 1);
    assert_eq!(
        handlers[0].kind,
        ConfiguredHandlerKind::Command {
            command: if cfg!(windows) {
                "echo windows"
            } else {
                "echo unix"
            }
            .to_string(),
            env: std::collections::HashMap::new(),
            r#async: false,
        }
    );
}

fn config_with_malformed_state_and_session_start_hook() -> TomlValue {
    serde_json::from_value(serde_json::json!({
        "hooks": {
            "state": {
                "some_key": {
                    "enabled": "not a bool",
                },
            },
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": "echo hello",
                }],
            }],
        },
    }))
    .expect("config TOML should deserialize")
}

#[test]
fn hook_metadata_for_config_layer_source_discards_source_details() {
    let config_file = test_path_buf("/tmp/.codex/config.toml").abs();
    let dot_codex_folder = test_path_buf("/tmp/worktree/.codex").abs();

    assert_eq!(
        super::hook_metadata_for_config_layer_source(&ConfigLayerSource::System {
            file: config_file.clone(),
        }),
        (HookSource::System, true),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(&ConfigLayerSource::User {
            file: config_file.clone(),
            profile: None,
        }),
        (HookSource::User, false),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(&ConfigLayerSource::Project {
            dot_codex_folder
        }),
        (HookSource::Project, false),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(&ConfigLayerSource::Mdm {
            domain: "com.openai.codex".to_string(),
            key: "config".to_string(),
        }),
        (HookSource::Mdm, true),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(&ConfigLayerSource::EnterpriseManaged {
            id: "cfg_123".to_string(),
            name: "Base policy".to_string(),
        }),
        (HookSource::CloudManagedConfig, true),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(&ConfigLayerSource::SessionFlags),
        (HookSource::SessionFlags, false),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(
            &ConfigLayerSource::LegacyManagedConfigTomlFromFile { file: config_file },
        ),
        (HookSource::LegacyManagedConfigFile, true),
    );
    assert_eq!(
        super::hook_metadata_for_config_layer_source(
            &ConfigLayerSource::LegacyManagedConfigTomlFromMdm,
        ),
        (HookSource::LegacyManagedConfigMdm, true),
    );
}
