use super::*;
use crate::app_event::AppEvent;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use tokio::sync::mpsc::unbounded_channel;

struct MarkerRenderable {
    marker: &'static str,
    height: u16,
}

impl Renderable for MarkerRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                if x < buf.area().width && y < buf.area().height {
                    buf[(x, y)].set_symbol(self.marker);
                }
            }
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }
}

struct StyledMarkerRenderable {
    marker: &'static str,
    style: Style,
    height: u16,
}

impl Renderable for StyledMarkerRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                if x < buf.area().width && y < buf.area().height {
                    buf[(x, y)].set_symbol(self.marker).set_style(self.style);
                }
            }
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.height
    }
}

fn new_view(params: SelectionViewParams, tx: AppEventSender) -> ListSelectionView {
    ListSelectionView::new(params, tx, crate::keymap::RuntimeKeymap::defaults().list)
}

#[test]
fn secondary_action_respects_search_disabled_rows_and_dismissal() {
    for (is_searchable, is_disabled, key, expected) in [
        (false, false, KeyCode::Char('s'), Some("secondary")),
        (false, false, KeyCode::Enter, Some("primary")),
        (false, true, KeyCode::Char('s'), None),
        (true, false, KeyCode::Char('s'), None),
    ] {
        let (tx, mut rx) = unbounded_channel();
        let mut view = new_view(
            SelectionViewParams {
                is_searchable,
                items: vec![SelectionItem {
                    name: "Selection".into(),
                    is_disabled,
                    dismiss_on_select: true,
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::UpdateModel("primary".into()))
                    })],
                    secondary_action: Some(SelectionSecondaryAction {
                        key: crate::key_hint::plain(KeyCode::Char('s')),
                        action: Box::new(|tx| tx.send(AppEvent::UpdateModel("secondary".into()))),
                        footer_hint: "s for secondary".into(),
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            },
            AppEventSender::new(tx),
        );
        view.handle_key_event(KeyEvent::from(key));
        let actual = rx.try_recv().ok().map(|event| match event {
            AppEvent::UpdateModel(model) => model,
            event => panic!("unexpected event: {event:?}"),
        });
        assert_eq!(actual.as_deref(), expected);
        assert_eq!(view.completion, expected.map(|_| ViewCompletion::Accepted));
        assert_eq!(view.search_query, if is_searchable { "s" } else { "" });
    }
}

#[test]
fn searchable_list_honors_configured_printable_accept_alternative() {
    let (tx, mut rx) = unbounded_channel();
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.accept = vec![
        crate::key_hint::plain(KeyCode::Char('z')),
        crate::key_hint::plain(KeyCode::Enter),
    ];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            is_searchable: true,
            items: vec![SelectionItem {
                name: "Selection".into(),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::UpdateModel("accepted".into()))
                })],
                ..Default::default()
            }],
            ..Default::default()
        },
        AppEventSender::new(tx),
        keymap,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Char('z')));

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateModel(model)) if model == "accepted"
    ));
    assert_eq!(view.search_query(), Some(""));
}

fn make_selection_view(subtitle: Option<&str>) -> ListSelectionView {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![
        SelectionItem {
            name: "Read Only".to_string(),
            description: Some("Codex can read files".to_string()),
            is_current: true,
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "Full Access".to_string(),
            description: Some("Codex can edit files".to_string()),
            is_current: false,
            dismiss_on_select: true,
            ..Default::default()
        },
    ];
    new_view(
        SelectionViewParams {
            title: Some("Select Approval Mode".to_string()),
            subtitle: subtitle.map(str::to_string),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        },
        tx,
    )
}

fn render_lines(view: &ListSelectionView) -> String {
    render_lines_with_width(view, /*width*/ 48)
}

fn render_lines_with_width(view: &ListSelectionView, width: u16) -> String {
    render_lines_in_area(view, width, view.desired_height(width))
}

fn render_lines_in_area(view: &ListSelectionView, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);

    let lines: Vec<String> = (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(area.x + col, area.y + row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line
        })
        .collect();
    lines.join("\n")
}

fn description_col(rendered: &str, item_marker: &str, description: &str) -> usize {
    let line = rendered
        .lines()
        .find(|line| line.contains(item_marker) && line.contains(description))
        .expect("expected rendered line to contain row marker and description");
    line.find(description)
        .expect("expected rendered line to contain description")
}

