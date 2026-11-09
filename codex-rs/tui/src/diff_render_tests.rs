use super::*;
use insta::assert_debug_snapshot;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use unicode_width::UnicodeWidthChar;

#[test]
fn syntax_colors_follow_the_actual_diff_background() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    let colors = crate::terminal_probe::DefaultColors {
        fg: (30, 30, 30),
        bg: (255, 255, 255),
    };
    crate::terminal_palette::with_test_default_colors(colors, || {
        let spans = [
            RtSpan::from("let value = 1;")
                .fg(rgb_color((235, 210, 170)))
                .dim(),
            RtSpan::from(" plain"),
            RtSpan::from(" reset").fg(Color::Reset),
        ];
        let style = DiffRenderStyleContext {
            theme: DiffTheme::Light,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: ResolvedDiffBackgrounds {
                add: Some(rgb_color((220, 250, 230))),
                del: Some(rgb_color((55, 25, 35))),
            },
        };
        let lines = [DiffLineType::Insert, DiffLineType::Delete]
            .into_iter()
            .flat_map(|kind| {
                render_wrapped_diff_line(
                    /*line_number*/ 1,
                    kind,
                    "let value = 1; plain reset",
                    /*width*/ 40,
                    /*line_number_width*/ 1,
                    Some(&spans),
                    style,
                )
            })
            .map(|line| line.line)
            .collect::<Vec<_>>();
        let area = Rect::new(
            /*x*/ 0, /*y*/ 0, /*width*/ 40, /*height*/ 2,
        );
        let mut buffer = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buffer);
        assert!(!buffer[(3, 0)].modifier.contains(Modifier::DIM));
        assert!(!buffer[(3, 1)].modifier.contains(Modifier::DIM));
        assert_ne!(buffer[(3, 0)].fg, buffer[(3, 1)].fg);
        assert_ne!(buffer[(18, 0)].fg, buffer[(18, 1)].fg);
        assert_ne!(buffer[(24, 0)].fg, buffer[(24, 1)].fg);
        assert_snapshot!(format!("{buffer:?}"));
    });
}

#[test]
fn ansi16_add_style_uses_foreground_only() {
    let style = style_add(
        DiffTheme::Dark,
        DiffColorLevel::Ansi16,
        fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
    );
    assert_eq!(style.fg, Some(Color::Green));
    assert_eq!(style.bg, None);
}

#[test]
fn ansi16_del_style_uses_foreground_only() {
    let style = style_del(
        DiffTheme::Dark,
        DiffColorLevel::Ansi16,
        fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
    );
    assert_eq!(style.fg, Some(Color::Red));
    assert_eq!(style.bg, None);
}

#[test]
fn ansi16_sign_styles_use_foreground_only() {
    let add_sign = style_sign_add(
        DiffTheme::Dark,
        DiffColorLevel::Ansi16,
        fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
    );
    assert_eq!(add_sign.fg, Some(Color::Green));
    assert_eq!(add_sign.bg, None);

    let del_sign = style_sign_del(
        DiffTheme::Dark,
        DiffColorLevel::Ansi16,
        fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
    );
    assert_eq!(del_sign.fg, Some(Color::Red));
    assert_eq!(del_sign.bg, None);
}
fn diff_summary_for_tests(changes: &HashMap<PathBuf, FileChange>) -> Vec<RtLine<'static>> {
    create_diff_summary(changes, &PathBuf::from("/"), /*wrap_cols*/ 80)
}

fn snapshot_lines(name: &str, lines: Vec<RtLine<'static>>, width: u16, height: u16) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|f| {
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .render(f.area(), f.buffer_mut())
        })
        .expect("draw");
    assert!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .all(|cell| !cell.symbol().contains('\t')),
        "diff buffer should not contain literal tabs"
    );
    assert_snapshot!(name, terminal.backend());
}

fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 }))
        .sum()
}

fn line_display_width(line: &RtLine<'static>) -> usize {
    line.spans
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum()
}

