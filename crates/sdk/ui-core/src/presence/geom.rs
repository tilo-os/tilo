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
    fn an_offscreen_peer_mists_the_edge_it_overflows_most() {
        assert!(edge_mist_for(vp(), Point::new(50.0, 50.0)).is_none());
        // On the border still counts as inside (no overflow).
        assert!(edge_mist_for(vp(), Point::new(0.0, 0.0)).is_none());
        assert!(edge_mist_for(vp(), Point::new(100.0, 100.0)).is_none());

        // (case, peer, edge, along, distance when asserted)
        let cases = [
            // Mid-height projects to 0.5.
            (
                "off-right",
                Point::new(180.0, 50.0),
                MistEdge::Right,
                0.5,
                Some(80.0),
            ),
            (
                "off-left",
                Point::new(-40.0, 25.0),
                MistEdge::Left,
                0.25,
                Some(40.0),
            ),
            (
                "off-top",
                Point::new(75.0, -60.0),
                MistEdge::Top,
                0.75,
                None,
            ),
            (
                "off-bottom",
                Point::new(10.0, 250.0),
                MistEdge::Bottom,
                0.10,
                None,
            ),
        ];
        for (case, peer, edge, along, distance) in cases {
            let m = edge_mist_for(vp(), peer).expect(case);
            assert_eq!(m.edge, edge, "case {case}");
            assert!(
                (m.along - along).abs() < 1e-9,
                "case {case}: along {}",
                m.along
            );
            if let Some(distance) = distance {
                assert!(
                    (m.distance - distance).abs() < 1e-9,
                    "case {case}: distance {}",
                    m.distance
                );
            }
        }

        // The dominant axis wins at a corner. Far to the right, only a little
        // above the top: horizontal overflow (200) dominates vertical (10), so
        // it reads as Right.
        let m = edge_mist_for(vp(), Point::new(300.0, -10.0)).expect("corner");
        assert_eq!(m.edge, MistEdge::Right);
        // Flip it: far above, only a little to the right → Top.
        let m2 = edge_mist_for(vp(), Point::new(110.0, -300.0)).expect("corner2");
        assert_eq!(m2.edge, MistEdge::Top);
    }
}
