use super::*;
use crate::terminal_hyperlinks::visible_lines;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

fn test_cwd() -> PathBuf {
    // These tests only need a stable absolute cwd; using temp_dir() avoids baking Unix- or
    // Windows-specific root semantics into the fixtures.
    std::env::temp_dir()
}

fn stream_controller(width: Option<usize>) -> StreamController {
    StreamController::new(width, &test_cwd(), HistoryRenderMode::Rich)
}

fn plan_stream_controller(width: Option<usize>) -> PlanStreamController {
    PlanStreamController::new(width, &test_cwd(), HistoryRenderMode::Rich)
}

fn lines_to_plain_strings(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

fn hyperlink_lines_to_plain_strings(lines: &[HyperlinkLine]) -> Vec<String> {
    lines_to_plain_strings(&visible_lines(lines.to_vec()))
}

fn collect_streamed_lines(deltas: &[&str], width: Option<usize>) -> Vec<String> {
    let mut ctrl = stream_controller(width);
    let mut lines = Vec::new();
    for d in deltas {
        ctrl.push(d);
        while let (Some(cell), idle) = ctrl.on_commit_tick() {
            lines.extend(cell.transcript_lines(u16::MAX));
            if idle {
                break;
            }
        }
    }
    if let (Some(cell), _source) = ctrl.finalize() {
        lines.extend(cell.transcript_lines(u16::MAX));
    }
    lines_to_plain_strings(&lines)
        .into_iter()
        .map(|s| s.chars().skip(2).collect::<String>())
        .collect()
}

#[test]
fn queued_heading_is_emitted_once_after_incremental_append() {
    let mut ctrl = stream_controller(Some(80));
    assert!(ctrl.push("Paragraph.\n\n# Heading\n\n"));
    ctrl.push("Next paragraph.\n");

    let (cell, _) = ctrl.on_commit_tick_batch(usize::MAX);
    let mut streamed = cell
        .into_iter()
        .flat_map(|cell| cell.transcript_lines(u16::MAX))
        .collect::<Vec<_>>();
    if let (Some(cell), _source) = ctrl.finalize() {
        streamed.extend(cell.transcript_lines(u16::MAX));
    }
    let streamed = lines_to_plain_strings(&streamed);
    assert_eq!(
        streamed
            .iter()
            .filter(|line| line.contains("# Heading"))
            .count(),
        1,
        "expected the streamed heading to be emitted once: {streamed:?}",
    );
}

fn collect_plan_streamed_lines(deltas: &[&str], width: Option<usize>) -> Vec<String> {
    let mut ctrl = plan_stream_controller(width);
    let mut lines = Vec::new();
    for d in deltas {
        ctrl.push(d);
        while let (Some(cell), idle) = ctrl.on_commit_tick() {
            lines.extend(cell.transcript_lines(u16::MAX));
            if idle {
                break;
            }
        }
    }
    if let (Some(cell), _source) = ctrl.finalize() {
        lines.extend(cell.transcript_lines(u16::MAX));
    }
    lines_to_plain_strings(&lines)
}

#[test]
fn controller_set_width_rebuilds_queued_lines() {
    let mut ctrl = stream_controller(Some(120));
    let delta = "This is a long line that should wrap into multiple rows when resized.\n";
    assert!(ctrl.push(delta));
    assert_eq!(ctrl.queued_lines(), 1);

    ctrl.set_width(Some(24));
    let (cell, idle) = ctrl.on_commit_tick_batch(usize::MAX);
    let rendered = lines_to_plain_strings(
        &cell
            .expect("expected resized queued lines")
            .transcript_lines(u16::MAX),
    );

    assert!(idle);
    assert!(
        rendered.len() > 1,
        "expected resized content to occupy multiple lines, got {rendered:?}",
    );
}

#[test]
fn controller_set_width_no_duplicate_after_emit() {
    let mut ctrl = stream_controller(Some(120));
    let line =
        "This is a long line that definitely wraps when the terminal shrinks to 24 columns.\n";
    ctrl.push(line);
    let (cell, _) = ctrl.on_commit_tick_batch(usize::MAX);
    assert!(cell.is_some(), "expected emitted cell");
    assert_eq!(ctrl.queued_lines(), 0);

    ctrl.set_width(Some(24));

    assert_eq!(
        ctrl.queued_lines(),
        0,
        "already-emitted content must not be re-queued after resize",
    );
}

#[test]
fn controller_tick_batch_zero_is_noop() {
    let mut ctrl = stream_controller(Some(80));
    assert!(ctrl.push("line one\n"));
    assert_eq!(ctrl.queued_lines(), 1);

    let (cell, idle) = ctrl.on_commit_tick_batch(/*max_lines*/ 0);
    assert!(cell.is_none(), "batch size 0 should not emit lines");
    assert!(!idle, "batch size 0 should not drain queued lines");
    assert_eq!(
        ctrl.queued_lines(),
        1,
        "queue depth should remain unchanged"
    );
}

#[test]
fn controller_has_live_tail_reflects_tail_presence() {
    let mut ctrl = stream_controller(Some(80));
    assert!(!ctrl.has_live_tail());

    ctrl.core.render.lines = vec![Line::from("tail line").into()];
    ctrl.core.enqueued_stable_len = 0;
    assert!(ctrl.has_live_tail());

    ctrl.core.enqueued_stable_len = 1;
    assert!(!ctrl.has_live_tail());
}

#[test]
fn plan_controller_has_live_tail_reflects_tail_presence() {
    let mut ctrl = plan_stream_controller(Some(80));
    assert!(!ctrl.has_live_tail());

    ctrl.core.render.lines = vec![Line::from("tail line").into()];
    ctrl.core.enqueued_stable_len = 0;
    assert!(ctrl.has_live_tail());

    ctrl.core.enqueued_stable_len = 1;
    assert!(!ctrl.has_live_tail());
}

#[test]
fn controller_live_tail_keeps_uncommitted_table_cell_newline_gated() {
    let mut ctrl = stream_controller(Some(80));
    ctrl.push("| A | B |\n");
    ctrl.push("| --- | --- |\n");
    ctrl.push("| partial");

    let tail = hyperlink_lines_to_plain_strings(&ctrl.current_tail_lines()).join("\n");
    assert!(
        !tail.contains("partial"),
        "expected live tail to remain newline-gated: {tail:?}",
    );
}

#[test]
fn controller_live_tail_rerenders_table_tail_after_resize() {
    let mut ctrl = stream_controller(Some(96));
    ctrl.push("| # | Feature | Details | Link |\n");
    ctrl.push("| --- | --- | --- | --- |\n");
    ctrl.push(
            "| 1 | RESIZE_REPRO_SENTINEL | long wrapped content that should be reflowed | https://example.com/resize |\n",
        );

    for width in [48, 104, 56] {
        ctrl.set_width(Some(width));
        let tail = hyperlink_lines_to_plain_strings(&ctrl.current_tail_lines());

        let mut expected = Vec::new();
        crate::markdown::append_markdown_agent(
            ctrl.core.state.collector.committed_source(),
            Some(width),
            &mut expected,
        );
        let expected = lines_to_plain_strings(&expected);

        assert_eq!(
            tail, expected,
            "expected live table tail to be rerendered at width {width}",
        );
    }
}

#[test]
fn controller_set_width_partial_drain_no_lost_lines() {
    let mut ctrl = stream_controller(Some(40));
    ctrl.push("AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH IIII JJJJ\n");
    ctrl.push("second line\n");

    let (cell, idle) = ctrl.on_commit_tick();
    assert!(cell.is_some(), "expected 1 emitted line");
    assert!(!idle, "queue should still have lines");
    let remaining_before = ctrl.queued_lines();
    assert!(remaining_before > 0, "should have queued lines left");

    ctrl.set_width(Some(20));

    let (cell, source) = ctrl.finalize();
    let final_lines = cell
        .map(|c| lines_to_plain_strings(&c.transcript_lines(u16::MAX)))
        .unwrap_or_default();

    assert!(
        final_lines.iter().any(|l| l.contains("second line")),
        "un-emitted 'second line' was lost after resize; got: {final_lines:?}",
    );
    assert!(source.is_some(), "expected source from finalize");
}

#[test]
fn controller_set_width_partial_drain_keeps_pending_queue() {
    let mut ctrl = stream_controller(Some(40));
    ctrl.push("AAAA BBBB CCCC DDDD EEEE FFFF GGGG HHHH IIII JJJJ\n");
    ctrl.push("second line\n");

    let (cell, idle) = ctrl.on_commit_tick();
    assert!(cell.is_some(), "expected 1 emitted line");
    assert!(!idle, "queue should still have lines");
    assert!(ctrl.queued_lines() > 0, "expected pending queued lines");

    ctrl.set_width(Some(20));

    assert!(
        ctrl.queued_lines() > 0,
        "resize must preserve pending queued lines"
    );

    let mut drained = Vec::new();
    for _ in 0..64 {
        let (cell, is_idle) = ctrl.on_commit_tick();
        if let Some(cell) = cell {
            drained.extend(lines_to_plain_strings(&cell.transcript_lines(u16::MAX)));
        }
        if is_idle {
            break;
        }
    }

    assert!(
        drained.iter().any(|l| l.contains("second line")),
        "pending lines should continue draining after resize; got {drained:?}",
    );
}

#[test]
fn controller_set_width_preserves_in_flight_tail() {
    let mut ctrl = stream_controller(Some(80));
    ctrl.push("tail without newline");
    ctrl.set_width(Some(24));

    let (cell, _source) = ctrl.finalize();
    let rendered = lines_to_plain_strings(
        &cell
            .expect("expected finalized tail")
            .transcript_lines(u16::MAX),
    );

    assert_eq!(rendered, vec!["• tail without newline".to_string()]);
}

#[test]
fn controller_set_width_preserves_table_tail_when_queue_is_empty() {
    let mut ctrl = stream_controller(Some(80));
    ctrl.push("intro line\n");

    let (_cell, idle) = ctrl.on_commit_tick();
    assert!(idle, "intro line should fully drain");
    assert_eq!(ctrl.queued_lines(), 0, "expected empty queue before table");

    ctrl.push("| A | B |\n");
    assert_eq!(
        ctrl.queued_lines(),
        0,
        "pending table header should remain mutable tail, not queued",
    );
    assert!(ctrl.has_live_tail(), "expected live tail before resize");

    ctrl.set_width(Some(24));

    let tail_after = hyperlink_lines_to_plain_strings(&ctrl.current_tail_lines());
    assert!(
        !tail_after.is_empty(),
        "resize must keep mutable tail when queue is empty",
    );
    let joined = tail_after.join(" ");
    assert!(
        joined.contains('A') && joined.contains('B'),
        "expected table header content to remain in tail after resize: {tail_after:?}",
    );
}

#[test]
fn plan_controller_set_width_preserves_in_flight_tail() {
    let mut ctrl = plan_stream_controller(Some(80));
    ctrl.push("1. Item without newline");
    ctrl.set_width(Some(24));

    let rendered = lines_to_plain_strings(
        &(ctrl
            .finalize()
            .0
            .expect("expected finalized tail")
            .transcript_lines(u16::MAX)),
    );

    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Item without newline")),
        "expected finalized plan content after resize, got {rendered:?}",
    );
}

