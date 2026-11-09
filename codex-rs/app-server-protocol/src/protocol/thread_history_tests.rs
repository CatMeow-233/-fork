use super::*;
use crate::protocol::v2::AgentMessageDelivery;
use crate::protocol::v2::AsyncUserInputQuestion;
use crate::protocol::v2::CommandExecutionSource;
use codex_extension_items::ExtensionItem as CoreExtensionItem;
use codex_extension_items::sleep::SleepItem as CoreSleepItem;
use codex_protocol::ThreadId;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem as CoreDynamicToolCallOutputContentItem;
use codex_protocol::items::CommandExecutionItem as CoreCommandExecutionItem;
use codex_protocol::items::CommandExecutionStatus as CoreCommandExecutionStatus;
use codex_protocol::items::EnteredReviewModeItem as CoreEnteredReviewModeItem;
use codex_protocol::items::ExitedReviewModeItem as CoreExitedReviewModeItem;
use codex_protocol::items::HookPromptFragment as CoreHookPromptFragment;
use codex_protocol::items::SubAgentActivityItem as CoreSubAgentActivityItem;
use codex_protocol::items::TurnItem as CoreTurnItem;
use codex_protocol::items::UserMessageItem as CoreUserMessageItem;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::mcp::CallToolResult;
use codex_protocol::models::AudioReference as CoreAudioReference;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::MessagePhase as CoreMessagePhase;
use codex_protocol::models::WebSearchAction as CoreWebSearchAction;
use codex_protocol::parse_command::ParsedCommand;
use codex_protocol::protocol::AgentReasoningEvent;
use codex_protocol::protocol::AgentReasoningRawContentEvent;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::DynamicToolCallResponseEvent;
use codex_protocol::protocol::EnteredReviewModeEvent;
use codex_protocol::protocol::ExecCommandBeginEvent;
use codex_protocol::protocol::ExecCommandEndEvent;
use codex_protocol::protocol::ExecCommandSource;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::ItemStartedEvent;
use codex_protocol::protocol::McpInvocation;
use codex_protocol::protocol::McpToolCallEndEvent;
use codex_protocol::protocol::PatchApplyBeginEvent;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::SubAgentActivityKind as CoreSubAgentActivityKind;
use codex_protocol::protocol::ThreadRolledBackEvent;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::UserMessageEvent;
use codex_protocol::protocol::WebSearchBeginEvent;
use codex_protocol::protocol::WebSearchEndEvent;
use codex_protocol::user_input::UserInput as CoreUserInput;
use codex_rollout::CompactedItem;
use codex_utils_absolute_path::test_support::PathBufExt;
use codex_utils_absolute_path::test_support::test_path_buf;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

fn indexed_sleep_item(id: &str, duration_ms: u64) -> ThreadItemPayload {
    ThreadItemPayload::Sleep(CoreSleepItem {
        id: id.to_string(),
        duration_ms,
    })
}

#[test]
fn pushed_duplicates_resolve_to_first_occurrence_before_and_after_indexing() {
    for count in [2, TURN_ITEM_INDEX_THRESHOLD, TURN_ITEM_INDEX_THRESHOLD * 2] {
        let mut index = TurnItemIndex::default();
        let mut items = Vec::new();
        let original = indexed_sleep_item("duplicate", /*duration_ms*/ 1);
        for _ in 0..count {
            index.push(&mut items, original.clone());
        }
        let updated = indexed_sleep_item("duplicate", /*duration_ms*/ 2);
        index.upsert(&mut items, updated.clone());
        // Appends after index activation must also keep the first position.
        index.push(&mut items, original.clone());
        index.upsert(&mut items, updated.clone());
        let appended = indexed_sleep_item("appended", /*duration_ms*/ 3);
        index.push(
            &mut items,
            indexed_sleep_item("appended", /*duration_ms*/ 1),
        );
        index.upsert(&mut items, appended.clone());

        let mut expected = vec![original; count + 1];
        expected[0] = updated;
        expected.push(appended);
        assert_eq!(items, expected);
    }
}

#[test]
fn paginated_snapshot_keeps_reasoning_and_compaction_lifecycle() {
    use codex_protocol::items::ContextCompactionItem;
    use codex_protocol::items::ReasoningItem;
    use codex_protocol::protocol::HasLegacyEvent;

    let mode = ThreadHistoryMode::Paginated;
    let mut builder = ThreadHistoryBuilder::new();
    let turn_id = "turn-1".to_string();
    builder.handle_event_for_history_mode(
        &EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: turn_id.clone(),
            turn_attribution: None,
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        mode,
    );
    let mut expected = Vec::new();
    for item in [
        CoreTurnItem::Reasoning(ReasoningItem {
            id: "reasoning".into(),
            summary_text: vec!["summary".into()],
            raw_content: vec!["content".into()],
        }),
        CoreTurnItem::ContextCompaction(ContextCompactionItem {
            id: "compaction".into(),
        }),
    ] {
        let started = ItemStartedEvent {
            thread_id: ThreadId::default(),
            turn_id: turn_id.clone(),
            item: item.clone(),
            started_at_ms: 10,
        };
        builder.handle_event_for_history_mode(&EventMsg::ItemStarted(started), mode);
        let completed = ItemCompletedEvent {
            content_token_count: None,
            thread_id: ThreadId::default(),
            turn_id: turn_id.clone(),
            item,
            started_at_ms: Some(10),
            completed_at_ms: 20,
        };
        expected.push(ThreadItem::from(&completed));
        let copies = completed.as_legacy_events(/*show_raw_agent_reasoning*/ true);
        builder.handle_event_for_history_mode(&EventMsg::ItemCompleted(completed), mode);
        for copy in copies {
            builder.handle_event_for_history_mode(&copy, mode);
        }
    }
    assert_eq!(builder.active_turn_snapshot().unwrap().items, expected);
}

#[test]
fn builds_multiple_turns_with_reasoning_items() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "First turn".into(),
            images: Some(vec!["https://example.com/one.png".into()]),
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "Hi there".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::AgentReasoning(AgentReasoningEvent {
            text: "thinking".into(),
        }),
        EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent {
            text: "full reasoning".into(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Second turn".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "Reply two".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
    ];

    let mut builder = ThreadHistoryBuilder::new();
    for event in &events {
        builder.handle_event(event);
    }
    let turns = builder.finish();
    assert_eq!(turns.len(), 2);

    let first = &turns[0];
    assert!(Uuid::parse_str(&first.id).is_ok());
    assert_eq!(first.status, TurnStatus::Completed);
    assert_eq!(first.items.len(), 3);
    assert_eq!(
        first.items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-1".into(),
            client_id: None,
            content: vec![
                UserInput::Text {
                    text: "First turn".into(),
                    text_elements: Vec::new(),
                },
                UserInput::Image {
                    image: ImageReference::Inline {
                        url: "https://example.com/one.png".into(),
                    },
                    detail: None,
                }
            ],
        }
    );
    assert_eq!(
        first.items[1].payload,
        ThreadItemPayload::AgentMessage {
            moderation: None,
            id: "item-2".into(),
            text: "Hi there".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }
    );
    assert_eq!(
        first.items[2].payload,
        ThreadItemPayload::Reasoning {
            id: "item-3".into(),
            summary: vec!["thinking".into()],
            content: vec!["full reasoning".into()],
        }
    );

    let second = &turns[1];
    assert!(Uuid::parse_str(&second.id).is_ok());
    assert_ne!(first.id, second.id);
    assert_eq!(second.items.len(), 2);
    assert_eq!(
        second.items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-4".into(),
            client_id: None,
            content: vec![UserInput::Text {
                text: "Second turn".into(),
                text_elements: Vec::new(),
            }],
        }
    );
    assert_eq!(
        second.items[1].payload,
        ThreadItemPayload::AgentMessage {
            moderation: None,
            id: "item-5".into(),
            text: "Reply two".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }
    );
}

#[test]
fn review_mode_events_replay_persisted_ids() {
    let events = vec![
        EventMsg::EnteredReviewMode(EnteredReviewModeEvent {
            target: ReviewTarget::Custom {
                instructions: "review this".into(),
            },
            user_facing_hint: Some("Review requested.".into()),
            turn_id: Some("turn-1".into()),
            item_id: Some("entered-review".into()),
        }),
        EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
            turn_id: Some("turn-1".into()),
            item_id: Some("exited-review".into()),
            review_output: None,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-1".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let mut builder = ThreadHistoryBuilder::new();
    for event in &events {
        builder.handle_event(event);
    }
    let turns = builder.finish();

    assert_eq!(turns[0].id, "turn-1");
    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::EnteredReviewMode {
                id: "entered-review".into(),
                review: "Review requested.".into(),
            },
            &ThreadItemPayload::ExitedReviewMode {
                id: "exited-review".into(),
                review: REVIEW_FALLBACK_MESSAGE.into(),
            },
        ]
    );
}

#[test]
fn review_mode_items_replay_without_turn_started() {
    let thread_id = ThreadId::new();
    let entered = CoreTurnItem::EnteredReviewMode(CoreEnteredReviewModeItem {
        id: "entered-review".into(),
        target: ReviewTarget::Custom {
            instructions: "review this".into(),
        },
        user_facing_hint: "Review requested.".into(),
    });
    let exited = CoreTurnItem::ExitedReviewMode(CoreExitedReviewModeItem {
        id: "exited-review".into(),
        review_output: None,
    });
    let events = vec![
        EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id,
            turn_id: "turn-1".into(),
            item: entered,
            started_at_ms: Some(0),
            completed_at_ms: 0,
        }),
        EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id,
            turn_id: "turn-1".into(),
            item: exited,
            started_at_ms: Some(0),
            completed_at_ms: 0,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-1".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let mut builder = ThreadHistoryBuilder::new();
    for event in &events {
        builder.handle_event(event);
    }
    let turns = builder.finish();

    assert_eq!(turns[0].id, "turn-1");
    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::EnteredReviewMode {
                id: "entered-review".into(),
                review: "Review requested.".into(),
            },
            &ThreadItemPayload::ExitedReviewMode {
                id: "exited-review".into(),
                review: REVIEW_FALLBACK_MESSAGE.into(),
            },
        ]
    );
}

