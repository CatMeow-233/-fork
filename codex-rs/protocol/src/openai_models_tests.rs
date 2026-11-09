use super::*;
use pretty_assertions::assert_eq;
use serde_json::from_str;
use serde_json::to_string;

#[test]
fn legacy_shell_model_metadata_deserializes_as_unified_exec() {
    for legacy_shell_type in ["default", "local", "shell_command"] {
        assert_eq!(
            from_str::<ConfigShellToolType>(&format!("\"{legacy_shell_type}\""))
                .expect("legacy shell type"),
            ConfigShellToolType::UnifiedExec
        );
    }
    assert_eq!(
        to_string(&ConfigShellToolType::UnifiedExec).expect("serialize unified shell type"),
        "\"unified_exec\""
    );
}

pub(super) fn test_model(spec: Option<ModelMessages>) -> ModelInfo {
    ModelInfo {
        slug: "test-model".to_string(),
        display_name: "Test Model".to_string(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: vec![],
        shell_type: ConfigShellToolType::UnifiedExec,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: 1,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        available_access_programs: None,
        availability_nux: None,
        upgrade: None,
        model_messages: spec,
        include_skills_usage_instructions: false,
        include_plugin_usage_instructions: false,
        include_apps_usage_instructions: false,
        supports_reasoning_summary_parameter: true,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        web_search_provider: None,
        truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
        supports_image_detail_original: false,
        context_window: None,
        max_context_window: None,
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: vec![],
        input_modalities: default_input_modalities(),
        supports_search_tool: false,
        supports_experimental_context: false,
        use_responses_lite: false,
        supports_reasoning_effort_updates: false,
        guardian: None,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: None,
        multi_agent_version: None,
        multi_agent_reasoning_effort: None,
    }
}
#[test]
fn model_messages_deserialize_without_optional_sections() {
    let messages: ModelMessages =
        from_str(r#"{"instructions_template":null,"persistent_instructions":null}"#)
            .expect("model messages should deserialize");

    assert_eq!(messages, ModelMessages::default());
}

#[test]
fn apps_messages_preserve_nullable_and_literal_wire_values() {
    for apps in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!({"usage_instructions": null}),
        serde_json::json!({"usage_instructions": ""}),
        serde_json::json!({"usage_instructions": " \r\n\t"}),
        serde_json::json!({"usage_instructions": "  {{literal}}\r\n"}),
        serde_json::json!({"usage_instructions": "é".repeat(501)}),
    ] {
        let messages: ModelMessages =
            serde_json::from_value(serde_json::json!({"apps": apps})).unwrap();
        let serialized = serde_json::to_value(messages).unwrap();
        let mut expected = serde_json::to_value(ModelMessages::default()).unwrap();
        if !apps.is_null() {
            expected["apps"] = if apps["usage_instructions"].is_null() {
                serde_json::json!({})
            } else {
                apps
            };
        }
        assert_eq!(serialized, expected);
    }
}

#[test]
fn send_user_message_async_description_preserves_missing_null_and_empty_values() {
    for (value, expected) in [
        (serde_json::json!({}), None),
        (serde_json::json!({"tools": null}), None),
        (serde_json::json!({"tools": {}}), Some(None)),
        (
            serde_json::json!({"tools": {"send_user_message_async": null}}),
            Some(None),
        ),
        (
            serde_json::json!({"tools": {"send_user_message_async": {}}}),
            Some(Some(ToolMessage::default())),
        ),
        (
            serde_json::json!({"tools": {"send_user_message_async": {"description": null}}}),
            Some(Some(ToolMessage::default())),
        ),
        (
            serde_json::json!({"tools": {"send_user_message_async": {"description": ""}}}),
            Some(Some(ToolMessage {
                description: Some(String::new()),
                ..Default::default()
            })),
        ),
        (
            serde_json::json!({"tools": {"send_user_message_async": {"description": "Catalog description"}}}),
            Some(Some(ToolMessage {
                description: Some("Catalog description".to_string()),
                ..Default::default()
            })),
        ),
    ] {
        let messages: ModelMessages =
            serde_json::from_value(value).expect("model messages should deserialize");
        let serialized = serde_json::to_value(&messages).expect("model messages should serialize");
        assert_eq!(
            (
                messages.tools,
                serialized
                    .get("tools")
                    .map(|tools| tools.get("send_user_message_async").cloned()),
            ),
            (
                expected.as_ref().map(|tool| ToolMessages {
                    send_user_message_async: tool.clone(),
                    ..Default::default()
                }),
                expected.map(|tool| tool.map(|tool| match tool.description {
                    Some(description) => serde_json::json!({"description": description}),
                    None => serde_json::json!({}),
                })),
            )
        );
    }
}

#[test]
fn spawn_agent_messages_preserve_sparse_and_empty_values() {
    for (value, expected) in [
        (serde_json::json!({}), serde_json::json!({})),
        (
            serde_json::json!({"multi_agent": null}),
            serde_json::json!({}),
        ),
        (
            serde_json::json!({"multi_agent": {"spawn_agent": null}}),
            serde_json::json!({"multi_agent": {}}),
        ),
        (
            serde_json::json!({"multi_agent": {"spawn_agent": {"description": null}}}),
            serde_json::json!({"multi_agent": {"spawn_agent": {}}}),
        ),
        (
            serde_json::json!({"multi_agent": {"spawn_agent": {"description": "Catalog spawn"}}}),
            serde_json::json!({"multi_agent": {"spawn_agent": {"description": "Catalog spawn"}}}),
        ),
        (
            serde_json::json!({"multi_agent": {"spawn_agent": {"description": ""}}}),
            serde_json::json!({"multi_agent": {"spawn_agent": {"description": ""}}}),
        ),
    ] {
        let tools: ToolMessages = serde_json::from_value(value).expect("deserialize tool messages");
        assert_eq!(
            serde_json::to_value(&tools).expect("serialize tool messages"),
            expected
        );
        let restored: ToolMessages =
            serde_json::from_value(expected).expect("restore tool messages");
        assert_eq!(restored, tools);
    }
}

#[test]
fn approval_messages_preserve_missing_and_empty_values() {
    let messages: ModelMessages = from_str(
        r#"{
                "instructions_template": null,
                "approvals": {
                    "on_request": "",
                    "never": "",
                    "request_permissions": "",
                    "auto_review_suffix": null
                }
            }"#,
    )
    .expect("approval messages should deserialize");

    assert_eq!(
        messages.approvals,
        Some(ApprovalMessages {
            on_request: Some(String::new()),
            request_permissions: Some(String::new()),
            on_request_auto_review: None,
            never: Some(String::new()),
            unless_trusted: None,
            granular: None,
            ..Default::default()
        })
    );

    for field in ["request_permissions", "request_permissions_tool"] {
        for heading in [
            serde_json::Value::Null,
            serde_json::json!(""),
            serde_json::json!("Permission requests"),
        ] {
            let approvals: ApprovalMessages =
                serde_json::from_value(serde_json::json!({ field: heading }))
                    .expect("permission heading should deserialize with either field name");
            let mut expected = serde_json::json!({
                "on_request": null,
                "on_request_auto_review": null,
                "never": null,
                "unless_trusted": null,
            });
            if !heading.is_null() {
                expected["request_permissions"] = heading;
            }
            assert_eq!(
                serde_json::to_value(approvals).expect("serialize approvals"),
                expected
            );
        }
    }

    for (granular, expected) in [
        (serde_json::json!(null), None),
        (serde_json::json!({}), Some(serde_json::json!({}))),
        (
            serde_json::json!({"intro": null, "prompted_categories": null, "rejected_categories": null}),
            Some(serde_json::json!({})),
        ),
        (
            serde_json::json!({"intro": ""}),
            Some(serde_json::json!({"intro": ""})),
        ),
        (
            serde_json::json!({
                "intro": "Catalog: {{ policy }}",
                "prompted_categories": "Allowed: {{ categories }}",
                "rejected_categories": "Denied: {{ categories }}",
            }),
            Some(serde_json::json!({
                "intro": "Catalog: {{ policy }}",
                "prompted_categories": "Allowed: {{ categories }}",
                "rejected_categories": "Denied: {{ categories }}",
            })),
        ),
    ] {
        let wire = serde_json::json!({"approvals": {
            "on_request": "existing override", "granular": granular,
        }});
        let messages: ModelMessages =
            serde_json::from_value(wire).expect("sparse granular messages");
        let roundtrip = serde_json::to_value(messages.approvals).expect("serialize approvals");
        let mut expected_approvals = serde_json::json!({
            "on_request": "existing override",
            "on_request_auto_review": null,
            "never": null,
            "unless_trusted": null,
        });
        if let Some(expected) = expected {
            expected_approvals["granular"] = expected;
        }
        assert_eq!(roundtrip, expected_approvals);
    }
}