#[test]
fn plan_controller_holds_table_header_as_live_tail() {
    let mut ctrl = plan_stream_controller(Some(80));
    assert!(ctrl.push("Intro\n"));
    let (_cell, idle) = ctrl.on_commit_tick_batch(usize::MAX);
    assert!(idle, "intro line should fully drain");

    assert!(!ctrl.push("| Step | Owner |\n"));
    assert!(
        ctrl.has_live_tail(),
        "expected plan table header to be held"
    );
}

#[test]
fn controller_loose_vs_tight_with_commit_ticks_matches_full() {
    let mut ctrl = stream_controller(/*width*/ None);
    let mut lines = Vec::new();

    let deltas = vec![
        "\n\n",
        "Loose",
        " vs",
        ".",
        " tight",
        " list",
        " items",
        ":\n",
        "1",
        ".",
        " Tight",
        " item",
        "\n",
        "2",
        ".",
        " Another",
        " tight",
        " item",
        "\n\n",
        "1",
        ".",
        " Loose",
        " item",
        " with",
        " its",
        " own",
        " paragraph",
        ".\n\n",
        "  ",
        " This",
        " paragraph",
        " belongs",
        " to",
        " the",
        " same",
        " list",
        " item",
        ".\n\n",
        "2",
        ".",
        " Second",
        " loose",
        " item",
        " with",
        " a",
        " nested",
        " list",
        " after",
        " a",
        " blank",
        " line",
        ".\n\n",
        "  ",
        " -",
        " Nested",
        " bullet",
        " under",
        " a",
        " loose",
        " item",
        "\n",
        "  ",
        " -",
        " Another",
        " nested",
        " bullet",
        "\n\n",
    ];

    for d in deltas.iter() {
        ctrl.push(d);
        while let (Some(cell), idle) = ctrl.on_commit_tick() {
            lines.extend(cell.transcript_lines(u16::MAX));
            if idle {
                break;
            }
        }
    }
    if let (Some(cell), _source) = ctrl.finalize() {
        lines.extend(cell.transcript_lines(u16::MAX));
    }

    let streamed: Vec<_> = lines_to_plain_strings(&lines)
        .into_iter()
        .map(|s| s.chars().skip(2).collect::<String>())
        .collect();

    let source: String = deltas.iter().copied().collect();
    let mut rendered: Vec<ratatui::text::Line<'static>> = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ None, &mut rendered);
    let rendered_strs = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, rendered_strs);

    let expected = vec![
        "Loose vs. tight list items:".to_string(),
        "".to_string(),
        "1. Tight item".to_string(),
        "2. Another tight item".to_string(),
        "3. Loose item with its own paragraph.".to_string(),
        "".to_string(),
        "   This paragraph belongs to the same list item.".to_string(),
        "".to_string(),
        "4. Second loose item with a nested list after a blank line.".to_string(),
        "    • Nested bullet under a loose item".to_string(),
        "    • Another nested bullet".to_string(),
    ];
    assert_eq!(
        streamed, expected,
        "expected exact rendered lines for loose/tight section"
    );
}

