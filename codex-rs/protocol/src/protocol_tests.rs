use super::*;
use crate::items::CommandExecutionItem;
use crate::items::CommandExecutionStatus;
use crate::items::DynamicToolCallItem;
use crate::items::DynamicToolCallStatus;
use crate::items::EnteredReviewModeItem;
use crate::items::ExitedReviewModeItem;
use crate::items::FileChangeItem;
use crate::items::ImageGenerationItem;
use crate::items::McpToolCallItem;
use crate::items::McpToolCallStatus;
use crate::items::UserMessageItem;
use crate::items::WebSearchItem;
use crate::mcp::CallToolResult;
use crate::models::AudioReference;
use crate::permissions::FileSystemAccessMode;
use crate::permissions::FileSystemPath;
use crate::permissions::FileSystemSandboxEntry;
use crate::permissions::FileSystemSandboxPolicy;
use crate::permissions::FileSystemSpecialPath;
use crate::permissions::NetworkSandboxPolicy;
use anyhow::Result;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use tempfile::TempDir;

#[test]
fn old_turn_started_records_have_no_root_attribution() {
    let event: TurnStartedEvent = serde_json::from_value(serde_json::json!({
        "turn_id": "old-turn",
        "model_context_window": null
    }))
    .unwrap();
    assert_eq!(event.root_turn_id, None);
}

#[test]
fn review_decision_denied_round_trip() -> Result<()> {
    let decision = ReviewDecision::Denied {
        rejection: "denied reason".to_string(),
    };
    let value = json!({"denied": {"rejection": "denied reason"}});

    assert_eq!(serde_json::to_value(&decision)?, value);
    assert_eq!(serde_json::from_value::<ReviewDecision>(value)?, decision);
    Ok(())
}

#[test]
fn hook_builtin_classification_stays_internal() -> Result<()> {
    let wire = json!({
        "id": "cleanup-hook",
        "event_name": "stop",
        "handler_type": "mcp_tool",
        "execution_mode": "sync",
        "scope": "turn",
        "source_path": test_path_buf("/tmp/hooks.json").abs(),
        "source": "plugin",
        "display_order": 0,
        "status": "completed",
        "status_message": null,
        "started_at": 10,
        "completed_at": 11,
        "duration_ms": 1000,
        "entries": [],
    });
    let mut run: HookRunSummary = serde_json::from_value(wire.clone())?;
    assert!(!run.builtin);
    run.builtin = true;
    assert_eq!(serde_json::to_value(run)?, wire);

    let mut untrusted_wire = wire;
    untrusted_wire["builtin"] = json!(true);
    assert!(!serde_json::from_value::<HookRunSummary>(untrusted_wire)?.builtin);
    let schema = serde_json::to_value(schemars::schema_for!(HookRunSummary))?;
    assert!(
        !schema["properties"]
            .as_object()
            .expect("hook properties")
            .contains_key("builtin")
    );
    assert!(!HookRunSummary::decl().contains("builtin:"));
    Ok(())
}

#[test]
fn feature_thread_source_serializes_as_its_app_owned_label() -> Result<()> {
    let source = ThreadSource::Feature("automation".to_string());

    assert_eq!(serde_json::to_value(&source)?, json!("automation"));
    assert_eq!(
        serde_json::from_value::<ThreadSource>(json!("automation"))?,
        source
    );
    Ok(())
}

#[test]
fn session_meta_normalizes_legacy_dynamic_tools() -> Result<()> {
    let mut value = serde_json::to_value(SessionMeta::default())?;
    value["dynamic_tools"] = json!([
        {
            "namespace": "legacy_app",
            "name": "lookup_ticket",
            "description": "Look up a ticket",
            "inputSchema": {"type": "object", "properties": {}},
            "exposeToContext": false
        },
        {
            "namespace": "legacy_app",
            "name": "update_ticket",
            "description": "Update a ticket",
            "inputSchema": {"type": "object", "properties": {}},
            "deferLoading": false,
            "exposeToContext": false
        }
    ]);

    let meta: SessionMeta = serde_json::from_value(value)?;

    assert_eq!(
        meta.dynamic_tools,
        Some(vec![DynamicToolSpec::Namespace(
            crate::dynamic_tools::DynamicToolNamespaceSpec {
                name: "legacy_app".to_string(),
                description: String::new(),
                tools: vec![
                    crate::dynamic_tools::DynamicToolNamespaceTool::Function(
                        crate::dynamic_tools::DynamicToolFunctionSpec {
                            name: "lookup_ticket".to_string(),
                            description: "Look up a ticket".to_string(),
                            input_schema: json!({"type": "object", "properties": {}}),
                            defer_loading: true,
                        },
                    ),
                    crate::dynamic_tools::DynamicToolNamespaceTool::Function(
                        crate::dynamic_tools::DynamicToolFunctionSpec {
                            name: "update_ticket".to_string(),
                            description: "Update a ticket".to_string(),
                            input_schema: json!({"type": "object", "properties": {}}),
                            defer_loading: false,
                        },
                    ),
                ],
            },
        )])
    );
    Ok(())
}

fn sorted_writable_roots(roots: Vec<WritableRoot>) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let mut sorted_roots: Vec<(PathBuf, Vec<PathBuf>)> = roots
        .into_iter()
        .map(|root| {
            let mut read_only_subpaths: Vec<PathBuf> = root
                .read_only_subpaths
                .into_iter()
                .map(|path| path.to_path_buf())
                .collect();
            read_only_subpaths.sort();
            (root.root.to_path_buf(), read_only_subpaths)
        })
        .collect();
    sorted_roots.sort_by(|left, right| left.0.cmp(&right.0));
    sorted_roots
}

fn sandbox_policy_allows_read(policy: &SandboxPolicy, _path: &Path, _cwd: &Path) -> bool {
    policy.has_full_disk_read_access()
}

fn sandbox_policy_allows_write(policy: &SandboxPolicy, path: &Path, cwd: &Path) -> bool {
    if policy.has_full_disk_write_access() {
        return true;
    }

    policy
        .get_writable_roots_with_cwd(cwd)
        .iter()
        .any(|root| root.is_path_writable(path))
}

#[test]
fn session_source_from_startup_arg_maps_known_values() {
    assert_eq!(
        SessionSource::from_startup_arg("vscode").unwrap(),
        SessionSource::VSCode
    );
    assert_eq!(
        SessionSource::from_startup_arg("app-server").unwrap(),
        SessionSource::Mcp
    );
}

#[test]
fn inter_agent_communication_response_input_item_preserves_commentary_phase() {
    let mut communication = InterAgentCommunication {
        id: Some(ResponseItemId::with_suffix("amsg", "1")),
        author: AgentPath::root(),
        recipient: AgentPath::root().join("reviewer").expect("recipient path"),
        other_recipients: vec![AgentPath::root().join("worker").expect("recipient path")],
        content: "review the diff".to_string(),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
        trigger_turn: true,
    };
    communication.set_turn_id_if_missing("turn-1");
    let mut serialized_communication = communication.clone();
    serialized_communication.id = None;
    serialized_communication.internal_chat_message_metadata_passthrough = None;

    assert_eq!(
        communication.to_response_input_item(),
        ResponseInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                annotations: None,
                logprobs: None,
                text: serde_json::to_string(&serialized_communication)
                    .expect("serialize communication"),
            }],
            phase: Some(MessagePhase::Commentary),
        }
    );
}

