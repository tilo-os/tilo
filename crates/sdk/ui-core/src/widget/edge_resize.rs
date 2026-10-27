//! Resizing windows on a strip: which window an edge drag changes, and how.
//!
//! The strip is infinitely wide, so a resize is never zero-sum unless the
//! person asks for it. Two functions hold the whole model, and every pointer
//! path ([`crate::widget::GroupRow`]'s hover, press and drag) goes through
//! them:
//!
//! - [`seam_owner`] — **who owns an edge.** Every window owns both of its
//!   edges. The seam between two windows is split down the middle, the half
//!   nearest each window being its edge, except that the window you are
//!   working in (the focused one) owns the whole seam. Hovering shows that
//!   owner's edge before anything is pressed.
//! - [`EdgeDrag::resize`] — **what a drag does.** A plain drag changes only
//!   the grabbed window: its width follows the pointer 1:1 and its *opposite*
//!   edge holds still on screen, so everything beyond the dragged edge is
//!   pushed away or pulled along at its own width. Shift+drag is zero-sum
//!   between the two windows sharing that seam and nothing else. A limit (the
//!   app's real minimum, a client's maximum) stops the edge; nothing else is
//!   resized to make up for it.
//!
//! This is a design rule, not a tuning: `docs/architecture/window-management.md`
//! ("Resizing a window") is its contract, and the tests below pin each case.

use vello::kurbo::Rect;

/// One of a window's two vertical edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEdge {
    /// The left edge (the start of a left-to-right strip).
    Leading,
    /// The right edge.
    Trailing,
}

impl WindowEdge {
    /// The edge that holds still on screen while this one is dragged.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            Self::Leading => Self::Trailing,
            Self::Trailing => Self::Leading,
        }
    }
}

/// A window edge a pointer can grab: the strip's `group`-th window, and
/// which of its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeGrab {
    pub group: usize,
    pub edge: WindowEdge,
}

impl EdgeGrab {
    /// The window on the other side of this edge, if the edge is a seam:
    /// `count` windows in the run, so the first window's leading edge and
    /// the last one's trailing edge have none.
    #[must_use]
    pub fn across(self, count: usize) -> Option<usize> {
        match self.edge {
            WindowEdge::Leading => self.group.checked_sub(1),
            WindowEdge::Trailing => self.group.checked_add(1).filter(|&next| next < count),
        }
    }
}

/// The two windows either side of a seam, as laid out on screen: the
/// strip's `left`-th window and the one after it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seam {
    pub left: usize,
    pub left_rect: Rect,
    pub left_focused: bool,
    pub right_rect: Rect,
    pub right_focused: bool,
}

/// **The seam rule**: whose edge a point `x` on the seam between two windows
/// is.
///
/// Every window owns both of its edges, and the seam is split between the
/// two windows, the half nearest each being that window's edge. The window
/// the person is working in (focused) owns the whole seam: the seam is about
/// two grab zones plus the gap, too small to aim at halves reliably, and
/// "resize the thing I have open" is the person's own model of it.
#[must_use]
pub fn seam_owner(seam: &Seam, x: f64) -> EdgeGrab {
    let left = EdgeGrab {
        group: seam.left,
        edge: WindowEdge::Trailing,
    };
    let right = EdgeGrab {
        group: seam.left.saturating_add(1),
        edge: WindowEdge::Leading,
    };
    match (seam.left_focused, seam.right_focused) {
        (true, false) => left,
        (false, true) => right,
        _ if x < f64::midpoint(seam.left_rect.x1, seam.right_rect.x0) => left,
        _ => right,
    }
}

/// How a drag shares out width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeMode {
    /// A plain drag: only the grabbed window changes; the strip grows or
    /// shrinks by the same amount.
    Push,
    /// Shift+drag: what one of the two windows at the seam gains, the other
    /// loses. The only redistribution there is.
    ZeroSum,
}

/// The widths a window may take, in logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidthLimits {
    /// The app's real minimum: a Tilo app's compact layout, a client's
    /// `xdg_toplevel` min-size hint, the strip's floor for apps that say
    /// nothing.
    pub min: f64,
    /// A client's `xdg_toplevel` max-size hint; unbounded otherwise.
    pub max: f64,
}

impl WidthLimits {
    fn clamp(self, width: f64) -> f64 {
        width.clamp(self.min, self.max.max(self.min))
    }
}

/// One window of a drag, as it was when the drag began.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DraggedWindow {
    pub group: usize,
    pub width: f64,
    pub limits: WidthLimits,
}

/// An edge drag, from the moment it was pressed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeDrag {
    pub edge: WindowEdge,
    /// The window whose edge was grabbed.
    pub grabbed: DraggedWindow,
    /// The window across the seam a Shift+drag trades width with. `None` for
    /// a plain drag, and for a Shift+drag on an outer edge, which has no one
    /// to trade with and so pushes like a plain one.
    pub zero_sum_with: Option<DraggedWindow>,
}

