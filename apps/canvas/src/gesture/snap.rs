//! Snapping: what is dragged lands on the edges and centres of the other
//! marks and of the artwork, and a guide shows what it lined up with.
#![allow(clippy::arithmetic_side_effects, reason = "kurbo's float geometry")]

use ui::vello::kurbo::{Point, Rect, Vec2};

/// A line something snapped to: `x = at` from `y = from` to `to` when
/// `vertical`, else `y = at` from `x = from` to `to`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub vertical: bool,
    pub at: f64,
    pub from: f64,
    pub to: f64,
}

/// How far to shift what is dragged, and the guides it then lines up with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snap {
    pub offset: Vec2,
    pub guides: Vec<Guide>,
}

fn lines_x(rect: Rect) -> [f64; 3] {
    [rect.x0, rect.center().x, rect.x1]
}

fn lines_y(rect: Rect) -> [f64; 3] {
    [rect.y0, rect.center().y, rect.y1]
}

/// The nearest pairing of one of `moving` with one of `fixed`, within
/// `tolerance`: the shift that lines them up.
fn nearest(moving: &[f64], fixed: &[f64], tolerance: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for m in moving {
        for f in fixed {
            let shift = f - m;
            if shift.abs() <= tolerance && best.is_none_or(|best| shift.abs() < best.abs()) {
                best = Some(shift);
            }
        }
    }
    best
}

/// Guides along the lines `moved` now shares with `targets`.
fn guides(moved: Rect, targets: &[Rect]) -> Vec<Guide> {
    let mut found: Vec<Guide> = Vec::new();
    let mut add = |guide: Guide| {
        if let Some(same) = found
            .iter_mut()
            .find(|have| have.vertical == guide.vertical && (have.at - guide.at).abs() < 1e-6)
        {
            same.from = same.from.min(guide.from);
            same.to = same.to.max(guide.to);
        } else {
            found.push(guide);
        }
    };
    for target in targets {
        for x in lines_x(moved) {
            if lines_x(*target).iter().any(|line| (line - x).abs() < 1e-6) {
                add(Guide {
                    vertical: true,
                    at: x,
                    from: moved.y0.min(target.y0),
                    to: moved.y1.max(target.y1),
                });
            }
        }
        for y in lines_y(moved) {
            if lines_y(*target).iter().any(|line| (line - y).abs() < 1e-6) {
                add(Guide {
                    vertical: false,
                    at: y,
                    from: moved.x0.min(target.x0),
                    to: moved.x1.max(target.x1),
                });
            }
        }
    }
    found
}

/// Snap a box being moved onto `targets`' edges and centres, each axis on
/// its own, within `tolerance` units.
pub fn snap_rect(moving: Rect, targets: &[Rect], tolerance: f64) -> Snap {
    let fixed_x: Vec<f64> = targets.iter().flat_map(|rect| lines_x(*rect)).collect();
    let fixed_y: Vec<f64> = targets.iter().flat_map(|rect| lines_y(*rect)).collect();
    let offset = Vec2::new(
        nearest(&lines_x(moving), &fixed_x, tolerance).unwrap_or(0.0),
        nearest(&lines_y(moving), &fixed_y, tolerance).unwrap_or(0.0),
    );
    Snap {
        offset,
        guides: guides(moving + offset, targets),
    }
}

/// Snap one point (a corner being dragged, a new shape's corner) onto
/// `targets`' edges and centres.
pub fn snap_point(point: Point, targets: &[Rect], tolerance: f64) -> Snap {
    snap_rect(Rect::from_points(point, point), targets, tolerance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_snaps_its_edges_and_centre_to_others() {
        let others = [Rect::new(0.0, 0.0, 100.0, 100.0)];
        let snap = snap_rect(Rect::new(103.0, 48.0, 153.0, 98.0), &others, 5.0);
        assert_eq!(snap.offset, Vec2::new(-3.0, 2.0));
        assert!(
            snap.guides
                .iter()
                .any(|guide| guide.vertical && (guide.at - 100.0).abs() < 1e-9)
        );
        assert!(
            snap.guides
                .iter()
                .any(|guide| !guide.vertical && (guide.at - 100.0).abs() < 1e-9)
        );
        // Too far: no snap.
        let free = snap_rect(Rect::new(120.0, 300.0, 140.0, 320.0), &others, 5.0);
        assert_eq!(free.offset, Vec2::ZERO);
        assert!(free.guides.is_empty());
    }

    #[test]
    fn a_point_snaps_to_a_centre() {
        let snap = snap_point(
            Point::new(52.0, 200.0),
            &[Rect::new(0.0, 0.0, 100.0, 100.0)],
            4.0,
        );
        assert_eq!(snap.offset, Vec2::new(-2.0, 0.0));
        assert_eq!(snap.guides.len(), 1);
    }
}