#[test]
fn controller_streamed_table_matches_full_render_widths() {
    let deltas = vec![
        "| Key | Description |\n",
        "| --- | --- |\n",
        "| -v | Enable very verbose logging output for debugging |\n",
        "\n",
    ];

    let streamed = collect_streamed_lines(&deltas, Some(80));

    let source: String = deltas.iter().copied().collect();
    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ Some(80), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
}

#[test]
fn controller_holds_blockquoted_table_tail_until_stable() {
    let deltas = vec![
        "> | A | B |\n",
        "> | --- | --- |\n",
        "> | longvalue | ok |\n",
        "\n",
    ];

    let streamed = collect_streamed_lines(&deltas, Some(80));

    let source: String = deltas.iter().copied().collect();
    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ Some(80), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
}

#[test]
fn controller_keeps_pre_table_lines_queued_when_table_is_confirmed() {
    let mut ctrl = stream_controller(Some(80));

    ctrl.push("Intro line before table.\n");
    assert_eq!(ctrl.queued_lines(), 1);

    ctrl.push("| Key | Value |\n");
    ctrl.push("| --- | --- |\n");
    assert_eq!(
        ctrl.queued_lines(),
        1,
        "pre-table line should remain queued after table confirmation",
    );

    let (cell, idle) = ctrl.on_commit_tick();
    let committed = cell
        .map(|cell| lines_to_plain_strings(&cell.transcript_lines(u16::MAX)))
        .unwrap_or_default();
    assert!(
        committed
            .iter()
            .any(|line| line.contains("Intro line before table.")),
        "expected pre-table line to commit independently: {committed:?}",
    );
    assert!(idle, "only pre-table content should have been queued");

    ctrl.push(
        "| first | row |\n\nAfter table.\n\n| Next | Table |\n| --- | --- |\n| second | row |\n",
    );
    let (cell, idle) = ctrl.on_commit_tick_batch(usize::MAX);
    let committed = cell.expect("completed table and prose should enter scrollback");
    assert!(idle);
    assert!(ctrl.has_live_tail(), "the later table must stay mutable");
    insta::assert_debug_snapshot!(
        "completed_table_scrollback",
        lines_to_plain_strings(&committed.transcript_lines(u16::MAX)),
    );

    let context = InlineVisualizationContext::new(&test_cwd(), codex_protocol::ThreadId::new())
        .expect("visualization context");
    for (context, definition) in [
        (None, "[ref]: https://example.com\n"),
        (Some(context), "[ref]: https://example.com\n"),
        (None, "[ref]: https://example.com"),
    ] {
        let mut ctrl = StreamController::new_with_inline_visualizations(
            Some(80),
            &test_cwd(),
            HistoryRenderMode::Rich,
            context,
        );
        let mut committed = Vec::new();
        for delta in [
            "| Key | Value |\n| --- | --- |\n| [A][ref] | row |\n",
            "\nAfter table.\n\n",
            definition,
        ] {
            ctrl.push(delta);
            if let (Some(cell), _) = ctrl.on_commit_tick_batch(usize::MAX) {
                committed.extend(lines_to_plain_strings(&cell.transcript_lines(u16::MAX)));
            }
        }
        assert!(committed.iter().any(|line| line.contains("[A][ref]")));
        assert!(
            !ctrl.has_live_tail(),
            "the completed table should be released"
        );
        assert!(
            ctrl.needs_scrollback_reflow(),
            "late definitions must repair scrollback"
        );
        let (_, source) = ctrl.finalize();
        let rendered = render_source(
            source.as_deref().unwrap(),
            Some(80),
            &test_cwd(),
            HistoryRenderMode::Rich,
            /*inline_visualization_context*/ None,
        );
        insta::assert_debug_snapshot!(
            "completed_table_reference",
            lines_to_plain_strings(&visible_lines(rendered))
        );
    }
}

