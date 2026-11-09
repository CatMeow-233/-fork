use super::*;
use anyhow::Result;
use codex_execpolicy::Policy;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use tempfile::tempdir;

// A tiny valid PNG (1x1) so image conversion tests don't depend on cross-crate
// file paths, which break under Bazel sandboxing.
const TINY_PNG_BYTES: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2, 0, 0, 5, 0, 1,
    122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

#[test]
fn base_instructions_preserve_provenance_and_accept_legacy_rollouts() -> Result<()> {
    let legacy: BaseInstructions =
        serde_json::from_value(serde_json::json!({ "text": "legacy instructions" }))?;
    assert_eq!(legacy.provenance, None);

    for provenance in [
        BaseInstructionsProvenance::Custom,
        BaseInstructionsProvenance::Model {
            model: "gpt-5.2".to_string(),
        },
    ] {
        let instructions = BaseInstructions {
            text: "persisted instructions".to_string(),
            provenance: Some(provenance),
        };
        assert_eq!(
            serde_json::from_value::<BaseInstructions>(serde_json::to_value(&instructions)?)?,
            instructions
        );
    }

    Ok(())
}

#[test]
fn plaintext_agent_message_content_rejects_mixed_encrypted_content() {
    let content = vec![
        AgentMessageInputContent::InputText {
            text: "Message Type: MESSAGE\nPayload:\n".to_string(),
        },
        AgentMessageInputContent::EncryptedContent {
            encrypted_content: "encrypted-payload".to_string(),
        },
    ];

    assert_eq!(plaintext_agent_message_content(&content), None);
}

#[test]
fn response_input_message_conversion_preserves_phase() -> Result<()> {
    for phase in [
        None,
        Some(MessagePhase::Commentary),
        Some(MessagePhase::PartialAnswer),
        Some(MessagePhase::FinalAnswer),
    ] {
        let input = ResponseInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                annotations: None,
                logprobs: None,
                text: "answer text".to_string(),
            }],
            phase: phase.clone(),
        };
        let wire = serde_json::to_value(input)?;
        let item = ResponseItem::from(serde_json::from_value::<ResponseInputItem>(wire)?);
        let expected = ResponseItem::Message {
            status: None,
            encrypted_content: None,
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                annotations: None,
                logprobs: None,
                text: "answer text".to_string(),
            }],
            phase,
            internal_chat_message_metadata_passthrough: None,
        };
        assert_eq!(item, expected);
        assert_eq!(
            serde_json::from_value::<ResponseItem>(serde_json::to_value(item)?)?,
            expected
        );
    }
    Ok(())
}

#[test]
fn response_item_passthrough_metadata_round_trips_and_stamps_turn_ids() -> Result<()> {
    let mut item = response_item_with_passthrough_metadata(Some(passthrough_metadata("turn-1")));
    let round_trip: ResponseItem = serde_json::from_value(serde_json::to_value(&item)?)?;
    assert_eq!(round_trip, item);

    let unknown_metadata: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": "hello"}],
        "internal_chat_message_metadata_passthrough": {
            "turn_id": "turn-1",
            "other": "ignored",
        },
    }))?;
    assert_eq!(unknown_metadata, item);

    item.append_executed_tool_calls(vec![ExecutedToolCall::new(
        "test_tool".to_string(),
        serde_json::json!({"value": 1}),
    )]);
    let serialized = serde_json::to_value(&item)?;
    assert_eq!(
        serialized["internal_chat_message_metadata_passthrough"]["executed_tool_calls"],
        serde_json::json!([{"name": "test_tool", "arguments": {"value": 1}}]),
    );
    let deserialized = serde_json::from_value::<ResponseItem>(serialized)?;
    assert_eq!(deserialized.turn_id(), Some("turn-1"));
    assert!(
        deserialized
            .executed_tool_call_metadata()
            .and_then(|metadata| metadata.executed_tool_calls.as_ref())
            .is_none(),
        "provider and rollout input cannot forge local attempted-tool metadata",
    );
    item.clear_executed_tool_calls();
    item.set_turn_id_if_missing("turn-2");
    assert_eq!(item.turn_id(), Some("turn-1"));

    let mut empty_turn_id = response_item_with_passthrough_metadata(Some(passthrough_metadata("")));
    empty_turn_id.set_turn_id_if_missing("turn-1");
    assert_eq!(empty_turn_id.turn_id(), Some("turn-1"));

    let mut missing_turn_id = response_item_with_passthrough_metadata(
        /*internal_chat_message_metadata_passthrough*/ None,
    );
    let create_time = serde_json::Number::from(123);
    missing_turn_id.set_create_time_if_missing(create_time.clone());
    missing_turn_id.set_turn_id_if_missing("");
    missing_turn_id.set_turn_id_if_missing("turn-1");
    missing_turn_id.set_create_time_if_missing(serde_json::Number::from(456));
    assert_eq!(
        missing_turn_id.executed_tool_call_metadata(),
        Some(&InternalChatMessageMetadataPassthrough {
            turn_id: Some("turn-1".to_string()),
            create_time: Some(create_time),
            ..Default::default()
        })
    );

    let mut other = ResponseItem::Other;
    other.set_turn_id_if_missing("turn-1");
    assert_eq!(other.turn_id(), None);
    Ok(())
}

#[test]
fn response_item_id_getter_and_setter() {
    let mut item = response_item_with_passthrough_metadata(
        /*internal_chat_message_metadata_passthrough*/ None,
    );
    assert_eq!(item.id(), None);

    item.set_id(Some(ResponseItemId::with_suffix("msg", "test")));

    assert_eq!(item.id().map(ResponseItemId::as_str), Some("msg_test"));

    item.set_id(/*new_id*/ None);

    assert_eq!(item.id(), None);

    let mut additional_tools = ResponseItem::AdditionalTools {
        id: None,
        role: "developer".to_string(),
        tools: Arc::from([]),
    };
    additional_tools.set_id(Some(ResponseItemId::with_suffix("at", "test")));
    assert_eq!(
        additional_tools.id().map(ResponseItemId::as_str),
        Some("at_test")
    );
}

fn response_item_with_passthrough_metadata(
    internal_chat_message_metadata_passthrough: Option<InternalChatMessageMetadataPassthrough>,
) -> ResponseItem {
    ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "hello".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough,
    }
}

fn passthrough_metadata(turn_id: &str) -> InternalChatMessageMetadataPassthrough {
    InternalChatMessageMetadataPassthrough {
        turn_id: Some(turn_id.to_string()),
        ..Default::default()
    }
}

