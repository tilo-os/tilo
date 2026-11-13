//! Low-level helpers shared by the map canvas and the floating chrome: colours come from the
//! caller, text as a resolved [`TextStyle`].

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Line, Point, Stroke};
use vello::peniko::{Color, Fill};

use ui::paint::PaintCtx;
pub use ui::paint::{fill_rect, fill_round};
use ui::text_render::{TextRenderer, TextStyle};

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    let l = |x: u8, y: u8| {
        (f32::from(x) * (1.0 - t) + f32::from(y) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgba8(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

pub fn alpha(c: Color, a: f32) -> Color {
    c.multiply_alpha(a)
}

pub fn hex_color(s: &str) -> Option<Color> {
    ui::color::parse_hex(s)
}

pub fn fill_circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

pub fn stroke_circle(
    scene: &mut Scene,
    t: Affine,
    center: Point,
    r: f64,
    width: f64,
    color: Color,
) {
    scene.stroke(&Stroke::new(width), t, color, None, &Circle::new(center, r));
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, width: f64, color: Color) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

pub fn polyline(scene: &mut Scene, t: Affine, pts: &[Point], width: f64, color: Color) {
    if pts.len() < 2 {
        return;
    }
    let mut path = BezPath::new();
    let Some((&first, rest)) = pts.split_first() else {
        return;
    };
    path.move_to(first);
    for p in rest {
        path.line_to(*p);
    }
    let stroke = Stroke::new(width)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    scene.stroke(&stroke, t, color, None, &path);
}

pub fn fill_path(scene: &mut Scene, t: Affine, path: &BezPath, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, path);
}

pub fn stroke_path(scene: &mut Scene, t: Affine, path: &BezPath, width: f64, color: Color) {
    let stroke = Stroke::new(width)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    scene.stroke(&stroke, t, color, None, path);
}

pub fn polygon(scene: &mut Scene, t: Affine, pts: &[Point], color: Color) {
    if pts.len() < 3 {
        return;
    }
    let mut path = BezPath::new();
    let Some((&first, rest)) = pts.split_first() else {
        return;
    };
    path.move_to(first);
    for p in rest {
        path.line_to(*p);
    }
    path.close_path();
    scene.fill(Fill::NonZero, t, color, None, &path);
}

pub fn line_h(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style("Ag", None, style).1)
}

pub fn baseline(tr: &mut TextRenderer, s: &str, style: &TextStyle) -> f64 {
    f64::from(tr.first_baseline_style(s, None, style))
}

pub fn measure_w(tr: &mut TextRenderer, s: &str, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style(s, None, style).0)
}

pub fn text(ctx: &mut PaintCtx<'_>, s: &str, origin: Point, style: &TextStyle, color: Color) {
    ctx.text(s, style, color, origin, None);
}

pub fn text_clip(
    ctx: &mut PaintCtx<'_>,
    s: &str,
    origin: Point,
    style: &TextStyle,
    color: Color,
    max_w: f64,
) {
    let width = ui::num::f32_of_f64(max_w.max(0.0));
    let shown = ctx.text.elide_to_width_style(s, width, style);
    ctx.text(&shown, style, color, origin, None);
}

pub fn text_right(
    ctx: &mut PaintCtx<'_>,
    s: &str,
    x_right: f64,
    y: f64,
    style: &TextStyle,
    color: Color,
) {
    let w = measure_w(ctx.text, s, style);
    ctx.text(s, style, color, Point::new(x_right - w, y), None);
}

pub fn text_center(
    ctx: &mut PaintCtx<'_>,
    s: &str,
    top_center: Point,
    style: &TextStyle,
    color: Color,
) {
    let w = measure_w(ctx.text, s, style);
    ctx.text(
        s,
        style,
        color,
        Point::new(top_center.x - w / 2.0, top_center.y),
        None,
    );
}

pub fn vmid(tr: &mut TextRenderer, y: f64, h: f64, style: &TextStyle) -> f64 {
    ui::text_metrics::centered_text_top(y, h, line_h(tr, style))
}
