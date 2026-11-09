use super::*;
use crate::app_event::AppEvent;
use crate::bottom_pane::selection_popup_common::menu_surface_inset;
use crate::render::renderable::Renderable;
use codex_config::types::KeybindingSpec;
use codex_config::types::KeybindingsSpec;
use codex_config::types::TuiKeymap;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc::unbounded_channel;
use unicode_width::UnicodeWidthStr;

fn test_sender() -> (
    AppEventSender,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    (AppEventSender::new(tx_raw), rx)
}

fn expect_interrupt_only(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let event = rx.try_recv().expect("expected interrupt AppEvent");
    let AppEvent::CodexOp(op) = event else {
        panic!("expected CodexOp");
    };
    assert!(matches!(op, Op::Interrupt));
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvents before interrupt completion"
    );
}

fn question_with_options(id: &str, header: &str) -> ToolRequestUserInputQuestion {
    ToolRequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Choose an option.".to_string(),
        is_other: false,
        is_secret: false,
        options: Some(vec![
            ToolRequestUserInputOption {
                label: "Option 1".to_string(),
                description: "First choice.".to_string(),
            },
            ToolRequestUserInputOption {
                label: "Option 2".to_string(),
                description: "Second choice.".to_string(),
            },
            ToolRequestUserInputOption {
                label: "Option 3".to_string(),
                description: "Third choice.".to_string(),
            },
        ]),
    }
}

fn question_with_options_and_other(id: &str, header: &str) -> ToolRequestUserInputQuestion {
    ToolRequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Choose an option.".to_string(),
        is_other: true,
        is_secret: false,
        options: Some(vec![
            ToolRequestUserInputOption {
                label: "Option 1".to_string(),
                description: "First choice.".to_string(),
            },
            ToolRequestUserInputOption {
                label: "Option 2".to_string(),
                description: "Second choice.".to_string(),
            },
            ToolRequestUserInputOption {
                label: "Option 3".to_string(),
                description: "Third choice.".to_string(),
            },
        ]),
    }
}

fn question_with_wrapped_options(id: &str, header: &str) -> ToolRequestUserInputQuestion {
    ToolRequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Choose the next step for this task.".to_string(),
        is_other: false,
        is_secret: false,
        options: Some(vec![
            ToolRequestUserInputOption {
                label: "Discuss a code change".to_string(),
                description: "Walk through a plan, then implement it together with careful checks."
                    .to_string(),
            },
            ToolRequestUserInputOption {
                label: "Run targeted tests".to_string(),
                description:
                    "Pick the most relevant crate and validate the current behavior first."
                        .to_string(),
            },
            ToolRequestUserInputOption {
                label: "Review the diff".to_string(),
                description:
                    "Summarize the changes and highlight the most important risks and gaps."
                        .to_string(),
            },
        ]),
    }
}

fn question_with_very_long_option_text(id: &str, header: &str) -> ToolRequestUserInputQuestion {
    ToolRequestUserInputQuestion {
            id: id.to_string(),
            header: header.to_string(),
            question: "Choose one option.".to_string(),
            is_other: false,
            is_secret: false,
            options: Some(vec![
                ToolRequestUserInputOption {
                    label: "Job: running/completed/failed/expired; Run/Experiment: succeeded/failed/unknown (Recommended when triaging long-running background work and status transitions)".to_string(),
                    description: "Keep async job statuses for progress tracking and include enough context for debugging retries, stale workers, and unexpected expiration paths.".to_string(),
                },
                ToolRequestUserInputOption {
                    label: "Add a short status model".to_string(),
                    description: "Simpler labels with less detail for quick rollouts.".to_string(),
                },
            ]),
        }
}

fn question_with_long_scroll_options(id: &str, header: &str) -> ToolRequestUserInputQuestion {
    ToolRequestUserInputQuestion {
            id: id.to_string(),
            header: header.to_string(),
            question:
                "Choose one option; each hint is intentionally very long to test wrapped scrolling."
                    .to_string(),
            is_other: false,
            is_secret: false,
            options: Some(vec![
                ToolRequestUserInputOption {
                    label: "Use Detailed Hint A (Recommended)".to_string(),
                    description: "Select this if you want a deliberately overextended explanatory hint that reads like a miniature specification, including context, rationale, expected behavior, and an explicit statement that this choice is mainly for testing how gracefully the interface wraps, truncates, and preserves readability under unusually verbose helper text conditions.".to_string(),
                },
                ToolRequestUserInputOption {
                    label: "Use Detailed Hint B".to_string(),
                    description: "Select this if you want an equally verbose but differently phrased guidance block that emphasizes user-facing clarity, spacing tolerance, multiline wrapping, visual hierarchy interactions, and whether long descriptive metadata remains understandable when scanned quickly in a constrained layout where cognitive load is already high.".to_string(),
                },
                ToolRequestUserInputOption {
                    label: "Use Detailed Hint C".to_string(),
                    description: "Select this when you specifically want to verify that navigating downward will keep the currently highlighted option visible, even when previous options consume many wrapped lines and would otherwise push the selection out of the viewport.".to_string(),
                },
                ToolRequestUserInputOption {
                    label: "None of the above".to_string(),
                    description:
                        "Use this only if the previous long-form options do not apply.".to_string(),
                },
            ]),
        }
}

fn question_without_options(id: &str, header: &str) -> ToolRequestUserInputQuestion {
    ToolRequestUserInputQuestion {
        id: id.to_string(),
        header: header.to_string(),
        question: "Share details.".to_string(),
        is_other: false,
        is_secret: false,
        options: None,
    }
}