#[test]
fn auto_review_messages_preserve_missing_and_empty_values() {
    let missing_template: ModelMessages = from_str(
        r#"{
                "instructions_template": null,
                "auto_review": {
                    "policy": "policy"
                }
            }"#,
    )
    .expect("auto-review messages should deserialize without a policy template");
    let empty_template: ModelMessages = from_str(
        r#"{
                "instructions_template": null,
                "auto_review": {
                    "policy": "policy",
                    "policy_template": "",
                    "node_repl_policy": "",
                    "rejection_instructions": "",
                    "timeout_instructions": ""
                }
            }"#,
    )
    .expect("auto-review messages should deserialize with an empty policy template");

    assert_eq!(
        missing_template.auto_review,
        Some(AutoReviewMessages {
            policy: Some("policy".to_string()),
            policy_template: None,
            node_repl_policy: None,
            rejection_instructions: None,
            timeout_instructions: None,
        })
    );
    assert_eq!(
        empty_template.auto_review,
        Some(AutoReviewMessages {
            policy: Some("policy".to_string()),
            policy_template: Some(String::new()),
            node_repl_policy: Some(String::new()),
            rejection_instructions: Some(String::new()),
            timeout_instructions: Some(String::new()),
        })
    );
}

#[test]
fn permission_messages_preserve_missing_and_empty_values() {
    let messages: ModelMessages = from_str(
        r#"{
                "instructions_template": null,
                "permissions": {
                    "workspace_write": ""
                }
            }"#,
    )
    .expect("permission messages should deserialize");

    assert_eq!(
        messages.permissions,
        Some(PermissionMessages {
            danger_full_access: None,
            workspace_write: Some(String::new()),
            read_only: None,
            approved_prefixes: None,
            ..PermissionMessages::default()
        })
    );
}