fn make_scrolling_width_items() -> Vec<SelectionItem> {
    let mut items: Vec<SelectionItem> = (1..=8)
        .map(|idx| SelectionItem {
            name: format!("Item {idx}"),
            description: Some(format!("desc {idx}")),
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();
    items.push(SelectionItem {
        name: "Item 9 with an intentionally much longer name".to_string(),
        description: Some("desc 9".to_string()),
        dismiss_on_select: true,
        ..Default::default()
    });
    items
}

fn render_before_after_scroll_snapshot(col_width_mode: ColumnWidthMode, width: u16) -> String {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: make_scrolling_width_items(),
            col_width_mode,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    let before_scroll = render_lines_with_width(&view, width);
    for _ in 0..8 {
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    let after_scroll = render_lines_with_width(&view, width);

    format!("before scroll:\n{before_scroll}\n\nafter scroll:\n{after_scroll}")
}

#[test]
fn renders_blank_line_between_title_and_items_without_subtitle() {
    let view = make_selection_view(/*subtitle*/ None);
    assert_snapshot!(
        "list_selection_spacing_without_subtitle",
        render_lines(&view)
    );
}

#[test]
fn renders_blank_line_between_subtitle_and_items() {
    let view = make_selection_view(Some("Switch between Codex approval presets"));
    assert_snapshot!("list_selection_spacing_with_subtitle", render_lines(&view));
}

#[test]
fn theme_picker_subtitle_uses_fallback_text_in_94x35_terminal() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let home = dirs::home_dir().expect("home directory should be available");
    let codex_home = home.join(".codex");
    let params = crate::theme_picker::build_theme_picker_params(
        /*current_name*/ None,
        Some(&codex_home),
        Some(94),
    );
    let view = new_view(params, tx);

    let rendered = render_lines_in_area(&view, /*width*/ 94, /*height*/ 35);
    assert!(rendered.contains("Move up/down to live preview themes"));
}

#[test]
fn theme_picker_enables_side_content_background_preservation() {
    let params = crate::theme_picker::build_theme_picker_params(
        /*current_name*/ None,
        /*codex_home*/ None,
        Some(120),
    );
    assert!(
        params.preserve_side_content_bg,
        "theme picker should preserve side-content backgrounds to keep diff preview styling",
    );
}

#[test]
fn preserve_side_content_bg_keeps_rendered_background_colors() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: vec![SelectionItem {
                name: "Item 1".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            side_content: Box::new(StyledMarkerRenderable {
                marker: "+",
                style: Style::default().bg(Color::Blue),
                height: 1,
            }),
            side_content_width: SideContentWidth::Half,
            side_content_min_width: 10,
            preserve_side_content_bg: true,
            ..Default::default()
        },
        tx,
    );
    let area = Rect::new(0, 0, 120, 35);
    let mut buf = Buffer::empty(area);

    view.render(area, &mut buf);

    let plus_bg = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .find_map(|(x, y)| {
            let cell = &buf[(x, y)];
            (cell.symbol() == "+").then(|| cell.style().bg)
        })
        .expect("expected side content to render at least one '+' marker");
    assert_eq!(
        plus_bg,
        Some(Color::Blue),
        "expected side-content marker to preserve custom background styling",
    );
}

#[test]
fn snapshot_footer_note_wraps() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![SelectionItem {
        name: "Read Only".to_string(),
        description: Some("Codex can read files".to_string()),
        is_current: true,
        dismiss_on_select: true,
        ..Default::default()
    }];
    let footer_note = Line::from(vec![
        "Note: ".dim(),
        "Use /setup-default-sandbox".cyan(),
        " to allow network access.".dim(),
    ]);
    let view = new_view(
        SelectionViewParams {
            title: Some("Select Approval Mode".to_string()),
            footer_note: Some(footer_note),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        },
        tx,
    );
    assert_snapshot!(
        "list_selection_footer_note_wraps",
        render_lines_with_width(&view, /*width*/ 40)
    );
}

#[test]
fn renders_search_query_line_when_enabled() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![SelectionItem {
        name: "Read Only".to_string(),
        description: Some("Codex can read files".to_string()),
        is_current: false,
        dismiss_on_select: true,
        ..Default::default()
    }];
    let mut view = new_view(
        SelectionViewParams {
            title: Some("Select Approval Mode".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        },
        tx,
    );
    view.set_search_query("filters".to_string());

    let lines = render_lines(&view);
    assert!(
        lines.contains("filters"),
        "expected search query line to include rendered query, got {lines:?}"
    );
}

