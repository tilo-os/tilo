//! Small Vello drawing helpers shared across the chat render modules.
//!
//! These are thin wrappers over `vello::Scene` + [`TextRenderer`] so the
//! render code in `sidebar` / `messages` / `composer` / `call` reads as
//! layout intent rather than primitive plumbing. Everything pulls its
//! colours from the [`Theme`] the caller passes — nothing here hardcodes
//! a colour, in keeping with the OS-controlled theming contract.

use ui::color_compat::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::tokens::Shadow;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

/// Draw a soft themed drop shadow beneath a rounded panel from an elevation
/// [`Shadow`] token: the token's vertical offset plus a two-stack blur
/// approximation. Every floating surface (the call huddle, popovers) uses this
/// so none invents its own oversized, hard-edged slab.
pub fn soft_shadow(scene: &mut Scene, tf: Affine, rect: Rect, radius: f64, elev: &Shadow) {
    let off = elev.offset_y as f64;
    let base = Rect::new(rect.x0, rect.y0 + off, rect.x1, rect.y1 + off);
    for (grow, alpha) in [(elev.blur as f64, 0.5f32), (elev.blur as f64 * 0.5, 0.9f32)] {
        let shape = RoundedRect::from_rect(base.inflate(grow, grow), radius + grow);
        scene.fill(Fill::NonZero, tf, elev.color.with_alpha_factor(alpha), None, &shape);
    }
}

/// Fill a rounded rectangle.
pub fn fill_round(scene: &mut Scene, tf: Affine, rect: Rect, radius: f64, color: Color) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let r = radius.min(rect.width() * 0.5).min(rect.height() * 0.5).max(0.0);
    let shape = RoundedRect::from_rect(rect, r);
    scene.fill(Fill::NonZero, tf, color, None, &shape);
}

/// Stroke a rounded rectangle outline.
pub fn stroke_round(
    scene: &mut Scene,
    tf: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let r = radius.min(rect.width() * 0.5).min(rect.height() * 0.5).max(0.0);
    let shape = RoundedRect::from_rect(rect, r);
    scene.stroke(&Stroke::new(width), tf, color, None, &shape);
}

/// Fill a plain rectangle.
pub fn fill_rect(scene: &mut Scene, tf: Affine, rect: Rect, color: Color) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    scene.fill(Fill::NonZero, tf, color, None, &rect);
}

/// Fill a circle centred at `c` with radius `r`.
pub fn fill_circle(scene: &mut Scene, tf: Affine, c: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, tf, color, None, &Circle::new(c, r));
}

/// Stroke a circle outline.
pub fn stroke_circle(scene: &mut Scene, tf: Affine, c: Point, r: f64, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), tf, color, None, &Circle::new(c, r));
}

/// Draw a horizontal hairline divider across `[x0, x1]` at `y`.
pub fn hline(scene: &mut Scene, tf: Affine, x0: f64, x1: f64, y: f64, color: Color) {
    scene.stroke(
        &Stroke::new(1.0),
        tf,
        color,
        None,
        &Line::new(Point::new(x0, y), Point::new(x1, y)),
    );
}

/// Draw text with its top-left corner at `(x, y)`. Returns the measured
/// `(width, height)` so callers can advance the layout cursor.
#[allow(clippy::too_many_arguments)]
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    tf: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    weight: TextWeight,
    max_width: Option<f32>,
) -> (f32, f32) {
    if s.is_empty() {
        return (0.0, 0.0);
    }
    let measured = tr.measure_weighted(s, size, max_width, weight);
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        tf * Affine::translate((x, y)),
        max_width,
        weight,
    );
    measured
}

/// Convenience: regular-weight single-line text, no wrap.
pub fn label(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    tf: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
) -> (f32, f32) {
    text(scene, tr, tf, s, size, color, x, y, TextWeight::Regular, None)
}

/// Push a rectangular clip layer; remember to `scene.pop_layer()`.
pub fn push_clip(scene: &mut Scene, tf: Affine, rect: Rect) {
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, tf, &rect);
}

/// Truncate `s` to fit `max_w` pixels at `size`, appending an ellipsis
/// when it overflows. Cheap char-walk; good enough for single-line chrome.
pub fn truncate_to(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let (w, _) = tr.measure(s, size, None);
    if (w as f64) <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    let ell = "…";
    for ch in s.chars() {
        let candidate = format!("{out}{ch}{ell}");
        let (cw, _) = tr.measure(&candidate, size, None);
        if (cw as f64) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push_str(ell);
    out
}

/// Deterministic accent colour for a 0-based palette index, sampling the
/// theme accent + a couple of semantic stops so a roster of users / tints
/// reads as visually distinct without the OS shipping per-user theming.
pub fn palette_color(theme: &Theme, idx: usize) -> Color {
    let c = &theme.tokens.colors;
    let ring = [
        c.accent[5],
        c.semantic.success,
        c.accent[6],
        c.semantic.warning,
        c.accent[3],
        c.semantic.info,
        c.accent[7],
        c.semantic.danger,
    ];
    ring[idx % ring.len()]
}
