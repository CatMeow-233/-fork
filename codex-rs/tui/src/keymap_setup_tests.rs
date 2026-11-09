use super::picker::KEYMAP_ALL_TAB_ID;
use super::picker::KEYMAP_COMMON_TAB_ID;
use super::picker::KEYMAP_CUSTOM_TAB_ID;
use super::picker::KEYMAP_DEBUG_TAB_ID;
use super::picker::KEYMAP_UNBOUND_TAB_ID;
use super::*;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionTab;
use crate::render::renderable::Renderable;
use crate::tui::FrameRequester;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

fn app_event_sender() -> AppEventSender {
    let (tx, _rx) = unbounded_channel();
    AppEventSender::new(tx)
}

fn render_capture(view: &KeymapCaptureView, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    buf
}

fn render_debug(view: &KeymapDebugView, width: u16) -> String {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    render_buffer(&buf)
}

fn render_picker(params: SelectionViewParams, width: u16) -> String {
    let view = ListSelectionView::new(params, app_event_sender(), RuntimeKeymap::defaults().list);
    render_picker_from_view(&view, width)
}

fn render_picker_from_view(view: &ListSelectionView, width: u16) -> String {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view.render(area, &mut buf);
    render_buffer(&buf)
}

fn fast_mode_action_filter() -> KeymapActionFilter {
    KeymapActionFilter {
        fast_mode_enabled: true,
        ..Default::default()
    }
}

fn render_buffer(buf: &Buffer) -> String {
    let area = buf.area();
    (0..area.height)
        .map(|row| {
            let mut line = String::new();
            for col in 0..area.width {
                let symbol = buf[(col, row)].symbol();
                if symbol.is_empty() {
                    line.push(' ');
                } else {
                    line.push_str(symbol);
                }
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_pane() -> (BottomPane, AppEventSender, UnboundedReceiver<AppEvent>) {
    let (tx_raw, rx) = unbounded_channel::<AppEvent>();
    let tx = AppEventSender::new(tx_raw);
    let pane = BottomPane::new(BottomPaneParams {
        app_event_tx: tx.clone(),
        frame_requester: FrameRequester::test_dummy(),
        has_input_focus: true,
        enhanced_keys_supported: false,
        placeholder_text: "Ask Codex to do anything".to_string(),
        disable_paste_burst: false,
        animations_enabled: false,
        effects: Default::default(),
        skills: Some(Vec::new()),
    });
    (pane, tx, rx)
}

fn selection_tab<'a>(params: &'a SelectionViewParams, id: &str) -> &'a SelectionTab {
    params
        .tabs
        .iter()
        .find(|tab| tab.id == id)
        .expect("selection tab")
}

fn selection_item<'a>(params: &'a SelectionViewParams, name: &str) -> &'a SelectionItem {
    params
        .items
        .iter()
        .find(|item| item.name == name)
        .expect("selection item")
}