fn request_event(
    turn_id: &str,
    questions: Vec<ToolRequestUserInputQuestion>,
) -> ToolRequestUserInputParams {
    ToolRequestUserInputParams {
        thread_id: "00000000-0000-0000-0000-000000000001".to_string(),
        item_id: "call-1".to_string(),
        turn_id: turn_id.to_string(),
        questions,
        is_blocking: true,
        auto_resolution_ms: None,
    }
}

fn expect_user_input_answer(event: AppEvent) -> (ThreadId, String, ToolRequestUserInputResponse) {
    let AppEvent::SubmitThreadOp {
        thread_id,
        op: Op::UserInputAnswer { id, response },
    } = event
    else {
        panic!("expected thread-scoped UserInputAnswer event");
    };
    (thread_id, id, response)
}

fn request_event_with_auto_resolution(
    turn_id: &str,
    questions: Vec<ToolRequestUserInputQuestion>,
) -> ToolRequestUserInputParams {
    let mut request = request_event(turn_id, questions);
    request.is_blocking = false;
    request.auto_resolution_ms = Some(60_000);
    request
}

fn snapshot_buffer(buf: &Buffer) -> String {
    let mut lines = Vec::new();
    for y in 0..buf.area().height {
        let mut row = String::new();
        for x in 0..buf.area().width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        lines.push(row);
    }
    lines.join("\n")
}

fn render_snapshot(overlay: &RequestUserInputOverlay, area: Rect) -> String {
    render_snapshot_at(overlay, area, Instant::now())
}

fn render_snapshot_at(overlay: &RequestUserInputOverlay, area: Rect, now: Instant) -> String {
    let mut buf = Buffer::empty(area);
    overlay.render_ui_at(area, &mut buf, now);
    snapshot_buffer(&buf)
}

#[test]
fn queued_requests_are_fifo() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.try_consume_user_input_request(request_event(
        "turn-2",
        vec![question_with_options("q2", "Second")],
    ));
    overlay.try_consume_user_input_request(request_event(
        "turn-3",
        vec![question_with_options("q3", "Third")],
    ));

    overlay.submit_answers();
    assert_eq!(overlay.request.turn_id, "turn-2");

    overlay.submit_answers();
    assert_eq!(overlay.request.turn_id, "turn-3");
}

#[test]
fn interrupt_discards_queued_requests_and_emits_interrupt() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.try_consume_user_input_request(ToolRequestUserInputParams {
        thread_id: "thread-1".to_string(),
        item_id: "call-2".to_string(),
        turn_id: "turn-2".to_string(),
        questions: vec![question_with_options("q2", "Second")],
        is_blocking: true,
        auto_resolution_ms: None,
    });
    overlay.try_consume_user_input_request(ToolRequestUserInputParams {
        thread_id: "thread-1".to_string(),
        item_id: "call-3".to_string(),
        turn_id: "turn-3".to_string(),
        questions: vec![question_with_options("q3", "Third")],
        is_blocking: true,
        auto_resolution_ms: None,
    });

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert!(overlay.done, "expected overlay to be done");
    expect_interrupt_only(&mut rx);
}

#[test]
fn auto_resolution_absent_has_no_timer() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();

    assert_eq!(
        overlay.auto_resolution_timing_at(now),
        AutoResolutionTiming::Disabled
    );
    assert_eq!(overlay.auto_resolution_next_frame_delay_at(now), None);
    assert_eq!(overlay.auto_resolution_countdown_text_at(now), None);
}

#[test]
fn auto_resolution_uses_is_blocking_without_auto_resolution_ms() {
    let (tx, mut rx) = test_sender();
    let mut request = request_event("turn-1", vec![question_with_options("q1", "First")]);
    request.is_blocking = false;
    assert_eq!(request.auto_resolution_ms, None);
    let mut overlay = RequestUserInputOverlay::new(
        request, tx, /*has_input_focus*/ true, /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    overlay.request_started_at = now;

    assert_eq!(
        overlay.auto_resolution_timing_at(now),
        AutoResolutionTiming::HiddenGrace {
            remaining: AUTO_RESOLUTION_HIDDEN_GRACE
        }
    );

    let total_timeout = AUTO_RESOLUTION_HIDDEN_GRACE + AUTO_RESOLUTION_VISIBLE_COUNTDOWN;
    let due = now + total_timeout;
    assert!(overlay.pre_draw_tick(due));
    assert!(overlay.done);

    let event = rx.try_recv().expect("expected UserInputAnswer event");
    let (thread_id, id, response) = expect_user_input_answer(event);
    assert_eq!(thread_id.to_string(), overlay.request.thread_id);
    assert_eq!(id, "turn-1");
    assert_eq!(response.answers, HashMap::new());
}

#[test]
fn auto_resolution_hides_timer_during_grace_period() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    overlay.request_started_at = now;

    assert_eq!(
        overlay.auto_resolution_timing_at(now),
        AutoResolutionTiming::HiddenGrace {
            remaining: AUTO_RESOLUTION_HIDDEN_GRACE
        }
    );
    assert_eq!(
        overlay.auto_resolution_next_frame_delay_at(now),
        Some(AUTO_RESOLUTION_HIDDEN_GRACE)
    );
    assert!(!render_snapshot_at(&overlay, Rect::new(0, 0, 120, 16), now).contains("auto-resolves"));
}

