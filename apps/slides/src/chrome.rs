//! Small drawing helpers for the editor chrome (toolbar / rail /
//! inspector). Pure Vello primitives — every colour comes from the
//! caller (the theme), nothing is hard-coded.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

pub fn stroke_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color, w: f64) {
    let shape = RoundedRect::from_rect(rect, radius);
    scene.stroke(&Stroke::new(w), t, color, None, &shape);
}

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

/// Estimated advance width of a label at `font` px. Used for pure
/// (no-TextRenderer) chrome layout so draw + hit-test agree.
pub fn est_width(label: &str, font: f64) -> f64 {
    // Average glyph advance ≈ 0.55 em for the UI font; widen slightly so
    // pills never clip their label.
    label.chars().count() as f64 * font * 0.56
}

/// Draw a single line of text left-aligned at `pos` (top-left).
#[allow(clippy::too_many_arguments)]
pub fn text(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    s: &str,
    pos: Point,
    font: f32,
    color: Color,
    weight: TextWeight,
) {
    tr.draw_text_weighted(
        scene,
        s,
        font,
        color,
        t * Affine::translate((pos.x, pos.y)),
        None,
        weight,
    );
}

/// Draw a line of text centred inside `rect` (both axes).
#[allow(clippy::too_many_arguments)]
pub fn text_centered(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    s: &str,
    rect: Rect,
    font: f32,
    color: Color,
    weight: TextWeight,
) {
    let (text_w, text_h) = tr.measure_weighted(s, font, None, weight);
    let pos_x = rect.x0 + (rect.width() - f64::from(text_w)) / 2.0;
    let pos_y = rect.y0 + (rect.height() - f64::from(text_h)) / 2.0;
    tr.draw_text_weighted(
        scene,
        s,
        font,
        color,
        t * Affine::translate((pos_x, pos_y)),
        None,
        weight,
    );
}

/// Draw a line of text vertically centred, left-aligned at `left_x`.
#[allow(clippy::too_many_arguments)]
pub fn text_vcenter(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    s: &str,
    rect: Rect,
    left_x: f64,
    font: f32,
    color: Color,
    weight: TextWeight,
) {
    let (_, text_h) = tr.measure_weighted(s, font, None, weight);
    let pos_y = rect.y0 + (rect.height() - f64::from(text_h)) / 2.0;
    tr.draw_text_weighted(
        scene,
        s,
        font,
        color,
        t * Affine::translate((left_x, pos_y)),
        None,
        weight,
    );
}
