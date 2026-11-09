//! Guided keymap remapping UI for `/keymap`.
//!
//! This module owns the interactive editing flow that starts from a resolved
//! [`RuntimeKeymap`] and produces a new root-level [`TuiKeymap`] override. The
//! picker and action menus show users the currently active binding, which may
//! come from defaults, global fallback, or explicit config, while writes always
//! target the concrete `tui.keymap.<context>.<action>` slot selected by the
//! user.
//!
//! The flow is intentionally split into three steps: choose an action, choose
//! whether to replace/add/remove a binding, then capture either one terminal
//! key event, a bounded two-stroke chord, or the key following the configured
//! leader. Validation reuses runtime keymap resolution, so conflict rules stay
//! centralized in `keymap.rs` instead of being duplicated in the UI.
//!
//! This module does not persist config files directly. It emits app events with
//! the edited config so the app layer can decide how to save, reload, and
//! surface errors.

mod actions;
mod capture;
mod debug;
mod picker;

#[cfg(test)]
#[path = "keymap_setup/menu_tests.rs"]
mod menu_tests;

pub(crate) use actions::KeymapActionFilter;
use capture::KeymapCaptureView;
pub(crate) use debug::build_keymap_debug_view;
pub(crate) use picker::KEYMAP_PICKER_VIEW_ID;
#[cfg(test)]
pub(crate) use picker::build_keymap_picker_params;
#[cfg(test)]
pub(crate) use picker::build_keymap_picker_params_for_selected_action;
pub(crate) use picker::build_keymap_picker_params_for_selected_action_with_filter;
pub(crate) use picker::build_keymap_picker_params_with_filter;

use codex_config::types::KeybindingSpec;
use codex_config::types::KeybindingsSpec;
use codex_config::types::MAX_FUNCTION_KEY;
use codex_config::types::TuiKeymap;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use crate::app_event::AppEvent;
use crate::app_event::KeymapCaptureMode;
use crate::app_event::KeymapEditIntent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::picker_hint_line_for_keymap;
use crate::key_hint::KeyBinding;
use crate::keymap::RuntimeKeymap;
use crate::keymap::bindings_for_action;
use crate::keymap::keymap_action_id;
use crate::style::accent_color;
use actions::KEYMAP_ACTIONS;
use actions::action_label;
use actions::binding_slot;
use actions::format_action_binding_summary;
#[cfg(test)]
use debug::KeymapDebugView;

pub(crate) const KEYMAP_ACTION_MENU_VIEW_ID: &str = "keymap-action-menu";
pub(crate) const KEYMAP_REPLACE_BINDING_MENU_VIEW_ID: &str = "keymap-replace-binding-menu";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeymapEditOutcome {
    /// The edit produced a new config snapshot and user-facing status message.
    Updated {
        keymap_config: Box<TuiKeymap>,
        bindings: Vec<String>,
        message: String,
    },
    /// The requested edit resolved to the same effective binding set.
    Unchanged { message: String },
}

fn key_binding_span(binding: &str) -> ratatui::text::Span<'static> {
    if binding == "unbound" {
        binding.to_string().dim()
    } else {
        binding.to_string().fg(accent_color())
    }
}

fn open_capture_action(
    context: String,
    action: String,
    intent: KeymapEditIntent,
    capture_mode: KeymapCaptureMode,
) -> Box<dyn Fn(&AppEventSender) + Send + Sync> {
    Box::new(move |tx| {
        tx.send(AppEvent::OpenKeymapCapture {
            context: context.clone(),
            action: action.clone(),
            intent: intent.clone(),
            capture_mode,
        });
    })
}

fn action_menu_item(
    name: &str,
    description: String,
    context: &str,
    action: &str,
    intent: KeymapEditIntent,
    capture_mode: KeymapCaptureMode,
) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        description: Some(description),
        actions: vec![open_capture_action(
            context.to_string(),
            action.to_string(),
            intent,
            capture_mode,
        )],
        ..Default::default()
    }
}

