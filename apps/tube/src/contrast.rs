//! Contrast-aware foreground selection for selected / active chrome.
//!
//! Selected tabs, nav items and status pills route through this module so a
//! control never picks a highlight without a label that reads on it. The
//! toolkit already owns that measurement — [`ui::tokens::Colors::readable_on`]
//! prefers the theme's own text, then white or black — so this file is the
//! app-facing seam, not a second palette.
//!
//! Hard-coded RGB used to stand in for "slate-200" / "indigo-500" in the
//! tests; those were one dark-theme snapshot and silently drifted from the
//! OS tokens. The tests now ask every shipped theme.

use ui::ColorCompat;
use ui::theme::Theme;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
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

/// Pick a foreground that reads on `bg`, using the theme's own tokens.
pub fn readable_on(theme: &Theme, bg: Color) -> Color {
    theme.tokens.colors.readable_on(bg, Color::WHITE)
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
    /// Build a style whose foreground is the most-readable theme choice on `bg`.
    pub fn on(theme: &Theme, bg: Color) -> Self {
        Self {
            bg,
            fg: readable_on(theme, bg),
        }
    }

    /// Build a style for a translucent highlight `wash` layered over `base`,
    /// measuring contrast against the composited result the user sees.
    pub fn over(theme: &Theme, wash: Color, base: Color) -> Self {
        let bg = composite_over(wash, base);
        Self {
            bg,
            fg: readable_on(theme, bg),
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
pub const AA_BODY: f64 = ON_FILL_MIN_CONTRAST;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratio_and_luminance_endpoints() {
        // Case `luminance_endpoints`.
        {
            assert!(relative_luminance(Color::BLACK) < 0.001);
            assert!(relative_luminance(Color::WHITE) > 0.999);
        }
        // Case `black_white_extreme_ratio_is_21`.
        {
            let r = contrast_ratio(Color::BLACK, Color::WHITE);
            assert!(
                (r - 21.0).abs() < 0.01,
                "black/white should be 21:1, was {r}"
            );
        }
    }

    #[test]
    fn selected_chrome_reads_on_every_shipped_theme() {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("shipped theme resolves");
            let c = &theme.tokens.colors;
            for (what, bg) in [
                ("accent_hover", theme.accent_hover),
                ("accent_primary", theme.accent_primary),
                ("accent[3]", c.accent[3]),
                ("raised", c.surfaces.raised),
                ("neutral[8]", c.neutral[8]),
            ] {
                let style = SelectedStyle::on(&theme, bg);
                assert!(
                    style.ratio() >= AA_BODY,
                    "[{name}] {what} selected label {:.2}:1 < {AA_BODY}",
                    style.ratio()
                );
            }

            // A translucent overlay wash over the base surface — the old
            // status-pill path — still has to produce a readable pair.
            let washed = SelectedStyle::over(&theme, c.overlay_strong, c.surfaces.base);
            assert!(
                washed.ratio() >= AA_BODY,
                "[{name}] overlay-on-base selected label {:.2}:1 < {AA_BODY}",
                washed.ratio()
            );
        }
    }
}