#[test]
fn permission_messages_preserve_sparse_wire_values() {
    for input in [
        serde_json::json!({}),
        serde_json::json!({"approved_command_prefix_saved": null}),
        serde_json::json!({"approved_command_prefix_saved": ""}),
        serde_json::json!({"approved_command_prefix_saved": "Saved", "approved_prefixes": "Allowed: {{ approved_prefixes }}"}),
        serde_json::json!({"writable_root": null, "writable_roots": null}),
        serde_json::json!({"writable_root": "", "writable_roots": "Roots: {{ roots }}."}),
        serde_json::json!({"omitted_paths": null, "denied_reads": null}),
        serde_json::json!({"omitted_paths": "", "denied_reads": "Blocked:\n{{ denied_reads }}"}),
    ] {
        let messages: PermissionMessages = serde_json::from_value(input.clone()).unwrap();
        let mut expected = serde_json::json!({
            "danger_full_access": null,
            "workspace_write": null,
            "read_only": null,
        });
        for key in [
            "approved_command_prefix_saved",
            "approved_prefixes",
            "writable_root",
            "writable_roots",
            "omitted_paths",
            "denied_reads",
        ] {
            if let Some(value) = input.get(key).filter(|value| !value.is_null()) {
                expected[key] = value.clone();
            }
        }
        assert_eq!(serde_json::to_value(messages).unwrap(), expected);
    }
}

#[test]
fn multi_agent_messages_preserve_missing_and_empty_values() {
    let messages: ModelMessages = from_str(
            r#"{"instructions_template":null,"multi_agent":{"role":{"root":"","subagent":"subagent base"},"mode":{"explicit":"explicit mode","proactive":"","hint_text":""}}}"#,
        )
        .expect("multi-agent messages should deserialize");

    assert_eq!(
        messages.multi_agent,
        Some(MultiAgentMessages {
            role: Some(MultiAgentRoleMessages {
                root: Some(String::new()),
                subagent: Some("subagent base".to_string()),
            }),
            mode: Some(MultiAgentModeMessages {
                explicit: Some("explicit mode".to_string()),
                proactive: Some(String::new()),
                hint_text: Some(String::new()),
            }),
        })
    );
}