#[test]
fn queued_encrypted_inter_agent_communication_renders_message_envelope() {
    let communication = InterAgentCommunication::new_encrypted(
        AgentPath::root().join("worker").expect("author path"),
        AgentPath::root(),
        Vec::new(),
        "encrypted payload".to_string(),
        /*trigger_turn*/ false,
    );

    assert_eq!(
        communication.to_model_input_item(),
        ResponseItem::AgentMessage {
            id: None,
            author: "/root/worker".to_string(),
            recipient: "/root".to_string(),
            content: vec![
                AgentMessageInputContent::InputText {
                    text:
                        "Message Type: MESSAGE\nTask name: /root\nSender: /root/worker\nPayload:\n"
                            .to_string(),
                },
                AgentMessageInputContent::EncryptedContent {
                    encrypted_content: "encrypted payload".to_string(),
                },
            ],
            internal_chat_message_metadata_passthrough: None,
        }
    );
}

#[test]
fn session_source_from_startup_arg_normalizes_custom_values() {
    assert_eq!(
        SessionSource::from_startup_arg("atlas").unwrap(),
        SessionSource::Custom("atlas".to_string())
    );
    assert_eq!(
        SessionSource::from_startup_arg(" Atlas ").unwrap(),
        SessionSource::Custom("atlas".to_string())
    );
}

#[test]
fn session_source_restriction_product_defaults_non_subagent_sources_to_codex() {
    assert_eq!(
        SessionSource::Cli.restriction_product(),
        Some(Product::Codex)
    );
    assert_eq!(
        SessionSource::VSCode.restriction_product(),
        Some(Product::Codex)
    );
    assert_eq!(
        SessionSource::Exec.restriction_product(),
        Some(Product::Codex)
    );
    assert_eq!(
        SessionSource::Mcp.restriction_product(),
        Some(Product::Codex)
    );
    assert_eq!(
        SessionSource::Unknown.restriction_product(),
        Some(Product::Codex)
    );
}

#[test]
fn session_source_restriction_product_does_not_guess_subagent_products() {
    assert_eq!(
        SessionSource::SubAgent(SubAgentSource::Review).restriction_product(),
        None
    );
    assert_eq!(
        SessionSource::Internal(InternalSessionSource::MemoryConsolidation).restriction_product(),
        None
    );
}

#[test]
fn session_source_restriction_product_maps_custom_sources_to_products() {
    assert_eq!(
        SessionSource::Custom("chatgpt".to_string()).restriction_product(),
        Some(Product::Chatgpt)
    );
    assert_eq!(
        SessionSource::Custom("ATLAS".to_string()).restriction_product(),
        Some(Product::Atlas)
    );
    assert_eq!(
        SessionSource::Custom("codex".to_string()).restriction_product(),
        Some(Product::Codex)
    );
    assert_eq!(
        SessionSource::Custom("atlas-dev".to_string()).restriction_product(),
        None
    );
}

#[test]
fn session_source_matches_product_restriction() {
    assert!(
        SessionSource::Custom("chatgpt".to_string())
            .matches_product_restriction(&[Product::Chatgpt])
    );
    assert!(
        !SessionSource::Custom("chatgpt".to_string())
            .matches_product_restriction(&[Product::Codex])
    );
    assert!(SessionSource::VSCode.matches_product_restriction(&[Product::Codex]));
    assert!(
        !SessionSource::Custom("atlas-dev".to_string())
            .matches_product_restriction(&[Product::Atlas])
    );
    assert!(SessionSource::Custom("atlas-dev".to_string()).matches_product_restriction(&[]));
}

fn sandbox_policy_probe_paths(policy: &SandboxPolicy, cwd: &Path) -> Vec<PathBuf> {
    let mut paths = vec![cwd.to_path_buf()];
    for root in policy.get_writable_roots_with_cwd(cwd) {
        paths.push(root.root.to_path_buf());
        paths.extend(
            root.read_only_subpaths
                .into_iter()
                .map(|path| path.to_path_buf()),
        );
    }
    paths.sort();
    paths.dedup();
    paths
}

fn assert_same_sandbox_policy_semantics(
    expected: &SandboxPolicy,
    actual: &SandboxPolicy,
    cwd: &Path,
) {
    assert_eq!(
        actual.has_full_disk_read_access(),
        expected.has_full_disk_read_access()
    );
    assert_eq!(
        actual.has_full_disk_write_access(),
        expected.has_full_disk_write_access()
    );
    assert_eq!(
        actual.has_full_network_access(),
        expected.has_full_network_access()
    );
    let mut probe_paths = sandbox_policy_probe_paths(expected, cwd);
    probe_paths.extend(sandbox_policy_probe_paths(actual, cwd));
    probe_paths.sort();
    probe_paths.dedup();

    for path in probe_paths {
        assert_eq!(
            sandbox_policy_allows_read(actual, &path, cwd),
            sandbox_policy_allows_read(expected, &path, cwd),
            "read access mismatch for {}",
            path.display()
        );
        assert_eq!(
            sandbox_policy_allows_write(actual, &path, cwd),
            sandbox_policy_allows_write(expected, &path, cwd),
            "write access mismatch for {}",
            path.display()
        );
    }
}

#[test]
fn external_sandbox_reports_full_access_flags() {
    let restricted = SandboxPolicy::ExternalSandbox {
        network_access: NetworkAccess::Restricted,
    };
    assert!(restricted.has_full_disk_write_access());
    assert!(!restricted.has_full_network_access());

    let enabled = SandboxPolicy::ExternalSandbox {
        network_access: NetworkAccess::Enabled,
    };
    assert!(enabled.has_full_disk_write_access());
    assert!(enabled.has_full_network_access());
}

#[test]
fn read_only_reports_network_access_flags() {
    let restricted = SandboxPolicy::new_read_only_policy();
    assert!(!restricted.has_full_network_access());

    let enabled = SandboxPolicy::ReadOnly {
        network_access: true,
    };
    assert!(enabled.has_full_network_access());
}

#[test]
fn granular_approval_config_mcp_elicitation_flag_is_field_driven() {
    assert!(
        GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            skill_approval: false,
            request_permissions: false,
            mcp_elicitations: true,
        }
        .allows_mcp_elicitations()
    );
    assert!(
        !GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            skill_approval: false,
            request_permissions: false,
            mcp_elicitations: false,
        }
        .allows_mcp_elicitations()
    );
}

#[test]
fn granular_approval_config_skill_approval_flag_is_field_driven() {
    assert!(
        GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            skill_approval: true,
            request_permissions: false,
            mcp_elicitations: false,
        }
        .allows_skill_approval()
    );
    assert!(
        !GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            skill_approval: false,
            request_permissions: false,
            mcp_elicitations: false,
        }
        .allows_skill_approval()
    );
}

#[test]
fn granular_approval_config_request_permissions_flag_is_field_driven() {
    assert!(
        GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            skill_approval: false,
            request_permissions: true,
            mcp_elicitations: false,
        }
        .allows_request_permissions()
    );
    assert!(
        !GranularApprovalConfig {
            sandbox_approval: false,
            rules: false,
            skill_approval: false,
            request_permissions: false,
            mcp_elicitations: false,
        }
        .allows_request_permissions()
    );
}

