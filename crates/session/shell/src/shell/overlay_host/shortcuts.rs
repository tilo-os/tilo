//! Shortcut normalization and reference-guide policy.
//!
//! Keeping matching, capture, labels, and guide layout together ensures the
//! input pre-router and the visible shortcut guide describe the same bindings.

use std::collections::BTreeSet;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};

use crate::shell::fixed_chords::FixedChordAction;
use crate::shell::i18n::{t, t_args};
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

pub(super) fn shortcut_action_label(action: ShortcutAction) -> String {
    let key = match action {
        ShortcutAction::Search => "shortcut-action-search",
        ShortcutAction::WorkspaceOverview => "shortcut-action-workspace-overview",
        ShortcutAction::Lock => "shortcut-action-lock",
        ShortcutAction::QuickSettings => "shortcut-action-quick-settings",
        ShortcutAction::ShortcutHelp => "shortcut-action-shortcut-help",
        ShortcutAction::FocusPreviousWindow => "shortcut-action-focus-previous-window",
        ShortcutAction::FocusNextWindow => "shortcut-action-focus-next-window",
        ShortcutAction::PreviousTab => "shortcut-action-previous-tab",
        ShortcutAction::NextTab => "shortcut-action-next-tab",
        ShortcutAction::PreviousStrip => "shortcut-action-previous-strip",
        ShortcutAction::NextStrip => "shortcut-action-next-strip",
        ShortcutAction::PreviousWorkspace => "shortcut-action-previous-workspace",
        ShortcutAction::NextWorkspace => "shortcut-action-next-workspace",
        ShortcutAction::ToggleMaximize => "shortcut-action-toggle-maximize",
        ShortcutAction::DetachTab => "shortcut-action-detach-tab",
        ShortcutAction::AttachTabLeft => "shortcut-action-attach-tab-left",
        ShortcutAction::AttachTabRight => "shortcut-action-attach-tab-right",
        ShortcutAction::MoveGroupLeft => "shortcut-action-move-group-left",
        ShortcutAction::MoveGroupRight => "shortcut-action-move-group-right",
        ShortcutAction::MoveGroupToPreviousDisplay => {
            "shortcut-action-move-group-to-previous-display"
        }
        ShortcutAction::MoveGroupToNextDisplay => "shortcut-action-move-group-to-next-display",
        ShortcutAction::ResizeNarrower => "shortcut-action-resize-narrower",
        ShortcutAction::ResizeWider => "shortcut-action-resize-wider",
        ShortcutAction::ReopenClosedTab => "shortcut-action-reopen-closed-tab",
        ShortcutAction::FocusGroup1 => "shortcut-action-focus-group-1",
        ShortcutAction::FocusGroup2 => "shortcut-action-focus-group-2",
        ShortcutAction::FocusGroup3 => "shortcut-action-focus-group-3",
        ShortcutAction::FocusGroup4 => "shortcut-action-focus-group-4",
        ShortcutAction::FocusGroup5 => "shortcut-action-focus-group-5",
        ShortcutAction::FocusGroup6 => "shortcut-action-focus-group-6",
        ShortcutAction::FocusGroup7 => "shortcut-action-focus-group-7",
        ShortcutAction::FocusGroup8 => "shortcut-action-focus-group-8",
        ShortcutAction::FocusGroup9 => "shortcut-action-focus-group-9",
        ShortcutAction::OpenFiles => "shortcut-action-open-files",
        ShortcutAction::OpenTerminal => "shortcut-action-open-terminal",
        ShortcutAction::ClipboardHistory => "shortcut-action-clipboard-history",
        ShortcutAction::Notifications => "shortcut-action-notifications",
        ShortcutAction::ToggleFullscreen => "shortcut-action-toggle-fullscreen",
        ShortcutAction::ToggleMagnifier => "shortcut-action-toggle-magnifier",
        ShortcutAction::MagnifierZoomIn => "shortcut-action-magnifier-zoom-in",
        ShortcutAction::MagnifierZoomOut => "shortcut-action-magnifier-zoom-out",
        ShortcutAction::PowerMenu => "shortcut-action-power-menu",
        ShortcutAction::NextInputSource => "shortcut-action-next-input-source",
        ShortcutAction::EmojiPicker => "shortcut-action-emoji-picker",
    };
    t(key)
}

/// Compact, non-editable reference shown below the configurable navigation
/// pairs. Configurable actions use their live chord so the guide never
/// promises a default the user has rebound; fixed chords come from the
/// shell's one fixed-chord table.
pub(super) fn shortcut_reference_rows(shortcuts: &ShortcutConfig) -> Vec<(String, String)> {
    let chord = |action| {
        shortcuts
            .bindings
            .get(&action)
            .map_or_else(|| t("guide-unassigned"), shortcut_guide_chord)
    };
    let fixed = |action| {
        crate::shell::fixed_chords::entry_for(action).map(|entry| (entry.label(), entry.display()))
    };
    let editable = |key: &str, action| Some((t(key), chord(action)));
    [
        fixed(FixedChordAction::SwitchWindows),
        fixed(FixedChordAction::CloseTab),
        // Changing workspace is already an editable navigation pair above;
        // its essentials slot names how to undo a close instead.
        editable("guide-reopen-closed-tab", ShortcutAction::ReopenClosedTab),
        fixed(FixedChordAction::NewWindow),
        fixed(FixedChordAction::AddTab),
        fixed(FixedChordAction::DuplicateTab),
        editable("shortcut-global-search", ShortcutAction::Search),
        // Super+Space and a lone Super tap search too; the keyboard map
        // lists every alias, the guide keeps one row per action.
        editable("guide-emoji", ShortcutAction::EmojiPicker),
        editable("guide-overview", ShortcutAction::WorkspaceOverview),
        editable("guide-quick-settings", ShortcutAction::QuickSettings),
        fixed(FixedChordAction::WorkspaceWheel),
        editable("shortcut-lock", ShortcutAction::Lock),
        fixed(FixedChordAction::Screenshot),
        fixed(FixedChordAction::DisplayLayout),
        fixed(FixedChordAction::CaptureScreenshot),
        fixed(FixedChordAction::OpenTerminal),
        fixed(FixedChordAction::PictureInPicture),
        fixed(FixedChordAction::SaveRecentReplay),
    ]
    .into_iter()
    .flatten()
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
