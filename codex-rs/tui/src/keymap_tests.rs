use super::*;
use codex_config::types::KeybindingSpec;

fn one(spec: &str) -> KeybindingsSpec {
    KeybindingsSpec::One(KeybindingSpec(spec.to_string()))
}

fn expect_conflict(keymap: &TuiKeymap, first: &str, second: &str) {
    let err = RuntimeKeymap::from_config(keymap).expect_err("expected conflict");
    assert!(err.contains(first));
    assert!(err.contains(second));
}

#[test]
fn parses_canonical_binding() {
    let binding = parse_keybinding("ctrl-alt-shift-super-a").expect("binding should parse");
    assert_eq!(binding.parts().0, KeyCode::Char('a'));
    assert_eq!(
        binding.parts().1,
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER
    );
}

#[test]
fn rejects_shadowing_composer_binding_in_app_scope() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("ctrl-t"));
    keymap.composer.submit = Some(one("ctrl-t"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("composer.submit"));
    assert!(err.contains("open_transcript"));
}

#[test]
fn rejects_shadowing_composer_queue_in_app_scope() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_external_editor = Some(one("ctrl-g"));
    keymap.composer.queue = Some(one("ctrl-g"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("composer.queue"));
    assert!(err.contains("open_external_editor"));
}

#[test]
fn rejects_shadowing_composer_toggle_shortcuts_in_app_scope() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("ctrl-k"));
    keymap.composer.toggle_shortcuts = Some(one("ctrl-k"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("composer.toggle_shortcuts"));
    assert!(err.contains("open_transcript"));
}