/// Build the action-specific menu after a user chooses a shortcut row.
///
/// The menu is based on both active runtime bindings and root config state: the
/// active bindings decide whether replace/add choices are available, while the
/// config state decides whether "remove custom binding" can restore fallback
/// behavior. Passing stale context/action strings yields a generic fallback
/// menu rather than panicking, because selection views can outlive config reloads.
pub(crate) fn build_keymap_action_menu_params(
    context: String,
    action: String,
    runtime_keymap: &RuntimeKeymap,
    keymap_config: &TuiKeymap,
) -> SelectionViewParams {
    let current_bindings =
        active_binding_specs(runtime_keymap, &context, &action).unwrap_or_else(|_| Vec::new());
    let current_binding = if current_bindings.is_empty() {
        "unbound".to_string()
    } else {
        current_bindings.join(", ")
    };
    let active_binding_count = current_bindings.len();
    let custom_binding = has_custom_binding(keymap_config, &context, &action).unwrap_or(false);
    let descriptor = KEYMAP_ACTIONS
        .iter()
        .find(|descriptor| descriptor.context == context && descriptor.action == action);
    let context_label = descriptor
        .map(|descriptor| descriptor.context_label)
        .unwrap_or(context.as_str())
        .to_string();
    let description = descriptor
        .map(|descriptor| descriptor.description)
        .unwrap_or("Configure this shortcut.");
    let remove_disabled_reason =
        (!custom_binding).then(|| "No custom root override to remove".to_string());
    let label = action_label(&action);
    let remove_context = context.clone();
    let remove_action = action.clone();
    let config_path = format!("tui.keymap.{context}.{action}");
    let source = if custom_binding {
        "Custom root override".fg(accent_color())
    } else {
        "Default keymap".dim()
    };
    let header = vec![
        Line::from("Edit Shortcut".bold()),
        Line::from(vec![label.bold(), " · ".dim(), context_label.dim()]),
        Line::from(vec![
            "Current ".dim(),
            key_binding_span(&current_binding),
            " · ".dim(),
            source,
        ]),
        Line::from(vec![
            "Config ".dim(),
            format!("`{config_path}`").fg(accent_color()),
        ]),
        Line::from(description.to_string().dim()),
    ];

    let mut items = Vec::new();
    match active_binding_count {
        0 => {
            items.push(action_menu_item(
                "Set key",
                "Capture a key for this unbound action".to_string(),
                &context,
                &action,
                KeymapEditIntent::ReplaceAll,
                KeymapCaptureMode::SingleKey,
            ));
        }
        1 => {
            items.push(action_menu_item(
                "Replace binding",
                format!("Capture a replacement key for `{current_binding}`"),
                &context,
                &action,
                KeymapEditIntent::ReplaceAll,
                KeymapCaptureMode::SingleKey,
            ));
            items.push(action_menu_item(
                "Add alternate binding",
                format!("Keep `{current_binding}` and add another key"),
                &context,
                &action,
                KeymapEditIntent::AddAlternate,
                KeymapCaptureMode::SingleKey,
            ));
        }
        _ => {
            let replace_one_context = context.clone();
            let replace_one_action = action.clone();
            items.push(SelectionItem {
                name: "Replace one binding...".to_string(),
                description: Some("Choose which existing binding to replace".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenKeymapReplaceBindingMenu {
                        context: replace_one_context.clone(),
                        action: replace_one_action.clone(),
                    });
                })],
                ..Default::default()
            });
            items.push(action_menu_item(
                "Replace all bindings",
                format!("Replace `{current_binding}` with one key"),
                &context,
                &action,
                KeymapEditIntent::ReplaceAll,
                KeymapCaptureMode::SingleKey,
            ));
            items.push(action_menu_item(
                "Add alternate binding",
                format!("Keep `{current_binding}` and add another key"),
                &context,
                &action,
                KeymapEditIntent::AddAlternate,
                KeymapCaptureMode::SingleKey,
            ));
        }
    }
    if action != "leader" {
        let leader_available = !runtime_keymap.chords.leader.is_empty();
        items.push(SelectionItem {
            name: "Use leader...".to_string(),
            description: Some("Capture the key that follows the configured leader".to_string()),
            disabled_reason: (!leader_available).then(|| "The leader is unbound".to_string()),
            actions: vec![open_capture_action(
                context.clone(),
                action.clone(),
                KeymapEditIntent::ReplaceAll,
                KeymapCaptureMode::LeaderChord,
            )],
            ..Default::default()
        });
        if !leader_available {
            items.push(SelectionItem {
                name: "Configure leader...".to_string(),
                description: Some("Set a leader before assigning this shortcut".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::OpenKeymapActionMenu {
                        context: "global".to_string(),
                        action: "leader".to_string(),
                    });
                })],
                ..Default::default()
            });
        }
    }
    if action != "leader" && active_binding_count == 0 {
        items.push(action_menu_item(
            "Set key chord",
            "Capture two consecutive keys for this action".to_string(),
            &context,
            &action,
            KeymapEditIntent::ReplaceAll,
            KeymapCaptureMode::Chord,
        ));
    } else if action != "leader" {
        items.push(action_menu_item(
            "Replace with key chord",
            format!("Replace `{current_binding}` with a two-stroke key chord"),
            &context,
            &action,
            KeymapEditIntent::ReplaceAll,
            KeymapCaptureMode::Chord,
        ));
        items.push(action_menu_item(
            "Add alternate key chord",
            format!("Keep `{current_binding}` and add a two-stroke key chord"),
            &context,
            &action,
            KeymapEditIntent::AddAlternate,
            KeymapCaptureMode::Chord,
        ));
    }
    items.push(SelectionItem {
        name: "Remove custom binding".to_string(),
        description: custom_binding.then(|| "Restore the default keymap binding".to_string()),
        disabled_reason: remove_disabled_reason,
        disabled_gutter_marker: Some("–"),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::KeymapCleared {
                context: remove_context.clone(),
                action: remove_action.clone(),
            });
        })],
        ..Default::default()
    });
    items.push(SelectionItem {
        name: "Back to shortcuts".to_string(),
        description: Some("Return to the shortcut list".to_string()),
        dismiss_on_select: true,
        ..Default::default()
    });

    SelectionViewParams {
        allow_input_when_disconnected: true,
        view_id: Some(KEYMAP_ACTION_MENU_VIEW_ID),
        header: Box::new(Paragraph::new(header).wrap(Wrap { trim: false })),
        footer_note: Some(Line::from(vec![
            "Changes write the root ".dim(),
            "`tui.keymap.*`".fg(accent_color()),
            " override.".dim(),
        ])),
        footer_hint: Some(picker_hint_line_for_keymap(&runtime_keymap.list)),
        items,
        ..SelectionViewParams::picker()
    }
}

