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
    shortcuts
        .bindings
        .iter()
        .find_map(|(action, chord)| {
            shortcut_chord_matches(logical_key, modifiers, chord).then_some(*action)
        })
        .or_else(|| is_bare_f11(logical_key, modifiers).then_some(ShortcutAction::ToggleFullscreen))
}

/// F11 is every desktop's full-screen key; it toggles the same real
/// fullscreen as the configurable chord, unless the person bound it to
/// something else.
fn is_bare_f11(logical_key: &Key, modifiers: Modifiers) -> bool {
    *logical_key == Key::Named(NamedKey::F11) && modifiers.state().is_empty()
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
        // A lone Super tap searches too; the keyboard map lists every
        // alias, the guide keeps one row per action.
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

/// How many reference groups sit side by side in a guide body of this
/// width. Fewer columns on a narrow body keep every chord inside its cell
/// instead of clipping it.
pub(super) fn shortcut_reference_columns(body_width: f64) -> usize {
    if body_width >= 640.0 {
        3
    } else if body_width >= 460.0 {
        2
    } else {
        1
    }
}

/// Vertical pitch of one reference row: its label, then its keycaps on a
/// line of their own, so no chord is ever laid over its own label.
pub(super) fn shortcut_reference_step() -> f64 {
    54.0
}

/// Number of reference rows in one semantic group.
pub(super) const SHORTCUT_REFERENCE_GROUP_ROWS: usize = 6;

/// Number of semantic reference groups.
const SHORTCUT_REFERENCE_GROUPS: usize = 3;

pub(super) fn shortcut_reference_group_height() -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a constant of six rows converts exactly"
    )]
    let rows = SHORTCUT_REFERENCE_GROUP_ROWS as f64;
    29.0 + rows * shortcut_reference_step() + 4.0
}

/// Columns of the editable navigation pairs.
pub(super) fn shortcut_navigation_columns(body_width: f64) -> usize {
    if body_width >= 620.0 { 2 } else { 1 }
}

/// Height of the editable-navigation surface: heading plus one 44 px row per
/// pair, derived from the row count so a surface can never be shorter than
/// the rows it holds.
pub(super) fn shortcut_navigation_height(pairs: usize, columns: usize, step: f64) -> f64 {
    let rows = pairs.div_ceil(columns.max(1));
    #[allow(
        clippy::cast_precision_loss,
        reason = "a handful of rows converts exactly"
    )]
    let rows = rows as f64;
    31.0 + rows * step + 4.0
}

/// Distance between navigation rows in `theme`: the action's caption line,
/// its keycaps under it, and a tight gap before the next row.
pub(super) fn shortcut_navigation_step(theme: &ui::theme::Theme) -> f64 {
    18.0 + ui::widget::InputHint::keycap_height(theme)
        + f64::from(theme.tokens.spacing.rhythm.tight)
}

/// Lines of pointer notes: the fixed pointer line, your action chords and the
/// vim-mode hint when present.
pub(super) fn shortcut_note_lines(has_summary: bool, vim_mode: bool) -> usize {
    1_usize
        .saturating_add(usize::from(has_summary))
        .saturating_add(usize::from(vim_mode))
}

/// Height of the pointer notes under the reference: a rule, then one line per
/// note.
pub(super) fn shortcut_notes_height(lines: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "at most three note lines convert exactly"
    )]
    let lines = lines as f64;
    4.0 + 14.0 + lines * 18.0
}

/// Intrinsic height of the keyboard guide body for `body_width`: everything
/// the guide paints, so the panel is exactly as tall as its content (up to
/// the window) and scrolls only when the window is shorter.
pub(super) fn shortcut_guide_content_height(
    body_width: f64,
    pairs: usize,
    note_lines: usize,
    navigation_step: f64,
) -> f64 {
    let navigation = shortcut_navigation_height(
        pairs,
        shortcut_navigation_columns(body_width),
        navigation_step,
    );
    let group_rows = SHORTCUT_REFERENCE_GROUPS.div_ceil(shortcut_reference_columns(body_width));
    #[allow(
        clippy::cast_precision_loss,
        reason = "at most three group rows convert exactly"
    )]
    let group_rows = group_rows as f64;
    let groups = group_rows * shortcut_reference_group_height() + (group_rows - 1.0) * 12.0;
    navigation + 12.0 + 18.0 + groups + shortcut_notes_height(note_lines)
}