#[test]
fn empty_searchable_list_keeps_search_and_no_matches_visible() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        },
        tx,
    );

    let rendered = render_lines_with_width(&view, /*width*/ 48)
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!("list_selection_empty_searchable", rendered);
}

#[test]
fn paste_appends_to_search_query_and_filters_items() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = new_view(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "main -> feature/other".to_string(),
                    search_value: Some("feature/other".to_string()),
                    ..Default::default()
                },
                SelectionItem {
                    name: "main -> feature/paste-support".to_string(),
                    search_value: Some("feature/paste-support".to_string()),
                    ..Default::default()
                },
            ],
            is_searchable: true,
            ..Default::default()
        },
        tx,
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));

    assert!(view.handle_paste("eature/paste-support\n".to_string()));

    assert_eq!(view.search_query, "feature/paste-support");
    assert_eq!(view.filtered_indices, vec![1]);
    assert_eq!(view.selected_actual_idx(), Some(1));
}

#[test]
fn whitespace_only_paste_is_ignored() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = new_view(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "main".to_string(),
                search_value: Some("main".to_string()),
                ..Default::default()
            }],
            is_searchable: true,
            ..Default::default()
        },
        tx,
    );

    assert!(!view.handle_paste(" \n\t ".to_string()));

    assert_eq!(view.search_query, "");
    assert_eq!(view.filtered_indices, vec![0]);
}

#[test]
fn switching_tabs_changes_visible_items_and_clears_search() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            tabs: vec![
                SelectionTab {
                    id: "alpha".to_string(),
                    label: "Alpha".to_string(),
                    header: Box::new(()),
                    items: vec![SelectionItem {
                        name: "Alpha Item".to_string(),
                        dismiss_on_select: true,
                        ..Default::default()
                    }],
                },
                SelectionTab {
                    id: "beta".to_string(),
                    label: "Beta".to_string(),
                    header: Box::new(()),
                    items: vec![SelectionItem {
                        name: "Beta Item".to_string(),
                        dismiss_on_select: true,
                        ..Default::default()
                    }],
                },
            ],
            initial_tab_id: Some("beta".to_string()),
            is_searchable: true,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.set_search_query("beta".to_string());

    view.handle_key_event(KeyEvent::from(KeyCode::Left));

    assert_eq!(view.active_tab_id(), Some("alpha"));
    assert_eq!(view.search_query, "");
    view.handle_key_event(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert_eq!(view.active_tab_id(), Some("beta"));
    view.handle_key_event(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
    assert_eq!(view.active_tab_id(), Some("alpha"));
    let rendered = render_lines(&view);
    assert!(
        rendered.contains("Alpha Item") && !rendered.contains("Beta Item"),
        "expected switched tab to render the alpha items, got:\n{rendered}"
    );
}

#[test]
fn tabbed_view_preserves_current_row_on_initial_selection_and_tab_switch() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            tabs: vec![
                SelectionTab {
                    id: "alpha".to_string(),
                    label: "Alpha".to_string(),
                    header: Box::new(()),
                    items: vec![
                        SelectionItem {
                            name: "Alpha First".to_string(),
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                        SelectionItem {
                            name: "Alpha Current".to_string(),
                            is_current: true,
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                    ],
                },
                SelectionTab {
                    id: "beta".to_string(),
                    label: "Beta".to_string(),
                    header: Box::new(()),
                    items: vec![
                        SelectionItem {
                            name: "Beta First".to_string(),
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                        SelectionItem {
                            name: "Beta Current".to_string(),
                            is_current: true,
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                    ],
                },
            ],
            initial_tab_id: Some("beta".to_string()),
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_eq!(view.active_tab_id(), Some("beta"));
    assert_eq!(view.selected_actual_idx(), Some(1));

    view.handle_key_event(KeyEvent::from(KeyCode::Left));

    assert_eq!(view.active_tab_id(), Some("alpha"));
    assert_eq!(view.selected_actual_idx(), Some(1));
}

#[test]
fn space_appends_to_active_search_instead_of_toggling_selected_item() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Plugin".to_string(),
                toggle: Some(SelectionToggle {
                    is_on: false,
                    action: Box::new(|_enabled, tx: &_| {
                        tx.send(AppEvent::OpenApprovalsPopup);
                    }),
                }),
                ..Default::default()
            }],
            is_searchable: true,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    view.set_search_query("plugin".to_string());

    view.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    assert_eq!(view.search_query, "plugin ");
    assert!(
        !view.active_items()[0]
            .toggle
            .as_ref()
            .is_some_and(|toggle| toggle.is_on),
        "expected Space to leave the toggle state unchanged while search is active"
    );
    assert!(
        rx.try_recv().is_err(),
        "expected Space with an active search query to avoid firing the toggle action"
    );
}

#[test]
fn configured_list_binding_takes_precedence_over_search_toggle_key() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_down = vec![crate::key_hint::plain(KeyCode::Tab)];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "Plugin".to_string(),
                    toggle: Some(SelectionToggle {
                        is_on: false,
                        action: Box::new(|_enabled, tx: &_| {
                            tx.send(AppEvent::OpenApprovalsPopup);
                        }),
                    }),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Next".to_string(),
                    ..Default::default()
                },
            ],
            is_searchable: true,
            search_toggle_key: Some(crate::key_hint::plain(KeyCode::Tab)),
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Tab));

    assert_eq!(view.selected_actual_idx(), Some(1));
    assert!(
        !view.active_items()[0]
            .toggle
            .as_ref()
            .is_some_and(|toggle| toggle.is_on),
        "expected the configured move-down binding to leave the toggle unchanged"
    );
    assert!(
        rx.try_recv().is_err(),
        "expected the configured move-down binding not to fire the toggle action"
    );
}