fn snapshot_lines_text(name: &str, lines: &[RtLine<'static>]) {
    // Convert Lines to plain text rows and trim trailing spaces so it's
    // easier to validate indentation visually in snapshots.
    let text = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .map(|s| s.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!(name, text);
}

fn diff_gallery_changes() -> HashMap<PathBuf, FileChange> {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

    let rust_original =
        "fn greet(name: &str) {\n    println!(\"hello\");\n    println!(\"bye\");\n}\n";
    let rust_modified = "fn greet(name: &str) {\n    println!(\"hello {name}\");\n    println!(\"emoji: 🚀✨ and CJK: 你好世界\");\n}\n";
    let rust_patch = diffy::create_patch(rust_original, rust_modified).to_string();
    changes.insert(
        PathBuf::from("src/lib.rs"),
        FileChange::Update {
            unified_diff: rust_patch,
            move_path: None,
        },
    );

    let py_original = "def add(a, b):\n\treturn a + b\n\nprint(add(1, 2))\n";
    let py_modified = "def add(a, b):\n\treturn a + b + 42\n\nprint(add(1, 2))\n";
    let py_patch = diffy::create_patch(py_original, py_modified).to_string();
    changes.insert(
        PathBuf::from("scripts/calc.txt"),
        FileChange::Update {
            unified_diff: py_patch,
            move_path: Some(PathBuf::from("scripts/calc.py")),
        },
    );

    changes.insert(
        PathBuf::from("assets/banner.txt"),
        FileChange::Add {
            content: "HEADER\tVALUE\nrocket\t🚀\ncity\t東京\n".to_string(),
        },
    );
    changes.insert(
        PathBuf::from("examples/new_sample.rs"),
        FileChange::Add {
            content: "pub fn greet(name: &str) {\n    println!(\"Hello, {name}!\");\n}\n"
                .to_string(),
        },
    );

    changes.insert(
        PathBuf::from("tmp/obsolete.log"),
        FileChange::Delete {
            content: "old line 1\nold line 2\nold line 3\n".to_string(),
        },
    );
    changes.insert(
        PathBuf::from("legacy/old_script.py"),
        FileChange::Delete {
            content: "def legacy(x):\n    return x + 1\nprint(legacy(3))\n".to_string(),
        },
    );

    changes
}

fn snapshot_diff_gallery(name: &str, width: u16, height: u16) {
    let lines = create_diff_summary(
        &diff_gallery_changes(),
        &PathBuf::from("/"),
        usize::from(width),
    );
    snapshot_lines(name, lines, width, height);
}

#[test]
fn display_path_prefers_cwd_without_git_repo() {
    let cwd = if cfg!(windows) {
        PathBuf::from(r"C:\workspace\codex")
    } else {
        PathBuf::from("/workspace/codex")
    };
    let path = cwd.join("tui").join("example.png");

    let rendered = display_path_for(&path, &cwd);

    assert_eq!(
        rendered,
        PathBuf::from("tui")
            .join("example.png")
            .display()
            .to_string()
    );
}

#[test]
fn ui_snapshot_wrap_behavior_insert() {
    // Narrow width to force wrapping within our diff line rendering
    let long_line =
        "this is a very long line that should wrap across multiple terminal columns and continue";

    // Call the wrapping function directly so we can precisely control the width
    let lines = push_wrapped_diff_line_with_style_context(
        /*line_number*/ 1,
        DiffLineType::Insert,
        long_line,
        /*width*/ 80,
        line_number_width(/*max_line_number*/ 1),
        current_diff_render_style_context(),
    );

    // Render into a small terminal to capture the visual layout
    snapshot_lines(
        "wrap_behavior_insert",
        lines,
        /*width*/ 90,
        /*height*/ 8,
    );
}

#[test]
fn ui_snapshot_apply_update_block() {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    let original = "line one\nline two\nline three\n";
    let modified = "line one\nline two changed\nline three\n";
    let patch = diffy::create_patch(original, modified).to_string();

    changes.insert(
        PathBuf::from("example.txt"),
        FileChange::Update {
            unified_diff: patch,
            move_path: None,
        },
    );

    let lines = diff_summary_for_tests(&changes);

    snapshot_lines(
        "apply_update_block",
        lines,
        /*width*/ 80,
        /*height*/ 12,
    );
}

#[test]
fn ui_snapshot_apply_update_with_rename_block() {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    let original = "A\nB\nC\n";
    let modified = "A\nB changed\nC\n";
    let patch = diffy::create_patch(original, modified).to_string();

    changes.insert(
        PathBuf::from("old_name.rs"),
        FileChange::Update {
            unified_diff: patch,
            move_path: Some(PathBuf::from("new_name.rs")),
        },
    );

    let lines = diff_summary_for_tests(&changes);

    snapshot_lines(
        "apply_update_with_rename_block",
        lines,
        /*width*/ 80,
        /*height*/ 12,
    );
}

#[test]
fn ui_snapshot_apply_multiple_files_block() {
    // Two files: one update and one add, to exercise combined header and per-file rows
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();

    // File a.txt: single-line replacement (one delete, one insert)
    let patch_a = diffy::create_patch("one\n", "one changed\n").to_string();
    changes.insert(
        PathBuf::from("a.txt"),
        FileChange::Update {
            unified_diff: patch_a,
            move_path: None,
        },
    );

    // File b.txt: newly added with one line
    changes.insert(
        PathBuf::from("b.txt"),
        FileChange::Add {
            content: "new\n".to_string(),
        },
    );

    let lines = diff_summary_for_tests(&changes);

    snapshot_lines(
        "apply_multiple_files_block",
        lines,
        /*width*/ 80,
        /*height*/ 14,
    );
}

#[test]
fn ui_snapshot_apply_add_block() {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("new_file.txt"),
        FileChange::Add {
            content: "alpha\nbeta\n".to_string(),
        },
    );

    let lines = diff_summary_for_tests(&changes);

    snapshot_lines(
        "apply_add_block",
        lines,
        /*width*/ 80,
        /*height*/ 10,
    );

    let narrow = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 3);
    assert_snapshot!(format!("{}\n{}", narrow[1], narrow[2]), @r"
            1 +a
               l
        ");
}

#[test]
fn ui_snapshot_apply_delete_block() {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("tmp_delete_example.txt"),
        FileChange::Delete {
            content: "first\nsecond\nthird\n".to_string(),
        },
    );

    let lines = diff_summary_for_tests(&changes);
    snapshot_lines(
        "apply_delete_block",
        lines,
        /*width*/ 80,
        /*height*/ 12,
    );
}

