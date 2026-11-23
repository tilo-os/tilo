//! The radial shapes the ring charts share: a point at an angle, an arc appended to a path, and the two
//! annular sector outlines — exact arcs for the shapes that rely on them, and a stepped outline where
//! the flat segments are what the chart has always painted.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded radial geometry uses floating point"
)]

use vello::kurbo::{Arc, BezPath, Point};

const SECTOR_STEPS: u32 = 64;

pub(crate) fn polar(center: Point, radius: f64, angle: f64) -> Point {
    Point::new(
        angle.cos().mul_add(radius, center.x),
        angle.sin().mul_add(radius, center.y),
    )
}

pub(crate) fn append_arc(path: &mut BezPath, center: Point, radius: f64, start: f64, sweep: f64) {
    Arc::new(center, (radius, radius), start, sweep, 0.0)
        .to_cubic_beziers(0.1, |a, b, c| path.curve_to(a, b, c));
}

pub(crate) fn annulus(center: Point, inner: f64, outer: f64, start: f64, end: f64) -> BezPath {
    let mut path = BezPath::new();
    path.move_to(polar(center, outer, start));
    append_arc(&mut path, center, outer, start, end - start);
    path.line_to(polar(center, inner, end));
    append_arc(&mut path, center, inner, end, start - end);
    path.close_path();
    path
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
