use super::*;
use crate::agent::host::AgentUsageAccounting;
use crate::agent::host::AgentUsageReport;
use crate::agent::host::UsageAccountingDecision;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::MessagePhase;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct UsageRecorder {
    reports: Mutex<Vec<AgentUsageReport>>,
    decision: UsageAccountingDecision,
}

impl crate::AgentTurnAdmission for UsageRecorder {
    fn admit_turn(
        &self,
        _: crate::AgentTurnAdmissionRequest,
    ) -> futures::future::BoxFuture<'_, CodexResult<crate::AgentExecutionGuard>> {
        Box::pin(async { Ok(crate::AgentExecutionGuard::new(())) })
    }
}

impl UsageRecorder {
    fn install(self: &Arc<Self>, session: &mut Arc<Session>) {
        let data = &session.services.thread_extension_data;
        data.insert(crate::AgentRuntimeOwnership::Host);
        data.insert(self.clone() as Arc<dyn crate::AgentTurnAdmission>);
        data.insert(self.clone() as Arc<dyn AgentUsageAccounting>);
        let runtime = crate::agent::host::AgentRuntimeServices::from_extensions(data)
            .expect("valid host providers");
        Arc::get_mut(session).expect("unique session").agent_runtime = runtime;
    }
}

impl AgentUsageAccounting for UsageRecorder {
    fn report_usage(
        &self,
        report: AgentUsageReport,
    ) -> futures::future::BoxFuture<'_, CodexResult<UsageAccountingDecision>> {
        self.reports.lock().unwrap().push(report);
        Box::pin(async { Ok(self.decision) })
    }
}

