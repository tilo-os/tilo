//! Reference geometry for the shell's hand-composed chrome. These are the sizes and
//! distances that are not a control height, a panel width or a spacing step: a resize
//! grip, a pinned avatar, a strip switcher card.

use super::Metrics;

/// Reference geometry (unscaled) for the shell's hand-composed chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub resize_grip: f32,
    pub window_grip: f32,
    /// The same band under touch: wider, because a finger covers more than a pointer.
    pub window_touch_grip: f32,
    pub jump_px_per_detent: f32,
    pub pin: f32,
    pub avatar_step: f32,
    pub avatar: f32,
    pub compact_avatar_step: f32,
    pub row_header: f32,
    pub team_row: f32,
    pub section_height: f32,
    pub divider_height: f32,
    pub strip_card_width: f32,
    pub pairing_sheet_height: f32,
    pub pairing_qr_side: f32,
    pub display_brightness_step: f32,
    pub wheel_step: f32,
    pub bar_reach: f32,
    pub desk_grip: f32,
    pub desk_art: f32,
    pub desk_badge: f32,
    pub identify_name_width: f32,
    pub search_card_width: f32,
    pub sheet_title_band: f32,
    pub sheet_detail_line: f32,
    pub sheet_footer: f32,
    pub field_label_height: f32,
    pub field_prose_height: f32,
}

impl Layout {
    /// The unscaled reference geometry.
    pub const BASE: Self = Self {
        resize_grip: 6.0,
        window_grip: 8.0,
        window_touch_grip: 22.0,
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
        bar_reach: 28.0,
        desk_grip: 18.0,
        desk_art: 48.0,
        desk_badge: 20.0,
        identify_name_width: 200.0,
        search_card_width: 640.0,
        sheet_title_band: 74.0,
        sheet_detail_line: 20.0,
        sheet_footer: 84.0,
        field_label_height: 22.0,
        field_prose_height: 88.0,
    };
}

impl Default for Layout {
    fn default() -> Self {
        Self::BASE
    }
}

/// The one size class a surface derives from the rect it is laid out in. A window's shape
/// is a fact about its current allocation, never a decision an app (or the host that
/// launched it) stores: every layout pass asks this of the rect it was just handed, so a
/// window narrowed or widened shows the layout that rect asks for on the very next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Breakpoint {
    #[default]
    Compact,
    Medium,
    Wide,
}

impl Breakpoint {
    /// The widths this class asks for, from the tokens: `(compact, wide)` — below the
    /// first a surface shows one column at a time, at or above the second it shows a
    /// column beside the primary one.
    #[must_use]
    pub fn thresholds(m: &Metrics) -> (f64, f64) {
        (
            f64::from(m.compact_breakpoint),
            f64::from(m.compact_breakpoint) + f64::from(m.overview_width),
        )
    }

    /// The class `width` asks for, without hysteresis: the answer a fresh rect gets.
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

/// The class a surface holds while it lays out, re-derived from the rect of every pass
/// ([`Widget::apply_layout`](crate::widget::Widget::apply_layout)).
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

    /// Derive the class `width` asks for, holding the class of the last pass until the
    /// width is clear of its threshold by the hysteresis band.
    pub fn derive(&mut self, width: f64, m: &Metrics) -> Breakpoint {
        let (compact_at, wide_at) = Breakpoint::thresholds(m);
        let band = f64::from(m.layout_hysteresis);
        let next = match self.held {
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

    /// Forget the held class: the surface has no rect to speak of (it was closed, or has
    /// not been laid out since its tree was rebuilt), so the next pass derives from its
    /// rect alone.
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
        assert_eq!(tracker.derive(compact_at - 1.0, &m), Breakpoint::Medium);
        assert_eq!(
            tracker.derive(compact_at - band + 1.0, &m),
            Breakpoint::Medium
        );
        // The band's own edge still holds: a class is clear of it only past that edge,
        // exactly as `NavShell`'s own thresholds read.
        assert_eq!(tracker.derive(compact_at - band, &m), Breakpoint::Medium);
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