#[test]
fn auto_resolution_visible_countdown_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution(
            "turn-1",
            vec![
                question_with_options("q1", "First"),
                question_with_options("q2", "Second"),
                question_with_options("q3", "Third"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    overlay.request_started_at = now - AUTO_RESOLUTION_HIDDEN_GRACE;

    insta::assert_snapshot!(
        "request_user_input_auto_resolution_countdown",
        render_snapshot_at(&overlay, Rect::new(0, 0, 120, 16), now)
    );
}

#[test]
fn auto_resolution_visible_countdown_is_red() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    overlay.request_started_at = now - AUTO_RESOLUTION_HIDDEN_GRACE;
    let area = Rect::new(0, 0, 120, 16);
    let mut buf = Buffer::empty(area);

    overlay.render_ui_at(area, &mut buf, now);

    let rendered = snapshot_buffer(&buf);
    let progress_line = rendered.lines().nth(1).expect("expected progress line");
    let countdown = "auto-resolves in 1m 00s";
    let countdown_byte_idx = progress_line
        .find(countdown)
        .expect("expected countdown in progress line");
    let countdown_x = progress_line[..countdown_byte_idx].width();
    for offset in 0..countdown.width() {
        assert_eq!(
            buf[((countdown_x + offset) as u16, 1)].style().fg,
            Some(Color::Red)
        );
    }
    let prefix_byte_idx = progress_line
        .find("Question")
        .expect("expected question prefix in progress line");
    let prefix_x = progress_line[..prefix_byte_idx].width();
    assert_ne!(buf[(prefix_x as u16, 1)].style().fg, Some(Color::Red));
}

#[test]
fn auto_resolution_expiry_emits_empty_answer() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    let total_timeout = AUTO_RESOLUTION_HIDDEN_GRACE + AUTO_RESOLUTION_VISIBLE_COUNTDOWN;
    overlay.request_started_at = now - total_timeout;

    assert!(overlay.pre_draw_tick(now));
    assert!(overlay.done);

    let event = rx.try_recv().expect("expected UserInputAnswer event");
    let (thread_id, id, response) = expect_user_input_answer(event);
    assert_eq!(thread_id.to_string(), overlay.request.thread_id);
    assert_eq!(id, "turn-1");
    assert_eq!(response.answers, HashMap::new());

    let event = rx.try_recv().expect("expected history cell event");
    assert!(
        matches!(event, AppEvent::InsertHistoryCell(_)),
        "expected history cell event"
    );
}

#[test]
fn auto_resolution_key_interaction_snoozes_timer() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    let total_timeout = AUTO_RESOLUTION_HIDDEN_GRACE + AUTO_RESOLUTION_VISIBLE_COUNTDOWN;
    overlay.request_started_at = now - AUTO_RESOLUTION_HIDDEN_GRACE;

    overlay.handle_key_event(KeyEvent::from(KeyCode::Down));

    assert_eq!(
        overlay.auto_resolution_timing_at(now + total_timeout),
        AutoResolutionTiming::Disabled
    );
    assert!(!overlay.pre_draw_tick(now + total_timeout));
    assert!(rx.try_recv().is_err());
}

#[test]
fn auto_resolution_paste_interaction_snoozes_timer() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let now = Instant::now();
    let total_timeout = AUTO_RESOLUTION_HIDDEN_GRACE + AUTO_RESOLUTION_VISIBLE_COUNTDOWN;
    overlay.request_started_at = now - AUTO_RESOLUTION_HIDDEN_GRACE;

    assert!(overlay.handle_paste("notes".to_string()));

    assert_eq!(
        overlay.auto_resolution_timing_at(now + total_timeout),
        AutoResolutionTiming::Disabled
    );
    assert!(!overlay.pre_draw_tick(now + total_timeout));
    assert!(rx.try_recv().is_err());
}

#[test]
fn auto_resolution_resets_for_queued_request() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event_with_auto_resolution("turn-1", vec![question_with_options("q1", "First")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.try_consume_user_input_request(request_event_with_auto_resolution(
        "turn-2",
        vec![question_with_options("q2", "Second")],
    ));
    let now = Instant::now();
    let total_timeout = AUTO_RESOLUTION_HIDDEN_GRACE + AUTO_RESOLUTION_VISIBLE_COUNTDOWN;
    overlay.request_started_at = now - total_timeout;

    assert!(overlay.pre_draw_tick(now));

    assert_eq!(overlay.request.turn_id, "turn-2");
    assert!(!overlay.auto_resolution_snoozed);
    assert_eq!(
        overlay.auto_resolution_timing_at(now),
        AutoResolutionTiming::HiddenGrace {
            remaining: AUTO_RESOLUTION_HIDDEN_GRACE
        }
    );
    assert!(!overlay.done);
    assert!(rx.try_recv().is_ok());
}

#[test]
fn resolved_request_dismisses_overlay_without_emitting_events() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        ToolRequestUserInputParams {
            thread_id: "thread-1".to_string(),
            item_id: "call-1".to_string(),
            turn_id: "turn-1".to_string(),
            questions: vec![question_with_options("q1", "First")],
            is_blocking: true,
            auto_resolution_ms: None,
        },
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert!(
        overlay.dismiss_app_server_request(&ResolvedAppServerRequest::UserInput {
            call_id: "call-1".to_string(),
        })
    );
    assert!(overlay.done, "resolved request should close the overlay");
    assert!(
        rx.try_recv().is_err(),
        "dismissing a stale request should not emit an interrupt or answer"
    );
}

#[test]
fn resolved_current_request_advances_to_next_same_turn_prompt() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        ToolRequestUserInputParams {
            thread_id: "thread-1".to_string(),
            item_id: "call-1".to_string(),
            turn_id: "turn-1".to_string(),
            questions: vec![question_with_options("q1", "First")],
            is_blocking: true,
            auto_resolution_ms: None,
        },
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.try_consume_user_input_request(ToolRequestUserInputParams {
        thread_id: "thread-1".to_string(),
        item_id: "call-2".to_string(),
        turn_id: "turn-1".to_string(),
        questions: vec![question_with_options("q2", "Second")],
        is_blocking: true,
        auto_resolution_ms: None,
    });

    assert!(
        overlay.dismiss_app_server_request(&ResolvedAppServerRequest::UserInput {
            call_id: "call-1".to_string(),
        })
    );

    assert!(!overlay.done, "newer same-turn prompt should stay pending");
    assert_eq!(overlay.request.item_id, "call-2");
    assert_eq!(overlay.request.turn_id, "turn-1");
    assert_eq!(overlay.request.questions[0].id, "q2");
    assert!(
        rx.try_recv().is_err(),
        "dismissing a stale request should not emit an interrupt or answer"
    );
}

