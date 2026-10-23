//! Every label a filled control paints on its own fill, measured on every
//! theme the shell can be running under.
//!
//! This exists because the defect it guards shipped: `ButtonRole::Primary`
//! and `Danger` hard-coded a white label over `accent_primary` /
//! `semantic.danger`, and a toggle group's selected segment did the same. On
//! the dark palette the primary button's label measured 3.17:1, and
//! high-contrast — the palette a person picks *because* they need contrast —
//! was worst of the three at 3.00:1 for the button and 2.37:1 hovered.
//!
//! A per-widget test on one theme could not have caught that, which is why
//! this one is a sweep over the product of {style, state} × {theme},
//! asserting only that the label clears its floor against the fill the
//! widget resolved for itself.

use tilo_ui::a11y::contrast_ratio;
use tilo_ui::color_compat::ColorCompat;
use tilo_ui::theme::Theme;
use tilo_ui::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_FILL_MIN_CONTRAST};
use tilo_ui::widget::{
    ButtonInteract, ButtonRole, SegmentedStyle, ToggleInteract, face_colors, segment_colors,
    toggle_face_colors,
};
use vello::peniko::Color;

fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn themes() -> Vec<Theme> {
    vec![Theme::dark(), Theme::light(), Theme::high_contrast()]
}

/// A transparent fill is not a ground — the label lands on whatever the control
/// was placed over. Measure those against the base surface, which is what the
/// widget itself assumes when it derives its ink.
fn ground(theme: &Theme, fill: Color) -> Color {
    if fill.components[3] <= 0.0 {
        theme.tokens.colors.surfaces.base
    } else {
        fill
    }
}

fn interacts() -> [(&'static str, ButtonInteract); 4] {
    [
        ("rest", ButtonInteract::default()),
        (
            "hovered",
            ButtonInteract {
                hovered: true,
                ..Default::default()
            },
        ),
        (
            "pressed",
            ButtonInteract {
                pressed: true,
                ..Default::default()
            },
        ),
        (
            "focused",
            ButtonInteract {
                focused: true,
                ..Default::default()
            },
        ),
    ]
}

const STYLES: [ButtonRole; 4] = [
    ButtonRole::Secondary,
    ButtonRole::Primary,
    ButtonRole::Destructive,
    ButtonRole::Plain,
];

#[test]
fn filled_control_labels_clear_their_floor_in_every_theme_and_state() {
    // Case `every_button_style_label_clears_its_floor_in_every_theme`.
    {
        let mut failures = Vec::new();
        for theme in themes() {
            for style in STYLES {
                for (state, interact) in interacts() {
                    let (fill, _border, label) = face_colors(&theme, style, interact);
                    let g = ground(&theme, fill);
                    let ratio = contrast_ratio(label, g);
                    if ratio < ON_FILL_MIN_CONTRAST {
                        failures.push(format!(
                        "{} {:?}/{state}: label {} on fill {} = {ratio:.2}:1 (floor {ON_FILL_MIN_CONTRAST})",
                        theme.name,
                        style,
                        hex(label),
                        hex(g),
                    ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "unreadable button labels:\n  {}",
            failures.join("\n  ")
        );
    }

    // Case `a_button_label_holds_its_colour_across_interaction`.
    // Deriving the label per state would let it cross the floor mid-gesture and
    // flip from white to near-black under the pointer. Whatever a style resolves,
    // it resolves once, for every state it can be in.
    {
        for theme in themes() {
            for style in STYLES {
                let rest = face_colors(&theme, style, ButtonInteract::default()).2;
                for (state, interact) in interacts() {
                    let got = face_colors(&theme, style, interact).2;
                    assert_eq!(
                        hex(got),
                        hex(rest),
                        "{} {:?} label changed on {state}",
                        theme.name,
                        style,
                    );
                }
            }
        }
    }

    // Case `every_segment_label_clears_its_floor_in_every_theme`.
    {
        let mut failures = Vec::new();
        for theme in themes() {
            for style in [
                SegmentedStyle::Segmented,
                SegmentedStyle::Tabs,
                SegmentedStyle::Quiet,
            ] {
                for selected in [true, false] {
                    for hovered in [true, false] {
                        let (fill, label) = segment_colors(&theme, style, selected, hovered);
                        let g = ground(&theme, fill.unwrap_or(Color::TRANSPARENT));
                        let ratio = contrast_ratio(label, g);
                        if ratio < ON_FILL_MIN_CONTRAST {
                            failures.push(format!(
                            "{} {:?}/selected={selected}/hovered={hovered}: label {} on {} = {ratio:.2}:1",
                            theme.name,
                            style,
                            hex(label),
                            hex(g),
                        ));
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "unreadable segment labels:\n  {}",
            failures.join("\n  ")
        );
    }
}

#[test]
fn toggle_knobs_stay_visible_and_in_place() {
    // Case `every_toggle_knob_stays_visible_on_its_track`.
    // The knob is a shape, not text, so it answers to the non-text floor — but it
    // answers to it on the accent track as well as the neutral one, and the accent
    // is the track a theme is free to make bright.
    {
        let mut failures = Vec::new();
        for theme in themes() {
            for on in [true, false] {
                for (state, interact) in [
                    ("rest", ToggleInteract::default()),
                    (
                        "hovered",
                        ToggleInteract {
                            hovered: true,
                            ..Default::default()
                        },
                    ),
                    (
                        "focused",
                        ToggleInteract {
                            focused: true,
                            ..Default::default()
                        },
                    ),
                ] {
                    let face = toggle_face_colors(&theme, on, interact);
                    let (track, knob) = (face.track, face.knob_outline());
                    let ratio = contrast_ratio(knob, track);
                    if ratio < NON_TEXT_MIN_CONTRAST {
                        failures.push(format!(
                        "{} on={on}/{state}: knob {} on track {} = {ratio:.2}:1 (floor {NON_TEXT_MIN_CONTRAST})",
                        theme.name,
                        hex(knob),
                        hex(track),
                    ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "invisible toggle knobs:\n  {}",
            failures.join("\n  ")
        );
    }

    // Case `a_toggle_knob_never_swaps_sides_under_the_pointer`.
    // Hover must read as the same knob pressed harder, not as a different one.
    // Resolving the resting and hovered tones independently is what produced a
    // near-white knob that turned near-black on hover, each measuring fine alone.
    {
        for theme in themes() {
            for on in [true, false] {
                let rest = toggle_face_colors(&theme, on, ToggleInteract::default());
                let hovered = toggle_face_colors(
                    &theme,
                    on,
                    ToggleInteract {
                        hovered: true,
                        ..Default::default()
                    },
                );
                let (track, rest, hovered) =
                    (rest.track, rest.knob_outline(), hovered.knob_outline());
                let lighter_than_track = |k: Color| {
                    contrast_ratio(k, Color::from_rgb8(255, 255, 255))
                        < contrast_ratio(track, Color::from_rgb8(255, 255, 255))
                };
                assert_eq!(
                    lighter_than_track(rest),
                    lighter_than_track(hovered),
                    "{} on={on}: knob crossed its track — rest {}, hovered {}",
                    theme.name,
                    hex(rest),
                    hex(hovered),
                );
                let at_maximum = contrast_ratio(rest, track) >= 20.9;
                assert!(
                    at_maximum || contrast_ratio(hovered, track) > contrast_ratio(rest, track),
                    "{} on={on}: hover must make the knob more present, not less",
                    theme.name,
                );
            }
        }
    }
}