#[test]
fn rejects_shadowing_editor_binding_in_main_scope() {
    let mut keymap = TuiKeymap::default();
    keymap.composer.submit = Some(one("ctrl-j"));
    keymap.editor.insert_newline = Some(one("ctrl-j"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("composer.submit"));
    assert!(err.contains("editor.insert_newline"));
}

#[test]
fn rejects_shadowing_editor_binding_from_outer_main_handler() {
    let mut keymap = TuiKeymap::default();
    keymap.global.copy = Some(one("ctrl-y"));
    keymap.editor.yank = Some(one("ctrl-y"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("copy"));
    assert!(err.contains("editor.yank"));
}

#[test]
fn rejects_shadowing_approval_binding_in_app_scope() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("y"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("approval.approve"));
    assert!(err.contains("open_transcript"));
}

#[test]
fn rejects_shadowing_list_binding_in_app_scope() {
    let mut keymap = TuiKeymap::default();
    keymap.global.copy = Some(one("down"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected shadowing conflict");
    assert!(err.contains("list.move_down"));
    assert!(err.contains("copy"));
}

#[test]
fn supports_string_or_array_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.composer.submit = Some(KeybindingsSpec::Many(vec![
        KeybindingSpec("ctrl-enter".to_string()),
        KeybindingSpec("meta-enter".to_string()),
    ]));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("meta is not a valid modifier");
    assert!(err.contains("tui.keymap.composer.submit"));

    keymap.composer.submit = Some(KeybindingsSpec::Many(vec![
        KeybindingSpec("ctrl-enter".to_string()),
        KeybindingSpec("ctrl-shift-enter".to_string()),
    ]));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("valid multi-binding");
    assert_eq!(runtime.composer.submit.len(), 2);
}

#[test]
fn deduplicates_repeated_bindings_while_preserving_first_seen_order() {
    let mut keymap = TuiKeymap::default();
    keymap.composer.submit = Some(KeybindingsSpec::Many(vec![
        KeybindingSpec("ctrl-enter".to_string()),
        KeybindingSpec("ctrl-enter".to_string()),
        KeybindingSpec("ctrl-shift-enter".to_string()),
    ]));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("valid multi-binding");
    assert_eq!(
        runtime.composer.submit,
        vec![
            key_hint::ctrl(KeyCode::Enter),
            KeyBinding::new(KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        ]
    );
}

#[test]
fn falls_back_to_global_binding_when_context_override_is_not_set() {
    let mut keymap = TuiKeymap::default();
    keymap.global.queue = Some(one("ctrl-q"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");
    assert_eq!(
        runtime.composer.queue,
        vec![key_hint::ctrl(KeyCode::Char('q'))]
    );
}

#[test]
fn invalid_global_open_transcript_binding_reports_global_path() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("meta-t"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected parse error");
    assert!(err.contains("tui.keymap.global.open_transcript"));
}

#[test]
fn invalid_global_open_external_editor_binding_reports_global_path() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_external_editor = Some(one("meta-g"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected parse error");
    assert!(err.contains("tui.keymap.global.open_external_editor"));
}

#[test]
fn permission_shortcuts_reserve_plain_text() {
    for binding in ["a", "shift-a", "2", "space"] {
        let mut keymap = TuiKeymap::default();
        keymap.chat.previous_permission_mode = Some(one(binding));
        assert!(
            RuntimeKeymap::from_config(&keymap)
                .expect_err("permission shortcuts must not intercept typing")
                .contains("printable keys")
        );
        keymap.chat.previous_permission_mode = None;
        keymap.chat.next_permission_mode = Some(one(binding));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
        keymap.chat.next_permission_mode = None;
        keymap.chat.skip_question = Some(one(binding));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
        keymap.chat.skip_question = None;
        keymap.chat.prompt_stack_back = Some(one(binding));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
    }
    #[cfg(windows)]
    {
        let mut keymap = TuiKeymap::default();
        keymap.chat.next_permission_mode = Some(one("ctrl-alt-q"));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
        keymap.chat.next_permission_mode = None;
        keymap.chat.skip_question = Some(one("ctrl-alt-q"));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
        keymap.chat.skip_question = None;
        keymap.chat.prompt_stack_back = Some(one("ctrl-alt-q"));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
    }
    let mut keymap = TuiKeymap::default();
    keymap.chat.previous_permission_mode = Some(one("f7"));
    keymap.chat.next_permission_mode = Some(one("ctrl-x enter"));
    assert!(RuntimeKeymap::from_config(&keymap).is_ok());
}

#[test]
fn defaults_include_reassignable_main_surface_actions() {
    let runtime = RuntimeKeymap::defaults();

    assert_eq!(
        runtime.app.clear_terminal,
        vec![key_hint::ctrl(KeyCode::Char('l'))]
    );
    assert_eq!(runtime.app.toggle_fast_mode, Vec::new());
    assert_eq!(
        runtime.chat.interrupt_turn,
        vec![key_hint::plain(KeyCode::Esc)]
    );
    assert_eq!(
        runtime.chat.decrease_reasoning_effort,
        vec![
            key_hint::alt(KeyCode::Char(',')),
            key_hint::shift(KeyCode::Down),
        ]
    );
    assert_eq!(
        runtime.chat.increase_reasoning_effort,
        vec![
            key_hint::alt(KeyCode::Char('.')),
            key_hint::shift(KeyCode::Up),
        ]
    );
    assert_eq!(
        runtime.chat.edit_queued_message,
        vec![key_hint::shift(KeyCode::Left), key_hint::alt(KeyCode::Up)]
    );
    assert_eq!(
        runtime.composer.history_search_previous,
        vec![key_hint::ctrl(KeyCode::Char('r'))]
    );
    assert_eq!(
        runtime.composer.history_search_next,
        vec![key_hint::ctrl(KeyCode::Char('s'))]
    );
    assert_eq!(runtime.editor.kill_whole_line, Vec::new());
}

#[test]
fn defaults_include_list_page_and_jump_actions() {
    let runtime = RuntimeKeymap::defaults();

    assert_eq!(
        runtime.list.move_up,
        vec![
            key_hint::plain(KeyCode::Up),
            key_hint::ctrl(KeyCode::Char('p')),
            key_hint::ctrl(KeyCode::Char('k')),
            key_hint::plain(KeyCode::Char('k')),
        ]
    );
    assert_eq!(
        runtime.list.move_down,
        vec![
            key_hint::plain(KeyCode::Down),
            key_hint::ctrl(KeyCode::Char('n')),
            key_hint::ctrl(KeyCode::Char('j')),
            key_hint::plain(KeyCode::Char('j')),
        ]
    );
    assert_eq!(
        runtime.list.move_left,
        vec![
            key_hint::plain(KeyCode::Left),
            key_hint::ctrl(KeyCode::Char('h')),
        ]
    );
    assert_eq!(
        runtime.list.move_right,
        vec![
            key_hint::plain(KeyCode::Right),
            key_hint::ctrl(KeyCode::Char('l')),
        ]
    );
    assert_eq!(
        runtime.list.page_up,
        vec![
            key_hint::plain(KeyCode::PageUp),
            key_hint::ctrl(KeyCode::Char('b')),
        ]
    );
    assert_eq!(
        runtime.list.page_down,
        vec![
            key_hint::plain(KeyCode::PageDown),
            key_hint::ctrl(KeyCode::Char('f')),
        ]
    );
    assert_eq!(runtime.list.jump_top, vec![key_hint::plain(KeyCode::Home)]);
    assert_eq!(
        runtime.list.jump_bottom,
        vec![key_hint::plain(KeyCode::End)]
    );
}

#[test]
fn configured_main_surface_bindings_prune_reasoning_fallback_aliases() {
    let mut keymap = TuiKeymap::default();
    keymap.editor.move_up = Some(one("shift-up"));
    keymap.vim_text_object.word = Some(one("shift-down"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(runtime.editor.move_up, vec![key_hint::shift(KeyCode::Up)]);
    assert_eq!(
        runtime.vim_text_object.word,
        vec![key_hint::shift(KeyCode::Down)]
    );
    assert_eq!(
        runtime.chat.decrease_reasoning_effort,
        vec![key_hint::alt(KeyCode::Char(','))]
    );
    assert_eq!(
        runtime.chat.increase_reasoning_effort,
        vec![key_hint::alt(KeyCode::Char('.'))]
    );
}

#[test]
fn explicit_reasoning_binding_still_conflicts_with_editor_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.editor.move_up = Some(one("shift-up"));
    keymap.chat.increase_reasoning_effort = Some(one("shift-up"));

    expect_conflict(&keymap, "chat.increase_reasoning_effort", "editor.move_up");
}

#[test]
fn configured_legacy_list_bindings_prune_new_default_overlaps() {
    let mut keymap = TuiKeymap::default();
    keymap.list.move_up = Some(one("page-up"));
    keymap.list.move_down = Some(one("page-down"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(runtime.list.move_up, vec![key_hint::plain(KeyCode::PageUp)]);
    assert_eq!(
        runtime.list.move_down,
        vec![key_hint::plain(KeyCode::PageDown)]
    );
    assert_eq!(
        runtime.list.page_up,
        vec![key_hint::ctrl(KeyCode::Char('b'))]
    );
    assert_eq!(
        runtime.list.page_down,
        vec![key_hint::ctrl(KeyCode::Char('f'))]
    );
}

#[test]
fn configured_legacy_list_bindings_can_prune_all_new_default_keys() {
    let mut keymap = TuiKeymap::default();
    keymap.list.move_up = Some(KeybindingsSpec::Many(vec![
        KeybindingSpec("page-up".to_string()),
        KeybindingSpec("ctrl-b".to_string()),
    ]));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.list.move_up,
        vec![
            key_hint::plain(KeyCode::PageUp),
            key_hint::ctrl(KeyCode::Char('b')),
        ]
    );
    assert_eq!(runtime.list.page_up, Vec::new());
}

#[test]
fn explicit_new_list_bindings_still_conflict_with_legacy_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.list.move_up = Some(one("page-up"));
    keymap.list.page_up = Some(one("page-up"));

    expect_conflict(&keymap, "move_up", "page_up");
}

#[test]
fn configured_app_bindings_prune_new_list_default_overlaps() {
    let mut keymap = TuiKeymap::default();
    keymap.global.copy = Some(one("page-down"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(runtime.app.copy, vec![key_hint::plain(KeyCode::PageDown)]);
    assert_eq!(
        runtime.list.page_down,
        vec![key_hint::ctrl(KeyCode::Char('f'))]
    );
}

#[test]
fn configured_approval_bindings_prune_new_list_default_overlaps() {
    let mut keymap = TuiKeymap::default();
    keymap.approval.approve = Some(one("home"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.approval.approve,
        vec![key_hint::plain(KeyCode::Home)]
    );
    assert_eq!(runtime.list.jump_top, Vec::new());
}

#[test]
fn explicit_new_list_bindings_still_conflict_with_configured_approval_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.approval.approve = Some(one("home"));
    keymap.list.jump_top = Some(one("home"));

    expect_conflict(&keymap, "list.jump_top", "approval.approve");
}

#[test]
fn configured_legacy_vim_normal_bindings_prune_new_change_operator_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("c"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.vim_normal.move_left,
        vec![key_hint::plain(KeyCode::Char('c'))]
    );
    assert_eq!(runtime.vim_normal.start_change_operator, Vec::new());
}

#[test]
fn explicit_new_vim_normal_binding_still_conflicts_with_legacy_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("c"));
    keymap.vim_normal.start_change_operator = Some(one("c"));

    expect_conflict(&keymap, "move_left", "start_change_operator");
}

#[test]
fn explicit_replace_mode_binding_conflicts_with_legacy_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("shift-r"));
    keymap.vim_normal.enter_replace_mode = Some(one("shift-r"));

    expect_conflict(&keymap, "move_left", "enter_replace_mode");
}

#[test]
fn configured_legacy_vim_normal_bindings_prune_new_substitute_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("s"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.vim_normal.move_left,
        vec![key_hint::plain(KeyCode::Char('s'))]
    );
    assert_eq!(runtime.vim_normal.substitute_char, Vec::new());
}

#[test]
fn explicit_new_vim_normal_substitute_binding_still_conflicts_with_legacy_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("s"));
    keymap.vim_normal.substitute_char = Some(one("s"));

    expect_conflict(&keymap, "move_left", "substitute_char");
}

#[test]
fn configured_legacy_vim_normal_bindings_prune_new_replace_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("r"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.vim_normal.move_left,
        vec![key_hint::plain(KeyCode::Char('r'))]
    );
    assert_eq!(runtime.vim_normal.replace_char, Vec::new());
}