#[test]
fn granular_approval_config_defaults_missing_optional_flags_to_false() {
    let decoded = serde_json::from_value::<GranularApprovalConfig>(serde_json::json!({
        "sandbox_approval": true,
        "rules": false,
        "mcp_elicitations": true,
    }))
    .expect("granular approval config should deserialize");

    assert_eq!(
        decoded,
        GranularApprovalConfig {
            sandbox_approval: true,
            rules: false,
            skill_approval: false,
            request_permissions: false,
            mcp_elicitations: true,
        }
    );
}

#[test]
fn restricted_file_system_policy_reports_full_access_from_root_entries() {
    let read_only = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Read,
        missing_path_behavior: None,
    }]);
    assert!(read_only.has_full_disk_read_access());
    assert!(!read_only.has_full_disk_write_access());
    assert!(!read_only.include_platform_defaults());

    let writable = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Write,
        missing_path_behavior: None,
    }]);
    assert!(writable.has_full_disk_read_access());
    assert!(writable.has_full_disk_write_access());
}

#[test]
fn restricted_file_system_policy_treats_root_with_carveouts_as_scoped_access() {
    let cwd = TempDir::new().expect("tempdir");
    let canonical_cwd = codex_utils_absolute_path::canonicalize_preserving_symlinks(cwd.path())
        .expect("canonicalize cwd");
    let root = AbsolutePathBuf::from_absolute_path(&canonical_cwd)
        .expect("absolute canonical tempdir")
        .as_path()
        .ancestors()
        .last()
        .and_then(|path| AbsolutePathBuf::from_absolute_path(path).ok())
        .expect("filesystem root");
    let blocked = AbsolutePathBuf::resolve_path_against_base("blocked", cwd.path());
    let expected_blocked = AbsolutePathBuf::from_absolute_path(
        codex_utils_absolute_path::canonicalize_preserving_symlinks(cwd.path())
            .expect("canonicalize cwd")
            .join("blocked"),
    )
    .expect("canonical blocked");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: blocked.into(),
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);

    assert!(!policy.has_full_disk_read_access());
    assert!(!policy.has_full_disk_write_access());
    assert_eq!(
        policy.get_readable_roots_with_cwd(cwd.path()),
        vec![root.clone()]
    );
    assert_eq!(
        policy.get_unreadable_roots_with_cwd(cwd.path()),
        vec![expected_blocked.clone()]
    );

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, root);
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_blocked.as_path())
    );
}

#[test]
fn restricted_file_system_policy_derives_effective_paths() {
    let cwd = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(cwd.path().join(".agents")).expect("create .agents");
    std::fs::create_dir_all(cwd.path().join(".codex")).expect("create .codex");
    let canonical_cwd = codex_utils_absolute_path::canonicalize_preserving_symlinks(cwd.path())
        .expect("canonicalize cwd");
    let cwd_absolute =
        AbsolutePathBuf::from_absolute_path(&canonical_cwd).expect("absolute tempdir");
    let secret = AbsolutePathBuf::resolve_path_against_base("secret", cwd.path());
    let expected_secret = AbsolutePathBuf::from_absolute_path(canonical_cwd.join("secret"))
        .expect("canonical secret");
    let expected_agents = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".agents"))
        .expect("canonical .agents");
    let expected_codex = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".codex"))
        .expect("canonical .codex");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: secret.into(),
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);

    assert!(!policy.has_full_disk_read_access());
    assert!(!policy.has_full_disk_write_access());
    assert!(policy.include_platform_defaults());
    assert_eq!(
        policy.get_readable_roots_with_cwd(cwd.path()),
        vec![cwd_absolute.clone()]
    );
    assert_eq!(
        policy.get_unreadable_roots_with_cwd(cwd.path()),
        vec![expected_secret.clone()]
    );

    let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
    assert_eq!(writable_roots.len(), 1);
    assert_eq!(writable_roots[0].root, cwd_absolute);
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_secret.as_path())
    );
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_agents.as_path())
    );
    assert!(
        writable_roots[0]
            .read_only_subpaths
            .iter()
            .any(|path| path.as_path() == expected_codex.as_path())
    );
}

#[test]
fn restricted_file_system_policy_treats_read_entries_as_read_only_subpaths() {
    let cwd = TempDir::new().expect("tempdir");
    let canonical_cwd = codex_utils_absolute_path::canonicalize_preserving_symlinks(cwd.path())
        .expect("canonicalize cwd");
    let docs = AbsolutePathBuf::resolve_path_against_base("docs", cwd.path());
    let docs_public = AbsolutePathBuf::resolve_path_against_base("docs/public", cwd.path());
    let expected_docs =
        AbsolutePathBuf::from_absolute_path(canonical_cwd.join("docs")).expect("canonical docs");
    let expected_docs_public =
        AbsolutePathBuf::from_absolute_path(canonical_cwd.join("docs/public"))
            .expect("canonical docs/public");
    let expected_dot_codex = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".codex"))
        .expect("canonical .codex");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: docs.into(),
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: docs_public.into(),
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
    ]);

    assert!(!policy.has_full_disk_write_access());
    assert_eq!(
        sorted_writable_roots(policy.get_writable_roots_with_cwd(cwd.path())),
        vec![
            (
                canonical_cwd,
                vec![
                    expected_dot_codex.to_path_buf(),
                    expected_docs.to_path_buf()
                ],
            ),
            (expected_docs_public.to_path_buf(), Vec::new()),
        ]
    );
}

#[test]
fn file_system_policy_rejects_legacy_bridge_for_non_workspace_writes() {
    let cwd = if cfg!(windows) {
        Path::new(r"C:\workspace")
    } else {
        Path::new("/tmp/workspace")
    };
    let external_write_path = if cfg!(windows) {
        AbsolutePathBuf::from_absolute_path(r"C:\temp").expect("absolute windows temp path")
    } else {
        AbsolutePathBuf::from_absolute_path("/tmp").expect("absolute tmp path")
    };
    let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: external_write_path.into(),
        },
        access: FileSystemAccessMode::Write,
        missing_path_behavior: None,
    }]);

    let err = policy
        .to_legacy_sandbox_policy(NetworkSandboxPolicy::Restricted, cwd)
        .expect_err("non-workspace writes should be rejected");

    assert!(
        err.to_string()
            .contains("filesystem writes outside the workspace root"),
        "{err}"
    );
}

#[test]
fn legacy_sandbox_policy_semantics_survive_split_bridge() {
    let cwd = TempDir::new().expect("tempdir");
    let writable_root = AbsolutePathBuf::resolve_path_against_base("writable", cwd.path());
    let policies = [
        SandboxPolicy::DangerFullAccess,
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Restricted,
        },
        SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Enabled,
        },
        SandboxPolicy::ReadOnly {
            network_access: false,
        },
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: false,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![writable_root],
            network_access: true,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: true,
        },
    ];

    for expected in policies {
        let actual =
            FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(&expected, cwd.path())
                .to_legacy_sandbox_policy(NetworkSandboxPolicy::from(&expected), cwd.path())
                .expect("legacy bridge should preserve legacy policy semantics");

        assert_same_sandbox_policy_semantics(&expected, &actual, cwd.path());
    }
}

