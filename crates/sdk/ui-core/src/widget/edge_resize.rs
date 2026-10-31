//! Resizing windows from their edges: which edge an edge drag changes, and
//! how.
//!
//! The strip is infinitely wide, so a resize is never zero-sum unless the
//! person asks for it. Every pointer path goes through the same few rules:
//! [`crate::widget::GroupRow`]'s hover, press and drag for a window on a
//! strip, and [`crate::widget::FloatingWindow`]'s for a free one.
//!
//! - [`seam_owner`] — **who owns an edge on a strip.** Every window owns both
//!   of its edges. The seam between two windows is split down the middle, the
//!   half nearest each window being its edge, except that the window you are
//!   working in (the focused one) owns the whole seam. Hovering shows that
//!   owner's edge before anything is pressed.
//! - [`RectEdge::at`] — **which sides a point on a free rect grabs**: a side,
//!   the bottom, or a corner, never the top where the title bar drags.
//! - [`EdgeDrag::resize`] and [`RectEdge::resize`] — **what a drag does**, both
//!   on `dragged_span`: the grabbed side follows the pointer 1:1 and its
//!   *opposite* side holds still, so everything beyond the dragged edge is
//!   pushed away or pulled along at its own size. On a strip, Shift+drag is
//!   zero-sum between the two windows sharing that seam and nothing else. A
//!   limit (the app's real minimum, a client's maximum) stops the edge;
//!   nothing else is resized to make up for it.
//!
//! This is a design rule, not a tuning: `docs/architecture/window-management.md`
//! ("Resizing a window") is its contract, and the tests below pin each case.

use vello::kurbo::{Point, Rect};

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

/// **The edge rule on one axis**: move the grabbed side `travel` px and hold
/// the opposite side still, so everything beyond the grabbed edge is pushed
/// away or pulled along at its own size; a limit stops the edge, and nothing
/// else changes to make up for it.
///
/// `start` and `end` are the axis's two sides as the caller lays them out
/// (`x0` and `x1`, `y0` and `y1`); `moving` names the side the pointer holds.
/// Returns the new `start` and `end`.
#[must_use]
fn dragged_span(
    start: f64,
    end: f64,
    moving: WindowEdge,
    travel: f64,
    min: f64,
    max: f64,
) -> (f64, f64) {
    let wanted = (end - start)
        + match moving {
            WindowEdge::Trailing => travel,
            WindowEdge::Leading => -travel,
        };
    let span = wanted.clamp(min, max.max(min));
    match moving {
        WindowEdge::Trailing => (start, start + span),
        WindowEdge::Leading => (end - span, end),
    }
}

/// The edge of a free rect (a floating window, a picture in a document) that
/// a drag grabbed: which side of each axis moves. Absent on an axis the edge
/// leaves alone, so a side edge moves one axis and a corner both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectEdge {
    pub x: Option<WindowEdge>,
    pub y: Option<WindowEdge>,
}

impl RectEdge {
    /// The sides a pointer `grip` px from `rect`'s edges grabs, or `None` off
    /// every edge. `top_band` is the height at the top of the rect that drags
    /// the rect instead of resizing it (a title bar), so no side edge is
    /// offered there; the bottom edge is everywhere along the width.
    #[must_use]
    pub fn at(rect: Rect, p: Point, grip: f64, top_band: f64) -> Option<Self> {
        let in_title = p.y <= rect.y0 + top_band;
        let near_start_x = !in_title && (p.x - rect.x0).abs() <= grip;
        let near_end_x = (p.x - rect.x1).abs() <= grip;
        let near_end_y = (p.y - rect.y1).abs() <= grip;
        let edge = Self {
            x: if near_end_x {
                Some(WindowEdge::Trailing)
            } else if near_start_x {
                Some(WindowEdge::Leading)
            } else {
                None
            },
            y: near_end_y.then_some(WindowEdge::Trailing),
        };
        (edge.x.is_some() || edge.y.is_some()).then_some(edge)
    }