#[test]
fn image_detail_roundtrips_all_wire_values() -> Result<()> {
    assert_eq!(
        serde_json::from_str::<ImageDetail>("\"auto\"")?,
        ImageDetail::Auto
    );
    assert_eq!(
        serde_json::from_str::<ImageDetail>("\"low\"")?,
        ImageDetail::Low
    );
    assert_eq!(serde_json::to_string(&ImageDetail::Auto)?, "\"auto\"");
    assert_eq!(serde_json::to_string(&ImageDetail::Low)?, "\"low\"");

    let content_item: ContentItem = serde_json::from_value(serde_json::json!({
        "type": "input_image",
        "image_url": "data:image/png;base64,abc",
        "detail": "auto",
    }))?;

    assert_eq!(
        content_item,
        ContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,abc".to_string(),
            },
            detail: Some(ImageDetail::Auto),
        }
    );

    Ok(())
}

#[test]
fn sandbox_permissions_helpers_match_documented_semantics() {
    let cases = [
        (SandboxPermissions::UseDefault, false, false, false),
        (SandboxPermissions::RequireEscalated, true, true, false),
        (
            SandboxPermissions::WithAdditionalPermissions,
            false,
            true,
            true,
        ),
    ];

    for (
        sandbox_permissions,
        requires_escalated_permissions,
        requests_sandbox_override,
        uses_additional_permissions,
    ) in cases
    {
        assert_eq!(
            sandbox_permissions.requires_escalated_permissions(),
            requires_escalated_permissions
        );
        assert_eq!(
            sandbox_permissions.requests_sandbox_override(),
            requests_sandbox_override
        );
        assert_eq!(
            sandbox_permissions.uses_additional_permissions(),
            uses_additional_permissions
        );
    }
}

#[test]
fn convert_mcp_content_to_items_preserves_data_urls() {
    let contents = vec![serde_json::json!({
        "type": "image",
        "data": "data:image/png;base64,Zm9v",
        "mimeType": "image/png",
    })];

    let items = convert_mcp_content_to_items(&contents);
    assert_eq!(
        items,
        vec![FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,Zm9v".to_string(),
            },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        }]
    );
}

#[test]
fn response_item_parses_image_generation_call() {
    let item = serde_json::from_value::<ResponseItem>(serde_json::json!({
        "id": "ig_123",
        "type": "image_generation_call",
        "status": "completed",
        "revised_prompt": "A small blue square",
        "result": "Zm9v",
    }))
    .expect("image generation item should deserialize");

    assert_eq!(
        item,
        ResponseItem::ImageGenerationCall {
            encrypted_content: None,
            id: Some(ResponseItemId::with_suffix("ig", "123")),
            status: "completed".to_string(),
            revised_prompt: Some("A small blue square".to_string()),
            result: "Zm9v".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }
    );
}

#[test]
fn response_item_parses_image_generation_call_without_revised_prompt() {
    let item = serde_json::from_value::<ResponseItem>(serde_json::json!({
        "id": "ig_123",
        "type": "image_generation_call",
        "status": "completed",
        "result": "Zm9v",
    }))
    .expect("image generation item should deserialize");

    assert_eq!(
        item,
        ResponseItem::ImageGenerationCall {
            encrypted_content: None,
            id: Some(ResponseItemId::with_suffix("ig", "123")),
            status: "completed".to_string(),
            revised_prompt: None,
            result: "Zm9v".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }
    );
}

#[test]
fn additional_permission_profile_is_empty_when_all_fields_are_none() {
    assert_eq!(AdditionalPermissionProfile::default().is_empty(), true);
}

#[test]
fn additional_permission_profile_is_not_empty_when_field_is_present_but_nested_empty() {
    let permission_profile = AdditionalPermissionProfile {
        network: Some(NetworkPermissions { enabled: None }),
        file_system: None,
    };
    assert_eq!(permission_profile.is_empty(), false);
}

#[test]
fn permission_profile_round_trip_preserves_glob_scan_max_depth() {
    let mut file_system_sandbox_policy =
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::GlobPattern {
                pattern: "**/*.env".to_string(),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        }]);
    file_system_sandbox_policy.glob_scan_max_depth = Some(2);

    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Restricted,
    );

    assert_eq!(
        permission_profile.file_system_sandbox_policy(),
        file_system_sandbox_policy
    );
}

#[cfg(unix)]
#[test]
fn permission_profile_round_trip_preserves_ambiguous_native_denies() -> Result<()> {
    let denied_path = AbsolutePathBuf::try_from(PathBuf::from("/C:/secret"))?;
    let file_system_sandbox_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(denied_path.into(), FileSystemAccessMode::Deny),
    ]);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        NetworkSandboxPolicy::Restricted,
    );

    assert_eq!(
        serde_json::from_value::<PermissionProfile>(serde_json::to_value(&permission_profile)?)?,
        permission_profile
    );
    Ok(())
}

#[test]
fn permission_profile_deserializes_legacy_rollout_shape() -> Result<()> {
    let legacy = serde_json::json!({
        "network": {
            "enabled": true,
        },
        "file_system": {
            "entries": [{
                "path": {
                    "type": "special",
                    "value": {
                        "kind": "root",
                    },
                },
                "access": "write",
            }],
            "glob_scan_max_depth": 2,
        },
    });

    let permission_profile: PermissionProfile = serde_json::from_value(legacy)?;

    assert_eq!(
        permission_profile,
        PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                }],
                glob_scan_max_depth: NonZeroUsize::new(2),
            },
            network: NetworkSandboxPolicy::Enabled,
        }
    );
    Ok(())
}

#[test]
fn permission_profile_presets_match_legacy_defaults() {
    assert_eq!(
        PermissionProfile::read_only(),
        PermissionProfile::from_legacy_sandbox_policy(&SandboxPolicy::new_read_only_policy())
    );
    assert_eq!(
        PermissionProfile::workspace_write(),
        PermissionProfile::from_legacy_sandbox_policy(&SandboxPolicy::new_workspace_write_policy())
    );
}

#[test]
fn permission_profile_round_trip_preserves_disabled_sandbox() -> Result<()> {
    let cwd = tempdir()?;
    let permission_profile =
        PermissionProfile::from_legacy_sandbox_policy(&SandboxPolicy::DangerFullAccess);

    assert_eq!(permission_profile, PermissionProfile::Disabled);
    assert_eq!(
        permission_profile.to_legacy_sandbox_policy(cwd.path())?,
        SandboxPolicy::DangerFullAccess
    );
    assert_eq!(
        permission_profile.to_runtime_permissions(),
        (
            FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Enabled
        )
    );
    Ok(())
}

#[test]
fn disabled_permission_profile_ignores_runtime_network_policy() {
    let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
        SandboxEnforcement::Disabled,
        &FileSystemSandboxPolicy::unrestricted(),
        NetworkSandboxPolicy::Restricted,
    );

    assert_eq!(permission_profile, PermissionProfile::Disabled);
}

