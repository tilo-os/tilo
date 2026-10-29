//! Shortcut normalization and reference-guide policy.
//!
//! Keeping matching, capture, labels, and guide layout together ensures the
//! input pre-router and the visible shortcut guide describe the same bindings.

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};

use crate::shell::i18n::{t, t_args};
use crate::shell::keyboard_shortcuts;
use settings_store::keyboard_shortcuts::catalogue;
#[cfg(test)]
use winit::event::Modifiers;
#[cfg(test)]
use winit::keyboard::{Key, NamedKey};

pub(crate) use tilo_desktop::shortcuts::shortcut_chord_matches;
pub(super) use tilo_desktop::shortcuts::{configured_shortcut_action, shortcut_chord_from_event};

#[cfg(test)]
pub(super) fn configured_shortcut_matches(
    shortcuts: &ShortcutConfig,
    logical_key: &Key,
    modifiers: Modifiers,
) -> bool {
    configured_shortcut_action(shortcuts, logical_key, modifiers).is_some()
}

/// What `action` is called, as the keyboard-shortcuts catalogue names it.
pub(super) fn shortcut_action_label(action: ShortcutAction) -> String {
    t(&action.label_key())
}

/// The compact, non-editable reference under the configurable navigation
/// pairs, in the guide's semantic groups, in the order of their titles:
/// windows and tabs, search and the desktop, capture. Every row is a
/// keyboard-shortcuts catalogue entry, so the guide lists the same names and
/// live chords as the action door and Settings, and never promises a default
/// the user has rebound. A group holds what belongs to it, never a
/// neighbour's row to fill it up.
pub(super) fn shortcut_reference_groups(shortcuts: &ShortcutConfig) -> Vec<Vec<(String, String)>> {
    let entries = catalogue(shortcuts);
    let groups: [&[&str]; SHORTCUT_REFERENCE_GROUPS] = [
        &[
            "fixed-switch-windows",
            "fixed-close-tab",
            // Changing workspace is already an editable navigation pair
            // above; its essentials slot names how to undo a close instead.
            ShortcutAction::ReopenClosedTab.id(),
            "fixed-new-window",
            "fixed-duplicate-tab",
        ],
        &[
            ShortcutAction::Search.id(),
            // A lone Super tap opens the Super surface, whose field is this
            // search; the keyboard map lists every alias, the guide keeps one
            // row per action.
            ShortcutAction::EmojiPicker.id(),
            ShortcutAction::WorkspaceOverview.id(),
            ShortcutAction::QuickSettings.id(),
            "fixed-strip-switcher",
            ShortcutAction::Lock.id(),
        ],
        &[
            "fixed-screenshot",
            "fixed-display-layout",
            "fixed-capture-screenshot",
            "fixed-open-terminal",
            "fixed-picture-in-picture",
            "fixed-save-recent-replay",
        ],
    ];
    groups
        .iter()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| entries.iter().find(|entry| entry.id == *id))
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
        })
        .collect()
}

/// The guide's note on going straight to a strip, with the live chords of
/// the first and the last strip: "Super+1 … Super+9" by default.
pub(super) fn shortcut_strip_jump_note(shortcuts: &ShortcutConfig) -> String {
    let chord = |action: ShortcutAction| {
        shortcuts
            .bindings
            .get(&action)
            .map_or_else(|| t("guide-unassigned-inline"), shortcut_guide_chord)
    };
    t_args(
        "guide-strip-jump",
        &[
            ("first", chord(ShortcutAction::FocusStrip1).into()),
            ("last", chord(ShortcutAction::FocusStrip9).into()),
        ],
    )
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

/// The guide's note on the app tab chords: Ctrl+Tab, Ctrl+Shift+Tab and
/// Ctrl+Shift+T reach the focused app first and change tabs only where it
/// leaves them unhandled, while the Super spellings always do. Written in the
/// active layout's keycaps, so a German keyboard reads `Strg`.
pub(super) fn shortcut_app_tab_note() -> String {
    let chord = |text: &str| {
        KeyChord::parse(text)
            .map_or_else(|_| text.to_string(), |chord| shortcut_guide_chord(&chord))
    };
    t_args(
        "guide-app-tab-chords",
        &[
            ("next", chord("Ctrl+Tab").into()),
            ("previous", chord("Ctrl+Shift+Tab").into()),
            ("reopen", chord("Ctrl+Shift+T").into()),
        ],
    )
}

/// Number of semantic reference groups.
const SHORTCUT_REFERENCE_GROUPS: usize = 3;

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
}
