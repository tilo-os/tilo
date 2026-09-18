//! Small Vello drawing helpers shared across the firewall views.
//!
//! These are thin wrappers over `Scene::fill` / `Scene::stroke` and the
//! `TextRenderer` so the view code reads declaratively (`card`, `label`,
//! `chip`) instead of repeating affine + shape boilerplate. Every colour is
//! passed in by the caller — the helpers never reach for a literal — so the
//! whole app stays theme-driven. Edged surfaces (`card`, `separator`,
//! `chip`) go through `ui::surface`, so the edge is derived from the fill
//! it sits on (HIG §9.5) rather than declared.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::text_render::TextRenderer;

/// Top edge that vertically centres `text` inside `[y0, y0 + h]`, using the
/// height the renderer will actually paint at its current accessibility
/// scale. Centring on the nominal font size drifts by a few pixels at 1× and
/// by half a line at 2×, so every centred label goes through here.
pub fn centered_top(tr: &mut TextRenderer, text: &str, size: f32, y0: f64, h: f64) -> f64 {
    let (_, text_h) = tr.measure(text, size, None);
    y0 + (h - f64::from(text_h)) / 2.0
}

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

/// An emphasis stroke (selection, danger, focus) on top of a surface that
/// already carries its derived rest edge from [`card`].
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

/// A filled card / tile / row with its rest edge derived from `fill`
/// (`ui::surface::card`): the one way this app paints an edged surface.
pub fn card(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, fill: Color) {
    ui::surface::card(scene, t, rect, radius, fill);
}

/// A 1 px rule between rows inside `surface`, inset `inset` from both ends
/// and derived against that surface (`ui::surface::separator`).
pub fn separator(
    scene: &mut Scene,
    t: Affine,
    x0: f64,
    x1: f64,
    y: f64,
    inset: f64,
    surface: Color,
) {
    ui::surface::separator(scene, t, x0, x1, y, inset, surface);
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
#[allow(
    clippy::too_many_arguments,
    reason = "a text-draw primitive called from dozens of sites across app.rs; bundling \
              position/size/color into a struct would only add construction boilerplate \
              at every call site for no real readability gain"
)]
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
#[allow(
    clippy::too_many_arguments,
    reason = "a text-draw primitive called from dozens of sites across app.rs; bundling \
              position/size/color into a struct would only add construction boilerplate \
              at every call site for no real readability gain"
)]
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
    let y = centered_top(tr, text, size, y0, h);
    label(tr, scene, t, text, size, color, x, y, max_w);
}

/// Draw `text` right-aligned so its right edge sits at `x_right`,
/// vertically centred in `[y0, y0+h]`.
#[allow(
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "a text-draw primitive called from dozens of sites across app.rs; bundling \
              position/size/color into a struct would only add construction boilerplate \
              at every call site for no real readability gain, and the short names (t, \
              h, w, x, y) mirror the affine/geometry vocabulary used throughout paint.rs"
)]
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
    let (w, text_h) = tr.measure(text, size, None);
    let x = x_right - f64::from(w);
    let y = y0 + (h - f64::from(text_h)) / 2.0;
    label(tr, scene, t, text, size, color, x, y, f64::from(w) + 2.0);
}

/// A filled pill with centred label. Returns the pill width used. The fill
/// is a tint or a solid accent, so the pill reads as a contrast step against
/// the surface it sits on rather than needing a declared border.
#[allow(
    clippy::too_many_arguments,
    reason = "a pill-draw primitive called from dozens of sites across app.rs; bundling \
              position/size/color into a struct would only add construction boilerplate \
              at every call site for no real readability gain"
)]
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
    let (w, text_h) = tr.measure(text, size, None);
    let w = f64::from(w) + pad_x * 2.0;
    let rect = Rect::new(x, y_center - height / 2.0, x + w, y_center + height / 2.0);
    ui::surface::pill(scene, t, rect, bg, None, Some(radius));
    let ty = y_center - f64::from(text_h) / 2.0;
    label(tr, scene, t, text, size, text_color, x + pad_x, ty, w);
    w
}

/// Measure the on-screen width of `text` at `size`.
pub fn text_width(tr: &mut TextRenderer, text: &str, size: f32) -> f64 {
    f64::from(tr.measure(text, size, None).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The centred top must follow the renderer's measured line, not the
    /// nominal size: at 2× the text is twice as tall, so its top moves up by
    /// half the extra height instead of staying where a 1× box would put it.
    #[test]
    fn centered_top_tracks_the_renderer_text_scale() {
        let mut tr = TextRenderer::new();
        let (_, h1) = tr.measure("Set as active", 12.0, None);
        let top1 = centered_top(&mut tr, "Set as active", 12.0, 100.0, 40.0);
        assert!((top1 - (100.0 + (40.0 - f64::from(h1)) / 2.0)).abs() < 1e-6);
        assert!(
            f64::from(h1) > 12.0,
            "measured line height exceeds the font size"
        );

        tr.set_text_scale(2.0);
        let (_, h2) = tr.measure("Set as active", 12.0, None);
        let top2 = centered_top(&mut tr, "Set as active", 12.0, 100.0, 40.0);
        assert!(h2 > h1 * 1.5, "2× scale must enlarge the measured line");
        assert!(top2 < top1, "taller text must start higher to stay centred");
        assert!((top2 - (100.0 + (40.0 - f64::from(h2)) / 2.0)).abs() < 1e-6);
    }
}
