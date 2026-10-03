//! Small drawing primitives shared by Calendar's content views.
//!
//! The month grid and the week/day timeline are genuine content drawing:
//! their rules, chips and blocks are painted here from theme values. Every
//! rule is derived from the ground it sits on ([`ui::surface::edge_on`]) and
//! every piece of text is set in a [`TextRole`]'s style.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use ui::paint::PaintCtx;
use ui::surface::edge_on;
use ui::text_render::TextStyle;

/// A 1 px rule between `p0` and `p1`, derived against `surface` — the
/// grid line or column edge the calendar draws on its own ground. Painted as
/// a filled rect snapped to the device pixel that contains the coordinate
/// (`x.floor()..x.floor() + 1`), so an integer or fractional grid position
/// paints exactly one crisp row of pixels rather than a half-alpha smear
/// across two. A rule on a surface's right or bottom edge is therefore
/// passed `x1 - 1.0` / `y1 - 1.0` — the last pixel inside the surface.
pub(crate) fn hairline(scene: &mut Scene, transform: Affine, surface: Color, p0: Point, p1: Point) {
    let color = edge_on(surface);
    let rect = if (p0.x - p1.x).abs() < f64::EPSILON {
        let x = p0.x.floor();
        Rect::new(x, p0.y.min(p1.y), x + 1.0, p0.y.max(p1.y))
    } else {
        let y = p0.y.floor();
        Rect::new(p0.x.min(p1.x), y, p0.x.max(p1.x), y + 1.0)
    };
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

/// A straight stroke of `width` from `start` to `end` (the timeline's
/// current-time line).
pub(crate) fn stroke_line(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    start: Point,
    end: Point,
    width: f64,
) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &Line::new(start, end),
    );
}

/// A filled disc of `radius` at `center`: the today marker, an event's
/// colour dot, the start of the current-time line.
pub(crate) fn disc(ctx: &mut PaintCtx<'_>, center: Point, radius: f64, color: Color) {
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        color,
        None,
        &Circle::new(center, radius),
    );
}

/// One line of `text` in `style`, centred in `rect` on its measured line box.
pub(crate) fn text_centered(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: Color,
    rect: Rect,
) {
    let (w, h) = ctx.text.measure_style(text, None, style);
    let origin = Point::new(
        rect.center().x - f64::from(w) / 2.0,
        rect.center().y - f64::from(h) / 2.0,
    );
    ctx.text(text, style, color, origin, None);
}

/// One line of `text` in `style` starting at `x`, vertically centred in
/// `band`, elided to `max_width`.
pub(crate) fn text_in_band(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    color: Color,
    x: f64,
    band: Rect,
    max_width: f64,
) {
    let shown = ctx
        .text
        .elide_to_width_style(text, ui::num::f32_of_f64(max_width.max(0.0)), style);
    let (_, h) = ctx.text.measure_style(&shown, None, style);
    let top = ui::text_metrics::centered_text_top(band.y0, band.height(), f64::from(h));
    ctx.text(&shown, style, color, Point::new(x, top), None);
}

/// The measured height of one line in `style`.
pub(crate) fn line_height(ctx: &mut PaintCtx<'_>, style: &TextStyle) -> f64 {
    f64::from(ctx.text.measure_style("Ag", None, style).1)
}