fn message(role: &str, text: &str, phase: Option<MessagePhase>) -> ResponseItem {
    ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn build_without_metadata(
    input: Vec<ResponseItem>,
    output: ResponseItem,
) -> (Vec<ResponseItemEnvelope>, usize) {
    let metadata = vec![None; input.len()];
    build_v2_compacted_history(
        input,
        metadata,
        output,
        &Features::with_defaults(),
        RetainedImageBudget::Disabled,
        &FileAudioDurations::default(),
    )
}

fn annotated(items: Vec<ResponseItem>) -> Vec<ResponseItemEnvelope> {
    items.into_iter().map(ResponseItemEnvelope::new).collect()
}

fn raw(items: Vec<ResponseItemEnvelope>) -> Vec<ResponseItem> {
    items
        .into_iter()
        .map(ResponseItemEnvelope::into_item)
        .collect()
}

fn truncate_without_metadata(items: Vec<ResponseItem>, max_tokens: usize) -> Vec<ResponseItem> {
    raw(truncate_retained_messages_for_remote_compaction(
        annotated(items),
        max_tokens,
        &FileAudioDurations::default(),
    ))
}

fn response_stream(events: Vec<CodexResult<ResponseEvent>>) -> ResponseStream {
    let (tx_event, rx_event) = mpsc::channel(events.len().max(1));
    for event in events {
        tx_event
            .try_send(event)
            .expect("response stream test channel should have capacity");
    }
    drop(tx_event);
    ResponseStream {
        routing_affinity: None,
        rx_event,
        interrupt: None,
        consumer_dropped: CancellationToken::new(),
    }
}

#[test]
fn build_v2_compacted_history_filters_to_installed_retention_shape() {
    let hook = codex_protocol::items::build_hook_prompt_message(
        &[codex_protocol::items::HookPromptFragment::from_single_hook(
            "hook", "hook-run",
        )],
        &codex_protocol_test_support::uuid_v7_item_id_generator_for_test(),
    )
    .expect("hook prompt");
    let input = vec![
        message("developer", "dev", /*phase*/ None),
        message("system", "sys", /*phase*/ None),
        message("user", "user", /*phase*/ None),
        hook.clone(),
        message("assistant", "commentary", Some(MessagePhase::Commentary)),
        message("assistant", "final", Some(MessagePhase::FinalAnswer)),
        ResponseItem::FunctionCall {
            status: None,
            encrypted_content: None,
            id: None,
            name: "shell_command".to_string(),
            namespace: None,
            arguments: "{}".to_string(),
            call_id: "call_1".to_string(),
            encrypted_function_args: None,
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "old".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
    ];
    let output = ResponseItem::Compaction {
        id: None,
        encrypted_content: "new".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };

    let (history, _) = build_without_metadata(input, output.clone());

    assert_eq!(
        raw(history),
        vec![message("user", "user", /*phase*/ None), hook, output]
    );
}

#[test]
fn build_v2_compacted_history_preserves_retained_metadata_sidecar() {
    let retained = message("user", "keep", /*phase*/ None);
    let generated_notice = message(
        "developer",
        "<image_resize_notice>generated</image_resize_notice>",
        /*phase*/ None,
    );
    let harness = message("developer", "drop", /*phase*/ None);
    let client = message(
        "developer",
        "<image_resize_notice>client</image_resize_notice>",
        /*phase*/ None,
    );
    let output = ResponseItem::Compaction {
        id: None,
        encrypted_content: "new".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };

    for enabled in [false, true] {
        let mut features = Features::with_defaults();
        if enabled {
            features.enable(Feature::RetainClientDeveloperMessages);
        }
        let (history, _) = build_v2_compacted_history(
            vec![
                harness.clone(),
                client.clone(),
                retained.clone(),
                generated_notice.clone(),
            ],
            vec![
                None,
                Some(CodexHarnessMetadata {
                    client_authored: true,
                    ..Default::default()
                }),
                Some(CodexHarnessMetadata::default()),
                None,
            ],
            output.clone(),
            &features,
            RetainedImageBudget::Disabled,
            &FileAudioDurations::default(),
        );
        let mut expected = vec![
            ResponseItemEnvelope {
                item: retained.clone(),
                metadata: Some(CodexHarnessMetadata::default()),
            },
            ResponseItemEnvelope::new(generated_notice.clone()),
            ResponseItemEnvelope {
                item: output.clone(),
                metadata: Some(CodexHarnessMetadata {
                    compaction_without_file_error_sources: true,
                    ..Default::default()
                }),
            },
        ];
        if enabled {
            expected.insert(
                0,
                ResponseItemEnvelope {
                    item: client.clone(),
                    metadata: Some(CodexHarnessMetadata {
                        client_authored: true,
                        ..Default::default()
                    }),
                },
            );
        }
        assert_eq!(history, expected);
    }
}

#[test]
fn retained_history_truncation_preserves_metadata() {
    let item = ResponseItemEnvelope {
        item: message("user", "word ".repeat(200).as_str(), /*phase*/ None),
        metadata: Some(CodexHarnessMetadata::default()),
    };

    let truncated = truncate_retained_messages_for_remote_compaction(
        vec![item],
        /*max_tokens*/ 4,
        &FileAudioDurations::default(),
    );

    assert_eq!(truncated.len(), 1);
    assert_eq!(truncated[0].metadata, Some(CodexHarnessMetadata::default()));
}

#[test]
fn build_v2_compacted_history_discards_messages_before_truncating() {
    let old = message("user", "old", /*phase*/ None);
    let new = message("user", "new", /*phase*/ None);
    let huge_developer_message = "d".repeat((RETAINED_MESSAGE_TOKEN_BUDGET + 1) * 4);
    let huge_contextual_message = format!(
        "<environment_context>\n{}\n</environment_context>",
        "c".repeat((RETAINED_MESSAGE_TOKEN_BUDGET + 1) * 4)
    );
    let input = vec![
        old.clone(),
        message("developer", &huge_developer_message, /*phase*/ None),
        message("user", &huge_contextual_message, /*phase*/ None),
        new.clone(),
    ];
    let output = ResponseItem::Compaction {
        id: None,
        encrypted_content: "new".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };

    let (history, _) = build_without_metadata(input, output.clone());

    assert_eq!(raw(history), vec![old, new, output]);
}

#[test]
fn build_v2_compacted_history_counts_retained_input_images() {
    let input = vec![ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "user".to_string(),
            },
            ContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,abc".to_string(),
                },
                detail: None,
            },
            ContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,def".to_string(),
                },
                detail: None,
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];
    let output = ResponseItem::Compaction {
        id: None,
        encrypted_content: "new".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };

    let (_, retained_image_count) = build_without_metadata(input, output);

    assert_eq!(retained_image_count, 2);
}