#[test]
fn item_started_event_from_web_search_emits_begin_event() {
    let event = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        item: TurnItem::WebSearch(WebSearchItem {
            id: "search-1".into(),
            query: "find docs".into(),
            action: WebSearchAction::Search {
                query: Some("find docs".into()),
                queries: None,
            },
            results: None,
        }),
        started_at_ms: 0,
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::WebSearchBegin(event) => assert_eq!(event.call_id, "search-1"),
        _ => panic!("expected WebSearchBegin event"),
    }
}

#[test]
fn item_started_event_from_non_web_search_emits_no_legacy_events() {
    let event = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        item: TurnItem::UserMessage(UserMessageItem::new(&[])),
        started_at_ms: 0,
    };

    assert!(
        event
            .as_legacy_events(/*show_raw_agent_reasoning*/ false)
            .is_empty()
    );
}

#[test]
fn item_started_event_from_image_generation_emits_begin_event() {
    let event = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        item: TurnItem::ImageGeneration(ImageGenerationItem {
            id: "ig-1".into(),
            status: "in_progress".into(),
            revised_prompt: None,
            result: String::new(),
            saved_path: None,
        }),
        started_at_ms: 0,
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::ImageGenerationBegin(event) => assert_eq!(event.call_id, "ig-1"),
        _ => panic!("expected ImageGenerationBegin event"),
    }
}

#[test]
fn item_started_event_from_file_change_emits_patch_begin_event() {
    let event = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: 0,
        item: TurnItem::FileChange(FileChangeItem {
            id: "patch-1".into(),
            changes: [(
                PathBuf::from("new.txt"),
                FileChange::Add {
                    content: "hello".into(),
                },
            )]
            .into_iter()
            .collect(),
            status: None,
            auto_approved: Some(true),
            stdout: None,
            stderr: None,
        }),
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::PatchApplyBegin(event) => {
            assert_eq!(event.call_id, "patch-1");
            assert_eq!(event.turn_id, "turn-1");
            assert!(event.auto_approved);
            assert!(event.changes.contains_key(&PathBuf::from("new.txt")));
        }
        _ => panic!("expected PatchApplyBegin event"),
    }
}

#[test]
fn item_started_event_from_mcp_tool_call_emits_begin_event() {
    let event = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: 0,
        item: TurnItem::McpToolCall(McpToolCallItem {
            id: "mcp-1".into(),
            server: "server".into(),
            tool: "tool".into(),
            arguments: json!({"arg": "value"}),
            connector_id: Some("connector".into()),
            mcp_app_resource_uri: Some("app://connector".into()),
            mcp_app_ui: None,
            link_id: Some("link_123".into()),
            app_name: Some("Calendar".into()),
            action_name: Some("create_event".into()),
            plugin_id: Some("sample@test".into()),
            read_only_hint: Some(false),
            status: McpToolCallStatus::InProgress,
            result: None,
            error: None,
            duration: None,
        }),
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::McpToolCallBegin(event) => {
            assert_eq!(event.turn_id, "turn-1");
            assert_eq!(event.call_id, "mcp-1");
            assert_eq!(event.invocation.server, "server");
            assert_eq!(event.invocation.tool, "tool");
            assert_eq!(event.connector_id.as_deref(), Some("connector"));
            assert_eq!(
                event.mcp_app_resource_uri.as_deref(),
                Some("app://connector")
            );
            assert_eq!(event.link_id.as_deref(), Some("link_123"));
            assert_eq!(event.app_name.as_deref(), Some("Calendar"));
            assert_eq!(event.action_name.as_deref(), Some("create_event"));
            assert_eq!(event.plugin_id.as_deref(), Some("sample@test"));
            assert_eq!(event.read_only_hint, Some(false));
        }
        _ => panic!("expected McpToolCallBegin event"),
    }
}

#[test]
fn item_completed_event_from_image_generation_emits_end_event() {
    let event = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        item: TurnItem::ImageGeneration(ImageGenerationItem {
            id: "ig-1".into(),
            status: "completed".into(),
            revised_prompt: Some("A tiny blue square".into()),
            result: "Zm9v".into(),
            saved_path: Some(test_path_buf("/tmp/ig-1.png").abs()),
        }),
        started_at_ms: Some(0),
        completed_at_ms: 0,
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::ImageGenerationEnd(event) => {
            assert_eq!(event.call_id, "ig-1");
            assert_eq!(event.status, "completed");
            assert_eq!(event.revised_prompt.as_deref(), Some("A tiny blue square"));
            assert_eq!(
                event.image,
                GeneratedImageRef::Inline {
                    result: "Zm9v".to_string(),
                }
            );
            assert_eq!(
                event.saved_path.as_ref().map(AbsolutePathBuf::as_path),
                Some(test_path_buf("/tmp/ig-1.png").as_path())
            );
        }
        _ => panic!("expected ImageGenerationEnd event"),
    }
}

#[test]
fn item_completed_event_from_file_change_emits_patch_end_event() {
    let event = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: Some(0),
        completed_at_ms: 0,
        item: TurnItem::FileChange(FileChangeItem {
            id: "patch-1".into(),
            changes: [(
                PathBuf::from("new.txt"),
                FileChange::Add {
                    content: "hello".into(),
                },
            )]
            .into_iter()
            .collect(),
            status: Some(PatchApplyStatus::Completed),
            auto_approved: None,
            stdout: Some("Done!".into()),
            stderr: Some(String::new()),
        }),
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::PatchApplyEnd(event) => {
            assert_eq!(event.call_id, "patch-1");
            assert_eq!(event.turn_id, "turn-1");
            assert_eq!(event.stdout, "Done!");
            assert!(event.success);
            assert_eq!(event.status, PatchApplyStatus::Completed);
            assert!(event.changes.contains_key(&PathBuf::from("new.txt")));
        }
        _ => panic!("expected PatchApplyEnd event"),
    }
}

#[test]
fn item_completed_event_from_mcp_tool_call_emits_end_event() {
    let event = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: Some(0),
        completed_at_ms: 0,
        item: TurnItem::McpToolCall(McpToolCallItem {
            id: "mcp-1".into(),
            server: "server".into(),
            tool: "tool".into(),
            arguments: json!({"arg": "value"}),
            connector_id: Some("connector".into()),
            mcp_app_resource_uri: Some("app://connector".into()),
            mcp_app_ui: None,
            link_id: Some("link_123".into()),
            app_name: Some("Calendar".into()),
            action_name: Some("create_event".into()),
            plugin_id: Some("sample@test".into()),
            read_only_hint: None,
            status: McpToolCallStatus::Completed,
            result: Some(CallToolResult {
                content: vec![json!({"type": "text", "text": "ok"})],
                structured_content: None,
                is_error: Some(false),
                meta: None,
            }),
            error: None,
            duration: Some(Duration::from_millis(42)),
        }),
    };

    let legacy_events = event.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    assert_eq!(legacy_events.len(), 1);
    match &legacy_events[0] {
        EventMsg::McpToolCallEnd(event) => {
            assert_eq!(event.turn_id, "turn-1");
            assert_eq!(event.call_id, "mcp-1");
            assert_eq!(event.invocation.server, "server");
            assert_eq!(event.invocation.tool, "tool");
            assert_eq!(event.connector_id.as_deref(), Some("connector"));
            assert_eq!(
                event.mcp_app_resource_uri.as_deref(),
                Some("app://connector")
            );
            assert_eq!(event.link_id.as_deref(), Some("link_123"));
            assert_eq!(event.app_name.as_deref(), Some("Calendar"));
            assert_eq!(event.action_name.as_deref(), Some("create_event"));
            assert_eq!(event.plugin_id.as_deref(), Some("sample@test"));
            assert_eq!(event.duration, Duration::from_millis(42));
            assert!(event.is_success());
        }
        _ => panic!("expected McpToolCallEnd event"),
    }
}