pub(crate) fn build_keymap_replace_binding_menu_params(
    context: String,
    action: String,
    runtime_keymap: &RuntimeKeymap,
) -> SelectionViewParams {
    let bindings = active_binding_specs(runtime_keymap, &context, &action).unwrap_or_default();
    let label = action_label(&action);
    let header = vec![
        Line::from("Replace Binding".bold()),
        Line::from(vec![
            label.bold(),
            " · ".dim(),
            format!("{context}.{action}").dim(),
        ]),
        Line::from("Choose the binding to replace.".dim()),
    ];

    let items = bindings
        .into_iter()
        .flat_map(|binding| {
            [
                SelectionItem {
                    name: binding.clone(),
                    description: Some(format!("Replace `{binding}` with another key")),
                    actions: vec![open_capture_action(
                        context.clone(),
                        action.clone(),
                        KeymapEditIntent::ReplaceOne {
                            old_key: binding.clone(),
                        },
                        KeymapCaptureMode::SingleKey,
                    )],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: format!("{binding} (key chord)"),
                    description: Some(format!("Replace `{binding}` with a two-stroke key chord")),
                    actions: vec![open_capture_action(
                        context.clone(),
                        action.clone(),
                        KeymapEditIntent::ReplaceOne { old_key: binding },
                        KeymapCaptureMode::Chord,
                    )],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ]
            .into_iter()
            .take(if action == "leader" { 1 } else { 2 })
        })
        .collect();

    SelectionViewParams {
        allow_input_when_disconnected: true,
        view_id: Some(KEYMAP_REPLACE_BINDING_MENU_VIEW_ID),
        header: Box::new(Paragraph::new(header).wrap(Wrap { trim: false })),
        footer_hint: Some(picker_hint_line_for_keymap(&runtime_keymap.list)),
        items,
        ..SelectionViewParams::picker()
    }
}

pub(crate) fn build_keymap_conflict_params(
    context: String,
    action: String,
    key: String,
    intent: KeymapEditIntent,
    error: String,
    runtime_keymap: &RuntimeKeymap,
) -> SelectionViewParams {
    let capture_mode = if key.starts_with("leader ") {
        KeymapCaptureMode::LeaderChord
    } else if key.contains(' ') {
        KeymapCaptureMode::Chord
    } else {
        KeymapCaptureMode::SingleKey
    };
    SelectionViewParams {
        allow_input_when_disconnected: true,
        header: Box::new(
            Paragraph::new(vec![
                Line::from("Shortcut Conflict".bold()),
                Line::from(format!("{context}.{action} cannot use `{key}`.").dim()),
                Line::from(error.red()),
            ])
            .wrap(Wrap { trim: false }),
        ),
        footer_hint: Some(picker_hint_line_for_keymap(&runtime_keymap.list)),
        items: vec![
            SelectionItem {
                name: "Pick another key".to_string(),
                description: Some("Return to key capture for this action".to_string()),
                actions: vec![open_capture_action(context, action, intent, capture_mode)],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                description: Some("Leave keymap unchanged".to_string()),
                dismiss_on_select: true,
                ..Default::default()
            },
        ],
        ..SelectionViewParams::picker()
    }
}

/// Build the transient capture view for the selected keymap edit.
///
/// The view displays the current binding summary from the latest runtime map
/// and then delegates the captured key back to the app event loop. Unknown
/// actions are rendered as unbound so the eventual edit path can report the
/// stale selection with a precise error.
pub(crate) fn build_keymap_capture_view(
    context: String,
    action: String,
    intent: KeymapEditIntent,
    capture_mode: KeymapCaptureMode,
    runtime_keymap: &RuntimeKeymap,
    app_event_tx: AppEventSender,
) -> KeymapCaptureView {
    let current_binding = format_action_binding_summary(runtime_keymap, &context, &action);
    let label = action_label(&action);
    KeymapCaptureView::new(
        context,
        action,
        intent,
        capture_mode,
        label,
        current_binding,
        app_event_tx,
    )
}

#[cfg(test)]
fn keymap_with_replacement(
    keymap: &TuiKeymap,
    context: &str,
    action: &str,
    key: &str,
) -> Result<TuiKeymap, String> {
    keymap_with_bindings(keymap, context, action, &[key.to_string()])
}

/// Apply a captured key to one action and return the edited root config.
///
/// The current effective bindings come from `runtime_keymap`, so adding an
/// alternate to a default-only action first materializes those defaults into
/// root config before appending the captured key. Replacing one binding guards
/// against stale menus by requiring the selected `old_key` to still be active;
/// otherwise a user could overwrite a binding that changed after the menu was
/// opened.
pub(crate) fn keymap_with_edit(
    keymap: &TuiKeymap,
    runtime_keymap: &RuntimeKeymap,
    context: &str,
    action: &str,
    key: &str,
    intent: &KeymapEditIntent,
) -> Result<KeymapEditOutcome, String> {
    let current_bindings = active_binding_specs(runtime_keymap, context, action)?;
    let next_bindings = match intent {
        KeymapEditIntent::ReplaceAll => vec![key.to_string()],
        KeymapEditIntent::AddAlternate => {
            if current_bindings.iter().any(|binding| binding == key) {
                return Ok(KeymapEditOutcome::Unchanged {
                    message: format!("No change: `{context}.{action}` already uses `{key}`."),
                });
            }
            let mut bindings = current_bindings.clone();
            bindings.push(key.to_string());
            bindings
        }
        KeymapEditIntent::ReplaceOne { old_key } => {
            if !current_bindings.iter().any(|binding| binding == old_key) {
                return Err(format!(
                    "`{context}.{action}` no longer uses `{old_key}`. Reopen /keymap and choose a binding again."
                ));
            }
            let bindings = current_bindings
                .iter()
                .map(|binding| {
                    if binding == old_key {
                        key.to_string()
                    } else {
                        binding.clone()
                    }
                })
                .collect::<Vec<_>>();
            dedup_bindings(bindings)
        }
    };

    if next_bindings == current_bindings {
        return Ok(KeymapEditOutcome::Unchanged {
            message: format!("No change: `{context}.{action}` already uses `{key}`."),
        });
    }

    let message = match intent {
        KeymapEditIntent::ReplaceAll => format!("Remapped `{context}.{action}` to `{key}`."),
        KeymapEditIntent::AddAlternate => format!("Added `{key}` to `{context}.{action}`."),
        KeymapEditIntent::ReplaceOne { old_key } => {
            format!("Replaced `{old_key}` with `{key}` for `{context}.{action}`.")
        }
    };

    Ok(KeymapEditOutcome::Updated {
        keymap_config: Box::new(keymap_with_bindings(
            keymap,
            context,
            action,
            &next_bindings,
        )?),
        bindings: next_bindings,
        message,
    })
}

fn keymap_with_bindings(
    keymap: &TuiKeymap,
    context: &str,
    action: &str,
    keys: &[String],
) -> Result<TuiKeymap, String> {
    let mut keymap = keymap.clone();
    let slot = binding_slot(&mut keymap, context, action).ok_or_else(|| {
        format!("Unknown keymap action `{context}.{action}`. Reopen /keymap and choose an action.")
    })?;
    *slot = Some(match keys {
        [key] => KeybindingsSpec::One(KeybindingSpec(key.clone())),
        keys => KeybindingsSpec::Many(
            keys.iter()
                .map(|key| KeybindingSpec(key.clone()))
                .collect::<Vec<_>>(),
        ),
    });
    Ok(keymap)
}

/// Return the active config key specs for one runtime action.
///
/// This converts resolved [`crate::key_hint::KeyBinding`] values back into
/// canonical config strings for display and for edit operations that need to
/// preserve existing bindings. Callers should treat errors as stale UI state,
/// because valid menu entries should always point at known actions.
pub(crate) fn active_binding_specs(
    runtime_keymap: &RuntimeKeymap,
    context: &str,
    action: &str,
) -> Result<Vec<String>, String> {
    let action_id = keymap_action_id(context, action);
    if let Some(action_id) = action_id
        && let Some(specs) = runtime_keymap.chords.configured_specs(action_id)
    {
        return Ok(specs.to_vec());
    }

    let bindings = bindings_for_action(runtime_keymap, context, action).ok_or_else(|| {
        format!("Unknown keymap action `{context}.{action}`. Reopen /keymap and choose an action.")
    })?;
    let mut specs = bindings
        .iter()
        .filter(|binding| !crate::keymap::is_dispatch_token(**binding))
        .map(|binding| binding_to_config_key_spec(*binding))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(action_id) = action_id {
        let chords: Vec<_> = runtime_keymap
            .chords
            .bindings
            .iter()
            .filter(|binding| binding.action == action_id)
            .collect();
        let available: std::collections::HashSet<_> = chords
            .iter()
            .map(|binding| (binding.chord.prefix, binding.chord.completion))
            .collect();
        for binding in &chords {
            // An implicit default can yield to a different action on some leader aliases.
            // Only persist the symbolic form when all aliases remain available.
            let partial_leader = binding.spec.starts_with("leader ")
                && runtime_keymap
                    .chords
                    .leader
                    .iter()
                    .any(|prefix| !available.contains(&(*prefix, binding.chord.completion)));
            specs.push(if partial_leader {
                format!(
                    "{} {}",
                    binding_to_config_key_spec(binding.chord.prefix)?,
                    binding_to_config_key_spec(binding.chord.completion)?
                )
            } else {
                binding.spec.clone()
            });
        }
    }
    Ok(dedup_bindings(specs))
}

fn dedup_bindings(bindings: Vec<String>) -> Vec<String> {
    bindings.into_iter().fold(Vec::new(), |mut deduped, key| {
        if !deduped.contains(&key) {
            deduped.push(key);
        }
        deduped
    })
}

/// Remove the root-level custom binding for one action.
///
/// Clearing the slot with `None` is different from setting an empty binding
/// list: `None` restores default/global fallback behavior, while an empty list
/// explicitly unbinds the action in runtime resolution.
pub(crate) fn keymap_without_custom_binding(
    keymap: &TuiKeymap,
    context: &str,
    action: &str,
) -> Result<TuiKeymap, String> {
    let mut keymap = keymap.clone();
    let slot = binding_slot(&mut keymap, context, action).ok_or_else(|| {
        format!("Unknown keymap action `{context}.{action}`. Reopen /keymap and choose an action.")
    })?;
    *slot = None;
    Ok(keymap)
}

fn has_custom_binding(keymap: &TuiKeymap, context: &str, action: &str) -> Result<bool, String> {
    let mut keymap = keymap.clone();
    let slot = binding_slot(&mut keymap, context, action).ok_or_else(|| {
        format!("Unknown keymap action `{context}.{action}`. Reopen /keymap and choose an action.")
    })?;
    Ok(slot.is_some())
}

fn key_event_to_config_key_spec(key_event: KeyEvent) -> Result<String, String> {
    binding_to_config_key_spec(KeyBinding::from_event(key_event))
}

fn binding_to_config_key_spec(binding: KeyBinding) -> Result<String, String> {
    let (code, modifiers) = binding.parts();
    key_parts_to_config_key_spec(code, modifiers)
}

fn key_parts_to_config_key_spec(
    code: KeyCode,
    mut modifiers: KeyModifiers,
) -> Result<String, String> {
    let (code, normalized_modifiers) = crate::key_hint::normalize_key_parts(code, modifiers);
    modifiers = normalized_modifiers;

    let supported_modifiers =
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER;
    if !modifiers.difference(supported_modifiers).is_empty() {
        return Err(
            "Only ctrl, alt, shift, and super modifiers can be stored in `tui.keymap`.".to_string(),
        );
    }

    let key = match code {
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => {
            modifiers.insert(KeyModifiers::SHIFT);
            "tab".to_string()
        }
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "page-up".to_string(),
        KeyCode::PageDown => "page-down".to_string(),
        KeyCode::F(number) if (1..=MAX_FUNCTION_KEY).contains(&number) => format!("f{number}"),
        KeyCode::F(_) => {
            return Err(format!(
                "Only function keys F1 through F{MAX_FUNCTION_KEY} can be stored in `tui.keymap`."
            ));
        }
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(mut ch) => {
            if ch == '-' {
                return Ok(format_key_spec(modifiers, "minus"));
            }
            if !ch.is_ascii() || ch.is_ascii_control() {
                return Err("Only printable ASCII keys can be stored in `tui.keymap`.".to_string());
            }
            if ch.is_ascii_uppercase() {
                modifiers.insert(KeyModifiers::SHIFT);
                ch = ch.to_ascii_lowercase();
            }
            ch.to_string()
        }
        _ => {
            return Err("That key is not supported by `tui.keymap`.".to_string());
        }
    };

    Ok(format_key_spec(modifiers, &key))
}

fn format_key_spec(modifiers: KeyModifiers, key: &str) -> String {
    let mut parts = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl");
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt");
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift");
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        parts.push("super");
    }
    parts.push(key);
    parts.join("-")
}

#[cfg(test)]
#[path = "keymap_setup_tests.rs"]
mod tests;