#[test]
fn controller_set_width_during_confirmed_table_stream_matches_finalize_render() {
    let mut ctrl = stream_controller(Some(120));
    let deltas = [
        "| Key | Description |\n",
        "| --- | --- |\n",
        "| one | value that should wrap after resize |\n",
    ];
    for delta in deltas {
        ctrl.push(delta);
    }
    assert_eq!(
        ctrl.queued_lines(),
        0,
        "confirmed table should remain mutable"
    );

    ctrl.set_width(Some(32));

    let (cell, source) = ctrl.finalize();
    let source = source.expect("expected finalized source");
    let streamed = lines_to_plain_strings(
        &cell
            .expect("expected finalized table")
            .transcript_lines(u16::MAX),
    )
    .into_iter()
    .map(|line| line.chars().skip(2).collect::<String>())
    .collect::<Vec<_>>();

    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ Some(32), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);
    assert_eq!(streamed, expected);
}

#[test]
fn controller_does_not_hold_back_pipe_prose_without_table_delimiter() {
    let mut ctrl = stream_controller(Some(80));

    ctrl.push("status | owner | note\n");
    let (_first_commit, first_idle) = ctrl.on_commit_tick();
    assert!(first_idle);

    ctrl.push("next line\n");
    let (second_commit, _second_idle) = ctrl.on_commit_tick();
    assert!(
        second_commit.is_some(),
        "expected prose lines to be released once no table delimiter follows"
    );
}

#[test]
fn controller_does_not_stall_repeated_pipe_prose_paragraphs() {
    let mut ctrl = stream_controller(Some(80));

    ctrl.push("alpha | beta\n\n");
    let (_first_commit, first_idle) = ctrl.on_commit_tick();
    assert!(first_idle);

    ctrl.push("gamma | delta\n\n");
    let (second_commit, _second_idle) = ctrl.on_commit_tick();
    let second_lines = second_commit
        .map(|cell| lines_to_plain_strings(&cell.transcript_lines(u16::MAX)))
        .unwrap_or_default();

    assert!(
        second_lines
            .iter()
            .any(|line| line.contains("alpha | beta")),
        "expected the first pipe-prose paragraph to stream before finalize; got {second_lines:?}",
    );
}

