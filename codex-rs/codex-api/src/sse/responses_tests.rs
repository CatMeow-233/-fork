use super::*;
use assert_matches::assert_matches;
use bytes::Bytes;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_http_client::RetryAfter;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::MisalignmentErrorDetails;
use futures::TryStreamExt;
use futures::stream;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_test::io::Builder as IoBuilder;
use tokio_util::io::ReaderStream;

async fn collect_events(chunks: &[&[u8]]) -> Vec<Result<ResponseEvent, ApiError>> {
    let mut builder = IoBuilder::new();
    for chunk in chunks {
        builder.read(chunk);
    }

    let reader = builder.build();
    let stream = ReaderStream::new(reader).map_err(|err| TransportError::Network(err.to_string()));
    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        idle_timeout(),
        /*telemetry*/ None,
    ));

    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    events
}

async fn run_sse(events: Vec<serde_json::Value>) -> Vec<ResponseEvent> {
    let mut body = String::new();
    for e in events {
        let kind = e
            .get("type")
            .and_then(|v| v.as_str())
            .expect("fixture event missing type");
        if e.as_object().map(|o| o.len() == 1).unwrap_or(false) {
            body.push_str(&format!("event: {kind}\n\n"));
        } else {
            body.push_str(&format!("event: {kind}\ndata: {e}\n\n"));
        }
    }

    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    let stream = ReaderStream::new(std::io::Cursor::new(body))
        .map_err(|err| TransportError::Network(err.to_string()));
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        idle_timeout(),
        /*telemetry*/ None,
    ));

    let mut out = Vec::new();
    while let Some(ev) = rx.recv().await {
        out.push(ev.expect("channel closed"));
    }
    out
}

fn idle_timeout() -> Duration {
    Duration::from_millis(1000)
}

#[tokio::test]
async fn partial_answer_phase_survives_item_start_and_completion() {
    let item = json!({
        "id": "msg_partial",
        "type": "message",
        "role": "assistant",
        "content": [],
        "phase": "partial_answer"
    });
    let added = json!({"type": "response.output_item.added", "item": item});
    let delta = json!({"type": "response.output_text.delta", "delta": "First result."});
    let mut done_item = item;
    done_item["content"] = json!([{"type": "output_text", "text": "First result."}]);
    let done = json!({"type": "response.output_item.done", "item": done_item});
    let completed = json!({
        "type": "response.completed",
        "response": {"id": "resp1", "end_turn": false}
    });
    let stream = [added, delta, done, completed]
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();

    let events = collect_events(&[stream.as_bytes()]).await;
    assert_matches!(events.as_slice(), [
        Ok(ResponseEvent::OutputItemAdded(ResponseItem::Message {
            phase: Some(MessagePhase::PartialAnswer), content: start_content, ..
        })),
        Ok(ResponseEvent::OutputTextDelta(text)),
        Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
            phase: Some(MessagePhase::PartialAnswer), content: done_content, ..
        })),
        Ok(ResponseEvent::Completed { end_turn: Some(false), .. }),
    ] if start_content.is_empty()
        && text == "First result."
        && done_content == &vec![codex_protocol::models::ContentItem::OutputText {
            annotations: None,
            logprobs: None,
            text: "First result.".to_string()
        }]);
}

#[tokio::test]
async fn parses_items_and_completed() {
    let item1 = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Hello"}],
            "phase": "commentary"
        }
    })
    .to_string();

    let item2 = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "World"}]
        }
    })
    .to_string();

    let completed = json!({
        "type": "response.completed",
        "response": { "id": "resp1" }
    })
    .to_string();

    let sse1 = format!("event: response.output_item.done\ndata: {item1}\n\n");
    let sse2 = format!("event: response.output_item.done\ndata: {item2}\n\n");
    let sse3 = format!("event: response.completed\ndata: {completed}\n\n");

    let events = collect_events(&[sse1.as_bytes(), sse2.as_bytes(), sse3.as_bytes()]).await;

    assert_eq!(events.len(), 3);

    assert_matches!(
        &events[0],
        Ok(ResponseEvent::OutputItemDone(ResponseItem::Message {
            role,
            phase: Some(MessagePhase::Commentary),
            ..
        })) if role == "assistant"
    );

    assert_matches!(
        &events[1],
        Ok(ResponseEvent::OutputItemDone(ResponseItem::Message { role, .. }))
            if role == "assistant"
    );

    match &events[2] {
        Ok(ResponseEvent::Completed {
            response_id,
            token_usage,
            usage_metadata,
            end_turn,
        }) => {
            assert_eq!(response_id, "resp1");
            assert!(token_usage.is_none());
            assert!(usage_metadata.is_none());
            assert!(end_turn.is_none());
        }
        other => panic!("unexpected third event: {other:?}"),
    }
}