#[test]
fn resolved_queued_request_removes_only_that_prompt() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        ToolRequestUserInputParams {
            thread_id: "00000000-0000-0000-0000-000000000001".to_string(),
            item_id: "call-1".to_string(),
            turn_id: "turn-1".to_string(),
            questions: vec![question_with_options("q1", "First")],
            is_blocking: true,
            auto_resolution_ms: None,
        },
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.try_consume_user_input_request(ToolRequestUserInputParams {
        thread_id: "thread-1".to_string(),
        item_id: "call-2".to_string(),
        turn_id: "turn-1".to_string(),
        questions: vec![question_with_options("q2", "Second")],
        is_blocking: true,
        auto_resolution_ms: None,
    });
    overlay.try_consume_user_input_request(ToolRequestUserInputParams {
        thread_id: "thread-1".to_string(),
        item_id: "call-3".to_string(),
        turn_id: "turn-1".to_string(),
        questions: vec![question_with_options("q3", "Third")],
        is_blocking: true,
        auto_resolution_ms: None,
    });

    assert!(
        overlay.dismiss_app_server_request(&ResolvedAppServerRequest::UserInput {
            call_id: "call-2".to_string(),
        })
    );

    assert_eq!(overlay.request.item_id, "call-1");
    assert!(
        rx.try_recv().is_err(),
        "dismissing a stale queued request should not emit an event"
    );
    overlay.submit_answers();
    assert_eq!(overlay.request.item_id, "call-3");
    assert_eq!(overlay.request.questions[0].id, "q3");
    assert!(
        rx.try_recv().is_ok(),
        "submitting the still-current prompt should emit an answer"
    );
    assert!(
        rx.try_recv().is_ok(),
        "submitting the still-current prompt should emit a history cell"
    );
}

#[test]
fn options_can_submit_empty_when_unanswered() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let (thread_id, id, response) = expect_user_input_answer(event);
    assert_eq!(thread_id.to_string(), overlay.request.thread_id);
    assert_eq!(id, "turn-1");
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

#[test]
fn enter_commits_default_selection_on_last_option_question() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, vec!["Option 1".to_string()]);
}

#[test]
fn enter_commits_default_selection_on_non_last_option_question() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_eq!(overlay.current_index(), 1);
    let first_answer = &overlay.answers[0];
    assert!(first_answer.answer_committed);
    assert_eq!(first_answer.options_state.selected_idx, Some(0));
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvent before full submission"
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let mut expected = HashMap::new();
    expected.insert(
        "q1".to_string(),
        ToolRequestUserInputAnswer {
            answers: vec!["Option 1".to_string()],
        },
    );
    expected.insert(
        "q2".to_string(),
        ToolRequestUserInputAnswer {
            answers: vec!["Option 1".to_string()],
        },
    );
    assert_eq!(response.answers, expected);
}

#[test]
fn number_keys_select_and_submit_options() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('2')));

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, vec!["Option 2".to_string()]);
}

#[test]
fn vim_keys_move_option_selection() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(0));

    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('j')));
    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(1));

    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('k')));
    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(0));
}

#[test]
fn typing_in_options_does_not_open_notes() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.current_index(), 0);
    assert_eq!(overlay.notes_ui_visible(), false);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
    assert_eq!(overlay.current_index(), 0);
    assert_eq!(overlay.notes_ui_visible(), false);
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.composer.current_text_with_pending(), "");
}

#[test]
fn h_l_move_between_questions_in_options() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.current_index(), 0);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('l')));
    assert_eq!(overlay.current_index(), 1);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
    assert_eq!(overlay.current_index(), 0);
}

#[test]
fn left_right_move_between_questions_in_options() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.current_index(), 0);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Right));
    assert_eq!(overlay.current_index(), 1);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Left));
    assert_eq!(overlay.current_index(), 0);
}

#[test]
fn horizontal_list_keys_move_between_questions_in_options() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.current_index(), 0);
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(overlay.current_index(), 1);
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(overlay.current_index(), 0);
}

#[test]
fn options_notes_focus_hides_question_navigation_tip() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec![
            "tab to add notes",
            "enter to submit answer",
            "←/→ to navigate questions",
            "esc to interrupt",
        ]
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec!["tab or esc to clear notes", "enter to submit answer",]
    );
}

#[test]
fn freeform_shows_ctrl_p_and_ctrl_n_question_navigation_tip() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.move_question(/*next*/ true);

    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec![
            "enter to submit all",
            "⌃p / ⌃n change question",
            "esc to interrupt",
        ]
    );
}

#[test]
fn freeform_footer_shows_configured_submit_binding() {
    let (tx, _rx) = test_sender();
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.submit = vec![crate::key_hint::ctrl(KeyCode::Char('j'))];
    let overlay = RequestUserInputOverlay::new_with_keymap(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );

    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(tip_texts, vec!["⌃j to submit answer", "esc to interrupt"]);
}