#[test]
fn rebuilds_user_message_attachments_from_legacy_events() {
    let local_image_path = PathBuf::from("/tmp/local.png");
    let local_audio_path = PathBuf::from("/tmp/local.wav");
    let events = vec![RolloutItem::EventMsg(EventMsg::UserMessage(
        UserMessageEvent {
            client_id: None,
            message: "inspect these".into(),
            images: Some(vec!["https://example.com/image.png".into()]),
            image_details: vec![Some(ImageDetail::Original)],
            file_ids: Some(vec!["file_123".into()]),
            file_id_details: vec![Some(ImageDetail::High)],
            image_order: vec![UserMessageImageKind::File, UserMessageImageKind::Inline],
            local_images: vec![local_image_path.clone()],
            local_image_details: vec![Some(ImageDetail::Original)],
            audio: Some(vec!["https://example.com/audio.mp3".into()]),
            audio_references: None,
            audio_order: Vec::new(),
            local_audio: vec![local_audio_path.clone()],
            text_elements: Vec::new(),
        },
    ))];

    let turns = build_turns_from_rollout_items(&events);

    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-1".into(),
            client_id: None,
            content: vec![
                UserInput::Text {
                    text: "inspect these".into(),
                    text_elements: Vec::new(),
                },
                UserInput::Image {
                    image: ImageReference::File {
                        file_id: "file_123".into(),
                    },
                    detail: Some(ImageDetail::High),
                },
                UserInput::Image {
                    image: ImageReference::Inline {
                        url: "https://example.com/image.png".into(),
                    },
                    detail: Some(ImageDetail::Original),
                },
                UserInput::LocalImage {
                    path: local_image_path,
                    detail: Some(ImageDetail::Original),
                },
                UserInput::Audio {
                    audio: crate::AudioReference::Inline {
                        url: "https://example.com/audio.mp3".into()
                    }
                },
                UserInput::LocalAudio {
                    path: local_audio_path,
                },
            ],
        }
    );
}

/// Incomplete ordering metadata must not drop references from the legacy split arrays.
#[test]
fn incomplete_image_order_falls_back_to_legacy_order() {
    let events = vec![RolloutItem::EventMsg(EventMsg::UserMessage(
        UserMessageEvent {
            images: Some(vec!["https://example.com/image.png".into()]),
            file_ids: Some(vec!["file_123".into()]),
            image_order: vec![UserMessageImageKind::File],
            ..Default::default()
        },
    ))];

    let turns = build_turns_from_rollout_items(&events);

    assert_eq!(
        turns[0].items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-1".into(),
            client_id: None,
            content: vec![
                UserInput::Image {
                    image: ImageReference::Inline {
                        url: "https://example.com/image.png".into(),
                    },
                    detail: None,
                },
                UserInput::Image {
                    image: ImageReference::File {
                        file_id: "file_123".into(),
                    },
                    detail: None,
                },
            ],
        }
    );
}

#[test]
fn rebuilds_uploaded_and_inline_audio_in_order_from_legacy_events() {
    let item = CoreUserMessageItem::new(&[
        CoreUserInput::Audio {
            audio: CoreAudioReference::File {
                file_id: "file_audio".into(),
            },
        },
        CoreUserInput::Audio {
            audio: CoreAudioReference::Inline {
                audio_url: "data:audio/wav;base64,YXVkaW8=".into(),
            },
        },
    ]);
    let turns = build_turns_from_rollout_items(&[RolloutItem::EventMsg(item.as_legacy_event())]);

    assert_eq!(
        serde_json::to_value(&turns[0].items[0]).unwrap()["content"],
        serde_json::json!([
            {"type": "audio", "fileId": "file_audio"},
            {"type": "audio", "url": "data:audio/wav;base64,YXVkaW8="},
        ]),
    );
}

#[test]
fn ignores_user_message_item_lifecycle_events() {
    let turn_id = "turn-1";
    let thread_id = ThreadId::new();
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::ItemStarted(ItemStartedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item: CoreTurnItem::UserMessage(CoreUserMessageItem {
                id: "user-item-id".to_string(),
                client_id: None,
                content: Vec::new(),
            }),
            started_at_ms: 0,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: turn_id.to_string(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 1);
    assert_eq!(
        turns[0].items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-1".into(),
            client_id: None,
            content: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
        }
    );
}

#[test]
fn rebuilds_sleep_item_from_persisted_completion() {
    let turn_id = "turn-1";
    let thread_id = ThreadId::new();
    let sleep_item = CoreTurnItem::Extension(CoreExtensionItem::Sleep(CoreSleepItem {
        id: "sleep-1".to_string(),
        duration_ms: 1_000,
    }));
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id,
            turn_id: turn_id.to_string(),
            item: sleep_item,
            started_at_ms: Some(0),
            completed_at_ms: 1_000,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: turn_id.to_string(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);

    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [&ThreadItemPayload::Sleep(CoreSleepItem {
            id: "sleep-1".to_string(),
            duration_ms: 1_000,
        })]
    );
    assert_eq!(
        (
            turns[0].items[0].started_at_ms,
            turns[0].items[0].completed_at_ms
        ),
        (Some(0), Some(1_000))
    );
}

#[test]
fn indexed_items_stay_with_their_turn_after_rollback() {
    let mut builder = ThreadHistoryBuilder::new();
    let mut expected: Vec<_> = (0..64)
        .map(|i| indexed_sleep_item(&i.to_string(), /*duration_ms*/ 1))
        .collect();
    for turn_id in ["retained", "discarded"] {
        builder.ensure_turn().id = turn_id.into();
        for item in &expected {
            builder.upsert_item_in_current_turn(item.clone());
        }
        builder.finish_current_turn();
        // Reuse IDs at different positions in the other turn.
        expected.reverse();
    }
    builder.handle_thread_rollback(&ThreadRolledBackEvent { num_turns: 1 });
    expected[0] = indexed_sleep_item("0", /*duration_ms*/ 2);
    builder.upsert_item_in_turn_id("retained", expected[0].clone());
    assert_eq!(
        builder.finish()[0].item_payloads().collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
}

#[test]
fn rebuilds_extension_image_generation_item_from_persisted_completion() {
    let turn_id = "turn-1";
    let thread_id = ThreadId::new();
    let saved_path = test_path_buf("/tmp/image-1.png").abs();
    let input_images = vec![ImageGenerationInputImage {
        path: test_path_buf("/tmp/reference.png").abs(),
        sha256: "ab".repeat(32),
    }];
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id,
            turn_id: turn_id.to_string(),
            item: CoreTurnItem::Extension(CoreExtensionItem::ImageGeneration(
                ImageGenerationItem {
                    id: "image-1".to_string(),
                    status: "completed".to_string(),
                    revised_prompt: Some("A blue square".to_string()),
                    image: GeneratedImageRef::Inline {
                        result: "cG5n".to_string(),
                    },
                    transparent_background: Some(true),
                    failure: None,
                    saved_path: Some(saved_path.clone()),
                    input_images: input_images.clone(),
                    imagegen_request_id: None,
                    generation_id: None,
                },
            )),
            started_at_ms: Some(0),
            completed_at_ms: 1_000,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: turn_id.to_string(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];
    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .map(|item| {
            serde_json::from_value(serde_json::to_value(item).expect("serialize rollout"))
                .expect("deserialize rollout")
        })
        .collect::<Vec<_>>();

    let turns = build_turns_from_rollout_items(&items);

    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [&ThreadItemPayload::ImageGeneration(ImageGenerationItem {
            id: "image-1".to_string(),
            status: "completed".to_string(),
            revised_prompt: Some("A blue square".to_string()),
            image: GeneratedImageRef::Inline {
                result: "cG5n".to_string(),
            },
            transparent_background: Some(true),
            failure: None,
            saved_path: Some(saved_path),
            input_images,
            imagegen_request_id: None,
            generation_id: None,
        })]
    );
}

#[test]
fn preserves_command_plugin_id_and_redacts_secrets_across_legacy_upsert() {
    let turn_id = "turn-1";
    let thread_id = ThreadId::new();
    let command = vec![
        "git".to_string(),
        "-c".to_string(),
        "http.extraHeader=Authorization: Bearer example_synthetic_bearer_token_123456".to_string(),
        "push".to_string(),
    ];
    let parsed_cmd = vec![ParsedCommand::Unknown {
            cmd: "git -c 'http.extraHeader=Authorization: Bearer example_synthetic_bearer_token_123456' push"
                .to_string(),
        }];
    let command_item = CoreTurnItem::CommandExecution(CoreCommandExecutionItem {
        model_context: None,
        sandbox_type: None,
        id: "exec-1".to_string(),
        plugin_id: Some("sample@openai-curated".to_string()),
        script_path: Some("scripts/run.py".to_string()),
        process_id: Some("pid-1".to_string()),
        command: command.clone(),
        cwd: test_path_buf("/tmp").abs().into(),
        parsed_cmd: parsed_cmd.clone(),
        source: ExecCommandSource::Agent,
        interaction_input: None,
        status: CoreCommandExecutionStatus::Completed,
        aggregated_output: Some("hello world\n".to_string()),
        exit_code: Some(0),
        duration: Some(Duration::from_millis(12)),
    });
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
            call_id: "exec-1".to_string(),
            plugin_id: Some("sample@openai-curated".to_string()),
            script_path: Some("scripts/run.py".to_string()),
            process_id: Some("pid-1".to_string()),
            turn_id: turn_id.to_string(),
            started_at_ms: 0,
            command: command.clone(),
            cwd: test_path_buf("/tmp").abs().into(),
            parsed_cmd: parsed_cmd.clone(),
            source: ExecCommandSource::Agent,
            interaction_input: None,
        }),
        EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id,
            turn_id: turn_id.to_string(),
            item: command_item,
            started_at_ms: Some(0),
            completed_at_ms: 1_000,
        }),
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "exec-1".to_string(),
            plugin_id: Some("sample@openai-curated".to_string()),
            script_path: Some("scripts/run.py".to_string()),
            process_id: Some("pid-1".to_string()),
            turn_id: turn_id.to_string(),
            completed_at_ms: 1_000,
            command,
            cwd: test_path_buf("/tmp").abs().into(),
            parsed_cmd,
            source: ExecCommandSource::Agent,
            interaction_input: None,
            aggregated_output: "hello world\n".to_string(),
            exit_code: 0,
            duration: Duration::from_millis(12),
            status: CoreExecCommandStatus::Completed,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: turn_id.to_string(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();

    assert_eq!(
        build_turns_from_rollout_items(&items[..2])[0]
            .item_payloads()
            .collect::<Vec<_>>(),
        [&ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "exec-1".to_string(),
            plugin_id: Some("sample@openai-curated".to_string()),
            script_path: Some("scripts/run.py".to_string()),
            command: "git -c 'http.extraHeader=Authorization: Bearer [REDACTED_SECRET]' push"
                .to_string(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: Some("pid-1".to_string()),
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::InProgress,
            command_actions: vec![CommandAction::Unknown {
                command: "git -c 'http.extraHeader=Authorization: Bearer [REDACTED_SECRET]' push"
                    .to_string(),
            }],
            aggregated_output: None,
            exit_code: None,
            duration_ms: None,
        }]
    );
    let turns = build_turns_from_rollout_items(&items);

    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [&ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "exec-1".to_string(),
            plugin_id: Some("sample@openai-curated".to_string()),
            script_path: Some("scripts/run.py".to_string()),
            command: "git -c 'http.extraHeader=Authorization: Bearer [REDACTED_SECRET]' push"
                .to_string(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: Some("pid-1".to_string()),
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::Completed,
            command_actions: vec![CommandAction::Unknown {
                command: "git -c 'http.extraHeader=Authorization: Bearer [REDACTED_SECRET]' push"
                    .to_string(),
            }],
            aggregated_output: Some("hello world\n".to_string()),
            exit_code: Some(0),
            duration_ms: Some(12),
        }]
    );
}