#[test]
fn controller_handles_table_immediately_after_heading() {
    let deltas = vec![
        "### 1) Basic table\n",
        "| Name | Role | Status |\n",
        "|---|---|---|\n",
        "| Alice | Admin | Active |\n",
        "| Bob | Editor | Pending |\n",
        "\n",
    ];

    let streamed = collect_streamed_lines(&deltas, Some(100));

    let source: String = deltas.iter().copied().collect();
    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ Some(100), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
}

#[test]
fn controller_renders_separators_for_multi_table_response_shape() {
    let source = "Absolutely. Here are several different Markdown table patterns you can use for rendering tests.\n\n| Name  | Role      |
  Location |\n|-------|-----------|----------|\n| Ava   | Engineer  | NYC      |\n| Malik | Designer  | Berlin   |\n| Priya | PM        | Remote
  |\n\n| Item        | Qty | Price | In Stock |\n|:------------|----:|------:|:--------:|\n| Keyboard    |   2 | 49.99 |    Yes   |\n| Mouse       |  10
   | 19.50 |    Yes   |\n| Monitor     |   1 | 219.0 |    No    |\n\n| Field         | Example                         | Notes
  |\n|---------------|----------------------------------|--------------------------|\n| Escaped pipe  | `foo \\| bar`                    | Should stay
  in one cell  |\n| Inline code   | `let x = value;`                | Monospace inline content |\n| Link          | [OpenAI](https://openai.com)    |
  Standard markdown link   |\n";

    let chunked = source
        .split_inclusive('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let deltas = chunked.iter().map(String::as_str).collect::<Vec<_>>();
    let streamed = collect_streamed_lines(&deltas, Some(120));
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separator in streamed output: {streamed:?}"
    );
}

#[test]
fn controller_renders_separators_for_no_outer_pipes_table_shape() {
    let source = "### 1) Basic\n\n| Name | Role | Active |\n|---|---|---|\n| Alice | Engineer | Yes |\n| Bob | Designer | No |\n\n### 2) No outer
  pipes\n\nCol A | Col B | Col C\n--- | --- | ---\nx | y | z\n10 | 20 | 30\n\n### 3) Another table\n\n| Key | Value |\n|---|---|\n| a | b |\n";

    let chunked = source
        .split_inclusive('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let deltas = chunked.iter().map(String::as_str).collect::<Vec<_>>();
    let streamed = collect_streamed_lines(&deltas, Some(100));

    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(source, /*width*/ Some(100), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
    let has_raw_no_outer_header = streamed
        .iter()
        .any(|line| line.trim() == "Col A | Col B | Col C");
    assert!(
        !has_raw_no_outer_header,
        "no-outer-pipes header should not remain raw in final streamed output: {streamed:?}"
    );
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separator in final streamed output: {streamed:?}"
    );
}

#[test]
fn controller_stabilizes_first_no_outer_pipes_table_in_response() {
    let deltas = vec![
        "### No outer pipes first\n\n",
        "Col A | Col B | Col C\n",
        "--- | --- | ---\n",
        "x | y | z\n",
        "10 | 20 | 30\n",
        "\n",
        "After table paragraph.\n",
    ];
    let streamed = collect_streamed_lines(&deltas, Some(100));

    let source: String = deltas.iter().copied().collect();
    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ Some(100), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separator for no-outer-pipes streaming: {streamed:?}"
    );
    assert!(
        !streamed
            .iter()
            .any(|line| line.trim() == "Col A | Col B | Col C"),
        "did not expect raw no-outer-pipes header in final streamed output: {streamed:?}"
    );
}

#[test]
fn controller_stabilizes_two_column_no_outer_table_in_response() {
    let deltas = vec![
        "A | B\n",
        "--- | ---\n",
        "left | right\n",
        "\n",
        "After table paragraph.\n",
    ];
    let streamed = collect_streamed_lines(&deltas, Some(80));

    let source: String = deltas.iter().copied().collect();
    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(&source, /*width*/ Some(80), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separator for two-column no-outer table: {streamed:?}"
    );
    assert!(
        !streamed.iter().any(|line| line.trim() == "A | B"),
        "did not expect raw two-column no-outer header in final streamed output: {streamed:?}"
    );
}

#[test]
fn controller_converts_no_outer_table_between_preboxed_sections() {
    let source = "  ┌───────┬──────────┬────────┐\n  │ Name  │ Role     │ Active │\n  ├───────┼──────────┼────────┤\n  │ Alice │ Engineer │ Yes    │\n  │ Bob   │ Designer │ No     │\n  │ Cara  │ PM       │ Yes    │\n  └───────┴──────────┴────────┘\n\n  ### 3) No outer pipes\n\n  Col A | Col B | Col C\n  --- | --- | ---\n  x | y | z\n  10 | 20 | 30\n\n  ┌─────────────────┬────────┬────────────────────────┐\n  │ Example         │ Output │ Notes                  │\n  ├─────────────────┼────────┼────────────────────────┤\n  │ a | b           │ `a     │ b`                     │\n  │ npm run test    │ ok     │ Inline code formatting │\n  │ SELECT * FROM t │ 3 rows │ SQL snippet            │\n  └─────────────────┴────────┴────────────────────────┘\n";

    let deltas = source
        .split_inclusive('\n')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let streamed = collect_streamed_lines(
        &deltas.iter().map(String::as_str).collect::<Vec<_>>(),
        Some(100),
    );

    let has_raw_no_outer_header = streamed
        .iter()
        .any(|line| line.trim() == "Col A | Col B | Col C");
    assert!(
        !has_raw_no_outer_header,
        "no-outer table header remained raw in streamed output: {streamed:?}"
    );
    assert!(
        streamed
            .iter()
            .any(|line| line.contains(" Col A    Col B    Col C")),
        "expected converted no-outer table header in streamed output: {streamed:?}"
    );
}

#[test]
fn controller_keeps_markdown_fenced_tables_mutable_until_finalize() {
    let source = "```md\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
    let deltas = vec![
        "```md\n",
        "| A | B |\n",
        "|---|---|\n",
        "| 1 | 2 |\n",
        "```\n",
    ];
    let streamed = collect_streamed_lines(&deltas, Some(80));

    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(source, /*width*/ Some(80), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separator in streamed output: {streamed:?}"
    );
    assert!(
        !streamed.iter().any(|line| line.trim() == "| A | B |"),
        "did not expect raw table header line after finalize: {streamed:?}"
    );
}

#[test]
fn controller_keeps_markdown_fenced_no_outer_tables_mutable_until_finalize() {
    let source = "```md\nCol A | Col B | Col C\n--- | --- | ---\nx | y | z\n10 | 20 | 30\n```\n";
    let deltas = vec![
        "```md\n",
        "Col A | Col B | Col C\n",
        "--- | --- | ---\n",
        "x | y | z\n",
        "10 | 20 | 30\n",
        "```\n",
    ];
    let streamed = collect_streamed_lines(&deltas, Some(100));

    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(source, /*width*/ Some(100), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separator in streamed output: {streamed:?}"
    );
    assert!(
        !streamed
            .iter()
            .any(|line| line.trim() == "Col A | Col B | Col C"),
        "did not expect raw no-outer-pipes header line after finalize: {streamed:?}"
    );
}

#[test]
fn controller_live_view_matches_render_during_interleaved_table_streaming() {
    let source = "Project updates are easier to scan when narrative and structured data alternate.\n\n| Focus Area | Owner | Priority | Status |\n|---|---|---|---|\n| Authentication cleanup | Maya | High | 80% |\n| CLI error messages | Jordan | Medium | 55% |\n| Docs refresh | Lee | Low | 30% |\n\nThe first checkpoint shows progress, but we still have open risks.\n\n| Task | Command / Artifact | Due | State |\n|---|---|---|---|\n| Run unit tests | `cargo test -p codex-core` | Today | ✅ |\n| Snapshot review | `cargo insta pending-snapshots -p codex-tui` | Today | ⏳ |\n| Changelog draft | Release template (https://replacechangelog.com/) | Tomorrow | 📝 |\n\nFinal sign-off criteria are summarized below.\n";
    let width = Some(72usize);
    let mut ctrl = stream_controller(width);
    let mut emitted_lines: Vec<Line<'static>> = Vec::new();

    for delta in source.split_inclusive('\n') {
        ctrl.push(delta);
        loop {
            let (cell, idle) = ctrl.on_commit_tick();
            if let Some(cell) = cell {
                emitted_lines.extend(cell.transcript_lines(u16::MAX).into_iter().map(|line| {
                    let plain: String = line
                        .spans
                        .iter()
                        .map(|s| s.content.clone())
                        .collect::<Vec<_>>()
                        .join("");
                    Line::from(plain.chars().skip(2).collect::<String>())
                }));
            }
            if idle {
                break;
            }
        }

        let mut visible = emitted_lines.clone();
        visible.extend(visible_lines(ctrl.current_tail_lines()));
        let visible_plain = lines_to_plain_strings(&visible);

        let mut expected = Vec::new();
        crate::markdown::append_markdown_agent(
            ctrl.core.state.collector.committed_source(),
            /*width*/ width,
            &mut expected,
        );
        let expected_plain = lines_to_plain_strings(&expected);

        assert_eq!(
            visible_plain, expected_plain,
            "live view diverged after delta: {delta:?}"
        );
    }
    assert!(
        !ctrl.has_live_tail(),
        "completed tables and prose should enter scrollback before finalization",
    );
}

#[test]
fn finalized_stream_table_preserves_semantic_url_fragments() {
    let destination = "https://example.com/a/very/long/path/to/a/table/artifact";
    let source = format!("| Item | URL |\n| --- | --- |\n| report | {destination} |\n");
    let mut ctrl = stream_controller(/*width*/ Some(32));
    ctrl.push(&source);

    let (cell, _) = ctrl.finalize();
    let lines = cell
        .expect("final stream table cell")
        .display_hyperlink_lines(/*width*/ 32);
    let linked_rows = lines
        .iter()
        .filter(|line| !line.hyperlinks.is_empty())
        .collect::<Vec<_>>();

    assert!(linked_rows.len() > 1);
    assert!(linked_rows.iter().all(|line| {
        line.hyperlinks
            .iter()
            .all(|link| link.destination == destination)
    }));
}

#[test]
fn controller_keeps_non_markdown_fenced_tables_as_code() {
    let source = "```sh\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
    let deltas = vec![
        "```sh\n",
        "| A | B |\n",
        "|---|---|\n",
        "| 1 | 2 |\n",
        "```\n",
    ];
    let streamed = collect_streamed_lines(&deltas, Some(80));

    let mut rendered = Vec::new();
    crate::markdown::append_markdown_agent(source, /*width*/ Some(80), &mut rendered);
    let expected = lines_to_plain_strings(&rendered);

    assert_eq!(streamed, expected);
    assert!(
        streamed.iter().any(|line| line.trim() == "| A | B |"),
        "expected code-fenced pipe line to remain raw: {streamed:?}"
    );
    assert!(
        !streamed
            .iter()
            .any(|line| line.contains('━') || line.contains('─')),
        "did not expect a table separator for non-markdown fence: {streamed:?}"
    );
}

#[test]
fn plan_controller_streamed_table_matches_final_render() {
    let deltas = vec![
        "## Build plan\n\n",
        "| Step | Owner |\n",
        "|---|---|\n",
        "| Write tests | Agent |\n",
        "| Verify output | User |\n",
        "\n",
    ];
    let streamed = collect_plan_streamed_lines(&deltas, Some(80));

    let source: String = deltas.iter().copied().collect();
    let baseline = collect_plan_streamed_lines(&[source.as_str()], Some(80));

    assert_eq!(streamed, baseline);
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separators in plan streamed output: {streamed:?}"
    );
    assert!(
        !streamed
            .iter()
            .any(|line| line.trim() == "| Step | Owner |"),
        "did not expect raw table header line in plan output: {streamed:?}"
    );
}

