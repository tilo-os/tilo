//! Shortcut normalization and reference-guide policy.
//!
//! Keeping matching, capture, labels, and guide layout together ensures the
//! input pre-router and the visible shortcut guide describe the same bindings.

use std::collections::BTreeSet;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
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

pub(super) fn shortcut_action_label(action: ShortcutAction) -> &'static str {
    match action {
        ShortcutAction::Search => "open search",
        ShortcutAction::WorkspaceOverview => "open the workspace overview",
        ShortcutAction::Lock => "lock the screen",
        ShortcutAction::QuickSettings => "open Quick Settings",
        ShortcutAction::ShortcutHelp => "open this guide",
        ShortcutAction::FocusPreviousWindow => "focus the previous window",
        ShortcutAction::FocusNextWindow => "focus the next window",
        ShortcutAction::PreviousTab => "select the previous tab",
        ShortcutAction::NextTab => "select the next tab",
        ShortcutAction::PreviousStrip => "select the previous strip",
        ShortcutAction::NextStrip => "select the next strip",
        ShortcutAction::PreviousWorkspace => "select the previous workspace",
        ShortcutAction::NextWorkspace => "select the next workspace",
        ShortcutAction::ToggleMaximize => "maximize or restore the window",
        ShortcutAction::DetachTab => "move the tab into a new group",
        ShortcutAction::AttachTabLeft => "move the tab into the group on the left",
        ShortcutAction::AttachTabRight => "move the tab into the group on the right",
        ShortcutAction::MoveGroupLeft => "move the group left",
        ShortcutAction::MoveGroupRight => "move the group right",
        ShortcutAction::ResizeNarrower => "make the window narrower",
        ShortcutAction::ResizeWider => "make the window wider",
    }
}

/// Compact, non-editable reference shown below the configurable navigation
/// pairs. Configurable actions use their live chord so the guide never
/// promises a default the user has rebound.
pub(super) fn shortcut_reference_rows(shortcuts: &ShortcutConfig) -> Vec<(&'static str, String)> {
    let chord = |action| {
        shortcuts
            .bindings
            .get(&action)
            .map_or_else(|| "Unassigned".into(), shortcut_guide_chord)
    };
    vec![
        ("Switch windows", "Alt+Tab".into()),
        ("Close tab", "Alt+F4  /  Super+W".into()),
        ("Change workspace", "Super+Page Up / Down".into()),
        ("New window", "Super+N".into()),
        ("Add tab here", "Super+T".into()),
        ("Duplicate tab", "Super+D".into()),
        ("Global search", chord(ShortcutAction::Search)),
        ("Global search", "Super+Space".into()),
        ("Overview", chord(ShortcutAction::WorkspaceOverview)),
        ("Quick Settings", chord(ShortcutAction::QuickSettings)),
        ("Workspace wheel", "Super+S".into()),
        ("Lock", chord(ShortcutAction::Lock)),
        ("Screenshot", "Print Screen".into()),
        ("Wireless display", "Super+P".into()),
        ("Capture screenshot", "Super+Shift+S".into()),
        ("Open terminal", "Super+Enter".into()),
        ("Picture in picture", "Super+Shift+P".into()),
        ("Save recent replay", "Super+Shift+R".into()),
    ]
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
    let display = chord.display();
    let Some((modifiers, key)) = display.rsplit_once('+') else {
        return display;
    };
    if key.chars().count() == 1 {
        format!("{modifiers}+{}", key.to_uppercase())
    } else {
        display
    }
}