#[test]
fn ui_snapshot_apply_update_block_wraps_long_lines() {
    // Create a patch with a long modified line to force wrapping
    let original = "line 1\nshort\nline 3\n";
    let modified = "line 1\nshort this_is_a_very_long_modified_line_that_should_wrap_across_multiple_terminal_columns_and_continue_even_further_beyond_eighty_columns_to_force_multiple_wraps\nline 3\n";
    let patch = diffy::create_patch(original, modified).to_string();

    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("long_example.txt"),
        FileChange::Update {
            unified_diff: patch,
            move_path: None,
        },
    );

    let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 72);

    // Render with backend width wider than wrap width to avoid Paragraph auto-wrap.
    snapshot_lines(
        "apply_update_block_wraps_long_lines",
        lines,
        /*width*/ 80,
        /*height*/ 12,
    );
}

#[test]
fn ui_snapshot_apply_update_block_wraps_long_lines_text() {
    // This mirrors the desired layout example: sign only on first inserted line,
    // subsequent wrapped pieces start aligned under the line number gutter.
    let original = "1\n2\n3\n4\n";
    let modified = "1\nadded long line which wraps and_if_there_is_a_long_token_it_will_be_broken\n3\n4 context line which also wraps across\n";
    let patch = diffy::create_patch(original, modified).to_string();

    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("wrap_demo.txt"),
        FileChange::Update {
            unified_diff: patch,
            move_path: None,
        },
    );

    let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 28);
    snapshot_lines_text("apply_update_block_wraps_long_lines_text", &lines);
}

#[test]
fn ui_snapshot_apply_update_block_line_numbers_three_digits_text() {
    let original = (1..=110).map(|i| format!("line {i}\n")).collect::<String>();
    let modified = (1..=110)
        .map(|i| {
            if i == 100 {
                format!("line {i} changed\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect::<String>();
    let patch = diffy::create_patch(&original, &modified).to_string();

    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("hundreds.txt"),
        FileChange::Update {
            unified_diff: patch,
            move_path: None,
        },
    );

    let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);
    snapshot_lines_text("apply_update_block_line_numbers_three_digits_text", &lines);
}

#[test]
fn ui_snapshot_apply_update_block_relativizes_path() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let abs_old = cwd.join("abs_old.rs");
    let abs_new = cwd.join("abs_new.rs");

    let original = "X\nY\n";
    let modified = "X changed\nY\n";
    let patch = diffy::create_patch(original, modified).to_string();

    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        abs_old,
        FileChange::Update {
            unified_diff: patch,
            move_path: Some(abs_new),
        },
    );

    let lines = create_diff_summary(&changes, &cwd, /*wrap_cols*/ 80);

    snapshot_lines(
        "apply_update_block_relativizes_path",
        lines,
        /*width*/ 80,
        /*height*/ 10,
    );
}

