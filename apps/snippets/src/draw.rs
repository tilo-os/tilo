//! Small drawing helpers shared by the app's panes.
//!
//! These wrap the handful of Vello primitives the snippet manager paints
//! (filled rounded rects, hairline borders, pills) so the pane code stays
//! about layout, not about `kurbo` plumbing. Every colour comes in from a
//! [`ui::theme::Theme`] token at the call site — nothing here invents a
//! colour.

use ui::text_render::{TextRenderer, TextWeight};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// Fill a rounded rectangle.
pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

/// Fill a plain rectangle.
pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

/// Stroke a rounded-rect border.
pub fn stroke_round(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: f64,
    width: f64,
    color: Color,
) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

/// Stroke a straight line between two points (toggle glyphs: hamburger,
/// close-X, back chevron).
pub fn stroke_line(scene: &mut Scene, t: Affine, a: Point, b: Point, width: f64, color: Color) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

/// Draw a single line of text at `(x, y)` (y is the top of the line).
#[allow(clippy::too_many_arguments)]
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        weight,
    );
}

/// Draw monospace text (used by the body preview / highlighter).
#[allow(clippy::too_many_arguments)]
pub fn mono(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
    weight: TextWeight,
) {
    tr.draw_text_styled(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        None,
        weight,
        false,
        true,
    );
}

/// Measure the advance width of a monospace string at `size`.
pub fn mono_width(tr: &mut TextRenderer, s: &str, size: f32) -> f64 {
    f64::from(
        tr.measure_styled(s, size, None, TextWeight::Regular, false, true)
            .0,
    )
}

/// Draw a small filled pill (rounded capsule) with a centred label.
/// Returns the pill's outer rect so callers can lay the next pill out.
#[allow(clippy::too_many_arguments)]
pub fn pill(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    label: &str,
    x: f64,
    y: f64,
    height: f64,
    size: f32,
    fg: Color,
    bg: Color,
) -> Rect {
    let pad_x = 8.0;
    let text_w = f64::from(tr.measure(label, size, None).0);
    let w = text_w + pad_x * 2.0;
    let rect = Rect::new(x, y, x + w, y + height);
    fill_round(scene, t, rect, height / 2.0, bg);
    let ty = y + (height - f64::from(size) * 1.0) / 2.0 - 1.0;
    text(
        scene,
        tr,
        t,
        label,
        x + pad_x,
        ty,
        size,
        fg,
        TextWeight::Medium,
    );
    rect
}
