//! Small Vello drawing helpers shared across the firewall views.
//!
//! These are thin wrappers over `Scene::fill` / `Scene::stroke` and the
//! `TextRenderer` so the view code reads declaratively (`fill_rrect`,
//! `label`, `chip`) instead of repeating affine + shape boilerplate. Every
//! colour is passed in by the caller — the helpers never reach for a
//! literal — so the whole app stays theme-driven.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::text_render::TextRenderer;

/// Approximate line height for a given font size (matches the toolkit's
/// 1.3 ratio closely enough for vertical centring of single lines).
pub fn line_height(size: f32) -> f64 {
    f64::from(size) * 1.3
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

pub fn hline(scene: &mut Scene, t: Affine, x0: f64, x1: f64, y: f64, color: Color, width: f64) {
    scene.stroke(
        &Stroke::new(width),
        t,
        color,
        None,
        &Line::new(Point::new(x0, y), Point::new(x1, y)),
    );
}

pub fn dot(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

/// Draw `text` with its top-left at `(x, y)`, clipped to `max_w`.
pub fn label(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    text: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    max_w: f64,
) {
    tr.draw_text(
        scene,
        text,
        size,
        color,
        t * Affine::translate((x, y)),
        Some(max_w as f32 + 1.0),
    );
}

/// Draw `text` vertically centred within `[y0, y0+h]`, left edge at `x`.
pub fn label_v(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    text: &str,
    size: f32,
    color: Color,
    x: f64,
    y0: f64,
    h: f64,
    max_w: f64,
) {
    let y = y0 + (h - line_height(size)) / 2.0;
    label(tr, scene, t, text, size, color, x, y, max_w);
}

/// Draw `text` right-aligned so its right edge sits at `x_right`,
/// vertically centred in `[y0, y0+h]`.
pub fn label_right_v(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    text: &str,
    size: f32,
    color: Color,
    x_right: f64,
    y0: f64,
    h: f64,
) {
    let (w, _) = tr.measure(text, size, None);
    let x = x_right - f64::from(w);
    let y = y0 + (h - line_height(size)) / 2.0;
    label(tr, scene, t, text, size, color, x, y, f64::from(w) + 2.0);
}

/// A filled pill with centred label. Returns the pill width used.
pub fn chip(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    text: &str,
    size: f32,
    text_color: Color,
    bg: Color,
    x: f64,
    y_center: f64,
    pad_x: f64,
    height: f64,
    radius: f64,
) -> f64 {
    let (w, _) = tr.measure(text, size, None);
    let w = f64::from(w) + pad_x * 2.0;
    let rect = Rect::new(x, y_center - height / 2.0, x + w, y_center + height / 2.0);
    fill_rrect(scene, t, rect, radius, bg);
    let ty = y_center - line_height(size) / 2.0;
    label(tr, scene, t, text, size, text_color, x + pad_x, ty, w);
    w
}

/// Measure the on-screen width of `text` at `size`.
pub fn text_width(tr: &mut TextRenderer, text: &str, size: f32) -> f64 {
    f64::from(tr.measure(text, size, None).0)
}
