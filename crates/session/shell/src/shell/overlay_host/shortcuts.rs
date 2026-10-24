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

/// Rows a semantic group holds at most: the longest group's.
#[cfg(test)]
pub(super) const SHORTCUT_REFERENCE_GROUP_ROWS: usize = 6;

/// Number of semantic reference groups.
const SHORTCUT_REFERENCE_GROUPS: usize = 3;

/// A reference group's card: its header, then its rows, the last row's
/// trailing gap closing the card.
pub(super) fn shortcut_reference_group_height(steps: &[f64], rhythm: GuideRhythm) -> f64 {
    shortcut_group_rows_top(rhythm) + steps.iter().sum::<f64>()
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

/// Height of the editable-navigation surface: its header plus each row's
/// measured step, so a surface is never shorter than the rows it holds.
pub(super) fn shortcut_navigation_height(rows: &GuideRows, rhythm: GuideRhythm) -> f64 {
    shortcut_group_rows_top(rhythm) + rows.navigation.iter().sum::<f64>() + 4.0
}

/// The width of one navigation column in a body `body_width` wide.
pub(super) fn shortcut_navigation_column_width(body_width: f64) -> f64 {
    let columns = shortcut_navigation_columns(body_width);
    (body_width - 28.0 - 8.0 * f64_of(columns.saturating_sub(1))) / f64_of(columns)
}

/// The width of one reference group in a body `body_width` wide.
pub(super) fn shortcut_reference_width(body_width: f64) -> f64 {
    let columns = shortcut_reference_columns(body_width);
    (body_width - 12.0 * f64_of(columns.saturating_sub(1))) / f64_of(columns)
}

/// The room a navigation row's reset button takes after its chords.
pub(super) fn shortcut_reset_room(theme: &ui::theme::Theme) -> f64 {
    8.0 + ui::widget::ControlSize::Small.height(theme)
}

fn f64_of(count: usize) -> f64 {
    ui::num::f64_of_usize(count)
}

/// The keyboard guide's rows measured in a body width and theme: each
/// navigation row's step (its label, its chords, which take two lines when
/// they wrap after "or", and a tight gap) and each reference group's row
/// steps. Resolved where the theme is in hand, so the panel's height, its
/// paint and its hit-testing share one answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct GuideRows {
    pub(super) body_width: f64,
    pub(super) navigation: Vec<f64>,
    pub(super) groups: Vec<Vec<f64>>,
    /// What these rows were measured for: the theme, the bindings and the
    /// keys' legends.
    measured_for: Option<(
        ui::theme::Theme,
        ShortcutConfig,
        ui::widget::LegendConventions,
    )>,
}

impl GuideRows {
    /// Whether these rows still hold for `theme`, `body_width`, `config`
    /// and `conventions`.
    pub(super) fn holds_for(
        &self,
        theme: &ui::theme::Theme,
        body_width: f64,
        config: &ShortcutConfig,
        conventions: ui::widget::LegendConventions,
    ) -> bool {
        (self.body_width - body_width).abs() < 0.5
            && self
                .measured_for
                .as_ref()
                .is_some_and(|(seen, bindings, legends)| {
                    seen == theme && bindings == config && *legends == conventions
                })
    }
}

/// A configured shortcut as the keycaps the guide draws for it.
pub(super) fn shortcut_keyboard_binding(
    config: &ShortcutConfig,
    action: ShortcutAction,
) -> ui::input::KeyboardBinding {
    use ui::input::{KeyLegend, KeyboardModifier};
    let chord = config.bindings.get(&action);
    let modifiers = chord
        .into_iter()
        .flat_map(|chord| chord.modifiers.iter())
        .filter_map(|modifier| match modifier.as_str() {
            "Super" => Some(KeyboardModifier::System),
            "Ctrl" => Some(KeyboardModifier::Control),
            "Alt" => Some(KeyboardModifier::Alt),
            "Shift" => Some(KeyboardModifier::Shift),
            _ => None,
        });
    let key = chord.map_or_else(
        || KeyLegend::Character("—".into()),
        |chord| KeyLegend::from_name(&chord.key),
    );
    ui::input::KeyboardBinding::new(modifiers, key)
}

/// Measure the guide's rows for a body `body_width` wide in `theme`.
pub(super) fn shortcut_guide_rows(
    theme: &ui::theme::Theme,
    body_width: f64,
    config: &ShortcutConfig,
    conventions: ui::widget::LegendConventions,
) -> GuideRows {
    use ui::widget::{InputHint, KeyHintsFace};
    let tight = f64::from(theme.tokens.spacing.rhythm.tight);
    let cap = InputHint::keycap_height(theme);
    let columns = shortcut_navigation_columns(body_width);
    let chords_end = shortcut_navigation_column_width(body_width) - shortcut_reset_room(theme);
    let cell = shortcut_reference_width(body_width) - 24.0;
    ui::text_render::with_layout_renderer(theme.text_scale, |text| {
        let pairs: Vec<f64> = super::shortcut_config::SHORTCUT_GUIDE_ROWS
            .iter()
            .map(|(_, previous, next)| {
                let pair = [
                    &shortcut_keyboard_binding(config, *previous),
                    &shortcut_keyboard_binding(config, *next),
                ];
                InputHint::keyboard_choice_height(text, theme, pair, conventions, (0.0, chords_end))
            })
            .collect();
        let navigation = pairs
            .chunks(columns.max(1))
            .map(|row| 18.0 + row.iter().copied().fold(0.0, f64::max) + tight)
            .collect();
        let groups = shortcut_reference_groups(config)
            .iter()
            .map(|group| {
                group
                    .iter()
                    .map(|(_, chord)| {
                        let hints = shortcut_chord_hints(chord);
                        let chords = KeyHintsFace::new(&hints).height_in(cell, text, theme);
                        shortcut_reference_step() - cap + chords
                    })
                    .collect()
            })
            .collect();
        GuideRows {
            body_width,
            navigation,
            groups,
            measured_for: Some((theme.clone(), config.clone(), conventions)),
        }
    })
}