#[test]
fn permission_profile_from_runtime_permissions_preserves_external_sandbox() {
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::external_sandbox(),
        NetworkSandboxPolicy::Restricted,
    );

    assert_eq!(
        permission_profile,
        PermissionProfile::External {
            network: NetworkSandboxPolicy::Restricted,
        }
    );
    assert_eq!(
        PermissionProfile::from_runtime_permissions_with_enforcement(
            SandboxEnforcement::Managed,
            &FileSystemSandboxPolicy::external_sandbox(),
            NetworkSandboxPolicy::Restricted,
        ),
        permission_profile,
    );
}

#[test]
fn permission_profile_from_runtime_permissions_preserves_unrestricted_managed_network() {
    let permission_profile = PermissionProfile::from_runtime_permissions_with_enforcement(
        SandboxEnforcement::External,
        &FileSystemSandboxPolicy::unrestricted(),
        NetworkSandboxPolicy::Restricted,
    );

    assert_eq!(
        permission_profile,
        PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Restricted,
        },
        "the legacy ExternalSandbox projection must not hide a split unrestricted filesystem policy"
    );
    assert_eq!(
        permission_profile.to_runtime_permissions(),
        (
            FileSystemSandboxPolicy::unrestricted(),
            NetworkSandboxPolicy::Restricted,
        )
    );
}

#[test]
fn permission_profile_round_trip_preserves_external_sandbox() -> Result<()> {
    let cwd = tempdir()?;
    let sandbox_policy = SandboxPolicy::ExternalSandbox {
        network_access: crate::protocol::NetworkAccess::Restricted,
    };
    let permission_profile = PermissionProfile::from_legacy_sandbox_policy(&sandbox_policy);

    assert_eq!(
        permission_profile,
        PermissionProfile::External {
            network: NetworkSandboxPolicy::Restricted,
        }
    );
    assert_eq!(
        permission_profile.to_legacy_sandbox_policy(cwd.path())?,
        sandbox_policy
    );
    assert_eq!(
        permission_profile.to_runtime_permissions(),
        (
            FileSystemSandboxPolicy::external_sandbox(),
            NetworkSandboxPolicy::Restricted
        )
    );
    Ok(())
}

#[test]
fn file_system_permissions_with_glob_scan_depth_uses_canonical_json() -> Result<()> {
    let path = AbsolutePathBuf::try_from(PathBuf::from(if cfg!(windows) {
        r"C:\tmp\allowed"
    } else {
        "/tmp/allowed"
    }))
    .expect("absolute path");
    let file_system_permissions = FileSystemPermissions {
        entries: vec![FileSystemSandboxEntry {
            path: path.into(),
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        }],
        glob_scan_max_depth: NonZeroUsize::new(2),
    };

    let serialized = serde_json::to_value(&file_system_permissions)?;

    assert_eq!(serialized.get("read"), None);
    assert_eq!(serialized.get("write"), None);
    assert_eq!(
        serialized.get("glob_scan_max_depth"),
        Some(&serde_json::json!(2))
    );
    assert!(serialized.get("entries").is_some());
    assert_eq!(
        serde_json::from_value::<FileSystemPermissions>(serialized)?,
        file_system_permissions
    );
    Ok(())
}

#[test]
fn file_system_permissions_rejects_zero_glob_scan_depth() {
    serde_json::from_value::<FileSystemPermissions>(serde_json::json!({
        "entries": [],
        "glob_scan_max_depth": 0,
    }))
    .expect_err("zero glob scan depth should fail deserialization");
}

#[test]
fn convert_mcp_content_to_items_builds_data_urls_when_missing_prefix() {
    let contents = vec![serde_json::json!({
        "type": "image",
        "data": "Zm9v",
        "mimeType": "image/png",
    })];

    let items = convert_mcp_content_to_items(&contents);
    assert_eq!(
        items,
        vec![FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,Zm9v".to_string(),
            },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        }]
    );
}

#[test]
fn convert_mcp_audio_content_builds_data_urls_and_preserves_existing_data_urls() {
    let contents = vec![
        serde_json::json!({
            "type": "audio",
            "data": "Zm9v",
            "mimeType": "audio/wav",
            "_meta": {"source": "microphone"},
        }),
        serde_json::json!({
            "type": "audio",
            "data": "data:audio/ogg;base64,YmFy",
            "mimeType": "audio/ogg",
        }),
    ];

    assert_eq!(
        convert_mcp_content_to_items(&contents),
        vec![
            FunctionCallOutputContentItem::InputAudio {
                audio_url: "data:audio/wav;base64,Zm9v".to_string(),
            },
            FunctionCallOutputContentItem::InputAudio {
                audio_url: "data:audio/ogg;base64,YmFy".to_string(),
            },
        ]
    );
}

#[test]
fn convert_mcp_content_to_items_converts_text_without_media() {
    let contents = vec![serde_json::json!({
        "type": "text",
        "text": "hello",
    })];

    assert_eq!(
        convert_mcp_content_to_items(&contents),
        vec![FunctionCallOutputContentItem::InputText {
            text: "hello".to_string(),
        }]
    );
}

#[test]
fn function_call_output_content_items_to_text_joins_text_segments() {
    let content_items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "line 1".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,AAA".to_string(),
            },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        },
        FunctionCallOutputContentItem::InputText {
            text: "line 2".to_string(),
        },
    ];

    let text = function_call_output_content_items_to_text(&content_items);
    assert_eq!(text, Some("line 1\nline 2".to_string()));
}

#[test]
fn function_call_output_content_items_to_text_ignores_blank_text_and_media() {
    let content_items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "   ".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,AAA".to_string(),
            },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        },
        FunctionCallOutputContentItem::InputAudio {
            audio_url: "data:audio/wav;base64,AAA".to_string(),
        },
        FunctionCallOutputContentItem::EncryptedContent {
            encrypted_content: "enc_opaque".to_string(),
        },
    ];

    let text = function_call_output_content_items_to_text(&content_items);
    assert_eq!(text, None);
}

#[test]
fn function_call_output_body_to_text_returns_plain_text_content() {
    let body = FunctionCallOutputBody::Text("ok".to_string());
    let text = body.to_text();
    assert_eq!(text, Some("ok".to_string()));
}

#[test]
fn function_call_output_body_to_text_uses_content_item_fallback() {
    let body = FunctionCallOutputBody::ContentItems(vec![
        FunctionCallOutputContentItem::InputText {
            text: "line 1".to_string(),
        },
        FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,AAA".to_string(),
            },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        },
    ]);

    let text = body.to_text();
    assert_eq!(text, Some("line 1".to_string()));
}

#[test]
fn function_call_deserializes_optional_namespace() {
    let item: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "function_call",
        "name": "mcp__codex_apps__gmail_get_recent_emails",
        "namespace": "mcp__codex_apps__gmail",
        "arguments": "{\"top_k\":5}",
        "call_id": "call-1",
    }))
    .expect("function_call should deserialize");

    assert_eq!(
        item,
        ResponseItem::FunctionCall {
            status: None,
            encrypted_content: None,
            id: None,
            name: "mcp__codex_apps__gmail_get_recent_emails".to_string(),
            namespace: Some("mcp__codex_apps__gmail".to_string()),
            arguments: "{\"top_k\":5}".to_string(),
            encrypted_function_args: None,
            call_id: "call-1".to_string(),
            internal_chat_message_metadata_passthrough: None,
        }
    );
}