fn action_menu_rows(params: &SelectionViewParams) -> String {
    params
        .items
        .iter()
        .map(|item| {
            format!(
                "{} | {} | {}",
                item.name,
                item.description.as_deref().unwrap_or_default(),
                item.disabled_reason.as_deref().unwrap_or("enabled")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn picker_covers_every_replaceable_action() {
    let keymap = TuiKeymap::default();
    let runtime = RuntimeKeymap::for_session(&keymap, /*workspace*/ true).unwrap();
    let params = build_keymap_picker_params_with_filter(
        &runtime,
        &keymap,
        KeymapActionFilter {
            fast_mode_enabled: true,
            multi_backend_enabled: true,
            workspace_enabled: true,
        },
    );
    let all_tab = selection_tab(&params, KEYMAP_ALL_TAB_ID);

    assert!(params.items.is_empty());
    assert_eq!(all_tab.items.len(), KEYMAP_ACTIONS.len());
    assert!(
        all_tab.items.iter().all(|item| !item.dismiss_on_select),
        "keymap picker should stay open behind the action menu"
    );
    assert!(KEYMAP_ACTIONS.iter().all(|descriptor| {
        binding_slot(
            &mut TuiKeymap::default(),
            descriptor.context,
            descriptor.action,
        )
        .is_some()
    }));
    assert!(KEYMAP_ACTIONS.iter().all(|descriptor| {
        bindings_for_action(&runtime, descriptor.context, descriptor.action).is_some()
    }));
}

#[test]
fn picker_hides_fast_mode_action_when_feature_is_disabled() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let all_tab = selection_tab(&params, KEYMAP_ALL_TAB_ID);

    assert!(
        all_tab
            .items
            .iter()
            .all(|item| item.name != "Toggle Fast Mode")
    );
}

#[test]
fn picker_shows_fast_mode_action_when_feature_is_enabled() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params_with_filter(
        &runtime,
        &TuiKeymap::default(),
        fast_mode_action_filter(),
    );
    let all_tab = selection_tab(&params, KEYMAP_ALL_TAB_ID);
    let common_tab = selection_tab(&params, KEYMAP_COMMON_TAB_ID);
    let app_tab = selection_tab(&params, "app-shortcuts");
    let unbound_tab = selection_tab(&params, KEYMAP_UNBOUND_TAB_ID);

    for tab in [all_tab, common_tab, app_tab, unbound_tab] {
        assert!(
            tab.items.iter().any(|item| item.name == "Toggle Fast Mode"),
            "expected Toggle Fast Mode in {}",
            tab.label
        );
    }
}

#[test]
fn keymap_picker_fast_mode_enabled_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params_with_filter(
        &runtime,
        &TuiKeymap::default(),
        fast_mode_action_filter(),
    );

    assert_snapshot!(
        "keymap_picker_fast_mode_enabled",
        render_picker(params, /*width*/ 120)
    );
}

#[test]
fn picker_common_tab_lists_curated_actions() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let common_tab = selection_tab(&params, KEYMAP_COMMON_TAB_ID);
    let actions = common_tab
        .items
        .iter()
        .map(|item| {
            item.search_value
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actions,
        vec![
            "Composer.submit",
            "Chat.interrupt_turn",
            "Editor.insert_newline",
            "Composer.queue",
            "Global.open_external_editor",
            "Global.copy",
            "Global.toggle_vim_mode",
            "Editor.delete_backward_word",
            "Editor.delete_forward_word",
            "Editor.move_word_left",
            "Editor.move_word_right",
            "Global.open_transcript",
            "Pager.close",
            "Pager.page_up",
            "Pager.page_down",
            "Approval.open_fullscreen",
            "Approval.approve",
            "Approval.approve_for_session",
            "Approval.decline",
            "Approval.cancel",
        ]
    );
}

#[test]
fn picker_approval_tab_lists_all_approval_actions() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let approval_tab = selection_tab(&params, "approval-shortcuts");
    let actions = approval_tab
        .items
        .iter()
        .map(|item| {
            item.search_value
                .as_deref()
                .unwrap_or_default()
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(".")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        vec![
            "Approval.open_fullscreen",
            "Approval.open_thread",
            "Approval.approve",
            "Approval.approve_for_session",
            "Approval.approve_for_prefix",
            "Approval.deny",
            "Approval.decline",
            "Approval.cancel",
        ]
    );
}

#[test]
fn picker_content_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let all_tab = selection_tab(&params, KEYMAP_ALL_TAB_ID);
    let snapshot = params
        .tabs
        .iter()
        .map(|tab| {
            let selectable = tab.items.iter().filter(|item| !item.is_disabled).count();
            format!("tab: {} ({selectable} selectable)", tab.label)
        })
        .chain(all_tab.items.iter().take(13).map(|item| {
            format!(
                "{} | {} | {}",
                item.name,
                item.description.as_deref().unwrap_or_default(),
                item.search_value.as_deref().unwrap_or_default()
            )
        }))
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!("keymap_picker_first_actions", snapshot);
    let agents = all_tab
        .items
        .iter()
        .filter(|item| {
            item.search_value
                .as_deref()
                .unwrap_or_default()
                .starts_with("Agents ")
        })
        .map(|item| {
            format!(
                "{} | {} | {}",
                item.name,
                item.description.as_deref().unwrap_or_default(),
                item.search_value.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_snapshot!("keymap_picker_agents_actions", agents);
}

#[test]
fn picker_customized_tab_contains_root_overrides() {
    let keymap = keymap_with_replacement(&TuiKeymap::default(), "composer", "submit", "ctrl-enter")
        .expect("replace binding");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    let params = build_keymap_picker_params(&runtime, &keymap);
    let custom_tab = selection_tab(&params, KEYMAP_CUSTOM_TAB_ID);
    let composer_tab = selection_tab(&params, "composer-shortcuts");

    assert_eq!(
        custom_tab
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Submit"]
    );
    assert!(
        composer_tab
            .items
            .iter()
            .any(|item| item.description.as_deref() == Some("ctrl-enter"))
    );
}

#[test]
fn picker_unbound_tab_lists_default_unbound_actions() {
    let runtime = RuntimeKeymap::defaults();
    for (context, action) in [
        ("vim_normal", "jump_top"),
        ("vim_operator", "motion_jump_top"),
    ] {
        assert_eq!(
            active_binding_specs(&runtime, context, action).expect("native Vim chord"),
            ["g g"]
        );
    }
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let unbound_tab = selection_tab(&params, KEYMAP_UNBOUND_TAB_ID);

    assert_eq!(
        unbound_tab
            .items
            .iter()
            .map(|item| (
                item.name.as_str(),
                item.description.as_deref(),
                item.is_disabled
            ))
            .collect::<Vec<_>>(),
        vec![
            ("Open Agents", Some("unbound"), false),
            ("Toggle Vim Mode", Some("unbound"), false),
            ("Previous Permission Mode", Some("unbound"), false),
            ("Next Permission Mode", Some("unbound"), false),
            ("Kill Whole Line", Some("unbound"), false),
        ]
    );
}

#[test]
fn picker_debug_tab_is_last_and_opens_inspector() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let debug_tab = params.tabs.last().expect("debug tab");

    assert_eq!(debug_tab.id, KEYMAP_DEBUG_TAB_ID);
    assert_eq!(debug_tab.label, "Debug");
    assert_eq!(debug_tab.items.len(), 1);
    assert_eq!(debug_tab.items[0].name, "Inspect keypresses");
    assert_eq!(
        debug_tab.items[0].description.as_deref(),
        Some("Press Enter, then any key to inspect it (Ctrl+C exits)")
    );
    assert!(
        params
            .tab_footer_hints
            .iter()
            .any(|(tab_id, _)| tab_id == KEYMAP_DEBUG_TAB_ID)
    );
}

#[test]
fn picker_selected_action_starts_on_matching_all_tab_row() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params_for_selected_action(
        &runtime,
        &TuiKeymap::default(),
        "composer",
        "submit",
    );
    let all_tab = selection_tab(&params, KEYMAP_ALL_TAB_ID);

    assert_eq!(params.initial_tab_id.as_deref(), Some(KEYMAP_ALL_TAB_ID));
    assert_eq!(
        params.initial_selected_idx,
        all_tab.items.iter().position(|item| item.name == "Submit")
    );
}

#[test]
fn picker_repeat_and_redo_render_snapshots() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params_for_selected_action(
        &runtime,
        &TuiKeymap::default(),
        "vim_normal",
        "repeat_last_change",
    );
    let rendered = render_picker(params, /*width*/ 120);
    let repeat_row = rendered
        .lines()
        .find(|line| line.contains("Repeat Last Change"))
        .expect("repeat-last-change row should render");

    assert_snapshot!("keymap_picker_repeat_last_change", repeat_row);

    let params = build_keymap_picker_params_for_selected_action(
        &runtime,
        &TuiKeymap::default(),
        "vim_normal",
        "redo",
    );
    let rendered = render_picker(params, /*width*/ 120);
    let redo_row = rendered
        .lines()
        .find(|line| line.contains("Redo"))
        .expect("redo row should render");

    assert_snapshot!("keymap_picker_redo", redo_row);
}