#[test]
fn retained_history_truncation_keeps_newest_messages_first() {
    let middle = message("user", "middle1234", /*phase*/ None);
    let new = message("user", "new", /*phase*/ None);
    let retained = vec![
        message("user", "old-old", /*phase*/ None),
        middle,
        new.clone(),
    ];

    let truncated = truncate_without_metadata(retained, /*max_tokens*/ 3);

    assert_eq!(
        truncated,
        vec![
            ResponseItem::Message {
                encrypted_content: None,
                status: None,
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "midd…1 tokens truncated…1234".to_string(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: Some(
                    InternalChatMessageMetadataPassthrough {
                        content_item_kinds: Some(vec![ContentItemKind("unknown".to_string())]),
                        ..Default::default()
                    },
                ),
            },
            new,
        ]
    );
}

#[test]
fn retained_history_truncation_preserves_images_and_truncates_later_text_parts() {
    let item = ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "abcdef".to_string(),
            },
            ContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,abc".to_string(),
                },
                detail: None,
            },
            ContentItem::OutputText {
                annotations: None,
                logprobs: None,
                text: "uvwxyz".to_string(),
            },
            ContentItem::InputText {
                text: "discarded after the text budget is exhausted".to_string(),
            },
            ContentItem::InputImage {
                image: ImageReference::Inline {
                    image_url: "data:image/png;base64,def".to_string(),
                },
                detail: None,
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: Some(InternalChatMessageMetadataPassthrough {
            turn_id: Some("turn-1".to_string()),
            content_item_kinds: Some(vec![
                ContentItemKind("user.text".to_string()),
                ContentItemKind("user.image".to_string()),
                ContentItemKind("user.text".to_string()),
                ContentItemKind("user.text".to_string()),
                ContentItemKind("user.image".to_string()),
            ]),
            ..Default::default()
        }),
    };

    let source = codex_history::RetainedSource {
        id: codex_history::RetainedSourceId {
            message_id: "original".to_owned(),
            turn_id: "parent".to_owned(),
            role: codex_history::RetainedSourceRole::User,
        },
        revision: codex_protocol::ResponseItemId::from_server("revision-1".to_owned()),
        complete: true,
    };
    let mut metadata = CodexHarnessMetadata {
        retained_source: Some(source.clone()),
        guardian_sources: vec![source],
        ..Default::default()
    };
    let truncated = truncate_message_text_to_token_budget(
        ResponseItemEnvelope {
            item,
            metadata: Some(metadata.clone()),
        },
        /*max_tokens*/ 3,
    )
    .unwrap();
    metadata.mark_retained_sources_incomplete();
    assert_eq!(truncated.metadata, Some(metadata));
    let truncated = vec![truncated.item];

    assert_eq!(
        truncated,
        vec![ResponseItem::Message {
            encrypted_content: None,
            status: None,
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "abcdef".to_string(),
                },
                ContentItem::InputImage {
                    image: ImageReference::Inline {
                        image_url: "data:image/png;base64,abc".to_string()
                    },
                    detail: None,
                },
                ContentItem::OutputText {
                    annotations: None,
                    logprobs: None,
                    text: "uv…1 tokens truncated…yz".to_string(),
                },
                ContentItem::InputImage {
                    image: ImageReference::Inline {
                        image_url: "data:image/png;base64,def".to_string()
                    },
                    detail: None,
                },
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: Some(
                InternalChatMessageMetadataPassthrough {
                    turn_id: Some("turn-1".to_string()),
                    content_item_kinds: Some(vec![
                        ContentItemKind("user.text".to_string()),
                        ContentItemKind("user.image".to_string()),
                        ContentItemKind("user.text".to_string()),
                        ContentItemKind("user.image".to_string()),
                    ]),
                    ..Default::default()
                },
            ),
        }]
    );
}

#[test]
fn retained_history_truncation_charges_image_only_messages() {
    let image_only_message = ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,abc".to_string(),
            },
            detail: None,
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let newest = message("user", "new", /*phase*/ None);
    let retained = vec![
        message("user", "old", /*phase*/ None),
        image_only_message.clone(),
        newest.clone(),
    ];

    let truncated = truncate_without_metadata(retained, /*max_tokens*/ 2);

    assert_eq!(truncated, vec![image_only_message, newest]);
}

#[test]
fn retained_history_truncation_drops_image_only_messages_after_budget_is_spent() {
    let image_only_message = ResponseItem::Message {
        status: None,
        encrypted_content: None,
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image: ImageReference::Inline {
                image_url: "data:image/png;base64,abc".to_string(),
            },
            detail: None,
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let newest = message("user", "new", /*phase*/ None);
    let retained = vec![image_only_message, newest.clone()];

    let truncated = truncate_without_metadata(retained, /*max_tokens*/ 1);

    assert_eq!(truncated, vec![newest]);
}

#[tokio::test]
async fn collect_background_incomplete_does_not_publish_foreground_usage() {
    let usage = TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
        total_tokens: 120,
        ..Default::default()
    };
    let stream = response_stream(vec![Err(CodexErr::new(
        CodexErrorDetails::ResponseIncomplete(Box::new(
            codex_protocol::error::ResponseIncompleteError {
                partial_output: vec![],
                reason: "max_output_tokens".to_string(),
                response_id: Some("resp-background".to_string()),
                token_usage: Some(usage.clone()),
            },
        )),
    ))]);
    let (mut sess, turn_context, rx) =
        crate::session::tests::make_session_and_context_with_rx().await;
    let accounting = Arc::new(UsageRecorder {
        reports: Mutex::new(Vec::new()),
        decision: UsageAccountingDecision::Exhausted,
    });
    accounting.install(&mut sess);
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = collect_compaction_output(
        &sess,
        &turn_context,
        stream,
        CompactionRequestKind::Background(&cancellation),
    )
    .await;

    assert!(matches!(result, Err(ref err)
            if matches!(err.details(), CodexErrorDetails::ResponseIncomplete(_))));
    assert_eq!(sess.token_usage_info().await, None);
    assert!(rx.is_empty());
    assert_eq!(
        *accounting.reports.lock().unwrap(),
        vec![AgentUsageReport {
            thread_id: sess.thread_id(),
            turn_id: turn_context.sub_id.clone(),
            usage,
        }]
    );
}