#[test]
fn paired_function_call_output_preserves_existing_wire_shape() {
    let value = serde_json::json!({
        "type": "function_call_output",
        "call_id": "call-1",
        "output": "done",
    });
    let item: ResponseItem = serde_json::from_value(value.clone())
        .expect("paired function call output should deserialize");

    assert_eq!(
        item,
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("call-1".to_string()),
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload::from_text("done".to_string()),
            internal_chat_message_metadata_passthrough: None,
        }
    );
    assert_eq!(serde_json::to_value(item).expect("serialize item"), value);
}

#[test]
fn named_unpaired_function_call_output_round_trips_without_call_id() {
    let value = serde_json::json!({
        "type": "function_call_output",
        "name": "notifications",
        "namespace": "slack",
        "output": "Alice mentioned you.",
    });
    let item: ResponseItem = serde_json::from_value(value.clone())
        .expect("named unpaired function call output should deserialize");

    assert_eq!(
        item,
        ResponseItem::FunctionCallOutput {
            id: None,
            call_id: None,
            name: Some("notifications".to_string()),
            namespace: Some("slack".to_string()),
            output: FunctionCallOutputPayload::from_text("Alice mentioned you.".to_string()),
            internal_chat_message_metadata_passthrough: None,
        }
    );
    assert_eq!(serde_json::to_value(item).expect("serialize item"), value);
}

#[test]
fn function_call_preserves_empty_encrypted_function_args() {
    let value = serde_json::json!({
        "type": "function_call",
        "name": "spawn_agent",
        "namespace": "collaboration",
        "arguments": "{\"message\":\"hello\",\"task_name\":\"worker\"}",
        "encrypted_function_args": [],
        "call_id": "call-1",
    });
    let item: ResponseItem =
        serde_json::from_value(value.clone()).expect("function_call should deserialize");

    assert_eq!(serde_json::to_value(item).expect("serialize item"), value);
}

#[test]
fn render_command_prefix_list_sorts_by_len_then_total_len_then_alphabetical() {
    let prefixes = vec![
        vec!["b".to_string(), "zz".to_string()],
        vec!["aa".to_string()],
        vec!["b".to_string()],
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        vec!["a".to_string()],
        vec!["b".to_string(), "a".to_string()],
    ];

    let output = format_allow_prefixes(prefixes).expect("rendered list");
    assert_eq!(
        output,
        r#"- ["a"]
- ["b"]
- ["aa"]
- ["b", "a"]
- ["b", "zz"]
- ["a", "b", "c"]"#
            .to_string(),
    );
}

#[test]
fn render_command_prefix_list_limits_output_to_max_prefixes() {
    let prefixes = (0..(MAX_RENDERED_PREFIXES + 5))
        .map(|i| vec![format!("{i:03}")])
        .collect::<Vec<_>>();

    let output = format_allow_prefixes(prefixes).expect("rendered list");
    assert_eq!(output.ends_with(TRUNCATED_MARKER), true);
    eprintln!("output: {output}");
    assert_eq!(output.lines().count(), MAX_RENDERED_PREFIXES + 1);
}

#[test]
fn format_allow_prefixes_limits_output() {
    let mut exec_policy = Policy::empty();
    for i in 0..200 {
        exec_policy
            .add_prefix_rule(
                &[format!("tool-{i:03}"), "x".repeat(500)],
                codex_execpolicy::Decision::Allow,
            )
            .expect("add rule");
    }

    let output =
        format_allow_prefixes(exec_policy.get_allowed_prefixes()).expect("formatted prefixes");
    assert!(
        output.len() <= MAX_ALLOW_PREFIX_TEXT_BYTES + TRUNCATED_MARKER.len(),
        "output length exceeds expected limit: {output}",
    );
}

#[test]
fn serializes_success_as_plain_string() -> Result<()> {
    let item = ResponseInputItem::FunctionCallOutput {
        call_id: "call1".into(),
        output: FunctionCallOutputPayload::from_text("ok".into()),
    };

    let json = serde_json::to_string(&item)?;
    let v: serde_json::Value = serde_json::from_str(&json)?;

    // Success case -> output should be a plain string
    assert_eq!(v.get("output").unwrap().as_str().unwrap(), "ok");
    Ok(())
}

#[test]
fn serializes_failure_as_string() -> Result<()> {
    let item = ResponseInputItem::FunctionCallOutput {
        call_id: "call1".into(),
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text("bad".into()),
            success: Some(false),
        },
    };

    let json = serde_json::to_string(&item)?;
    let v: serde_json::Value = serde_json::from_str(&json)?;

    assert_eq!(v.get("output").unwrap().as_str().unwrap(), "bad");
    Ok(())
}

#[test]
fn converts_unstructured_mcp_content_to_items() {
    let content = vec![
        serde_json::json!({"type":"text","text":"caption"}),
        serde_json::json!({
            "type": "resource_link",
            "uri": "file:///notes.txt",
            "name": "notes",
        }),
        serde_json::json!({
            "type": "audio",
            "mimeType": "audio/wav",
        }),
    ];
    let call_tool_result = CallToolResult {
        content: content.clone(),
        structured_content: Some(serde_json::Value::Null),
        is_error: Some(false),
        meta: None,
    };

    let resource_link = serde_json::to_string(&content[1]).expect("serialize resource link");
    let malformed_audio = serde_json::to_string(&content[2]).expect("serialize malformed audio");
    assert_eq!(
        call_tool_result.as_function_call_output_payload(),
        FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "caption".to_string(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: resource_link,
                },
                FunctionCallOutputContentItem::InputText {
                    text: malformed_audio,
                },
            ]),
            success: Some(true),
        }
    );
}

#[test]
fn preserves_structured_mcp_content() {
    let call_tool_result = CallToolResult {
        content: vec![serde_json::json!({"type":"text","text":"ignored"})],
        structured_content: Some(serde_json::json!({"result":"structured"})),
        is_error: Some(false),
        meta: None,
    };

    assert_eq!(
        call_tool_result.as_function_call_output_payload(),
        FunctionCallOutputPayload {
            body: FunctionCallOutputBody::Text(
                serde_json::json!({"result":"structured"}).to_string(),
            ),
            success: Some(true),
        }
    );
}