#[test]
fn picker_voice_toggle_snapshot() {
    assert_snapshot!(
        "keymap_voice_toggle",
        render_picker(
            build_keymap_action_menu_params(
                "chat".into(),
                "toggle_voice".into(),
                &RuntimeKeymap::defaults(),
                &TuiKeymap::default(),
            ),
            /*width*/ 80,
        )
    );
}

#[test]
fn picker_question_actions_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let descriptions = ["edit_queued_message", "prompt_stack_back", "skip_question"]
        .map(|action| {
            render_picker(
                build_keymap_action_menu_params(
                    "chat".into(),
                    action.into(),
                    &runtime,
                    &TuiKeymap::default(),
                ),
                /*width*/ 120,
            )
            .split("\n\n")
            .next()
            .unwrap()
            .to_string()
        })
        .join("\n\n");
    assert_snapshot!("keymap_question_action_descriptions", descriptions);
}

#[test]
fn picker_all_tab_items_remain_searchable() {
    let keymap = TuiKeymap::default();
    let runtime = RuntimeKeymap::for_session(&keymap, /*workspace*/ true).unwrap();
    let params = build_keymap_picker_params_with_filter(
        &runtime,
        &keymap,
        KeymapActionFilter {
            workspace_enabled: true,
            ..Default::default()
        },
    );
    let all_tab = selection_tab(&params, KEYMAP_ALL_TAB_ID);
    let snapshot = all_tab
        .items
        .iter()
        .take(13)
        .map(|item| {
            format!(
                "{} | {} | {}",
                item.name,
                item.description.as_deref().unwrap_or_default(),
                item.search_value.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!("keymap_picker_all_tab_search", snapshot);
}

#[test]
fn picker_wide_render_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());

    assert_snapshot!("keymap_picker_wide", render_picker(params, /*width*/ 120));
}

#[test]
fn picker_narrow_render_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());

    assert_snapshot!("keymap_picker_narrow", render_picker(params, /*width*/ 78));
}

#[test]
fn picker_editor_render_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let mut params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    params.initial_tab_id = Some("editor-shortcuts".to_string());

    assert_snapshot!("keymap_picker_editor", render_picker(params, /*width*/ 120));
}

#[test]
fn picker_custom_render_snapshot() {
    let keymap = keymap_with_replacement(&TuiKeymap::default(), "composer", "submit", "ctrl-enter")
        .expect("replace binding");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    let params = build_keymap_picker_params(&runtime, &keymap);

    assert_snapshot!("keymap_picker_custom", render_picker(params, /*width*/ 120));
}