#[test]
fn freeform_footer_displays_configured_chords_without_internal_dispatch_keys() {
    for (specs, expected_tips) in [
        (
            KeybindingsSpec::One(KeybindingSpec("ctrl-x enter".to_string())),
            vec!["⌃x enter to submit answer", "esc to interrupt"],
        ),
        (
            KeybindingsSpec::Many(vec![
                KeybindingSpec("ctrl-enter".to_string()),
                KeybindingSpec("ctrl-x enter".to_string()),
            ]),
            vec!["⌃enter to submit answer", "esc to interrupt"],
        ),
        (
            KeybindingsSpec::Many(vec![
                KeybindingSpec("ctrl-x enter".to_string()),
                KeybindingSpec("ctrl-enter".to_string()),
            ]),
            vec!["⌃x enter to submit answer", "esc to interrupt"],
        ),
    ] {
        let (tx, _rx) = test_sender();
        let mut config = TuiKeymap::default();
        config.composer.submit = Some(specs);
        let keymap = RuntimeKeymap::from_config(&config).expect("valid submit keymap");
        let overlay = RequestUserInputOverlay::new_with_keymap(
            request_event("turn-1", vec![question_without_options("q1", "Notes")]),
            tx,
            /*has_input_focus*/ true,
            /*enhanced_keys_supported*/ false,
            /*disable_paste_burst*/ false,
            keymap,
        );
        let tips = overlay.footer_tips();

        assert_eq!(
            tips.iter().map(ToString::to_string).collect::<Vec<_>>(),
            expected_tips
        );
    }
}

#[test]
fn request_user_input_uses_remapped_interrupt_binding_while_notes_are_visible() {
    let (tx, mut rx) = test_sender();
    let mut keymap = RuntimeKeymap::defaults();
    keymap.chat.interrupt_turn = vec![crate::key_hint::plain(KeyCode::F(12))];
    let mut overlay = RequestUserInputOverlay::new_with_keymap(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));

    let tips = overlay.footer_tips();
    let tip_texts = tips.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        tip_texts,
        vec![
            "tab or esc to clear notes",
            "enter to submit answer",
            "f12 to interrupt",
        ]
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::F(12)));

    assert_eq!(overlay.done, true);
    expect_interrupt_only(&mut rx);
}

#[test]
fn tab_opens_notes_when_option_selected() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(1);

    assert_eq!(overlay.notes_ui_visible(), false);
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    assert_eq!(overlay.notes_ui_visible(), true);
    assert!(matches!(overlay.focus, Focus::Notes));
}

#[test]
fn switching_to_options_resets_notes_focus_when_notes_hidden() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_with_options("q2", "Pick one"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert!(matches!(overlay.focus, Focus::Notes));
    overlay.move_question(/*next*/ true);

    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
}

#[test]
fn switching_from_freeform_with_text_resets_focus_and_keeps_last_option_empty() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_with_options("q2", "Pick one"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay
        .composer
        .set_text_content("freeform notes".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.move_question(/*next*/ true);

    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(overlay.confirm_unanswered_active());
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvent before confirmation submit"
    );
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('1')));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
    let answer = response.answers.get("q2").expect("answer missing");
    assert_eq!(answer.answers, vec!["Option 1".to_string()]);
}

#[test]
fn esc_in_notes_mode_without_options_interrupts() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_eq!(overlay.done, true);
    expect_interrupt_only(&mut rx);
}

#[test]
fn esc_in_options_mode_interrupts() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_eq!(overlay.done, true);
    expect_interrupt_only(&mut rx);
}

#[test]
fn esc_in_notes_mode_clears_notes_and_hides_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);
    answer.answer_committed = true;

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(overlay.done, false);
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(overlay.composer.current_text_with_pending(), "");
    assert_eq!(answer.draft.text, "");
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert_eq!(answer.answer_committed, false);
    assert!(rx.try_recv().is_err());
}

#[test]
fn esc_in_notes_mode_with_text_clears_notes_and_hides_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);
    answer.answer_committed = true;

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Char('a')));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(overlay.done, false);
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(overlay.composer.current_text_with_pending(), "");
    assert_eq!(answer.draft.text, "");
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert_eq!(answer.answer_committed, false);
    assert!(rx.try_recv().is_err());
}

#[test]
fn esc_drops_committed_answers() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "First"),
                question_without_options("q2", "Second"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(
        rx.try_recv().is_err(),
        "unexpected AppEvent before interruption"
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Esc));

    expect_interrupt_only(&mut rx);
}

#[test]
fn backspace_in_options_clears_selection() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(1);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Backspace));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, None);
    assert_eq!(overlay.notes_ui_visible(), false);
    assert!(rx.try_recv().is_err());
}

#[test]
fn backspace_on_empty_notes_closes_notes_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    assert!(matches!(overlay.focus, Focus::Notes));
    assert_eq!(overlay.notes_ui_visible(), true);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Backspace));

    let answer = overlay.current_answer().expect("answer missing");
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert!(rx.try_recv().is_err());
}

#[test]
fn tab_in_notes_clears_notes_and_hides_ui() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay
        .composer
        .set_text_content("Some notes".to_string(), Vec::new(), Vec::new());

    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));

    let answer = overlay.current_answer().expect("answer missing");
    assert!(matches!(overlay.focus, Focus::Options));
    assert_eq!(overlay.notes_ui_visible(), false);
    assert_eq!(overlay.composer.current_text_with_pending(), "");
    assert_eq!(answer.draft.text, "");
    assert_eq!(answer.options_state.selected_idx, Some(0));
    assert!(rx.try_recv().is_err());
}

#[test]
fn skipped_option_questions_count_as_unanswered() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    assert_eq!(overlay.unanswered_count(), 1);
}

#[test]
fn highlighted_option_questions_are_unanswered() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    assert_eq!(overlay.unanswered_count(), 1);
}

