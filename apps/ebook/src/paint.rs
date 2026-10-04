//! The reader's page marks: the rules and margin glyphs drawn on the page
//! itself (a separator, a note or bookmark beside its paragraph). They are
//! content drawing in the page's own palette; every colour is the caller's.
//! The bookmark glyph also stands in the reader's header, which has no
//! bookmark icon in `ui::icons`.

// Every glyph helper below shares the same tiny geometry vocabulary: `t`
// (transform), `w` (stroke width), `x`/`y`/`s` (the glyph box's origin and
// side, from `box_of`) and `p` for a path.
#![allow(
    clippy::many_single_char_names,
    reason = "shared geometry vocabulary (t/w/x/y/s/p) across this file's glyph helpers"
)]

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&rstroke(width), t, color, None, &Line::new(a, b));
}

fn rstroke(w: f64) -> Stroke {
    Stroke::new(w).with_caps(Cap::Round).with_join(Join::Round)
}

/// The square glyph box inside `rect`, inset by `pad_frac` of its side.
fn box_of(rect: Rect, pad_frac: f64) -> (f64, f64, f64) {
    let side = rect.width().min(rect.height());
    let pad = side * pad_frac;
    (
        rect.center().x - side / 2.0 + pad,
        rect.center().y - side / 2.0 + pad,
        side - 2.0 * pad,
    )
}

/// A speech-bubble "note" glyph.
pub fn note(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    let (x, y, s) = box_of(rect, 0.12);
    let body = Rect::new(x, y, x + s, y + s * 0.72);
    scene.stroke(
        &rstroke(w),
        t,
        color,
        None,
        &RoundedRect::from_rect(body, s * 0.16),
    );
    let mut tail = BezPath::new();
    tail.move_to(Point::new(x + s * 0.30, y + s * 0.72));
    tail.line_to(Point::new(x + s * 0.30, y + s));
    tail.line_to(Point::new(x + s * 0.55, y + s * 0.72));
    scene.stroke(&rstroke(w), t, color, None, &tail);
}

/// Bookmark glyph. `filled` toggles solid vs outline (set vs not).
pub fn bookmark(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64, filled: bool) {
    let (x, y, s) = box_of(rect, 0.12);
    let left = x + s * 0.18;
    let right = x + s * 0.82;
    let top = y;
    let bottom = y + s;
    let notch = y + s * 0.70;
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