#[test]
fn configured_substitute_binding_prunes_new_replace_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.substitute_char = Some(one("r"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.vim_normal.substitute_char,
        vec![key_hint::plain(KeyCode::Char('r'))]
    );
    assert!(runtime.vim_normal.replace_char.is_empty());
}

#[test]
fn configured_vim_normal_chord_prefix_prunes_new_replace_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_line_start = Some(one("r g"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert!(runtime.vim_normal.replace_char.is_empty());
    assert!(runtime.chords.bindings.iter().any(|binding| {
        binding.action.context == KeymapContext::VimNormal
            && binding.chord.prefix == key_hint::plain(KeyCode::Char('r'))
    }));
}

#[test]
fn explicit_new_vim_normal_replace_binding_still_conflicts_with_legacy_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("r"));
    keymap.vim_normal.replace_char = Some(one("r"));

    expect_conflict(&keymap, "move_left", "replace_char");
}

#[test]
fn configured_legacy_vim_normal_bindings_prune_new_repeat_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("."));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(runtime.vim_normal.repeat_last_change, Vec::new());
}

#[test]
fn configured_legacy_vim_bindings_prune_new_navigation_defaults() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("f"));
    keymap.vim_operator.motion_left = Some(one("g"));
    keymap.vim_normal.move_right = Some(one("t"));
    keymap.vim_normal.move_up = Some(one("shift-f"));
    keymap.vim_operator.motion_right = Some(one("shift-t"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(runtime.vim_normal.find_forward, Vec::new());
    assert_eq!(runtime.vim_normal.find_backward, Vec::new());
    assert_eq!(runtime.vim_operator.motion_jump_top, Vec::new());
    assert_eq!(runtime.vim_normal.till_forward, Vec::new());
    assert_eq!(runtime.vim_operator.motion_till_backward, Vec::new());
}

#[test]
fn explicit_vim_navigation_binding_still_conflicts_with_legacy_binding() {
    for key in ["f", "t"] {
        let mut keymap = TuiKeymap::default();
        keymap.vim_normal.move_left = Some(one(key));
        let action = if key == "f" {
            keymap.vim_normal.find_forward = Some(one(key));
            "find_forward"
        } else {
            keymap.vim_normal.till_forward = Some(one(key));
            "till_forward"
        };
        expect_conflict(&keymap, "move_left", action);
    }
}

#[test]
fn vim_search_preserves_custom_motion_bindings() {
    for operator in [false, true] {
        let mut config = TuiKeymap::default();
        if operator {
            config.vim_operator.motion_left = Some(one("n"));
        } else {
            config.vim_normal.move_left = Some(one("n"));
        }
        assert!(
            RuntimeKeymap::from_config(&config)
                .unwrap()
                .vim_search
                .next
                .is_empty()
        );
        config.vim_search.next = Some(one("n"));
        assert!(RuntimeKeymap::from_config(&config).is_err());
    }
}

#[test]
fn configured_vim_normal_chord_prefix_prunes_new_repeat_default() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_line_start = Some(one(". g"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert!(runtime.vim_normal.repeat_last_change.is_empty());
    assert!(runtime.chords.bindings.iter().any(|binding| {
        binding.action.context == KeymapContext::VimNormal
            && binding.chord.prefix == key_hint::plain(KeyCode::Char('.'))
    }));
}

#[test]
fn configured_legacy_bindings_prune_new_vim_defaults() {
    for (key, new_action) in [
        ("u", "undo"),
        ("ctrl-r", "redo"),
        ("shift-r", "enter_replace_mode"),
    ] {
        for (context, action, suffix) in [
            ("vim_normal", "move_left", ""),
            ("vim_normal", "move_left", " g"),
            ("vim_search", "forward", ""),
            ("vim_search", "forward", " g"),
            ("composer", "submit", ""),
            ("global", "submit", ""),
            ("global", "queue", ""),
            ("global", "toggle_shortcuts", ""),
        ] {
            let mut keymap: TuiKeymap = serde_json::from_value(serde_json::json!({
                (context): { (action): format!("{key}{suffix}") }
            }))
            .expect("config should deserialize");
            if key == "ctrl-r" && (!suffix.is_empty() || matches!(context, "composer" | "global")) {
                // Ctrl+R chords and main-surface actions must unbind the older history key.
                keymap.composer.history_search_previous = Some(KeybindingsSpec::Many(vec![]));
            }
            let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");
            assert_eq!(
                bindings_for_action(&runtime, "vim_normal", new_action),
                Some([].as_slice()),
                "{context}.{action} = {key}{suffix}"
            );
        }
    }
}

#[test]
fn explicit_vim_history_bindings_still_conflict_with_legacy_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.move_left = Some(one("u"));
    keymap.vim_normal.undo = Some(one("u"));
    expect_conflict(&keymap, "move_left", "undo");

    keymap.vim_normal.move_left = Some(one("ctrl-r"));
    keymap.vim_normal.undo = None;
    keymap.vim_normal.redo = Some(one("ctrl-r"));
    expect_conflict(&keymap, "move_left", "redo");
}

#[test]
fn explicit_empty_arrays_unbind_vim_history_actions() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_normal.undo = Some(KeybindingsSpec::Many(vec![]));
    keymap.vim_normal.redo = Some(KeybindingsSpec::Many(vec![]));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert!(runtime.vim_normal.undo.is_empty());
    assert!(runtime.vim_normal.redo.is_empty());
}

