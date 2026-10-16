//! Window-group widths as a fraction of the strip, never as pixels.

use serde::{Deserialize, Serialize};

/// Per-mille (not px) so a tier means the same fraction on every monitor.
/// Thirds tile the strip cleanly; `Wide` is the full usable width.
pub const WIDTH_NARROW_PERMILLE: u16 = 333;
pub const WIDTH_NORMAL_PERMILLE: u16 = 667;
pub const WIDTH_WIDE_PERMILLE: u16 = 1000;

/// Bounds of what a width can *store*, not of what a person may choose: a
/// group may be a sliver of a share up to several strips wide, the strip
/// scrolls. Windows' own minimum and maximum sizes are the client's to
/// state (`xdg_toplevel` hints are honoured when it is configured); the
/// only limit of ours is [`WIDTH_FLOOR_PX`].
pub const WIDTH_MIN_PERMILLE: u16 = 1;
pub const WIDTH_MAX_PERMILLE: u16 = 10_000;

/// The one sane minimum: enough for a tab pill, its controls and a
/// scrollbar. A fraction below it resolves up to it.
pub const WIDTH_FLOOR_PX: f64 = 160.0;

/// Narrowest a group may resolve to on a strip of `strip_usable_px`.
pub fn min_group_px(strip_usable_px: f64) -> f64 {
    let usable = strip_usable_px.max(1.0);
    (usable * f64::from(WIDTH_MIN_PERMILLE) / 1000.0)
        .max(WIDTH_FLOOR_PX)
        .min(usable)
}

/// Widest a group may resolve to on a strip of `strip_usable_px`: the
/// widest width that can be stored. Far past the display; the strip scrolls.
pub fn max_group_px(strip_usable_px: f64) -> f64 {
    strip_usable_px.max(1.0) * f64::from(WIDTH_MAX_PERMILLE) / 1000.0
}

/// Inverse of [`WindowWidth::resolve`]: how a pointer drag (pixels)
/// becomes a stored resolution-independent width.
pub fn permille_for_px(px: f64, strip_usable_px: f64) -> u16 {
    let usable = strip_usable_px.max(1.0);
    let raw = (px.max(0.0) / usable * 1000.0).round();
    let clamped = raw.clamp(f64::from(WIDTH_MIN_PERMILLE), f64::from(WIDTH_MAX_PERMILLE));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [150.0, 1000.0] above, which fits losslessly in u16"
    )]
    {
        clamped as u16
    }
}

/// Persisted width of a [`crate::WindowGroup`], as a fraction of the
/// strip's usable width — a stored pixel count would mean a different
/// screen share on every monitor.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum WindowWidth {
    Narrow,
    Normal,
    Wide,
    /// An arbitrary fraction of the strip's usable width, in per-mille.
    /// What a manual resize persists.
    Fraction(u16),
}

impl WindowWidth {
    /// The fraction this width represents, in per-mille of the strip's
    /// usable width.
    pub fn permille(&self) -> u16 {
        match self {
            WindowWidth::Narrow => WIDTH_NARROW_PERMILLE,
            WindowWidth::Normal => WIDTH_NORMAL_PERMILLE,
            WindowWidth::Wide => WIDTH_WIDE_PERMILLE,
            WindowWidth::Fraction(p) => (*p).clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE),
        }
    }

    /// [`Self::permille`] as a fraction in `[0.15, 1.0]`.
    pub fn fraction(&self) -> f64 {
        f64::from(self.permille()) / 1000.0
    }

    /// Resolve to logical pixels against a strip of `strip_usable_px`,
    /// clamped into `[min_group_px, max_group_px]` so a fraction can
    /// neither collapse a group below usability nor push it past the
    /// output's right edge.
    pub fn resolve(&self, strip_usable_px: f64) -> f64 {
        let usable = strip_usable_px.max(1.0);
        // Rounded: a client's geometry is integers, and a fractional width
        // would leave sub-pixel seams between groups.
        (usable * self.fraction())
            .round()
            .clamp(min_group_px(usable), max_group_px(usable))
    }

    /// Always a [`WindowWidth::Fraction`] — records the proportion the
    /// user landed on rather than a coincident tier, so a later tier
    /// retune doesn't silently move their window.
    pub fn from_px(px: f64, strip_usable_px: f64) -> Self {
        WindowWidth::Fraction(permille_for_px(px, strip_usable_px))
    }
}

impl<'de> Deserialize<'de> for WindowWidth {
    /// Clamps a persisted fraction into the supported range.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        enum Raw {
            Narrow,
            Normal,
            Wide,
            Fraction(u16),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Narrow => WindowWidth::Narrow,
            Raw::Normal => WindowWidth::Normal,
            Raw::Wide => WindowWidth::Wide,
            Raw::Fraction(p) => {
                WindowWidth::Fraction(p.clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_load_as_clamped_fractions_with_a_pixel_floor() {
        // Phase `pixel_widths_are_refused`.
        {
            assert!(serde_json::from_str::<WindowWidth>(r#"{"Custom":600}"#).is_err());
        }
        // Phase `out_of_range_fraction_is_clamped_on_load`.
        {
            let width: WindowWidth = serde_json::from_str(r#"{"Fraction":0}"#).unwrap();
            assert_eq!(width, WindowWidth::Fraction(WIDTH_MIN_PERMILLE));
            let width: WindowWidth = serde_json::from_str(r#"{"Fraction":65000}"#).unwrap();
            assert_eq!(width, WindowWidth::Fraction(WIDTH_MAX_PERMILLE));
        }
        // Phase `no_artificial_limits_beyond_the_floor_and_the_storable_range`.
        {
            // A 10% window on a 3840 px strip is 384 px: no share floor.
            assert!((WindowWidth::Fraction(100).resolve(3840.0) - 384.0).abs() < 1.0);
            // Narrower than the floor resolves to the floor, nothing more.
            assert!((WindowWidth::Fraction(10).resolve(1472.0) - WIDTH_FLOOR_PX).abs() < 1e-9);
            // Wider than the strip is allowed.
            assert!((WindowWidth::Fraction(2500).resolve(1000.0) - 2500.0).abs() < 1e-9);
        }
        // Phase `resolve_respects_the_pixel_floor`.
        {
            assert!(
                (WindowWidth::Fraction(150).resolve(800.0) - WIDTH_FLOOR_PX).abs() < f64::EPSILON
            );
            assert!((WindowWidth::Wide.resolve(1000.0) - 1000.0).abs() < f64::EPSILON);
        }
    }
}
