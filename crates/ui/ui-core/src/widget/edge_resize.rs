//! Resizing a free rect from its edges (a floating window, a picture in a document): which edge
//! a drag grabbed, and how it moves.

use vello::kurbo::{Point, Rect};

/// One of a window's two vertical edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEdge {
    Leading,
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

/// The edge of a free rect (a floating window, a picture in a document) that a drag
/// grabbed: which side of each axis moves.
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

    /// `rect` after this edge was carried `by`: each grabbed side follows the pointer and
    /// its opposite side holds still, down to `minimum` on each axis.
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

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// Resizing by an edge.
        /// Each phase was a test of its own and builds its own fixture.
        fn an_edge_drag_moves_the_grabbed_edge_within_its_limits;


        phase one_axis_moves_its_grabbed_side_and_holds_the_other;
        {
            let free = f64::INFINITY;
            {
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

        phase a_free_rects_grabbable_edges_are_its_sides_its_bottom_and_its_corners;
        {
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

        phase a_free_rects_edge_drag_keeps_far_sides_still_and_stops_at_the_minimum;
        {
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
}
