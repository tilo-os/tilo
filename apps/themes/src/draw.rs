//! Thin Vello drawing helpers shared across the app's custom widgets.
//! Keeps the per-widget `draw` code declarative instead of repeating the
//! `Fill::NonZero` / `Affine` boilerplate everywhere.

use ui::paint::PaintCtx;
pub use ui::paint::fill_rect;
use ui::text_render::{TextRenderer, TextStyle};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, PathEl, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius.max(0.0));
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

pub fn stroke_rrect(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    let shape = RoundedRect::from_rect(rect, radius.max(0.0));
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

pub fn circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

pub fn stroke_circle(
    scene: &mut Scene,
    t: Affine,
    center: Point,
    r: f64,
    color: Color,
    width: f64,
) {
    scene.stroke(&Stroke::new(width), t, color, None, &Circle::new(center, r));
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

/// Left-aligned text whose baseline-ish top sits at (x, y).
pub fn text(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
) {
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), None);
}

/// Text centred in `rect` on both axes. The horizontal fit measures the
/// regular-weight advance and the vertical one the bare font size, whatever
/// weight `style` paints in.
pub fn text_center(ctx: &mut PaintCtx<'_>, s: &str, style: &TextStyle, color: Color, rect: Rect) {
    let (w, _) = ctx.text.measure(s, style.size, None);
    let x = rect.x0 + (rect.width() - f64::from(w)) / 2.0;
    let y = rect.y0 + (rect.height() - f64::from(style.size)) / 2.0;
    ctx.text(s, style, color, Point::new(x, y), None);
}

/// Filled smooth polyline area (for the preview sparkline). `pts` are
/// absolute scene-space points; the area is closed down to `baseline_y`.
pub fn area(scene: &mut Scene, t: Affine, pts: &[Point], baseline_y: f64, color: Color) {
    if pts.len() < 2 {
        return;
    }
    // `first`/`last` are only `None` for an empty slice, already ruled out
    // by the length check above.
    let (Some(first), Some(last)) = (pts.first(), pts.last()) else {
        return;
    };
    let mut path = BezPath::new();
    path.push(PathEl::MoveTo(Point::new(first.x, baseline_y)));
    for p in pts {
        path.push(PathEl::LineTo(*p));
    }
    path.push(PathEl::LineTo(Point::new(last.x, baseline_y)));
    path.push(PathEl::ClosePath);
    scene.fill(Fill::NonZero, t, color, None, &path);
}

/// Stroked polyline (the sparkline's top edge).
pub fn polyline(scene: &mut Scene, t: Affine, pts: &[Point], color: Color, width: f64) {
    if pts.len() < 2 {
        return;
    }
    // `split_first` is only `None` for an empty slice, already ruled out by
    // the length check above.
    let Some((first, rest)) = pts.split_first() else {
        return;
    };
    let mut path = BezPath::new();
    path.push(PathEl::MoveTo(*first));
    for p in rest {
        path.push(PathEl::LineTo(*p));
    }
    scene.stroke(&Stroke::new(width), t, color, None, &path);
}