#[test]
fn finalized_plan_stream_preserves_semantic_url_fragments() {
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (30, 30, 30),
            bg: (255, 255, 255),
        },
        || {
            let destination = "https://example.com/a/very/long/path/to/a/table/artifact";
            let source = format!(
                "# Plan\n\n> Quote\n\n| Step | URL |\n| --- | --- |\n| Verify | {destination} |\n"
            );
            let mut ctrl = PlanStreamController::new(
                /*width*/ Some(32),
                &test_cwd(),
                HistoryRenderMode::Rich,
            );
            ctrl.push(&source);

            let (cell, _) = ctrl.finalize();
            let lines = cell
                .expect("final plan stream table cell")
                .display_hyperlink_lines(/*width*/ 32);
            insta::assert_debug_snapshot!(lines);
            let linked_rows = lines
                .iter()
                .filter(|line| !line.hyperlinks.is_empty())
                .collect::<Vec<_>>();

            assert!(linked_rows.len() > 1);
            assert!(linked_rows.iter().all(|line| {
                line.hyperlinks
                    .iter()
                    .all(|link| link.destination == destination)
            }));
        },
    );
}

#[test]
fn plan_controller_streamed_markdown_fenced_table_matches_final_render() {
    let deltas = vec![
        "## Build plan\n\n",
        "```md\n",
        "| Step | Owner |\n",
        "|---|---|\n",
        "| Write tests | Agent |\n",
        "| Verify output | User |\n",
        "```\n",
        "\n",
    ];
    let streamed = collect_plan_streamed_lines(&deltas, Some(80));

    let source: String = deltas.iter().copied().collect();
    let baseline = collect_plan_streamed_lines(&[source.as_str()], Some(80));

    assert_eq!(streamed, baseline);
    assert!(
        streamed.iter().any(|line| line.contains('━')),
        "expected table separators in fenced plan output: {streamed:?}"
    );
    assert!(
        !streamed
            .iter()
            .any(|line| line.trim() == "| Step | Owner |"),
        "did not expect raw table header line in fenced plan output: {streamed:?}"
    );
}

