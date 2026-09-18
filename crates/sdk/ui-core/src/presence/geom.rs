//! Pure geometry for off-screen peer wayfinding ("mist").
//!
//! When a collaborator's pointer / viewport is outside the local visible
//! area, we don't lose them — we paint a soft colored gradient band at the
//! edge of the screen in the direction they went, so you can tell *where*
//! a peer is even though their cursor isn't on your canvas. This module is
//! the direction math only (no rendering), kept pure so it unit-tests
//! without a GPU.

use vello::kurbo::{Point, Rect};

/// Which screen edge a mist band hugs — i.e. the direction an off-screen
/// peer is relative to the viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MistEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl MistEdge {
    /// A short human label for the direction, for a11y / debug overlays.
    pub fn label(self) -> &'static str {
        match self {
            MistEdge::Left => "left",
            MistEdge::Right => "right",
            MistEdge::Top => "top",
            MistEdge::Bottom => "bottom",
        }
    }
}

/// A resolved off-screen wayfinding cue: which edge to paint on, where
/// along that edge the peer projects to, and how far off-screen they are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeMist {
    pub edge: MistEdge,
    /// Normalised position along the edge (0..=1). For a Left/Right edge
    /// this is the vertical fraction (0 = top, 1 = bottom); for Top/Bottom
    /// it is the horizontal fraction (0 = left, 1 = right). This is where
    /// the band is brightest, so the glow points *at* the peer.
    pub along: f64,
    /// How far outside the viewport the peer is, in the dominant axis, in
    /// scene pixels. Used to taper the band's intensity — a peer just past
    /// the edge glows brighter than one who scrolled far away.
    pub distance: f64,
}

/// Compute the edge-mist cue for `target` relative to `viewport`.
///
/// Returns `None` when `target` is inside (or exactly on the border of)
/// the viewport — an on-screen peer needs a real cursor, not a mist. When
/// the peer is off-screen the dominant overflow axis picks the edge: a
/// peer mostly to the right gets a Right band even if slightly above the
/// top, so the single strongest cue reads unambiguously.
pub fn edge_mist_for(viewport: Rect, target: Point) -> Option<EdgeMist> {
    // Normalise in case a caller passes an inverted rect.
    let x0 = viewport.x0.min(viewport.x1);
    let x1 = viewport.x0.max(viewport.x1);
    let y0 = viewport.y0.min(viewport.y1);
    let y1 = viewport.y0.max(viewport.y1);

    // Signed overflow on each axis: how far past the near edge the target
    // sits. Zero when the target is within the span on that axis.
    let over_left = x0 - target.x; // > 0 when off the left
    let over_right = target.x - x1; // > 0 when off the right
    let over_top = y0 - target.y; // > 0 when above the top
    let over_bottom = target.y - y1; // > 0 when below the bottom

    let horiz = over_left.max(over_right).max(0.0);
    let vert = over_top.max(over_bottom).max(0.0);

    if horiz <= 0.0 && vert <= 0.0 {
        return None; // inside the viewport
    }

    let w = (x1 - x0).max(1.0);
    let h = (y1 - y0).max(1.0);

    // Dominant axis wins the edge. Ties resolve to horizontal (Left/Right)
    // — a corner peer reads more naturally as "off to the side".
    if horiz >= vert {
        let edge = if over_left >= over_right {
            MistEdge::Left
        } else {
            MistEdge::Right
        };
        let along = ((target.y - y0) / h).clamp(0.0, 1.0);
        Some(EdgeMist {
            edge,
            along,
            distance: horiz,
        })
    } else {
        let edge = if over_top >= over_bottom {
            MistEdge::Top
        } else {
            MistEdge::Bottom
        };
        let along = ((target.x - x0) / w).clamp(0.0, 1.0);
        Some(EdgeMist {
            edge,
            along,
            distance: vert,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 100.0)
    }

    #[test]
    fn inside_viewport_yields_no_mist() {
        assert!(edge_mist_for(vp(), Point::new(50.0, 50.0)).is_none());
        // On the border still counts as inside (no overflow).
        assert!(edge_mist_for(vp(), Point::new(0.0, 0.0)).is_none());
        assert!(edge_mist_for(vp(), Point::new(100.0, 100.0)).is_none());
    }

    #[test]
    fn peer_to_the_right_gets_a_right_band() {
        let m = edge_mist_for(vp(), Point::new(180.0, 50.0)).expect("off-right");
        assert_eq!(m.edge, MistEdge::Right);
        assert!((m.along - 0.5).abs() < 1e-9, "mid-height projects to 0.5");
        assert!((m.distance - 80.0).abs() < 1e-9);
    }

    #[test]
    fn peer_to_the_left_gets_a_left_band() {
        let m = edge_mist_for(vp(), Point::new(-40.0, 25.0)).expect("off-left");
        assert_eq!(m.edge, MistEdge::Left);
        assert!((m.along - 0.25).abs() < 1e-9);
        assert!((m.distance - 40.0).abs() < 1e-9);
    }

    #[test]
    fn peer_above_gets_a_top_band() {
        let m = edge_mist_for(vp(), Point::new(75.0, -60.0)).expect("off-top");
        assert_eq!(m.edge, MistEdge::Top);
        assert!((m.along - 0.75).abs() < 1e-9);
    }

    #[test]
    fn peer_below_gets_a_bottom_band() {
        let m = edge_mist_for(vp(), Point::new(10.0, 250.0)).expect("off-bottom");
        assert_eq!(m.edge, MistEdge::Bottom);
        assert!((m.along - 0.10).abs() < 1e-9);
    }

    #[test]
    fn dominant_axis_wins_at_a_corner() {
        // Far to the right, only a little above the top: horizontal
        // overflow (200) dominates vertical (10), so it reads as Right.
        let m = edge_mist_for(vp(), Point::new(300.0, -10.0)).expect("corner");
        assert_eq!(m.edge, MistEdge::Right);

        // Flip it: far above, only a little to the right → Top.
        let m2 = edge_mist_for(vp(), Point::new(110.0, -300.0)).expect("corner2");
        assert_eq!(m2.edge, MistEdge::Top);
    }
}
