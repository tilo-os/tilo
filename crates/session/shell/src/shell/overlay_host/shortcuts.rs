//! Shortcut normalization and reference-guide policy.
//!
//! Keeping matching, capture, labels, and guide layout together ensures the
//! input pre-router and the visible shortcut guide describe the same bindings.

use std::collections::BTreeSet;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};

use crate::shell::i18n::{t, t_args};
use crate::shell::keyboard_shortcuts;
use settings_store::keyboard_shortcuts::catalogue;
use winit::event::Modifiers;
use winit::keyboard::{Key, NamedKey};

/// Match a persisted shortcut against a live winit key event.
///
/// The native compositor uses this for session-owned bindings too. Keeping
/// that path on the same key-name translation as the shortcut editor avoids
/// claiming a configured named key before a client sees it and then failing
/// to perform its action.
pub(crate) fn shortcut_chord_matches(
    logical_key: &Key,
    modifiers: Modifiers,
    chord: &KeyChord,
) -> bool {
    let state = modifiers.state();
    let key_matches = shortcut_key_name(logical_key).is_some_and(|key| {
        chord.key.eq_ignore_ascii_case(&key)
            || state.shift_key()
                && unshifted_shortcut_key(&key)
                    .is_some_and(|base| chord.key.eq_ignore_ascii_case(base))
    });
    key_matches
        && chord.modifiers.contains("Super") == state.super_key()
        && chord.modifiers.contains("Ctrl") == state.control_key()
        && chord.modifiers.contains("Alt") == state.alt_key()
        && chord.modifiers.contains("Shift") == state.shift_key()
}

/// Winit reports the logical character produced by Shift, while shortcut
/// configuration stores the physical punctuation printed on the key.
fn unshifted_shortcut_key(key: &str) -> Option<&'static str> {
    Some(match key {
        "?" => "/",
        ":" => ";",
        "\"" => "'",
        "<" => ",",
        ">" => ".",
        "_" => "-",
        "+" => "=",
        "{" => "[",
        "}" => "]",
        "|" => "\\",
        "~" => "`",
        "!" => "1",
        "@" => "2",
        "#" => "3",
        "$" => "4",
        "%" => "5",
        "^" => "6",
        "&" => "7",
        "*" => "8",
        "(" => "9",
        ")" => "0",
        _ => return None,
    })
}

/// The sole configurable shortcut resolver. The input pre-router owns a press
/// exactly when this returns an action; the dispatcher performs that result.
pub(super) fn configured_shortcut_action(
    shortcuts: &ShortcutConfig,
    logical_key: &Key,
    modifiers: Modifiers,
) -> Option<ShortcutAction> {
    shortcuts.bindings.iter().find_map(|(action, chord)| {
        shortcut_chord_matches(logical_key, modifiers, chord).then_some(*action)
    })
}

#[cfg(test)]
pub(super) fn configured_shortcut_matches(
    shortcuts: &ShortcutConfig,
    logical_key: &Key,
    modifiers: Modifiers,
) -> bool {
    configured_shortcut_action(shortcuts, logical_key, modifiers).is_some()
}

fn shortcut_key_name(logical_key: &Key) -> Option<String> {
    let name = match logical_key {
        Key::Character(key) if key.as_str() == " " => "space".to_string(),
        Key::Character(key) if key.chars().count() == 1 => key.to_ascii_lowercase(),
        Key::Named(named) => match named {
            NamedKey::ArrowLeft => "left",
            NamedKey::ArrowRight => "right",
            NamedKey::ArrowUp => "up",
            NamedKey::ArrowDown => "down",
            NamedKey::Tab => "tab",
            NamedKey::Enter => "enter",
            NamedKey::Space => "space",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Backspace => "backspace",
            NamedKey::Delete => "delete",
            NamedKey::Insert => "insert",
            NamedKey::PrintScreen => "printscreen",
            NamedKey::F1 => "f1",
            NamedKey::F2 => "f2",
            NamedKey::F3 => "f3",
            NamedKey::F4 => "f4",
            NamedKey::F5 => "f5",
            NamedKey::F6 => "f6",
            NamedKey::F7 => "f7",
            NamedKey::F8 => "f8",
            NamedKey::F9 => "f9",
            NamedKey::F10 => "f10",
            NamedKey::F11 => "f11",
            NamedKey::F12 => "f12",
            _ => return None,
        }
        .to_string(),
        _ => return None,
    };
    Some(name)
}