#[test]
fn command_execution_item_lifecycle_emits_legacy_exec_events() {
    let cwd = PathUri::from_abs_path(&test_path_buf("/tmp").abs());
    let started = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: 10,
        item: TurnItem::CommandExecution(CommandExecutionItem {
            model_context: None,
            sandbox_type: None,
            id: "exec-1".into(),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            process_id: Some("pid-1".into()),
            command: vec!["echo".into(), "done".into()],
            cwd: cwd.clone(),
            parsed_cmd: vec![ParsedCommand::Unknown {
                cmd: "echo done".into(),
            }],
            source: ExecCommandSource::Agent,
            interaction_input: None,
            status: CommandExecutionStatus::InProgress,
            aggregated_output: None,
            exit_code: None,
            duration: None,
        }),
    };
    let completed = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: Some(10),
        completed_at_ms: 20,
        item: TurnItem::CommandExecution(CommandExecutionItem {
            model_context: None,
            sandbox_type: None,
            id: "exec-1".into(),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            process_id: Some("pid-1".into()),
            command: vec!["echo".into(), "done".into()],
            cwd,
            parsed_cmd: vec![ParsedCommand::Unknown {
                cmd: "echo done".into(),
            }],
            source: ExecCommandSource::Agent,
            interaction_input: None,
            status: CommandExecutionStatus::Completed,
            aggregated_output: Some("done\n".into()),
            exit_code: Some(0),
            duration: Some(Duration::from_millis(5)),
        }),
    };

    assert!(matches!(
        started.as_legacy_events(/*show_raw_agent_reasoning*/ false).as_slice(),
        [EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
            call_id,
            plugin_id,
            script_path,
            turn_id,
            started_at_ms: 10,
            ..
        })] if call_id == "exec-1"
            && plugin_id.as_deref() == Some("sample@openai-curated")
            && script_path.as_deref() == Some("scripts/run.py")
            && turn_id == "turn-1"
    ));
    assert!(matches!(
        completed
            .as_legacy_events(/*show_raw_agent_reasoning*/ false)
            .as_slice(),
        [EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id,
            plugin_id,
            script_path,
            turn_id,
            completed_at_ms: 20,
            aggregated_output,
            ..
        })] if call_id == "exec-1"
            && plugin_id.as_deref() == Some("sample@openai-curated")
            && script_path.as_deref() == Some("scripts/run.py")
            && turn_id == "turn-1"
            && aggregated_output == "done\n"
    ));
}

#[test]
fn dynamic_tool_call_item_lifecycle_emits_legacy_dynamic_tool_events() {
    let started = ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: 10,
        item: TurnItem::DynamicToolCall(DynamicToolCallItem {
            id: "dynamic-1".into(),
            namespace: Some("apps".into()),
            tool: "lookup".into(),
            arguments: json!({"id": "123"}),
            status: DynamicToolCallStatus::InProgress,
            content_items: None,
            success: None,
            error: None,
            duration: None,
        }),
    };
    let completed = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: Some(10),
        completed_at_ms: 20,
        item: TurnItem::DynamicToolCall(DynamicToolCallItem {
            id: "dynamic-1".into(),
            namespace: Some("apps".into()),
            tool: "lookup".into(),
            arguments: json!({"id": "123"}),
            status: DynamicToolCallStatus::Completed,
            content_items: Some(vec![DynamicToolCallOutputContentItem::InputText {
                text: "ok".into(),
            }]),
            success: Some(true),
            error: None,
            duration: Some(Duration::from_millis(5)),
        }),
    };

    assert!(matches!(
        started.as_legacy_events(/*show_raw_agent_reasoning*/ false).as_slice(),
        [EventMsg::DynamicToolCallRequest(DynamicToolCallRequest {
            call_id,
            turn_id,
            started_at_ms: 10,
            ..
        })] if call_id == "dynamic-1" && turn_id == "turn-1"
    ));
    assert!(matches!(
        completed
            .as_legacy_events(/*show_raw_agent_reasoning*/ false)
            .as_slice(),
        [EventMsg::DynamicToolCallResponse(DynamicToolCallResponseEvent {
            call_id,
            turn_id,
            completed_at_ms: 20,
            success: true,
            ..
        })] if call_id == "dynamic-1" && turn_id == "turn-1"
    ));
}

#[test]
fn review_mode_item_completion_emits_legacy_events_with_ids() {
    let entered = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: Some(0),
        completed_at_ms: 0,
        item: TurnItem::EnteredReviewMode(EnteredReviewModeItem {
            id: "entered-review".into(),
            target: ReviewTarget::Custom {
                instructions: "review this".into(),
            },
            user_facing_hint: "Review requested.".into(),
        }),
    };
    let exited = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        started_at_ms: Some(0),
        completed_at_ms: 0,
        item: TurnItem::ExitedReviewMode(ExitedReviewModeItem {
            id: "exited-review".into(),
            review_output: Some(ReviewOutputEvent {
                overall_explanation: "Looks good.".into(),
                ..Default::default()
            }),
        }),
    };

    assert!(matches!(
        entered
            .as_legacy_events(/*show_raw_agent_reasoning*/ false)
            .as_slice(),
        [EventMsg::EnteredReviewMode(EnteredReviewModeEvent {
            target: ReviewTarget::Custom { instructions },
            user_facing_hint: Some(user_facing_hint),
            turn_id: Some(turn_id),
            item_id: Some(item_id),
        })]
            if instructions == "review this"
                && user_facing_hint == "Review requested."
                && turn_id == "turn-1"
                && item_id == "entered-review"
    ));
    assert!(matches!(
        exited
            .as_legacy_events(/*show_raw_agent_reasoning*/ false)
            .as_slice(),
        [EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
            turn_id: Some(turn_id),
            item_id: Some(item_id),
            review_output: Some(review_output),
        })]
            if turn_id == "turn-1"
                && item_id == "exited-review"
                && review_output.overall_explanation == "Looks good."
    ));
}

#[test]
fn item_started_event_requires_started_at_ms() {
    let mut value = serde_json::to_value(ItemStartedEvent {
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        item: TurnItem::UserMessage(UserMessageItem::new(&[])),
        started_at_ms: 123,
    })
    .unwrap();
    value.as_object_mut().unwrap().remove("started_at_ms");

    assert!(serde_json::from_value::<ItemStartedEvent>(value).is_err());
}

#[test]
fn item_completed_event_defaults_missing_completed_at_ms() {
    let mut value = serde_json::to_value(ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-1".into(),
        item: TurnItem::UserMessage(UserMessageItem::new(&[])),
        started_at_ms: None,
        completed_at_ms: 123,
    })
    .unwrap();
    value.as_object_mut().unwrap().remove("completed_at_ms");

    let event = serde_json::from_value::<ItemCompletedEvent>(value).unwrap();
    assert_eq!(event.started_at_ms, None);
    assert_eq!(event.completed_at_ms, 0);
}