#[test]
fn picker_global_chord_render_snapshot() {
    let keymap = keymap_with_bindings(
        &TuiKeymap::default(),
        "global",
        "open_transcript",
        &["ctrl-t".to_string(), "ctrl-x ctrl-t".to_string()],
    )
    .expect("global chord binding");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    let params = build_keymap_picker_params(&runtime, &keymap);

    assert_snapshot!(
        "keymap_picker_global_chord",
        render_picker(params, /*width*/ 120)
    );
}

#[test]
fn configured_global_chord_alternatives_remain_in_order() {
    let keymap = keymap_with_bindings(
        &TuiKeymap::default(),
        "global",
        "open_transcript",
        &["ctrl-t".to_string(), "ctrl-x ctrl-t".to_string()],
    )
    .expect("global chord bindings");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");

    assert_eq!(
        active_binding_specs(&runtime, "global", "open_transcript"),
        Ok(vec!["ctrl-t".to_string(), "ctrl-x ctrl-t".to_string()])
    );
    assert_eq!(
        format_action_binding_summary(&runtime, "global", "open_transcript"),
        "ctrl-t, ctrl-x ctrl-t"
    );
}

#[test]
fn picker_narrow_uses_compact_tabs() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params(&runtime, &TuiKeymap::default());
    let rendered = render_picker(params, /*width*/ 78);

    assert!(rendered.contains("Keymap"));
    assert!(rendered.contains("Open Transcript"));
    assert!(rendered.contains("ctrl-t"));
    assert!(!rendered.contains("Selected Action"));
    assert!(!rendered.contains("Source: default keymap"));
}

#[test]
fn action_menu_content_snapshot() {
    let unbound_keymap =
        keymap_with_bindings(&TuiKeymap::default(), "global", "copy", &[]).expect("unbound copy");
    let unbound_runtime = RuntimeKeymap::from_config(&unbound_keymap).expect("runtime keymap");
    let unbound_params = build_keymap_action_menu_params(
        "global".to_string(),
        "copy".to_string(),
        &unbound_runtime,
        &unbound_keymap,
    );

    let single_keymap =
        keymap_with_replacement(&TuiKeymap::default(), "composer", "submit", "ctrl-enter")
            .expect("replace binding");
    let single_runtime = RuntimeKeymap::from_config(&single_keymap).expect("runtime keymap");
    let single_params = build_keymap_action_menu_params(
        "composer".to_string(),
        "submit".to_string(),
        &single_runtime,
        &single_keymap,
    );

    let multi_keymap = keymap_with_bindings(
        &TuiKeymap::default(),
        "composer",
        "submit",
        &["ctrl-enter".to_string(), "alt-shift-enter".to_string()],
    )
    .expect("multi binding");
    let multi_runtime = RuntimeKeymap::from_config(&multi_keymap).expect("runtime keymap");
    let multi_params = build_keymap_action_menu_params(
        "composer".to_string(),
        "submit".to_string(),
        &multi_runtime,
        &multi_keymap,
    );
    let replace_params = build_keymap_replace_binding_menu_params(
        "composer".to_string(),
        "submit".to_string(),
        &multi_runtime,
    );
    let snapshot = [
        "unbound:",
        &action_menu_rows(&unbound_params),
        "",
        "single:",
        &action_menu_rows(&single_params),
        "",
        "multi:",
        &action_menu_rows(&multi_params),
        "",
        "replace picker:",
        &action_menu_rows(&replace_params),
    ]
    .join("\n");

    assert_snapshot!("keymap_action_menu", snapshot);

    for params in [unbound_params, single_params, multi_params, replace_params] {
        let item_count = params.items.len();
        let mut view =
            ListSelectionView::new(params, app_event_sender(), RuntimeKeymap::defaults().list);
        let initial = render_picker_from_view(&view, /*width*/ 160).replace('›', " ");
        for _ in 1..item_count {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
            assert_eq!(
                render_picker_from_view(&view, /*width*/ 160).replace('›', " "),
                initial,
            );
        }
    }
}

#[test]
fn action_menu_responsive_render_snapshot() {
    let runtime = RuntimeKeymap::defaults();
    let render_at = |width| {
        let params = build_keymap_action_menu_params(
            "global".to_string(),
            "open_transcript".to_string(),
            &runtime,
            &TuiKeymap::default(),
        );
        render_picker(params, width)
    };
    let snapshot = [
        "48 columns:",
        &render_at(/*width*/ 48),
        "",
        "64 columns:",
        &render_at(/*width*/ 64),
        "",
        "96 columns:",
        &render_at(/*width*/ 96),
    ]
    .join("\n");

    assert_snapshot!("keymap_action_menu_responsive", snapshot);
}

