//! Reference geometry for the shell's hand-composed chrome.
//!
//! These are the sizes and distances that are not a control height, a panel
//! width or a spacing step: a resize grip, a pinned avatar, a strip switcher
//! card. They live here, beside the other tokens, so a shell file names the
//! token instead of restating the number. [`Layout::BASE`] is the unscaled
//! reference, readable from a `const` context.
//!
//! [`Breakpoint`] lives here too, because it is the one size class every
//! surface shares: it is a token's answer about a rect, not a number an app
//! keeps.

use super::Metrics;

/// Reference geometry (unscaled) for the shell's hand-composed chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// How far from a timeline event's edge a pointer can grab to resize it.
    pub resize_grip: f32,
    pub jump_px_per_detent: f32,
    /// A pinned participant's face on the desk.
    pub pin: f32,
    /// Between two pinned participant faces.
    pub avatar_step: f32,
    /// A compact avatar in the strip switcher or a pairing sheet.
    pub avatar: f32,
    /// Between two compact avatars.
    pub compact_avatar_step: f32,
    /// One header row above a desk card's list.
    pub row_header: f32,
    /// One row of the desk's team list.
    pub team_row: f32,
    /// A compact section row in the strip switcher.
    pub section_height: f32,
    /// The rule between two strip switcher sections.
    pub divider_height: f32,
    /// A strip switcher card.
    pub strip_card_width: f32,
    pub pairing_sheet_height: f32,
    pub pairing_qr_side: f32,
    pub display_brightness_step: f32,
    /// One line-stepping wheel notch.
    pub wheel_step: f32,
}

impl Layout {
    /// The unscaled reference geometry.
    pub const BASE: Self = Self {
        resize_grip: 6.0,
        jump_px_per_detent: 50.0,
        pin: 26.0,
        avatar_step: 22.0,
        avatar: 18.0,
        compact_avatar_step: 13.0,
        row_header: 36.0,
        team_row: 62.0,
        section_height: 26.0,
        divider_height: 9.0,
        strip_card_width: 272.0,
        pairing_sheet_height: 576.0,
        pairing_qr_side: 232.0,
        display_brightness_step: 10.0,
        wheel_step: 15.0,
    };
}

impl Default for Layout {
    fn default() -> Self {
        Self::BASE
    }
}

/// The one size class a surface derives from the rect it is laid out in.
///
/// A window's shape is a fact about its current allocation, never a decision
/// an app (or the host that launched it) stores: every layout pass asks this
/// of the rect it was just handed, so a window narrowed or widened shows the
/// layout that rect asks for on the very next frame.
///
/// * [`Compact`](Self::Compact) — one column at a time: phone width, a
///   tablet held upright, a window dragged narrow. The sidebar, list and
///   content are screens the person moves between.
/// * [`Medium`](Self::Medium) — a docked sidebar and one primary column
///   beside it: a small laptop, a large tablet on its side.
/// * [`Wide`](Self::Wide) — a sidebar, a primary column and a second column
///   (an overview beside a detail, a document beside an inspector).
///
/// The thresholds come from [`Metrics`], so they grow with `ui_scale` and the
/// text scale exactly as the panels they separate do. A surface that holds a
/// class across frames uses [`BreakpointTracker`], which keeps a threshold
/// from flapping when a window is dragged across it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Breakpoint {
    /// One column at a time.
    #[default]
    Compact,
    /// A docked sidebar and a primary column.
    Medium,
    /// A sidebar, a primary column and a column beside it.
    Wide,
}

impl Breakpoint {
    /// The widths this class asks for, from the tokens: `(compact, wide)` —
    /// below the first a surface shows one column at a time, at or above the
    /// second it shows a column beside the primary one. Between them it
    /// docks its sidebar and keeps one column.
    ///
    /// The compact threshold is the sidebar beside the content it has to
    /// keep ([`Metrics::compact_breakpoint`]); a second column joins at the
    /// overview's width past it, which is where a list beside a detail fits
    /// beside a docked sidebar.
    #[must_use]
    pub fn thresholds(m: &Metrics) -> (f64, f64) {
        (
            f64::from(m.compact_breakpoint),
            f64::from(m.compact_breakpoint) + f64::from(m.overview_width),
        )
    }

    /// The class `width` asks for, without hysteresis: the answer a fresh
    /// rect gets. A surface that lays out repeatedly asks
    /// [`BreakpointTracker::derive`] instead.
    ///
    /// `width` is in the space `m` is measured in. A surface whose text has
    /// grown divides its rect's width by [`Metrics::text_growth`] first, so a
    /// window at 200 % text reaches each class where a 1× window half its
    /// width does — the rule every text-sized breakpoint in the toolkit
    /// already follows.
    #[must_use]
    pub fn of(width: f64, m: &Metrics) -> Self {
        let (compact, wide) = Self::thresholds(m);
        if width >= wide {
            Self::Wide
        } else if width >= compact {
            Self::Medium
        } else {
            Self::Compact
        }
    }

    /// Whether the surface shows one column at a time.
    #[must_use]
    pub fn is_compact(self) -> bool {
        self == Self::Compact
    }

    /// Whether the surface has room for a column beside its primary one.
    #[must_use]
    pub fn is_wide(self) -> bool {
        self == Self::Wide
    }
}

/// The class a surface holds while it lays out, re-derived from the rect of
/// every pass ([`Widget::apply_layout`](crate::widget::Widget::apply_layout)).
///
/// Hysteresis is the point: a window dragged to rest exactly on a threshold
/// would otherwise flicker between two layouts, a frame each, as the person
/// lets go. A class that does not hold has to see a threshold pass by
/// [`Metrics::layout_hysteresis`] before it changes, the same band
/// [`NavShell`](crate::widget::NavShell) uses for its own thresholds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BreakpointTracker {
    held: Option<Breakpoint>,
}

