use super::*;
use pretty_assertions::assert_eq;
use ratatui::text::Text;

fn lines_to_strings(text: &Text<'_>) -> Vec<String> {
    text.lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect()
}

#[test]
fn wraps_plain_text_when_width_provided() {
    let markdown = "This is a simple sentence that should wrap.";
    let rendered = render_markdown_text_with_width(markdown, Some(16));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "This is a simple".to_string(),
            "sentence that".to_string(),
            "should wrap.".to_string(),
        ]
    );
}

#[test]
fn wraps_list_items_preserving_indent() {
    let markdown = "- first second third fourth";
    let rendered = render_markdown_text_with_width(markdown, Some(14));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec!["• first second".to_string(), "  third fourth".to_string(),]
    );
}

#[test]
fn wraps_nested_lists() {
    let markdown =
        "- outer item with several words to wrap\n  - inner item that also needs wrapping";
    let rendered = render_markdown_text_with_width(markdown, Some(20));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "• outer item with".to_string(),
            "  several words to".to_string(),
            "  wrap".to_string(),
            "    • inner item".to_string(),
            "      that also".to_string(),
            "      needs wrapping".to_string(),
        ]
    );
}

#[test]
fn wraps_ordered_lists() {
    let markdown = "1. ordered item contains many words for wrapping";
    let rendered = render_markdown_text_with_width(markdown, Some(18));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "1. ordered item".to_string(),
            "   contains many".to_string(),
            "   words for".to_string(),
            "   wrapping".to_string(),
        ]
    );
}

#[test]
fn wraps_blockquotes() {
    let markdown = "> block quote with **content** that should wrap nicely";
    let rendered = render_markdown_lines_with_width_and_cwd(markdown, Some(22), /*cwd*/ None);
    let source = rendered[0].source.as_ref().expect("blockquote source");
    let styles = MarkdownStyles::default();
    assert_eq!(
        source.styled_range(0..source.text.len()),
        Line::from(vec![
            Span::styled("block quote with ", styles.blockquote),
            Span::styled("content", styles.blockquote.patch(styles.strong)),
            Span::styled(" that should wrap nicely", styles.blockquote),
        ])
    );
    let lines: Vec<_> = rendered.iter().map(|line| line.line.to_string()).collect();
    assert_eq!(
        lines,
        vec![
            "> block quote with".to_string(),
            "> content that should".to_string(),
            "> wrap nicely".to_string(),
        ]
    );
}

#[test]
fn wraps_blockquotes_inside_lists() {
    let markdown = "- list item\n  > block quote inside list that wraps";
    let rendered = render_markdown_text_with_width(markdown, Some(24));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "• list item".to_string(),
            "  > block quote inside".to_string(),
            "  > list that wraps".to_string(),
        ]
    );
}

#[test]
fn wraps_list_items_containing_blockquotes() {
    let markdown = "1. item with quote\n   > quoted text that should wrap";
    let rendered = render_markdown_text_with_width(markdown, Some(24));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec![
            "1. item with quote".to_string(),
            "   > quoted text that".to_string(),
            "   > should wrap".to_string(),
        ]
    );
}

#[test]
fn does_not_wrap_code_blocks() {
    let markdown = "````\nfn main() { println!(\"hi from a long line\"); }\n````";
    let rendered = render_markdown_text_with_width(markdown, Some(10));
    let lines = lines_to_strings(&rendered);
    assert_eq!(
        lines,
        vec!["fn main() { println!(\"hi from a long line\"); }".to_string(),]
    );
}

#[test]
fn does_not_split_long_url_like_token_without_scheme() {
    let url_like = "example.test/api/v1/projects/alpha-team/releases/2026-02-17/builds/1234567890";
    let rendered = render_markdown_text_with_width(url_like, Some(24));
    let lines = lines_to_strings(&rendered);

    assert_eq!(
        lines.iter().filter(|line| line.contains(url_like)).count(),
        1,
        "expected full URL-like token in one rendered line, got: {lines:?}"
    );
}