#[test]
fn ui_snapshot_syntax_highlighted_insert_wraps() {
    // A long Rust line that exceeds 80 cols with syntax highlighting should
    // wrap to multiple output lines rather than being clipped.
    let long_rust = "fn very_long_function_name(arg_one: String, arg_two: String, arg_three: String, arg_four: String) -> Result<String, Box<dyn std::error::Error>> { Ok(arg_one) }";

    let syntax_spans =
        highlight_code_to_styled_spans(long_rust, "rust").expect("rust highlighting");
    let spans = &syntax_spans[0];

    let lines = render_wrapped_diff_line(
        /*line_number*/ 1,
        DiffLineType::Insert,
        long_rust,
        /*width*/ 80,
        line_number_width(/*max_line_number*/ 1),
        Some(spans),
        current_diff_render_style_context(),
    );

    assert!(
        lines.len() > 1,
        "syntax-highlighted long line should wrap to multiple lines, got {}",
        lines.len()
    );

    for line in &lines {
        let source = line.source.as_ref().expect("diff source");
        let restored = source.styled_range(source.range.clone());
        let visible = RtLine::from(line.line.spans[1..].to_vec()).style(line.line.style);
        let area = Rect::new(
            /*x*/ 0,
            /*y*/ 0,
            restored.width() as u16,
            /*height*/ 1,
        );
        let mut restored_buffer = Buffer::empty(area);
        let mut visible_buffer = Buffer::empty(area);
        Paragraph::new(restored).render(area, &mut restored_buffer);
        Paragraph::new(visible).render(area, &mut visible_buffer);
        assert_eq!(restored_buffer, visible_buffer);
    }

    snapshot_lines(
        "syntax_highlighted_insert_wraps",
        visible_lines(lines),
        /*width*/ 90,
        /*height*/ 10,
    );
}

#[test]
fn ui_snapshot_diff_gallery_80x24() {
    snapshot_diff_gallery("diff_gallery_80x24", /*width*/ 80, /*height*/ 24);
}

#[test]
fn ui_snapshot_diff_gallery_94x35() {
    snapshot_diff_gallery("diff_gallery_94x35", /*width*/ 94, /*height*/ 35);
}

#[test]
fn ui_snapshot_diff_gallery_120x40() {
    snapshot_diff_gallery(
        "diff_gallery_120x40",
        /*width*/ 120,
        /*height*/ 40,
    );
}

#[test]
fn ui_snapshot_ansi16_insert_delete_no_background() {
    let mut lines = push_wrapped_diff_line_with_style_context(
        /*line_number*/ 1,
        DiffLineType::Insert,
        "added in ansi16 mode",
        /*width*/ 80,
        line_number_width(/*max_line_number*/ 2),
        DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::Ansi16,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        },
    );
    lines.extend(push_wrapped_diff_line_with_style_context(
        /*line_number*/ 2,
        DiffLineType::Delete,
        "deleted in ansi16 mode",
        /*width*/ 80,
        line_number_width(/*max_line_number*/ 2),
        DiffRenderStyleContext {
            theme: DiffTheme::Dark,
            color_level: DiffColorLevel::Ansi16,
            diff_backgrounds: fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16),
        },
    ));

    snapshot_lines(
        "ansi16_insert_delete_no_background",
        lines,
        /*width*/ 40,
        /*height*/ 4,
    );
}

#[test]
fn truecolor_dark_theme_uses_configured_backgrounds() {
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Insert,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor)
        ),
        Style::default().bg(rgb_color(DARK_TC_ADD_LINE_BG_RGB))
    );
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Delete,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::TrueColor)
        ),
        Style::default().bg(rgb_color(DARK_TC_DEL_LINE_BG_RGB))
    );
    assert_eq!(
        style_gutter_for(
            DiffLineType::Insert,
            DiffTheme::Dark,
            DiffColorLevel::TrueColor
        ),
        style_gutter_dim()
    );
    assert_eq!(
        style_gutter_for(
            DiffLineType::Delete,
            DiffTheme::Dark,
            DiffColorLevel::TrueColor
        ),
        style_gutter_dim()
    );
}

#[test]
fn ansi256_dark_theme_uses_distinct_add_and_delete_backgrounds() {
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Insert,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
        ),
        Style::default().bg(indexed_color(DARK_256_ADD_LINE_BG_IDX))
    );
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Delete,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
        ),
        Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
    );
    assert_ne!(
        style_line_bg_for(
            DiffLineType::Insert,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
        ),
        style_line_bg_for(
            DiffLineType::Delete,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi256)
        ),
        "256-color mode should keep add/delete backgrounds distinct"
    );
}

#[test]
fn theme_scope_backgrounds_override_truecolor_fallback_when_available() {
    let backgrounds = resolve_diff_backgrounds_for(
        DiffTheme::Dark,
        DiffColorLevel::TrueColor,
        DiffScopeBackgrounds {
            inserted: Some(DiffScopeBackground::Rgb((1, 2, 3))),
            deleted: Some(DiffScopeBackground::Rgb((4, 5, 6))),
        },
    );
    assert_eq!(
        style_line_bg_for(DiffLineType::Insert, backgrounds),
        Style::default().bg(rgb_color((1, 2, 3)))
    );
    assert_eq!(
        style_line_bg_for(DiffLineType::Delete, backgrounds),
        Style::default().bg(rgb_color((4, 5, 6)))
    );
}