impl BreakpointTracker {
    /// A tracker that has not laid out yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive the class `width` asks for, holding the class of the last pass
    /// until the width is clear of its threshold by the hysteresis band.
    pub fn derive(&mut self, width: f64, m: &Metrics) -> Breakpoint {
        let (compact_at, wide_at) = Breakpoint::thresholds(m);
        let band = f64::from(m.layout_hysteresis);
        let next = match self.held {
            // A surface that has not laid out takes the rect it is given.
            None => Breakpoint::of(width, m),
            Some(Breakpoint::Compact) => {
                if width >= wide_at + band {
                    Breakpoint::Wide
                } else if width >= compact_at + band {
                    Breakpoint::Medium
                } else {
                    Breakpoint::Compact
                }
            }
            Some(Breakpoint::Medium) => {
                if width >= wide_at + band {
                    Breakpoint::Wide
                } else if width >= compact_at - band {
                    Breakpoint::Medium
                } else {
                    Breakpoint::Compact
                }
            }
            Some(Breakpoint::Wide) => {
                if width >= wide_at - band {
                    Breakpoint::Wide
                } else if width >= compact_at - band {
                    Breakpoint::Medium
                } else {
                    Breakpoint::Compact
                }
            }
        };
        self.held = Some(next);
        next
    }

    /// The class of the last pass, `None` before the first.
    #[must_use]
    pub fn held(&self) -> Option<Breakpoint> {
        self.held
    }

    /// Forget the held class: the surface has no rect to speak of (it was
    /// closed, or has not been laid out since its tree was rebuilt), so the
    /// next pass derives from its rect alone.
    pub fn forget(&mut self) {
        self.held = None;
    }
}

#[cfg(test)]
mod breakpoint_tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics::BASE
    }

    #[test]
    fn the_thresholds_are_tokens_not_literals() {
        let m = metrics();
        let (compact, wide) = Breakpoint::thresholds(&m);
        assert_eq!(compact, f64::from(m.compact_breakpoint));
        assert_eq!(wide, f64::from(m.compact_breakpoint + m.overview_width));
        let scaled = m.scaled(1.5);
        let (compact, wide) = Breakpoint::thresholds(&scaled);
        assert!((compact - 1260.0).abs() < 1e-4, "ui scale grows it");
        assert!(wide > compact);
    }

    #[test]
    fn each_size_lands_in_its_class() {
        let m = metrics();
        // The product acceptance sizes: phone, tablet, laptop, desktop.
        assert_eq!(Breakpoint::of(390.0, &m), Breakpoint::Compact);
        assert_eq!(Breakpoint::of(768.0, &m), Breakpoint::Compact);
        assert_eq!(Breakpoint::of(1366.0, &m), Breakpoint::Wide);
        assert_eq!(Breakpoint::of(1920.0, &m), Breakpoint::Wide);
        assert_eq!(Breakpoint::of(1000.0, &m), Breakpoint::Medium);
        let (compact, wide) = Breakpoint::thresholds(&m);
        assert_eq!(Breakpoint::of(compact, &m), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(wide - 1.0, &m), Breakpoint::Medium);
        assert_eq!(Breakpoint::of(wide, &m), Breakpoint::Wide);
    }

    #[test]
    fn a_tracked_class_does_not_flutter_on_its_threshold() {
        let m = metrics();
        let (compact_at, _) = Breakpoint::thresholds(&m);
        let band = f64::from(m.layout_hysteresis);
        let mut tracker = BreakpointTracker::new();
        assert_eq!(tracker.derive(compact_at + 1.0, &m), Breakpoint::Medium);
        // Just under the threshold, inside the band: it stays Medium.
        assert_eq!(tracker.derive(compact_at - 1.0, &m), Breakpoint::Medium);
        assert_eq!(
            tracker.derive(compact_at - band + 1.0, &m),
            Breakpoint::Medium
        );
        // The band's own edge still holds: a class is clear of it only past
        // that edge, exactly as `NavShell`'s own thresholds read.
        assert_eq!(tracker.derive(compact_at - band, &m), Breakpoint::Medium);
        // Clear of the band it is Compact, and now needs the band's room back.
        assert_eq!(
            tracker.derive(compact_at - band - 0.5, &m),
            Breakpoint::Compact
        );
        assert_eq!(tracker.derive(compact_at, &m), Breakpoint::Compact);
        assert_eq!(
            tracker.derive(compact_at + band - 1.0, &m),
            Breakpoint::Compact
        );
        assert_eq!(tracker.derive(compact_at + band, &m), Breakpoint::Medium);
    }

    #[test]
    fn a_tracker_that_has_not_laid_out_takes_its_rect() {
        let m = metrics();
        let mut tracker = BreakpointTracker::new();
        assert_eq!(tracker.held(), None);
        assert_eq!(tracker.derive(390.0, &m), Breakpoint::Compact);
        assert_eq!(tracker.held(), Some(Breakpoint::Compact));
        tracker.forget();
        assert_eq!(tracker.held(), None);
        assert_eq!(tracker.derive(1920.0, &m), Breakpoint::Wide);
    }

    #[test]
    fn a_large_type_window_reaches_wide_later() {
        let m = metrics();
        let large = m.scaled(2.0);
        assert_eq!(Breakpoint::of(1366.0, &m), Breakpoint::Wide);
        assert_eq!(
            Breakpoint::of(1366.0, &large),
            Breakpoint::Compact,
            "the same window holds less at twice the density"
        );
        assert_eq!(
            Breakpoint::of(2000.0, &large),
            Breakpoint::Medium,
            "and reaches the next class where a 1x window reaches it at half the width"
        );
    }
}