#[test]
fn action_menu_disables_clear_when_action_has_no_custom_binding() {
    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_action_menu_params(
        "composer".to_string(),
        "submit".to_string(),
        &runtime,
        &TuiKeymap::default(),
    );

    assert_eq!(params.view_id, Some(KEYMAP_ACTION_MENU_VIEW_ID));
    let replace = selection_item(&params, "Replace binding");
    let add_alternate = selection_item(&params, "Add alternate binding");
    let remove = selection_item(&params, "Remove custom binding");
    let back = selection_item(&params, "Back to shortcuts");
    assert_eq!(
        remove.disabled_reason.as_deref(),
        Some("No custom root override to remove")
    );
    assert!(
        !replace.dismiss_on_select,
        "replace should keep the action menu under key capture"
    );
    assert!(
        !add_alternate.dismiss_on_select,
        "add alternate should keep the action menu under key capture"
    );
    assert!(!remove.dismiss_on_select, "clear-key waits for save result");
    assert!(
        back.dismiss_on_select,
        "back should dismiss the action menu"
    );
}

#[test]
fn capture_view_snapshot() {
    let view = KeymapCaptureView::new(
        "composer".to_string(),
        "submit".to_string(),
        KeymapEditIntent::ReplaceAll,
        KeymapCaptureMode::SingleKey,
        "Submit".to_string(),
        "enter".to_string(),
        app_event_sender(),
    );

    assert_snapshot!(
        "keymap_capture_view",
        format!("{:?}", render_capture(&view, /*width*/ 80, /*height*/ 8))
    );
}

#[test]
fn debug_view_initial_snapshot() {
    let view = build_keymap_debug_view(&RuntimeKeymap::defaults(), &TuiKeymap::default());

    assert_snapshot!(
        "keymap_debug_view_initial",
        render_debug(&view, /*width*/ 80)
    );
}

#[test]
fn debug_view_shows_delayed_missing_key_hint() {
    let mut view = build_keymap_debug_view(&RuntimeKeymap::defaults(), &TuiKeymap::default());
    view.show_delayed_hint_for_test();

    let rendered = render_debug(&view, /*width*/ 100);
    assert!(rendered.contains("Still waiting?"));
    assert_snapshot!("keymap_debug_view_delayed_hint", rendered);
}

#[test]
fn debug_view_reports_detected_key_and_matching_actions() {
    let mut view = build_keymap_debug_view(&RuntimeKeymap::defaults(), &TuiKeymap::default());
    view.show_delayed_hint_for_test();

    view.handle_key_event(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));

    let rendered = render_debug(&view, /*width*/ 100);
    assert!(!rendered.contains("Still waiting?"));
    assert_snapshot!("keymap_debug_view_match", rendered);
}

#[test]
fn debug_view_uses_custom_binding_source() {
    let keymap =
        keymap_with_replacement(&TuiKeymap::default(), "global", "copy", "ctrl-x").unwrap();
    let runtime = RuntimeKeymap::from_config(&keymap).unwrap();
    let mut view = build_keymap_debug_view(&runtime, &keymap);

    view.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));

    let rendered = render_debug(&view, /*width*/ 100);
    assert!(rendered.contains("global.copy (Copy)"));
    assert!(rendered.contains("[Custom]"));
}

#[test]
fn debug_view_labels_custom_global_fallback_source() {
    let mut keymap = TuiKeymap::default();
    keymap.global.queue = Some(KeybindingsSpec::One(KeybindingSpec("ctrl-q".to_string())));
    let runtime = RuntimeKeymap::from_config(&keymap).unwrap();
    let mut view = build_keymap_debug_view(&runtime, &keymap);

    view.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

    let rendered = render_debug(&view, /*width*/ 100);
    assert!(rendered.contains("composer.queue (Queue)"));
    assert!(rendered.contains("[Custom global]"));
}