#[test]
fn collaboration_mode_messages_preserve_missing_and_empty_values() {
    let messages: ModelMessages = from_str(
        r#"{
                "instructions_template": "legacy catalog instructions",
                "instructions_variables": {
                    "personality_default": "default",
                    "personality_friendly": "friendly",
                    "personality_pragmatic": "pragmatic"
                },
                "collaboration_modes": {
                    "default": ""
                }
            }"#,
    )
    .expect("collaboration mode messages should deserialize");

    assert_eq!(
        messages,
        ModelMessages {
            instructions_template: Some("legacy catalog instructions".to_string()),
            collaboration_modes: Some(CollaborationModeMessages {
                default: Some(String::new()),
                plan: None,
            }),
            ..Default::default()
        }
    );
    let roundtripped: ModelMessages =
        serde_json::from_value(serde_json::to_value(&messages).expect("serialize model messages"))
            .expect("deserialize model messages");
    assert_eq!(roundtripped, messages);
}

#[test]
fn reasoning_effort_accepts_known_and_custom_values() {
    let custom = ReasoningEffort::Custom("future".to_string());
    let deserialized = from_str::<ReasoningEffort>(r#""future""#)
        .expect("custom reasoning effort should deserialize");
    let serialized = to_string(&custom).expect("custom reasoning effort should serialize");
    let serialized_max = to_string(&ReasoningEffort::Max).expect("Max should serialize");
    let serialized_ultra = to_string(&ReasoningEffort::Ultra).expect("Ultra should serialize");
    let serialized_persistent =
        to_string(&ReasoningEffort::Persistent).expect("Persistent should serialize");

    assert_eq!(
        (
            "high".parse(),
            "max".parse(),
            "ultra".parse(),
            "persistent".parse(),
            "future".parse(),
            deserialized,
            serialized,
            serialized_max,
            serialized_ultra,
            serialized_persistent,
            custom.to_string(),
        ),
        (
            Ok(ReasoningEffort::High),
            Ok(ReasoningEffort::Max),
            Ok(ReasoningEffort::Ultra),
            Ok(ReasoningEffort::Persistent),
            Ok(custom.clone()),
            custom,
            r#""future""#.to_string(),
            r#""max""#.to_string(),
            r#""ultra""#.to_string(),
            r#""persistent""#.to_string(),
            "future".to_string(),
        )
    );
}

#[test]
fn reasoning_effort_rejects_empty_values() {
    assert_eq!(
        "".parse::<ReasoningEffort>(),
        Err("reasoning_effort must not be empty".to_string())
    );
}

#[test]
fn reasoning_effort_json_schema_is_an_open_string() {
    let mut effort_generator = SchemaGenerator::default();

    assert_eq!(
        ReasoningEffort::json_schema(&mut effort_generator),
        Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            metadata: Some(Box::new(Metadata {
                description: Some(
                    "A non-empty reasoning effort value advertised by the model.".to_string(),
                ),
                ..Default::default()
            })),
            string: Some(Box::new(StringValidation {
                min_length: Some(1),
                ..Default::default()
            })),
            ..Default::default()
        })
    );
}

#[test]
fn models_response_promotes_legacy_base_instructions() {
    let mut value = serde_json::to_value(ModelsResponse {
        models: vec![test_model(/*spec*/ None)],
    })
    .expect("serialize models response");
    value["models"][0]["base_instructions"] = serde_json::json!("legacy instructions");

    let response: ModelsResponse =
        serde_json::from_value(value).expect("deserialize legacy models response");
    let model = &response.models[0];

    assert_eq!(
        model.model_messages,
        Some(ModelMessages {
            instructions_template: Some("legacy instructions".to_string()),
            ..Default::default()
        })
    );
    let serialized = serde_json::to_value(response).expect("serialize canonical response");
    assert_eq!(
        serialized["models"][0]["base_instructions"],
        "legacy instructions"
    );
}

#[test]
fn models_response_rejects_model_without_instruction_source() {
    let mut value = serde_json::to_value(ModelsResponse {
        models: vec![test_model(/*spec*/ None)],
    })
    .expect("serialize models response");
    value["models"][0]
        .as_object_mut()
        .expect("model should serialize as an object")
        .remove("base_instructions")
        .expect("serialized model should include legacy base instructions");

    let error = serde_json::from_value::<ModelsResponse>(value)
        .expect_err("model without instructions should be rejected");

    assert_eq!(
        error.to_string(),
        "model `test-model` is missing both `base_instructions` and \
             `model_messages.instructions_template`"
    );
}