    /// `rect` after this edge was carried `by`: each grabbed side follows the
    /// pointer and its opposite side holds still, down to `minimum` on each
    /// axis.
    #[must_use]
    pub fn resize(self, rect: Rect, by: vello::kurbo::Vec2, minimum: vello::kurbo::Size) -> Rect {
        let mut out = rect;
        if let Some(moving) = self.x {
            (out.x0, out.x1) =
                dragged_span(rect.x0, rect.x1, moving, by.x, minimum.width, f64::INFINITY);
        }
        if let Some(moving) = self.y {
            (out.y0, out.y1) = dragged_span(
                rect.y0,
                rect.y1,
                moving,
                by.y,
                minimum.height,
                f64::INFINITY,
            );
        }
        out
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
            let limits = grabbed.limits;
            // The strip lays a window out from its start, so a leading-edge
            // drag is the same rule measured from the other side: the width
            // moves the other way, and the pan is how far that edge left the
            // place the strip gave it.
            let signed = match self.edge {
                WindowEdge::Trailing => travel,
                WindowEdge::Leading => -travel,
            };
            let (_, width) = dragged_span(
                0.0,
                grabbed.width,
                WindowEdge::Trailing,
                signed,
                limits.min,
                limits.max,
            );
            let pan = if self.edge == WindowEdge::Leading {
                width - grabbed.width
            } else {
                0.0
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

    #[test]
    fn one_axis_moves_its_grabbed_side_and_holds_the_other() {
        let free = f64::INFINITY;
        {
            // The grabbed side follows the pointer 1:1 and the opposite side
            // stays exactly where it was.
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Trailing, 40.0, 280.0, free),
                (100.0, 640.0)
            );
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Leading, -40.0, 280.0, free),
                (60.0, 600.0),
                "a leading edge dragged left grows the span leftwards"
            );
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Leading, 40.0, 280.0, free),
                (140.0, 600.0)
            );
        }
        {
            // The minimum stops the grabbed side; the opposite one never moves
            // to make up for it.
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Trailing, -1000.0, 280.0, free),
                (100.0, 380.0)
            );
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Leading, 1000.0, 280.0, free),
                (320.0, 600.0)
            );
            // A maximum stops the span just the same: the far side stays put
            // and only the grabbed side stops short.
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Trailing, 100.0, 280.0, 700.0),
                (100.0, 700.0),
                "the span reaches its maximum and no further"
            );
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Trailing, 500.0, 280.0, 700.0),
                (100.0, 800.0)
            );
            assert_eq!(
                dragged_span(100.0, 600.0, WindowEdge::Leading, -500.0, 280.0, 700.0),
                (-100.0, 600.0)
            );
        }
    }

    #[test]
    fn a_free_rects_grabbable_edges_are_its_sides_its_bottom_and_its_corners() {
        let rect = Rect::new(100.0, 100.0, 500.0, 400.0);
        let grip = 8.0;
        let bar = 40.0;
        let at = |x: f64, y: f64| RectEdge::at(rect, Point::new(x, y), grip, bar);
        assert_eq!(
            at(494.0, 300.0),
            Some(RectEdge {
                x: Some(WindowEdge::Trailing),
                y: None
            })
        );
        assert_eq!(
            at(106.0, 300.0),
            Some(RectEdge {
                x: Some(WindowEdge::Leading),
                y: None
            })
        );
        assert_eq!(
            at(300.0, 396.0),
            Some(RectEdge {
                x: None,
                y: Some(WindowEdge::Trailing)
            })
        );
        assert_eq!(
            at(494.0, 396.0),
            Some(RectEdge {
                x: Some(WindowEdge::Trailing),
                y: Some(WindowEdge::Trailing)
            }),
            "a corner moves both axes"
        );
        assert_eq!(
            at(106.0, 396.0),
            Some(RectEdge {
                x: Some(WindowEdge::Leading),
                y: Some(WindowEdge::Trailing)
            })
        );
        assert_eq!(at(300.0, 300.0), None, "the middle grabs nothing");
        assert_eq!(
            at(106.0, 110.0),
            None,
            "the title bar owns the top: no side edge there"
        );
    }

    #[test]
    fn a_free_rects_edge_drag_keeps_far_sides_still_and_stops_at_the_minimum() {
        let rect = Rect::new(100.0, 100.0, 500.0, 400.0);
        let minimum = vello::kurbo::Size::new(280.0, 200.0);
        let right = RectEdge {
            x: Some(WindowEdge::Trailing),
            y: None,
        };
        assert_eq!(
            right.resize(rect, vello::kurbo::Vec2::new(40.0, -70.0), minimum),
            Rect::new(100.0, 100.0, 540.0, 400.0),
            "the leading edge and the whole height hold still"
        );
        assert_eq!(
            right.resize(rect, vello::kurbo::Vec2::new(-1000.0, 0.0), minimum),
            Rect::new(100.0, 100.0, 380.0, 400.0)
        );
        let left = RectEdge {
            x: Some(WindowEdge::Leading),
            y: None,
        };
        assert_eq!(
            left.resize(rect, vello::kurbo::Vec2::new(-60.0, 0.0), minimum),
            Rect::new(40.0, 100.0, 500.0, 400.0)
        );
        assert_eq!(
            left.resize(rect, vello::kurbo::Vec2::new(1000.0, 0.0), minimum),
            Rect::new(220.0, 100.0, 500.0, 400.0)
        );
        let corner = RectEdge {
            x: Some(WindowEdge::Leading),
            y: Some(WindowEdge::Trailing),
        };
        assert_eq!(
            corner.resize(rect, vello::kurbo::Vec2::new(-20.0, 30.0), minimum),
            Rect::new(80.0, 100.0, 500.0, 430.0)
        );
        assert_eq!(
            corner.resize(rect, vello::kurbo::Vec2::new(0.0, -1000.0), minimum),
            Rect::new(100.0, 100.0, 500.0, 300.0)
        );
    }
}
