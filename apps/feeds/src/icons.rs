//! Small vector glyphs drawn straight into the Vello scene.
//!
//! These are intentionally tiny, theme-coloured primitives (the caller
//! always passes a `Theme` token as the colour) used for affordances the
//! `ui` toolkit doesn't ship as standalone widgets: the unread dot, the
//! star, the refresh arrow, folder chevrons, and the add (+) glyph.

use std::f64::consts::PI;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Stroke};
use vello::peniko::{Color, Fill};

/// A filled or hollow five-pointed star centred at `center`.
pub fn star(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    radius: f64,
    filled: bool,
    color: Color,
) {
    let r_inner = radius * 0.42;
    let mut path = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { radius } else { r_inner };
        let angle = -PI / 2.0 + f64::from(i) * PI / 5.0;
        let p = Point::new(center.x + r * angle.cos(), center.y + r * angle.sin());
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    path.close_path();
    if filled {
        scene.fill(Fill::NonZero, transform, color, None, &path);
    } else {
        scene.stroke(&Stroke::new(1.3), transform, color, None, &path);
    }
}

/// A solid dot — the "unread" marker.
pub fn dot(scene: &mut Scene, transform: Affine, center: Point, radius: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &Circle::new(center, radius),
    );
}

/// A hollow ring — the "read" marker (subtle).
pub fn ring(scene: &mut Scene, transform: Affine, center: Point, radius: f64, color: Color) {
    scene.stroke(
        &Stroke::new(1.2),
        transform,
        color,
        None,
        &Circle::new(center, radius),
    );
}

/// A circular refresh arrow centred at `center`.
pub fn refresh(scene: &mut Scene, transform: Affine, center: Point, radius: f64, color: Color) {
    let stroke = Stroke::new(1.5);
    // Open arc: ~300 degrees, leaving a gap at the top for the arrowhead.
    let start = -PI * 0.35;
    let sweep = 1.6 * PI;
    let mut path = BezPath::new();
    let steps = 32;
    for i in 0..=steps {
        let a = start + sweep * (f64::from(i) / f64::from(steps));
        let p = Point::new(center.x + radius * a.cos(), center.y + radius * a.sin());
        if i == 0 {
            path.move_to(p);
        } else {
            path.line_to(p);
        }
    }
    scene.stroke(&stroke, transform, color, None, &path);
    // Arrowhead tangent to the arc's start point.
    let tip = Point::new(
        center.x + radius * start.cos(),
        center.y + radius * start.sin(),
    );
    let mut head = BezPath::new();
    head.move_to(Point::new(tip.x - radius * 0.55, tip.y - radius * 0.25));
    head.line_to(tip);
    head.line_to(Point::new(tip.x - radius * 0.1, tip.y + radius * 0.6));
    scene.stroke(&stroke, transform, color, None, &head);
}

/// A plus (+) glyph filling a box of half-size `half` around `center`.
pub fn plus(scene: &mut Scene, transform: Affine, center: Point, half: f64, color: Color) {
    let stroke = Stroke::new(1.8);
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &Line::new(
            Point::new(center.x - half, center.y),
            Point::new(center.x + half, center.y),
        ),
    );
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &Line::new(
            Point::new(center.x, center.y - half),
            Point::new(center.x, center.y + half),
        ),
    );
}

/// A disclosure chevron. `expanded` points it down, else right.
pub fn chevron(
    scene: &mut Scene,
    transform: Affine,
    center: Point,
    size: f64,
    expanded: bool,
    color: Color,
) {
    let stroke = Stroke::new(1.6);
    let mut path = BezPath::new();
    if expanded {
        path.move_to(Point::new(center.x - size, center.y - size * 0.4));
        path.line_to(Point::new(center.x, center.y + size * 0.5));
        path.line_to(Point::new(center.x + size, center.y - size * 0.4));
    } else {
        path.move_to(Point::new(center.x - size * 0.4, center.y - size));
        path.line_to(Point::new(center.x + size * 0.5, center.y));
        path.line_to(Point::new(center.x - size * 0.4, center.y + size));
    }
    scene.stroke(&stroke, transform, color, None, &path);
}