#[test]
fn explicit_default_scope_disables_only_its_diff_fill() {
    for theme in [DiffTheme::Light, DiffTheme::Dark] {
        for color_level in [
            DiffColorLevel::TrueColor,
            DiffColorLevel::Ansi256,
            DiffColorLevel::Ansi16,
        ] {
            let backgrounds = resolve_diff_backgrounds_for(
                theme,
                color_level,
                DiffScopeBackgrounds {
                    inserted: Some(DiffScopeBackground::TerminalDefault),
                    deleted: None,
                },
            );
            assert_eq!(
                style_line_bg_for(DiffLineType::Insert, backgrounds),
                Style::default()
            );
            assert_eq!(
                style_line_bg_for(DiffLineType::Delete, backgrounds),
                style_line_bg_for(
                    DiffLineType::Delete,
                    fallback_diff_backgrounds(theme, color_level)
                )
            );
        }
    }
}

#[test]
fn theme_scope_backgrounds_quantize_to_ansi256() {
    let backgrounds = resolve_diff_backgrounds_for(
        DiffTheme::Dark,
        DiffColorLevel::Ansi256,
        DiffScopeBackgrounds {
            inserted: Some(DiffScopeBackground::Rgb((0, 95, 0))),
            deleted: None,
        },
    );
    assert_eq!(
        style_line_bg_for(DiffLineType::Insert, backgrounds),
        Style::default().bg(indexed_color(/*index*/ 22))
    );
    assert_eq!(
        style_line_bg_for(DiffLineType::Delete, backgrounds),
        Style::default().bg(indexed_color(DARK_256_DEL_LINE_BG_IDX))
    );
}

#[test]
fn ui_snapshot_theme_scope_background_resolution() {
    let backgrounds = resolve_diff_backgrounds_for(
        DiffTheme::Dark,
        DiffColorLevel::TrueColor,
        DiffScopeBackgrounds {
            inserted: Some(DiffScopeBackground::Rgb((12, 34, 56))),
            deleted: None,
        },
    );
    let snapshot = format!(
        "insert={:?}\ndelete={:?}",
        style_line_bg_for(DiffLineType::Insert, backgrounds).bg,
        style_line_bg_for(DiffLineType::Delete, backgrounds).bg,
    );
    assert_snapshot!("theme_scope_background_resolution", snapshot);
}

#[test]
fn ansi16_disables_line_and_gutter_backgrounds() {
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Insert,
            fallback_diff_backgrounds(DiffTheme::Dark, DiffColorLevel::Ansi16)
        ),
        Style::default()
    );
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Delete,
            fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::Ansi16)
        ),
        Style::default()
    );
    assert_eq!(
        style_gutter_for(
            DiffLineType::Insert,
            DiffTheme::Light,
            DiffColorLevel::Ansi16
        ),
        Style::default().fg(Color::Black)
    );
    assert_eq!(
        style_gutter_for(
            DiffLineType::Delete,
            DiffTheme::Light,
            DiffColorLevel::Ansi16
        ),
        Style::default().fg(Color::Black)
    );
    let themed_backgrounds = resolve_diff_backgrounds_for(
        DiffTheme::Light,
        DiffColorLevel::Ansi16,
        DiffScopeBackgrounds {
            inserted: Some(DiffScopeBackground::Rgb((8, 9, 10))),
            deleted: Some(DiffScopeBackground::Rgb((11, 12, 13))),
        },
    );
    assert_eq!(
        style_line_bg_for(DiffLineType::Insert, themed_backgrounds),
        Style::default()
    );
    assert_eq!(
        style_line_bg_for(DiffLineType::Delete, themed_backgrounds),
        Style::default()
    );
}

#[test]
fn light_truecolor_theme_uses_readable_gutter_and_line_backgrounds() {
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Insert,
            fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor)
        ),
        Style::default().bg(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB))
    );
    assert_eq!(
        style_line_bg_for(
            DiffLineType::Delete,
            fallback_diff_backgrounds(DiffTheme::Light, DiffColorLevel::TrueColor)
        ),
        Style::default().bg(rgb_color(LIGHT_TC_DEL_LINE_BG_RGB))
    );
    assert_eq!(
        style_gutter_for(
            DiffLineType::Insert,
            DiffTheme::Light,
            DiffColorLevel::TrueColor
        ),
        Style::default()
            .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
            .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
    );
    assert_eq!(
        style_gutter_for(
            DiffLineType::Delete,
            DiffTheme::Light,
            DiffColorLevel::TrueColor
        ),
        Style::default()
            .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
            .bg(rgb_color(LIGHT_TC_DEL_NUM_BG_RGB))
    );
}