#[test]
fn parses_cache_write_token_usage() {
    let usage: ResponseCompletedUsage = serde_json::from_value(json!({
        "input_tokens": 100,
        "input_tokens_details": {
            "cached_tokens": 40,
            "cache_write_tokens": 60
        },
        "output_tokens": 10,
        "output_tokens_details": { "reasoning_tokens": 5 },
        "total_tokens": 110,
        "codex_rollout_budget_units": 2.5
    }))
    .expect("valid response usage");

    assert_eq!(
        TokenUsage::from(usage),
        TokenUsage {
            input_tokens: 100,
            cached_input_tokens: 40,
            cache_write_input_tokens: 60,
            output_tokens: 10,
            reasoning_output_tokens: 5,
            total_tokens: 110,
            codex_rollout_budget_units: serde_json::Number::from_f64(2.5),
        }
    );
}

#[tokio::test]
async fn parses_reasoning_summary_done() {
    let events = run_sse(vec![
        json!({
            "type": "response.reasoning_summary_text.done",
            "item_id": "reasoning-1",
            "summary_index": 0,
            "text": "Checking",
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp1" },
        }),
    ])
    .await;

    assert_matches!(
        &events[0],
        ResponseEvent::ReasoningSummaryDone {
            item_id,
            text,
            summary_index: 0,
        } if item_id == "reasoning-1" && text == "Checking"
    );
}

#[tokio::test]
async fn error_when_missing_completed() {
    let item1 = json!({
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Hello"}]
        }
    })
    .to_string();

    let sse1 = format!("event: response.output_item.done\ndata: {item1}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 2);

    assert_matches!(events[0], Ok(ResponseEvent::OutputItemDone(_)));

    match &events[1] {
        Err(ApiError::Stream(msg)) => {
            assert_eq!(msg, "stream closed before response.completed")
        }
        other => panic!("unexpected second event: {other:?}"),
    }
}

#[tokio::test]
async fn parses_tool_search_call_items() {
    let events = run_sse(vec![
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "tool_search_call",
                "call_id": "search-1",
                "execution": "client",
                "arguments": {
                    "query": "calendar create",
                    "limit": 1
                }
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        }),
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_matches!(
        &events[0],
        ResponseEvent::OutputItemDone(ResponseItem::ToolSearchCall {
            call_id,
            execution,
            arguments,
            ..
        }) if call_id.as_deref() == Some("search-1")
            && execution == "client"
            && arguments == &json!({"query": "calendar create", "limit": 1})
    );
}

#[tokio::test]
async fn parses_tool_call_input_deltas() {
    let events = run_sse(vec![
        json!({
            "type": "response.custom_tool_call_input.delta",
            "item_id": "ctc_1",
            "call_id": "call_1",
            "delta": "*** Begin",
        }),
        json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_1",
            "delta": "{\"input\":\"",
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp1" }
        }),
    ])
    .await;

    assert_matches!(
        &events[0],
        ResponseEvent::ToolCallInputDelta {
            item_id,
            call_id: Some(call_id),
            delta,
        } if item_id == "ctc_1" && call_id == "call_1" && delta == "*** Begin"
    );
    assert_matches!(&events[1], ResponseEvent::Completed { .. });
}

