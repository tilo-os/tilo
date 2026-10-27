//! Small drawing helpers shared across the chat render modules.
//!
//! Thin wrappers over the toolkit's role-based text painting so the render
//! code reads as layout intent. Every style comes from a `TextRole` of the
//! [`Theme`](ui::theme::Theme) the caller passes, and every colour from its tokens.

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::tokens::colors::Colors;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Color, Fill};

/// Draw `s` in `style` with its top-left corner at `at`. Returns the measured
/// `(width, height)` so callers can advance the layout cursor.
pub fn text(
    ctx: &mut PaintCtx<'_>,
    s: &str,
    style: &TextStyle,
    color: Color,
    at: Point,
    max_width: Option<f32>,
) -> (f32, f32) {
    if s.is_empty() {
        return (0.0, 0.0);
    }
    let measured = ctx.text.measure_style(s, max_width, style);
    ctx.text(s, style, color, at, max_width);
    measured
}

/// One line of `s` in `style`, elided to `max_w`, starting at `x` and centred
/// on its measured line box inside `band`. Returns the width painted.
pub fn line_in(
    ctx: &mut PaintCtx<'_>,
    s: &str,
    style: &TextStyle,
    color: Color,
    x: f64,
    band: Rect,
    max_w: f64,
) -> f64 {
    if s.is_empty() || max_w <= 0.0 {
        return 0.0;
    }
    let shown = elide(ctx.text, s, style, max_w);
    let (w, h) = ctx.text.measure_style(&shown, None, style);
    let top = band.center().y - f64::from(h) / 2.0;
    ctx.text(&shown, style, color, Point::new(x, top), None);
    f64::from(w)
}

pub fn width_of(tr: &mut TextRenderer, s: &str, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style(s, None, style).0)
}

pub fn line_of(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style("Ag", None, style).1)
}

/// `s` elided with an ellipsis to fit `max_w` in `style`.
pub fn elide(tr: &mut TextRenderer, s: &str, style: &TextStyle, max_w: f64) -> String {
    tr.elide_to_width_style(s, ui::num::f32_of_f64(max_w.max(0.0)), style)
}

/// Push a rectangular clip layer; remember to `scene.pop_layer()`.
pub fn push_clip(scene: &mut Scene, tf: Affine, rect: Rect) {
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, tf, &rect);
}

/// Strengthen a tint until it stands `target` apart from the ground it sits on.
///
/// The first thing tried is more of the colour the tint came from — a band
/// that gets darker by turning grey has stopped saying whose it is, which is
/// the only reason the band exists. Only when saturating the tint still falls
/// short does it step toward the contrasting extreme.
pub fn band_at_least(
    colors: &Colors,
    tint: Color,
    own: Color,
    ground: Color,
    target: f64,
) -> Color {
    use ui::a11y::contrast_ratio;
    use ui::tokens::colors::mix8;
    if contrast_ratio(tint, ground) >= target {
        return tint;
    }
    for step in 1..=16_u8 {
        let candidate = mix8(tint, own, f32::from(step) / 16.0);
        if contrast_ratio(candidate, ground) >= target {
            return candidate;
        }
    }
    colors.ink_on(ground, own, target)
}
