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

/// Compact, non-editable reference shown below the configurable navigation
/// pairs. Every row is a keyboard-shortcuts catalogue entry, so the guide
/// lists the same names and live chords as the action door and Settings,
/// and never promises a default the user has rebound.
pub(super) fn shortcut_reference_rows(shortcuts: &ShortcutConfig) -> Vec<(String, String)> {
    shortcut_reference_groups(shortcuts)
        .into_iter()
        .flatten()
        .collect()
}

/// [`shortcut_reference_rows`] in the guide's semantic groups, in the order
/// of their titles: windows and tabs, search and the desktop, capture. A
/// group holds what belongs to it, never a neighbour's row to fill it up.
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

/// Rows a semantic group has room for: the longest group's.
pub(super) const SHORTCUT_REFERENCE_GROUP_ROWS: usize = 6;

/// Number of semantic reference groups.
const SHORTCUT_REFERENCE_GROUPS: usize = 3;

/// A reference group's card: its header, then its six rows, the last row's
/// trailing gap closing the card.
pub(super) fn shortcut_reference_group_height(rhythm: GuideRhythm) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a constant of six rows converts exactly"
    )]
    let rows = SHORTCUT_REFERENCE_GROUP_ROWS as f64;
    shortcut_group_rows_top(rhythm) + rows * shortcut_reference_step()
}

/// Where a group's first row starts below the top of its surface: the
/// header, a tight gap in. The header's own height already centres its
/// title with room above and below, so no gap follows it.
pub(super) fn shortcut_group_rows_top(rhythm: GuideRhythm) -> f64 {
    rhythm.inset + rhythm.header
}

/// Columns of the editable navigation pairs.
pub(super) fn shortcut_navigation_columns(body_width: f64) -> usize {
    if body_width >= 620.0 { 2 } else { 1 }
}

/// Height of the editable-navigation surface: its header plus one row per
/// pair, derived from the row count so a surface can never be shorter than
/// the rows it holds.
pub(super) fn shortcut_navigation_height(pairs: usize, columns: usize, rhythm: GuideRhythm) -> f64 {
    let rows = pairs.div_ceil(columns.max(1));
    #[allow(
        clippy::cast_precision_loss,
        reason = "a handful of rows converts exactly"
    )]
    let rows = rows as f64;
    shortcut_group_rows_top(rhythm) + rows * rhythm.navigation_step + 4.0
}

/// The keyboard guide's steps in a theme, resolved where the theme is in
/// hand so the guide's hit-testing and paint share one answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GuideRhythm {
    /// Distance between navigation rows: the action's caption line, its
    /// keycaps under it, and a tight gap before the next row.
    pub(super) navigation_step: f64,
    /// A group header's height (`ui::widget::SectionHeaderFace::group`).
    pub(super) header: f64,
    /// The gap around a header inside its surface.
    pub(super) inset: f64,
}

pub(super) fn shortcut_guide_rhythm(theme: &ui::theme::Theme) -> GuideRhythm {
    let tight = f64::from(theme.tokens.spacing.rhythm.tight);
    let header = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
        ui::widget::SectionHeaderFace::group(&t("guide-essentials-heading")).height(
            text,
            theme,
            f64::MAX,
        )
    });
    GuideRhythm {
        navigation_step: 18.0 + ui::widget::InputHint::keycap_height(theme) + tight,
        header,
        inset: tight,
    }
}

/// A reference row's chord as `KeyHints`: one keys-only hint whose
/// alternatives follow the first chord after "or".
pub(super) fn shortcut_chord_hints(chord_text: &str) -> Vec<ui::widget::KeyHint> {
    ui::widget::KeyHint::any_of(shortcut_chord_keycaps(chord_text), String::new())
        .into_iter()
        .collect()
}