#[test]
fn review_mode_events_deserialize_legacy_payloads() {
    let entered = serde_json::from_value::<EnteredReviewModeEvent>(json!({
        "target": {
            "type": "custom",
            "instructions": "review this"
        },
        "user_facing_hint": "hint"
    }))
    .unwrap();
    assert_eq!(entered.turn_id, None);
    assert_eq!(entered.item_id, None);

    let exited = serde_json::from_value::<ExitedReviewModeEvent>(json!({
        "review_output": null
    }))
    .unwrap();
    assert_eq!(exited.turn_id, None);
    assert_eq!(exited.item_id, None);
}

#[test]
fn rollback_failed_error_does_not_affect_turn_status() {
    let event = ErrorEvent {
        misalignment: None,
        message: "rollback failed".into(),
        codex_error_info: Some(CodexErrorInfo::ThreadRollbackFailed),
        rejected_audio_file_ids: Vec::new(),
    };
    assert!(!event.affects_turn_status());
}

#[test]
fn active_turn_not_steerable_error_does_not_affect_turn_status() {
    let event = ErrorEvent {
        misalignment: None,
        message: "cannot steer a review turn".into(),
        codex_error_info: Some(CodexErrorInfo::ActiveTurnNotSteerable {
            turn_kind: NonSteerableTurnKind::Review,
        }),
        rejected_audio_file_ids: Vec::new(),
    };
    assert!(!event.affects_turn_status());
}

#[test]
fn generic_error_affects_turn_status() {
    let event = ErrorEvent {
        misalignment: None,
        message: "generic".into(),
        codex_error_info: Some(CodexErrorInfo::Other),
        rejected_audio_file_ids: Vec::new(),
    };
    assert!(event.affects_turn_status());
}

#[test]
fn misalignment_explanation_and_steer_are_never_serialized_into_error_events() {
    let event = ErrorEvent {
        message: "This request violated the misalignment policy.".to_string(),
        codex_error_info: Some(CodexErrorInfo::MisalignmentPolicyViolation),
        rejected_audio_file_ids: Vec::new(),
        misalignment: Some(MisalignmentErrorDetails {
            error_type: Some("unauthorized_data_transfer".to_string()),
            detailed_explanation: Some("Sensitive customer explanation".to_string()),
            review_target: Some("sensitive-review-target".to_string()),
            steer: Some(MisalignmentSteer {
                message: "Sensitive customer steering".to_string(),
            }),
        }),
    };

    let serialized = serde_json::to_value(&event).expect("serialize error event");
    assert_eq!(
        serialized,
        json!({
            "message": "This request violated the misalignment policy.",
            "codex_error_info": "misalignment_policy_violation"
        })
    );
    let restored: ErrorEvent =
        serde_json::from_value(serialized).expect("deserialize persisted error event");
    assert_eq!(restored.misalignment, None);

    let debug = format!("{event:?}");
    assert!(!debug.contains("Sensitive customer explanation"));
    assert!(!debug.contains("Sensitive customer steering"));
    assert!(!debug.contains("sensitive-review-target"));
}

#[test]
fn rejected_audio_identity_round_trips_for_host_recovery() {
    let event = ErrorEvent {
        message: "invalid recording".to_string(),
        codex_error_info: Some(CodexErrorInfo::BadRequest),
        rejected_audio_file_ids: vec!["file_audio".to_string()],
        misalignment: None,
    };

    let serialized = serde_json::to_value(&event).expect("serialize error event");
    assert_eq!(serialized["rejected_audio_file_ids"], json!(["file_audio"]));
    let restored: ErrorEvent = serde_json::from_value(serialized).expect("deserialize error event");
    assert_eq!(restored.rejected_audio_file_ids, vec!["file_audio"]);
}

#[test]
fn realtime_conversation_started_event_uses_realtime_session_id() {
    let event = RealtimeConversationStartedEvent {
        realtime_session_id: Some("conv_1".to_string()),
        version: RealtimeConversationVersion::V2,
    };

    assert_eq!(
        serde_json::to_value(&event).unwrap(),
        json!({
            "realtime_session_id": "conv_1",
            "version": "v2"
        })
    );
}

#[test]
fn realtime_voice_list_is_stable() {
    assert_eq!(
        RealtimeVoicesList::builtin(),
        RealtimeVoicesList {
            v1: vec![
                RealtimeVoice::Juniper,
                RealtimeVoice::Maple,
                RealtimeVoice::Spruce,
                RealtimeVoice::Ember,
                RealtimeVoice::Vale,
                RealtimeVoice::Breeze,
                RealtimeVoice::Arbor,
                RealtimeVoice::Sol,
                RealtimeVoice::Cove,
            ],
            v2: vec![
                RealtimeVoice::Alloy,
                RealtimeVoice::Ash,
                RealtimeVoice::Ballad,
                RealtimeVoice::Coral,
                RealtimeVoice::Echo,
                RealtimeVoice::Sage,
                RealtimeVoice::Shimmer,
                RealtimeVoice::Verse,
                RealtimeVoice::Marin,
                RealtimeVoice::Cedar,
            ],
            v3: RealtimeVoicesList::builtin().v3,
            default_v1: RealtimeVoice::Cove,
            default_v2: RealtimeVoice::Marin,
        }
    );
}

#[test]
fn user_input_text_serializes_empty_text_elements() -> Result<()> {
    let input = crate::user_input::UserInput::Text {
        text: "hello".to_string(),
        text_elements: Vec::new(),
    };

    let json_input = serde_json::to_value(input)?;
    assert_eq!(
        json_input,
        json!({
            "type": "text",
            "text": "hello",
            "text_elements": [],
        })
    );

    Ok(())
}

#[test]
fn user_message_event_serializes_empty_metadata_vectors() -> Result<()> {
    let event = UserMessageEvent {
        client_id: None,
        message: "hello".to_string(),
        images: None,
        local_images: Vec::new(),
        text_elements: Vec::new(),
        ..Default::default()
    };

    let json_event = serde_json::to_value(event)?;
    assert_eq!(
        json_event,
        json!({
            "message": "hello",
            "local_images": [],
            "local_audio": [],
            "text_elements": [],
        })
    );

    Ok(())
}

#[test]
fn user_message_event_deserializes_without_image_detail_fields() -> Result<()> {
    let event: UserMessageEvent = serde_json::from_value(json!({
        "message": "hello",
        "images": ["https://example.com/image.png"],
        "local_images": ["/tmp/local.png"],
        "text_elements": [],
    }))?;

    assert_eq!(event.message, "hello");
    assert_eq!(
        event.images,
        Some(vec!["https://example.com/image.png".to_string()])
    );
    assert_eq!(event.image_details, Vec::<Option<ImageDetail>>::new());
    assert_eq!(event.file_ids, None);
    assert_eq!(event.file_id_details, Vec::<Option<ImageDetail>>::new());
    assert_eq!(event.image_order, Vec::<UserMessageImageKind>::new());
    assert_eq!(event.local_images, vec![PathBuf::from("/tmp/local.png")]);
    assert_eq!(event.local_image_details, Vec::<Option<ImageDetail>>::new());
    assert_eq!(event.audio, None);
    assert_eq!(event.local_audio, Vec::<PathBuf>::new());
    assert_eq!(event.text_elements, Vec::new());

    Ok(())
}