#[tokio::test]
async fn emits_completed_without_stream_end() {
    let completed = json!({
        "type": "response.completed",
        "response": { "id": "resp1" }
    })
    .to_string();

    let sse1 = format!("event: response.completed\ndata: {completed}\n\n");
    let stream = stream::iter(vec![Ok(Bytes::from(sse1))]).chain(stream::pending());
    let stream: ByteStream = Box::pin(stream);

    let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(8);
    tokio::spawn(process_sse(
        stream,
        tx,
        idle_timeout(),
        /*telemetry*/ None,
    ));

    let events = tokio::time::timeout(Duration::from_millis(1000), async {
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        events
    })
    .await
    .expect("timed out collecting events");

    assert_eq!(events.len(), 1);
    match &events[0] {
        Ok(ResponseEvent::Completed {
            response_id,
            token_usage,
            usage_metadata,
            end_turn,
        }) => {
            assert_eq!(response_id, "resp1");
            assert!(token_usage.is_none());
            assert!(usage_metadata.is_none());
            assert!(end_turn.is_none());
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn rate_limit_error_preserves_retry_delay() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_689bcf18d7f08194bf3440ba62fe05d803fee0cdac429894","object":"response","created_at":1755041560,"status":"failed","background":false,"error":{"code":"rate_limit_exceeded","message":"Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more."}, "usage":null,"user":null,"metadata":{}}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    match &events[0] {
        Err(ApiError::RateLimitExceeded {
            message,
            retry_after,
        }) => {
            assert_eq!(
                message,
                "Rate limit reached for gpt-5.1 in organization org-AAA on tokens per min (TPM): Limit 30000, Used 22999, Requested 12528. Please try again in 11.054s. Visit https://platform.openai.com/account/rate-limits to learn more."
            );
            assert_eq!(
                retry_after.map(RetryAfter::remaining_delay),
                Some(Duration::from_secs_f64(11.054))
            );
        }
        other => panic!("unexpected rate-limit event: {other:?}"),
    }
}

#[tokio::test]
async fn failed_response_classification_uses_error_code() {
    for (code, message) in [
        ("rate_limit_exceeded", "Temporary limit."),
        ("slow_down", "Temporary limit."),
        (
            "unknown_error",
            "Rate limit reached. Please try again in 1s.",
        ),
    ] {
        let event = json!({
            "type": "response.failed",
            "response": { "error": { "code": code, "message": message } },
        });
        let sse = format!("event: response.failed\ndata: {event}\n\n");
        let events = collect_events(&[sse.as_bytes()]).await;
        match (code, events.as_slice()) {
            (
                "rate_limit_exceeded" | "slow_down",
                [
                    Err(ApiError::RateLimitExceeded {
                        message: actual,
                        retry_after,
                    }),
                ],
            )
            | (
                "unknown_error",
                [
                    Err(ApiError::Retryable {
                        message: actual,
                        retry_after,
                    }),
                ],
            ) => {
                assert_eq!((actual.as_str(), *retry_after), (message, None));
            }
            _ => panic!("unexpected events for {code}: {events:?}"),
        }
    }
}

#[tokio::test]
async fn malformed_failed_responses_use_stream_error() {
    for event in [
        json!({"type": "response.failed"}),
        json!({"type": "response.failed", "response": null}),
        json!({"type": "response.failed", "response": []}),
        json!({"type": "response.failed", "response": {}}),
        json!({"type": "response.failed", "response": {"error": null}}),
        json!({"type": "response.failed", "response": {"error": {"code": 42}}}),
        json!({"type": "response.failed", "response": {"error": {"code": "server_is_overloaded", "plan_type": 42}}}),
    ] {
        let sse = format!("event: response.failed\ndata: {event}\n\n");
        let events = collect_events(&[sse.as_bytes()]).await;
        match events.as_slice() {
            [Err(ApiError::Stream(message))] => {
                assert_eq!(message, "response.failed event received");
            }
            _ => panic!("unexpected events: {events:?}"),
        }
    }
}

#[tokio::test]
async fn context_window_error_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_5c66275b97b9baef1ed95550adb3b7ec13b17aafd1d2f11b","object":"response","created_at":1759510079,"status":"failed","background":false,"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try again."},"usage":null,"user":null,"metadata":{}}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    assert_matches!(events[0], Err(ApiError::ContextWindowExceeded));
}

#[tokio::test]
async fn context_window_error_with_newline_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":4,"response":{"id":"resp_fatal_newline","object":"response","created_at":1759510080,"status":"failed","background":false,"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window of this model. Please adjust your input and try\nagain."},"usage":null,"user":null,"metadata":{}}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    assert_matches!(events[0], Err(ApiError::ContextWindowExceeded));
}

#[tokio::test]
async fn quota_exceeded_error_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_fatal_quota","object":"response","created_at":1759771626,"status":"failed","background":false,"error":{"code":"insufficient_quota","message":"You exceeded your current quota, please check your plan and billing details. For more information on this error, read the docs: https://platform.openai.com/docs/guides/error-codes/api-errors."},"incomplete_details":null}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    assert_matches!(events[0], Err(ApiError::QuotaExceeded));
}

