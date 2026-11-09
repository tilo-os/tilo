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

/// Widest a [`WindowWidth::Normal`] window resolves to: two thirds of a
/// 1920 px display. On a wider screen two thirds is a window mostly made of
/// empty panes and lines too long to read, so the default stops growing and
/// the strip shows more of its neighbours instead.
pub const NORMAL_MAX_PX: f64 = 1280.0;

/// Widest a [`WindowWidth::Normal`] window alone on its strip resolves to.
/// Two thirds is the default so that a neighbour has room beside it; with
/// no neighbour that third is wallpaper nobody can use, so a lone window
/// takes the strip, up to the width of a 1920 px display, past which a
/// single window is mostly empty panes and over-long lines.
pub const ALONE_MAX_PX: f64 = 1920.0;

/// Narrowest a window is made to open beside another so that both are on
/// screen together. Below it, an app's own layout (a sidebar and a body,
/// a header with its controls) no longer fits, so a share narrower than
/// this is worse than the strip scrolling to show the new window whole.
pub const SIDE_BY_SIDE_MIN_PX: f64 = 480.0;

/// Widths that put a window just opened beside `neighbour` on screen
/// together with it: both within `strip_usable_px`, the strip's width less
/// the edge gutters every window stays clear of, the two `gap_px` apart.
/// `screen_px` is the display the strip stands on, its edge gutters
/// included, and only decides whether that display is wide enough to split:
/// the widths are fractions of the usable width either way. `None` when the
/// two already fit, or when the display has no room for two windows
/// of [`SIDE_BY_SIDE_MIN_PX`]: the strip scrolls then, framing the new window
/// whole on its own.
///
/// The window the person was in keeps its width when the space beside it
/// is enough for the new one; otherwise the two share the strip equally.
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
    let room = usable - gap_px - beside;
    if room >= opened.resolve(usable) {
        return None;
    }
    // Floored so that rounding never pushes the pair a pixel past the edge.
    let fitting = |px: f64| WindowWidth::Fraction(floor_permille(px, usable));
    if room >= SIDE_BY_SIDE_MIN_PX {
        return Some((neighbour, fitting(room)));
    }
    let half = (usable - gap_px) / 2.0;
    // Whether the display is worth splitting is a question about the
    // display, whose edge gutters are chrome outside the windows: a display
    // a gutter short of two full windows shows both of them a little
    // smaller rather than scrolling one out of sight. The widths themselves
    // stay fractions of the strip's usable width, so the pair tiles it and
    // both gutters stand.
    let screen_half = (screen_px.max(usable) - gap_px) / 2.0;
    (screen_half >= SIDE_BY_SIDE_MIN_PX).then(|| (fitting(half), fitting(half)))
}

/// The widths a window opened over the desk takes beside the window it
/// opened next to: the card it draws at — the window, the band of its own
/// ground round it and the focus ring outside that — is `card_px` wide, and
/// the window beside it gives up exactly that much of the strip, the two
/// `gap_px` apart, so the whole card stands beside what the person was reading
/// instead of over it. `None` when the strip has no room for a window beside
/// the card.
///
/// Returns `(beside, card)`.
pub fn share_for_float(
    card_px: f64,
    strip_usable_px: f64,
    gap_px: f64,
) -> Option<(WindowWidth, WindowWidth)> {
    let usable = strip_usable_px.max(1.0);
    let beside = usable - gap_px - card_px;
    (beside >= min_group_px(usable)).then(|| {
        (
            WindowWidth::Fraction(floor_permille(beside, usable)),
            WindowWidth::Fraction(floor_permille(card_px, usable)),
        )
    })
}

/// The largest per-mille of `usable` that [`WindowWidth::resolve`]s to no
/// more than `px`.
///
/// Floored on the pixels it resolves to, not on the fraction: `resolve`
/// rounds the fraction back to whole pixels, and a fraction floored before
/// that rounding can resolve a pixel short of the room it was given — which,
/// for a pair sharing the strip, would leave the trailing gutter wider than
/// the leading one.
fn floor_permille(px: f64, usable: f64) -> u32 {
    let mut permille = permille_for_px(px, usable);
    while permille > WIDTH_MIN_PERMILLE && WindowWidth::Fraction(permille).resolve(usable) > px {
        permille = permille.saturating_sub(1);
    }
    permille
}