#[test]
fn table_holdback_state_detects_header_plus_delimiter() {
    let source = "| Key | Description |\n| --- | --- |\n";
    assert!(matches!(
        table_holdback_state(source),
        TableHoldbackState::Confirmed { .. }
    ));
}

#[test]
fn table_holdback_state_detects_single_column_header_plus_delimiter() {
    let source = "| Only |\n| --- |\n";
    assert!(matches!(
        table_holdback_state(source),
        TableHoldbackState::Confirmed { .. }
    ));
}

#[test]
fn table_holdback_state_ignores_table_like_lines_inside_unclosed_long_fence() {
    let source = "````sh\n```cmd\n| Key | Description |\n| --- | --- |\n````\n";
    assert!(
        matches!(table_holdback_state(source), TableHoldbackState::None),
        "table holdback should ignore pipe lines inside an open non-markdown fence",
    );
}

#[test]
fn table_holdback_state_treats_indented_fence_text_as_plain_content() {
    let source = "    ```sh\n| Key | Description |\n| --- | --- |\n";
    assert!(
        matches!(
            table_holdback_state(source),
            TableHoldbackState::Confirmed { .. }
        ),
        "indented fence-like text should not open a fence and should not block table detection",
    );
}

#[test]
fn table_holdback_state_ignores_table_like_lines_inside_blockquoted_other_fence() {
    let source = "> ```sh\n> | Key | Value |\n> | --- | --- |\n> ```\n";
    assert!(
        matches!(table_holdback_state(source), TableHoldbackState::None),
        "table holdback should ignore pipe lines inside non-markdown blockquoted fences",
    );
}

