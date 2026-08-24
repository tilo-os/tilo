//! Contrast floors for chrome the shell paints by hand.
//!
//! A sweep of composited pixels across the scenario renders found fifteen
//! separate contrast failures in three themes. They had two causes, and both
//! are mistakes about *what a colour is measured against*:
//!
//! 1. A label drawn in the same tint as its own wash of that tint. Both sides
//!    of the ratio move together, so no palette tuning can separate them —
//!    these measured 2.6:1 to 3.6:1 and would have measured the same after any
//!    theme change.
//! 2. A tone measured against the token a translucent body was mixed from
//!    rather than against the body itself. The token is not what reaches the
//!    screen; only the composite is.
//!
//! Fifteen symptoms from two causes means the missing thing was a check, not
//! fifteen fixes. This is that check. Every case below resolves its colours
//! through the **production** function the draw path calls, and measures each
//! tone against the ground that same production code composites — never
//! against a token, and never against a value restated here.
//!
//! Adding a hand-rolled tinted surface to the chrome means adding its
//! resolver here.

use ui::a11y::contrast_ratio;
use ui::theme::Theme;
use ui::tokens::colors::{NON_TEXT_MIN_CONTRAST, ON_CARD_SMALL_TEXT_CONTRAST};

/// Every theme the shell ships, by the name the renders use.
///
/// High contrast is in the list precisely because it is the one a person
/// reaches for when they are already struggling to see: a floor missed there
/// is missed where it matters most. It is also derived from the dark palette
/// by override, so it inherits any token the dark theme gets wrong.
fn themes() -> Vec<(&'static str, Theme)> {
    vec![
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ]
}

/// The recipe both failures collapse into, checked directly.
///
/// A chip's label must clear its floor against the chip's own composited
/// body, for every tint the chrome makes chips from and in every theme. This
/// is the generic case; the named sites below are the specific ones.
#[test]
fn a_tinted_chip_label_clears_its_own_body() {
    for (name, theme) in themes() {
        let colors = &theme.tokens.colors;
        let tints = [
            ("accent", colors.accent[5]),
            ("danger", colors.semantic.danger),
            ("warning", colors.semantic.warning),
            ("success", colors.semantic.success),
            ("info", colors.semantic.info),
        ];
        // The grounds a chip actually lands on in this chrome.
        let grounds = [
            ("base", colors.surfaces.base),
            ("raised", colors.surfaces.raised),
            ("overlay", colors.surfaces.overlay),
            ("panel", ui::chrome::shell::panel_ground(&theme)),
        ];
        for (tint_name, tint) in tints {
            for (ground_name, ground) in grounds {
                // 0.20 is the wash every hand-rolled pill in the chrome used.
                let chip = colors.tinted_chip_on(ground, tint, 0.20, ON_CARD_SMALL_TEXT_CONTRAST);
                let ratio = contrast_ratio(chip.ink, chip.fill);
                assert!(
                    ratio >= ON_CARD_SMALL_TEXT_CONTRAST,
                    "{name}: a {tint_name} chip on {ground_name} labels itself at \
                     {ratio:.2}:1 against its own body, under the {:.1}:1 floor",
                    ON_CARD_SMALL_TEXT_CONTRAST,
                );
                // The edge is what keeps a quiet pill findable, so it answers
                // to the shape floor against the ground — not the body.
                let edge = contrast_ratio(chip.border, ground);
                assert!(
                    edge >= NON_TEXT_MIN_CONTRAST,
                    "{name}: a {tint_name} chip on {ground_name} has an edge at \
                     {edge:.2}:1, under the {NON_TEXT_MIN_CONTRAST:.1}:1 shape floor",
                );
            }
        }
    }
}

/// The quick-settings Power glyph, against the tile it sits in.
///
/// It used to be the same `danger` the tile is washed with.
#[test]
fn the_power_glyph_clears_its_own_tile() {
    for (name, theme) in themes() {
        let glyph = crate::shell::overlay_host::power_tile_glyph(&theme);
        let body = crate::shell::overlay_host::power_tile_body(&theme);
        let ratio = contrast_ratio(glyph, body);
        println!("power-glyph  {name:<14} {ratio:.2}:1");
        assert!(
            ratio >= NON_TEXT_MIN_CONTRAST,
            "{name}: the Power glyph reads at {ratio:.2}:1 on its own tile, \
             under the {NON_TEXT_MIN_CONTRAST:.1}:1 shape floor",
        );
    }
}

/// The battery gauge, against the tray pill it is drawn on, in every state.
#[test]
fn the_battery_gauge_clears_the_tray_pill_in_every_state() {
    for (name, theme) in themes() {
        for (state, critical, low, charging) in [
            ("critical", true, true, false),
            ("low", false, true, false),
            ("charging", false, false, true),
            ("normal", false, false, false),
        ] {
            let hue = crate::shell::topbar_chrome::battery_gauge_hue(
                &theme, critical, low, charging,
            );
            let body = crate::shell::topbar_chrome::tray_pill_body(&theme, critical);
            let ratio = contrast_ratio(hue, body);
            println!("battery-{state:<9} {name:<14} {ratio:.2}:1");
            assert!(
                ratio >= NON_TEXT_MIN_CONTRAST,
                "{name}: the {state} battery gauge reads at {ratio:.2}:1 on the \
                 tray pill, under the {NON_TEXT_MIN_CONTRAST:.1}:1 shape floor",
            );
        }
    }
}