#[tokio::test]
async fn background_compaction_charges_usage_even_if_output_is_missing() {
    let (mut sess, turn, _) = crate::session::tests::make_session_and_context_with_rx().await;
    let accounting = Arc::new(UsageRecorder {
        reports: Mutex::new(Vec::new()),
        decision: UsageAccountingDecision::Continue,
    });
    accounting.install(&mut sess);
    let usage = TokenUsage {
        output_tokens: 42,
        ..Default::default()
    };
    let stream = response_stream(vec![Ok(ResponseEvent::Completed {
        response_id: "resp-compact".into(),
        token_usage: Some(usage.clone()),
        usage_metadata: None,
        end_turn: Some(true),
    })]);
    let result = collect_compaction_output(
        &sess,
        &turn,
        stream,
        CompactionRequestKind::Background(&CancellationToken::new()),
    )
    .await;

    assert!(matches!(result, Err(error) if matches!(error.details(), CodexErrorDetails::Fatal(_))));
    assert_eq!(
        *accounting.reports.lock().unwrap(),
        [AgentUsageReport {
            thread_id: sess.thread_id(),
            turn_id: turn.sub_id.clone(),
            usage,
        }]
    );
}

#[tokio::test]
async fn collect_compaction_output_accepts_additional_output_items() {
    let compaction = ResponseItem::Compaction {
        id: None,
        encrypted_content: "encrypted".to_string(),
        internal_chat_message_metadata_passthrough: None,
    };
    let stream = response_stream(vec![
        Ok(ResponseEvent::OutputItemDone(message(
            "assistant",
            "IGNORED_COMPACT_REPLY",
            Some(MessagePhase::FinalAnswer),
        ))),
        Ok(ResponseEvent::OutputItemDone(compaction.clone())),
        Ok(ResponseEvent::Completed {
            response_id: "resp-compact".to_string(),
            token_usage: Some(TokenUsage {
                input_tokens: 123_456,
                cached_input_tokens: 7_890,
                cache_write_input_tokens: 0,
                output_tokens: 42,
                reasoning_output_tokens: 5,
                total_tokens: 123_498,
                codex_rollout_budget_units: None,
            }),
            usage_metadata: Some(codex_protocol::ResponseUsageMetadata {
                amount: Some("0.125".to_string()),
                metadata: Some(serde_json::json!({ "extra": { "label": "example" } })),
            }),
            end_turn: Some(true),
        }),
    ]);

    let (sess, turn_context, rx) = crate::session::tests::make_session_and_context_with_rx().await;
    let output = collect_compaction_output(
        &sess,
        &turn_context,
        stream,
        CompactionRequestKind::Foreground,
    )
    .await
    .expect("compaction should be collected");

    assert_eq!(output.compaction_output, compaction);
    assert_eq!(output.response_id, "resp-compact");
    let event = rx.recv().await.expect("raw response completion");
    let EventMsg::RawResponseCompleted(completed) = event.msg else {
        panic!("expected raw response completion, got {:?}", event.msg);
    };
    assert_eq!(completed.response_id, "resp-compact");
    assert_eq!(
        completed.usage_metadata,
        Some(codex_protocol::ResponseUsageMetadata {
            amount: Some("0.125".to_string()),
            metadata: Some(serde_json::json!({ "extra": { "label": "example" } })),
        })
    );
    assert_eq!(
        output.token_usage,
        Some(TokenUsage {
            input_tokens: 123_456,
            cached_input_tokens: 7_890,
            cache_write_input_tokens: 0,
            output_tokens: 42,
            reasoning_output_tokens: 5,
            total_tokens: 123_498,
            codex_rollout_budget_units: None,
        })
    );
}