#[test]
fn fenced_code_info_string_with_metadata_highlights() {
    // CommonMark info strings like "rust,no_run" or "rust title=demo"
    // contain metadata after the language token.  The language must be
    // extracted (first word / comma-separated token) so highlighting works.
    for info in &["rust,no_run", "rust no_run", "rust title=\"demo\""] {
        let markdown = format!("```{info}\nfn main() {{}}\n```\n");
        let rendered = crate::terminal_palette::with_test_default_colors(
            crate::terminal_probe::DefaultColors {
                fg: (220, 220, 220),
                bg: (20, 20, 20),
            },
            || render_markdown_text(&markdown),
        );
        let has_rgb = rendered.lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
        });
        assert!(
            has_rgb,
            "info string \"{info}\" should still produce syntax highlighting"
        );
    }
}

#[test]
fn crlf_code_block_no_extra_blank_lines() {
    // pulldown-cmark can split CRLF code blocks into multiple Text events.
    // The buffer must concatenate them verbatim — no inserted separators.
    let markdown = "```rust\r\nfn main() {}\r\n    line2\r\n```\r\n";
    let rendered = render_markdown_text(markdown);
    let lines = lines_to_strings(&rendered);
    // Should be exactly two code lines; no spurious blank line between them.
    assert_eq!(
        lines,
        vec!["fn main() {}".to_string(), "    line2".to_string()],
        "CRLF code block should not produce extra blank lines: {lines:?}"
    );
}