#[test]
fn configured_cancel_binding_takes_precedence_over_space_toggle() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.cancel = vec![crate::key_hint::plain(KeyCode::Char(' '))];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Animations".to_string(),
                toggle: Some(SelectionToggle {
                    is_on: true,
                    action: Box::new(|_enabled, tx: &_| {
                        tx.send(AppEvent::OpenApprovalsPopup);
                    }),
                }),
                checkbox_toggle: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    assert_eq!(view.completion, Some(ViewCompletion::Cancelled));
    assert!(
        view.active_items()[0]
            .toggle
            .as_ref()
            .is_some_and(|toggle| toggle.is_on)
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn single_line_row_display_truncates_instead_of_wrapping() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let single_line_view = ListSelectionView::new(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: vec![SelectionItem {
                name: "A very long plugin name".to_string(),
                description: Some(
                    "A very long description that would normally wrap onto another line."
                        .to_string(),
                ),
                dismiss_on_select: true,
                ..Default::default()
            }],
            row_display: SelectionRowDisplay::SingleLine,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    let (wrapped_tx_raw, _wrapped_rx) = unbounded_channel::<AppEvent>();
    let wrapped_tx = AppEventSender::new(wrapped_tx_raw);
    let wrapped_view = ListSelectionView::new(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: vec![SelectionItem {
                name: "A very long plugin name".to_string(),
                description: Some(
                    "A very long description that would normally wrap onto another line."
                        .to_string(),
                ),
                dismiss_on_select: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        wrapped_tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    let rendered = render_lines_with_width(&single_line_view, /*width*/ 36);
    assert!(
        rendered.contains("…"),
        "expected single-line rendering to truncate with an ellipsis, got:\n{rendered}"
    );
    assert!(
        single_line_view.desired_height(/*width*/ 36)
            < wrapped_view.desired_height(/*width*/ 36),
        "expected single-line rendering to reserve less height than wrapped rendering:\nsingle-line:\n{rendered}\n\nwrapped:\n{}",
        render_lines_with_width(&wrapped_view, /*width*/ 36)
    );
}

#[test]
fn name_column_width_override_moves_description_column_right() {
    let auto_items = vec![
        SelectionItem {
            name: "Short".to_string(),
            description: Some("desc".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "Longer".to_string(),
            description: Some("desc".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        },
    ];
    let widened_items = vec![
        SelectionItem {
            name: "Short".to_string(),
            description: Some("desc".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "Longer".to_string(),
            description: Some("desc".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        },
    ];
    let (auto_tx_raw, _auto_rx) = unbounded_channel::<AppEvent>();
    let auto_tx = AppEventSender::new(auto_tx_raw);
    let auto_view = ListSelectionView::new(
        SelectionViewParams {
            items: auto_items,
            row_display: SelectionRowDisplay::SingleLine,
            col_width_mode: ColumnWidthMode::AutoVisible,
            ..Default::default()
        },
        auto_tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    let (widened_tx_raw, _widened_rx) = unbounded_channel::<AppEvent>();
    let widened_tx = AppEventSender::new(widened_tx_raw);
    let widened_view = ListSelectionView::new(
        SelectionViewParams {
            items: widened_items,
            row_display: SelectionRowDisplay::SingleLine,
            col_width_mode: ColumnWidthMode::AutoVisible,
            name_column_width: Some(18),
            ..Default::default()
        },
        widened_tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    let auto_rendered = render_lines_with_width(&auto_view, /*width*/ 48);
    let widened_rendered = render_lines_with_width(&widened_view, /*width*/ 48);
    let auto_col = description_col(&auto_rendered, "1. Short", "desc");
    let widened_col = description_col(&widened_rendered, "1. Short", "desc");

    assert!(
        widened_col > auto_col,
        "expected name column override to push the description right:\nauto:\n{auto_rendered}\n\nwidened:\n{widened_rendered}"
    );
}

#[test]
fn enter_with_no_matches_triggers_cancel_callback() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = new_view(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Read Only".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            is_searchable: true,
            on_cancel: Some(Box::new(|tx: &_| {
                tx.send(AppEvent::OpenApprovalsPopup);
            })),
            ..Default::default()
        },
        tx,
    );
    view.set_search_query("no-matches".to_string());

    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert!(view.is_complete());
    match rx.try_recv() {
        Ok(AppEvent::OpenApprovalsPopup) => {}
        Ok(other) => panic!("expected OpenApprovalsPopup cancel event, got {other:?}"),
        Err(err) => panic!("expected cancel callback event, got {err}"),
    }
}

#[test]
fn move_down_without_selection_change_does_not_fire_callback() {
    let (tx_raw, mut rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = new_view(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Only choice".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            on_selection_changed: Some(Box::new(|_idx, tx: &_| {
                tx.send(AppEvent::OpenApprovalsPopup);
            })),
            ..Default::default()
        },
        tx,
    );

    while rx.try_recv().is_ok() {}

    view.handle_key_event(KeyEvent::from(KeyCode::Down));

    assert!(
        rx.try_recv().is_err(),
        "moving down in a single-item list should not fire on_selection_changed",
    );
}

#[test]
fn disabled_current_rows_skip_default_selection_and_number_shortcuts() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "Unavailable".to_string(),
                    description: Some("Not available right now.".to_string()),
                    is_current: true,
                    is_disabled: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Alpha".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Busy".to_string(),
                    description: Some("Still disabled.".to_string()),
                    disabled_reason: Some("Try again later.".to_string()),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Beta".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    assert_eq!(view.selected_actual_idx(), Some(1));

    let rendered = render_lines_with_width(&view, /*width*/ 60);
    assert!(
        rendered.contains("› 1. Alpha"),
        "expected first enabled row to be selected and numbered 1, got:\n{rendered}"
    );
    assert!(
        rendered.contains("  2. Beta"),
        "expected second enabled row to be numbered 2, got:\n{rendered}"
    );
    assert!(
        !rendered.contains("1. Unavailable") && !rendered.contains("3. Beta"),
        "expected disabled rows to be skipped by numbering, got:\n{rendered}"
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));

    assert_eq!(view.take_last_selected_index(), Some(3));
}

#[test]
fn c0_ctrl_p_respects_unbound_list_move_up() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_up.clear();
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "First".to_string(),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Second".to_string(),
                    ..Default::default()
                },
            ],
            initial_selected_idx: Some(1),
            is_searchable: true,
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('\u{0010}'), KeyModifiers::NONE));

    assert_eq!(view.selected_actual_idx(), Some(1));
    assert_eq!(view.search_query, "");
}