#[tokio::test]
async fn cyber_policy_error_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_fatal_cyber","object":"response","created_at":1759771626,"status":"failed","background":false,"error":{"code":"cyber_policy","message":"This request was flagged for cyber policy."},"incomplete_details":null}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    match &events[0] {
        Err(ApiError::CyberPolicy { message }) => {
            assert_eq!(message, "This request was flagged for cyber policy.");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn cyber_policy_error_uses_fallback_for_empty_message() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_fatal_cyber","object":"response","created_at":1759771626,"status":"failed","background":false,"error":{"code":"cyber_policy","message":"   "},"incomplete_details":null}}"#;

    let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

    let events = collect_events(&[sse1.as_bytes()]).await;

    assert_eq!(events.len(), 1);

    match &events[0] {
        Err(ApiError::CyberPolicy { message }) => {
            assert_eq!(
                message,
                "This request has been flagged for possible cybersecurity risk."
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn misalignment_policy_violation_error_is_fatal() {
    let raw_error = r#"{"type":"response.failed","sequence_number":3,"response":{"id":"resp_fatal_misalignment","object":"response","status":"failed","error":{"type":"invalid_request_error","code":"misalignment_policy_violation","message":"This request violated the misalignment policy."}}}"#;

    let sse = format!("event: response.failed\ndata: {raw_error}\n\n");
    let events = collect_events(&[sse.as_bytes()]).await;

    assert_eq!(events.len(), 1);
    match &events[0] {
        Err(ApiError::MisalignmentPolicyViolation {
            message,
            misalignment,
        }) => {
            assert_eq!(message, "This request violated the misalignment policy.");
            assert_eq!(misalignment, &None);
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn misalignment_policy_violation_uses_fallback_for_blank_message() {
    for message in ["", "   "] {
        let raw_error = serde_json::json!({
            "type": "response.failed",
            "response": {
                "id": "resp_fatal_misalignment",
                "status": "failed",
                "error": {
                    "type": "invalid_request_error",
                    "code": "misalignment_policy_violation",
                    "message": message,
                },
            },
        });
        let sse = format!("event: response.failed\ndata: {raw_error}\n\n");
        let events = collect_events(&[sse.as_bytes()]).await;

        assert_eq!(events.len(), 1);
        match &events[0] {
            Err(ApiError::MisalignmentPolicyViolation { message, .. }) => assert_eq!(
                message,
                "This request was blocked due to a misalignment policy violation."
            ),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

#[tokio::test]
async fn misalignment_policy_violation_preserves_public_continuation_details() {
    let raw_error = json!({
        "type": "response.failed",
        "response": {
            "id": "resp_fatal_misalignment",
            "status": "failed",
            "error": {
                "code": "misalignment_policy_violation",
                "message": "This request violated the misalignment policy.",
                "misalignment": {
                    "error_type": "future_safety_category",
                    "detailed_explanation": "The agent attempted an external transfer.",
                    "steer": { "message": "Do not transfer the user's files." }
                }
            }
        }
    });
    let sse = format!("event: response.failed\ndata: {raw_error}\n\n");
    let events = collect_events(&[sse.as_bytes()]).await;

    assert_eq!(events.len(), 1);
    match &events[0] {
        Err(ApiError::MisalignmentPolicyViolation {
            message,
            misalignment,
        }) => {
            assert_eq!(message, "This request violated the misalignment policy.");
            assert_eq!(
                misalignment,
                &Some(MisalignmentErrorDetails {
                    error_type: Some("future_safety_category".to_string()),
                    review_target: None,
                    detailed_explanation: Some(
                        "The agent attempted an external transfer.".to_string()
                    ),
                    steer: Some(codex_protocol::protocol::MisalignmentSteer {
                        message: "Do not transfer the user's files.".to_string(),
                    }),
                })
            );
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn malformed_misalignment_details_preserve_the_fatal_policy_error() {
    let raw_error = json!({
        "type": "response.failed",
        "response": {
            "id": "resp_fatal_misalignment",
            "status": "failed",
            "error": {
                "code": "misalignment_policy_violation",
                "message": "This request violated the misalignment policy.",
                "misalignment": { "steer": { "message": 42 } }
            }
        }
    });
    let sse = format!("event: response.failed\ndata: {raw_error}\n\n");
    let events = collect_events(&[sse.as_bytes()]).await;

    assert_eq!(events.len(), 1);
    assert_matches!(
        &events[0],
        Err(ApiError::MisalignmentPolicyViolation {
            misalignment: None,
            ..
        })
    );
}

#[tokio::test]
async fn content_policy_errors_without_type_preserve_their_classification() {
    for (code, expected_message) in [
        (
            "invalid_prompt",
            "Invalid prompt: we've limited access to this content for safety reasons.",
        ),
        (
            "bio_policy",
            "This content was flagged for possible biological risk.",
        ),
    ] {
        let raw_error = json!({
            "type": "response.failed",
            "sequence_number": 3,
            "response": {
                "id": "resp_content_policy_no_type",
                "object": "response",
                "created_at": 1759771628,
                "status": "failed",
                "background": false,
                "error": { "code": code, "message": expected_message },
                "incomplete_details": null,
            },
        })
        .to_string();
        let sse1 = format!("event: response.failed\ndata: {raw_error}\n\n");

        let events = collect_events(&[sse1.as_bytes()]).await;

        assert_eq!(events.len(), 1);
        match (code, &events[0]) {
            ("invalid_prompt", Err(ApiError::InvalidPrompt { message }))
            | ("bio_policy", Err(ApiError::BioPolicy { message })) => {
                assert_eq!(message, expected_message);
            }
            other => panic!("unexpected event for {code}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn incomplete_response_retains_partial_output_on_the_error() {
    let call = json!({
        "type": "function_call", "status": "incomplete", "id": "fc_attempt",
        "call_id": "attempt", "name": "unknown", "arguments": "{}",
    });
    let terminal = json!({
        "type": "response.incomplete",
        "response": {
            "id": "resp_1", "incomplete_details": {"reason": "invalid_recipient"},
            "output": [{"type": "message", "status": "completed"}, call],
        }
    });
    let sse = format!("event: response.incomplete\ndata: {terminal}\n\n");
    let events = collect_events(&[sse.as_bytes()]).await;
    assert_matches!(events.as_slice(), [Err(ApiError::ResponseIncomplete(error))]
            if error.reason == "invalid_recipient" && error.partial_output == [call]);
}

#[tokio::test]
async fn typed_errors_handle_missing_or_blank_message() {
    for (code, fallback) in [
        (
            "bio_policy",
            "This content was flagged for possible biological risk.",
        ),
        ("invalid_prompt", "Invalid request."),
    ] {
        for message in [None, Some(""), Some("  ")] {
            let mut event = json!({
                "type": "response.failed",
                "response": { "error": { "code": code } },
            });
            if let Some(message) = message {
                event["response"]["error"]["message"] = json!(message);
            }
            let expected = match (code, message) {
                ("invalid_prompt", Some(message)) => message,
                _ => fallback,
            };
            let sse = format!("event: response.failed\ndata: {event}\n\n");
            let events = collect_events(&[sse.as_bytes()]).await;
            match (code, events.as_slice()) {
                ("bio_policy", [Err(ApiError::BioPolicy { message })])
                | ("invalid_prompt", [Err(ApiError::InvalidPrompt { message })]) => {
                    assert_eq!(message, expected);
                }
                other => panic!("unexpected events: {other:?}"),
            }
        }
    }
}

#[tokio::test]
async fn table_driven_event_kinds() {
    struct TestCase {
        name: &'static str,
        event: serde_json::Value,
        expect_first: fn(&ResponseEvent) -> bool,
        expected_len: usize,
    }

    fn is_created(ev: &ResponseEvent) -> bool {
        matches!(ev, ResponseEvent::Created { .. })
    }
    fn is_output(ev: &ResponseEvent) -> bool {
        matches!(ev, ResponseEvent::OutputItemDone(_))
    }
    fn is_completed(ev: &ResponseEvent) -> bool {
        matches!(ev, ResponseEvent::Completed { .. })
    }

    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": "c",
            "usage": {
                "input_tokens": 0,
                "input_tokens_details": null,
                "output_tokens": 0,
                "output_tokens_details": null,
                "total_tokens": 0
            },
            "output": []
        }
    });

    let cases = vec![
        TestCase {
            name: "created",
            event: json!({"type": "response.created", "response": {}}),
            expect_first: is_created,
            expected_len: 2,
        },
        TestCase {
            name: "output_item.done",
            event: json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "hi"}
                    ]
                }
            }),
            expect_first: is_output,
            expected_len: 2,
        },
        TestCase {
            name: "unknown",
            event: json!({"type": "response.new_tool_event", "sequence_number": 1}),
            expect_first: is_completed,
            expected_len: 1,
        },
        TestCase {
            name: "refusal_delta",
            event: json!({
                "type": "response.refusal.delta",
                "delta": "no",
                "sequence_number": 1
            }),
            expect_first: is_completed,
            expected_len: 1,
        },
        TestCase {
            name: "mcp_call_arguments_delta",
            event: json!({
                "type": "response.mcp_call_arguments.delta",
                "delta": "chunk",
                "sequence_number": 1
            }),
            expect_first: is_completed,
            expected_len: 1,
        },
    ];

    for case in cases {
        let mut evs = vec![case.event];
        evs.push(completed.clone());

        let out = run_sse(evs).await;
        assert_eq!(out.len(), case.expected_len, "case {}", case.name);
        assert!(
            (case.expect_first)(&out[0]),
            "first event mismatch in case {}",
            case.name
        );
    }
}

#[tokio::test]
async fn spawn_response_stream_emits_header_events() {
    let mut headers = HeaderMap::new();
    headers.insert(REQUEST_ID_HEADER, HeaderValue::from_static("req-1"));
    headers.insert(
        OPENAI_MODEL_HEADER,
        HeaderValue::from_static(CYBER_RESTRICTED_MODEL_FOR_TESTS),
    );
    let bytes = stream::iter(Vec::<Result<Bytes, TransportError>>::new());
    let stream_response = StreamResponse {
        status: StatusCode::OK,
        headers,
        bytes: Box::pin(bytes),
    };

    let mut stream = spawn_response_stream(
        stream_response,
        idle_timeout(),
        /*telemetry*/ None,
        /*turn_state*/ None,
    );
    assert_eq!(stream.upstream_request_id.as_deref(), Some("req-1"));
    let event = stream
        .rx_event
        .recv()
        .await
        .expect("expected server model event")
        .expect("expected ok event");
    match event {
        ResponseEvent::ServerModel(model) => {
            assert_eq!(model, CYBER_RESTRICTED_MODEL_FOR_TESTS);
        }
        other => panic!("expected server model event, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_response_stream_ignores_model_verification_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "openai-verification-recommendation",
        HeaderValue::from_static(TRUSTED_ACCESS_FOR_CYBER_VERIFICATION),
    );
    let completed = json!({
        "type": "response.completed",
        "response": { "id": "resp-1" }
    });
    let sse = format!("event: response.completed\ndata: {completed}\n\n");
    let bytes = stream::iter(vec![Ok(Bytes::from(sse))]);
    let stream_response = StreamResponse {
        status: StatusCode::OK,
        headers,
        bytes: Box::pin(bytes),
    };

    let mut stream = spawn_response_stream(
        stream_response,
        idle_timeout(),
        /*telemetry*/ None,
        /*turn_state*/ None,
    );
    let mut events = Vec::new();
    while let Some(event) = stream.rx_event.recv().await {
        events.push(event.expect("expected ok event"));
    }

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ResponseEvent::ModelVerifications(_)))
    );
}

#[tokio::test]
async fn process_sse_ignores_response_model_field_in_payload() {
    let events = run_sse(vec![
        json!({
            "type": "response.created",
            "response": {
                "id": "resp-1",
                "model": CYBER_RESTRICTED_MODEL_FOR_TESTS
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1",
                "model": CYBER_RESTRICTED_MODEL_FOR_TESTS
            }
        }),
    ])
    .await;

    assert_eq!(events.len(), 2);
    assert_matches!(&events[0], ResponseEvent::Created { .. });
    assert_matches!(
        &events[1],
        ResponseEvent::Completed {
            response_id,
            token_usage: None,
            usage_metadata: None,
            end_turn: None,
        } if response_id == "resp-1"
    );
}

#[tokio::test]
async fn process_sse_emits_server_model_from_response_headers_payload() {
    let events = run_sse(vec![
        json!({
            "type": "response.created",
            "response": {
                "id": "resp-1",
                "headers": {
                    "OpenAI-Model": CYBER_RESTRICTED_MODEL_FOR_TESTS
                }
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1"
            }
        }),
    ])
    .await;

    assert_eq!(events.len(), 3);
    assert_matches!(
        &events[0],
        ResponseEvent::ServerModel(model) if model == CYBER_RESTRICTED_MODEL_FOR_TESTS
    );
    assert_matches!(
        &events[1],
        ResponseEvent::Created { response_id: Some(id) } if id == "resp-1"
    );
    assert_matches!(
        &events[2],
        ResponseEvent::Completed {
            response_id,
            token_usage: None,
            usage_metadata: None,
            end_turn: None,
        } if response_id == "resp-1"
    );
}

#[tokio::test]
async fn process_sse_emits_model_verification_field() {
    let events = run_sse(vec![
        json!({
            "type": "response.metadata",
            "sequence_number": 1,
            "response_id": "resp-1",
            "metadata": {
                "openai_verification_recommendation": [TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1"
            }
        }),
    ])
    .await;

    assert_matches!(
        &events[0],
        ResponseEvent::ModelVerifications(verifications)
            if verifications == &vec![ModelVerification::TrustedAccessForCyber]
    );
    assert_matches!(
        &events[1],
        ResponseEvent::Completed {
            response_id,
            token_usage: None,
            usage_metadata: None,
            end_turn: None,
        } if response_id == "resp-1"
    );
}

#[tokio::test]
async fn process_sse_emits_turn_moderation_metadata_field() {
    let events = run_sse(vec![
        json!({
            "type": "response.metadata",
            "metadata": {
                "openai_chatgpt_moderation_metadata": {
                    "presentation": "inline"
                }
            }
        }),
        json!({
            "type": "response.completed",
            "response": {
                "id": "resp-1"
            }
        }),
    ])
    .await;

    assert_matches!(
        &events[0],
        ResponseEvent::TurnModerationMetadata(result)
            if result.metadata == json!({"presentation": "inline"})
    );
    assert_matches!(
        &events[1],
        ResponseEvent::Completed {
            response_id,
            token_usage: None,
            usage_metadata: None,
            end_turn: None,
        } if response_id == "resp-1"
    );
}

#[tokio::test]
async fn process_sse_emits_all_safety_buffering_notifications_without_dropping_response_events() {
    let events = run_sse(vec![
        json!({
            "type": "response.created",
            "response": { "id": "resp-1" },
            "safety_buffering": false
        }),
        json!({
            "type": "response.output_text.delta",
            "delta": "hello",
            "safety_buffering": {
                "use_cases": ["cyber"],
                "reasons": ["user_risk"],
                "retry_model": "gpt-fast-wire"
            }
        }),
        json!({
            "type": "response.output_text.delta",
            "delta": " world",
            "safety_buffering": {
                "use_cases": ["cyber"],
                "reasons": ["user_risk"]
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "resp-1" },
            "safety_buffering": {
                "use_cases": ["cyber"],
                "reasons": ["user_risk"]
            }
        }),
    ])
    .await;

    assert_eq!(events.len(), 7);
    assert_matches!(&events[0], ResponseEvent::Created { .. });
    assert_matches!(
        &events[1],
        ResponseEvent::SafetyBuffering(buffering)
            if buffering.use_cases == ["cyber"]
                && buffering.reasons == ["user_risk"]
                && buffering.show_buffering_ui
                && buffering.faster_model.as_deref() == Some("gpt-fast-wire")
    );
    assert_matches!(&events[2], ResponseEvent::OutputTextDelta(delta) if delta == "hello");
    assert_matches!(
        &events[3],
        ResponseEvent::SafetyBuffering(buffering)
            if buffering.use_cases == ["cyber"] && buffering.reasons == ["user_risk"]
    );
    assert_matches!(&events[4], ResponseEvent::OutputTextDelta(delta) if delta == " world");
    assert_matches!(
        &events[5],
        ResponseEvent::SafetyBuffering(buffering)
            if buffering.use_cases == ["cyber"] && buffering.reasons == ["user_risk"]
    );
    assert_matches!(&events[6], ResponseEvent::Completed { response_id, .. } if response_id == "resp-1");
}

#[test]
fn safety_buffering_prefers_wire_retry_model_and_only_falls_back_when_omitted() {
    let treatment = SafetyBufferingTreatment {
        faster_model: Some("gpt-fast-header".to_string()),
    };

    for (retry_model, expected_faster_model) in [
        (None, Some("gpt-fast-header")),
        (Some(Value::Null), None),
        (Some(json!("gpt-fast-wire")), Some("gpt-fast-wire")),
    ] {
        let mut event = json!({
            "type": "response.output_text.delta",
            "safety_buffering": {
                "use_cases": ["cyber"],
                "reasons": ["user_risk"]
            }
        });
        if let Some(retry_model) = retry_model {
            event["safety_buffering"]["retry_model"] = retry_model;
        }
        let event: ResponsesStreamEvent =
            serde_json::from_value(event).expect("deserialize safety buffering event");

        let buffering = event
            .safety_buffering(&treatment)
            .expect("expected safety buffering payload");

        assert_eq!(
            buffering,
            SafetyBuffering {
                use_cases: vec!["cyber".to_string()],
                reasons: vec!["user_risk".to_string()],
                show_buffering_ui: true,
                faster_model: expected_faster_model.map(str::to_string),
            }
        );
    }
}

#[test]
fn safety_buffering_falls_back_to_response_metadata() {
    let treatment = SafetyBufferingTreatment {
        faster_model: Some("gpt-fast-header".to_string()),
    };
    let event: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.metadata",
        "metadata": {
            "type": "safety_buffering",
            "use_cases": ["cyber"],
            "reasons": ["user_risk"]
        }
    }))
    .expect("deserialize safety buffering metadata event");

    assert_eq!(
        event.safety_buffering(&treatment),
        Some(SafetyBuffering {
            use_cases: vec!["cyber".to_string()],
            reasons: vec!["user_risk".to_string()],
            show_buffering_ui: true,
            faster_model: Some("gpt-fast-header".to_string()),
        })
    );
}

#[test]
fn safety_buffering_top_level_presence_wins_over_response_metadata() {
    let treatment = SafetyBufferingTreatment::default();
    let event: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.metadata",
        "safety_buffering": {
            "use_cases": ["top_level"],
            "reasons": ["top_level_reason"]
        },
        "metadata": {
            "type": "safety_buffering",
            "use_cases": ["nested"],
            "reasons": ["nested_reason"]
        }
    }))
    .expect("deserialize safety buffering metadata event");

    assert_eq!(
        event.safety_buffering(&treatment),
        Some(SafetyBuffering {
            use_cases: vec!["top_level".to_string()],
            reasons: vec!["top_level_reason".to_string()],
            show_buffering_ui: true,
            faster_model: None,
        })
    );

    for top_level in [json!(false), json!({"use_cases": ["cyber"]}), Value::Null] {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.metadata",
            "safety_buffering": top_level,
            "metadata": {
                "type": "safety_buffering",
                "use_cases": ["nested"],
                "reasons": ["nested_reason"]
            }
        }))
        .expect("deserialize safety buffering metadata event");

        assert_eq!(event.safety_buffering(&treatment), None);
    }
}