#[test]
fn light_theme_wrapped_lines_keep_number_gutter_contrast() {
    let lines = push_wrapped_diff_line_with_style_context(
        /*line_number*/ 12,
        DiffLineType::Insert,
        "abcdefghij",
        /*width*/ 8,
        line_number_width(/*max_line_number*/ 12),
        DiffRenderStyleContext {
            theme: DiffTheme::Light,
            color_level: DiffColorLevel::TrueColor,
            diff_backgrounds: fallback_diff_backgrounds(
                DiffTheme::Light,
                DiffColorLevel::TrueColor,
            ),
        },
    );

    assert!(
        lines.len() > 1,
        "expected wrapped output for gutter style verification"
    );
    assert_eq!(
        lines[0].spans[0].style,
        Style::default()
            .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
            .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
    );
    assert_eq!(
        lines[1].spans[0].style,
        Style::default()
            .fg(rgb_color(LIGHT_TC_GUTTER_FG_RGB))
            .bg(rgb_color(LIGHT_TC_ADD_NUM_BG_RGB))
    );
    assert_eq!(lines[0].style.bg, Some(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB)));
    assert_eq!(lines[1].style.bg, Some(rgb_color(LIGHT_TC_ADD_LINE_BG_RGB)));
}

#[test]
fn windows_terminal_promotes_ansi16_to_truecolor_for_diffs() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Ansi16,
            TerminalName::WindowsTerminal,
            /*has_wt_session*/ false,
            /*has_force_color_override*/ false,
        ),
        DiffColorLevel::TrueColor
    );
}

#[test]
fn wt_session_promotes_ansi16_to_truecolor_for_diffs() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Ansi16,
            TerminalName::Unknown,
            /*has_wt_session*/ true,
            /*has_force_color_override*/ false,
        ),
        DiffColorLevel::TrueColor
    );
}

#[test]
fn non_windows_terminal_keeps_ansi16_diff_palette() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Ansi16,
            TerminalName::WezTerm,
            /*has_wt_session*/ false,
            /*has_force_color_override*/ false,
        ),
        DiffColorLevel::Ansi16
    );
}

#[test]
fn wt_session_promotes_unknown_color_level_to_truecolor() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Unknown,
            TerminalName::WindowsTerminal,
            /*has_wt_session*/ true,
            /*has_force_color_override*/ false,
        ),
        DiffColorLevel::TrueColor
    );
}

#[test]
fn non_wt_windows_terminal_keeps_unknown_color_level_conservative() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Unknown,
            TerminalName::WindowsTerminal,
            /*has_wt_session*/ false,
            /*has_force_color_override*/ false,
        ),
        DiffColorLevel::Ansi16
    );
}

#[test]
fn explicit_force_override_keeps_ansi16_on_windows_terminal() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Ansi16,
            TerminalName::WindowsTerminal,
            /*has_wt_session*/ false,
            /*has_force_color_override*/ true,
        ),
        DiffColorLevel::Ansi16
    );
}

#[test]
fn explicit_force_override_keeps_ansi256_on_windows_terminal() {
    assert_eq!(
        diff_color_level_for_terminal(
            StdoutColorLevel::Ansi256,
            TerminalName::WindowsTerminal,
            /*has_wt_session*/ true,
            /*has_force_color_override*/ true,
        ),
        DiffColorLevel::Ansi256
    );
}

#[test]
fn add_diff_uses_path_extension_for_highlighting() {
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
            changes.insert(
                PathBuf::from("highlight_add.rs"),
                FileChange::Add {
                    content: "pub fn sum(a: i32, b: i32) -> i32 { a + b }\n".to_string(),
                },
            );

            let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);
            let has_rgb = lines.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
            });
            assert!(
                has_rgb,
                "add diff for .rs file should produce syntax-highlighted (RGB) spans"
            );
        },
    );
}

#[test]
fn cpp_module_extensions_use_cpp_highlighting() {
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            let highlighted_tokens = [
                "cpp",
                // CUDA source and header extensions use C++ highlighting as a fallback.
                "cu", "cuh", "cppm", "CPPM", "cxxm", "CxXm", "ixx", "IXX",
            ]
            .into_iter()
            .map(|extension| {
                let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
                changes.insert(
                    PathBuf::from(format!("math.{extension}")),
                    FileChange::Add {
                        content:
                            "export module math;\nexport int sum(int a, int b) { return a + b; }\n"
                                .to_string(),
                    },
                );

                let lines =
                    create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);
                let rgb_tokens = lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .filter(|span| matches!(span.style.fg, Some(ratatui::style::Color::Rgb(..))))
                    .map(|span| span.content.to_string())
                    .collect::<Vec<_>>();
                assert!(
                    !rgb_tokens.is_empty(),
                    "add diff for .{extension} file should produce syntax-highlighted (RGB) spans"
                );
                (extension, rgb_tokens.join("|"))
            })
            .collect::<Vec<_>>();

            assert_debug_snapshot!("cpp_module_extension_highlighting", highlighted_tokens);
        },
    );
}

