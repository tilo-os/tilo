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
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use ui::paint::PaintCtx;
pub use ui::paint::{fill_rect, fill_round, stroke_round};
use ui::text_render::{TextRenderer, TextStyle};

/// Top edge that vertically centres `text` inside `[y0, y0 + h]`, using the
/// height the renderer will actually paint at its current accessibility
/// scale. Centring on the nominal font size drifts by a few pixels at 1× and
/// by half a line at 2×, so every centred label goes through here.
pub fn centered_top(tr: &mut TextRenderer, text: &str, size: f32, y0: f64, h: f64) -> f64 {
    let (_, text_h) = tr.measure(text, size, None);
    y0 + (h - f64::from(text_h)) / 2.0
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

/// Draw `text` with its top-left at `origin`, clipped to `max_w`.
pub fn label(
    ctx: &mut PaintCtx<'_>,
    text: &str,
    size: f32,
    color: Color,
    origin: Point,
    max_w: f64,
) {
    ctx.text(
        text,
        &TextStyle::new(size),
        color,
        origin,
        Some(max_w as f32 + 1.0),
    );
}

/// Draw `text` vertically centred within `rect`, left edge at `rect.x0`,
/// clipped to its width.
pub fn label_v(ctx: &mut PaintCtx<'_>, text: &str, size: f32, color: Color, rect: Rect) {
    let y = centered_top(ctx.text, text, size, rect.y0, rect.height());
    label(ctx, text, size, color, Point::new(rect.x0, y), rect.width());
}

/// Draw `text` right-aligned so its right edge sits at `row.x1`,
/// vertically centred in `row`.
pub fn label_right_v(ctx: &mut PaintCtx<'_>, text: &str, size: f32, color: Color, row: Rect) {
    let (w, text_h) = ctx.text.measure(text, size, None);
    let x = row.x1 - f64::from(w);
    let y = row.y0 + (row.height() - f64::from(text_h)) / 2.0;
    label(ctx, text, size, color, Point::new(x, y), f64::from(w) + 2.0);
}

/// How a [`chip`] is painted: its label size and ink, its fill, and the
/// capsule's horizontal padding, height and corner radius.
pub struct ChipStyle {
    pub size: f32,
    pub ink: Color,
    pub fill: Color,
    pub pad_x: f64,
    pub height: f64,
    pub radius: f64,
}

/// A filled pill with centred label, starting at `left_mid.x` and
/// vertically centred on `left_mid.y`. Returns the pill width used. The fill
/// is a tint or a solid accent, so the pill reads as a contrast step against
/// the surface it sits on rather than needing a declared border.
pub fn chip(ctx: &mut PaintCtx<'_>, text: &str, style: &ChipStyle, left_mid: Point) -> f64 {
    let (x, y_center) = (left_mid.x, left_mid.y);
    let (w, text_h) = ctx.text.measure(text, style.size, None);
    let w = f64::from(w) + style.pad_x * 2.0;
    let rect = Rect::new(
        x,
        y_center - style.height / 2.0,
        x + w,
        y_center + style.height / 2.0,
    );
    ui::surface::pill(
        ctx.scene,
        ctx.transform,
        rect,
        style.fill,
        None,
        Some(style.radius),
    );
    let ty = y_center - f64::from(text_h) / 2.0;
    label(
        ctx,
        text,
        style.size,
        style.ink,
        Point::new(x + style.pad_x, ty),
        w,
    );
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
