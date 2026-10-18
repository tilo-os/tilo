//! Window-group widths as a fraction of the strip, never as pixels.

use serde::{Deserialize, Serialize};

/// Per-mille (not px) so a tier means the same fraction on every monitor.
/// Thirds tile the strip cleanly; `Wide` is the full usable width.
pub const WIDTH_NARROW_PERMILLE: u32 = 333;
pub const WIDTH_NORMAL_PERMILLE: u32 = 667;
pub const WIDTH_WIDE_PERMILLE: u32 = 1000;

/// Bounds of what a width can *store*, not of what a person may choose: a
/// group may be a sliver of a share up to any number of strips wide, the
/// strip scrolls. Windows' own minimum and maximum sizes are the apps' to
/// state (`xdg_toplevel` hints, a Tilo app's compact layout); the only limit
/// of ours is [`WIDTH_FLOOR_PX`].
pub const WIDTH_MIN_PERMILLE: u32 = 1;
/// The storage's own end, over four million strips: a drag cannot reach it,
/// even resting at the display's edge while the strip scrolls under it, so
/// there is no widest window anyone can feel. (Ten strips used to be the
/// limit, which an edge drag that scrolls the strip reaches in seconds.)
pub const WIDTH_MAX_PERMILLE: u32 = u32::MAX;

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
/// widest width that can be stored, millions of displays. The strip
/// scrolls.
pub fn max_group_px(strip_usable_px: f64) -> f64 {
    strip_usable_px.max(1.0) * f64::from(WIDTH_MAX_PERMILLE) / 1000.0
}

/// Inverse of [`WindowWidth::resolve`]: how a pointer drag (pixels)
/// becomes a stored resolution-independent width.
pub fn permille_for_px(px: f64, strip_usable_px: f64) -> u32 {
    let usable = strip_usable_px.max(1.0);
    let raw = (px.max(0.0) / usable * 1000.0).round();
    let clamped = raw.clamp(f64::from(WIDTH_MIN_PERMILLE), f64::from(WIDTH_MAX_PERMILLE));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the storable per-mille range above, which fits losslessly in u32"
    )]
    {
        clamped as u32
    }
}

/// Narrowest a window is made to open beside another so that both are on
/// screen together. Below it, an app's own layout (a sidebar and a body,
/// a header with its controls) no longer fits, so a share narrower than
/// this is worse than the strip scrolling to show the new window whole.
pub const SIDE_BY_SIDE_MIN_PX: f64 = 480.0;

/// Widths that put a window just opened beside `neighbour` on screen
/// together with it. `screen_px` is how wide a run of windows can be and
/// still be seen whole (the strip pans past its leading gutter, so this can
/// exceed `strip_usable_px`, which widths are fractions of); the two are
/// `gap_px` apart. `None` when they already fit, or when no share leaves
/// both at least [`SIDE_BY_SIDE_MIN_PX`]: the strip scrolls then, framing
/// the new window whole on its own.
///
/// The window the person was in keeps its width when the space beside it
/// is enough for the new one; otherwise the two share the screen equally.
/// Returns `(neighbour, opened)`.
pub fn share_beside(
    neighbour: WindowWidth,
    opened: WindowWidth,
    strip_usable_px: f64,
    screen_px: f64,
    gap_px: f64,
) -> Option<(WindowWidth, WindowWidth)> {
    let usable = strip_usable_px.max(1.0);
    let beside = neighbour.resolve(usable);
    let room = screen_px - gap_px - beside;
    if room >= opened.resolve(usable) {
        return None;
    }
    // Floored so that rounding never pushes the pair a pixel past the edge.
    let fitting = |px: f64| WindowWidth::Fraction(floor_permille(px, usable));
    if room >= SIDE_BY_SIDE_MIN_PX {
        return Some((neighbour, fitting(room)));
    }
    let half = (screen_px - gap_px) / 2.0;
    (half >= SIDE_BY_SIDE_MIN_PX).then(|| (fitting(half), fitting(half)))
}

/// The largest per-mille of `usable` that resolves to no more than `px`.
fn floor_permille(px: f64, usable: f64) -> u32 {
    let raw = (px.max(0.0) / usable * 1000.0).floor();
    let clamped = raw.clamp(f64::from(WIDTH_MIN_PERMILLE), f64::from(WIDTH_MAX_PERMILLE));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the storable per-mille range above, which fits losslessly in u32"
    )]
    {
        clamped as u32
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
    Fraction(u32),
}