#[test]
fn unknown_extension_falls_back_without_syntax_highlighting() {
    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("math.unknown-extension"),
        FileChange::Add {
            content: "export module math;\nexport int value = 42;\n".to_string(),
        },
    );

    let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);
    assert!(lines.iter().all(|line| {
        line.spans
            .iter()
            .all(|span| !matches!(span.style.fg, Some(ratatui::style::Color::Rgb(..))))
    }));
}

#[test]
fn delete_diff_uses_path_extension_for_highlighting() {
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
            changes.insert(
                PathBuf::from("highlight_delete.py"),
                FileChange::Delete {
                    content: "def scale(x):\n    return x * 2\n".to_string(),
                },
            );

            let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);
            let has_rgb = lines.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
            });
            assert!(
                has_rgb,
                "delete diff for .py file should produce syntax-highlighted (RGB) spans"
            );
        },
    );
}

#[test]
fn detect_lang_for_common_paths() {
    // Standard extensions are detected.
    assert!(detect_lang_for_path(Path::new("foo.rs")).is_some());
    assert!(detect_lang_for_path(Path::new("bar.py")).is_some());
    assert!(detect_lang_for_path(Path::new("app.tsx")).is_some());

    // Extensionless files return None.
    assert!(detect_lang_for_path(Path::new("Makefile")).is_none());
    assert!(detect_lang_for_path(Path::new("randomfile")).is_none());
}

#[test]
fn wrap_styled_spans_single_line() {
    // Content that fits in one line should produce exactly one chunk.
    let spans = vec![RtSpan::raw("short")];
    let result = wrap_styled_spans(&spans, /*max_cols*/ 80);
    assert_eq!(result.len(), 1);
}

#[test]
fn wrap_styled_spans_splits_long_content() {
    // Content wider than max_cols should produce multiple chunks.
    let long_text = "a".repeat(100);
    let spans = vec![RtSpan::raw(long_text)];
    let result = wrap_styled_spans(&spans, /*max_cols*/ 40);
    assert!(
        result.len() >= 3,
        "100 chars at 40 cols should produce at least 3 lines, got {}",
        result.len()
    );
}

#[test]
fn wrap_styled_spans_flushes_at_span_boundary() {
    // When span A fills exactly to max_cols and span B follows, the line
    // must be flushed before B starts. Otherwise B's first character lands
    // on an already-full line, producing over-width output.
    let style_a = Style::default().fg(Color::Red);
    let style_b = Style::default().fg(Color::Blue);
    let spans = vec![
        RtSpan::styled("aaaa", style_a), // 4 cols, fills line exactly at max_cols=4
        RtSpan::styled("bb", style_b),   // should start on a new line
    ];
    let result = wrap_styled_spans(&spans, /*max_cols*/ 4);
    assert_eq!(
        result.len(),
        2,
        "span ending exactly at max_cols should flush before next span: {result:?}"
    );
    // First line should only contain the 'a' span.
    let first_width: usize = result[0].iter().map(|s| s.content.chars().count()).sum();
    assert!(
        first_width <= 4,
        "first line should be at most 4 cols wide, got {first_width}"
    );
}

#[test]
fn wrap_styled_spans_preserves_styles() {
    // Verify that styles survive split boundaries.
    let style = Style::default().fg(Color::Green);
    let text = "x".repeat(50);
    let spans = vec![RtSpan::styled(text, style)];
    let result = wrap_styled_spans(&spans, /*max_cols*/ 20);
    for chunk in &result {
        for span in chunk {
            assert_eq!(span.style, style, "style should be preserved across wraps");
        }
    }
}

#[test]
fn wrap_styled_spans_tabs_have_visible_width() {
    // A tab should count as TAB_WIDTH columns, not zero.
    // With max_cols=8, a tab (4 cols) + "abcde" (5 cols) = 9 cols → must wrap.
    let style = Style::default().fg(Color::Green);
    let spans = vec![RtSpan::styled("\tabcde", style)];
    let result = wrap_styled_spans(&spans, /*max_cols*/ 8);
    assert_eq!(
        result,
        vec![
            vec![RtSpan::styled("    abcd", style)],
            vec![RtSpan::styled("e", style)],
        ]
    );
}

#[test]
fn wrap_styled_spans_wraps_before_first_overflowing_char() {
    let spans = vec![RtSpan::raw("abcd\t界")];
    let result = wrap_styled_spans(&spans, /*max_cols*/ 5);

    let line_text: Vec<String> = result
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect();
    assert_eq!(line_text, vec!["abcd", "    ", "界"]);

    let line_width = |line: &[RtSpan<'static>]| -> usize {
        line.iter()
            .flat_map(|span| span.content.chars())
            .map(|ch| ch.width().unwrap_or(if ch == '\t' { TAB_WIDTH } else { 0 }))
            .sum()
    };
    for line in &result {
        assert!(
            line_width(line) <= 5,
            "wrapped line exceeded width 5: {line:?}"
        );
    }
}