/// What a drag sets for one pointer position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeResize {
    /// The grabbed window's width.
    pub width: f64,
    /// The width of [`EdgeDrag::zero_sum_with`], when there is one.
    pub other_width: Option<f64>,
    /// How far the strip has to pan (positive: its canvas moves left) so the
    /// grabbed window's opposite edge stays where it was on screen. Layout
    /// places windows from the strip's start, so only a leading-edge push
    /// needs one: its window grows towards the start, and everything before
    /// it moves away.
    pub pan: f64,
}

impl EdgeDrag {
    /// The mode this drag resizes in.
    #[must_use]
    pub fn mode(&self) -> ResizeMode {
        if self.zero_sum_with.is_some() {
            ResizeMode::ZeroSum
        } else {
            ResizeMode::Push
        }
    }

    /// The widths and pan for an edge carried `travel` px rightwards across
    /// the strip's canvas since the press: the pointer's travel on screen
    /// plus whatever the strip has auto-scrolled under it. Taking canvas
    /// travel is what keeps the edge under the pointer.
    ///
    /// Widths are whole pixels, as a client's geometry is: the travel is
    /// rounded once, here, so from whole-pixel starting widths every width
    /// and the pan come out whole and agree exactly, and the fixed edge
    /// does not wobble by a rounding.
    #[must_use]
    pub fn resize(&self, travel: f64) -> EdgeResize {
        let travel = travel.round();
        let grabbed = self.grabbed;
        let Some(other) = self.zero_sum_with else {
            let (width, pan) = match self.edge {
                WindowEdge::Trailing => (grabbed.limits.clamp(grabbed.width + travel), 0.0),
                WindowEdge::Leading => {
                    let width = grabbed.limits.clamp(grabbed.width - travel);
                    (width, width - grabbed.width)
                }
            };
            return EdgeResize {
                width,
                other_width: None,
                pan,
            };
        };
        // The seam moves; the window left of it grows by what the right one
        // loses. Both limits bound the one travel, so whichever binds first
        // stops the seam, and neither window is pushed past its own.
        let (left, right) = match self.edge {
            WindowEdge::Trailing => (grabbed, other),
            WindowEdge::Leading => (other, grabbed),
        };
        let lo = (left.limits.min - left.width).max(right.width - right.limits.max);
        let hi = (left.limits.max - left.width).min(right.width - right.limits.min);
        let seam = if lo <= hi { travel.clamp(lo, hi) } else { 0.0 };
        let (left_w, right_w) = (left.width + seam, right.width - seam);
        let (width, other_width) = match self.edge {
            WindowEdge::Trailing => (left_w, right_w),
            WindowEdge::Leading => (right_w, left_w),
        };
        EdgeResize {
            width,
            other_width: Some(other_width),
            pan: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREE: WidthLimits = WidthLimits {
        min: 160.0,
        max: f64::INFINITY,
    };

    fn seam(left_focused: bool, right_focused: bool) -> Seam {
        // Two windows 8 px apart: the gap's centre is at 604.
        Seam {
            left: 3,
            left_rect: Rect::new(0.0, 0.0, 600.0, 400.0),
            left_focused,
            right_rect: Rect::new(608.0, 0.0, 1208.0, 400.0),
            right_focused,
        }
    }

    const LEFT_TRAILING: EdgeGrab = EdgeGrab {
        group: 3,
        edge: WindowEdge::Trailing,
    };
    const RIGHT_LEADING: EdgeGrab = EdgeGrab {
        group: 4,
        edge: WindowEdge::Leading,
    };

    fn window(group: usize, width: f64) -> DraggedWindow {
        DraggedWindow {
            group,
            width,
            limits: FREE,
        }
    }

    #[test]
    fn the_seam_belongs_to_the_window_you_are_in_else_half_to_each() {
        {
            let s = seam(false, false);
            assert_eq!(seam_owner(&s, 594.0), LEFT_TRAILING, "inside the left grip");
            assert_eq!(seam_owner(&s, 603.9), LEFT_TRAILING, "left half of the gap");
            assert_eq!(
                seam_owner(&s, 604.0),
                RIGHT_LEADING,
                "right half of the gap"
            );
            assert_eq!(
                seam_owner(&s, 613.0),
                RIGHT_LEADING,
                "inside the right grip"
            );
        }
        // Phase `focus_wins_the_whole_seam`: grabbing Files' left edge, between
        // Files and a terminal, resizes Files when Files is the one in use,
        // however close to the terminal the pointer lands.
        {
            let files_on_the_right = seam(false, true);
            for x in [594.0, 600.0, 603.0, 604.0, 613.0] {
                assert_eq!(seam_owner(&files_on_the_right, x), RIGHT_LEADING, "x = {x}");
            }
            let files_on_the_left = seam(true, false);
            for x in [594.0, 604.0, 607.0, 613.0] {
                assert_eq!(seam_owner(&files_on_the_left, x), LEFT_TRAILING, "x = {x}");
            }
        }
        {
            assert_eq!(LEFT_TRAILING.across(5), Some(4));
            assert_eq!(RIGHT_LEADING.across(5), Some(3));
            let first = EdgeGrab {
                group: 0,
                edge: WindowEdge::Leading,
            };
            assert_eq!(first.across(5), None, "the strip's start is no seam");
            let last = EdgeGrab {
                group: 4,
                edge: WindowEdge::Trailing,
            };
            assert_eq!(last.across(5), None, "nor is its end");
        }
    }

    #[test]
    fn a_plain_drag_resizes_only_the_grabbed_window_and_holds_its_opposite_edge() {
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Trailing,
                grabbed: window(1, 500.0),
                zero_sum_with: None,
            };
            assert_eq!(drag.mode(), ResizeMode::Push);
            let r = drag.resize(40.0);
            assert_eq!(r.width, 540.0, "1:1 with the pointer");
            assert_eq!(r.other_width, None, "no other window changes size");
            assert_eq!(r.pan, 0.0, "the leading edge is laid out where it was");
            assert_eq!(drag.resize(-40.0).width, 460.0);
        }
        // Phase `leading_edge_grows_left_and_the_strip_pans_to_hold_the_right_edge`:
        // dragging Files' left edge left grows Files leftwards; the terminal
        // slides left at its own width and Files' right edge does not move.
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Leading,
                grabbed: window(2, 500.0),
                zero_sum_with: None,
            };
            let r = drag.resize(-40.0);
            assert_eq!(r.width, 540.0, "dragged left by 40, 40 wider");
            assert_eq!(r.other_width, None, "the terminal keeps its width");
            assert_eq!(
                r.pan, 40.0,
                "the strip pans by the growth, so the right edge stays put"
            );
            let r = drag.resize(40.0);
            assert_eq!((r.width, r.pan), (460.0, -40.0), "and narrows the same way");
        }
    }

    #[test]
    fn shift_drag_trades_width_between_the_two_windows_at_the_seam_only() {
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Trailing,
                grabbed: window(1, 500.0),
                zero_sum_with: Some(window(2, 700.0)),
            };
            assert_eq!(drag.mode(), ResizeMode::ZeroSum);
            let r = drag.resize(60.0);
            assert_eq!((r.width, r.other_width), (560.0, Some(640.0)));
            assert_eq!(r.width + r.other_width.unwrap(), 1200.0, "zero-sum");
            assert_eq!(r.pan, 0.0, "both outer edges stay put");
        }
        // Phase `leading_edge_with_shift`: the seam moves the same way, so
        // the window left of it is the one that gains.
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Leading,
                grabbed: window(2, 700.0),
                zero_sum_with: Some(window(1, 500.0)),
            };
            let r = drag.resize(-60.0);
            assert_eq!((r.width, r.other_width), (760.0, Some(440.0)));
            assert_eq!(r.pan, 0.0);
        }
    }

    #[test]
    fn a_limit_stops_the_edge_and_nothing_else_makes_up_for_it() {
        let app = WidthLimits {
            min: 320.0,
            max: f64::INFINITY,
        };
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Leading,
                grabbed: DraggedWindow {
                    group: 1,
                    width: 400.0,
                    limits: app,
                },
                zero_sum_with: None,
            };
            let r = drag.resize(500.0);
            assert_eq!(r.width, 320.0, "the app's real minimum, not the floor");
            assert_eq!(r.pan, -80.0, "the right edge still holds");
        }
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Trailing,
                grabbed: window(1, 500.0),
                zero_sum_with: Some(DraggedWindow {
                    group: 2,
                    width: 400.0,
                    limits: app,
                }),
            };
            let r = drag.resize(300.0);
            assert_eq!(
                (r.width, r.other_width),
                (580.0, Some(320.0)),
                "the neighbour bottoms out, so the seam stops"
            );
            let r = drag.resize(-1000.0);
            assert_eq!((r.width, r.other_width), (160.0, Some(740.0)));
        }
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Trailing,
                grabbed: DraggedWindow {
                    group: 0,
                    width: 500.0,
                    limits: WidthLimits {
                        min: 160.0,
                        max: 640.0,
                    },
                },
                zero_sum_with: None,
            };
            assert_eq!(drag.resize(400.0).width, 640.0);
        }
        {
            let drag = EdgeDrag {
                edge: WindowEdge::Trailing,
                grabbed: window(0, 500.0),
                zero_sum_with: None,
            };
            assert_eq!(drag.resize(100_000.0).width, 100_500.0);
        }
    }
}