#[test]
fn preserves_user_message_client_id_from_legacy_event() {
    let turn_id = "turn-1";
    let thread_id = ThreadId::new();
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::ItemStarted(ItemStartedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item: CoreTurnItem::UserMessage(CoreUserMessageItem {
                id: "user-item-id".to_string(),
                client_id: Some("client-message-1".to_string()),
                content: vec![codex_protocol::user_input::UserInput::Text {
                    text: "hello".into(),
                    text_elements: Vec::new(),
                }],
            }),
            started_at_ms: 0,
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: Some("client-message-1".to_string()),
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: turn_id.to_string(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].items,
        [ThreadItem::untimed(ThreadItemPayload::UserMessage {
            id: "item-1".into(),
            client_id: Some("client-message-1".to_string()),
            content: vec![UserInput::Text {
                text: "hello".into(),
                text_elements: Vec::new(),
            }],
        })]
    );
}

#[test]
fn preserves_agent_message_phase_delivery_and_questions_in_history() {
    let questions = vec![
        AsyncUserInputQuestion {
            title: "Which environment?".into(),
            options: Some(vec!["Staging".into(), "Production".into()]),
        },
        AsyncUserInputQuestion {
            title: "Anything else?".into(),
            options: None,
        },
    ];
    let events = vec![EventMsg::AgentMessage(AgentMessageEvent {
        moderation: None,
        message: "Final reply".into(),
        phase: Some(CoreMessagePhase::FinalAnswer),
        memory_citation: None,
        delivery: Some(AgentMessageDelivery::Async),
        questions: Some(questions.clone()),
    })];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].items[0].payload,
        ThreadItemPayload::AgentMessage {
            moderation: None,
            id: "item-1".into(),
            text: "Final reply".into(),
            phase: Some(CoreMessagePhase::FinalAnswer),
            memory_citation: None,
            delivery: Some(AgentMessageDelivery::Async),
            questions: Some(questions),
        }
    );
}

#[test]
fn replays_file_backed_image_generation_end_events_into_turn_history() {
    let items = vec![
            RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
                turn_attribution: None,
                turn_id: "turn-image".into(),
                root_turn_id: None,
                trace_id: None,
                started_at: None,
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            })),
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                client_id: None,
                message: "generate an image".into(),
                images: None,
                text_elements: Vec::new(),
                local_images: Vec::new(),
                ..Default::default()
            })),
            RolloutItem::EventMsg(EventMsg::ImageGenerationEnd(ImageGenerationEndEvent {
                call_id: "ig_123".into(),
                status: "completed".into(),
                revised_prompt: Some("final prompt".into()),
                image: GeneratedImageRef::File {
                    file_id: "file_123".into(),
                },
                transparent_background: Some(true),
                failure: Some(
                    codex_extension_items::image_generation::ImageGenerationFailure::UsageLimitExceeded {
                        limit_id: "image_gen".into(),
                        resets_at: Some(1_786_150_800),
                    },
                ),
                saved_path: Some(test_path_buf("/tmp/ig_123.png").abs()),
                input_images: vec![ImageGenerationInputImage {
                    path: test_path_buf("/tmp/reference.png").abs(),
                    sha256: "ab".repeat(32),
                }],
                generation_id: None,
            })),
            RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
                root_turn_id: None,
                turn_id: "turn-image".into(),
                started_at: None,
                last_agent_message: None,
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            })),
        ];

    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(
            turns[0],
            Turn {
                id: "turn-image".into(),
                root_turn_id: None,
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                items_view: TurnItemsView::Full,
                items: vec![
                    ThreadItem::untimed(ThreadItemPayload::UserMessage {
                        id: "item-1".into(),
                        client_id: None,
                        content: vec![UserInput::Text {
                            text: "generate an image".into(),
                            text_elements: Vec::new(),
                        }],
                    }),
                    ThreadItem::untimed(ThreadItemPayload::ImageGeneration(ImageGenerationItem {
                        id: "ig_123".into(),
                        status: "completed".into(),
                        revised_prompt: Some("final prompt".into()),
                        image: GeneratedImageRef::File {
                            file_id: "file_123".into(),
                        },
                        transparent_background: Some(true),
                        failure: Some(
                            codex_extension_items::image_generation::ImageGenerationFailure::UsageLimitExceeded {
                                limit_id: "image_gen".into(),
                                resets_at: Some(1_786_150_800),
                            },
                        ),
                        saved_path: Some(test_path_buf("/tmp/ig_123.png").abs()),
                        input_images: vec![ImageGenerationInputImage {
                    path: test_path_buf("/tmp/reference.png").abs(),
                    sha256: "ab".repeat(32),
                }],
                        imagegen_request_id: None,
                        generation_id: None,
                    })),
                ],
            }
        );
}

#[test]
fn splits_reasoning_when_interleaved() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Turn start".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentReasoning(AgentReasoningEvent {
            text: "first summary".into(),
        }),
        EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent {
            text: "first content".into(),
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "interlude".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::AgentReasoning(AgentReasoningEvent {
            text: "second summary".into(),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    let turn = &turns[0];
    assert_eq!(turn.items.len(), 4);

    assert_eq!(
        turn.items[1].payload,
        ThreadItemPayload::Reasoning {
            id: "item-2".into(),
            summary: vec!["first summary".into()],
            content: vec!["first content".into()],
        }
    );
    assert_eq!(
        turn.items[3].payload,
        ThreadItemPayload::Reasoning {
            id: "item-4".into(),
            summary: vec!["second summary".into()],
            content: Vec::new(),
        }
    );
}

#[test]
fn marks_turn_as_interrupted_when_aborted() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Please do the thing".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "Working...".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::TurnAborted(TurnAbortedEvent {
            root_turn_id: None,
            turn_id: Some("turn-1".into()),
            started_at: None,
            reason: TurnAbortReason::Replaced,
            error: None,
            completed_at: None,
            duration_ms: None,
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Let's try again".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "Second attempt complete.".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 2);

    let first_turn = &turns[0];
    assert_eq!(first_turn.status, TurnStatus::Interrupted);
    assert_eq!(first_turn.items.len(), 2);
    assert_eq!(
        first_turn.items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-1".into(),
            client_id: None,
            content: vec![UserInput::Text {
                text: "Please do the thing".into(),
                text_elements: Vec::new(),
            }],
        }
    );
    assert_eq!(
        first_turn.items[1].payload,
        ThreadItemPayload::AgentMessage {
            moderation: None,
            id: "item-2".into(),
            text: "Working...".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }
    );

    let second_turn = &turns[1];
    assert_eq!(second_turn.status, TurnStatus::Completed);
    assert_eq!(second_turn.items.len(), 2);
    assert_eq!(
        second_turn.items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-3".into(),
            client_id: None,
            content: vec![UserInput::Text {
                text: "Let's try again".into(),
                text_elements: Vec::new(),
            }],
        }
    );
    assert_eq!(
        second_turn.items[1].payload,
        ThreadItemPayload::AgentMessage {
            moderation: None,
            id: "item-4".into(),
            text: "Second attempt complete.".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }
    );
}

#[test]
fn drops_last_turns_on_thread_rollback() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "First".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "A1".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Second".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "A2".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::ThreadRolledBack(ThreadRolledBackEvent { num_turns: 1 }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Third".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "A3".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id, "rollout-0");
    assert_eq!(turns[1].id, "rollout-5");
    assert_ne!(turns[0].id, turns[1].id);
    assert_eq!(turns[0].status, TurnStatus::Completed);
    assert_eq!(turns[1].status, TurnStatus::Completed);
    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::UserMessage {
                id: "item-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "First".into(),
                    text_elements: Vec::new(),
                }],
            },
            &ThreadItemPayload::AgentMessage {
                moderation: None,
                id: "item-2".into(),
                text: "A1".into(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
        ]
    );
    assert_eq!(
        turns[1].item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::UserMessage {
                id: "item-3".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "Third".into(),
                    text_elements: Vec::new(),
                }],
            },
            &ThreadItemPayload::AgentMessage {
                moderation: None,
                id: "item-4".into(),
                text: "A3".into(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
        ]
    );
}

#[test]
fn thread_rollback_clears_all_turns_when_num_turns_exceeds_history() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "One".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "A1".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Two".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "A2".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::ThreadRolledBack(ThreadRolledBackEvent { num_turns: 99 }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns, Vec::<Turn>::new());
}