#[test]
fn freeform_requires_enter_with_text_to_mark_answered() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay
        .composer
        .set_text_content("Draft".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    assert_eq!(overlay.unanswered_count(), 2);

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(overlay.answers[0].answer_committed, true);
    assert_eq!(overlay.unanswered_count(), 1);
}

#[test]
fn freeform_enter_with_empty_text_is_unanswered() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(overlay.answers[0].answer_committed, false);
    assert_eq!(overlay.unanswered_count(), 2);
}

#[test]
fn freeform_shift_enter_inserts_newline_without_advancing() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ true,
        /*disable_paste_burst*/ false,
    );

    overlay
        .composer
        .set_text_content("Draft".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

    assert_eq!(overlay.current_index(), 0);
    assert_eq!(overlay.composer.current_text_with_pending(), "Draft\n");
    assert_eq!(overlay.answers[0].answer_committed, false);
}

#[test]
fn freeform_uses_configured_composer_submit_binding() {
    let (tx, _rx) = test_sender();
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.submit = vec![crate::key_hint::ctrl(KeyCode::Char('j'))];
    let mut overlay = RequestUserInputOverlay::new_with_keymap(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );

    overlay
        .composer
        .set_text_content("Draft".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));

    assert_eq!(overlay.current_index(), 1);
    assert_eq!(overlay.answers[0].answer_committed, true);
}

#[test]
fn freeform_submit_binding_wins_over_question_navigation() {
    let (tx, _rx) = test_sender();
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.submit = vec![crate::key_hint::ctrl(KeyCode::Char('n'))];
    let mut overlay = RequestUserInputOverlay::new_with_keymap(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );

    overlay
        .composer
        .set_text_content("Draft".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));

    assert_eq!(overlay.current_index(), 1);
    assert_eq!(overlay.answers[0].answer_committed, true);
}

#[test]
fn freeform_questions_submit_empty_when_empty() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

#[test]
fn freeform_draft_is_not_submitted_without_enter() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Notes")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay
        .composer
        .set_text_content("Draft text".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

#[test]
fn freeform_commit_resets_when_draft_changes() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "Notes"),
                question_without_options("q2", "More"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay
        .composer
        .set_text_content("Committed".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_eq!(overlay.answers[0].answer_committed, true);
    let _ = rx.try_recv();

    overlay.move_question(/*next*/ false);
    overlay
        .composer
        .set_text_content("Edited".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    overlay.move_question(/*next*/ true);
    assert_eq!(overlay.answers[0].answer_committed, false);

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(answer.answers, Vec::<String>::new());
}

#[test]
fn notes_are_captured_for_selected_option() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(1);
    }
    overlay.select_current_option(/*committed*/ false);
    overlay
        .composer
        .set_text_content("Notes for option 2".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    let draft = overlay.capture_composer_draft();
    if let Some(answer) = overlay.current_answer_mut() {
        answer.draft = draft;
        answer.answer_committed = true;
    }

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(
        answer.answers,
        vec![
            "Option 2".to_string(),
            "user_note: Notes for option 2".to_string(),
        ]
    );
}

#[test]
fn notes_submission_commits_selected_option() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.handle_key_event(KeyEvent::from(KeyCode::Down));
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
    overlay
        .composer
        .set_text_content("Notes".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_eq!(overlay.current_index(), 1);
    let answer = overlay.answers.first().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(1));
    assert!(answer.answer_committed);
}

#[test]
fn tab_and_enter_open_notes_for_other_option() {
    for key in [KeyCode::Tab, KeyCode::Enter] {
        let (tx, mut rx) = test_sender();
        let mut overlay = RequestUserInputOverlay::new(
            request_event(
                "turn-1",
                vec![question_with_options_and_other("q1", "Pick one")],
            ),
            tx,
            /*has_input_focus*/ true,
            /*enhanced_keys_supported*/ false,
            /*disable_paste_burst*/ false,
        );
        let other_idx = overlay.options_len().saturating_sub(1);
        overlay
            .current_answer_mut()
            .expect("answer missing")
            .options_state
            .selected_idx = Some(other_idx);

        overlay.handle_key_event(KeyEvent::from(key));

        assert!(matches!(overlay.focus, Focus::Notes));
        assert!(overlay.notes_ui_visible());
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn is_other_adds_none_of_the_above_and_submits_it() {
    let (tx, mut rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_options_and_other("q1", "Pick one")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    let rows = overlay.option_rows();
    let other_row = rows.last().expect("expected none-of-the-above row");
    assert_eq!(other_row.name, "  4. None of the above");
    assert_eq!(
        other_row.description.as_deref(),
        Some(OTHER_OPTION_DESCRIPTION)
    );

    let other_idx = overlay.options_len().saturating_sub(1);
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(other_idx);
    }
    overlay
        .composer
        .set_text_content("Custom answer".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();
    let draft = overlay.capture_composer_draft();
    if let Some(answer) = overlay.current_answer_mut() {
        answer.draft = draft;
        answer.answer_committed = true;
    }

    overlay.submit_answers();

    let event = rx.try_recv().expect("expected AppEvent");
    let (_, _, response) = expect_user_input_answer(event);
    let answer = response.answers.get("q1").expect("answer missing");
    assert_eq!(
        answer.answers,
        vec![
            OTHER_OPTION_LABEL.to_string(),
            "user_note: Custom answer".to_string(),
        ]
    );
}

#[test]
fn large_paste_is_preserved_when_switching_questions() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_without_options("q1", "First"),
                question_without_options("q2", "Second"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    let large = "x".repeat(1_500);
    overlay.composer.handle_paste(large.clone());
    overlay.move_question(/*next*/ true);

    let draft = &overlay.answers[0].draft;
    assert_eq!(draft.pending_pastes.len(), 1);
    assert_eq!(draft.pending_pastes[0].1, large);
    assert!(draft.text.contains(&draft.pending_pastes[0].0));
    assert_eq!(draft.text_with_pending(), large);
}

#[test]
fn pending_paste_placeholder_survives_submission_and_back_navigation() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "First"),
                question_with_options("q2", "Second"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    let large = "x".repeat(1_200);
    overlay.focus = Focus::Notes;
    overlay.ensure_selected_for_notes();
    overlay.composer.handle_paste(large.clone());

    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    overlay.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));

    let draft = &overlay.answers[0].draft;
    assert_eq!(draft.pending_pastes.len(), 1);
    assert!(draft.text.contains(&draft.pending_pastes[0].0));
    assert_eq!(draft.text_with_pending(), large);
}