#[test]
fn c0_ctrl_n_respects_unbound_list_move_down() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_down.clear();
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "First".to_string(),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Second".to_string(),
                    ..Default::default()
                },
            ],
            is_searchable: true,
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('\u{000e}'), KeyModifiers::NONE));

    assert_eq!(view.selected_actual_idx(), Some(0));
    assert_eq!(view.search_query, "");
}

#[test]
fn c0_ctrl_p_respects_remapped_list_move_down() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.move_up.clear();
    keymap.move_down = vec![crate::key_hint::ctrl(KeyCode::Char('p'))];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: vec![
                SelectionItem {
                    name: "First".to_string(),
                    ..Default::default()
                },
                SelectionItem {
                    name: "Second".to_string(),
                    ..Default::default()
                },
            ],
            is_searchable: true,
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Char('\u{0010}'), KeyModifiers::NONE));

    assert_eq!(view.selected_actual_idx(), Some(1));
}

#[test]
fn page_and_jump_navigation_use_list_keymap() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut keymap = crate::keymap::RuntimeKeymap::defaults().list;
    keymap.page_down = vec![crate::key_hint::ctrl(KeyCode::Char('d'))];
    keymap.page_up = vec![crate::key_hint::ctrl(KeyCode::Char('u'))];
    keymap.jump_bottom = vec![crate::key_hint::ctrl(KeyCode::Char('e'))];
    keymap.jump_top = vec![crate::key_hint::ctrl(KeyCode::Char('a'))];
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: (0..12)
                .map(|idx| SelectionItem {
                    name: format!("Item {idx}"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        tx,
        keymap,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::PageDown));
    assert_eq!(view.selected_actual_idx(), Some(0));

    view.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(view.selected_actual_idx(), Some(8));

    view.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(view.selected_actual_idx(), Some(0));

    view.handle_key_event(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert_eq!(view.selected_actual_idx(), Some(11));

    view.handle_key_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(view.selected_actual_idx(), Some(0));
}

#[test]
fn page_and_jump_navigation_skip_trailing_disabled_rows_without_wrapping() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let mut view = ListSelectionView::new(
        SelectionViewParams {
            items: (0..12)
                .map(|idx| SelectionItem {
                    name: format!("Item {idx}"),
                    is_disabled: idx >= 8,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::PageDown));
    assert_eq!(view.selected_actual_idx(), Some(7));
    let selected = view.state.selected_idx.expect("selection should be set");
    assert!(view.state.scroll_top <= selected);
    assert!(selected < view.state.scroll_top + view.visible_rows(/*len*/ 12));

    view.handle_key_event(KeyEvent::from(KeyCode::End));
    assert_eq!(view.selected_actual_idx(), Some(7));
    let selected = view.state.selected_idx.expect("selection should be set");
    assert!(view.state.scroll_top <= selected);
    assert!(selected < view.state.scroll_top + view.visible_rows(/*len*/ 12));
}

#[test]
fn wraps_long_option_without_overflowing_columns() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![
            SelectionItem {
                name: "Yes, proceed".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Yes, and don't ask again for commands that start with `python -mpre_commit run --files eslint-plugin/no-mixed-const-enum-exports.js`".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
    let view = new_view(
        SelectionViewParams {
            title: Some("Approval".to_string()),
            items,
            ..Default::default()
        },
        tx,
    );

    let rendered = render_lines_with_width(&view, /*width*/ 60);
    let command_line = rendered
        .lines()
        .find(|line| line.contains("python -mpre_commit run"))
        .expect("rendered lines should include wrapped command");
    assert!(
        command_line.starts_with("     `python -mpre_commit run"),
        "wrapped command line should align under the numbered prefix:\n{rendered}"
    );
    assert!(
        rendered.contains("eslint-plugin/no-") && rendered.contains("mixed-const-enum-exports.js"),
        "long command should not be truncated even when wrapped:\n{rendered}"
    );
}

#[test]
fn width_changes_do_not_hide_rows() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![
        SelectionItem {
            name: "gpt-5.1-codex".to_string(),
            description: Some(
                "Optimized for Codex. Balance of reasoning quality and coding ability.".to_string(),
            ),
            is_current: true,
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "gpt-5.1-codex-mini".to_string(),
            description: Some(
                "Optimized for Codex. Cheaper, faster, but less capable.".to_string(),
            ),
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "gpt-4.1-codex".to_string(),
            description: Some(
                "Legacy model. Use when you need compatibility with older automations.".to_string(),
            ),
            dismiss_on_select: true,
            ..Default::default()
        },
    ];
    let view = new_view(
        SelectionViewParams {
            title: Some("Select Model and Effort".to_string()),
            items,
            ..Default::default()
        },
        tx,
    );
    let mut missing: Vec<u16> = Vec::new();
    for width in 60..=90 {
        let rendered = render_lines_with_width(&view, width);
        if !rendered.contains("3.") {
            missing.push(width);
        }
    }
    assert!(
        missing.is_empty(),
        "third option missing at widths {missing:?}"
    );
}

#[test]
fn narrow_width_keeps_all_rows_visible() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let desc = "x".repeat(10);
    let items: Vec<SelectionItem> = (1..=3)
        .map(|idx| SelectionItem {
            name: format!("Item {idx}"),
            description: Some(desc.clone()),
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();
    let view = new_view(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items,
            ..Default::default()
        },
        tx,
    );
    let rendered = render_lines_with_width(&view, /*width*/ 24);
    assert!(
        rendered.contains("3."),
        "third option missing for width 24:\n{rendered}"
    );
}

#[test]
fn snapshot_model_picker_width_80() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![
        SelectionItem {
            name: "gpt-5.1-codex".to_string(),
            description: Some(
                "Optimized for Codex. Balance of reasoning quality and coding ability.".to_string(),
            ),
            is_current: true,
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "gpt-5.1-codex-mini".to_string(),
            description: Some(
                "Optimized for Codex. Cheaper, faster, but less capable.".to_string(),
            ),
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "gpt-4.1-codex".to_string(),
            description: Some(
                "Legacy model. Use when you need compatibility with older automations.".to_string(),
            ),
            dismiss_on_select: true,
            ..Default::default()
        },
    ];
    let view = new_view(
        SelectionViewParams {
            title: Some("Select Model and Effort".to_string()),
            items,
            ..Default::default()
        },
        tx,
    );
    assert_snapshot!(
        "list_selection_model_picker_width_80",
        render_lines_with_width(&view, /*width*/ 80)
    );
}

#[test]
fn snapshot_narrow_width_preserves_third_option() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let desc = "x".repeat(10);
    let items: Vec<SelectionItem> = (1..=3)
        .map(|idx| SelectionItem {
            name: format!("Item {idx}"),
            description: Some(desc.clone()),
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();
    let view = new_view(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items,
            ..Default::default()
        },
        tx,
    );
    assert_snapshot!(
        "list_selection_narrow_width_preserves_rows",
        render_lines_with_width(&view, /*width*/ 24)
    );
}

#[test]
fn snapshot_narrow_width_counts_halfwidth_sound_marks() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let items = vec![
        SelectionItem {
            name: "abｶﾞc".to_string(),
            description: Some("dakuten description".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "aﾊﾟc".to_string(),
            description: Some("handakuten description".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        },
    ];
    let view = new_view(
        SelectionViewParams {
            title: Some("Halfwidth sound marks".to_string()),
            items,
            ..Default::default()
        },
        tx,
    );

    let rendered = format!(
        "width 20:\n{}\n\nwidth 24:\n{}",
        render_lines_with_width(&view, /*width*/ 20),
        render_lines_with_width(&view, /*width*/ 24)
    );
    assert_snapshot!("list_selection_halfwidth_sound_marks_narrow", rendered);
}

#[test]
fn snapshot_auto_visible_col_width_mode_scroll_behavior() {
    assert_snapshot!(
        "list_selection_col_width_mode_auto_visible_scroll",
        render_before_after_scroll_snapshot(ColumnWidthMode::AutoVisible, /*width*/ 96)
    );
}

#[test]
fn snapshot_auto_all_rows_col_width_mode_scroll_behavior() {
    assert_snapshot!(
        "list_selection_col_width_mode_auto_all_rows_scroll",
        render_before_after_scroll_snapshot(ColumnWidthMode::AutoAllRows, /*width*/ 96)
    );
}

#[test]
fn auto_all_rows_col_width_does_not_shift_when_scrolling() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);

    let mut view = ListSelectionView::new(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: make_scrolling_width_items(),
            col_width_mode: ColumnWidthMode::AutoAllRows,
            ..Default::default()
        },
        tx,
        crate::keymap::RuntimeKeymap::defaults().list,
    );

    let before_scroll = render_lines_with_width(&view, /*width*/ 96);
    for _ in 0..8 {
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    let after_scroll = render_lines_with_width(&view, /*width*/ 96);

    assert!(
        after_scroll.contains("9. Item 9 with an intentionally much longer name"),
        "expected the scrolled view to include the longer row:\n{after_scroll}"
    );

    let before_col = description_col(&before_scroll, "8. Item 8", "desc 8");
    let after_col = description_col(&after_scroll, "8. Item 8", "desc 8");
    assert_eq!(
        before_col, after_col,
        "description column changed across scroll:\nbefore:\n{before_scroll}\nafter:\n{after_scroll}"
    );
}