#[test]
fn uses_explicit_turn_boundaries_for_mid_turn_steering() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Start".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "Steer".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, "turn-a");
    assert_eq!(
        turns[0].item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::UserMessage {
                id: "item-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "Start".into(),
                    text_elements: Vec::new(),
                }],
            },
            &ThreadItemPayload::UserMessage {
                id: "item-2".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "Steer".into(),
                    text_elements: Vec::new(),
                }],
            },
        ]
    );
}

#[test]
fn reconstructs_tool_items_from_persisted_completion_events() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "run tools".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id: "search-1".into(),
            query: "codex".into(),
            action: CoreWebSearchAction::Search {
                query: Some("codex".into()),
                queries: None,
            },
            results: Some(vec![serde_json::json!({
                "type": "text_result",
                "ref_id": "turn0search0",
                "url": "https://example.com/codex",
            })]),
        }),
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "exec-1".into(),
            plugin_id: None,
            script_path: None,
            process_id: Some("pid-1".into()),
            turn_id: "turn-1".into(),
            completed_at_ms: 0,
            command: vec!["echo".into(), "hello world".into()],
            cwd: test_path_buf("/tmp").abs().into(),
            parsed_cmd: vec![ParsedCommand::Unknown {
                cmd: "echo hello world".into(),
            }],
            source: ExecCommandSource::Agent,
            interaction_input: None,
            aggregated_output: "hello world\n".into(),
            exit_code: 0,
            duration: Duration::from_millis(12),
            status: CoreExecCommandStatus::Completed,
        }),
        EventMsg::McpToolCallEnd(McpToolCallEndEvent {
            turn_id: String::new(),
            call_id: "mcp-1".into(),
            invocation: McpInvocation {
                server: "docs".into(),
                tool: "lookup".into(),
                arguments: Some(serde_json::json!({"id":"123"})),
            },
            connector_id: None,
            mcp_app_resource_uri: None,
            mcp_app_ui: None,
            link_id: None,
            app_name: None,
            action_name: None,
            plugin_id: None,
            read_only_hint: None,
            duration: Duration::from_millis(8),
            result: Err("boom".into()),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 4);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::WebSearch(WebSearchItem {
            id: "search-1".into(),
            query: "codex".into(),
            action: Some(WebSearchAction::Search {
                query: Some("codex".into()),
                queries: None,
            }),
            results: Some(vec![serde_json::json!({
                "type": "text_result",
                "ref_id": "turn0search0",
                "url": "https://example.com/codex",
            })]),
        })
    );
    assert_eq!(
        turns[0].items[2].payload,
        ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "exec-1".into(),
            plugin_id: None,
            script_path: None,
            command: "echo 'hello world'".into(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: Some("pid-1".into()),
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::Completed,
            command_actions: vec![CommandAction::Unknown {
                command: "echo hello world".into(),
            }],
            aggregated_output: Some("hello world\n".into()),
            exit_code: Some(0),
            duration_ms: Some(12),
        }
    );
    assert_eq!(
        turns[0].items[3].payload,
        ThreadItemPayload::McpToolCall {
            id: "mcp-1".into(),
            server: "docs".into(),
            tool: "lookup".into(),
            status: McpToolCallStatus::Failed,
            arguments: serde_json::json!({"id":"123"}),
            app_context: None,
            mcp_app_resource_uri: None,
            mcp_app_ui: None,
            plugin_id: None,
            read_only_hint: None,
            result: None,
            error: Some(McpToolCallError {
                message: "boom".into(),
            }),
            duration_ms: Some(8),
        }
    );
}

#[test]
fn reconstructs_mcp_tool_result_meta_from_persisted_completion_events() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::McpToolCallEnd(McpToolCallEndEvent {
            turn_id: String::new(),
            call_id: "mcp-1".into(),
            invocation: McpInvocation {
                server: "docs".into(),
                tool: "lookup".into(),
                arguments: Some(serde_json::json!({"id":"123"})),
            },
            connector_id: Some("calendar".into()),
            mcp_app_resource_uri: Some("ui://widget/lookup.html".into()),
            mcp_app_ui: None,
            link_id: Some("link_calendar".into()),
            app_name: Some("Calendar".into()),
            action_name: Some("lookup".into()),
            plugin_id: Some("sample@test".into()),
            read_only_hint: Some(false),
            duration: Duration::from_millis(8),
            result: Ok(CallToolResult {
                content: vec![serde_json::json!({
                    "type": "text",
                    "text": "result"
                })],
                structured_content: Some(serde_json::json!({"id":"123"})),
                is_error: Some(false),
                meta: Some(serde_json::json!({
                    "ui/resourceUri": "ui://widget/lookup.html"
                })),
            }),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].items[0].payload,
        ThreadItemPayload::McpToolCall {
            id: "mcp-1".into(),
            server: "docs".into(),
            tool: "lookup".into(),
            status: McpToolCallStatus::Completed,
            arguments: serde_json::json!({"id":"123"}),
            app_context: Some(McpToolCallAppContext {
                connector_id: "calendar".into(),
                link_id: Some("link_calendar".into()),
                resource_uri: Some("ui://widget/lookup.html".into()),
                app_name: Some("Calendar".into()),
                action_name: Some("lookup".into()),
            }),
            mcp_app_resource_uri: Some("ui://widget/lookup.html".into()),
            mcp_app_ui: None,
            plugin_id: Some("sample@test".into()),
            read_only_hint: Some(false),
            result: Some(Box::new(McpToolCallResult {
                content: vec![serde_json::json!({
                    "type": "text",
                    "text": "result"
                })],
                structured_content: Some(serde_json::json!({"id":"123"})),
                meta: Some(serde_json::json!({
                    "ui/resourceUri": "ui://widget/lookup.html"
                })),
            })),
            error: None,
            duration_ms: Some(8),
        }
    );
}

#[test]
fn reconstructs_dynamic_tool_items_from_request_and_response_events() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "run dynamic tool".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::DynamicToolCallRequest(codex_protocol::dynamic_tools::DynamicToolCallRequest {
            call_id: "dyn-1".into(),
            turn_id: "turn-1".into(),
            started_at_ms: 0,
            namespace: Some("codex_app".into()),
            tool: "lookup_ticket".into(),
            arguments: serde_json::json!({"id":"ABC-123"}),
        }),
        EventMsg::DynamicToolCallResponse(DynamicToolCallResponseEvent {
            call_id: "dyn-1".into(),
            turn_id: "turn-1".into(),
            completed_at_ms: 0,
            namespace: Some("codex_app".into()),
            tool: "lookup_ticket".into(),
            arguments: serde_json::json!({"id":"ABC-123"}),
            content_items: vec![
                CoreDynamicToolCallOutputContentItem::InputText {
                    text: "Ticket is open".into(),
                },
                CoreDynamicToolCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAA".into(),
                },
                CoreDynamicToolCallOutputContentItem::InputAudio {
                    audio_url: "data:audio/wav;base64,YXVkaW8=".into(),
                },
            ],
            success: true,
            error: None,
            duration: Duration::from_millis(42),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::DynamicToolCall {
            id: "dyn-1".into(),
            namespace: Some("codex_app".into()),
            tool: "lookup_ticket".into(),
            arguments: serde_json::json!({"id":"ABC-123"}),
            status: DynamicToolCallStatus::Completed,
            content_items: Some(vec![
                DynamicToolCallOutputContentItem::InputText {
                    text: "Ticket is open".into(),
                },
                DynamicToolCallOutputContentItem::InputImage {
                    image_url: "data:image/png;base64,AAA".into(),
                },
                DynamicToolCallOutputContentItem::InputAudio {
                    audio_url: "data:audio/wav;base64,YXVkaW8=".into(),
                },
            ]),
            success: Some(true),
            duration_ms: Some(42),
        }
    );
}

#[test]
fn reconstructs_declined_exec_and_patch_items() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "run tools".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "exec-declined".into(),
            plugin_id: None,
            script_path: None,
            process_id: Some("pid-2".into()),
            turn_id: "turn-1".into(),
            completed_at_ms: 0,
            command: vec!["ls".into()],
            cwd: test_path_buf("/tmp").abs().into(),
            parsed_cmd: vec![ParsedCommand::Unknown { cmd: "ls".into() }],
            source: ExecCommandSource::Agent,
            interaction_input: None,
            aggregated_output: "exec command rejected by user".into(),
            exit_code: -1,
            duration: Duration::ZERO,
            status: CoreExecCommandStatus::Declined,
        }),
        EventMsg::PatchApplyEnd(PatchApplyEndEvent {
            call_id: "patch-declined".into(),
            turn_id: "turn-1".into(),
            stdout: String::new(),
            stderr: "patch rejected by user".into(),
            success: false,
            changes: [(
                PathBuf::from("README.md"),
                codex_protocol::protocol::FileChange::Add {
                    content: "hello\n".into(),
                },
            )]
            .into_iter()
            .collect(),
            status: CorePatchApplyStatus::Declined,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 3);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "exec-declined".into(),
            plugin_id: None,
            script_path: None,
            command: "ls".into(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: Some("pid-2".into()),
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::Declined,
            command_actions: vec![CommandAction::Unknown {
                command: "ls".into(),
            }],
            aggregated_output: Some("exec command rejected by user".into()),
            exit_code: Some(-1),
            duration_ms: Some(0),
        }
    );
    assert_eq!(
        turns[0].items[2].payload,
        ThreadItemPayload::FileChange {
            id: "patch-declined".into(),
            changes: vec![FileUpdateChange {
                path: "README.md".into(),
                kind: PatchChangeKind::Add,
                diff: "hello\n".into(),
            }],
            status: PatchApplyStatus::Declined,
        }
    );
}