#[test]
fn configured_legacy_vim_operator_bindings_prune_new_text_object_defaults() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_operator.motion_left = Some(one("i"));
    keymap.vim_operator.motion_right = Some(one("a"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.vim_operator.motion_left,
        vec![key_hint::plain(KeyCode::Char('i'))]
    );
    assert_eq!(
        runtime.vim_operator.motion_right,
        vec![key_hint::plain(KeyCode::Char('a'))]
    );
    assert_eq!(runtime.vim_operator.select_inner_text_object, Vec::new());
    assert_eq!(runtime.vim_operator.select_around_text_object, Vec::new());
}

#[test]
fn explicit_new_vim_operator_binding_still_conflicts_with_legacy_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.vim_operator.motion_left = Some(one("i"));
    keymap.vim_operator.select_inner_text_object = Some(one("i"));

    expect_conflict(&keymap, "motion_left", "select_inner_text_object");
}

#[test]
fn vim_normal_defaults_include_insert_and_arrow_aliases() {
    let runtime = RuntimeKeymap::defaults();

    assert_eq!(
        runtime.vim_normal.enter_insert,
        vec![
            key_hint::plain(KeyCode::Char('i')),
            key_hint::plain(KeyCode::Insert)
        ]
    );
    assert_eq!(
        runtime.vim_normal.move_left,
        vec![
            key_hint::plain(KeyCode::Char('h')),
            key_hint::plain(KeyCode::Left)
        ]
    );
    assert_eq!(
        runtime.vim_normal.move_right,
        vec![
            key_hint::plain(KeyCode::Char('l')),
            key_hint::plain(KeyCode::Right)
        ]
    );
    assert_eq!(
        runtime.vim_normal.move_up,
        vec![
            key_hint::plain(KeyCode::Char('k')),
            key_hint::plain(KeyCode::Up)
        ]
    );
    assert_eq!(
        runtime.vim_normal.move_down,
        vec![
            key_hint::plain(KeyCode::Char('j')),
            key_hint::plain(KeyCode::Down)
        ]
    );
}

#[test]
fn invalid_global_copy_binding_reports_global_path() {
    let mut keymap = TuiKeymap::default();
    keymap.global.copy = Some(one("meta-o"));

    let err = RuntimeKeymap::from_config(&keymap).expect_err("expected parse error");
    assert!(err.contains("tui.keymap.global.copy"));
}

#[test]
fn rejects_conflicting_editor_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.editor.move_left = Some(one("ctrl-h"));
    keymap.editor.move_right = Some(one("ctrl-h"));

    expect_conflict(&keymap, "move_left", "move_right");
}

#[test]
fn rejects_conflicting_pager_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.pager.scroll_up = Some(one("ctrl-u"));
    keymap.pager.scroll_down = Some(one("ctrl-u"));

    expect_conflict(&keymap, "scroll_up", "scroll_down");
}

#[test]
fn rejects_conflicting_list_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.list.move_up = Some(one("up"));
    keymap.list.move_down = Some(one("up"));

    expect_conflict(&keymap, "move_up", "move_down");

    let mut keymap = TuiKeymap::default();
    keymap.list.move_left = Some(one("left"));
    keymap.list.move_right = Some(one("left"));

    expect_conflict(&keymap, "move_left", "move_right");
}

