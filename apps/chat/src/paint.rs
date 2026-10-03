//! Small Vello drawing helpers shared across the chat render modules.
//!
//! These are thin wrappers over `vello::Scene` + [`TextRenderer`] so the
//! render code in `sidebar` / `messages` / `composer` / `call` reads as
//! layout intent rather than primitive plumbing. Everything pulls its
//! colours from the [`Theme`] the caller passes — nothing here hardcodes
//! a colour, in keeping with the OS-controlled theming contract.

use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

/// Fill a rounded rectangle.
pub fn fill_round(scene: &mut Scene, tf: Affine, rect: Rect, radius: f64, color: Color) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
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
    let r = radius
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5)
        .max(0.0);
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
    if s.is_empty() {
        return (0.0, 0.0);
    }
    let style = TextStyle::new(size);
    let measured = tr.measure_style(s, None, &style);
    tr.draw_style(
        scene,
        s,
        color,
        tf * Affine::translate((x, y)),
        None,
        &style,
    );
    measured
}

/// Push a rectangular clip layer; remember to `scene.pop_layer()`.
pub fn push_clip(scene: &mut Scene, tf: Affine, rect: Rect) {
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, tf, &rect);
}

/// Truncate `s` to fit `max_w` pixels at `size`, appending an ellipsis
/// when it overflows. Thin wrapper over [`TextRenderer::elide_to_width`] so
/// chat chrome shares the grapheme-safe cut with every other app.
pub fn truncate_to(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    tr.elide_to_width(s, size, ui::num::f32_of_f64(max_w))
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
    ring.get(idx % ring.len())
        .copied()
        .unwrap_or(c.text_primary)
}

/// Blend `a` toward `b` by `t` (0 = all `a`, 1 = all `b`), in sRGB.
///
/// Crate-private rather than a shared `mix()`: that helper is already
/// duplicated as a public utility across 7+ crates in this workspace and
/// belongs promoted into `ui`, not copied an 8th time as another public copy
/// here.
pub(crate) fn blend_toward(a: Color, b: Color, t: f64) -> Color {
    use ui::color_compat::ColorCompat;
    let t = t.clamp(0.0, 1.0) as f32;
    let lerp = |x: u8, y: u8| {
        (f32::from(x) + (f32::from(y) - f32::from(x)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::from_rgb8(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Move `ink` toward whichever of black/white it needs, until it clears
/// `target` contrast against `bg` — and return the first step that does.
///
/// A participant's colour is assigned to tell people apart, not to be readable
/// on any particular surface, so painting text in it is a contrast defect
/// waiting to happen: the same blue that reads cleanly on a light card is
/// unreadable on a dark one, and the high-contrast theme's ground is neither.
/// This keeps the colour recognisably theirs while making the text legible on
/// the ground it actually lands on.
///
/// Both directions are tried, not just the one the background suggests: a
/// mid-toned ground can be short of the target in the obvious direction and
/// clear it in the other, and stopping after one direction is how a run ends up
/// at 4.38:1 in exactly one theme. `fallback` is used only when neither
/// direction can reach the target, and only if it does better than the closest
/// step found.
pub fn legible_on(ink: Color, bg: Color, target: f64, fallback: Color) -> Color {
    use ui::a11y::contrast_ratio;
    if contrast_ratio(ink, bg) >= target {
        return ink;
    }
    let bg_is_light = ui::a11y::relative_luminance(bg) >= 0.5;
    let first = if bg_is_light {
        Color::BLACK
    } else {
        Color::WHITE
    };
    let second = if bg_is_light {
        Color::WHITE
    } else {
        Color::BLACK
    };
    let mut best = ink;
    let mut best_ratio = contrast_ratio(ink, bg);
    for toward in [first, second] {
        for step in 1..=24 {
            let candidate = blend_toward(ink, toward, f64::from(step) / 24.0);
            let r = contrast_ratio(candidate, bg);
            if r >= target {
                return candidate;
            }
            if r > best_ratio {
                best_ratio = r;
                best = candidate;
            }
        }
    }
    if contrast_ratio(fallback, bg) > best_ratio {
        fallback
    } else {
        best
    }
}

/// Strengthen a tint until it stands `target` apart from the ground it sits on.
///
/// Unlike [`legible_on`], the first thing tried is more of the colour the tint
/// came from — a band that gets darker by turning grey has stopped saying whose
/// it is, which is the only reason the band exists. Only when saturating the
/// tint still falls short does it move toward black or white.
pub fn band_at_least(tint: Color, own: Color, ground: Color, target: f64) -> Color {
    use ui::a11y::contrast_ratio;
    if contrast_ratio(tint, ground) >= target {
        return tint;
    }
    for step in 1..=16 {
        let candidate = blend_toward(tint, own, f64::from(step) / 16.0);
        if contrast_ratio(candidate, ground) >= target {
            return candidate;
        }
    }
    legible_on(own, ground, target, own)
}