#[test]
fn reconstructs_declined_guardian_command_item() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "review this command".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::GuardianAssessment(GuardianAssessmentEvent {
            review_reason: None,
            model_context: None,
            id: "review-guardian-exec".into(),
            target_item_id: Some("guardian-exec".into()),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            turn_id: "turn-1".into(),
            started_at_ms: 1_000,
            completed_at_ms: None,
            status: GuardianAssessmentStatus::InProgress,
            risk_level: None,
            user_authorization: None,
            rationale: None,
            decision_source: None,
            action: serde_json::from_value(serde_json::json!({
                "type": "command",
                "source": "shell",
                "command": "rm -rf /tmp/guardian",
                "cwd": test_path_buf("/tmp"),
            }))
            .expect("guardian action"),
        }),
        EventMsg::GuardianAssessment(GuardianAssessmentEvent {
            review_reason: None,
            model_context: None,
            id: "review-guardian-exec".into(),
            target_item_id: Some("guardian-exec".into()),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            turn_id: "turn-1".into(),
            started_at_ms: 1_000,
            completed_at_ms: Some(1_042),
            status: GuardianAssessmentStatus::Denied,
            risk_level: Some(codex_protocol::protocol::GuardianRiskLevel::High),
            user_authorization: Some(codex_protocol::protocol::GuardianUserAuthorization::Low),
            rationale: Some("Would delete user data.".into()),
            decision_source: Some(
                codex_protocol::protocol::GuardianAssessmentDecisionSource::Agent,
            ),
            action: serde_json::from_value(serde_json::json!({
                "type": "command",
                "source": "shell",
                "command": "rm -rf /tmp/guardian",
                "cwd": test_path_buf("/tmp"),
            }))
            .expect("guardian action"),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    let item = &turns[0].items[1];
    assert_eq!(
        (item.started_at_ms, item.completed_at_ms),
        (Some(1_000), Some(1_042))
    );
    assert_eq!(
        item.payload,
        ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "guardian-exec".into(),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            command: "rm -rf /tmp/guardian".into(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: None,
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::Declined,
            command_actions: vec![CommandAction::Unknown {
                command: "rm -rf /tmp/guardian".into(),
            }],
            aggregated_output: None,
            exit_code: None,
            duration_ms: None,
        }
    );
}

#[test]
fn reconstructs_in_progress_guardian_execve_item() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-1".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "run a subcommand".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::GuardianAssessment(GuardianAssessmentEvent {
            review_reason: None,
            model_context: None,
            id: "review-guardian-execve".into(),
            target_item_id: Some("guardian-execve".into()),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            turn_id: "turn-1".into(),
            started_at_ms: 2_000,
            completed_at_ms: None,
            status: GuardianAssessmentStatus::InProgress,
            risk_level: None,
            user_authorization: None,
            rationale: None,
            decision_source: None,
            action: serde_json::from_value(serde_json::json!({
                "type": "execve",
                "source": "shell",
                "program": "/bin/rm",
                "argv": ["/usr/bin/rm", "-f", "/tmp/file.sqlite"],
                "cwd": test_path_buf("/tmp"),
            }))
            .expect("guardian action"),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "guardian-execve".into(),
            plugin_id: Some("sample@openai-curated".into()),
            script_path: Some("scripts/run.py".into()),
            command: "/bin/rm -f /tmp/file.sqlite".into(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: None,
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::InProgress,
            command_actions: vec![CommandAction::Unknown {
                command: "/bin/rm -f /tmp/file.sqlite".into(),
            }],
            aggregated_output: None,
            exit_code: None,
            duration_ms: None,
        }
    );
}

#[test]
fn assigns_late_legacy_mcp_completion_to_original_turn() {
    use codex_protocol::items::McpToolCallItem;
    use codex_protocol::items::McpToolCallStatus as CoreMcpToolCallStatus;
    use codex_protocol::protocol::HasLegacyEvent;

    let completion = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-a".into(),
        started_at_ms: None,
        completed_at_ms: 0,
        item: CoreTurnItem::McpToolCall(McpToolCallItem {
            id: "mcp-1".into(),
            server: "slack".into(),
            tool: "search".into(),
            arguments: serde_json::json!({"query": "incident"}),
            connector_id: None,
            mcp_app_resource_uri: None,
            mcp_app_ui: None,
            link_id: None,
            app_name: None,
            action_name: None,
            plugin_id: None,
            read_only_hint: Some(true),
            status: CoreMcpToolCallStatus::Completed,
            result: Some(CallToolResult {
                content: vec![serde_json::json!({"type": "text", "text": "Found incident"})],
                structured_content: None,
                is_error: None,
                meta: None,
            }),
            error: None,
            duration: Some(Duration::from_millis(8)),
        }),
    };
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            message: "Find related incident discussions".into(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-b".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            message: "Explain retry logic while that runs".into(),
            ..Default::default()
        }),
    ];
    let mut items: Vec<_> = events.into_iter().map(RolloutItem::EventMsg).collect();
    let mut expected = build_turns_from_rollout_items(&items);
    expected[0]
        .items
        .push(ThreadItem::untimed(ThreadItemPayload::from(
            completion.item.clone(),
        )));

    let legacy = completion.as_legacy_events(/*show_raw_agent_reasoning*/ false);
    let serialized = serde_json::to_string(&legacy[0]).unwrap();
    items.push(RolloutItem::EventMsg(
        serde_json::from_str(&serialized).unwrap(),
    ));
    assert_eq!(build_turns_from_rollout_items(&items), expected);

    // Old records have no owner, so retain their current-turn fallback.
    let mut old_record: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    old_record.as_object_mut().unwrap().remove("turn_id");
    *items.last_mut().unwrap() = RolloutItem::EventMsg(serde_json::from_value(old_record).unwrap());
    let mut expected_old = expected;
    let mcp_item = expected_old[0].items.pop().unwrap();
    expected_old[1].items.push(mcp_item);
    assert_eq!(build_turns_from_rollout_items(&items), expected_old);
}

#[test]
fn assigns_late_exec_completion_to_original_turn() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "first".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-b".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "second".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "exec-late".into(),
            plugin_id: None,
            script_path: None,
            process_id: Some("pid-42".into()),
            turn_id: "turn-a".into(),
            completed_at_ms: 0,
            command: vec!["echo".into(), "done".into()],
            cwd: test_path_buf("/tmp").abs().into(),
            parsed_cmd: vec![ParsedCommand::Unknown {
                cmd: "echo done".into(),
            }],
            source: ExecCommandSource::Agent,
            interaction_input: None,
            aggregated_output: "done\n".into(),
            exit_code: 0,
            duration: Duration::from_millis(5),
            status: CoreExecCommandStatus::Completed,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-b".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id, "turn-a");
    assert_eq!(turns[1].id, "turn-b");
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(turns[1].items.len(), 1);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::CommandExecution {
            model_context: None,
            sandbox_type: None,
            id: "exec-late".into(),
            plugin_id: None,
            script_path: None,
            command: "echo done".into(),
            cwd: test_path_buf("/tmp").abs().into(),
            process_id: Some("pid-42".into()),
            source: CommandExecutionSource::Agent,
            status: CommandExecutionStatus::Completed,
            command_actions: vec![CommandAction::Unknown {
                command: "echo done".into(),
            }],
            aggregated_output: Some("done\n".into()),
            exit_code: Some(0),
            duration_ms: Some(5),
        }
    );
}

#[test]
fn drops_late_turn_scoped_item_for_unknown_turn_id() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "first".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-b".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "second".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::ExecCommandEnd(ExecCommandEndEvent {
            call_id: "exec-unknown-turn".into(),
            plugin_id: None,
            script_path: None,
            process_id: Some("pid-42".into()),
            turn_id: "turn-missing".into(),
            completed_at_ms: 0,
            command: vec!["echo".into(), "done".into()],
            cwd: test_path_buf("/tmp").abs().into(),
            parsed_cmd: vec![ParsedCommand::Unknown {
                cmd: "echo done".into(),
            }],
            source: ExecCommandSource::Agent,
            interaction_input: None,
            aggregated_output: "done\n".into(),
            exit_code: 0,
            duration: Duration::from_millis(5),
            status: CoreExecCommandStatus::Completed,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-b".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let mut builder = ThreadHistoryBuilder::new();
    for event in &events {
        builder.handle_event(event);
    }
    let turns = builder.finish();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id, "turn-a");
    assert_eq!(turns[1].id, "turn-b");
    assert_eq!(turns[0].items.len(), 1);
    assert_eq!(turns[1].items.len(), 1);
    assert_eq!(
        turns[1].items[0].payload,
        ThreadItemPayload::UserMessage {
            id: "item-2".into(),
            client_id: None,
            content: vec![UserInput::Text {
                text: "second".into(),
                text_elements: Vec::new(),
            }],
        }
    );
}

#[test]
fn live_paginated_turn_releases_superseded_agent_text() {
    let mut builder = ThreadHistoryBuilder::new();
    let turn_id = builder.ensure_turn().id.clone();
    for (id, text) in [
        ("a", "old"),
        ("b", "draft"),
        ("b", "corrected"),
        ("c", "latest"),
    ] {
        builder.handle_live_paginated_event(&EventMsg::ItemCompleted(ItemCompletedEvent {
            content_token_count: None,
            thread_id: ThreadId::default(),
            turn_id: turn_id.clone(),
            item: CoreTurnItem::AgentMessage(codex_protocol::items::AgentMessageItem {
                moderation: None,
                id: id.into(),
                content: vec![codex_protocol::items::AgentMessageContent::Text {
                    text: text.into(),
                }],
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            }),
            started_at_ms: None,
            completed_at_ms: 1,
        }));
    }
    let mut texts = builder.current_turn.unwrap().items.into_iter().map(|item| {
        let ThreadItemPayload::AgentMessage { text, .. } = item else {
            panic!("expected assistant item")
        };
        text
    });
    assert_eq!(texts.next().unwrap().capacity(), 0);
    assert_eq!(texts.collect::<Vec<_>>(), ["corrected", "latest"]);
}