#[test]
fn serializes_image_outputs_as_array() -> Result<()> {
    let call_tool_result = CallToolResult {
        content: vec![
            serde_json::json!({"type":"text","text":"caption"}),
            serde_json::json!({"type":"image","data":"BASE64","mimeType":"image/png"}),
        ],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    };

    let payload = call_tool_result.into_function_call_output_payload();
    assert_eq!(payload.success, Some(true));
    let Some(items) = payload.content_items() else {
        panic!("expected content items");
    };
    let items = items.to_vec();
    assert_eq!(
        items,
        vec![
            FunctionCallOutputContentItem::InputText {
                text: "caption".into(),
            },
            FunctionCallOutputContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,BASE64".into(),
                },
                detail: Some(DEFAULT_IMAGE_DETAIL),
            },
        ]
    );

    let item = ResponseInputItem::FunctionCallOutput {
        call_id: "call1".into(),
        output: payload,
    };

    let json = serde_json::to_string(&item)?;
    let v: serde_json::Value = serde_json::from_str(&json)?;

    let output = v.get("output").expect("output field");
    assert!(output.is_array(), "expected array output");

    Ok(())
}

#[test]
fn serializes_audio_outputs_as_array() -> Result<()> {
    let call_tool_result = CallToolResult {
        content: vec![
            serde_json::json!({"type":"text","text":"caption"}),
            serde_json::json!({"type":"audio","data":"BASE64","mimeType":"audio/wav"}),
        ],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    };

    let payload = call_tool_result.into_function_call_output_payload();
    assert_eq!(
        payload,
        FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "caption".into(),
                },
                FunctionCallOutputContentItem::InputAudio {
                    audio_url: "data:audio/wav;base64,BASE64".into(),
                },
            ]),
            success: Some(true),
        }
    );

    let item = ResponseInputItem::FunctionCallOutput {
        call_id: "call1".into(),
        output: payload,
    };

    assert_eq!(
        serde_json::to_value(item)?,
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call1",
            "output": [
                {"type": "input_text", "text": "caption"},
                {"type": "input_audio", "audio_url": "data:audio/wav;base64,BASE64"},
            ],
        })
    );

    Ok(())
}

#[test]
fn serializes_custom_tool_image_outputs_as_array() -> Result<()> {
    let item = ResponseInputItem::CustomToolCallOutput {
        call_id: "call1".into(),
        name: None,
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,BASE64".into(),
                },
                detail: Some(DEFAULT_IMAGE_DETAIL),
            },
        ]),
    };

    let json = serde_json::to_string(&item)?;
    let v: serde_json::Value = serde_json::from_str(&json)?;

    let output = v.get("output").expect("output field");
    assert!(output.is_array(), "expected array output");

    Ok(())
}

#[test]
fn serializes_encrypted_function_output_content_as_array() -> Result<()> {
    let item = ResponseInputItem::FunctionCallOutput {
        call_id: "call1".into(),
        output: FunctionCallOutputPayload::from_content_items(vec![
            FunctionCallOutputContentItem::EncryptedContent {
                encrypted_content: "enc_opaque".into(),
            },
        ]),
    };

    let json = serde_json::to_value(&item)?;
    assert_eq!(
        json,
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call1",
            "output": [
                {
                    "type": "encrypted_content",
                    "encrypted_content": "enc_opaque",
                }
            ],
        })
    );

    Ok(())
}

#[test]
fn preserves_existing_image_data_urls() -> Result<()> {
    let call_tool_result = CallToolResult {
        content: vec![serde_json::json!({
            "type": "image",
            "data": "data:image/png;base64,BASE64",
            "mimeType": "image/png"
        })],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    };

    let payload = call_tool_result.into_function_call_output_payload();
    let Some(items) = payload.content_items() else {
        panic!("expected content items");
    };
    let items = items.to_vec();
    assert_eq!(
        items,
        vec![FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,BASE64".into(),
            },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        }]
    );

    Ok(())
}

#[test]
fn preserves_original_detail_metadata_on_mcp_images() -> Result<()> {
    let call_tool_result = CallToolResult {
        content: vec![serde_json::json!({
            "type": "image",
            "data": "BASE64",
            "mimeType": "image/png",
            "_meta": {
                "codex/imageDetail": "original",
            },
        })],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    };

    let payload = call_tool_result.into_function_call_output_payload();
    let Some(items) = payload.content_items() else {
        panic!("expected content items");
    };
    let items = items.to_vec();
    assert_eq!(
        items,
        vec![FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,BASE64".into(),
            },
            detail: Some(ImageDetail::Original),
        }]
    );

    Ok(())
}

#[test]
fn preserves_standard_detail_metadata_on_mcp_images() -> Result<()> {
    let call_tool_result = CallToolResult {
        content: vec![serde_json::json!({
            "type": "image",
            "data": "BASE64",
            "mimeType": "image/png",
            "_meta": {
                "codex/imageDetail": "high",
            },
        })],
        structured_content: None,
        is_error: Some(false),
        meta: None,
    };

    let payload = call_tool_result.into_function_call_output_payload();
    let Some(items) = payload.content_items() else {
        panic!("expected content items");
    };
    let items = items.to_vec();
    assert_eq!(
        items,
        vec![FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,BASE64".into(),
            },
            detail: Some(ImageDetail::High),
        }]
    );

    Ok(())
}

#[test]
fn deserializes_array_payload_into_items() -> Result<()> {
    let json = r#"[
            {"type": "input_text", "text": "note"},
            {"type": "input_image", "image_url": "data:image/png;base64,XYZ"}
        ]"#;

    let payload: FunctionCallOutputPayload = serde_json::from_str(json)?;

    assert_eq!(payload.success, None);
    let expected_items = vec![
        FunctionCallOutputContentItem::InputText {
            text: "note".into(),
        },
        FunctionCallOutputContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,XYZ".into(),
            },
            detail: None,
        },
    ];
    assert_eq!(
        payload.body,
        FunctionCallOutputBody::ContentItems(expected_items.clone())
    );
    assert_eq!(
        serde_json::to_string(&payload)?,
        serde_json::to_string(&expected_items)?
    );

    Ok(())
}

#[test]
fn deserializes_encrypted_array_payload_into_items() -> Result<()> {
    let json = r#"[
            {"type": "encrypted_content", "encrypted_content": "enc_opaque"}
        ]"#;

    let payload: FunctionCallOutputPayload = serde_json::from_str(json)?;
    let expected_items = vec![FunctionCallOutputContentItem::EncryptedContent {
        encrypted_content: "enc_opaque".into(),
    }];

    assert_eq!(payload.success, None);
    assert_eq!(
        payload.body,
        FunctionCallOutputBody::ContentItems(expected_items.clone())
    );
    assert_eq!(
        serde_json::to_string(&payload)?,
        serde_json::to_string(&expected_items)?
    );

    Ok(())
}

#[test]
fn deserializes_compaction_alias() -> Result<()> {
    let json = r#"{"type":"compaction_summary","encrypted_content":"abc"}"#;

    let item: ResponseItem = serde_json::from_str(json)?;

    assert_eq!(
        item,
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "abc".into(),
            internal_chat_message_metadata_passthrough: None,
        }
    );
    Ok(())
}

