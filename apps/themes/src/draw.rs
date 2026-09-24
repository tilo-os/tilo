//! Thin Vello drawing helpers shared across the app's custom widgets.
//! Keeps the per-widget `draw` code declarative instead of repeating the
//! `Fill::NonZero` / `Affine` boilerplate everywhere.

pub use ui::paint::fill_rect;
use ui::text_render::{FontChoice, TextRenderer, TextWeight};
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
#[allow(
    clippy::too_many_arguments,
    reason = "a text paint helper takes the scene, renderer, transform, text, position, size and colour, each chosen per call"
)]
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

/// Weighted variant of [`text`].
#[allow(
    clippy::too_many_arguments,
    reason = "a text paint helper takes the scene, renderer, transform, text, position, size and colour, each chosen per call"
)]
pub fn text_w(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    weight: TextWeight,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        weight,
    );
}

/// Left-aligned text shaped in an explicit [`FontChoice`] family, so the
/// preview can render its specimen in the theme's chosen typeface. Baseline-
/// ish top sits at (x, y).
#[allow(
    clippy::too_many_arguments,
    reason = "a text paint helper takes the scene, renderer, transform, text, position, size and colour, each chosen per call"
)]
pub fn text_font(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    weight: TextWeight,
    family: &FontChoice,
) {
    tr.draw_text_font(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        weight,
        false,
        family,
    );
}

/// Text vertically centred in `[y0, y1]` at left edge `x`.
#[allow(
    clippy::too_many_arguments,
    reason = "a text paint helper takes the scene, renderer, transform, text, position, size and colour, each chosen per call"
)]
pub fn text_vcenter(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y0: f64,
    y1: f64,
    weight: TextWeight,
) {
    let y = y0 + ((y1 - y0) - f64::from(size)) / 2.0;
    text_w(tr, scene, t, s, size, color, x, y, weight);
}

/// Right-aligned text ending at `x_right`, vertically centred in `[y0,y1]`.
#[allow(
    clippy::too_many_arguments,
    reason = "a text paint helper takes the scene, renderer, transform, text, position, size and colour, each chosen per call"
)]
pub fn text_right(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x_right: f64,
    y0: f64,
    y1: f64,
) {
    let (w, _) = tr.measure(s, size, None);
    let y = y0 + ((y1 - y0) - f64::from(size)) / 2.0;
    text(tr, scene, t, s, size, color, x_right - f64::from(w), y);
}

/// Horizontally centred text in `[x0, x1]`, vertically centred in `[y0,y1]`.
#[allow(
    clippy::too_many_arguments,
    reason = "a text paint helper takes the scene, renderer, transform, text, position, size and colour, each chosen per call"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "t/s/w/x/y are the standard graphics convention: transform, string, measured width, position"
)]
pub fn text_center(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    weight: TextWeight,
) {
    let (w, _) = tr.measure(s, size, None);
    let x = x0 + ((x1 - x0) - f64::from(w)) / 2.0;
    let y = y0 + ((y1 - y0) - f64::from(size)) / 2.0;
    text_w(tr, scene, t, s, size, color, x, y, weight);
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