#[test]
fn active_turn_summary_preserves_selection_and_completed_metadata() {
    let mut builder = ThreadHistoryBuilder::new();
    assert_eq!(
        builder.active_turn_snapshot_with_items_view(TurnItemsView::Summary),
        None
    );
    builder.handle_event(&EventMsg::TurnStarted(TurnStartedEvent {
        turn_attribution: None,
        turn_id: "turn-1".into(),
        root_turn_id: None,
        trace_id: None,
        started_at: Some(100),
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }));
    builder.handle_event(&EventMsg::UserMessage(UserMessageEvent {
        message: "first".into(),
        ..Default::default()
    }));
    let first_user = builder.active_turn_snapshot().unwrap().items[0].clone();
    builder.handle_event(&EventMsg::UserMessage(UserMessageEvent {
        message: "steering".into(),
        ..Default::default()
    }));
    for message in ["draft", "answer"] {
        builder.handle_event(&EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: message.into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }));
    }
    let final_agent = builder
        .active_turn_snapshot()
        .unwrap()
        .items
        .last()
        .unwrap()
        .clone();
    builder.handle_event(&EventMsg::AgentReasoning(AgentReasoningEvent {
        text: "not included in the summary".repeat(1024),
    }));
    let mut expected = builder.active_turn_snapshot().unwrap();
    expected.items = vec![first_user, final_agent];
    expected.items_view = TurnItemsView::Summary;
    assert_eq!(
        builder.active_turn_snapshot_with_items_view(TurnItemsView::Summary),
        Some(expected.clone())
    );
    builder.handle_event(&EventMsg::TurnComplete(TurnCompleteEvent {
        root_turn_id: None,
        turn_id: "turn-1".into(),
        started_at: Some(100),
        last_agent_message: None,
        error: None,
        completed_at: Some(102),
        duration_ms: Some(2_000),
        time_to_first_token_ms: None,
    }));
    expected.status = TurnStatus::Completed;
    expected.completed_at = Some(102);
    expected.duration_ms = Some(2_000);
    assert_eq!(
        builder.active_turn_snapshot_with_items_view(TurnItemsView::Summary),
        Some(expected)
    );
    assert_eq!(
        builder.active_turn_snapshot_with_items_view(TurnItemsView::NotLoaded),
        builder.active_turn_metadata_snapshot().map(Turn::from)
    );
}

#[test]
fn active_turn_metadata_snapshot_tracks_turn_lifecycle() {
    let mut builder = ThreadHistoryBuilder::new();
    assert_eq!(builder.active_turn_metadata_snapshot(), None);
    builder.handle_event(&EventMsg::TurnStarted(TurnStartedEvent {
        turn_attribution: None,
        turn_id: "turn-1".into(),
        root_turn_id: Some("root-turn".into()),
        trace_id: None,
        started_at: Some(100),
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }));
    builder.handle_event(&EventMsg::UserMessage(UserMessageEvent {
        message: "hello".into(),
        ..Default::default()
    }));
    let mut expected = ThreadHistoryTurnMetadata {
        turn_id: "turn-1".into(),
        root_turn_id: Some("root-turn".into()),
        error: None,
        status: TurnStatus::InProgress,
        started_at: Some(100),
        completed_at: None,
        duration_ms: None,
    };
    assert_eq!(
        builder.active_turn_metadata_snapshot(),
        Some(expected.clone())
    );
    assert_eq!(builder.active_turn_snapshot().unwrap().items.len(), 1);
    assert_eq!(
        builder.active_turn_snapshot().unwrap().root_turn_id,
        expected.root_turn_id
    );

    builder.handle_event(&EventMsg::TurnComplete(TurnCompleteEvent {
        root_turn_id: None,
        turn_id: "turn-1".into(),
        started_at: Some(100),
        last_agent_message: None,
        error: None,
        completed_at: Some(102),
        duration_ms: Some(2_000),
        time_to_first_token_ms: None,
    }));
    expected.status = TurnStatus::Completed;
    expected.completed_at = Some(102);
    expected.duration_ms = Some(2_000);
    assert_eq!(
        builder.active_turn_metadata_snapshot(),
        Some(expected.clone())
    );
    assert_eq!(
        builder.active_turn_snapshot_with_items_view(TurnItemsView::NotLoaded),
        Some(Turn::from(expected.clone()))
    );
    assert_eq!(builder.active_turn_snapshot().unwrap().items.len(), 1);
    assert_eq!(builder.finish()[0].root_turn_id, expected.root_turn_id);
}

#[test]
fn patch_apply_begin_updates_active_turn_snapshot_with_file_change() {
    let turn_id = "turn-1";
    let mut builder = ThreadHistoryBuilder::new();
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "apply patch".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
            call_id: "patch-call".into(),
            turn_id: turn_id.to_string(),
            auto_approved: false,
            changes: [(
                PathBuf::from("README.md"),
                codex_protocol::protocol::FileChange::Add {
                    content: "hello\n".into(),
                },
            )]
            .into_iter()
            .collect(),
        }),
    ];

    for event in &events {
        builder.handle_event(event);
    }

    let snapshot = builder
        .active_turn_snapshot()
        .expect("active turn snapshot");
    assert_eq!(snapshot.id, turn_id);
    assert_eq!(snapshot.status, TurnStatus::InProgress);
    assert_eq!(
        snapshot.item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::UserMessage {
                id: "item-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "apply patch".into(),
                    text_elements: Vec::new(),
                }],
            },
            &ThreadItemPayload::FileChange {
                id: "patch-call".into(),
                changes: vec![FileUpdateChange {
                    path: "README.md".into(),
                    kind: PatchChangeKind::Add,
                    diff: "hello\n".into(),
                }],
                status: PatchApplyStatus::InProgress,
            },
        ]
    );
}

#[test]
fn apply_patch_approval_request_updates_active_turn_snapshot_with_file_change() {
    let turn_id = "turn-1";
    let mut builder = ThreadHistoryBuilder::new();
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "apply patch".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
            call_id: "patch-call".into(),
            turn_id: turn_id.to_string(),
            started_at_ms: 1_250,
            changes: [(
                PathBuf::from("README.md"),
                codex_protocol::protocol::FileChange::Add {
                    content: "hello\n".into(),
                },
            )]
            .into_iter()
            .collect(),
            reason: None,
            grant_root: None,
        }),
    ];

    for event in &events {
        builder.handle_event(event);
    }

    let snapshot = builder
        .active_turn_snapshot()
        .expect("active turn snapshot");
    assert_eq!(snapshot.id, turn_id);
    assert_eq!(snapshot.status, TurnStatus::InProgress);
    assert_eq!(snapshot.items[1].started_at_ms, Some(1_250));
    assert_eq!(
        snapshot.item_payloads().collect::<Vec<_>>(),
        [
            &ThreadItemPayload::UserMessage {
                id: "item-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "apply patch".into(),
                    text_elements: Vec::new(),
                }],
            },
            &ThreadItemPayload::FileChange {
                id: "patch-call".into(),
                changes: vec![FileUpdateChange {
                    path: "README.md".into(),
                    kind: PatchChangeKind::Add,
                    diff: "hello\n".into(),
                }],
                status: PatchApplyStatus::InProgress,
            },
        ]
    );
}

#[test]
fn late_turn_complete_does_not_close_active_turn() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "first".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-b".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "second".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "still in b".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-b".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id, "turn-a");
    assert_eq!(turns[1].id, "turn-b");
    assert_eq!(turns[1].items.len(), 2);
}

#[test]
fn late_turn_complete_with_embedded_error_preserves_active_turn() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "first".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-b".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: Some(30),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "second".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: Some(10),
            last_agent_message: None,
            error: Some(ErrorEvent {
                misalignment: None,
                message: "Selected model is at capacity. Please try a different model.".into(),
                codex_error_info: Some(CodexErrorInfo::PolicyCapacityUnavailable),
                rejected_audio_file_ids: Vec::new(),
            }),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();

    assert_eq!(
        build_turns_from_rollout_items(&items),
        vec![
            Turn {
                id: "turn-a".into(),
                root_turn_id: None,
                items_view: TurnItemsView::Full,
                items: vec![ThreadItem::untimed(ThreadItemPayload::UserMessage {
                    id: "item-1".into(),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: "first".into(),
                        text_elements: Vec::new(),
                    }],
                })],
                status: TurnStatus::Failed,
                error: Some(TurnError {
                    misalignment: None,
                    message: "Selected model is at capacity. Please try a different model.".into(),
                    codex_error_info: Some(
                        crate::protocol::v2::CodexErrorInfo::PolicyCapacityUnavailable,
                    ),
                    additional_details: None,
                }),
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
            },
            Turn {
                id: "turn-b".into(),
                root_turn_id: None,
                items_view: TurnItemsView::Full,
                items: vec![ThreadItem::untimed(ThreadItemPayload::UserMessage {
                    id: "item-2".into(),
                    client_id: None,
                    content: vec![UserInput::Text {
                        text: "second".into(),
                        text_elements: Vec::new(),
                    }],
                })],
                status: TurnStatus::InProgress,
                error: None,
                started_at: Some(30),
                completed_at: None,
                duration_ms: None,
            },
        ]
    );
}

#[test]
fn late_turn_aborted_does_not_interrupt_active_turn() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "first".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-b".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "second".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnAborted(TurnAbortedEvent {
            root_turn_id: None,
            turn_id: Some("turn-a".into()),
            started_at: None,
            reason: TurnAbortReason::Replaced,
            error: None,
            completed_at: None,
            duration_ms: None,
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "still in b".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id, "turn-a");
    assert_eq!(turns[1].id, "turn-b");
    assert_eq!(turns[1].status, TurnStatus::InProgress);
    assert_eq!(turns[1].items.len(), 2);
}

#[test]
fn preserves_compaction_only_turn() {
    let items = vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-compact".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::Compacted(Box::new(CompactedItem {
            message: String::new(),
            replacement_history: None,
            retained_context: None,
            guardian_history: None,
            mcp_resource_origins: None,
            window_number: None,
            first_window_id: None,
            previous_window_id: None,
            window_id: None,
            compaction_response_id: None,
            latest_token_usage_record: None,
            resume_metadata: None,
        })),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-compact".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ];

    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(
        turns,
        vec![Turn {
            id: "turn-compact".into(),
            root_turn_id: None,
            status: TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            items_view: TurnItemsView::Full,
            items: Vec::new(),
        }]
    );
}

