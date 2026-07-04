//! Small drawing helpers: rounded fills/strokes and a set of crisp vector
//! icons drawn straight into the Vello scene (so we never depend on a glyph
//! existing in the system font). Every colour is supplied by the caller, which
//! always sources it from the OS [`Theme`](ui::theme::Theme) or a reader
//! palette — nothing here hard-codes a colour.

use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use vello::Scene;

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

pub fn stroke_round(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&rstroke(width), t, color, None, &Line::new(a, b));
}

pub fn circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

fn rstroke(w: f64) -> Stroke {
    Stroke::new(w).with_caps(Cap::Round).with_join(Join::Round)
}

fn poly(scene: &mut Scene, t: Affine, pts: &[Point], color: Color, w: f64) {
    if pts.len() < 2 {
        return;
    }
    let mut p = BezPath::new();
    p.move_to(pts[0]);
    for q in &pts[1..] {
        p.line_to(*q);
    }
    scene.stroke(&rstroke(w), t, color, None, &p);
}

/// Inset a square icon box from `rect` and return its geometry helpers.
fn box_of(rect: Rect, pad_frac: f64) -> (f64, f64, f64) {
    let side = rect.width().min(rect.height());
    let pad = side * pad_frac;
    (rect.x0 + pad, rect.y0 + pad, side - 2.0 * pad)
}

pub fn chevron_left(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.28);
    let cx = x + s * 0.62;
    poly(
        scene,
        t,
        &[
            Point::new(cx, y),
            Point::new(x + s * 0.18, y + s * 0.5),
            Point::new(cx, y + s),
        ],
        color,
        w,
    );
}

pub fn chevron_right(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.28);
    let cx = x + s * 0.38;
    poly(
        scene,
        t,
        &[
            Point::new(cx, y),
            Point::new(x + s * 0.82, y + s * 0.5),
            Point::new(cx, y + s),
        ],
        color,
        w,
    );
}

pub fn plus(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.3);
    line(scene, t, Point::new(x, y + s / 2.0), Point::new(x + s, y + s / 2.0), color, w);
    line(scene, t, Point::new(x + s / 2.0, y), Point::new(x + s / 2.0, y + s), color, w);
}

pub fn minus(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.3);
    line(scene, t, Point::new(x, y + s / 2.0), Point::new(x + s, y + s / 2.0), color, w);
}

pub fn close(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.32);
    line(scene, t, Point::new(x, y), Point::new(x + s, y + s), color, w);
    line(scene, t, Point::new(x + s, y), Point::new(x, y + s), color, w);
}

/// A hamburger "table of contents" glyph.
pub fn list(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.26);
    for i in 0..3 {
        let yy = y + s * (0.15 + 0.35 * i as f64);
        line(scene, t, Point::new(x, yy), Point::new(x + s, yy), color, w);
    }
}

/// A speech-bubble "note" glyph.
pub fn note(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.24);
    let body = Rect::new(x, y, x + s, y + s * 0.72);
    stroke_round(scene, t, body, s * 0.16, color, w);
    // tail
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.30, y + s * 0.72),
            Point::new(x + s * 0.30, y + s),
            Point::new(x + s * 0.55, y + s * 0.72),
        ],
        color,
        w,
    );
}

/// Bookmark glyph. `filled` toggles solid vs outline (active vs idle).
pub fn bookmark(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64, filled: bool) {
    let (x, y, s) = box_of(rect, 0.3);
    let left = x + s * 0.12;
    let right = x + s * 0.88;
    let top = y;
    let bottom = y + s;
    let notch = y + s * 0.66;
    let mut p = BezPath::new();
    p.move_to(Point::new(left, top));
    p.line_to(Point::new(right, top));
    p.line_to(Point::new(right, bottom));
    p.line_to(Point::new((left + right) / 2.0, notch));
    p.line_to(Point::new(left, bottom));
    p.close_path();
    if filled {
        scene.fill(Fill::NonZero, t, color, None, &p);
    } else {
        scene.stroke(&rstroke(w), t, color, None, &p);
    }
}

/// A small star/sparkle used as a decorative cover mark.
pub fn spark(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, w: f64) {
    line(scene, t, Point::new(center.x - r, center.y), Point::new(center.x + r, center.y), color, w);
    line(scene, t, Point::new(center.x, center.y - r), Point::new(center.x, center.y + r), color, w);
    let d = r * 0.62;
    line(scene, t, Point::new(center.x - d, center.y - d), Point::new(center.x + d, center.y + d), color, w);
    line(scene, t, Point::new(center.x - d, center.y + d), Point::new(center.x + d, center.y - d), color, w);
}

/// A horizontal progress bar with rounded ends.
pub fn progress(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    frac: f32,
    track: Color,
    fill: Color,
) {
    let r = rect.height() / 2.0;
    fill_round(scene, t, rect, r, track);
    let f = frac.clamp(0.0, 1.0) as f64;
    if f > 0.001 {
        let w = (rect.width() * f).max(rect.height());
        let fr = Rect::new(rect.x0, rect.y0, rect.x0 + w, rect.y1);
        fill_round(scene, t, fr, r, fill);
    }
}