#[test]
fn request_user_input_options_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_options_and_other("q1", "Area")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 120, 16);
    insta::assert_snapshot!(
        "request_user_input_options",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_options_notes_visible_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Area")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(0);
    }
    overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));

    let area = Rect::new(0, 0, 120, 16);
    insta::assert_snapshot!(
        "request_user_input_options_notes_visible",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_tight_height_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Area")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 120, 10);
    insta::assert_snapshot!(
        "request_user_input_tight_height",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn layout_allocates_all_wrapped_options_when_space_allows() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_wrapped_options("q1", "Next Step")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    let width = 48u16;
    let question_height = overlay.wrapped_question_lines(width).len() as u16;
    let options_height = overlay.options_required_height(width);
    let extras = 1u16 // progress
        .saturating_add(DESIRED_SPACERS_BETWEEN_SECTIONS)
        .saturating_add(overlay.footer_required_height(width));
    let height = question_height
        .saturating_add(options_height)
        .saturating_add(extras);
    let sections = overlay.layout_sections(Rect::new(0, 0, width, height));

    assert_eq!(sections.options_area.height, options_height);
}

#[test]
fn desired_height_keeps_spacers_and_preferred_options_visible() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_wrapped_options("q1", "Next Step")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    let width = 110u16;
    let height = overlay.desired_height(width);
    let content_area = menu_surface_inset(Rect::new(0, 0, width, height));
    let sections = overlay.layout_sections(content_area);
    let preferred = overlay.options_preferred_height(content_area.width);

    assert_eq!(sections.options_area.height, preferred);
    let question_bottom = sections.question_area.y + sections.question_area.height;
    let options_bottom = sections.options_area.y + sections.options_area.height;
    let spacer_after_question = sections.options_area.y.saturating_sub(question_bottom);
    let spacer_after_options = sections.notes_area.y.saturating_sub(options_bottom);
    assert_eq!(spacer_after_question, 1);
    assert_eq!(spacer_after_options, 1);
}

#[test]
fn footer_wraps_tips_without_splitting_individual_tips() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(0);

    let width = 36u16;
    let lines = overlay.footer_tip_lines(width);
    assert!(lines.len() > 1);
    let separator_width = UnicodeWidthStr::width(TIP_SEPARATOR);
    for tips in lines {
        let used = tips.iter().enumerate().fold(0usize, |acc, (idx, tip)| {
            let tip_width = tip.width().min(width as usize);
            let extra = if idx == 0 {
                tip_width
            } else {
                separator_width.saturating_add(tip_width)
            };
            acc.saturating_add(extra)
        });
        assert!(used <= width as usize);
    }
}

#[test]
fn request_user_input_wrapped_options_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_wrapped_options("q1", "Next Step")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(0);
    }

    let width = 110u16;
    let question_height = overlay.wrapped_question_lines(width).len() as u16;
    let options_height = overlay.options_required_height(width);
    let height = 1u16
        .saturating_add(question_height)
        .saturating_add(options_height)
        .saturating_add(8);
    let area = Rect::new(0, 0, width, height);
    insta::assert_snapshot!(
        "request_user_input_wrapped_options",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_long_option_text_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_very_long_option_text("q1", "Status")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 120, 18);
    insta::assert_snapshot!(
        "request_user_input_long_option_text",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn selected_long_wrapped_option_stays_visible() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![question_with_long_scroll_options("q1", "Scroll")],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(2);

    let rendered = render_snapshot(&overlay, Rect::new(0, 0, 80, 20));
    assert!(
        rendered.contains("› 3. Use Detailed Hint C"),
        "expected selected option to be visible in viewport\n{rendered}"
    );
}