#[test]
fn wrap_cell_preserves_hard_break_lines() {
    let mut cell = TableCell::default();
    cell.push_annotated(Line::from("first line").into(), &[]);
    cell.hard_break();
    cell.push_annotated(Line::from("second line").into(), &[]);

    let writer = W::new(
        "",
        /*wrap_width*/ Some(80),
        /*cwd*/ None,
        &never_hide_link_destination,
    );
    let wrapped = writer.wrap_cell(&cell, /*width*/ 40);
    let rendered = wrapped
        .iter()
        .map(|line| {
            line.line
                .spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        rendered,
        vec!["first line".to_string(), "second line".to_string()]
    );
}

#[test]
fn blank_table_continuations_do_not_repeat_the_first_cell() {
    let mut cell = make_cell("first");
    cell.hard_break();
    cell.hard_break();
    cell.push_annotated(Line::from("last").into(), &[]);
    cell.hard_break();
    let writer = W::new(
        "",
        /*wrap_width*/ Some(80),
        /*cwd*/ None,
        &never_hide_link_destination,
    );
    let lines = writer.render_table_row(
        &[make_cell("short"), cell],
        &[5, 5],
        &[Alignment::None, Alignment::None],
        Style::default(),
    );
    let rendered: Vec<_> = lines.iter().map(|line| line.line.to_string()).collect();
    insta::assert_debug_snapshot!(rendered);
}

// ---------------------------------------------------------------
// Type alias for calling private associated functions on Writer.
// ---------------------------------------------------------------
type W<'a> = Writer<'a, 'a>;

/// Build a single-line `TableCell` from plain text.
fn make_cell(text: &str) -> TableCell {
    let mut cell = TableCell::default();
    cell.push_annotated(Line::from(text.to_string()).into(), &[]);
    cell
}

fn make_body_row(cells: Vec<TableCell>, has_table_pipe_syntax: bool) -> TableBodyRow {
    TableBodyRow {
        cells,
        has_table_pipe_syntax,
    }
}

// ===== Column-metrics unit tests =====

#[test]
fn column_classification_narrative_by_word_count() {
    // Col 0: short tokens (1-2 words each) -> Compact
    // Col 1: prose (≥4 words per cell) → Narrative
    let header = vec![make_cell("ID"), make_cell("Description")];
    let rows = vec![
        vec![make_cell("1"), make_cell("a long description of the item")],
        vec![make_cell("2"), make_cell("another verbose body cell here")],
    ];
    let metrics = W::collect_table_column_metrics(&header, &rows, /*column_count*/ 2);
    assert_eq!(metrics[0].kind, TableColumnKind::Compact);
    assert_eq!(metrics[1].kind, TableColumnKind::Narrative);
}

#[test]
fn column_classification_token_heavy_by_url_like_tokens() {
    let header = vec![make_cell("URL")];
    let rows = vec![
        vec![make_cell("https://example.com/very/long/path")],
        vec![make_cell("https://another.example.org/deep")],
    ];
    let metrics = W::collect_table_column_metrics(&header, &rows, /*column_count*/ 1);
    assert_eq!(metrics[0].kind, TableColumnKind::TokenHeavy);
}

#[test]
fn column_classification_token_heavy_for_local_path_lists() {
    let header = vec![make_cell("Files")];
    let rows = vec![
        vec![make_cell(
            "codex-rs/core/src/next_prompt_suggestion.rs:1, codex-rs/core/src/next_prompt_suggestion_tests.rs:1",
        )],
        vec![make_cell(
            "codex-rs/core/src/context/next_prompt_suggestion.rs:1, codex-rs/core/src/context/contextual_user_message_tests.rs:1",
        )],
    ];
    let metrics = W::collect_table_column_metrics(&header, &rows, /*column_count*/ 1);
    assert_eq!(metrics[0].kind, TableColumnKind::TokenHeavy);
}

#[test]
fn column_classification_compact_all_short() {
    // Both columns short tokens -> both Compact
    let header = vec![make_cell("Status"), make_cell("Count")];
    let rows = vec![
        vec![make_cell("ok"), make_cell("42")],
        vec![make_cell("err"), make_cell("7")],
    ];
    let metrics = W::collect_table_column_metrics(&header, &rows, /*column_count*/ 2);
    assert_eq!(metrics[0].kind, TableColumnKind::Compact);
    assert_eq!(metrics[1].kind, TableColumnKind::Compact);
}

#[test]
fn preferred_floor_narrative_retains_readable_width() {
    let m = TableColumnMetrics {
        max_width: 40,
        header_token_width: 15,
        body_token_width: 8,
        kind: TableColumnKind::Narrative,
    };
    assert_eq!(W::preferred_column_floor(&m, /*min_column_width*/ 3), 16);

    let m2 = TableColumnMetrics {
        max_width: 12,
        header_token_width: 6,
        body_token_width: 8,
        kind: TableColumnKind::Narrative,
    };
    assert_eq!(W::preferred_column_floor(&m2, /*min_column_width*/ 3), 12);
}

#[test]
fn preferred_floor_token_heavy_retains_readable_width() {
    let m = TableColumnMetrics {
        max_width: 80,
        header_token_width: 5,
        body_token_width: 60,
        kind: TableColumnKind::TokenHeavy,
    };
    assert_eq!(W::preferred_column_floor(&m, /*min_column_width*/ 3), 16);
}

#[test]
fn preferred_floor_compact_uses_body_token() {
    // Compact: max(header_token_width, body_token_width.min(16))
    let m = TableColumnMetrics {
        max_width: 30,
        header_token_width: 5,
        body_token_width: 12,
        kind: TableColumnKind::Compact,
    };
    // max(5, min(12, 16)) = max(5, 12) = 12
    assert_eq!(W::preferred_column_floor(&m, /*min_column_width*/ 3), 12);

    // Body token exceeds 16 cap → capped at 16, then max with header
    let m2 = TableColumnMetrics {
        max_width: 30,
        header_token_width: 5,
        body_token_width: 20,
        kind: TableColumnKind::Compact,
    };
    // max(5, min(20, 16)) = max(5, 16) = 16
    assert_eq!(W::preferred_column_floor(&m2, /*min_column_width*/ 3), 16);
}

#[test]
fn bulk_column_shrink_matches_one_cell_at_a_time() {
    fn priority(kind: TableColumnKind) -> usize {
        match kind {
            TableColumnKind::TokenHeavy => 0,
            TableColumnKind::Narrative => 1,
            TableColumnKind::Compact => 2,
        }
    }

    fn shrink_one_at_a_time(
        widths: &mut [usize],
        floors: &[usize],
        metrics: &[TableColumnMetrics],
        mut amount: usize,
    ) -> usize {
        while amount > 0 {
            let Some(idx) = widths
                .iter()
                .enumerate()
                .filter(|(idx, width)| **width > floors[*idx])
                .min_by_key(|(idx, width)| {
                    (
                        priority(metrics[*idx].kind),
                        usize::MAX - width.saturating_sub(floors[*idx]),
                    )
                })
                .map(|(idx, _)| idx)
            else {
                break;
            };
            widths[idx] -= 1;
            amount -= 1;
        }
        amount
    }

    for case in 0..64usize {
        let metrics = (0..5)
            .map(|idx| TableColumnMetrics {
                max_width: 100,
                header_token_width: 8,
                body_token_width: 8,
                kind: match (case + idx) % 3 {
                    0 => TableColumnKind::TokenHeavy,
                    1 => TableColumnKind::Narrative,
                    _ => TableColumnKind::Compact,
                },
            })
            .collect::<Vec<_>>();
        let floors = (0..5)
            .map(|idx| 3 + (case * (idx + 1) + idx) % 14)
            .collect::<Vec<_>>();
        let initial = floors
            .iter()
            .enumerate()
            .map(|(idx, floor)| floor + (case * (idx + 3) + idx * 7) % 41)
            .collect::<Vec<_>>();
        let slack = initial
            .iter()
            .zip(&floors)
            .map(|(width, floor)| width - floor)
            .sum::<usize>();

        for amount in 0..=slack + 1 {
            let mut expected = initial.clone();
            let expected_remaining = shrink_one_at_a_time(&mut expected, &floors, &metrics, amount);
            let mut actual = initial.clone();
            let actual_remaining = W::shrink_columns(&mut actual, &floors, &metrics, amount);
            assert_eq!((actual, actual_remaining), (expected, expected_remaining));
        }
    }
}

#[test]
fn column_widths_bulk_shrink_large_token_heavy_column() {
    let metrics = [
        TableColumnMetrics {
            max_width: 1_000_000,
            header_token_width: 4,
            body_token_width: 1_000_000,
            kind: TableColumnKind::TokenHeavy,
        },
        TableColumnMetrics {
            max_width: 8,
            header_token_width: 5,
            body_token_width: 8,
            kind: TableColumnKind::Compact,
        },
    ];

    assert_eq!(
        W::compute_column_widths(&metrics, /*available_width*/ Some(24)),
        Some(vec![16, 8])
    );
}

// ===== Spillover-detection unit tests =====

#[test]
fn spillover_detects_single_cell_row() {
    let row = make_body_row(
        vec![make_cell("some trailing text")],
        /*has_table_pipe_syntax*/ false,
    );
    assert!(W::is_spillover_row(&row, /*next_row*/ None));
}

#[test]
fn spillover_keeps_single_cell_row_with_table_pipe_syntax() {
    let row = make_body_row(
        vec![make_cell("some sparse value")],
        /*has_table_pipe_syntax*/ true,
    );
    assert!(!W::is_spillover_row(&row, /*next_row*/ None));
}

#[test]
fn spillover_detects_html_content() {
    // 3-cell row where only cell 0 has HTML content
    let row = make_body_row(
        vec![
            make_cell("<div>content</div>"),
            make_cell(""),
            make_cell(""),
        ],
        /*has_table_pipe_syntax*/ false,
    );
    assert!(W::is_spillover_row(&row, /*next_row*/ None));
}

#[test]
fn spillover_detects_label_followed_by_html() {
    // cell 0 = "HTML block:" and next_row cell 0 = "<div>x</div>"
    let row = make_body_row(
        vec![make_cell("HTML block:"), make_cell(""), make_cell("")],
        /*has_table_pipe_syntax*/ false,
    );
    let next = make_body_row(
        vec![make_cell("<div>x</div>"), make_cell(""), make_cell("")],
        /*has_table_pipe_syntax*/ false,
    );
    assert!(W::is_spillover_row(&row, Some(&next)));
}

#[test]
fn spillover_detects_trailing_html_label() {
    // "HTML block:" with no next_row → trailing HTML label spillover
    let row = make_body_row(
        vec![make_cell("HTML block:"), make_cell(""), make_cell("")],
        /*has_table_pipe_syntax*/ false,
    );
    assert!(W::is_spillover_row(&row, /*next_row*/ None));
}

#[test]
fn spillover_keeps_normal_multi_cell_row() {
    // 3 cells all non-empty → not spillover
    let row = make_body_row(
        vec![make_cell("one"), make_cell("two"), make_cell("three")],
        /*has_table_pipe_syntax*/ true,
    );
    assert!(!W::is_spillover_row(&row, /*next_row*/ None));
}

#[test]
fn spillover_keeps_label_when_next_is_not_html() {
    // cell 0 = "Status:" and next_row cell 0 = "ok" → not spillover (not HTML)
    let row = make_body_row(
        vec![make_cell("Status:"), make_cell(""), make_cell("")],
        /*has_table_pipe_syntax*/ true,
    );
    let next = make_body_row(
        vec![make_cell("ok"), make_cell(""), make_cell("")],
        /*has_table_pipe_syntax*/ true,
    );
    assert!(!W::is_spillover_row(&row, Some(&next)));
}

#[test]
fn annotates_explicit_web_link_label_and_visible_destination() {
    let lines = render_markdown_lines_with_width_and_cwd(
        "See [docs](https://example.com/reference).",
        /*width*/ Some(80),
        /*cwd*/ None,
    );
    let links = lines
        .iter()
        .flat_map(|line| line.hyperlinks.iter())
        .collect::<Vec<_>>();

    assert_eq!(links.len(), 2);
    assert!(
        links
            .iter()
            .all(|link| link.destination == "https://example.com/reference")
    );
}

#[test]
fn wrapped_table_url_fragments_keep_complete_web_destination() {
    let destination = "https://example.com/a/very/long/path/to/a/table/artifact";
    let markdown = format!("| Item | URL |\n| --- | --- |\n| report | {destination} |\n");
    let lines = render_markdown_lines_with_width_and_cwd(
        &markdown,
        /*width*/ Some(32),
        /*cwd*/ None,
    );
    let linked_rows = lines
        .iter()
        .filter(|line| !line.hyperlinks.is_empty())
        .collect::<Vec<_>>();

    assert!(
        linked_rows.len() > 1,
        "expected a URL wrapped across table rows"
    );
    assert!(linked_rows.iter().all(|line| {
        line.hyperlinks
            .iter()
            .all(|link| link.destination == destination)
    }));
}

#[test]
fn table_widths_count_halfwidth_sound_marks() {
    let cell = make_cell("ｶﾞﾊﾟ");
    assert_eq!(W::cell_display_width(&cell), 4);
    assert_eq!(W::longest_token_width("ｶﾞﾊﾟtail"), 8);
}

#[test]
fn key_value_table_keeps_web_annotations() {
    let destination = "https://example.com/a/very/long/path";
    let markdown = format!(
        "| c1 | c2 | c3 | c4 | c5 | c6 |\n| --- | --- | --- | --- | --- | --- |\n| {destination} | 2 | 3 | 4 | 5 | 6 |\n"
    );
    let lines = render_markdown_lines_with_width_and_cwd(
        &markdown,
        /*width*/ Some(20),
        /*cwd*/ None,
    );
    let destinations = lines
        .iter()
        .flat_map(|line| line.hyperlinks.iter().map(|link| link.destination.as_str()))
        .collect::<Vec<_>>();

    assert!(!destinations.is_empty());
    assert!(destinations.iter().all(|link| *link == destination));
}

#[test]
fn does_not_annotate_code_or_non_web_markdown_links() {
    let markdown = "`https://example.com/inline`\n\n```text\nhttps://example.com/block\n```\n\n[mail](mailto:test@example.com)\n\n[https://example.com/label](mailto:test@example.com)\n\n| Target |\n| --- |\n| [https://example.com/table-label](mailto:test@example.com) |";
    let lines = render_markdown_lines_with_width_and_cwd(
        markdown,
        /*width*/ Some(80),
        /*cwd*/ None,
    );

    assert!(lines.iter().all(|line| line.hyperlinks.is_empty()));
}

#[test]
fn pipe_table_fallback_keeps_web_annotations() {
    let destination = "https://example.com/a/long/path";
    let target = "https://target.example/path";
    let code_url = "https://code.example/not-a-link";
    let markdown = format!(
        "| URL | Code | Label |\n| --- | --- | --- |\n| {destination} | `{code_url}` | [https://shown.example]({target}) |\n"
    );
    let lines = render_markdown_lines_with_width_and_cwd(
        &markdown,
        /*width*/ Some(5),
        /*cwd*/ None,
    );
    let destinations = lines
        .iter()
        .flat_map(|line| line.hyperlinks.iter().map(|link| link.destination.as_str()))
        .collect::<Vec<_>>();

    assert!(destinations.contains(&destination));
    assert!(destinations.contains(&target));
    assert!(!destinations.contains(&code_url));
    assert!(!destinations.contains(&"https://shown.example"));
}