/// Urgency and legibility must not run in opposite directions.
///
/// The critical battery measured 2.61:1 while the merely *low* one measured
/// 5.40:1 — the state you must not miss was the harder one to see, because
/// critical also tints the pill `danger` and the gauge was `danger` too. A
/// floor alone would not have caught the inversion, so it is asserted as its
/// own property.
#[test]
fn a_critical_battery_is_never_quieter_than_a_low_one() {
    for (name, theme) in themes() {
        let critical = contrast_ratio(
            crate::shell::topbar_chrome::battery_gauge_hue(&theme, true, true, false),
            crate::shell::topbar_chrome::tray_pill_body(&theme, true),
        );
        let low = contrast_ratio(
            crate::shell::topbar_chrome::battery_gauge_hue(&theme, false, true, false),
            crate::shell::topbar_chrome::tray_pill_body(&theme, false),
        );
        assert!(
            critical >= low,
            "{name}: a critical battery reads at {critical:.2}:1 where a low one \
             reads at {low:.2}:1 — the more urgent state is the harder to see",
        );
    }
}

/// What a window says when it went up with nothing in it, against the card it
/// says it on.
///
/// This is the one line on screen naming *what* failed to open, and it was
/// drawn from a fixed ramp index: near-black on a near-black card, 1.03:1.
#[test]
fn an_empty_window_names_itself_legibly() {
    for (name, theme) in themes() {
        let colors = &theme.tokens.colors;
        let card = colors.surfaces.window_card;
        let tiers = colors.on_card();
        for (tier, ink) in [("headline", tiers.ink), ("detail", tiers.ink_dim)] {
            let ratio = contrast_ratio(ink, card);
            println!("empty-window-{tier:<8} {name:<14} {ratio:.2}:1");
            assert!(
                ratio >= 4.5,
                "{name}: the empty-window {tier} reads at {ratio:.2}:1 on the \
                 window card, under the 4.5:1 floor",
            );
        }
    }
}

/// The top bar's own pills — clock band, workspace pill, tray — against the
/// bar they sit on.
///
/// A selector's body is a lift, and a lift is a step away from whatever is
/// underneath. These were lifted away from `surfaces.base` while the bar
/// paints `surfaces.overlay`, which on the light palette is a different
/// white: the clock pill came out `#ffffff` on `#ffffff`, 1.00:1, with a
/// hairline as the only evidence a pill was there.
#[test]
fn the_bar_pills_have_a_body_on_the_bar() {
    use ui::a11y::relative_luminance;
    for (name, theme) in themes() {
        let colors = &theme.tokens.colors;
        let bar = colors.surfaces.overlay;
        let tier = ui::chrome::selection_tier_on(
            &theme,
            ui::chrome::SelectionState::active(true),
            bar,
        );
        let body = ui::tokens::colors::over(bar, tier.fill, tier.fill.components[3]);
        let moved = (relative_luminance(body) - relative_luminance(bar)).abs();
        let ratio = contrast_ratio(body, bar);
        println!("bar-pill     {name:<14} {ratio:.3}:1 (luma moved {moved:.4})");
        assert!(
            moved > 0.0,
            "{name}: an active bar pill composites to the bar's own colour — \
             the pill has no body, only its hairline",
        );
    }
}

/// A veneer has to move the surface it is laid on.
///
/// `surface_white_5` was `white/5` in both the light and the dark palette.
/// Over the light theme's `#ffffff` card that is 1.01:1 — nothing — which is
/// why every unselected filter chip lost its pill and read as bare text.
/// The tier is deliberately quiet, so this asserts only that it is *there*:
/// a pill that must stay findable takes an edge, which
/// [`a_tinted_chip_label_clears_its_own_body`] covers.
#[test]
fn a_faint_surface_tier_is_visible_on_its_own_theme() {
    use ui::a11y::relative_luminance;
    for (name, theme) in themes() {
        let colors = &theme.tokens.colors;
        for (ground_name, ground) in [
            ("raised", colors.surfaces.raised),
            ("base", colors.surfaces.base),
        ] {
            for (tier, wash) in [
                ("surface_white_5", colors.surface_white_5),
                ("surface_white_10", colors.surface_white_10),
                ("surface_white_20", colors.surface_white_20),
            ] {
                let alpha = wash.components[3];
                let composited = ui::tokens::colors::over(ground, wash, alpha);
                let moved =
                    (relative_luminance(composited) - relative_luminance(ground)).abs();
                println!(
                    "{tier:<17} on {ground_name:<7} {name:<14} {:.3}:1",
                    contrast_ratio(composited, ground),
                );
                assert!(
                    moved > 0.0,
                    "{name}: {tier} over {ground_name} composites to the ground \
                     itself — the tier is invisible on this theme",
                );
            }
        }
    }
}