/// Give every window of `strip` the same share of it: each one
/// `1000 / count` per-mille wide, so `count` windows tile the strip's
/// usable width. Whether any window's width changed.
pub fn equalize_widths(strip: &mut crate::layout::Strip) -> bool {
    let count = u32::try_from(strip.window_groups.len()).unwrap_or(u32::MAX);
    if count == 0 {
        return false;
    }
    let share = WIDTH_WIDE_PERMILLE
        .checked_div(count)
        .unwrap_or(WIDTH_MIN_PERMILLE)
        .max(WIDTH_MIN_PERMILLE);
    let width = WindowWidth::Fraction(share);
    let mut changed = false;
    for group in &mut strip.window_groups {
        if group.width != width {
            group.width = width;
            changed = true;
        }
    }
    changed
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
        let px = (usable * self.fraction()).round();
        // The default width suits the screen it is on: never wider than
        // reads well on a big display, and never narrower than an app's
        // two-pane layout needs, so on a phone a window takes the whole
        // strip instead of a sliver with the next one peeking in. A
        // person's own resize (a `Fraction`) and the tiers they pick stay
        // what they asked for.
        let px = match self {
            WindowWidth::Normal => px
                .min(NORMAL_MAX_PX)
                .max(SIDE_BY_SIDE_MIN_PX.min(usable).round()),
            _ => px,
        };
        px.clamp(min_group_px(usable), max_group_px(usable))
    }

    /// [`Self::resolve`] for a window that is `alone` on its strip or not.
    /// A lone window at the default width takes the strip's usable width
    /// (up to [`ALONE_MAX_PX`]) instead of leaving a third of it empty;
    /// once a second window joins it, it is two thirds again and the
    /// newcomer takes the rest ([`share_beside`]). A width the person chose
    /// (a tier or their own resize) stays what they asked for either way.
    pub fn resolve_in_strip(&self, strip_usable_px: f64, alone: bool) -> f64 {
        if alone && matches!(self, WindowWidth::Normal) {
            let usable = strip_usable_px.max(1.0);
            return usable
                .min(ALONE_MAX_PX)
                .max(self.resolve(usable))
                .clamp(min_group_px(usable), max_group_px(usable));
        }
        self.resolve(strip_usable_px)
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
    fn a_window_opened_over_the_desk_takes_the_width_of_the_card_it_draws_at() {
        let (usable, gap, card) = (1216.0, 10.0, 640.0);
        let (beside, floating) =
            share_for_float(card, usable, gap).expect("a strip holds a window beside a card");
        assert!(
            (floating.resolve(usable) - card).abs() <= 1.0,
            "the card's place is the width it draws at: {:?}",
            floating.resolve(usable)
        );
        assert!(
            beside.resolve(usable) + gap + floating.resolve(usable) <= usable,
            "the two tile the strip: {beside:?} and {floating:?}"
        );
        assert!(
            beside.resolve(usable) >= min_group_px(usable),
            "the window beside the card keeps a window's least width: {beside:?}"
        );
        assert_eq!(
            share_for_float(card, 700.0, gap),
            None,
            "a strip too narrow for a window beside the card leaves none"
        );
    }

    #[test]
    fn a_window_opened_beside_another_shares_the_screen_with_it() {
        // Phase `two_windows_on_a_1280_screen_both_fit_on_it`.
        {
            // A 1280 px display whose strip keeps a 32 px gutter at each
            // end: two-thirds of what is left is 811 px, and two of those
            // overflow the usable width.
            let (usable, screen, gap) = (1216.0, 1280.0, 8.0);
            let pair = share_beside(
                WindowWidth::Normal,
                WindowWidth::Normal,
                usable,
                screen,
                gap,
            )
            .expect("two two-thirds windows overflow one strip");
            assert_eq!(pair.0, pair.1, "neither keeps more than its half");
            assert!(pair_px(pair, usable, gap) <= usable, "{pair:?}");
            assert!(pair.0.resolve(usable) >= SIDE_BY_SIDE_MIN_PX, "{pair:?}");
            assert!(
                usable - pair_px(pair, usable, gap) < 1.0,
                "the pair tiles the strip, both gutters stand: {pair:?}"
            );
        }
        {
            let (usable, screen, gap) = (1216.0, 1280.0, 8.0);
            let pair = share_beside(WindowWidth::Wide, WindowWidth::Normal, usable, screen, gap)
                .expect("a full-width window leaves no room beside it");
            assert_eq!(pair.0, pair.1, "neither keeps more than its half");
            assert!(pair_px(pair, usable, gap) <= usable, "{pair:?}");
            assert!(
                pair.0.resolve(usable) >= 603.0,
                "the pair uses the usable width: {pair:?}"
            );
        }
        {
            let usable = 1904.0;
            let screen = usable + 2.0 * 32.0;
            let pair = share_beside(
                WindowWidth::Normal,
                WindowWidth::Normal,
                usable,
                screen,
                8.0,
            )
            .expect("two two-thirds windows overflow a wide strip too");
            assert_eq!(
                pair.0,
                WindowWidth::Normal,
                "on a wide strip the window the person was in keeps its width"
            );
            assert!(pair_px(pair, usable, 8.0) <= usable, "{pair:?}");
            assert!(
                usable - pair_px(pair, usable, 8.0) < 1.0,
                "the newcomer takes the room beside it, both gutters stand: {pair:?}"
            );
            assert!(pair.1.resolve(usable) >= SIDE_BY_SIDE_MIN_PX, "{pair:?}");
        }
        {
            assert_eq!(
                share_beside(
                    WindowWidth::Narrow,
                    WindowWidth::Narrow,
                    1904.0,
                    1904.0 + 64.0,
                    8.0
                ),
                None
            );
        }
        {
            assert_eq!(
                share_beside(
                    WindowWidth::Normal,
                    WindowWidth::Normal,
                    800.0,
                    800.0 + 64.0,
                    8.0
                ),
                None,
                "two windows under the side-by-side minimum would both be unusable"
            );
        }
        {
            // A 1024 px display: each window's share of the strip is 476 px,
            // four short of the minimum, and the pair still shares it rather
            // than scrolling one window out of sight — the strip's 32 px
            // gutters are chrome, not window.
            let (usable, screen, gap) = (960.0, 1024.0, 8.0);
            let pair = share_beside(
                WindowWidth::Normal,
                WindowWidth::Normal,
                usable,
                screen,
                gap,
            )
            .expect("a 1024 px display shows two windows");
            assert_eq!(pair.0, pair.1, "{pair:?}");
            assert!(
                (pair.0.resolve(usable) - 476.0).abs() <= 1.0,
                "each takes half the strip's usable width: {pair:?}"
            );
            assert!(pair_px(pair, usable, gap) <= usable, "{pair:?}");
        }
    }

    #[test]
    fn the_default_width_suits_very_wide_and_very_narrow_screens() {
        // On a 1920 px display two thirds is as wide as a default window gets.
        assert!((WindowWidth::Normal.resolve(1920.0) - NORMAL_MAX_PX).abs() < 1.0);
        // A 2560 px display does not stretch it into a mostly empty pane.
        assert!((WindowWidth::Normal.resolve(2496.0) - NORMAL_MAX_PX).abs() < f64::EPSILON);
        assert!((WindowWidth::Normal.resolve(5000.0) - NORMAL_MAX_PX).abs() < f64::EPSILON);
        assert!((WindowWidth::Normal.resolve(1376.0) - 918.0).abs() < f64::EPSILON);
        // A phone's strip is one window wide: no sliver, no neighbour peeking.
        assert!((WindowWidth::Normal.resolve(326.0) - 326.0).abs() < f64::EPSILON);
        assert!((WindowWidth::Normal.resolve(600.0) - SIDE_BY_SIDE_MIN_PX).abs() < f64::EPSILON);
        assert!((WindowWidth::Wide.resolve(2496.0) - 2496.0).abs() < f64::EPSILON);
        assert!((WindowWidth::Fraction(667).resolve(2496.0) - 1665.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_lone_default_window_takes_the_strip_and_a_chosen_width_stays() {
        // The 1440 px browser desktop: 1376 px between the strip's gutters.
        assert!((WindowWidth::Normal.resolve_in_strip(1376.0, true) - 1376.0).abs() < f64::EPSILON);
        assert!((WindowWidth::Normal.resolve_in_strip(1376.0, false) - 918.0).abs() < f64::EPSILON);
        // A 1920 px display fills; a 2560 px one stops at a 1920 px window.
        assert!((WindowWidth::Normal.resolve_in_strip(1856.0, true) - 1856.0).abs() < f64::EPSILON);
        assert!(
            (WindowWidth::Normal.resolve_in_strip(2496.0, true) - ALONE_MAX_PX).abs()
                < f64::EPSILON
        );
        // What the person picked is not widened.
        assert!(
            (WindowWidth::Narrow.resolve_in_strip(1376.0, true)
                - WindowWidth::Narrow.resolve(1376.0))
            .abs()
                < f64::EPSILON
        );
        assert!(
            (WindowWidth::Fraction(500).resolve_in_strip(1376.0, true) - 688.0).abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn widths_load_as_clamped_fractions_with_a_pixel_floor() {
        {
            assert!(serde_json::from_str::<WindowWidth>(r#"{"Custom":600}"#).is_err());
        }
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
        {
            // A 10% window on a 3840 px strip is 384 px: no share floor.
            assert!((WindowWidth::Fraction(100).resolve(3840.0) - 384.0).abs() < 1.0);
            assert!((WindowWidth::Fraction(10).resolve(1472.0) - WIDTH_FLOOR_PX).abs() < 1e-9);
            assert!((WindowWidth::Fraction(2500).resolve(1000.0) - 2500.0).abs() < 1e-9);
        }
        {
            assert!(
                (WindowWidth::Fraction(150).resolve(800.0) - WIDTH_FLOOR_PX).abs() < f64::EPSILON
            );
            assert!((WindowWidth::Wide.resolve(1000.0) - 1000.0).abs() < f64::EPSILON);
        }
    }

    /// Every window of a strip takes the same share of it, and a window
    /// already at that share is left alone.
    #[test]
    fn equalizing_a_strip_gives_every_window_the_same_share() {
        use crate::ids::StripId;
        use crate::layout::{Strip, WindowGroup};

        let group = |id: u64, width: WindowWidth| WindowGroup {
            id,
            active_tab: id,
            tabs: Vec::new(),
            width,
            maximized: false,
            focus_history: Vec::new(),
        };
        let mut strip = Strip {
            id: StripId(1),
            name: "Desk".into(),
            window_groups: vec![
                group(1, WindowWidth::Narrow),
                group(2, WindowWidth::Wide),
                group(3, WindowWidth::Fraction(500)),
            ],
        };
        assert!(equalize_widths(&mut strip));
        let shares: Vec<WindowWidth> = strip
            .window_groups
            .iter()
            .map(|group| group.width)
            .collect();
        assert_eq!(
            shares,
            vec![WindowWidth::Fraction(333); 3],
            "three windows tile the strip"
        );
        assert!(!equalize_widths(&mut strip), "nothing left to change");
        let alone = &mut Strip {
            window_groups: vec![group(4, WindowWidth::Narrow)],
            ..strip
        };
        assert!(equalize_widths(alone));
        assert_eq!(alone.window_groups[0].width, WindowWidth::Fraction(1000));
    }
}