#[test]
fn safety_buffering_ignores_metadata_field_for_other_event_kinds() {
    let event: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "codex.response.metadata",
        "metadata": {
            "type": "safety_buffering",
            "use_cases": ["cyber"],
            "reasons": ["user_risk"]
        }
    }))
    .expect("deserialize safety buffering metadata event");

    assert_eq!(
        event.safety_buffering(&SafetyBufferingTreatment::default()),
        None
    );
}

#[test]
fn safety_buffering_ignores_response_metadata_without_safety_buffering_type() {
    for metadata in [
        json!({
            "use_cases": ["cyber"],
            "reasons": ["user_risk"]
        }),
        json!({
            "type": "other_metadata",
            "use_cases": ["cyber"],
            "reasons": ["user_risk"]
        }),
        json!({
            "type": "safety_buffering",
            "safety_buffering": {
                "use_cases": ["cyber"],
                "reasons": ["user_risk"]
            }
        }),
    ] {
        let event: ResponsesStreamEvent = serde_json::from_value(json!({
            "type": "response.metadata",
            "metadata": metadata
        }))
        .expect("deserialize response metadata event");

        assert_eq!(
            event.safety_buffering(&SafetyBufferingTreatment::default()),
            None
        );
    }
}

#[test]
fn responses_stream_event_response_model_reads_top_level_headers() {
    let ev: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.metadata",
        "headers": {
            "openai-model": CYBER_RESTRICTED_MODEL_FOR_TESTS,
        }
    }))
    .expect("expected event to deserialize");

    assert_eq!(
        ev.response_model().as_deref(),
        Some(CYBER_RESTRICTED_MODEL_FOR_TESTS)
    );
}