#[test]
fn fallback_wrapping_uses_display_width_for_tabs_and_wide_chars() {
    let width = 8;
    let lines = push_wrapped_diff_line_with_style_context(
        /*line_number*/ 1,
        DiffLineType::Insert,
        "abcd\t界🙂",
        width,
        line_number_width(/*max_line_number*/ 1),
        current_diff_render_style_context(),
    );

    assert!(lines.len() >= 2, "expected wrapped output, got {lines:?}");
    for line in &lines {
        assert!(
            line_display_width(line) <= width,
            "fallback wrapped line exceeded width {width}: {line:?}"
        );
    }
}

#[test]
fn large_update_diff_skips_highlighting() {
    // Build a patch large enough to exceed MAX_HIGHLIGHT_LINES (10_000).
    // Without the pre-check this would attempt 10k+ parser initializations.
    let line_count = 10_001;
    let patch = format!(
        "--- a/huge.rs\n+++ b/huge.rs\n@@ -0,0 +1,{line_count} @@\n{}",
        "+let value = 1;\n".repeat(line_count)
    );

    let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
    changes.insert(
        PathBuf::from("huge.rs"),
        FileChange::Update {
            unified_diff: patch,
            move_path: None,
        },
    );

    // Should complete quickly (no per-line parser init). If guardrails
    // are bypassed this would be extremely slow.
    let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);

    // The diff rendered without timing out — the guardrails prevented
    // thousands of per-line parser initializations.  Verify we actually
    // got output (the patch is non-empty).
    assert!(
        lines.len() > 100,
        "expected many output lines from large diff, got {}",
        lines.len(),
    );

    // No span should contain an RGB foreground color (syntax themes
    // produce RGB; plain diff styles only use named Color variants).
    for line in &lines {
        for span in &line.spans {
            if let Some(ratatui::style::Color::Rgb(..)) = span.style.fg {
                panic!(
                    "large diff should not have syntax-highlighted spans, \
                         got RGB color in style {:?} for {:?}",
                    span.style, span.content,
                );
            }
        }
    }
}

#[test]
fn rename_diff_uses_destination_extension_for_highlighting() {
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            // A rename from an unknown extension to .rs should highlight as Rust.
            // Without the fix, detect_lang_for_path uses the source path (.xyzzy),
            // which has no syntax definition, so highlighting is skipped.
            let original = "fn main() {}\n";
            let modified = "fn main() { println!(\"hi\"); }\n";
            let patch = diffy::create_patch(original, modified).to_string();

            let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
            changes.insert(
                PathBuf::from("foo.xyzzy"),
                FileChange::Update {
                    unified_diff: patch,
                    move_path: Some(PathBuf::from("foo.rs")),
                },
            );

            let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 80);
            let has_rgb = lines.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|s| matches!(s.style.fg, Some(ratatui::style::Color::Rgb(..))))
            });
            assert!(
                has_rgb,
                "rename from .xyzzy to .rs should produce syntax-highlighted (RGB) spans"
            );
        },
    );
}

#[test]
fn update_diff_preserves_multiline_highlight_state_within_hunk() {
    crate::terminal_palette::with_test_default_colors(
        crate::terminal_probe::DefaultColors {
            fg: (220, 220, 220),
            bg: (20, 20, 20),
        },
        || {
            let original = "fn demo() {\n    let s = \"hello\";\n}\n";
            let modified = "fn demo() {\n    let s = \"hello\nworld\";\n}\n";
            let patch = diffy::create_patch(original, modified).to_string();

            let mut changes: HashMap<PathBuf, FileChange> = HashMap::new();
            changes.insert(
                PathBuf::from("demo.rs"),
                FileChange::Update {
                    unified_diff: patch,
                    move_path: None,
                },
            );

            let expected_multiline =
                highlight_code_to_styled_spans("    let s = \"hello\nworld\";\n", "rust")
                    .expect("rust highlighting");
            let expected_style = expected_multiline
                .get(/*index*/ 1)
                .and_then(|line| {
                    line.iter()
                        .find(|span| span.content.as_ref().contains("world"))
                })
                .map(|span| span.style)
                .expect("expected highlighted span for second multiline string line");

            let lines = create_diff_summary(&changes, &PathBuf::from("/"), /*wrap_cols*/ 120);
            let actual_style = lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .find(|span| span.content.as_ref().contains("world"))
                .map(|span| span.style)
                .expect("expected rendered diff span containing 'world'");

            assert!(matches!(expected_style.fg, Some(Color::Rgb(..))));
            assert_eq!(actual_style, expected_style.remove_modifier(Modifier::DIM));
        },
    );
}
