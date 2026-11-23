//! The radial shapes the ring charts share: a point at an angle and an annular sector, outlined in
//! flat steps.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded radial geometry uses floating point"
)]

use vello::kurbo::{BezPath, Point};

const SECTOR_STEPS: u32 = 64;

pub(crate) fn polar(center: Point, radius: f64, angle: f64) -> Point {
    Point::new(
        angle.cos().mul_add(radius, center.x),
        angle.sin().mul_add(radius, center.y),
    )
}

pub(crate) fn ring_sector(center: Point, inner: f64, outer: f64, start: f64, end: f64) -> BezPath {
    let mut path = BezPath::new();
    let sweep = end - start;
    path.move_to(polar(center, outer, start));
    for step in 1..=SECTOR_STEPS {
        let angle = start + sweep * f64::from(step) / f64::from(SECTOR_STEPS);
        path.line_to(polar(center, outer, angle));
    }
    if inner <= 0.0 {
        path.line_to(center);
    } else {
        path.line_to(polar(center, inner, end));
        for step in 1..=SECTOR_STEPS {
            let angle = end - sweep * f64::from(step) / f64::from(SECTOR_STEPS);
            path.line_to(polar(center, inner, angle));
        }
    }
    path.close_path();
    path
}