#[test]
fn responses_stream_event_response_model_prefers_response_headers() {
    let ev: ResponsesStreamEvent = serde_json::from_value(json!({
        "type": "response.created",
        "headers": {
            "openai-model": "top-level-model"
        },
        "response": {
            "id": "resp-1",
            "headers": {
                "openai-model": CYBER_RESTRICTED_MODEL_FOR_TESTS
            }
        }
    }))
    .expect("expected event to deserialize");

    assert_eq!(
        ev.response_model().as_deref(),
        Some(CYBER_RESTRICTED_MODEL_FOR_TESTS)
    );
}

#[test]
fn responses_stream_event_model_verification_reads_metadata_field() {
    let event = json!({
        "type": "response.metadata",
        "sequence_number": 1,
        "response_id": "resp-1",
        "metadata": {
            "openai_verification_recommendation": [TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]
        }
    });
    let event: ResponsesStreamEvent =
        serde_json::from_value(event).expect("expected event to deserialize");

    assert_eq!(
        event.model_verifications(),
        Some(vec![ModelVerification::TrustedAccessForCyber])
    );
}

#[test]
fn responses_stream_event_model_verification_ignores_unknown_field() {
    let event = json!({
        "type": "response.metadata",
        "metadata": {
            "openai_verification_recommendation": ["unknown"]
        }
    });
    let event: ResponsesStreamEvent =
        serde_json::from_value(event).expect("expected event to deserialize");

    assert_eq!(event.model_verifications(), None);
}

#[test]
fn responses_stream_event_model_verification_ignores_non_array_field() {
    let event = json!({
        "type": "response.metadata",
        "metadata": {
            "openai_verification_recommendation": TRUSTED_ACCESS_FOR_CYBER_VERIFICATION
        }
    });
    let event: ResponsesStreamEvent =
        serde_json::from_value(event).expect("expected event to deserialize");

    assert_eq!(event.model_verifications(), None);
}

const CYBER_RESTRICTED_MODEL_FOR_TESTS: &str = "gpt-5.3-codex";