#[test]
fn reconstructs_collab_resume_end_item() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "resume agent".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::CollabResumeEnd(codex_protocol::protocol::CollabResumeEndEvent {
            call_id: "resume-1".into(),
            completed_at_ms: 2_000,
            sender_thread_id: ThreadId::try_from("00000000-0000-0000-0000-000000000001")
                .expect("valid sender thread id"),
            receiver_thread_id: ThreadId::try_from("00000000-0000-0000-0000-000000000002")
                .expect("valid receiver thread id"),
            receiver_agent_nickname: None,
            receiver_agent_role: None,
            status: AgentStatus::Completed(None),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    let item = &turns[0].items[1];
    assert_eq!(
        (item.started_at_ms, item.completed_at_ms),
        (None, Some(2_000))
    );
    assert_eq!(
        item.payload,
        ThreadItemPayload::CollabAgentToolCall {
            id: "resume-1".into(),
            tool: CollabAgentTool::ResumeAgent,
            status: CollabAgentToolCallStatus::Completed,
            sender_thread_id: "00000000-0000-0000-0000-000000000001".into(),
            receiver_thread_ids: vec!["00000000-0000-0000-0000-000000000002".into()],
            prompt: None,
            model: None,
            reasoning_effort: None,
            agents_states: [(
                "00000000-0000-0000-0000-000000000002".into(),
                CollabAgentState {
                    status: crate::protocol::v2::CollabAgentStatus::Completed,
                    message: None,
                },
            )]
            .into_iter()
            .collect(),
        }
    );
}

#[test]
fn reconstructs_collab_spawn_end_item_with_model_metadata() {
    let sender_thread_id =
        ThreadId::try_from("00000000-0000-0000-0000-000000000001").expect("valid sender thread id");
    let spawned_thread_id = ThreadId::try_from("00000000-0000-0000-0000-000000000002")
        .expect("valid receiver thread id");
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "spawn agent".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::CollabAgentSpawnEnd(codex_protocol::protocol::CollabAgentSpawnEndEvent {
            call_id: "spawn-1".into(),
            completed_at_ms: 0,
            sender_thread_id,
            new_thread_id: Some(spawned_thread_id),
            new_agent_nickname: Some("Scout".into()),
            new_agent_role: Some("explorer".into()),
            prompt: "inspect the repo".into(),
            model: "gpt-5.4-mini".into(),
            reasoning_effort: codex_protocol::openai_models::ReasoningEffort::Medium,
            status: AgentStatus::Running,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::CollabAgentToolCall {
            id: "spawn-1".into(),
            tool: CollabAgentTool::SpawnAgent,
            status: CollabAgentToolCallStatus::Completed,
            sender_thread_id: "00000000-0000-0000-0000-000000000001".into(),
            receiver_thread_ids: vec!["00000000-0000-0000-0000-000000000002".into()],
            prompt: Some("inspect the repo".into()),
            model: Some("gpt-5.4-mini".into()),
            reasoning_effort: Some(codex_protocol::openai_models::ReasoningEffort::Medium),
            agents_states: [(
                "00000000-0000-0000-0000-000000000002".into(),
                CollabAgentState {
                    status: crate::protocol::v2::CollabAgentStatus::Running,
                    message: None,
                },
            )]
            .into_iter()
            .collect(),
        }
    );
}

#[test]
fn reconstructs_interrupted_send_input_as_completed_collab_call() {
    // `send_input(interrupt=true)` first stops the child's active turn, then redirects it with
    // new input. The transient interrupted status should remain visible in agent state, but the
    // collab tool call itself is still a successful redirect rather than a failed operation.
    let sender =
        ThreadId::try_from("00000000-0000-0000-0000-000000000001").expect("valid sender thread id");
    let receiver = ThreadId::try_from("00000000-0000-0000-0000-000000000002")
        .expect("valid receiver thread id");
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "redirect".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::CollabAgentInteractionBegin(
            codex_protocol::protocol::CollabAgentInteractionBeginEvent {
                call_id: "send-1".into(),
                started_at_ms: 0,
                sender_thread_id: sender,
                receiver_thread_id: receiver,
                prompt: "new task".into(),
            },
        ),
        EventMsg::CollabAgentInteractionEnd(
            codex_protocol::protocol::CollabAgentInteractionEndEvent {
                call_id: "send-1".into(),
                completed_at_ms: 0,
                sender_thread_id: sender,
                receiver_thread_id: receiver,
                receiver_agent_nickname: None,
                receiver_agent_role: None,
                prompt: "new task".into(),
                status: AgentStatus::Interrupted,
            },
        ),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::CollabAgentToolCall {
            id: "send-1".into(),
            tool: CollabAgentTool::SendInput,
            status: CollabAgentToolCallStatus::Completed,
            sender_thread_id: sender.to_string(),
            receiver_thread_ids: vec![receiver.to_string()],
            prompt: Some("new task".into()),
            model: None,
            reasoning_effort: None,
            agents_states: [(
                receiver.to_string(),
                CollabAgentState {
                    status: crate::protocol::v2::CollabAgentStatus::Interrupted,
                    message: None,
                },
            )]
            .into_iter()
            .collect(),
        }
    );
}

#[test]
fn rollback_failed_error_does_not_mark_turn_failed() {
    let events = vec![
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::AgentMessage(AgentMessageEvent {
            moderation: None,
            message: "done".into(),
            phase: None,
            memory_citation: None,
            delivery: None,
            questions: None,
        }),
        EventMsg::Error(ErrorEvent {
            misalignment: None,
            message: "rollback failed".into(),
            codex_error_info: Some(CodexErrorInfo::ThreadRollbackFailed),
            rejected_audio_file_ids: Vec::new(),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Completed);
    assert_eq!(turns[0].error, None);
}

#[test]
fn out_of_turn_error_does_not_create_or_fail_a_turn() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
        EventMsg::Error(ErrorEvent {
            misalignment: None,
            message: "request-level failure".into(),
            codex_error_info: Some(CodexErrorInfo::BadRequest),
            rejected_audio_file_ids: Vec::new(),
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0],
        Turn {
            id: "turn-a".into(),
            root_turn_id: None,
            status: TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            items_view: TurnItemsView::Full,
            items: vec![ThreadItem::untimed(ThreadItemPayload::UserMessage {
                id: "item-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "hello".into(),
                    text_elements: Vec::new(),
                }],
            })],
        }
    );
}

#[test]
fn error_then_turn_complete_preserves_failed_status() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::Error(ErrorEvent {
            misalignment: None,
            message: "stream failure".into(),
            codex_error_info: Some(CodexErrorInfo::ResponseStreamDisconnected {
                http_status_code: Some(502),
            }),
            rejected_audio_file_ids: Vec::new(),
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();
    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, "turn-a");
    assert_eq!(turns[0].status, TurnStatus::Failed);
    assert_eq!(
        turns[0].error,
        Some(TurnError {
            misalignment: None,
            message: "stream failure".into(),
            codex_error_info: Some(
                crate::protocol::v2::CodexErrorInfo::ResponseStreamDisconnected {
                    http_status_code: Some(502),
                }
            ),
            additional_details: None,
        })
    );
}

#[test]
fn turn_complete_with_embedded_error_marks_turn_failed() {
    let events = vec![
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
        EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "retry me".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: Some(10),
            last_agent_message: None,
            error: Some(ErrorEvent {
                misalignment: None,
                message: "Selected model is at capacity. Please try a different model.".into(),
                codex_error_info: Some(CodexErrorInfo::ServerOverloaded),
                rejected_audio_file_ids: Vec::new(),
            }),
            completed_at: Some(20),
            duration_ms: Some(10_000),
            time_to_first_token_ms: None,
        }),
    ];

    let items = events
        .into_iter()
        .map(RolloutItem::EventMsg)
        .collect::<Vec<_>>();

    assert_eq!(
        build_turns_from_rollout_items(&items),
        vec![Turn {
            id: "turn-a".into(),
            root_turn_id: None,
            items_view: TurnItemsView::Full,
            items: vec![ThreadItem::untimed(ThreadItemPayload::UserMessage {
                id: "item-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "retry me".into(),
                    text_elements: Vec::new(),
                }],
            })],
            status: TurnStatus::Failed,
            error: Some(TurnError {
                misalignment: None,
                message: "Selected model is at capacity. Please try a different model.".into(),
                codex_error_info: Some(crate::protocol::v2::CodexErrorInfo::ServerOverloaded),
                additional_details: None,
            }),
            started_at: Some(10),
            completed_at: Some(20),
            duration_ms: Some(10_000),
        }]
    );
}

#[test]
fn rebuilds_hook_prompt_items_from_rollout_response_items() {
    let hook_prompt = build_hook_prompt_message(
        &[
            CoreHookPromptFragment::from_single_hook("Retry with tests.", "hook-run-1"),
            CoreHookPromptFragment::from_single_hook("Then summarize cleanly.", "hook-run-2"),
        ],
        &codex_protocol_test_support::uuid_v7_item_id_generator_for_test(),
    )
    .expect("hook prompt message");
    let items = vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        })),
        RolloutItem::ResponseItem(hook_prompt.into()),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ];

    let turns = build_turns_from_rollout_items(&items);

    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].items.len(), 2);
    assert_eq!(
        turns[0].items[1].payload,
        ThreadItemPayload::HookPrompt {
            id: turns[0].items[1].id().to_string(),
            fragments: vec![
                crate::protocol::v2::HookPromptFragment {
                    text: "Retry with tests.".into(),
                    hook_run_id: "hook-run-1".into(),
                },
                crate::protocol::v2::HookPromptFragment {
                    text: "Then summarize cleanly.".into(),
                    hook_run_id: "hook-run-2".into(),
                },
            ],
        }
    );
}

#[test]
fn canonical_hook_prompt_completion_updates_turn_history() {
    let hook_prompt = CoreTurnItem::HookPrompt(codex_protocol::items::HookPromptItem {
        id: "hook-prompt-1".into(),
        fragments: vec![CoreHookPromptFragment::from_single_hook(
            "Retry with tests.",
            "hook-run-1",
        )],
    });
    let expected_item = ThreadItem {
        payload: ThreadItemPayload::from(hook_prompt.clone()),
        started_at_ms: Some(10),
        completed_at_ms: Some(20),
    };
    let mut builder = ThreadHistoryBuilder::new();
    builder.handle_event(&EventMsg::TurnStarted(TurnStartedEvent {
        turn_attribution: None,
        turn_id: "turn-a".into(),
        root_turn_id: None,
        trace_id: None,
        started_at: None,
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }));
    let changes = builder.handle_event_with_changes(&EventMsg::ItemCompleted(ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-a".into(),
        item: hook_prompt,
        started_at_ms: Some(10),
        completed_at_ms: 20,
    }));

    let snapshot = builder.active_turn_snapshot().expect("active turn");
    assert_eq!(snapshot.items, [expected_item]);
    assert_eq!(changes.changed_items[0].item, snapshot.items[0]);
}