impl WindowWidth {
    /// The fraction this width represents, in per-mille of the strip's
    /// usable width.
    pub fn permille(&self) -> u32 {
        match self {
            WindowWidth::Narrow => WIDTH_NARROW_PERMILLE,
            WindowWidth::Normal => WIDTH_NORMAL_PERMILLE,
            WindowWidth::Wide => WIDTH_WIDE_PERMILLE,
            WindowWidth::Fraction(p) => (*p).clamp(WIDTH_MIN_PERMILLE, WIDTH_MAX_PERMILLE),
        }
    }

    /// [`Self::permille`] as a fraction of the strip's usable width.
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
            Fraction(u32),
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

    /// Pixels the pair takes, gap included, on `usable`.
    fn pair_px(pair: (WindowWidth, WindowWidth), usable: f64, gap: f64) -> f64 {
        pair.0.resolve(usable) + gap + pair.1.resolve(usable)
    }

    #[test]
    fn a_window_opened_beside_another_shares_the_screen_with_it() {
        // Phase `two_windows_on_a_1280_screen_both_fit_on_it`.
        {
            // A 1280 px display whose strip starts past the 251 px wheel
            // gutter and ends 32 px short of the edge: two-thirds of what
            // is left is 665 px, and two of those overflow the screen.
            let (usable, screen) = (997.0, 1248.0);
            let pair = share_beside(
                WindowWidth::Normal,
                WindowWidth::Normal,
                usable,
                screen,
                8.0,
            )
            .expect("two two-thirds windows overflow one screen");
            assert_eq!(
                pair.0,
                WindowWidth::Normal,
                "the window in use keeps its width"
            );
            assert!(pair_px(pair, usable, 8.0) <= screen, "{pair:?}");
            assert!(pair.1.resolve(usable) >= SIDE_BY_SIDE_MIN_PX, "{pair:?}");
        }
        // Phase `a_wide_window_shares_the_screen_equally`.
        {
            let (usable, screen) = (997.0, 1248.0);
            let pair = share_beside(WindowWidth::Wide, WindowWidth::Normal, usable, screen, 8.0)
                .expect("a full-width window leaves no room beside it");
            assert_eq!(pair.0, pair.1, "neither keeps more than its half");
            assert!(pair_px(pair, usable, 8.0) <= screen, "{pair:?}");
            assert!(
                pair.0.resolve(usable) >= 615.0,
                "the pair uses the whole screen, not just the usable width: {pair:?}"
            );
        }
        // Phase `room_beside_a_narrow_window_is_taken_without_moving_it`.
        {
            let usable = 1904.0;
            let pair = share_beside(
                WindowWidth::Normal,
                WindowWidth::Normal,
                usable,
                usable,
                8.0,
            )
            .expect("two two-thirds windows overflow a wide screen too");
            assert_eq!(
                pair.0,
                WindowWidth::Normal,
                "the window in use keeps its width"
            );
            assert!(pair_px(pair, usable, 8.0) <= usable, "{pair:?}");
            assert!(pair.1.resolve(usable) >= SIDE_BY_SIDE_MIN_PX, "{pair:?}");
        }
        // Phase `windows_that_already_fit_are_left_alone`.
        {
            assert_eq!(
                share_beside(
                    WindowWidth::Narrow,
                    WindowWidth::Narrow,
                    1904.0,
                    1904.0,
                    8.0
                ),
                None
            );
        }
        // Phase `a_screen_too_narrow_to_share_scrolls_instead`.
        {
            assert_eq!(
                share_beside(WindowWidth::Normal, WindowWidth::Normal, 800.0, 800.0, 8.0),
                None,
                "two windows under the side-by-side minimum would both be unusable"
            );
        }
    }

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
        }
        // Phase `there_is_no_widest_window_anyone_can_feel`: sixty-five strips
        // (the old storage's end) and far more load as they were stored.
        {
            let width: WindowWidth = serde_json::from_str(r#"{"Fraction":65000}"#).unwrap();
            assert_eq!(width, WindowWidth::Fraction(65_000));
            let width: WindowWidth = serde_json::from_str(r#"{"Fraction":2000000}"#).unwrap();
            assert!((width.resolve(1000.0) - 2_000_000.0).abs() < 1e-6);
            // An hour of a drag resting at the display's edge, where the strip
            // scrolls fastest, is far short of the storable end.
            let hour_of_scroll = 1600.0 * 3600.0;
            assert!(max_group_px(1000.0) > 100.0 * hour_of_scroll);
            assert_eq!(
                permille_for_px(hour_of_scroll, 1000.0),
                5_760_000,
                "stored as it landed"
            );
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