#[test]
fn deserializes_context_compaction() -> Result<()> {
    let json = r#"{"type":"context_compaction","encrypted_content":"abc"}"#;

    let item: ResponseItem = serde_json::from_str(json)?;

    assert_eq!(
        item,
        ResponseItem::ContextCompaction {
            id: None,
            encrypted_content: Some("abc".into()),
            internal_chat_message_metadata_passthrough: None,
        }
    );
    Ok(())
}

#[test]
fn serializes_compaction_trigger_without_payload() -> Result<()> {
    let item = ResponseItem::CompactionTrigger {};

    assert_eq!(
        serde_json::to_value(item)?,
        serde_json::json!({
            "type": "compaction_trigger",
        })
    );
    Ok(())
}

#[test]
fn deserializes_compaction_trigger_without_payload() -> Result<()> {
    let json = r#"{"type":"compaction_trigger"}"#;

    let item: ResponseItem = serde_json::from_str(json)?;

    assert_eq!(item, ResponseItem::CompactionTrigger {});
    Ok(())
}

#[test]
fn deserializes_legacy_ghost_snapshot_as_other() -> Result<()> {
    let json = r#"{
            "type":"ghost_snapshot",
            "ghost_commit":{
                "id":"ghost-1",
                "parent":null,
                "preexisting_untracked_files":[],
                "preexisting_untracked_dirs":[]
            }
        }"#;

    let item: ResponseItem = serde_json::from_str(json)?;

    assert_eq!(item, ResponseItem::Other);
    Ok(())
}

#[test]
fn roundtrips_web_search_call_actions() -> Result<()> {
    let cases = vec![
        (
            r#"{
                    "type": "web_search_call",
                    "status": "completed",
                    "action": {
                        "type": "search",
                        "query": "weather seattle",
                        "queries": ["weather seattle", "seattle weather now"]
                    }
                }"#,
            None,
            Some(WebSearchAction::Search {
                query: Some("weather seattle".into()),
                queries: Some(vec!["weather seattle".into(), "seattle weather now".into()]),
            }),
            Some("completed".into()),
        ),
        (
            r#"{
                    "type": "web_search_call",
                    "status": "open",
                    "action": {
                        "type": "open_page",
                        "url": "https://example.com"
                    }
                }"#,
            None,
            Some(WebSearchAction::OpenPage {
                url: Some("https://example.com".into()),
            }),
            Some("open".into()),
        ),
        (
            r#"{
                    "type": "web_search_call",
                    "status": "in_progress",
                    "action": {
                        "type": "find_in_page",
                        "url": "https://example.com/docs",
                        "pattern": "installation"
                    }
                }"#,
            None,
            Some(WebSearchAction::FindInPage {
                url: Some("https://example.com/docs".into()),
                pattern: Some("installation".into()),
            }),
            Some("in_progress".into()),
        ),
        (
            r#"{
                    "type": "web_search_call",
                    "status": "in_progress",
                    "id": "ws_partial"
                }"#,
            Some(ResponseItemId::with_suffix("ws", "partial")),
            None,
            Some("in_progress".into()),
        ),
    ];

    for (json_literal, expected_id, expected_action, expected_status) in cases {
        let parsed: ResponseItem = serde_json::from_str(json_literal)?;
        let expected = ResponseItem::WebSearchCall {
            encrypted_content: None,
            id: expected_id.clone(),
            status: expected_status.clone(),
            action: expected_action.clone(),
            internal_chat_message_metadata_passthrough: None,
        };
        assert_eq!(parsed, expected);

        let serialized = serde_json::to_value(&parsed)?;
        let expected_serialized: serde_json::Value = serde_json::from_str(json_literal)?;
        assert_eq!(serialized, expected_serialized);
    }

    Ok(())
}

#[test]
fn serializes_image_user_input_without_tags() -> Result<()> {
    let image_url = "data:image/png;base64,abc".to_string();

    let item = ResponseInputItem::from(vec![UserInput::Image {
        image: ImageReference::Inline {
            image_url: image_url.clone(),
        },
        detail: None,
    }]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            let expected = vec![ContentItem::InputImage {
                image: ImageReference::Inline { image_url },
                detail: Some(DEFAULT_IMAGE_DETAIL),
            }];
            assert_eq!(content, expected);
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}

/// A file-backed user image must reach the Responses API without being resolved by Core.
#[test]
fn file_image_user_input_serializes_file_id() -> Result<()> {
    let file_id = "file_123".to_string();

    let item = ResponseInputItem::from(vec![UserInput::Image {
        image: ImageReference::File {
            file_id: file_id.clone(),
        },
        detail: None,
    }]);

    let expected = ResponseInputItem::Message {
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image: ImageReference::File { file_id },
            detail: Some(DEFAULT_IMAGE_DETAIL),
        }],
        phase: None,
    };
    assert_eq!(item, expected);
    assert_eq!(
        serde_json::to_value(item)?,
        serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "file_id": "file_123",
                "detail": "high"
            }]
        })
    );

    Ok(())
}

#[test]
fn serializes_audio_user_input_without_tags() -> Result<()> {
    for (audio, wire) in [
        (
            AudioReference::Inline {
                audio_url: "data:audio/wav;base64,abc".into(),
            },
            serde_json::json!({"type": "input_audio", "audio_url": "data:audio/wav;base64,abc"}),
        ),
        (
            AudioReference::File {
                file_id: "file_audio".into(),
            },
            serde_json::json!({"type": "input_audio", "file_id": "file_audio"}),
        ),
    ] {
        let input = UserInput::Audio {
            audio: audio.clone(),
        };
        assert_eq!(
            serde_json::from_value::<UserInput>(serde_json::to_value(&input)?)?,
            input
        );
        let item = ResponseInputItem::from(vec![input]);
        assert_eq!(
            item,
            ResponseInputItem::Message {
                role: "user".into(),
                content: vec![ContentItem::InputAudio { audio }],
                phase: None,
            }
        );
        assert_eq!(
            serde_json::to_value(&item)?,
            serde_json::json!({
                "type": "message", "role": "user", "content": [wire],
            })
        );
        assert_eq!(
            serde_json::from_value::<ResponseInputItem>(serde_json::to_value(&item)?)?,
            item
        );
    }
    Ok(())
}