#[test]
fn rejects_conflicting_list_page_and_jump_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.list.page_up = Some(one("home"));
    keymap.list.jump_top = Some(one("home"));

    expect_conflict(&keymap, "page_up", "jump_top");
}

#[test]
fn rejects_conflicting_approval_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.approval.approve = Some(one("y"));
    keymap.approval.decline = Some(one("y"));

    expect_conflict(&keymap, "approve", "decline");
}

#[test]
fn rejects_conflicting_approval_deny_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.approval.approve = Some(one("y"));
    keymap.approval.deny = Some(one("y"));

    expect_conflict(&keymap, "approve", "deny");
}

#[test]
fn rejects_conflicting_approval_overlay_accept_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.list.accept = Some(one("y"));

    expect_conflict(&keymap, "list.accept", "approval.approve");
}

#[test]
fn rejects_conflicting_approval_overlay_cancel_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.list.cancel = Some(one("c"));

    expect_conflict(&keymap, "list.cancel", "approval.cancel");
}

#[test]
fn reassignable_fixed_shortcuts_conflict_until_original_action_is_unbound() {
    let mut keymap = TuiKeymap::default();
    keymap.global.copy = Some(one("alt-."));

    expect_conflict(&keymap, "copy", "chat.increase_reasoning_effort");

    keymap.chat.increase_reasoning_effort = Some(KeybindingsSpec::Many(vec![]));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("remapped key should be free");
    assert_eq!(runtime.app.copy, vec![key_hint::alt(KeyCode::Char('.'))]);
}

#[test]
fn kill_whole_line_can_be_assigned_without_default_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.editor.kill_whole_line = Some(one("ctrl-shift-u"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");

    assert_eq!(
        runtime.editor.kill_whole_line,
        vec![KeyBinding::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )]
    );
}

#[test]
fn kill_whole_line_conflicts_until_kill_line_start_is_unbound() {
    let mut keymap = TuiKeymap::default();
    keymap.editor.kill_whole_line = Some(one("ctrl-u"));

    expect_conflict(&keymap, "kill_line_start", "kill_whole_line");

    keymap.editor.kill_line_start = Some(KeybindingsSpec::Many(vec![]));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("remapped key should be free");
    assert_eq!(
        runtime.editor.kill_whole_line,
        vec![key_hint::ctrl(KeyCode::Char('u'))]
    );
}

#[test]
fn toggle_fast_mode_can_be_assigned_without_default_binding() {
    let mut keymap = TuiKeymap::default();
    keymap.global.toggle_fast_mode = Some(one("ctrl-shift-f"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");

    assert_eq!(
        runtime.app.toggle_fast_mode,
        vec![KeyBinding::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )]
    );
}

#[test]
fn toggle_fast_mode_conflicts_with_existing_main_surface_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.global.toggle_fast_mode = Some(one("ctrl-l"));

    expect_conflict(&keymap, "clear_terminal", "toggle_fast_mode");
}