#[test]
fn user_message_item_legacy_event_preserves_attachments() {
    let local_path = PathBuf::from("/tmp/local.png");
    let local_audio_path = PathBuf::from("/tmp/local.wav");
    let mut item = UserMessageItem::new(&[
        crate::user_input::UserInput::Image {
            image: crate::models::ImageReference::Inline {
                image_url: "https://example.com/first.png".to_string(),
            },
            detail: Some(ImageDetail::Original),
        },
        crate::user_input::UserInput::Image {
            image: crate::models::ImageReference::File {
                file_id: "file_123".to_string(),
            },
            detail: Some(ImageDetail::Low),
        },
        crate::user_input::UserInput::Image {
            image: crate::models::ImageReference::Inline {
                image_url: "https://example.com/second.png".to_string(),
            },
            detail: None,
        },
        crate::user_input::UserInput::LocalImage {
            path: local_path.clone(),
            detail: Some(ImageDetail::Original),
        },
        crate::user_input::UserInput::Audio {
            audio: AudioReference::Inline {
                audio_url: "https://example.com/remote.mp3".to_string(),
            },
        },
        crate::user_input::UserInput::Audio {
            audio: AudioReference::File {
                file_id: "file_audio".to_string(),
            },
        },
        crate::user_input::UserInput::LocalAudio {
            path: local_audio_path.clone(),
        },
    ]);
    item.client_id = Some("client-message-1".to_string());

    let EventMsg::UserMessage(event) = item.as_legacy_event() else {
        panic!("expected user message event");
    };
    let event_json = serde_json::to_value(&event).expect("serialize user message event");

    assert_eq!(
        event.images,
        Some(vec![
            "https://example.com/first.png".to_string(),
            "https://example.com/second.png".to_string(),
        ])
    );
    assert_eq!(event.client_id, Some("client-message-1".to_string()));
    assert_eq!(event.image_details, vec![Some(ImageDetail::Original)]);
    assert_eq!(event.file_ids, Some(vec!["file_123".to_string()]));
    assert_eq!(event.file_id_details, vec![Some(ImageDetail::Low)]);
    assert_eq!(
        event.image_order,
        vec![
            UserMessageImageKind::Inline,
            UserMessageImageKind::File,
            UserMessageImageKind::Inline,
        ]
    );
    assert_eq!(event_json["file_ids"], json!(["file_123"]));
    assert_eq!(event_json["file_id_details"], json!(["low"]));
    assert_eq!(
        event_json["image_order"],
        json!(["inline", "file", "inline"])
    );
    assert_eq!(event.local_images, vec![local_path]);
    assert_eq!(event.local_image_details, vec![Some(ImageDetail::Original)]);
    assert_eq!(
        event.audio,
        Some(vec!["https://example.com/remote.mp3".to_string()])
    );
    assert_eq!(event.local_audio, vec![local_audio_path]);
    assert_eq!(
        event.audio_references,
        Some(vec![AudioReference::File {
            file_id: "file_audio".to_string(),
        }])
    );
    assert_eq!(
        event.audio_order,
        vec![UserMessageAudioKind::Inline, UserMessageAudioKind::File]
    );
    // Earlier readers ignore the new ordered field and retain inline recordings.
    #[derive(Deserialize)]
    struct EarlierUserMessageEvent {
        audio: Option<Vec<String>>,
    }
    let earlier: EarlierUserMessageEvent = serde_json::from_value(event_json).unwrap();
    assert_eq!(earlier.audio, event.audio);
}

#[test]
fn audio_only_user_message_has_placeholder_preview() {
    for event in [
        UserMessageEvent {
            audio: Some(vec!["https://example.com/remote.mp3".to_string()]),
            ..Default::default()
        },
        UserMessageEvent {
            audio_references: Some(vec![AudioReference::File {
                file_id: "file_audio".into(),
            }]),
            ..Default::default()
        },
    ] {
        assert_eq!(user_message_preview(&event), Some("[Audio]".to_string()));
    }
}

#[test]
fn file_only_user_message_has_placeholder_preview() {
    let event = UserMessageEvent {
        file_ids: Some(vec!["file_123".to_string()]),
        ..Default::default()
    };

    assert_eq!(user_message_preview(&event), Some("[Image]".to_string()));
}

#[test]
fn turn_aborted_event_deserializes_without_turn_id() -> Result<()> {
    let event: EventMsg = serde_json::from_value(json!({
        "type": "turn_aborted",
        "reason": "interrupted",
    }))?;

    match event {
        EventMsg::TurnAborted(TurnAbortedEvent {
            turn_id,
            root_turn_id,
            reason,
            ..
        }) => {
            assert_eq!((turn_id, root_turn_id), (None, None));
            assert_eq!(reason, TurnAbortReason::Interrupted);
        }
        _ => panic!("expected turn_aborted event"),
    }

    Ok(())
}

#[test]
fn session_meta_defaults_legacy_history_mode() -> Result<()> {
    let session_meta: SessionMeta = serde_json::from_value(json!({
        "session_id": "00000000-0000-0000-0000-000000000001",
        "id": "00000000-0000-0000-0000-000000000001",
        "timestamp": "2026-01-01T00:00:00Z",
        "cwd": "/tmp",
        "originator": "codex",
        "cli_version": "0.0.0",
        "model_provider": null,
        "base_instructions": null
    }))?;

    assert_eq!(session_meta.history_mode, ThreadHistoryMode::Legacy);
    assert_eq!(session_meta.history_base, None);
    assert_eq!(session_meta.forked_from_ordinal_exclusive, None);
    let serialized = serde_json::to_value(&session_meta)?;
    assert!(serialized.get("forked_from_ordinal_exclusive").is_none());
    assert_eq!(serialized["history_mode"], json!("legacy"));
    let mut unknown = serialized;
    unknown["history_mode"] = json!("future");
    assert!(serde_json::from_value::<SessionMeta>(unknown).is_err());
    Ok(())
}

#[test]
fn turn_context_item_deserializes_without_summary() -> Result<()> {
    let item: TurnContextItem = serde_json::from_value(json!({
        "cwd": test_path_buf("/tmp"),
        "approval_policy": "never",
        "sandbox_policy": { "type": "danger-full-access" },
        "model": "gpt-5",
    }))?;

    assert_eq!(item.summary, None);
    assert_eq!(item.permission_profile, PermissionProfile::Disabled);
    assert_eq!(item.comp_hash, None);
    assert_eq!(serde_json::to_value(item)?.get("summary"), None);
    Ok(())
}

#[test]
fn turn_context_item_deserializes_legacy_on_failure_as_on_request() -> Result<()> {
    let item: TurnContextItem = serde_json::from_value(json!({
        "cwd": test_path_buf("/tmp"),
        "approval_policy": "on-failure",
        "sandbox_policy": { "type": "danger-full-access" },
        "model": "gpt-5",
        "summary": "auto",
    }))?;

    assert_eq!(item.approval_policy, AskForApproval::OnRequest);
    Ok(())
}