#[test]
fn serializes_local_audio_user_input_with_label_and_data_url() -> Result<()> {
    let temp_dir = tempdir()?;
    for (extension, mime) in [
        ("wav", "audio/wav"),
        ("mp3", "audio/mpeg"),
        ("m4a", "audio/mp4"),
        ("webm", "audio/webm"),
        ("ogg", "audio/ogg"),
    ] {
        let audio_path = temp_dir.path().join(format!("sample.{extension}"));
        std::fs::write(&audio_path, b"audio")?;

        let item = ResponseInputItem::from(vec![UserInput::LocalAudio {
            path: audio_path.clone(),
        }]);

        assert_eq!(
            item,
            ResponseInputItem::Message {
                role: "user".to_string(),
                content: vec![
                    ContentItem::InputText {
                        text: local_audio_open_tag_text_with_path(
                            /*label_number*/ 1,
                            &audio_path,
                        ),
                    },
                    ContentItem::InputAudio {
                        audio: AudioReference::Inline {
                            audio_url: format!("data:{mime};base64,YXVkaW8=")
                        },
                    },
                    ContentItem::InputText {
                        text: audio_close_tag_text(),
                    },
                ],
                phase: None,
            }
        );
    }

    Ok(())
}

#[test]
fn replaces_unsupported_local_audio_format_with_placeholder() -> Result<()> {
    let temp_dir = tempdir()?;
    let audio_path = temp_dir.path().join("sample.flac");
    std::fs::write(&audio_path, b"audio")?;

    let item = ResponseInputItem::from(vec![UserInput::LocalAudio {
        path: audio_path.clone(),
    }]);

    assert_eq!(
        item,
        ResponseInputItem::Message {
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: format!(
                    "Codex cannot attach audio at `{}`: unsupported audio format; use wav, mp3, m4a, webm, or ogg.",
                    audio_path.display()
                ),
            }],
            phase: None,
        }
    );

    Ok(())
}

#[test]
fn replaces_unreadable_local_audio_with_placeholder() {
    let audio_path = PathBuf::from("missing.wav");

    let item = ResponseInputItem::from(vec![UserInput::LocalAudio { path: audio_path }]);

    let ResponseInputItem::Message { content, .. } = item else {
        panic!("expected message response");
    };
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("expected local audio error placeholder");
    };
    assert!(
        text.starts_with("Codex could not read the local audio at `missing.wav`: "),
        "unexpected placeholder: {text}"
    );
}

#[test]
fn image_user_input_preserves_requested_detail() -> Result<()> {
    let image_url = "data:image/png;base64,abc".to_string();

    let item = ResponseInputItem::from(vec![UserInput::Image {
        image: ImageReference::Inline {
            image_url: image_url.clone(),
        },
        detail: Some(ImageDetail::Original),
    }]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            assert_eq!(
                content.first(),
                Some(&ContentItem::InputImage {
                    image: ImageReference::Inline { image_url },
                    detail: Some(ImageDetail::Original),
                })
            );
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}

#[test]
fn tool_search_call_roundtrips() -> Result<()> {
    let parsed: ResponseItem = serde_json::from_str(
        r#"{
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": {
                    "query": "calendar create",
                    "limit": 1
                }
            }"#,
    )?;

    assert_eq!(
        parsed,
        ResponseItem::ToolSearchCall {
            encrypted_content: None,
            id: None,
            call_id: Some("search-1".to_string()),
            status: None,
            execution: "client".to_string(),
            arguments: serde_json::json!({
                "query": "calendar create",
                "limit": 1,
            }),
            internal_chat_message_metadata_passthrough: None,
        }
    );

    assert_eq!(
        serde_json::to_value(&parsed)?,
        serde_json::json!({
            "type": "tool_search_call",
            "call_id": "search-1",
            "execution": "client",
            "arguments": {
                "query": "calendar create",
                "limit": 1,
            }
        })
    );

    Ok(())
}

#[test]
fn tool_search_output_roundtrips() -> Result<()> {
    let input = ResponseInputItem::ToolSearchOutput {
        call_id: "search-1".to_string(),
        status: "completed".to_string(),
        execution: "client".to_string(),
        tools: vec![serde_json::json!({
            "type": "function",
            "name": "mcp__codex_apps__calendar_create_event",
            "description": "Create a calendar event.",
            "defer_loading": true,
            "parameters": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"}
                },
                "required": ["title"],
                "additionalProperties": false,
            }
        })],
    };
    assert_eq!(
        ResponseItem::from(input.clone()),
        ResponseItem::ToolSearchOutput {
            id: None,
            call_id: Some("search-1".to_string()),
            status: "completed".to_string(),
            execution: "client".to_string(),
            tools: vec![serde_json::json!({
                "type": "function",
                "name": "mcp__codex_apps__calendar_create_event",
                "description": "Create a calendar event.",
                "defer_loading": true,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"}
                    },
                    "required": ["title"],
                    "additionalProperties": false,
                }
            })],
            internal_chat_message_metadata_passthrough: None,
        }
    );

    assert_eq!(
        serde_json::to_value(input)?,
        serde_json::json!({
            "type": "tool_search_output",
            "call_id": "search-1",
            "status": "completed",
            "execution": "client",
            "tools": [{
                "type": "function",
                "name": "mcp__codex_apps__calendar_create_event",
                "description": "Create a calendar event.",
                "defer_loading": true,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"}
                    },
                    "required": ["title"],
                    "additionalProperties": false,
                }
            }]
        })
    );

    Ok(())
}

#[test]
fn tool_search_server_items_allow_null_call_id() -> Result<()> {
    let parsed_call: ResponseItem = serde_json::from_str(
        r#"{
                "type": "tool_search_call",
                "execution": "server",
                "call_id": null,
                "status": "completed",
                "arguments": {
                    "paths": ["crm"]
                }
            }"#,
    )?;
    assert_eq!(
        parsed_call,
        ResponseItem::ToolSearchCall {
            encrypted_content: None,
            id: None,
            call_id: None,
            status: Some("completed".to_string()),
            execution: "server".to_string(),
            arguments: serde_json::json!({
                "paths": ["crm"],
            }),
            internal_chat_message_metadata_passthrough: None,
        }
    );

    let parsed_output: ResponseItem = serde_json::from_str(
        r#"{
                "type": "tool_search_output",
                "execution": "server",
                "call_id": null,
                "status": "completed",
                "tools": []
            }"#,
    )?;
    assert_eq!(
        parsed_output,
        ResponseItem::ToolSearchOutput {
            id: None,
            call_id: None,
            status: "completed".to_string(),
            execution: "server".to_string(),
            tools: vec![],
            internal_chat_message_metadata_passthrough: None,
        }
    );

    Ok(())
}