#[test]
fn capture_completion_returns_to_selected_keymap_picker_row() {
    let (mut pane, tx, mut rx) = test_pane();
    let runtime = RuntimeKeymap::defaults();
    pane.show_selection_view(build_keymap_picker_params(&runtime, &TuiKeymap::default()));
    pane.show_selection_view(build_keymap_action_menu_params(
        "composer".to_string(),
        "submit".to_string(),
        &runtime,
        &TuiKeymap::default(),
    ));

    pane.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let AppEvent::OpenKeymapCapture {
        context,
        action,
        intent,
        capture_mode,
    } = rx.try_recv().expect("open capture event")
    else {
        panic!("expected OpenKeymapCapture event");
    };
    assert_eq!(intent, KeymapEditIntent::ReplaceAll);
    assert_eq!(capture_mode, KeymapCaptureMode::SingleKey);
    assert_eq!(pane.active_view_id(), Some(KEYMAP_ACTION_MENU_VIEW_ID));

    pane.show_view(Box::new(build_keymap_capture_view(
        context,
        action,
        intent,
        capture_mode,
        &runtime,
        tx,
    )));
    pane.handle_key_event(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::CONTROL));

    let AppEvent::KeymapCaptured {
        context,
        action,
        key,
        intent,
    } = rx.try_recv().expect("captured key event")
    else {
        panic!("expected KeymapCaptured event");
    };
    assert_eq!(context, "composer");
    assert_eq!(action, "submit");
    assert_eq!(key, "ctrl-shift-k");
    assert_eq!(intent, KeymapEditIntent::ReplaceAll);
    assert_eq!(pane.active_view_id(), Some(KEYMAP_ACTION_MENU_VIEW_ID));

    let keymap = keymap_with_replacement(&TuiKeymap::default(), &context, &action, &key).unwrap();
    let runtime = RuntimeKeymap::from_config(&keymap).unwrap();
    let params =
        build_keymap_picker_params_for_selected_action(&runtime, &keymap, &context, &action);
    let selected_idx = params.initial_selected_idx;
    assert!(
        pane.replace_active_views_with_selection_view(
            &[
                KEYMAP_PICKER_VIEW_ID,
                KEYMAP_ACTION_MENU_VIEW_ID,
                KEYMAP_REPLACE_BINDING_MENU_VIEW_ID,
            ],
            params
        ),
        "successful assignment should return to the main picker"
    );
    assert_eq!(pane.active_view_id(), Some(KEYMAP_PICKER_VIEW_ID));
    assert_eq!(
        pane.selected_index_for_active_view(KEYMAP_PICKER_VIEW_ID),
        selected_idx
    );

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        pane.no_modal_or_popup_active(),
        "the original picker should not remain behind the refreshed picker"
    );
}

#[test]
fn clear_completion_returns_to_selected_keymap_picker_row() {
    let (mut pane, _tx, mut rx) = test_pane();
    let keymap =
        keymap_with_replacement(&TuiKeymap::default(), "composer", "submit", "ctrl-enter").unwrap();
    let runtime = RuntimeKeymap::from_config(&keymap).unwrap();
    pane.show_selection_view(build_keymap_picker_params(&runtime, &keymap));
    let action_menu = build_keymap_action_menu_params(
        "composer".to_string(),
        "submit".to_string(),
        &runtime,
        &keymap,
    );
    let remove_idx = action_menu
        .items
        .iter()
        .position(|item| item.name == "Remove custom binding")
        .expect("remove custom binding menu item");
    pane.show_selection_view(action_menu);

    for _ in 0..remove_idx {
        pane.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    pane.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let AppEvent::KeymapCleared { context, action } = rx.try_recv().expect("clear keymap event")
    else {
        panic!("expected KeymapCleared event");
    };
    assert_eq!(context, "composer");
    assert_eq!(action, "submit");
    assert_eq!(pane.active_view_id(), Some(KEYMAP_ACTION_MENU_VIEW_ID));

    let runtime = RuntimeKeymap::defaults();
    let params = build_keymap_picker_params_for_selected_action(
        &runtime,
        &TuiKeymap::default(),
        &context,
        &action,
    );
    let selected_idx = params.initial_selected_idx;
    assert!(
        pane.replace_active_views_with_selection_view(
            &[
                KEYMAP_PICKER_VIEW_ID,
                KEYMAP_ACTION_MENU_VIEW_ID,
                KEYMAP_REPLACE_BINDING_MENU_VIEW_ID,
            ],
            params
        ),
        "successful clear should return to the main picker"
    );
    assert_eq!(pane.active_view_id(), Some(KEYMAP_PICKER_VIEW_ID));
    assert_eq!(
        pane.selected_index_for_active_view(KEYMAP_PICKER_VIEW_ID),
        selected_idx
    );

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        pane.no_modal_or_popup_active(),
        "the original picker should not remain behind the refreshed picker"
    );
}

#[test]
fn replace_one_completion_drops_focused_keymap_submenus() {
    let (mut pane, _tx, _rx) = test_pane();
    let runtime = RuntimeKeymap::defaults();
    pane.show_selection_view(build_keymap_picker_params(&runtime, &TuiKeymap::default()));
    pane.show_selection_view(build_keymap_action_menu_params(
        "composer".to_string(),
        "toggle_shortcuts".to_string(),
        &runtime,
        &TuiKeymap::default(),
    ));
    pane.show_selection_view(build_keymap_replace_binding_menu_params(
        "composer".to_string(),
        "toggle_shortcuts".to_string(),
        &runtime,
    ));
    assert_eq!(
        pane.active_view_id(),
        Some(KEYMAP_REPLACE_BINDING_MENU_VIEW_ID)
    );

    let params = build_keymap_picker_params_for_selected_action(
        &runtime,
        &TuiKeymap::default(),
        "composer",
        "toggle_shortcuts",
    );
    assert!(
        pane.replace_active_views_with_selection_view(
            &[
                KEYMAP_PICKER_VIEW_ID,
                KEYMAP_ACTION_MENU_VIEW_ID,
                KEYMAP_REPLACE_BINDING_MENU_VIEW_ID,
            ],
            params
        ),
        "successful replace-one should return to the main picker"
    );
    assert_eq!(pane.active_view_id(), Some(KEYMAP_PICKER_VIEW_ID));

    pane.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        pane.no_modal_or_popup_active(),
        "the parent action menu should not remain behind the picker"
    );
}