pub(super) fn shortcut_chord_from_event(
    logical_key: &Key,
    modifiers: Modifiers,
) -> Option<KeyChord> {
    let key = shortcut_key_name(logical_key)?;
    let state = modifiers.state();
    let mut names = BTreeSet::new();
    if state.super_key() {
        names.insert("Super".to_string());
    }
    if state.control_key() {
        names.insert("Ctrl".to_string());
    }
    if state.alt_key() {
        names.insert("Alt".to_string());
    }
    if state.shift_key() {
        names.insert("Shift".to_string());
    }
    Some(KeyChord {
        modifiers: names,
        key,
    })
}

/// What `action` is called, as the keyboard-shortcuts catalogue names it.
pub(super) fn shortcut_action_label(action: ShortcutAction) -> String {
    t(&action.label_key())
}

/// Compact, non-editable reference shown below the configurable navigation
/// pairs. Every row is a keyboard-shortcuts catalogue entry, so the guide
/// lists the same names and live chords as the action door and Settings,
/// and never promises a default the user has rebound.
pub(super) fn shortcut_reference_rows(shortcuts: &ShortcutConfig) -> Vec<(String, String)> {
    let entries = catalogue(shortcuts);
    [
        "fixed-switch-windows",
        "fixed-close-tab",
        // Changing workspace is already an editable navigation pair above;
        // its essentials slot names how to undo a close instead.
        ShortcutAction::ReopenClosedTab.id(),
        "fixed-new-window",
        "fixed-add-tab",
        "fixed-duplicate-tab",
        ShortcutAction::Search.id(),
        // Super+Space and a lone Super tap search too; the keyboard map
        // lists every alias, the guide keeps one row per action.
        ShortcutAction::EmojiPicker.id(),
        ShortcutAction::WorkspaceOverview.id(),
        ShortcutAction::QuickSettings.id(),
        "fixed-workspace-wheel",
        ShortcutAction::Lock.id(),
        "fixed-screenshot",
        "fixed-display-layout",
        "fixed-capture-screenshot",
        "fixed-open-terminal",
        "fixed-picture-in-picture",
        "fixed-save-recent-replay",
    ]
    .into_iter()
    .filter_map(|id| entries.iter().find(|entry| entry.id == id))
    .map(|entry| {
        let chords = if entry.chords.is_empty() {
            t("guide-unassigned")
        } else {
            entry
                .chords
                .iter()
                .map(shortcut_guide_chord)
                .collect::<Vec<_>>()
                .join("  /  ")
        };
        (keyboard_shortcuts::label(entry), chords)
    })
    .collect()
}

/// The chords bound to published actions, as the guide lists them: what
/// each runs, then its chord.
pub(super) fn shortcut_action_rows(shortcuts: &ShortcutConfig) -> Vec<(String, String)> {
    shortcuts
        .actions
        .iter()
        .map(|bound| (bound.label.clone(), shortcut_guide_chord(&bound.chord)))
        .collect()
}

/// One line for the guide naming every action chord, or `None` when there
/// is none.
pub(super) fn shortcut_action_summary(shortcuts: &ShortcutConfig) -> Option<String> {
    let rows = shortcut_action_rows(shortcuts);
    (!rows.is_empty()).then(|| {
        let list = rows
            .into_iter()
            .map(|(label, chord)| format!("{label} · {chord}"))
            .collect::<Vec<_>>()
            .join("   ");
        t_args("guide-your-shortcuts", &[("shortcuts", list.into())])
    })
}

pub(super) fn shortcut_reference_columns(_body_width: f64) -> usize {
    3
}

pub(super) fn shortcut_reference_step(compact: bool) -> f64 {
    if compact { 31.0 } else { 34.0 }
}

pub(super) fn shortcut_reference_group_height(compact: bool) -> f64 {
    if compact { 221.0 } else { 238.0 }
}

pub(super) fn shortcut_guide_chord(chord: &KeyChord) -> String {
    keyboard_shortcuts::chord_text(chord)
}
