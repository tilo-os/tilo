//! Small drawing helpers: rounded fills/strokes and a set of crisp vector
//! icons drawn straight into the Vello scene (so we never depend on a glyph
//! existing in the system font). Every colour is supplied by the caller, which
//! always sources it from the OS [`Theme`](ui::theme::Theme) or a reader
//! palette — nothing here hard-codes a colour.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

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

/// A horizontal three-dot "more/overflow" glyph.
pub fn dots(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    let (x, y, s) = box_of(rect, 0.3);
    let cy = y + s / 2.0;
    let r = (s * 0.09).max(1.2);
    for i in 0..3 {
        let cx = x + s * (0.12 + 0.38 * f64::from(i));
        circle(scene, t, Point::new(cx, cy), r, color);
    }
}

/// A downward chevron (dropdown affordance).
pub fn chevron_down(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.3);
    let cy = y + s * 0.38;
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.18, cy),
            Point::new(x + s * 0.5, cy + s * 0.32),
            Point::new(x + s * 0.82, cy),
        ],
        color,
        w,
    );
}

pub fn plus(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.3);
    line(
        scene,
        t,
        Point::new(x, y + s / 2.0),
        Point::new(x + s, y + s / 2.0),
        color,
        w,
    );
    line(
        scene,
        t,
        Point::new(x + s / 2.0, y),
        Point::new(x + s / 2.0, y + s),
        color,
        w,
    );
}

pub fn minus(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.3);
    line(
        scene,
        t,
        Point::new(x, y + s / 2.0),
        Point::new(x + s, y + s / 2.0),
        color,
        w,
    );
}

pub fn close(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.32);
    line(
        scene,
        t,
        Point::new(x, y),
        Point::new(x + s, y + s),
        color,
        w,
    );
    line(
        scene,
        t,
        Point::new(x + s, y),
        Point::new(x, y + s),
        color,
        w,
    );
}

/// A hamburger "table of contents" glyph.
pub fn list(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.26);
    for i in 0..3 {
        let yy = y + s * (0.15 + 0.35 * f64::from(i));
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
    p.line_to(Point::new(f64::midpoint(left, right), notch));
    p.line_to(Point::new(left, bottom));
    p.close_path();
    if filled {
        scene.fill(Fill::NonZero, t, color, None, &p);
    } else {
        scene.stroke(&rstroke(w), t, color, None, &p);
    }
}

/// An open-book glyph — two pages meeting at a central spine. The onboarding
/// emblem and a natural mark for anything book-shaped.
pub fn book(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.1);
    let cx = x + s / 2.0;
    let top = y + s * 0.16;
    let bot = y + s * 0.9;
    // Spine.
    line(
        scene,
        t,
        Point::new(cx, top + s * 0.04),
        Point::new(cx, bot),
        color,
        w,
    );
    // Left and right pages, each a shallow open leaf meeting the spine.
    poly(
        scene,
        t,
        &[
            Point::new(cx, top + s * 0.04),
            Point::new(x + s * 0.06, top),
            Point::new(x + s * 0.06, bot - s * 0.06),
            Point::new(cx, bot),
        ],
        color,
        w,
    );
    poly(
        scene,
        t,
        &[
            Point::new(cx, top + s * 0.04),
            Point::new(x + s * 0.94, top),
            Point::new(x + s * 0.94, bot - s * 0.06),
            Point::new(cx, bot),
        ],
        color,
        w,
    );
}