#[test]
fn key_capture_serializes_modifier_order_for_config() {
    let event = KeyEvent::new(
        KeyCode::Char('K'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
    );

    assert_eq!(
        key_event_to_config_key_spec(event),
        Ok("ctrl-alt-shift-super-k".to_string())
    );
}

#[test]
fn key_capture_serializes_special_keys() {
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::PageDown, KeyModifiers::SHIFT)),
        Ok("shift-page-down".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::from(KeyCode::BackTab)),
        Ok("shift-tab".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
        Ok("shift-tab".to_string())
    );
}

#[test]
fn key_capture_serializes_function_keys_through_f24() {
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::from(KeyCode::F(13))),
        Ok("f13".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::from(KeyCode::F(24))),
        Ok("f24".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::from(KeyCode::F(25))),
        Err("Only function keys F1 through F24 can be stored in `tui.keymap`.".to_string())
    );
}

#[test]
fn key_capture_serializes_c0_control_chars_as_ctrl_bindings() {
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::Char('\u{000a}'), KeyModifiers::NONE,)),
        Ok("ctrl-j".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::Char('\u{0015}'), KeyModifiers::NONE,)),
        Ok("ctrl-u".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::Char('\u{0010}'), KeyModifiers::NONE,)),
        Ok("ctrl-p".to_string())
    );
}

#[test]
fn key_capture_serializes_minus_as_named_key() {
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE)),
        Ok("minus".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::ALT)),
        Ok("alt-minus".to_string())
    );
    assert_eq!(
        key_event_to_config_key_spec(KeyEvent::new(
            KeyCode::Char('-'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        )),
        Ok("ctrl-alt-minus".to_string())
    );
}

#[test]
fn replacement_sets_single_binding() {
    let keymap = keymap_with_replacement(&TuiKeymap::default(), "composer", "submit", "ctrl-enter")
        .expect("replace binding");

    assert_eq!(
        keymap.composer.submit,
        Some(KeybindingsSpec::One(KeybindingSpec(
            "ctrl-enter".to_string()
        )))
    );
}

#[test]
fn replace_all_collapses_multi_binding_to_single() {
    let keymap = keymap_with_bindings(
        &TuiKeymap::default(),
        "composer",
        "submit",
        &["ctrl-enter".to_string(), "alt-shift-enter".to_string()],
    )
    .expect("multi binding");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    let outcome = keymap_with_edit(
        &keymap,
        &runtime,
        "composer",
        "submit",
        "ctrl-shift-enter",
        &KeymapEditIntent::ReplaceAll,
    )
    .expect("replace all");

    let KeymapEditOutcome::Updated {
        keymap_config,
        bindings,
        ..
    } = outcome
    else {
        panic!("expected updated keymap");
    };
    assert_eq!(bindings, vec!["ctrl-shift-enter"]);
    assert_eq!(
        keymap_config.composer.submit,
        Some(KeybindingsSpec::One(KeybindingSpec(
            "ctrl-shift-enter".to_string()
        )))
    );
}

#[test]
fn add_alternate_grows_single_binding() {
    let runtime = RuntimeKeymap::defaults();
    let outcome = keymap_with_edit(
        &TuiKeymap::default(),
        &runtime,
        "composer",
        "submit",
        "ctrl-enter",
        &KeymapEditIntent::AddAlternate,
    )
    .expect("add alternate");

    let KeymapEditOutcome::Updated {
        keymap_config,
        bindings,
        ..
    } = outcome
    else {
        panic!("expected updated keymap");
    };
    assert_eq!(bindings, vec!["enter", "ctrl-enter"]);
    assert_eq!(
        keymap_config.composer.submit,
        Some(KeybindingsSpec::Many(vec![
            KeybindingSpec("enter".to_string()),
            KeybindingSpec("ctrl-enter".to_string())
        ]))
    );
}

#[test]
fn add_alternate_grows_default_multi_binding() {
    let runtime = RuntimeKeymap::defaults();
    let outcome = keymap_with_edit(
        &TuiKeymap::default(),
        &runtime,
        "editor",
        "move_left",
        "ctrl-shift-b",
        &KeymapEditIntent::AddAlternate,
    )
    .expect("add alternate");

    let KeymapEditOutcome::Updated {
        keymap_config,
        bindings,
        ..
    } = outcome
    else {
        panic!("expected updated keymap");
    };
    assert_eq!(bindings, vec!["left", "ctrl-b", "ctrl-shift-b"]);
    assert_eq!(
        keymap_config.editor.move_left,
        Some(KeybindingsSpec::Many(vec![
            KeybindingSpec("left".to_string()),
            KeybindingSpec("ctrl-b".to_string()),
            KeybindingSpec("ctrl-shift-b".to_string())
        ]))
    );
}