#[test]
fn side_layout_width_half_uses_exact_split() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Item 1".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            side_content: Box::new(MarkerRenderable {
                marker: "W",
                height: 1,
            }),
            side_content_width: SideContentWidth::Half,
            side_content_min_width: 10,
            ..Default::default()
        },
        tx,
    );

    let content_width: u16 = 120;
    let expected = content_width.saturating_sub(SIDE_CONTENT_GAP) / 2;
    assert_eq!(view.side_layout_width(content_width), Some(expected));
}

#[test]
fn side_layout_width_half_falls_back_when_list_would_be_too_narrow() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            items: vec![SelectionItem {
                name: "Item 1".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            side_content: Box::new(MarkerRenderable {
                marker: "W",
                height: 1,
            }),
            side_content_width: SideContentWidth::Half,
            side_content_min_width: 50,
            ..Default::default()
        },
        tx,
    );

    assert_eq!(view.side_layout_width(/*content_width*/ 80), None);
}

#[test]
fn stacked_side_content_is_used_when_side_by_side_does_not_fit() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: vec![SelectionItem {
                name: "Item 1".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            side_content: Box::new(MarkerRenderable {
                marker: "W",
                height: 1,
            }),
            stacked_side_content: Some(Box::new(MarkerRenderable {
                marker: "N",
                height: 1,
            })),
            side_content_width: SideContentWidth::Half,
            side_content_min_width: 60,
            ..Default::default()
        },
        tx,
    );

    let rendered = render_lines_with_width(&view, /*width*/ 70);
    assert!(
        rendered.contains('N'),
        "expected stacked marker to be rendered:\n{rendered}"
    );
    assert!(
        !rendered.contains('W'),
        "wide marker should not render in stacked mode:\n{rendered}"
    );
}