#[test]
fn models_response_serializes_literal_legacy_base_instructions() {
    for (template, expected) in [
        (
            Some("before {{ personality }} after"),
            "before {{ personality }} after",
        ),
        (Some(""), ""),
        (None, ""),
    ] {
        let response = ModelsResponse {
            models: vec![test_model(Some(ModelMessages {
                instructions_template: template.map(str::to_owned),
                ..Default::default()
            }))],
        };
        let serialized = serde_json::to_value(response).expect("serialize models response");
        assert_eq!(serialized["models"][0]["base_instructions"], expected);
    }
}

#[test]
fn models_response_prefers_template_and_preserves_message_siblings() {
    let messages = ModelMessages {
        apps: Some(AppsMessages {
            usage_instructions: Some("Literal Apps {{guidance}}".to_string()),
        }),
        plugins: None,
        content_filter_guidance: Some("Offer a permitted alternative.".to_string()),
        persistent_instructions: Some("Persistent catalog instructions".to_string()),
        tools: Some(ToolMessages {
            send_user_message_async: Some(ToolMessage {
                description: Some("Catalog description".to_string()),
                ..Default::default()
            }),
            multi_agent: Some(MultiAgentToolMessages {
                spawn_agent: Some(ToolMessage {
                    description: Some("Catalog spawn description".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
        instructions_template: None,
        approvals: Some(ApprovalMessages {
            on_request: Some("approval".to_string()),
            on_request_auto_review: None,
            never: Some("never approval".to_string()),
            unless_trusted: Some("unless-trusted approval".to_string()),
            granular: Some(GranularApprovalMessages {
                intro: Some("Catalog: {{ policy }}".to_string()),
                prompted_categories: Some("Allowed: {{ categories }}".to_string()),
                rejected_categories: Some("Denied: {{ categories }}".to_string()),
            }),
            ..Default::default()
        }),
        collaboration_modes: Some(CollaborationModeMessages {
            default: Some("default collaboration".to_string()),
            plan: Some("plan collaboration".to_string()),
        }),
        auto_review: Some(AutoReviewMessages {
            policy: Some("policy".to_string()),
            policy_template: None,
            node_repl_policy: None,
            rejection_instructions: Some("rejection instructions".to_string()),
            timeout_instructions: Some("timeout instructions".to_string()),
        }),
        permissions: Some(PermissionMessages {
            danger_full_access: None,
            workspace_write: Some("workspace".to_string()),
            read_only: None,
            approved_prefixes: None,
            ..PermissionMessages::default()
        }),
        multi_agent: None,
        token_budget: None,
        confirmation_policies: Some(ConfirmationPolicies {
            browser_use: Some(
                "# Browser confirmations\n\nKeep {{literal_markdown}}.\n".to_string(),
            ),
            computer_use: Some(
                "  # Native confirmations\r\n\nKeep ${native_markdown}.\n".to_string(),
            ),
        }),
        guardian_v2: Some(GuardianV2ModelConfig {
            classifier_instructions: Some("Guardian classification".to_string()),
            review_threshold_basis_points: Some(7_500),
            reasoning_effort: Some(ReasoningEffort::Minimal),
            transcript: Some(GuardianV2TranscriptModelConfig {
                sources: Some(vec!["reasoning".to_string()]),
                max_tool_entry_tokens: Some(500),
                ..Default::default()
            }),
            ..Default::default()
        }),
    };
    let mut value = serde_json::to_value(ModelsResponse {
        models: vec![test_model(Some(messages.clone()))],
    })
    .expect("serialize models response");
    assert_eq!(
        value["models"][0]["model_messages"]["confirmation_policies"],
        serde_json::json!({
            "browser_use": "# Browser confirmations\n\nKeep {{literal_markdown}}.\n",
            "computer_use": "  # Native confirmations\r\n\nKeep ${native_markdown}.\n",
        })
    );
    value["models"][0]["base_instructions"] = serde_json::json!("legacy instructions");

    let response: ModelsResponse =
        serde_json::from_value(value).expect("deserialize legacy models response");
    let mut expected_messages = messages;
    expected_messages.instructions_template = Some("legacy instructions".to_string());
    assert_eq!(response.models[0].model_messages, Some(expected_messages));

    let canonical_messages = ModelMessages {
        persistent_instructions: Some(String::new()),
        tools: Some(ToolMessages {
            send_user_message_async: Some(ToolMessage {
                description: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        instructions_template: Some("canonical instructions".to_string()),
        ..Default::default()
    };
    let mut value = serde_json::to_value(ModelsResponse {
        models: vec![test_model(Some(canonical_messages.clone()))],
    })
    .expect("serialize models response");
    value["models"][0]["base_instructions"] = serde_json::json!("legacy instructions");

    let response: ModelsResponse =
        serde_json::from_value(value).expect("deserialize mixed models response");
    assert_eq!(response.models[0].model_messages, Some(canonical_messages));
}

#[test]
fn model_info_defaults_availability_nux_to_none_when_omitted() {
    let model: ModelInfo = serde_json::from_value(serde_json::json!({
        "slug": "test-model",
        "display_name": "Test Model",
        "description": null,
        "supported_reasoning_levels": [],
        "shell_type": "unified_exec",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "model_messages": null,
        "default_reasoning_summary": "auto",
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {
            "mode": "bytes",
            "limit": 10000
        },
        "supports_image_detail_original": false,
        "context_window": null,
        "auto_compact_token_limit": null,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": []
    }))
    .expect("deserialize model info");

    assert_eq!(model.availability_nux, None);
    assert_eq!(
        model.input_modalities,
        vec![InputModality::Text, InputModality::Image]
    );
    assert!(!model.include_skills_usage_instructions);
    assert!(!model.include_plugin_usage_instructions);
    assert!(model.include_apps_usage_instructions);
    assert!(model.supports_reasoning_summary_parameter);
    assert!(!model.supports_image_detail_original);
    assert_eq!(model.web_search_tool_type, WebSearchToolType::Text);
    assert!(!model.supports_search_tool);
    assert!(!model.supports_experimental_context);
    assert!(!model.use_responses_lite);
    assert!(!model.supports_reasoning_effort_updates);
    assert!(!model.node_repl_auto_review_required);
    assert!(!model.node_repl_disabled);
    assert_eq!(model.comp_hash, None);
    assert_eq!(model.auto_review_model_override, None);
    assert_eq!(model.tool_mode, None);
    assert_eq!(model.multi_agent_reasoning_effort, None);
}

#[test]
fn model_info_deserializes_multi_agent_reasoning_effort() {
    let mut value = serde_json::to_value(test_model(/*spec*/ None)).expect("serialize test model");
    value
        .as_object_mut()
        .expect("model info should be an object")
        .insert(
            "multi_agent_reasoning_effort".to_string(),
            serde_json::Value::String("high".to_string()),
        );

    let model = serde_json::from_value::<ModelInfo>(value)
        .expect("deserialize multi-agent reasoning effort");

    assert_eq!(
        model.multi_agent_reasoning_effort,
        Some(ReasoningEffort::High)
    );
}

#[test]
fn model_info_deserializes_optional_upgrade_retirement_at() {
    let base = serde_json::to_value(test_model(/*spec*/ None))
        .expect("serialize test model without retirement time");

    let mut absent = base.clone();
    absent
        .as_object_mut()
        .expect("model info should be an object")
        .insert(
            "upgrade".to_string(),
            serde_json::json!({
                "model": "replacement-model",
                "migration_markdown": "Use the replacement model."
            }),
        );
    let absent = serde_json::from_value::<ModelInfo>(absent)
        .expect("deserialize model info without upgrade retirement time");
    assert_eq!(
        absent
            .upgrade
            .as_ref()
            .and_then(|upgrade| upgrade.retirement_at.as_ref())
            .map(DateTime::timestamp),
        None
    );

    let mut null = base.clone();
    null.as_object_mut()
        .expect("model info should be an object")
        .insert(
            "upgrade".to_string(),
            serde_json::json!({
                "model": "replacement-model",
                "migration_markdown": "Use the replacement model.",
                "retirement_at": null
            }),
        );
    let null = serde_json::from_value::<ModelInfo>(null)
        .expect("deserialize model info with null upgrade retirement time");
    assert_eq!(
        null.upgrade
            .as_ref()
            .and_then(|upgrade| upgrade.retirement_at.as_ref())
            .map(DateTime::timestamp),
        None
    );

    let mut populated = base;
    populated
        .as_object_mut()
        .expect("model info should be an object")
        .insert(
            "upgrade".to_string(),
            serde_json::json!({
                "model": "replacement-model",
                "migration_markdown": "Use the replacement model.",
                "retirement_at": "2030-01-01T00:00:00Z"
            }),
        );
    let populated = serde_json::from_value::<ModelInfo>(populated)
        .expect("deserialize model info with upgrade retirement time");
    assert_eq!(
        populated
            .upgrade
            .as_ref()
            .and_then(|upgrade| upgrade.retirement_at.as_ref())
            .map(DateTime::timestamp),
        Some(1_893_456_000)
    );

    let mut malformed = serde_json::to_value(test_model(/*spec*/ None))
        .expect("serialize test model for malformed retirement time");
    malformed
        .as_object_mut()
        .expect("model info should be an object")
        .insert(
            "upgrade".to_string(),
            serde_json::json!({
                "model": "replacement-model",
                "migration_markdown": "Use the replacement model.",
                "retirement_at": "not-a-timestamp"
            }),
        );
    let malformed = serde_json::from_value::<ModelInfo>(malformed)
        .expect("tolerate malformed upgrade retirement time");
    assert_eq!(
        malformed
            .upgrade
            .as_ref()
            .and_then(|upgrade| upgrade.retirement_at.as_ref())
            .map(DateTime::timestamp),
        None
    );
}

#[test]
fn model_info_preserves_explicit_apps_guidance_opt_out() {
    let value = serde_json::to_value(test_model(/*spec*/ None))
        .expect("serialize model info with explicit apps guidance opt-out");
    assert_eq!(value["include_apps_usage_instructions"], false);

    let model = serde_json::from_value::<ModelInfo>(value).expect("deserialize model info");
    assert!(!model.include_apps_usage_instructions);
}

#[test]
fn model_info_deserializes_known_tool_mode() {
    let mut value = serde_json::to_value(test_model(/*spec*/ None)).expect("serialize test model");
    let object = value
        .as_object_mut()
        .expect("model info should be an object");
    object.insert(
        "tool_mode".to_string(),
        serde_json::Value::String("code_mode_only".to_string()),
    );
    let model = serde_json::from_value::<ModelInfo>(value).expect("deserialize model info");

    assert_eq!(model.tool_mode, Some(ToolMode::CodeModeOnly));
}

#[test]
fn model_info_treats_unknown_tool_mode_as_omitted() {
    let mut value = serde_json::to_value(test_model(/*spec*/ None)).expect("serialize test model");
    let object = value
        .as_object_mut()
        .expect("model info should be an object");
    object.insert(
        "tool_mode".to_string(),
        serde_json::Value::String("future_tool_mode".to_string()),
    );
    let model = serde_json::from_value::<ModelInfo>(value).expect("deserialize model info");

    assert_eq!(model.tool_mode, None);
    let serialized = serde_json::to_value(model).expect("serialize model info");
    let object = serialized
        .as_object()
        .expect("model info should be an object");
    assert!(!object.contains_key("tool_mode"));
}

#[test]
fn model_info_treats_unknown_multi_agent_version_as_omitted() {
    let mut value = serde_json::to_value(test_model(/*spec*/ None)).expect("serialize test model");
    let object = value
        .as_object_mut()
        .expect("model info should be an object");
    object.insert(
        "multi_agent_version".to_string(),
        serde_json::Value::String("future_multi_agent_version".to_string()),
    );
    let model = serde_json::from_value::<ModelInfo>(value).expect("deserialize model info");

    assert_eq!(model.multi_agent_version, None);
}

#[test]
fn resolved_context_window_prefers_context_window() {
    let model = ModelInfo {
        context_window: Some(273_000),
        max_context_window: Some(400_000),
        ..test_model(/*spec*/ None)
    };

    assert_eq!(model.resolved_context_window(), Some(273_000));
}

#[test]
fn resolved_context_window_falls_back_to_max_context_window() {
    let model = ModelInfo {
        context_window: None,
        max_context_window: Some(400_000),
        ..test_model(/*spec*/ None)
    };

    assert_eq!(model.resolved_context_window(), Some(400_000));
    assert_eq!(model.usable_context_window(), Some(380_000));
    assert_eq!(model.auto_compact_token_limit(), Some(360_000));
}

#[test]
fn model_context_window_limits_preserve_their_distinct_meanings() {
    let model = ModelInfo {
        context_window: Some(272_000),
        max_context_window: Some(400_000),
        auto_compact_token_limit: Some(250_000),
        effective_context_window_percent: 95,
        ..test_model(/*spec*/ None)
    };

    assert_eq!(
        (
            model.resolved_context_window(),
            model.usable_context_window(),
            model.auto_compact_token_limit(),
        ),
        (Some(272_000), Some(258_400), Some(244_800))
    );
}

#[test]
fn model_preset_preserves_availability_nux() {
    let preset = ModelPreset::from(ModelInfo {
        availability_nux: Some(ModelAvailabilityNux {
            message: "Try Spark.".to_string(),
        }),
        additional_speed_tiers: vec![SPEED_TIER_FAST.to_string()],
        default_service_tier: Some(ServiceTier::Fast.request_value().to_string()),
        service_tiers: Vec::new(),
        ..test_model(/*spec*/ None)
    });

    assert_eq!(
        preset.availability_nux,
        Some(ModelAvailabilityNux {
            message: "Try Spark.".to_string(),
        })
    );
    assert!(preset.supports_fast_mode());
    assert_eq!(
        preset.default_service_tier,
        Some(ServiceTier::Fast.request_value().to_string())
    );
}

#[test]
fn model_access_programs_preserve_explicit_selection_availability() {
    use crate::turn_input::CyberAccessProgram;

    for (metadata, expected) in [
        (None, None),
        (Some(serde_json::Value::Null), None),
        (
            Some(serde_json::json!({ "cyber": [] })),
            Some(ModelAccessPrograms { cyber: vec![] }),
        ),
        (
            Some(serde_json::json!({
                "cyber": ["standard", "daybreak_blue", "future_program", "daybreak_red"]
            })),
            Some(ModelAccessPrograms {
                cyber: vec![
                    CyberAccessProgram::Standard,
                    CyberAccessProgram::DaybreakBlue,
                    CyberAccessProgram::DaybreakRed,
                ],
            }),
        ),
    ] {
        let mut value = serde_json::to_value(test_model(/*spec*/ None)).unwrap();
        if let Some(metadata) = metadata {
            value["available_access_programs"] = metadata;
        }
        let model: ModelInfo = serde_json::from_value(value).unwrap();
        assert_eq!(model.available_access_programs, expected);
        assert_eq!(ModelPreset::from(model).available_access_programs, expected);
    }
}

#[test]
fn model_preset_supports_fast_mode_from_service_tiers() {
    let preset = ModelPreset::from(ModelInfo {
        service_tiers: vec![ModelServiceTier {
            id: ServiceTier::Fast.request_value().to_string(),
            name: "Fast".to_string(),
            description: "Priority processing.".to_string(),
        }],
        ..test_model(/*spec*/ None)
    });

    assert!(preset.supports_fast_mode());
}

#[test]
fn service_tier_for_request_omits_explicit_default_tier() {
    let model = ModelInfo {
        default_service_tier: Some(ServiceTier::Fast.request_value().to_string()),
        service_tiers: vec![ModelServiceTier {
            id: ServiceTier::Fast.request_value().to_string(),
            name: "Fast".to_string(),
            description: "Priority processing.".to_string(),
        }],
        ..test_model(/*spec*/ None)
    };

    assert_eq!(
        model.service_tier_for_request(Some(SERVICE_TIER_DEFAULT_REQUEST_VALUE.to_string())),
        None
    );
}

#[test]
fn service_tier_for_request_preserves_flex_without_catalog_support() {
    let model = ModelInfo {
        service_tiers: Vec::new(),
        ..test_model(/*spec*/ None)
    };

    assert_eq!(
        model.service_tier_for_request(Some(ServiceTier::Flex.request_value().to_string())),
        Some(ServiceTier::Flex.request_value().to_string())
    );
}

#[test]
fn service_tier_for_request_filters_unsupported_tiers() {
    let model = ModelInfo {
        default_service_tier: Some(ServiceTier::Fast.request_value().to_string()),
        service_tiers: vec![ModelServiceTier {
            id: ServiceTier::Fast.request_value().to_string(),
            name: "Fast".to_string(),
            description: "Priority processing.".to_string(),
        }],
        ..test_model(/*spec*/ None)
    };

    assert_eq!(
        model.service_tier_for_request(Some(ServiceTier::Fast.request_value().to_string())),
        Some(ServiceTier::Fast.request_value().to_string())
    );
    assert_eq!(
        model.service_tier_for_request(Some("unsupported".to_string())),
        None
    );
    assert_eq!(model.service_tier_for_request(/*service_tier*/ None), None);
}