/// An external-link glyph (a box with an arrow out the corner).
pub fn external_link(scene: &mut Scene, transform: Affine, center: Point, half: f64, color: Color) {
    let stroke = Stroke::new(1.4);
    // Box (open at the top-right).
    let mut boxp = BezPath::new();
    boxp.move_to(Point::new(center.x + half * 0.2, center.y - half));
    boxp.line_to(Point::new(center.x - half, center.y - half));
    boxp.line_to(Point::new(center.x - half, center.y + half));
    boxp.line_to(Point::new(center.x + half, center.y + half));
    boxp.line_to(Point::new(center.x + half, center.y - half * 0.2));
    scene.stroke(&stroke, transform, color, None, &boxp);
    // Arrow out the top-right.
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &Line::new(
            Point::new(center.x, center.y),
            Point::new(center.x + half * 1.3, center.y - half * 1.3),
        ),
    );
    let mut head = BezPath::new();
    head.move_to(Point::new(center.x + half * 0.4, center.y - half * 1.3));
    head.line_to(Point::new(center.x + half * 1.3, center.y - half * 1.3));
    head.line_to(Point::new(center.x + half * 1.3, center.y - half * 0.4));
    scene.stroke(&stroke, transform, color, None, &head);
}

/// An import glyph: a downward arrow landing into an open tray. The mirror of
/// [`external_link`] (export), so the two toolbar buttons read as a pair.
pub fn import_arrow(scene: &mut Scene, transform: Affine, center: Point, half: f64, color: Color) {
    let stroke = Stroke::new(1.4);
    // Down arrow shaft.
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &Line::new(
            Point::new(center.x, center.y - half),
            Point::new(center.x, center.y + half * 0.35),
        ),
    );
    // Arrowhead.
    let mut head = BezPath::new();
    head.move_to(Point::new(center.x - half * 0.55, center.y - half * 0.2));
    head.line_to(Point::new(center.x, center.y + half * 0.35));
    head.line_to(Point::new(center.x + half * 0.55, center.y - half * 0.2));
    scene.stroke(&stroke, transform, color, None, &head);
    // Tray the arrow drops into (open at the top).
    let mut tray = BezPath::new();
    tray.move_to(Point::new(center.x - half, center.y + half * 0.55));
    tray.line_to(Point::new(center.x - half, center.y + half));
    tray.line_to(Point::new(center.x + half, center.y + half));
    tray.line_to(Point::new(center.x + half, center.y + half * 0.55));
    scene.stroke(&stroke, transform, color, None, &tray);
}

/// A three-line "hamburger" glyph used to summon the collapsed sidebar.
pub fn hamburger(scene: &mut Scene, transform: Affine, center: Point, half: f64, color: Color) {
    let stroke = Stroke::new(1.8);
    for i in -1..=1 {
        let y = center.y + f64::from(i) * (half * 0.62);
        scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Line::new(
                Point::new(center.x - half, y),
                Point::new(center.x + half, y),
            ),
        );
    }
}

/// A left-pointing "back" chevron used to return from a pushed detail pane.
pub fn back_arrow(scene: &mut Scene, transform: Affine, center: Point, size: f64, color: Color) {
    let stroke = Stroke::new(1.8);
    let mut path = BezPath::new();
    path.move_to(Point::new(center.x + size * 0.5, center.y - size));
    path.line_to(Point::new(center.x - size * 0.5, center.y));
    path.line_to(Point::new(center.x + size * 0.5, center.y + size));
    scene.stroke(&stroke, transform, color, None, &path);
}

/// A reading-glasses / "Aa" style gear glyph for reading settings: a small
/// cog (ring + short teeth). Kept tiny and theme-coloured like the rest.
pub fn gear(scene: &mut Scene, transform: Affine, center: Point, radius: f64, color: Color) {
    let stroke = Stroke::new(1.5);
    // Outer ring.
    scene.stroke(
        &stroke,
        transform,
        color,
        None,
        &Circle::new(center, radius),
    );
    // Hub.
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &Circle::new(center, radius * 0.34),
    );
    // Teeth.
    let teeth = 8;
    for i in 0..teeth {
        let a = f64::from(i) * (2.0 * PI / f64::from(teeth));
        let (sx, sy) = (a.cos(), a.sin());
        let p0 = Point::new(center.x + sx * radius, center.y + sy * radius);
        let p1 = Point::new(
            center.x + sx * (radius + radius * 0.42),
            center.y + sy * (radius + radius * 0.42),
        );
        scene.stroke(&stroke, transform, color, None, &Line::new(p0, p1));
    }
}

/// A double-check "mark all read" glyph.
pub fn check(scene: &mut Scene, transform: Affine, center: Point, half: f64, color: Color) {
    let stroke = Stroke::new(1.8);
    let mut path = BezPath::new();
    path.move_to(Point::new(center.x - half, center.y));
    path.line_to(Point::new(center.x - half * 0.2, center.y + half * 0.8));
    path.line_to(Point::new(center.x + half, center.y - half * 0.8));
    scene.stroke(&stroke, transform, color, None, &path);
}