#[test]
fn completed_sub_agent_activity_updates_completed_parent_turn() {
    use codex_protocol::protocol::HasLegacyEvent;

    let child_thread_id = ThreadId::new();
    let child_path = codex_protocol::AgentPath::root()
        .join("worker")
        .expect("worker path");
    let spawn = ItemCompletedEvent {
        content_token_count: None,
        thread_id: ThreadId::new(),
        turn_id: "turn-a".into(),
        item: CoreTurnItem::SubAgentActivity(CoreSubAgentActivityItem {
            model: Some("gpt-5".into()),
            reasoning_effort: Some(codex_protocol::openai_models::ReasoningEffort::High),
            id: "spawn-child".into(),
            kind: CoreSubAgentActivityKind::Started,
            agent_thread_id: child_thread_id,
            agent_path: child_path.clone(),
        }),
        started_at_ms: Some(1),
        completed_at_ms: 2,
    };
    let expected_spawn = ThreadItemPayload::from(spawn.item.clone());
    let legacy_spawn = spawn
        .as_legacy_events(/*show_raw_agent_reasoning*/ false)
        .pop()
        .expect("legacy spawn activity");
    let crate::ServerNotification::ItemCompleted(notification) =
        crate::item_event_to_server_notification(legacy_spawn.clone(), "parent", "turn-a")
    else {
        panic!("expected item completed");
    };
    assert_eq!(
        (notification.item.payload, notification.completed_at_ms),
        (expected_spawn.clone(), 2)
    );
    for spawn in [EventMsg::ItemCompleted(spawn), legacy_spawn] {
        let items = vec![
            RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
                turn_attribution: None,
                turn_id: "turn-a".into(),
                root_turn_id: None,
                trace_id: None,
                started_at: None,
                model_context_window: None,
                collaboration_mode_kind: Default::default(),
            })),
            RolloutItem::EventMsg(spawn),
            RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
                root_turn_id: None,
                turn_id: "turn-a".into(),
                started_at: None,
                last_agent_message: None,
                error: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            })),
            RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
                content_token_count: None,
                thread_id: ThreadId::new(),
                turn_id: "turn-a".into(),
                item: CoreTurnItem::SubAgentActivity(CoreSubAgentActivityItem {
                    model: None,
                    reasoning_effort: None,
                    id: "child-turn-completed".into(),
                    kind: CoreSubAgentActivityKind::Completed,
                    agent_thread_id: child_thread_id,
                    agent_path: child_path.clone(),
                }),
                started_at_ms: None,
                completed_at_ms: 0,
            })),
        ];

        let turns = build_turns_from_rollout_items(&items);

        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].item_payloads().collect::<Vec<_>>(),
            [
                &expected_spawn,
                &ThreadItemPayload::SubAgentActivity {
                    model: None,
                    reasoning_effort: None,
                    id: "child-turn-completed".into(),
                    kind: crate::protocol::v2::SubAgentActivityKind::Completed,
                    agent_thread_id: child_thread_id.to_string(),
                    agent_path: "/root/worker".into(),
                },
            ]
        );
    }
}

#[test]
fn ignores_plain_user_response_items_in_rollout_replay() {
    let items = vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::ResponseItem(
            codex_protocol::models::ResponseItem::Message {
                status: None,
                encrypted_content: None,
                id: Some(codex_protocol::ResponseItemId::with_suffix("msg", "1")),
                role: "user".into(),
                content: vec![codex_protocol::models::ContentItem::InputText {
                    text: "plain text".into(),
                }],
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            }
            .into(),
        ),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
        })),
    ];

    let turns = build_turns_from_rollout_items(&items);
    assert_eq!(turns.len(), 1);
    assert!(turns[0].items.is_empty());
}

#[test]
fn changed_rollout_item_reports_new_item_snapshot() {
    let mut builder = ThreadHistoryBuilder::new();

    let changes = builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(
        EventMsg::UserMessage(UserMessageEvent {
            client_id: Some("client-message-1".into()),
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        }),
    ));
    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_items: vec![ThreadHistoryItemChange {
                turn_id: "rollout-0".into(),
                item: ThreadItem::untimed(ThreadItemPayload::UserMessage {
                    id: "item-1".into(),
                    client_id: Some("client-message-1".into()),
                    content: vec![UserInput::Text {
                        text: "hello".into(),
                        text_elements: Vec::new(),
                    }],
                })
            }],
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "rollout-0".into(),
                root_turn_id: None,
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
            removed_turn_ids: Vec::new(),
        }
    );
}

#[test]
fn changed_rollout_item_reports_updated_existing_item_snapshot() {
    let mut builder = ThreadHistoryBuilder::new();
    builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(EventMsg::WebSearchBegin(
        WebSearchBeginEvent {
            call_id: "search-1".into(),
        },
    )));

    let changes = builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(
        EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id: "search-1".into(),
            query: "codex".into(),
            action: CoreWebSearchAction::Search {
                query: Some("codex".into()),
                queries: None,
            },
            results: None,
        }),
    ));
    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_items: vec![ThreadHistoryItemChange {
                turn_id: "rollout-0".into(),
                item: ThreadItem::untimed(ThreadItemPayload::WebSearch(WebSearchItem {
                    id: "search-1".into(),
                    query: "codex".into(),
                    action: Some(WebSearchAction::Search {
                        query: Some("codex".into()),
                        queries: None,
                    }),
                    results: None,
                }))
            }],
            changed_turns: Vec::new(),
            removed_turn_ids: Vec::new(),
        }
    );
}

#[test]
fn changed_rollout_item_reports_streaming_item_mutation() {
    let mut builder = ThreadHistoryBuilder::new();
    builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(EventMsg::AgentReasoning(
        AgentReasoningEvent {
            text: "summary".into(),
        },
    )));

    let changes = builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(
        EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent {
            text: "raw content".into(),
        }),
    ));
    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_items: vec![ThreadHistoryItemChange {
                turn_id: "rollout-0".into(),
                item: ThreadItem::untimed(ThreadItemPayload::Reasoning {
                    id: "item-1".into(),
                    summary: vec!["summary".into()],
                    content: vec!["raw content".into()],
                })
            }],
            changed_turns: Vec::new(),
            removed_turn_ids: Vec::new(),
        }
    );
}

#[test]
fn changed_rollout_item_reports_turn_completion_metadata() {
    let mut builder = ThreadHistoryBuilder::new();

    let start_changes = builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(
        EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: Some("root-turn".into()),
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }),
    ));
    assert_eq!(
        start_changes,
        ThreadHistoryChangeSet {
            changed_items: Vec::new(),
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "turn-a".into(),
                root_turn_id: Some("root-turn".into()),
                status: TurnStatus::InProgress,
                error: None,
                started_at: Some(10),
                completed_at: None,
                duration_ms: None,
            }],
            removed_turn_ids: Vec::new(),
        }
    );

    builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(EventMsg::UserMessage(
        UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        },
    )));
    let complete_changes = builder.handle_rollout_item_with_changes(&RolloutItem::EventMsg(
        EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: Some(20),
            duration_ms: Some(123),
            time_to_first_token_ms: None,
        }),
    ));

    assert_eq!(
        complete_changes,
        ThreadHistoryChangeSet {
            changed_items: Vec::new(),
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "turn-a".into(),
                root_turn_id: Some("root-turn".into()),
                status: TurnStatus::Completed,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(123),
            }],
            removed_turn_ids: Vec::new(),
        }
    );
}

#[test]
fn changed_rollout_items_dedupe_updated_item_snapshots() {
    let mut builder = ThreadHistoryBuilder::new();
    let changes = builder.handle_rollout_items_with_changes(&[
        RolloutItem::EventMsg(EventMsg::WebSearchBegin(WebSearchBeginEvent {
            call_id: "search-1".into(),
        })),
        RolloutItem::EventMsg(EventMsg::WebSearchEnd(WebSearchEndEvent {
            call_id: "search-1".into(),
            query: "codex".into(),
            action: CoreWebSearchAction::Search {
                query: Some("codex".into()),
                queries: None,
            },
            results: None,
        })),
    ]);
    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_items: vec![ThreadHistoryItemChange {
                turn_id: "rollout-0".into(),
                item: ThreadItem::untimed(ThreadItemPayload::WebSearch(WebSearchItem {
                    id: "search-1".into(),
                    query: "codex".into(),
                    action: Some(WebSearchAction::Search {
                        query: Some("codex".into()),
                        queries: None,
                    }),
                    results: None,
                }))
            }],
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "rollout-0".into(),
                root_turn_id: None,
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
            removed_turn_ids: Vec::new(),
        }
    );
}

#[test]
fn changed_rollout_items_dedupe_turn_metadata_snapshots() {
    let mut builder = ThreadHistoryBuilder::new();
    let changes = builder.handle_rollout_items_with_changes(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: Some("root-turn".into()),
            trace_id: None,
            started_at: Some(10),
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
            root_turn_id: None,
            turn_id: "turn-a".into(),
            started_at: None,
            last_agent_message: None,
            error: None,
            completed_at: Some(20),
            duration_ms: Some(123),
            time_to_first_token_ms: None,
        })),
    ]);

    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_items: Vec::new(),
            changed_turns: vec![ThreadHistoryTurnMetadata {
                turn_id: "turn-a".into(),
                root_turn_id: Some("root-turn".into()),
                status: TurnStatus::Completed,
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(123),
            }],
            removed_turn_ids: Vec::new(),
        }
    );
}

#[test]
fn changed_rollout_items_drop_prior_changes_for_removed_turns() {
    let mut builder = ThreadHistoryBuilder::new();
    let changes = builder.handle_rollout_items_with_changes(&[
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: "turn-a".into(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        })),
        RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            client_id: None,
            message: "hello".into(),
            images: None,
            text_elements: Vec::new(),
            local_images: Vec::new(),
            ..Default::default()
        })),
        RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
            num_turns: 1,
        })),
    ]);

    assert_eq!(
        changes,
        ThreadHistoryChangeSet {
            changed_items: Vec::new(),
            changed_turns: Vec::new(),
            removed_turn_ids: vec!["turn-a".into()],
        }
    );
}