/// Lines of notes under the reference: the fixed pointer line, the app tab
/// chords line, your action chords and the vim-mode hint when present.
pub(super) fn shortcut_note_lines(has_summary: bool, vim_mode: bool) -> usize {
    // The pointer, strip-number and app tab chord notes are always there.
    3_usize
        .saturating_add(usize::from(has_summary))
        .saturating_add(usize::from(vim_mode))
}

/// Height of the pointer notes under the reference: a rule, then one line per
/// note.
pub(super) fn shortcut_notes_height(lines: usize) -> f64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "at most four note lines convert exactly"
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
    rhythm: GuideRhythm,
) -> f64 {
    let navigation =
        shortcut_navigation_height(pairs, shortcut_navigation_columns(body_width), rhythm);
    let group_rows = SHORTCUT_REFERENCE_GROUPS.div_ceil(shortcut_reference_columns(body_width));
    #[allow(
        clippy::cast_precision_loss,
        reason = "at most three group rows convert exactly"
    )]
    let group_rows = group_rows as f64;
    let groups = group_rows * shortcut_reference_group_height(rhythm) + (group_rows - 1.0) * 12.0;
    // The essentials header sits between the navigation surface and the
    // groups, its own height spacing it from both.
    navigation + rhythm.header + groups + shortcut_notes_height(note_lines)
}

/// The keyboard guide's natural height: its title block, the body, and the
/// bottom margin.
pub(super) fn shortcut_guide_height(
    body_width: f64,
    pairs: usize,
    note_lines: usize,
    rhythm: GuideRhythm,
) -> f64 {
    98.0 + shortcut_guide_content_height(body_width, pairs, note_lines, rhythm) + 22.0
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
        // Eight pairs in two columns are four rows under the group header:
        // the surface must contain the last row's keycaps.
        let rhythm = shortcut_guide_rhythm(&ui::theme::Theme::dark());
        let height = shortcut_navigation_height(8, 2, rhythm);
        assert!(height >= shortcut_group_rows_top(rhythm) + 4.0 * rhythm.navigation_step);
        assert!(shortcut_navigation_height(8, 1, rhythm) > height);
    }

    #[test]
    fn reference_rows_leave_room_for_label_and_keycaps() {
        let theme = ui::theme::Theme::dark();
        let label = f64::from(theme.tokens.typography.size(ui::TextRole::Caption));
        let keycap = ui::widget::InputHint::keycap_height(&theme);
        assert!(shortcut_reference_step() >= label * 1.3 + 4.0 + keycap);
        let rhythm = shortcut_guide_rhythm(&theme);
        assert!(rhythm.header >= label, "a group header holds its title");
        assert!(
            shortcut_reference_group_height(rhythm)
                >= shortcut_group_rows_top(rhythm) + 6.0 * shortcut_reference_step()
        );
    }

    #[test]
    fn the_keyboard_guide_fits_a_1600_by_900_desktop() {
        // The guide is 960 wide (a 912 wide body) and keeps 32 px of the
        // desktop above and below it; with the default notes it needs no
        // scrolling there, in either theme.
        for theme in [ui::theme::Theme::dark(), ui::theme::Theme::light()] {
            let rhythm = shortcut_guide_rhythm(&theme);
            let needed = shortcut_guide_height(
                912.0,
                super::super::shortcut_config::SHORTCUT_GUIDE_ROWS.len(),
                shortcut_note_lines(false, false),
                rhythm,
            );
            assert!(needed <= 900.0 - 64.0, "the guide needs {needed} px");
        }
    }

    #[test]
    fn narrow_bodies_use_fewer_columns() {
        assert_eq!(shortcut_reference_columns(912.0), 3);
        assert_eq!(shortcut_reference_columns(560.0), 2);
        assert_eq!(shortcut_reference_columns(300.0), 1);
        let rhythm = shortcut_guide_rhythm(&ui::theme::Theme::dark());
        assert!(
            shortcut_guide_content_height(300.0, 8, 1, rhythm)
                > shortcut_guide_content_height(912.0, 8, 1, rhythm)
        );
    }
}