#[test]
fn incremental_holdback_matches_stateless_scan_per_chunk() {
    let chunks = [
        "status | owner\n",
        "\n",
        "> ```sh\n",
        "> | A | B |\n",
        "> | --- | --- |\n",
        "> ```\n",
        "> | Key | Value |\n",
        "> | --- | --- |\n",
    ];

    let mut scanner = TableHoldbackScanner::new();
    let mut source = String::new();
    for chunk in chunks {
        source.push_str(chunk);
        scanner.scan(&source, /*completed_source_len*/ 0);
        assert_eq!(
            scanner.state(),
            table_holdback_state(&source),
            "scanner mismatch after chunk: {chunk:?}\nsource:\n{source}",
        );
    }
}

#[test]
fn incremental_holdback_detects_header_delimiter_across_chunk_boundary() {
    let mut scanner = TableHoldbackScanner::new();
    scanner.scan("| A | B |\n", /*completed_source_len*/ 0);
    assert_eq!(
        scanner.state(),
        TableHoldbackState::PendingHeader { header_start: 0 }
    );
    scanner.scan(
        "| A | B |\n| --- | --- |\n",
        /*completed_source_len*/ 0,
    );
    assert_eq!(
        scanner.state(),
        TableHoldbackState::Confirmed { table_start: 0 }
    );
}

#[test]
fn controller_set_width_after_first_line_emit_does_not_requeue_first_line() {
    let mut ctrl = stream_controller(Some(120));
    ctrl.push("FIRSTTOKEN contains enough words to wrap once the width is reduced dramatically.\n");
    ctrl.push("second line remains pending\n");

    let (first_emit, _) = ctrl.on_commit_tick();
    assert!(first_emit.is_some(), "expected first line emission");

    ctrl.set_width(Some(20));

    let (cell, _source) = ctrl.finalize();
    let remaining = cell
        .map(|cell| lines_to_plain_strings(&cell.transcript_lines(u16::MAX)))
        .unwrap_or_default()
        .into_iter()
        .map(|line| line.chars().skip(2).collect::<String>())
        .collect::<Vec<_>>();
    assert!(
        !remaining.iter().any(|line| line.contains("FIRSTTOKEN")),
        "first line should not be re-queued after resize: {remaining:?}",
    );
    assert!(
        remaining.iter().any(|line| line.contains("second line")),
        "expected pending second line after resize: {remaining:?}",
    );
}

#[test]
fn controller_set_width_partial_wrapped_emit_preserves_remaining_content() {
    let mut ctrl = stream_controller(Some(20));
    ctrl.push("The quick brown fox jumps over the lazy dog near the riverbank.\n");
    ctrl.push("tail line\n");

    let (first_emit, idle) = ctrl.on_commit_tick();
    assert!(first_emit.is_some(), "expected first wrapped line emission");
    assert!(!idle, "expected remaining queued content after one tick");
    assert!(
        ctrl.queued_lines() > 0,
        "expected non-empty queue before resize"
    );

    ctrl.set_width(Some(120));

    let (cell, _source) = ctrl.finalize();
    let remaining = cell
        .map(|c| lines_to_plain_strings(&c.transcript_lines(u16::MAX)))
        .unwrap_or_default()
        .into_iter()
        .map(|line| line.chars().skip(2).collect::<String>())
        .collect::<Vec<_>>();
    assert!(
        remaining.iter().any(|line| line.contains("tail line")),
        "un-emitted content should remain after resize remap: {remaining:?}",
    );
}

#[test]
fn controller_set_width_partial_wrapped_emit_keeps_wrapped_remainder() {
    let mut ctrl = stream_controller(Some(18));
    ctrl.push("alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\n");

    let (first_emit, idle) = ctrl.on_commit_tick();
    assert!(first_emit.is_some(), "expected first wrapped line emission");
    assert!(!idle, "expected remaining wrapped content after one tick");
    assert!(
        ctrl.queued_lines() > 0,
        "expected queued wrapped remainder before resize"
    );

    ctrl.set_width(Some(80));

    let (cell, _source) = ctrl.finalize();
    let remaining = cell
        .map(|c| lines_to_plain_strings(&c.transcript_lines(u16::MAX)))
        .unwrap_or_default();
    let joined = remaining.join(" ");
    assert!(
        joined.contains("kappa") || joined.contains("lambda") || joined.contains("mu"),
        "wrapped remainder from partially emitted source line was lost after resize: {remaining:?}",
    );
}