/// A two-figure "people / shared" glyph — the mark for a book shared with the
/// workspace's peers.
pub fn people(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.12);
    // Back figure (smaller, offset).
    circle(
        scene,
        t,
        Point::new(x + s * 0.66, y + s * 0.34),
        s * 0.15,
        color,
    );
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.5, y + s * 0.92),
            Point::new(x + s * 0.54, y + s * 0.62),
            Point::new(x + s * 0.82, y + s * 0.62),
            Point::new(x + s * 0.86, y + s * 0.92),
        ],
        color,
        w,
    );
    // Front figure.
    circle(
        scene,
        t,
        Point::new(x + s * 0.34, y + s * 0.3),
        s * 0.17,
        color,
    );
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.12, y + s * 0.92),
            Point::new(x + s * 0.17, y + s * 0.58),
            Point::new(x + s * 0.51, y + s * 0.58),
            Point::new(x + s * 0.56, y + s * 0.92),
        ],
        color,
        w,
    );
}

/// A trash-can glyph — the "delete this" affordance. Lid + bin body with two
/// vertical ribs, drawn as strokes so it stays crisp at any size.
pub fn trash(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.26);
    // Lid (top rule) with a small handle notch above it.
    let lid_y = y + s * 0.20;
    line(
        scene,
        t,
        Point::new(x + s * 0.08, lid_y),
        Point::new(x + s * 0.92, lid_y),
        color,
        w,
    );
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.36, lid_y),
            Point::new(x + s * 0.40, y + s * 0.06),
            Point::new(x + s * 0.60, y + s * 0.06),
            Point::new(x + s * 0.64, lid_y),
        ],
        color,
        w,
    );
    // Bin body: two slightly-tapered sides + a base.
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.18, lid_y),
            Point::new(x + s * 0.26, y + s * 0.94),
            Point::new(x + s * 0.74, y + s * 0.94),
            Point::new(x + s * 0.82, lid_y),
        ],
        color,
        w,
    );
    // Ribs.
    for fx in [0.40_f64, 0.60] {
        line(
            scene,
            t,
            Point::new(x + s * fx, lid_y + s * 0.10),
            Point::new(x + s * fx, y + s * 0.84),
            color,
            w * 0.85,
        );
    }
}

/// A small pencil glyph — the "this text is editable" affordance.
pub fn pencil(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.28);
    // Shaft from lower-left to upper-right.
    line(
        scene,
        t,
        Point::new(x + s * 0.12, y + s * 0.88),
        Point::new(x + s * 0.74, y + s * 0.26),
        color,
        w,
    );
    // Nib.
    poly(
        scene,
        t,
        &[
            Point::new(x + s * 0.12, y + s * 0.88),
            Point::new(x + s * 0.06, y + s * 0.94),
            Point::new(x + s * 0.28, y + s * 0.82),
        ],
        color,
        w,
    );
    // Eraser end.
    line(
        scene,
        t,
        Point::new(x + s * 0.74, y + s * 0.26),
        Point::new(x + s * 0.9, y + s * 0.42),
        color,
        w,
    );
}

/// A small star/sparkle used as a decorative cover mark.
pub fn spark(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, w: f64) {
    line(
        scene,
        t,
        Point::new(center.x - r, center.y),
        Point::new(center.x + r, center.y),
        color,
        w,
    );
    line(
        scene,
        t,
        Point::new(center.x, center.y - r),
        Point::new(center.x, center.y + r),
        color,
        w,
    );
    let d = r * 0.62;
    line(
        scene,
        t,
        Point::new(center.x - d, center.y - d),
        Point::new(center.x + d, center.y + d),
        color,
        w,
    );
    line(
        scene,
        t,
        Point::new(center.x - d, center.y + d),
        Point::new(center.x + d, center.y - d),
        color,
        w,
    );
}

/// A horizontal progress bar with rounded ends.
pub fn progress(scene: &mut Scene, t: Affine, rect: Rect, frac: f32, track: Color, fill: Color) {
    let r = rect.height() / 2.0;
    fill_round(scene, t, rect, r, track);
    let f = f64::from(frac.clamp(0.0, 1.0));
    if f > 0.001 {
        let w = (rect.width() * f).max(rect.height());
        let fr = Rect::new(rect.x0, rect.y0, rect.x0 + w, rect.y1);
        fill_round(scene, t, fr, r, fill);
    }
}
