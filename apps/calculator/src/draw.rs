//! Small Vello drawing helpers shared across the calculator's panels.
//! Everything reads colours from the caller (which pulls them from the
//! `Theme`), so nothing here hard-codes a palette.

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// Horizontal text alignment for [`text_in`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
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
    let shape = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

/// Draw a single line of text inside `rect`, aligned + vertically centered.
#[allow(clippy::too_many_arguments)]
pub fn text_in(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    rect: Rect,
    text: &str,
    size: f32,
    color: Color,
    align: Align,
    weight: TextWeight,
) {
    if text.is_empty() {
        return;
    }
    let (w, _h) = tr.measure_weighted(text, size, None, weight);
    let line_h = f64::from(size) * 1.25;
    let x = match align {
        Align::Left => rect.x0,
        Align::Center => rect.center().x - f64::from(w) / 2.0,
        Align::Right => rect.x1 - f64::from(w),
    };
    let y = rect.y0 + (rect.height() - line_h) / 2.0;
    tr.draw_text_weighted(
        scene,
        text,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        weight,
    );
}

/// Draw text at an explicit top-left point.
#[allow(clippy::too_many_arguments)]
pub fn text_at(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    pos: Point,
    text: &str,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    if text.is_empty() {
        return;
    }
    tr.draw_text_weighted(
        scene,
        text,
        size,
        color,
        t * Affine::translate((pos.x, pos.y)),
        None,
        weight,
    );
}

/// Mix two colours by `t` in [0, 1] (straight, non-premultiplied bytes).
pub fn mix(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 {
        (f64::from(x) + (f64::from(y) - f64::from(x)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgba8(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

/// Apply an alpha to a colour.
pub fn with_alpha(c: Color, alpha: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), alpha)
}