#[test]
fn add_alternate_preserves_super_default_binding() {
    let runtime = RuntimeKeymap::defaults();
    let outcome = keymap_with_edit(
        &TuiKeymap::default(),
        &runtime,
        "editor",
        "move_line_start",
        "f12",
        &KeymapEditIntent::AddAlternate,
    )
    .expect("add alternate");

    let KeymapEditOutcome::Updated { bindings, .. } = outcome else {
        panic!("expected updated keymap");
    };
    assert_eq!(bindings, vec!["home", "ctrl-a", "super-left", "f12"]);
}

#[test]
fn add_alternate_duplicate_is_noop() {
    let runtime = RuntimeKeymap::defaults();
    let outcome = keymap_with_edit(
        &TuiKeymap::default(),
        &runtime,
        "composer",
        "submit",
        "enter",
        &KeymapEditIntent::AddAlternate,
    )
    .expect("duplicate alternate");

    assert_eq!(
        outcome,
        KeymapEditOutcome::Unchanged {
            message: "No change: `composer.submit` already uses `enter`.".to_string()
        }
    );
}

#[test]
fn replace_one_preserves_other_bindings() {
    let keymap = keymap_with_bindings(
        &TuiKeymap::default(),
        "composer",
        "submit",
        &["ctrl-enter".to_string(), "alt-shift-enter".to_string()],
    )
    .expect("multi binding");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    let outcome = keymap_with_edit(
        &keymap,
        &runtime,
        "composer",
        "submit",
        "ctrl-shift-enter",
        &KeymapEditIntent::ReplaceOne {
            old_key: "ctrl-enter".to_string(),
        },
    )
    .expect("replace one");

    let KeymapEditOutcome::Updated {
        keymap_config,
        bindings,
        ..
    } = outcome
    else {
        panic!("expected updated keymap");
    };
    assert_eq!(bindings, vec!["ctrl-shift-enter", "alt-shift-enter"]);
    assert_eq!(
        keymap_config.composer.submit,
        Some(KeybindingsSpec::Many(vec![
            KeybindingSpec("ctrl-shift-enter".to_string()),
            KeybindingSpec("alt-shift-enter".to_string())
        ]))
    );
}

#[test]
fn replace_one_deduplicates_replacement() {
    let keymap = keymap_with_bindings(
        &TuiKeymap::default(),
        "composer",
        "submit",
        &["ctrl-enter".to_string(), "ctrl-shift-enter".to_string()],
    )
    .expect("multi binding");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    let outcome = keymap_with_edit(
        &keymap,
        &runtime,
        "composer",
        "submit",
        "ctrl-shift-enter",
        &KeymapEditIntent::ReplaceOne {
            old_key: "ctrl-enter".to_string(),
        },
    )
    .expect("replace one");

    let KeymapEditOutcome::Updated {
        keymap_config,
        bindings,
        ..
    } = outcome
    else {
        panic!("expected updated keymap");
    };
    assert_eq!(bindings, vec!["ctrl-shift-enter"]);
    assert_eq!(
        keymap_config.composer.submit,
        Some(KeybindingsSpec::One(KeybindingSpec(
            "ctrl-shift-enter".to_string()
        )))
    );
}

#[test]
fn replace_one_rejects_stale_old_key() {
    let runtime = RuntimeKeymap::defaults();
    let err = keymap_with_edit(
        &TuiKeymap::default(),
        &runtime,
        "composer",
        "submit",
        "ctrl-enter",
        &KeymapEditIntent::ReplaceOne {
            old_key: "alt-enter".to_string(),
        },
    )
    .expect_err("stale old key");

    assert!(err.contains("composer.submit"));
    assert!(err.contains("alt-enter"));
}

#[test]
fn clear_removes_custom_binding() {
    let keymap = keymap_with_replacement(&TuiKeymap::default(), "composer", "submit", "ctrl-enter")
        .expect("replace binding");

    assert_eq!(has_custom_binding(&keymap, "composer", "submit"), Ok(true));

    let cleared =
        keymap_without_custom_binding(&keymap, "composer", "submit").expect("clear binding");

    assert_eq!(cleared.composer.submit, None);
    assert_eq!(
        has_custom_binding(&cleared, "composer", "submit"),
        Ok(false)
    );
}

#[test]
fn replacement_rejects_unknown_action() {
    let err = keymap_with_replacement(&TuiKeymap::default(), "composer", "nope", "ctrl-enter")
        .expect_err("unknown action");

    assert!(err.contains("composer.nope"));
}