/// Image associations use original input and expanded content positions, so omitted inputs,
/// local-image framing, failed reads, and duplicate URLs cannot shift a later substitution.
#[test]
fn user_input_image_indices_survive_content_expansion() -> Result<()> {
    let dir = tempdir()?;
    let local_path = dir.path().join("local.png");
    std::fs::write(&local_path, TINY_PNG_BYTES)?;
    let inline_image = UserInput::Image {
        image: ImageReference::Inline {
            image_url: data_url_from_bytes("image/png", TINY_PNG_BYTES),
        },
        detail: None,
    };
    let inputs = vec![
        UserInput::Text {
            text: "compare images".to_string(),
            text_elements: Vec::new(),
        },
        UserInput::Mention {
            name: "app".to_string(),
            path: "app://example".to_string(),
        },
        UserInput::Skill {
            name: "skill".to_string(),
            path: dir.path().join("SKILL.md"),
        },
        UserInput::LocalImage {
            path: dir.path().join("missing.png"),
            detail: None,
        },
        UserInput::LocalImage {
            path: local_path,
            detail: None,
        },
        inline_image.clone(),
        inline_image,
    ];
    for preparation in [LocalImagePreparation::Defer, LocalImagePreparation::Process] {
        let mut indices = HashMap::new();
        let item = ResponseInputItem::from_user_input(inputs.clone(), preparation, &mut indices);
        assert_eq!(indices, HashMap::from([(4, 3), (5, 5), (6, 6)]));
        let ResponseInputItem::Message { content, .. } = item else {
            panic!("expected a user message");
        };
        for content_index in indices.into_values() {
            assert!(matches!(
                content[content_index],
                ContentItem::InputImage { .. }
            ));
        }
    }
    Ok(())
}

#[test]
fn mixed_remote_and_local_images_share_label_sequence() -> Result<()> {
    let image_url = "data:image/png;base64,abc".to_string();
    let dir = tempdir()?;
    let local_path = dir.path().join("local.png");
    std::fs::write(&local_path, TINY_PNG_BYTES)?;

    let item = ResponseInputItem::from(vec![
        UserInput::Image {
            image: ImageReference::Inline {
                image_url: image_url.clone(),
            },
            detail: None,
        },
        UserInput::LocalImage {
            path: local_path.clone(),
            detail: None,
        },
    ]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            assert_eq!(
                content.first(),
                Some(&ContentItem::InputImage {
                    image: ImageReference::Inline { image_url },
                    detail: Some(DEFAULT_IMAGE_DETAIL),
                })
            );
            assert_eq!(
                content.get(1),
                Some(&ContentItem::InputText {
                    text: local_image_open_tag_text_with_path(/*label_number*/ 2, &local_path),
                })
            );
            assert!(matches!(
                content.get(2),
                Some(ContentItem::InputImage { .. })
            ));
            assert_eq!(
                content.get(3),
                Some(&ContentItem::InputText {
                    text: image_close_tag_text(),
                })
            );
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}

#[test]
fn mixed_remote_and_local_audio_share_label_sequence() -> Result<()> {
    let audio_url = "data:audio/wav;base64,abc".to_string();
    let dir = tempdir()?;
    let local_path = dir.path().join("local.mp3");
    std::fs::write(&local_path, b"audio")?;

    let item = ResponseInputItem::from(vec![
        UserInput::Audio {
            audio: AudioReference::Inline {
                audio_url: audio_url.clone(),
            },
        },
        UserInput::LocalAudio {
            path: local_path.clone(),
        },
    ]);

    assert_eq!(
        item,
        ResponseInputItem::Message {
            role: "user".to_string(),
            content: vec![
                ContentItem::InputAudio {
                    audio: AudioReference::Inline { audio_url }
                },
                ContentItem::InputText {
                    text: local_audio_open_tag_text_with_path(/*label_number*/ 2, &local_path,),
                },
                ContentItem::InputAudio {
                    audio: AudioReference::Inline {
                        audio_url: "data:audio/mpeg;base64,YXVkaW8=".to_string()
                    }
                },
                ContentItem::InputText {
                    text: audio_close_tag_text(),
                },
            ],
            phase: None,
        }
    );

    Ok(())
}

#[test]
fn local_image_open_tag_preserves_path() {
    assert_eq!(
        local_image_open_tag_text_with_path(
            /*label_number*/ 1,
            std::path::Path::new(r#"/tmp/a&"<b>.png"#),
        ),
        r#"<image name=[Image #1] path="/tmp/a&"<b>.png">"#
    );
}

#[test]
fn local_image_user_input_preserves_requested_detail() -> Result<()> {
    let dir = tempdir()?;
    let local_path = dir.path().join("local.png");
    std::fs::write(&local_path, TINY_PNG_BYTES)?;

    let item = ResponseInputItem::from(vec![UserInput::LocalImage {
        path: local_path,
        detail: Some(ImageDetail::Original),
    }]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            assert!(matches!(
                content.get(1),
                Some(ContentItem::InputImage {
                    detail: Some(ImageDetail::Original),
                    ..
                })
            ));
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}

#[test]
fn local_image_read_error_adds_placeholder() -> Result<()> {
    let dir = tempdir()?;
    let missing_path = dir.path().join("missing-image.png");

    let item = ResponseInputItem::from(vec![UserInput::LocalImage {
        path: missing_path.clone(),
        detail: None,
    }]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                ContentItem::InputText { text } => {
                    let display_path = missing_path.display().to_string();
                    assert!(
                        text.contains(&display_path),
                        "placeholder should mention missing path: {text}"
                    );
                    assert!(
                        text.contains("could not read"),
                        "placeholder should mention read issue: {text}"
                    );
                }
                other => panic!("expected placeholder text but found {other:?}"),
            }
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}

#[test]
fn local_image_non_image_adds_placeholder() -> Result<()> {
    let dir = tempdir()?;
    let json_path = dir.path().join("example.json");
    std::fs::write(&json_path, br#"{"hello":"world"}"#)?;

    let item = ResponseInputItem::from(vec![UserInput::LocalImage {
        path: json_path.clone(),
        detail: None,
    }]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            assert_eq!(content.len(), 1);
            match &content[0] {
                ContentItem::InputText { text } => {
                    assert!(
                        text.contains("unsupported image `application/json`"),
                        "placeholder should mention unsupported image MIME: {text}"
                    );
                    assert!(
                        text.contains(&json_path.display().to_string()),
                        "placeholder should mention path: {text}"
                    );
                }
                other => panic!("expected placeholder text but found {other:?}"),
            }
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}

#[test]
fn local_image_unsupported_image_format_adds_placeholder() -> Result<()> {
    let dir = tempdir()?;
    let svg_path = dir.path().join("example.svg");
    std::fs::write(
        &svg_path,
        br#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"></svg>"#,
    )?;

    let item = ResponseInputItem::from(vec![UserInput::LocalImage {
        path: svg_path.clone(),
        detail: None,
    }]);

    match item {
        ResponseInputItem::Message { content, .. } => {
            assert_eq!(content.len(), 1);
            let expected = format!(
                "Codex cannot attach image at `{}`: unsupported image `image/svg+xml`.",
                svg_path.display()
            );
            match &content[0] {
                ContentItem::InputText { text } => assert_eq!(text, &expected),
                other => panic!("expected placeholder text but found {other:?}"),
            }
        }
        other => panic!("expected message response but got {other:?}"),
    }

    Ok(())
}