#[test]
fn request_user_input_footer_wrap_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Pick one"),
                question_with_options("q2", "Pick two"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let answer = overlay.current_answer_mut().expect("answer missing");
    answer.options_state.selected_idx = Some(1);

    let width = 52u16;
    let height = overlay.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    insta::assert_snapshot!(
        "request_user_input_footer_wrap",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_scroll_options_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![ToolRequestUserInputQuestion {
                id: "q1".to_string(),
                header: "Next Step".to_string(),
                question: "What would you like to do next?".to_string(),
                is_other: false,
                is_secret: false,
                options: Some(vec![
                    ToolRequestUserInputOption {
                        label: "Discuss a code change (Recommended)".to_string(),
                        description: "Walk through a plan and edit code together.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Run tests".to_string(),
                        description: "Pick a crate and run its tests.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Review a diff".to_string(),
                        description: "Summarize or review current changes.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Refactor".to_string(),
                        description: "Tighten structure and remove dead code.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Ship it".to_string(),
                        description: "Finalize and open a PR.".to_string(),
                    },
                ]),
            }],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(3);
    }
    let area = Rect::new(0, 0, 120, 12);
    insta::assert_snapshot!(
        "request_user_input_scrolling_options",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_hidden_options_footer_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![ToolRequestUserInputQuestion {
                id: "q1".to_string(),
                header: "Next Step".to_string(),
                question: "What would you like to do next?".to_string(),
                is_other: false,
                is_secret: false,
                options: Some(vec![
                    ToolRequestUserInputOption {
                        label: "Discuss a code change (Recommended)".to_string(),
                        description: "Walk through a plan and edit code together.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Run tests".to_string(),
                        description: "Pick a crate and run its tests.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Review a diff".to_string(),
                        description: "Summarize or review current changes.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Refactor".to_string(),
                        description: "Tighten structure and remove dead code.".to_string(),
                    },
                    ToolRequestUserInputOption {
                        label: "Ship it".to_string(),
                        description: "Finalize and open a PR.".to_string(),
                    },
                ]),
            }],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    {
        let answer = overlay.current_answer_mut().expect("answer missing");
        answer.options_state.selected_idx = Some(3);
    }
    let area = Rect::new(0, 0, 80, 10);
    insta::assert_snapshot!(
        "request_user_input_hidden_options_footer",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn wrapped_question_preserves_the_complete_url_destination() {
    let url = "https://github.com/openai/codex/pull/12345?diff=split";
    let (tx, _rx) = test_sender();
    let mut question = question_without_options("q1", "Release");
    question.question = format!("Review the release\n\nReview {url} before choosing.");
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 40, 18);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    let linked = buf
        .content
        .iter()
        .filter(|cell| cell.symbol().contains("\x1b]8;;"))
        .map(|cell| {
            assert!(cell.symbol().starts_with(&format!("\x1b]8;;{url}\x07")));
            crate::terminal_hyperlinks::strip_osc8(cell.symbol())
        })
        .collect::<String>();
    assert_eq!(linked, url);
    let visible = buf
        .content
        .chunks(40)
        .map(|row| {
            row.iter()
                .map(|cell| crate::terminal_hyperlinks::strip_osc8(cell.symbol()))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("request_user_input_wrapped_url", visible);
}

#[test]
fn request_user_input_freeform_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_without_options("q1", "Goal")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 120, 10);
    insta::assert_snapshot!(
        "request_user_input_freeform",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_freeform_chord_only_submit_snapshot() {
    let (tx, _rx) = test_sender();
    let mut config = TuiKeymap::default();
    config.composer.submit = Some(KeybindingsSpec::One(KeybindingSpec(
        "ctrl-x enter".to_string(),
    )));
    let keymap = RuntimeKeymap::from_config(&config).expect("valid submit chord");
    let overlay = RequestUserInputOverlay::new_with_keymap(
        request_event("turn-1", vec![question_without_options("q1", "Goal")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 120, /*height*/ 10,
    );
    let snapshot = render_snapshot(&overlay, area)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");

    insta::assert_snapshot!(snapshot, @"

        Question 1/1 (1 unanswered)
        Share details.

        › Type your answer (optional)



        ⌃x enter to submit answer | esc to interrupt
        ");
}

#[test]
fn request_user_input_freeform_remapped_submit_snapshot() {
    let (tx, _rx) = test_sender();
    let mut keymap = RuntimeKeymap::defaults();
    keymap.composer.submit = vec![crate::key_hint::ctrl(KeyCode::Char('j'))];
    let overlay = RequestUserInputOverlay::new_with_keymap(
        request_event("turn-1", vec![question_without_options("q1", "Goal")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );
    let area = Rect::new(0, 0, 120, 10);
    insta::assert_snapshot!(
        "request_user_input_freeform_remapped_submit",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_freeform_remapped_interrupt_snapshot() {
    let (tx, _rx) = test_sender();
    let mut keymap = RuntimeKeymap::defaults();
    keymap.chat.interrupt_turn = vec![crate::key_hint::plain(KeyCode::F(12))];
    let overlay = RequestUserInputOverlay::new_with_keymap(
        request_event("turn-1", vec![question_without_options("q1", "Goal")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
        keymap,
    );
    let area = Rect::new(0, 0, 120, 10);
    insta::assert_snapshot!(
        "request_user_input_freeform_remapped_interrupt",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_multi_question_first_snapshot() {
    let (tx, _rx) = test_sender();
    let overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    let area = Rect::new(0, 0, 120, 15);
    insta::assert_snapshot!(
        "request_user_input_multi_question_first",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_multi_question_last_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.move_question(/*next*/ true);
    let area = Rect::new(0, 0, 120, 12);
    insta::assert_snapshot!(
        "request_user_input_multi_question_last",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn request_user_input_unanswered_confirmation_snapshot() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event(
            "turn-1",
            vec![
                question_with_options("q1", "Area"),
                question_without_options("q2", "Goal"),
            ],
        ),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );

    overlay.open_unanswered_confirmation();

    let area = Rect::new(0, 0, 80, 12);
    insta::assert_snapshot!(
        "request_user_input_unanswered_confirmation",
        render_snapshot(&overlay, area)
    );
}

#[test]
fn options_scroll_while_editing_notes() {
    let (tx, _rx) = test_sender();
    let mut overlay = RequestUserInputOverlay::new(
        request_event("turn-1", vec![question_with_options("q1", "Pick one")]),
        tx,
        /*has_input_focus*/ true,
        /*enhanced_keys_supported*/ false,
        /*disable_paste_burst*/ false,
    );
    overlay.select_current_option(/*committed*/ false);
    overlay.focus = Focus::Notes;
    overlay
        .composer
        .set_text_content("Notes".to_string(), Vec::new(), Vec::new());
    overlay.composer.move_cursor_to_end();

    overlay.handle_key_event(KeyEvent::from(KeyCode::Down));

    let answer = overlay.current_answer().expect("answer missing");
    assert_eq!(answer.options_state.selected_idx, Some(1));
    assert!(!answer.answer_committed);
}