#[test]
fn turn_context_item_normalizes_legacy_permissions_and_writes_profile_only() -> Result<()> {
    let cwd = test_path_buf("/tmp").abs();
    let file_system = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            pattern: "/tmp/private/**/*.txt".to_string(),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    }]);
    let mut legacy = json!({
        "cwd": cwd,
        "approval_policy": "never",
        "sandbox_policy": { "type": "danger-full-access" },
        "file_system_sandbox_policy": RawFileSystemSandboxPolicy::try_from(file_system.clone())
            .expect("serializable split policy"),
        "model": "gpt-5",
        "summary": "none",
    });
    let item: TurnContextItem = serde_json::from_value(legacy.clone())?;
    let profile =
        PermissionProfile::from_runtime_permissions(&file_system, NetworkSandboxPolicy::Enabled);
    assert_eq!(item.permission_profile, profile);
    let canonical = serde_json::to_value(&item)?;
    assert_eq!(
        canonical,
        json!({
            "cwd": cwd,
            "approval_policy": "never",
            "permission_profile": profile,
            "model": "gpt-5",
            "summary": "none",
        })
    );
    assert_eq!(
        serde_json::from_value::<TurnContextItem>(canonical.clone())?,
        item
    );

    legacy["file_system_sandbox_policy"]["entries"][0]["path"] =
        json!({ "type": "path", "path": "://" });
    assert_eq!(
        serde_json::from_value::<TurnContextItem>(legacy.clone())?.permission_profile,
        PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(Vec::new()),
            NetworkSandboxPolicy::Enabled,
        ),
    );
    legacy["permission_profile"] = serde_json::to_value(&profile)?;
    assert_eq!(serde_json::from_value::<TurnContextItem>(legacy)?, item);
    let mut missing_permissions = canonical;
    missing_permissions
        .as_object_mut()
        .expect("context object")
        .remove("permission_profile");
    assert!(serde_json::from_value::<TurnContextItem>(missing_permissions).is_err());
    Ok(())
}

/// Serialize Event to verify that its JSON representation has the expected
/// amount of nesting.
#[test]
fn serialize_event() -> Result<()> {
    let session_id = SessionId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c7")?;
    let thread_id = ThreadId::from_string("67e55044-10b1-426f-9247-bb680e5fe0c8")?;
    let rollout_file = NamedTempFile::new()?;
    let permission_profile = PermissionProfile::read_only();
    let event = Event {
        id: "1234".to_string(),
        msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
            session_id,
            thread_id,
            forked_from_id: None,
            parent_thread_id: None,
            thread_source: None,
            thread_name: None,
            model: "codex-mini-latest".to_string(),
            model_provider_id: "openai".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: permission_profile.clone(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            reasoning_effort: Some(ReasoningEffortConfig::default()),
            network_proxy: None,
            rollout_path: Some(rollout_file.path().to_path_buf()),
        }),
    };

    let expected = json!({
        "id": "1234",
        "msg": {
            "type": "session_configured",
            "session_id": "67e55044-10b1-426f-9247-bb680e5fe0c7",
            "thread_id": "67e55044-10b1-426f-9247-bb680e5fe0c8",
            "model": "codex-mini-latest",
            "model_provider_id": "openai",
            "approval_policy": "never",
            "approvals_reviewer": "user",
            "permission_profile": permission_profile,
            "cwd": test_path_buf("/home/user/project"),
            "reasoning_effort": "medium",
            "rollout_path": format!("{}", rollout_file.path().display()),
        }
    });
    assert_eq!(expected, serde_json::to_value(&event)?);
    Ok(())
}

#[test]
fn deserialize_legacy_session_configured_event_uses_sandbox_policy() -> Result<()> {
    let cwd = test_path_buf("/home/user/project");
    let value = json!({
        "session_id": "67e55044-10b1-426f-9247-bb680e5fe0c8",
        "model": "codex-mini-latest",
        "model_provider_id": "openai",
        "approval_policy": "never",
        "approvals_reviewer": "user",
        "sandbox_policy": {
            "type": "read-only"
        },
        "cwd": cwd,
    });

    let event: SessionConfiguredEvent = serde_json::from_value(value)?;
    assert_eq!(event.permission_profile, PermissionProfile::read_only());
    Ok(())
}

#[test]
fn vec_u8_as_base64_serialization_and_deserialization() -> Result<()> {
    let event = ExecCommandOutputDeltaEvent {
        call_id: "call21".to_string(),
        stream: ExecOutputStream::Stdout,
        chunk: vec![1, 2, 3, 4, 5],
    };
    let serialized = serde_json::to_string(&event)?;
    assert_eq!(
        r#"{"call_id":"call21","stream":"stdout","chunk":"AQIDBAU="}"#,
        serialized,
    );

    let deserialized: ExecCommandOutputDeltaEvent = serde_json::from_str(&serialized)?;
    assert_eq!(deserialized, event);
    Ok(())
}

#[test]
fn serialize_mcp_startup_update_event() -> Result<()> {
    let event = Event {
        id: "init".to_string(),
        msg: EventMsg::McpStartupUpdate(McpStartupUpdateEvent {
            server: "srv".to_string(),
            status: McpStartupStatus::Failed {
                error: "boom".to_string(),
                reason: Some(McpStartupFailureReason::ReauthenticationRequired),
            },
        }),
    };

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["msg"]["type"], "mcp_startup_update");
    assert_eq!(value["msg"]["server"], "srv");
    assert_eq!(value["msg"]["status"]["state"], "failed");
    assert_eq!(value["msg"]["status"]["error"], "boom");
    assert_eq!(
        value["msg"]["status"]["reason"],
        "reauthentication_required"
    );
    Ok(())
}

#[test]
fn serialize_mcp_startup_complete_event() -> Result<()> {
    let event = Event {
        id: "init".to_string(),
        msg: EventMsg::McpStartupComplete(McpStartupCompleteEvent {
            ready: vec!["a".to_string()],
            failed: vec![McpStartupFailure {
                server: "b".to_string(),
                error: "bad".to_string(),
            }],
            cancelled: vec!["c".to_string()],
        }),
    };

    let value = serde_json::to_value(&event)?;
    assert_eq!(value["msg"]["type"], "mcp_startup_complete");
    assert_eq!(value["msg"]["ready"][0], "a");
    assert_eq!(value["msg"]["failed"][0]["server"], "b");
    assert_eq!(value["msg"]["failed"][0]["error"], "bad");
    assert_eq!(value["msg"]["cancelled"][0], "c");
    Ok(())
}

#[test]
fn token_usage_info_new_or_append_updates_context_window_when_provided() {
    let initial = Some(TokenUsageInfo {
        total_token_usage: TokenUsage::default(),
        last_usage_includes_reasoning: None,
        last_token_usage: TokenUsage::default(),
        model_context_window: Some(258_400),
    });
    let last = TokenUsage {
        input_tokens: 10,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        total_tokens: 10,
        codex_rollout_budget_units: None,
    };

    let info = TokenUsageInfo::new_or_append(
        &initial,
        &last,
        /*includes_reasoning*/ true,
        Some(128_000),
    );

    assert_eq!(info.model_context_window, Some(128_000));
}

#[test]
fn token_usage_info_new_or_append_preserves_context_window_when_not_provided() {
    let initial = Some(TokenUsageInfo {
        total_token_usage: TokenUsage::default(),
        last_usage_includes_reasoning: None,
        last_token_usage: TokenUsage::default(),
        model_context_window: Some(258_400),
    });
    let last = TokenUsage {
        input_tokens: 10,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
        total_tokens: 10,
        codex_rollout_budget_units: None,
    };

    let info = TokenUsageInfo::new_or_append(
        &initial, &last, /*includes_reasoning*/ true, /*model_context_window*/ None,
    );

    assert_eq!(info.model_context_window, Some(258_400));
}
