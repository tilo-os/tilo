//! Contrast-aware foreground selection for selected / active chrome.
//!
//! Tilo's [`ui::theme::Theme`] ships **no on-accent token**: there is no
//! field that says "the readable text colour to place on top of an accent
//! fill". Worse, `text_primary` is *white* in the dark theme but *black* in
//! the light theme, so any hard-coded `Color::WHITE` (or any "accent text on
//! a bright neutral surface") silently fails in one theme or the other.
//!
//! Selected tabs / nav items / chips are exactly the surfaces that get a
//! bright or saturated highlight, so picking their foreground by eye is how
//! the readability bug crept in (indigo-500 label on a slate-200 highlight =
//! 3.6:1; indigo-500 text on an indigo wash = 3.4:1 — both below the WCAG AA
//! 4.5:1 floor for body text).
//!
//! This module is the single seam every selected/active state in
//! [`crate::screens`] routes through. Given the background a control will be
//! painted with, [`readable_on`] returns black or white — whichever has the
//! higher WCAG contrast — and [`SelectedStyle`] bundles the
//! `(background, foreground)` pair so a screen never picks one without the
//! other. Because the same functions are called from the views *and* from the
//! tests, the contrast guarantee is asserted on the real code path.

use ui::ColorCompat;
use vello::peniko::Color;

pub use ui::color::{contrast_ratio, relative_luminance};

/// Composite a translucent `fg` over an opaque `bg`, returning the opaque
/// colour a viewer actually sees. Selected backgrounds are sometimes an
/// alpha wash over a base surface (e.g. the status pills); contrast has to be
/// measured against the *composited* result, not the wash in isolation.
pub fn composite_over(fg: Color, bg: Color) -> Color {
    let a = f64::from(fg.a()) / 255.0;
    let mix = |f: u8, b: u8| {
        let v = f64::from(f) * a + f64::from(b) * (1.0 - a);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "f, b are in [0, 255] and a in [0, 1], so the convex combination is \
                      within [0, 255]; rounding then casting cannot truncate or lose sign"
        )]
        let out = v.round() as u8;
        out
    };
    Color::from_rgb8(
        mix(fg.r(), bg.r()),
        mix(fg.g(), bg.g()),
        mix(fg.b(), bg.b()),
    )
}

/// Pick the foreground (black or white) with the higher contrast against
/// `bg`. This is the swappable seam every selected/active surface uses
/// instead of hard-coding `Color::WHITE` (which breaks in the light theme).
pub fn readable_on(bg: Color) -> Color {
    if contrast_ratio(Color::WHITE, bg) >= contrast_ratio(Color::BLACK, bg) {
        Color::WHITE
    } else {
        Color::BLACK
    }
}

/// A selected/active surface's background paired with a foreground that is
/// guaranteed to contrast it. Constructed via [`SelectedStyle::on`] so the
/// two can never drift apart.
#[derive(Clone, Copy, Debug)]
pub struct SelectedStyle {
    pub bg: Color,
    pub fg: Color,
}

impl SelectedStyle {
    /// Build a style whose foreground is the most-readable choice on `bg`.
    pub fn on(bg: Color) -> Self {
        Self {
            bg,
            fg: readable_on(bg),
        }
    }

    /// Build a style for a translucent highlight `wash` layered over `base`,
    /// measuring contrast against the composited result the user sees.
    pub fn over(wash: Color, base: Color) -> Self {
        let bg = composite_over(wash, base);
        Self {
            bg,
            fg: readable_on(bg),
        }
    }

    /// The WCAG contrast ratio of this style's text against its background.
    pub fn ratio(&self) -> f64 {
        contrast_ratio(self.fg, self.bg)
    }
}

/// The WCAG AA contrast floor for normal-size body text. Selected-state
/// labels (chips, nav items) are small, so they must clear this, not the
/// looser 3:1 large-text floor.
pub const AA_BODY: f64 = 4.5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_endpoints() {
        assert!(relative_luminance(Color::BLACK) < 0.001);
        assert!(relative_luminance(Color::WHITE) > 0.999);
    }

    #[test]
    fn black_white_extreme_ratio_is_21() {
        let r = contrast_ratio(Color::BLACK, Color::WHITE);
        assert!(
            (r - 21.0).abs() < 0.01,
            "black/white should be 21:1, was {r}"
        );
    }

    #[test]
    fn readable_on_bright_surface_is_black_not_white() {
        // slate-200 (a nav-item highlight candidate) is bright → black wins.
        let slate_200 = Color::from_rgb8(226, 232, 240);
        assert_eq!(readable_on(slate_200), Color::BLACK);
    }

    #[test]
    fn readable_on_dark_accent_fill_is_white() {
        // indigo-700: the dark theme's actual selected chip/nav fill.
        let indigo_700 = Color::from_rgb8(67, 56, 202);
        assert_eq!(readable_on(indigo_700), Color::WHITE);
    }

    #[test]
    fn readable_on_picks_the_genuinely_higher_contrast_choice() {
        // Mid-tone indigo-500: black (4.70:1) narrowly beats white (4.47:1),
        // so the seam must follow the maths, not an assumption that white
        // always reads on an accent.
        let indigo_500 = Color::from_rgb8(99, 102, 241);
        let picked = readable_on(indigo_500);
        let other = if picked == Color::WHITE {
            Color::BLACK
        } else {
            Color::WHITE
        };
        assert!(
            contrast_ratio(picked, indigo_500) >= contrast_ratio(other, indigo_500),
            "readable_on must pick the higher-contrast foreground"
        );
        assert_eq!(
            picked,
            Color::BLACK,
            "black narrowly beats white on indigo-500"
        );
    }

    #[test]
    fn regression_old_buggy_combos_fail_aa() {
        // Both were real selected-state foreground/background pairs and both
        // fall below the 4.5:1 body-text floor — pins the bug this module
        // fixes so the seam can't silently regress back to it.
        let indigo_500 = Color::from_rgb8(99, 102, 241); // accent_primary
        let slate_200 = Color::from_rgb8(226, 232, 240); // neutral.stops[8]
        // accent text/icon on a bright slate-200 chip (nav_item active).
        assert!(
            contrast_ratio(indigo_500, slate_200) < 4.5,
            "old nav-item combo should be below AA"
        );
        // accent text on an indigo-900 wash over slate-900 (status_pill on).
        let wash = Color::from_rgba8(49, 46, 129, 102); // indigo-900 @ ~0.4
        let bg = composite_over(wash, Color::from_rgb8(15, 23, 42));
        assert!(
            contrast_ratio(indigo_500, bg) < 4.5,
            "old status-pill combo should be below AA"
        );

        // The fix clears AA: white on indigo-700 (the on-pill bg) and the
        // readable_on choice on the accent fill both pass.
        let indigo_700 = Color::from_rgb8(67, 56, 202);
        assert!(contrast_ratio(Color::WHITE, indigo_700) >= 4.5);
        assert!(contrast_ratio(readable_on(indigo_500), indigo_500) >= 4.5);
    }
}