/// A chord as keycap labels, one list per alternative: `Alt+F4  /  Super+W`
/// is two caps rows, `Super++` ends in a literal plus key.
pub(super) fn shortcut_chord_keycaps(chord_text: &str) -> Vec<Vec<String>> {
    chord_text
        .split("  /  ")
        .map(|alternative| {
            let alternative = alternative.trim();
            let (head, plus) = match alternative.strip_suffix('+') {
                Some(head) if !head.is_empty() => (head.strip_suffix('+').unwrap_or(head), true),
                _ => (alternative, false),
            };
            let mut keys: Vec<String> = head
                .split('+')
                .filter(|key| !key.is_empty())
                .map(str::to_owned)
                .collect();
            if plus {
                keys.push("+".to_owned());
            }
            keys
        })
        .collect()
}

pub(super) fn shortcut_guide_chord(chord: &KeyChord) -> String {
    keyboard_shortcuts::chord_text(chord)
}

#[cfg(test)]
mod f11_tests {
    use super::*;

    #[test]
    fn bare_f11_toggles_fullscreen_unless_bound_to_something_else() {
        let f11 = Key::Named(NamedKey::F11);
        let none = Modifiers::default();
        let mut shortcuts = ShortcutConfig::default();
        assert_eq!(
            configured_shortcut_action(&shortcuts, &f11, none),
            Some(ShortcutAction::ToggleFullscreen)
        );
        let ctrl = Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        assert_eq!(configured_shortcut_action(&shortcuts, &f11, ctrl), None);
        shortcuts
            .set(
                ShortcutAction::Search,
                KeyChord::parse("F11").expect("F11 parses"),
            )
            .expect("F11 is free");
        assert_eq!(
            configured_shortcut_action(&shortcuts, &f11, none),
            Some(ShortcutAction::Search),
            "a person's own binding wins"
        );
    }
}

#[cfg(test)]
mod guide_layout_tests {
    use super::*;

    #[test]
    fn chords_split_into_caps_per_alternative() {
        assert_eq!(
            shortcut_chord_keycaps("Alt+F4  /  Super+W"),
            vec![vec!["Alt", "F4"], vec!["Super", "W"]]
        );
        assert_eq!(shortcut_chord_keycaps("Super++"), vec![vec!["Super", "+"]]);
        assert_eq!(shortcut_chord_keycaps("Super+."), vec![vec!["Super", "."]]);
        assert_eq!(
            shortcut_chord_keycaps("Print Screen"),
            vec![vec!["Print Screen"]]
        );
    }

    #[test]
    fn navigation_surface_holds_every_row() {
        // Eight pairs in two columns are four rows of 44 px under a 31 px
        // heading: the surface must contain the last row's keycaps.
        let step = shortcut_navigation_step(&ui::theme::Theme::dark());
        let height = shortcut_navigation_height(8, 2, step);
        assert!(height >= 31.0 + 4.0 * step);
        assert!(shortcut_navigation_height(8, 1, step) > height);
    }

    #[test]
    fn reference_rows_leave_room_for_label_and_keycaps() {
        let theme = ui::theme::Theme::dark();
        let label = f64::from(theme.tokens.typography.size(ui::TextRole::Caption));
        let keycap = ui::widget::InputHint::keycap_height(&theme);
        assert!(shortcut_reference_step() >= label * 1.3 + 4.0 + keycap);
        assert!(shortcut_reference_group_height() >= 29.0 + 6.0 * shortcut_reference_step());
    }

    #[test]
    fn narrow_bodies_use_fewer_columns() {
        assert_eq!(shortcut_reference_columns(912.0), 3);
        assert_eq!(shortcut_reference_columns(560.0), 2);
        assert_eq!(shortcut_reference_columns(300.0), 1);
        assert!(
            shortcut_guide_content_height(300.0, 8, 1, 44.0)
                > shortcut_guide_content_height(912.0, 8, 1, 44.0)
        );
    }
}