/// The height of reference band `band` (groups side by side): its tallest
/// group's card.
pub(super) fn shortcut_reference_band_height(
    rows: &GuideRows,
    band: usize,
    rhythm: GuideRhythm,
) -> f64 {
    let columns = shortcut_reference_columns(rows.body_width).max(1);
    rows.groups
        .iter()
        .skip(band.saturating_mul(columns))
        .take(columns)
        .map(|steps| shortcut_reference_group_height(steps, rhythm))
        .fold(0.0, f64::max)
}

/// The keyboard guide's steps in a theme, resolved where the theme is in
/// hand so the guide's hit-testing and paint share one answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GuideRhythm {
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

/// Intrinsic height of the keyboard guide body: everything the guide
/// paints, so the panel is exactly as tall as its content (up to the window)
/// and scrolls only when the window is shorter.
pub(super) fn shortcut_guide_content_height(
    rows: &GuideRows,
    note_lines: usize,
    rhythm: GuideRhythm,
) -> f64 {
    let bands = rows
        .groups
        .len()
        .div_ceil(shortcut_reference_columns(rows.body_width).max(1));
    let groups: f64 = (0..bands)
        .map(|band| shortcut_reference_band_height(rows, band, rhythm))
        .sum::<f64>()
        + f64_of(bands.saturating_sub(1)) * 12.0;
    // The essentials header sits between the navigation surface and the
    // groups, its own height spacing it from both.
    shortcut_navigation_height(rows, rhythm)
        + rhythm.header
        + groups
        + shortcut_notes_height(note_lines)
}

/// The keyboard guide's natural height: its title block, the body, and the
/// bottom margin.
pub(super) fn shortcut_guide_height(
    rows: &GuideRows,
    note_lines: usize,
    rhythm: GuideRhythm,
) -> f64 {
    98.0 + shortcut_guide_content_height(rows, note_lines, rhythm) + 22.0
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

    fn guide_rows(theme: &ui::theme::Theme, body_width: f64) -> GuideRows {
        shortcut_guide_rows(
            theme,
            body_width,
            &ShortcutConfig::default(),
            ui::widget::LegendConventions {
                platform: ui::PlatformConventions::default(),
                keyboard: ui::KeyboardFamily::Pc,
                controller: ui::ControllerFamily::Generic,
            },
        )
    }

    #[test]
    fn navigation_surface_holds_every_row() {
        let theme = ui::theme::Theme::dark();
        let rhythm = shortcut_guide_rhythm(&theme);
        let keycap = ui::widget::InputHint::keycap_height(&theme);
        let wide = guide_rows(&theme, 912.0);
        // Nine pairs in two columns are five rows under the group header,
        // each at least its label and one line of keycaps.
        assert_eq!(wide.navigation.len(), 5);
        let height = shortcut_navigation_height(&wide, rhythm);
        assert!(height >= shortcut_group_rows_top(rhythm) + 5.0 * (18.0 + keycap));
        assert!(shortcut_navigation_height(&guide_rows(&theme, 308.0), rhythm) > height);
    }

    #[test]
    fn a_narrow_guide_gives_a_wrapped_chord_row_its_second_line() {
        // A 420 px window leaves a 308 px body: the Tabs pair, two long
        // chords, wraps after "or" and its row grows by a line of keycaps.
        let theme = ui::theme::Theme::dark().with_density(ui::tokens::Density::Compact);
        let keycap = ui::widget::InputHint::keycap_height(&theme);
        let rows = guide_rows(&theme, 308.0);
        let tabs = super::super::shortcut_config::SHORTCUT_GUIDE_ROWS
            .iter()
            .position(|(label, ..)| *label == "guide-row-tabs")
            .unwrap();
        let windows = rows.navigation[0];
        assert!(
            rows.navigation[tabs] >= windows + keycap,
            "{:?}",
            rows.navigation
        );
        let rhythm = shortcut_guide_rhythm(&theme);
        let unwrapped =
            shortcut_group_rows_top(rhythm) + f64_of(rows.navigation.len()) * windows + 4.0;
        assert!(shortcut_navigation_height(&rows, rhythm) > unwrapped);
    }

    #[test]
    fn reference_rows_leave_room_for_label_and_keycaps() {
        let theme = ui::theme::Theme::dark();
        let label = f64::from(theme.tokens.typography.size(ui::TextRole::Caption));
        let keycap = ui::widget::InputHint::keycap_height(&theme);
        assert!(shortcut_reference_step() >= label * 1.3 + 4.0 + keycap);
        let rhythm = shortcut_guide_rhythm(&theme);
        assert!(rhythm.header >= label, "a group header holds its title");
        let rows = guide_rows(&theme, 912.0);
        for steps in &rows.groups {
            assert!(steps.iter().all(|step| *step >= shortcut_reference_step()));
        }
        assert!(
            shortcut_reference_band_height(&rows, 0, rhythm)
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
                &guide_rows(&theme, 912.0),
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
        let theme = ui::theme::Theme::dark();
        let rhythm = shortcut_guide_rhythm(&theme);
        assert!(
            shortcut_guide_content_height(&guide_rows(&theme, 300.0), 1, rhythm)
                > shortcut_guide_content_height(&guide_rows(&theme, 912.0), 1, rhythm)
        );
    }
}