#[test]
fn side_content_clearing_resets_symbols_and_style() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: vec![SelectionItem {
                name: "Item 1".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            side_content: Box::new(MarkerRenderable {
                marker: "W",
                height: 1,
            }),
            side_content_width: SideContentWidth::Half,
            side_content_min_width: 10,
            ..Default::default()
        },
        tx,
    );

    let width = 120;
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    for y in 0..height {
        for x in 0..width {
            buf[(x, y)]
                .set_symbol("X")
                .set_style(Style::default().bg(Color::Red));
        }
    }
    view.render(area, &mut buf);

    let cell = &buf[(width - 1, 0)];
    assert_eq!(cell.symbol(), " ");
    let style = cell.style();
    assert_eq!(style.fg, Some(Color::Reset));
    assert_eq!(style.bg, Some(Color::Reset));
    assert_eq!(style.underline_color, Some(Color::Reset));

    let mut saw_marker = false;
    for y in 0..height {
        for x in 0..width {
            let cell = &buf[(x, y)];
            if cell.symbol() == "W" {
                saw_marker = true;
                assert_eq!(cell.style().bg, Some(Color::Reset));
            }
        }
    }
    assert!(
        saw_marker,
        "expected side marker renderable to draw into buffer"
    );
}

#[test]
fn side_content_clearing_handles_non_zero_buffer_origin() {
    let (tx_raw, _rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let view = new_view(
        SelectionViewParams {
            title: Some("Debug".to_string()),
            items: vec![SelectionItem {
                name: "Item 1".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            }],
            side_content: Box::new(MarkerRenderable {
                marker: "W",
                height: 1,
            }),
            side_content_width: SideContentWidth::Half,
            side_content_min_width: 10,
            ..Default::default()
        },
        tx,
    );

    let width = 120;
    let height = view.desired_height(width);
    let area = Rect::new(0, 20, width, height);
    let mut buf = Buffer::empty(area);
    for y in area.y..area.y + height {
        for x in area.x..area.x + width {
            buf[(x, y)]
                .set_symbol("X")
                .set_style(Style::default().bg(Color::Red));
        }
    }
    view.render(area, &mut buf);

    let cell = &buf[(area.x + width - 1, area.y)];
    assert_eq!(cell.symbol(), " ");
    assert_eq!(cell.style().bg, Some(Color::Reset));
}
