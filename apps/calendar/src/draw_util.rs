//! Small drawing primitives shared by Calendar's content views.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use ui::paint::PaintCtx;
use ui::surface::edge_on;
use ui::text_render::TextStyle;

pub(crate) const ALL_DAY_WASH: f32 = 0.34;

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

pub(crate) fn disc(ctx: &mut PaintCtx<'_>, center: Point, radius: f64, color: Color) {
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        color,
        None,
        &Circle::new(center, radius),
    );
}

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

pub(crate) fn line_height(ctx: &mut PaintCtx<'_>, style: &TextStyle) -> f64 {
    f64::from(ctx.text.measure_style("Ag", None, style).1)
}

pub(crate) fn wrap_lines(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    style: &TextStyle,
    width: f64,
    max_lines: usize,
) -> Vec<String> {
    let fits = |ctx: &mut PaintCtx<'_>, line: &str| {
        f64::from(ctx.text.measure_style(line, None, style).0) <= width
    };
    let elide = |ctx: &mut PaintCtx<'_>, line: &str| {
        ctx.text
            .elide_to_width_style(line, ui::num::f32_of_f64(width.max(0.0)), style)
    };
    let mut lines = Vec::new();
    let mut words = text.split_whitespace().peekable();
    while words.peek().is_some() {
        if lines.len() + 1 >= max_lines {
            let rest: Vec<&str> = words.collect();
            lines.push(elide(ctx, &rest.join(" ")));
            break;
        }
        let mut line = String::new();
        while let Some(word) = words.peek() {
            let candidate = if line.is_empty() {
                (*word).to_string()
            } else {
                format!("{line} {word}")
            };
            if line.is_empty() || fits(ctx, &candidate) {
                line = candidate;
                words.next();
            } else {
                break;
            }
        }
        let shown = if fits(ctx, &line) {
            line
        } else {
            elide(ctx, &line)
        };
        lines.push(shown);
    }
    lines
}