#[test]
fn agents_overview_can_be_remapped_and_rejects_conflicts() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_agents = Some(one("f12"));
    keymap.agents.search = Some(one("f6"));
    keymap.agents.new_task = Some(one("f7"));
    keymap.agents.resume = Some(one("f5"));
    keymap.agents.rename = Some(one("f9"));
    keymap.agents.stop = Some(one("f10"));
    keymap.agents.toggle_grouping = Some(one("f8"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    assert_eq!(
        (
            runtime.app.open_agents,
            runtime.agents.search,
            runtime.agents.new_task,
            runtime.agents.resume,
            runtime.agents.rename,
            runtime.agents.stop,
            runtime.agents.toggle_grouping,
        ),
        (
            vec![key_hint::plain(KeyCode::F(12))],
            vec![key_hint::plain(KeyCode::F(6))],
            vec![key_hint::plain(KeyCode::F(7))],
            vec![key_hint::plain(KeyCode::F(5))],
            vec![key_hint::plain(KeyCode::F(9))],
            vec![key_hint::plain(KeyCode::F(10))],
            vec![key_hint::plain(KeyCode::F(8))],
        )
    );

    keymap.agents.resume = Some(one("f6"));
    expect_conflict(&keymap, "resume", "search");
    keymap.agents.resume = Some(one("f5"));

    keymap.agents.toggle_grouping = Some(one("right"));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    assert!(
        runtime
            .list
            .move_right
            .contains(&key_hint::plain(KeyCode::Right))
    );
    keymap.agents.toggle_grouping = Some(one("f8"));

    keymap.agents.stop = Some(one("f9"));
    expect_conflict(&keymap, "rename", "stop");

    keymap.agents.stop = Some(one("f10"));
    keymap.global.open_agents = Some(one("ctrl-t"));
    expect_conflict(&keymap, "open_agents", "open_transcript");

    keymap.global.open_agents = Some(one("f12"));
    keymap.agents.stop = Some(one("ctrl-c"));
    expect_conflict(&keymap, "stop", "fixed.interrupt_or_quit");

    #[cfg(unix)]
    {
        keymap.agents.stop = Some(one("ctrl-z"));
        assert!(
            RuntimeKeymap::from_config(&keymap)
                .unwrap_err()
                .contains("suspend")
        );
        keymap.agents.stop = Some(one("f10"));
        keymap.global.open_agents = Some(one("ctrl-z"));
        assert!(
            RuntimeKeymap::from_config(&keymap)
                .unwrap_err()
                .contains("suspend")
        );
        keymap.global.open_agents = Some(one("f12"));
    }

    keymap.agents.stop = Some(one("f10"));
    keymap.agents.search = Some(one("right"));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("dashboard shortcuts take priority");
    assert_eq!(runtime.agents.search, vec![key_hint::plain(KeyCode::Right)]);
    assert_eq!(
        runtime.list.move_right,
        RuntimeKeymap::defaults().list.move_right
    );

    keymap.agents.search = None;
    keymap.agents.rename = None;
    keymap.list.move_right = Some(one("ctrl-r"));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("list shortcut remains usable");
    assert_eq!(
        runtime.agents.rename,
        vec![key_hint::plain(KeyCode::Char('r'))]
    );

    keymap.agents.search = Some(one("s"));
    assert!(RuntimeKeymap::from_config(&keymap).is_ok());

    keymap.agents.search = None;
    keymap.agents.stop = Some(one("backspace"));
    assert!(
        RuntimeKeymap::from_config(&keymap)
            .expect_err("backspace is reserved for task input")
            .contains("backspace")
    );

    #[cfg(windows)]
    {
        keymap.agents.stop = Some(one("ctrl-alt-@"));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
        keymap.agents.stop = Some(one("f10"));
        keymap.global.open_agents = Some(one("ctrl-alt-@"));
        assert!(
            RuntimeKeymap::from_config(&keymap)
                .unwrap_err()
                .contains("AltGr")
        );
        keymap.global.open_agents = Some(one("f12"));
    }
    keymap.agents.stop = Some(one("backspace f10"));
    assert!(
        RuntimeKeymap::from_config(&keymap)
            .expect_err("backspace is reserved for task input")
            .contains("backspace")
    );
    for binding in ["alt-backspace", "ctrl-backspace"] {
        keymap.agents.stop = Some(one(binding));
        assert!(RuntimeKeymap::from_config(&keymap).is_ok());
    }
}

#[test]
fn agents_resume_default_yields_to_existing_custom_shortcuts() {
    for binding in ["o", "o f6"] {
        let mut keymap = TuiKeymap::default();
        keymap.agents.search = Some(one(binding));
        let runtime = RuntimeKeymap::from_config(&keymap).expect("existing keymap remains valid");
        assert!(runtime.agents.resume.is_empty());

        keymap.agents.resume = Some(one("o"));
        assert!(RuntimeKeymap::from_config(&keymap).is_err());
    }
}

#[test]
fn toggle_side_conversation_can_be_remapped_and_rejects_conflicts() {
    for (pane, section, action) in [
        ("hide_pane", "pager", "page_up"),
        ("hide_pane", "list", "move_down"),
        ("focus_main_composer", "pager", "page_up"),
        ("focus_sidebar_content", "list", "move_down"),
        ("focus_pane_left", "list", "move_down"),
        ("focus_pane_right", "pager", "page_up"),
        ("toggle_pane_zoom", "pager", "page_up"),
    ] {
        let config: TuiKeymap = toml::from_str(&format!(
            "[global]\n{pane} = 'f8'\n[{section}]\n{action} = 'f8'"
        ))
        .unwrap();
        let error = RuntimeKeymap::for_session(&config, /*workspace*/ true).unwrap_err();
        assert!(
            error.contains(&format!("global.{pane}")) && error.contains(action),
            "{error}"
        );
    }
    #[cfg(unix)]
    for action in [
        "hide_pane",
        "focus_main_composer",
        "focus_sidebar_content",
        "focus_pane_left",
        "focus_pane_right",
        "toggle_pane_zoom",
    ] {
        let config: TuiKeymap = toml::from_str(&format!("[global]\n{action} = 'ctrl-z'")).unwrap();
        assert!(
            RuntimeKeymap::for_session(&config, /*workspace*/ true)
                .unwrap_err()
                .contains("suspend")
        );
    }
    let mut keymap = TuiKeymap::default();
    keymap.global.toggle_side_conversation = Some(one("f12"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("runtime keymap");
    assert_eq!(
        runtime.app.toggle_side_conversation,
        vec![key_hint::plain(KeyCode::F(12))]
    );

    keymap.global.toggle_side_conversation = Some(one("ctrl-l"));
    expect_conflict(&keymap, "clear_terminal", "toggle_side_conversation");
}

#[test]
fn toggle_side_conversation_default_yields_to_existing_configured_bindings() {
    for binding in ["ctrl-/", "ctrl-7"] {
        let mut keymap = TuiKeymap::default();
        keymap.global.open_transcript = Some(one(binding));

        let runtime = RuntimeKeymap::from_config(&keymap).expect("existing keymap remains valid");

        assert_eq!(
            runtime.app.open_transcript,
            vec![parse_keybinding(binding).expect("configured binding")]
        );
        assert!(runtime.app.toggle_side_conversation.is_empty());
    }

    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("leader left"));
    keymap.global.focus_pane_right = Some(one("f12 b"));
    keymap.global.toggle_pane_zoom = Some(one("f12 z"));
    let runtime = RuntimeKeymap::for_session(&keymap, /*workspace*/ true).unwrap();
    for (action, expected) in [
        ("focus_pane_left", "leader h"),
        ("focus_pane_right", "f12 b"),
        ("toggle_pane_zoom", "f12 z"),
    ] {
        let installed: Vec<_> = runtime
            .chords
            .bindings
            .iter()
            .filter(|binding| binding.action.action == action)
            .map(|binding| binding.spec.as_str())
            .collect();
        assert_eq!(installed, [expected], "{action}");
    }

    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("ctrl-/"));
    keymap.global.copy = Some(one("ctrl-7"));

    let runtime =
        RuntimeKeymap::from_config(&keymap).expect("distinct legacy bindings remain valid");
    assert!(runtime.app.toggle_side_conversation.is_empty());
}

#[test]
fn pane_binding_only_shadows_side_toggle_in_workspace_sessions() {
    for action in ["focus_pane_right", "focus_sidebar_tabs"] {
        let keymap = toml::from_str(&format!("[global]\n{action} = 'ctrl-/'")).unwrap();
        let classic = RuntimeKeymap::for_session(&keymap, /*workspace*/ false).unwrap();
        assert_eq!(
            classic.app.toggle_side_conversation,
            RuntimeKeymap::defaults().app.toggle_side_conversation
        );
        let workspace = RuntimeKeymap::for_session(&keymap, /*workspace*/ true).unwrap();
        assert!(workspace.app.toggle_side_conversation.is_empty());
    }
}

#[test]
fn explicit_side_toggle_rejects_legacy_ctrl_slash_alias_conflicts() {
    let mut keymap = TuiKeymap::default();
    keymap.global.open_transcript = Some(one("ctrl-7"));
    keymap.global.toggle_side_conversation = Some(one("ctrl-/"));

    expect_conflict(&keymap, "open_transcript", "toggle_side_conversation");
}

#[test]
fn rejects_main_bindings_that_collide_with_remaining_fixed_shortcuts() {
    let mut keymap = TuiKeymap::default();
    keymap.composer.submit = Some(one("ctrl-v"));

    expect_conflict(&keymap, "composer.submit", "fixed.paste_image");
}

#[test]
fn interrupt_turn_allows_backtrack_escape_and_can_be_remapped_or_unbound() {
    let mut keymap = TuiKeymap::default();
    let runtime = RuntimeKeymap::from_config(&keymap).expect("default keymap should parse");
    assert_eq!(
        runtime.chat.interrupt_turn,
        vec![key_hint::plain(KeyCode::Esc)]
    );

    keymap.chat.interrupt_turn = Some(one("f12"));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("remapped keymap should parse");
    assert_eq!(
        runtime.chat.interrupt_turn,
        vec![key_hint::plain(KeyCode::F(12))]
    );

    keymap.chat.interrupt_turn = Some(KeybindingsSpec::Many(vec![]));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("unbound keymap should parse");
    assert!(runtime.chat.interrupt_turn.is_empty());
}

#[test]
fn interrupt_turn_rejects_other_fixed_shortcuts() {
    let mut keymap = TuiKeymap::default();
    keymap.chat.interrupt_turn = Some(one("ctrl-v"));

    expect_conflict(&keymap, "chat.interrupt_turn", "fixed.paste_image");
}

#[test]
fn interrupt_turn_rejects_request_user_input_question_navigation_bindings() {
    let mut keymap = TuiKeymap::default();
    keymap.chat.interrupt_turn = Some(one("f12"));
    keymap.list.move_right = Some(one("f12"));

    expect_conflict(&keymap, "chat.interrupt_turn", "list.move_right");
}

#[test]
fn interrupt_turn_rejects_question_back_and_skip_bindings() {
    for (key, action) in [
        ("alt-down", "chat.prompt_stack_back"),
        ("ctrl-]", "chat.skip_question"),
        ("ctrl-5", "chat.skip_question"),
        ("ctrl-] x", "chat.skip_question"),
        ("ctrl-5 x", "chat.skip_question"),
    ] {
        let mut keymap = TuiKeymap::default();
        keymap.chat.interrupt_turn = Some(one(key));
        if key == "alt-down" {
            keymap.chat.prompt_stack_back = Some(one(key));
        } else {
            let runtime = RuntimeKeymap::from_config(&keymap).unwrap();
            assert!(runtime.chat.skip_question.is_empty());
            keymap.chat.skip_question = Some(one("ctrl-]"));
        }
        expect_conflict(&keymap, "chat.interrupt_turn", action);
    }
}

#[test]
fn rejects_pager_bindings_that_collide_with_transcript_backtrack_keys() {
    let mut keymap = TuiKeymap::default();
    keymap.pager.close = Some(one("left"));

    expect_conflict(&keymap, "close", "fixed.transcript_edit_previous");
}

#[test]
fn parses_function_keys_and_rejects_out_of_range_function_keys() {
    assert_eq!(
        parse_keybinding("f1").map(|binding| binding.parts()),
        Some((KeyCode::F(1), KeyModifiers::NONE))
    );
    assert_eq!(
        parse_keybinding("f24").map(|binding| binding.parts()),
        Some((KeyCode::F(24), KeyModifiers::NONE))
    );
    assert_eq!(parse_keybinding("f25"), None);
}

#[test]
fn parses_all_named_non_character_keys() {
    let cases = [
        ("tab", KeyCode::Tab),
        ("backspace", KeyCode::Backspace),
        ("esc", KeyCode::Esc),
        ("delete", KeyCode::Delete),
        ("up", KeyCode::Up),
        ("down", KeyCode::Down),
        ("left", KeyCode::Left),
        ("right", KeyCode::Right),
        ("home", KeyCode::Home),
        ("end", KeyCode::End),
        ("page-up", KeyCode::PageUp),
        ("page-down", KeyCode::PageDown),
        ("space", KeyCode::Char(' ')),
        ("minus", KeyCode::Char('-')),
    ];

    for (spec, expected_key) in cases {
        assert_eq!(
            parse_keybinding(spec).map(|binding| binding.parts()),
            Some((expected_key, KeyModifiers::NONE)),
            "failed to parse {spec}"
        );
    }
}

#[test]
fn rejects_modifier_only_and_nonnumeric_function_key_specs() {
    assert_eq!(parse_keybinding("ctrl"), None);
    assert_eq!(parse_keybinding("ff"), None);
}

#[test]
fn parses_minus_alias_and_legacy_literal_minus() {
    assert_eq!(
        parse_keybinding("alt-minus").map(|binding| binding.parts()),
        Some((KeyCode::Char('-'), KeyModifiers::ALT))
    );
    assert_eq!(
        parse_keybinding("alt--").map(|binding| binding.parts()),
        Some((KeyCode::Char('-'), KeyModifiers::ALT))
    );
    assert_eq!(
        parse_keybinding("-").map(|binding| binding.parts()),
        Some((KeyCode::Char('-'), KeyModifiers::NONE))
    );
}

#[test]
fn explicit_empty_array_unbinds_action() {
    let mut keymap = TuiKeymap::default();
    keymap.composer.toggle_shortcuts = Some(KeybindingsSpec::Many(vec![]));
    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");
    assert!(runtime.composer.toggle_shortcuts.is_empty());
    keymap.global.hide_pane = Some(KeybindingsSpec::Many(vec![]));
    keymap.global.focus_sidebar_content = Some(KeybindingsSpec::Many(vec![]));
    keymap.global.focus_pane_right = Some(KeybindingsSpec::Many(vec![]));
    let runtime = RuntimeKeymap::for_session(&keymap, /*workspace*/ true).unwrap();
    for action in ["hide_pane", "focus_sidebar_content", "focus_pane_right"] {
        assert!(
            runtime
                .primary_hint(KeymapContext::Global, action)
                .is_none()
        );
    }
}

#[test]
fn raw_output_toggle_defaults_to_alt_r() {
    let runtime = RuntimeKeymap::defaults();
    assert_eq!(
        runtime.app.toggle_raw_output,
        vec![key_hint::alt(KeyCode::Char('r'))]
    );
}

#[test]
fn raw_output_toggle_can_be_remapped() {
    let mut keymap = TuiKeymap::default();
    keymap.global.toggle_raw_output = Some(one("f12"));

    let runtime = RuntimeKeymap::from_config(&keymap).expect("config should parse");

    assert_eq!(
        runtime.app.toggle_raw_output,
        vec![key_hint::plain(KeyCode::F(12))]
    );
}

#[test]
fn default_editor_insert_newline_includes_current_aliases() {
    let runtime = RuntimeKeymap::defaults();
    assert_eq!(
        runtime.editor.insert_newline,
        vec![
            key_hint::ctrl(KeyCode::Char('j')),
            key_hint::ctrl(KeyCode::Char('m')),
            key_hint::plain(KeyCode::Enter),
            key_hint::shift(KeyCode::Enter),
            key_hint::alt(KeyCode::Enter),
        ]
    );
}

#[test]
fn default_editor_delete_forward_word_includes_alt_d() {
    let runtime = RuntimeKeymap::defaults();
    assert!(
        runtime
            .editor
            .delete_forward_word
            .contains(&key_hint::alt(KeyCode::Char('d')))
    );
}

#[test]
fn default_editor_deletion_includes_modified_backspace_delete_aliases() {
    let runtime = RuntimeKeymap::defaults();

    assert!(
        runtime
            .editor
            .delete_backward
            .contains(&key_hint::shift(KeyCode::Backspace))
    );
    assert!(
        runtime
            .editor
            .delete_forward
            .contains(&key_hint::shift(KeyCode::Delete))
    );
    assert!(
        runtime
            .editor
            .delete_backward_word
            .contains(&key_hint::ctrl(KeyCode::Backspace))
    );
    assert!(
        runtime
            .editor
            .delete_backward_word
            .contains(&KeyBinding::new(
                KeyCode::Backspace,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
    );
    assert!(
        runtime
            .editor
            .delete_forward_word
            .contains(&key_hint::ctrl(KeyCode::Delete))
    );
    assert!(
        runtime
            .editor
            .delete_forward_word
            .contains(&KeyBinding::new(
                KeyCode::Delete,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
    );
}

#[test]
fn default_composer_toggle_shortcuts_includes_shift_question_mark() {
    let runtime = RuntimeKeymap::defaults();
    assert!(
        runtime
            .composer
            .toggle_shortcuts
            .contains(&key_hint::shift(KeyCode::Char('?')))
    );
}

#[test]
fn default_approval_open_fullscreen_includes_ctrl_shift_a() {
    let runtime = RuntimeKeymap::defaults();
    assert!(runtime.approval.open_fullscreen.contains(&KeyBinding::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
}

#[test]
fn primary_binding_returns_first_or_none() {
    let bindings = vec![
        key_hint::ctrl(KeyCode::Char('a')),
        key_hint::shift(KeyCode::Char('b')),
    ];
    assert_eq!(
        primary_binding(&bindings),
        Some(key_hint::ctrl(KeyCode::Char('a')))
    );
    assert_eq!(primary_binding(&[]), None);
}

#[test]
fn defaults_pass_conflict_validation() {
    RuntimeKeymap::defaults()
        .validate_conflicts()
        .expect("default keymap should be conflict free");

    let mut keymap = TuiKeymap::default();
    keymap.global.leader = Some(one("ctrl-r"));
    let runtime = RuntimeKeymap::from_config(&keymap)
        .expect("an unused leader must preserve single-key shortcuts");
    assert_eq!(
        (
            runtime.composer.history_search_previous,
            runtime.chat.open_presets,
            runtime.app.select_transcript
        ),
        (
            vec![key_hint::ctrl(KeyCode::Char('r'))],
            Vec::new(),
            Vec::new()
        )
    );
    let defaults = RuntimeKeymap::defaults();
    for leader in ["page-up", "page-down"] {
        keymap.global.leader = Some(one(leader));
        let runtime = RuntimeKeymap::from_config(&keymap)
            .expect("an unused leader must preserve transcript paging");
        assert_eq!(
            (
                runtime.pager.page_up,
                runtime.pager.page_down,
                runtime.chat.open_presets,
                runtime.app.select_transcript
            ),
            (
                defaults.pager.page_up.clone(),
                defaults.pager.page_down.clone(),
                Vec::new(),
                Vec::new()
            )
        );
    }
}
