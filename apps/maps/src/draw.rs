//! Low-level helpers shared by the map canvas and the floating chrome.
//!
//! Colours come from the caller (resolved from the `Theme`) and text comes in
//! as a resolved [`TextStyle`] (`theme.text(role)`); nothing here hard-codes a
//! palette or a type size. The shape primitives are for genuine content
//! drawing — cartography, routes, markers — never for chrome.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Circle, Join, Line, Point, Stroke};
use vello::peniko::{Color, Fill};

use ui::paint::PaintCtx;
pub use ui::paint::{fill_rect, fill_round};
use ui::text_render::{TextRenderer, TextStyle};

/// Linear blend between two sRGB colours (`t` in `0..=1`, 0 → a, 1 → b).
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

/// Apply an alpha multiplier to a colour.
pub fn alpha(c: Color, a: f32) -> Color {
    c.multiply_alpha(a)
}

/// Parse a hex colour string (`rrggbb`, optional leading `#`) to a `Color`.
/// Used for authentic GTFS transit line colours (`routeColor`).
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

/// Stroke an open polyline with round caps/joins.
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

/// Fill a prebuilt path (non-zero winding).
pub fn fill_path(scene: &mut Scene, t: Affine, path: &BezPath, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, path);
}

/// Stroke a prebuilt path with round caps/joins.
pub fn stroke_path(scene: &mut Scene, t: Affine, path: &BezPath, width: f64, color: Color) {
    let stroke = Stroke::new(width)
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    scene.stroke(&stroke, t, color, None, path);
}

/// Fill a closed polygon ring.
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

/// The measured height of one line in `style`.
pub fn line_h(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style("Ag", None, style).1)
}

/// The width `s` takes in `style`.
pub fn measure_w(tr: &mut TextRenderer, s: &str, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style(s, None, style).0)
}

/// Left-aligned text with its line box's top-left at `origin`.
pub fn text(ctx: &mut PaintCtx<'_>, s: &str, origin: Point, style: &TextStyle, color: Color) {
    ctx.text(s, style, color, origin, None);
}

/// Left-aligned text at `origin`, elided to `max_w`.
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

/// Right-aligned text with its right edge at `x_right` and its top at `y`.
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

/// Text centred horizontally on `top_center.x`, its top at `top_center.y`.
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

/// Top of one line in `style` centred in the band `y..y + h`.
pub fn vmid(tr: &mut TextRenderer, y: f64, h: f64, style: &TextStyle) -> f64 {
    ui::text_metrics::centered_text_top(y, h, line_h(tr, style))
}
