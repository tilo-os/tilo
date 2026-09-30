//! Small drawing helpers for the editor chrome (toolbar / rail /
//! inspector). Pure Vello primitives — every colour comes from the
//! caller (the theme), nothing is hard-coded.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use ui::paint::PaintCtx;
pub use ui::paint::{fill_rect, fill_round, stroke_round};
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;

/// A crisp 1 px horizontal rule covering the pixel row that starts at `y`.
/// A 1 px *stroke* centred on an integer coordinate straddles two rows and
/// paints as a 2 px blur; a filled rect never does.
pub fn hairline_h(scene: &mut Scene, t: Affine, x0: f64, x1: f64, y: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &Rect::new(x0, y, x1, y + 1.0),
    );
}

/// A crisp 1 px vertical rule covering the pixel column that starts at `x`.
pub fn hairline_v(scene: &mut Scene, t: Affine, x: f64, y0: f64, y1: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &Rect::new(x, y0, x + 1.0, y1),
    );
}

/// A floating panel (HIG §9.1 Panel role: menus, popovers, dialogs): its
/// elevation, the panel ground and the panel border, all derived from the
/// theme — never a raised fill under a declared-alpha ink stroke with a
/// literal black shadow.
pub fn panel(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, radius: f64) {
    ui::chrome::panel_shadow(scene, t, rect, radius, theme, 1.0);
    fill_round(scene, t, rect, radius, ui::chrome::panel_ground(theme));
    scene.stroke(
        &Stroke::new(1.0),
        t,
        ui::chrome::panel_border(theme),
        None,
        &ui::surface::edge_outline(rect, radius),
    );
}

/// Estimated advance width of a label at `font` px. Used for pure
/// (no-TextRenderer) chrome layout so draw + hit-test agree.
pub fn est_width(label: &str, font: f64) -> f64 {
    // Average glyph advance ≈ 0.55 em for the UI font; widen slightly so
    // pills never clip their label.
    label.chars().count() as f64 * font * 0.56
}

/// Draw a single line of text left-aligned at `pos` (top-left).
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
pub fn text_centered(ctx: &mut PaintCtx<'_>, s: &str, rect: Rect, style: &TextStyle, color: Color) {
    let (text_w, text_h) = ctx.text.measure_style(s, None, style);
    let pos_x = rect.x0 + (rect.width() - f64::from(text_w)) / 2.0;
    let pos_y = rect.y0 + (rect.height() - f64::from(text_h)) / 2.0;
    ctx.text(s, style, color, Point::new(pos_x, pos_y), None);
}

/// Draw a line of text vertically centred in `rect`, left-aligned at its `x0`.
pub fn text_vcenter(ctx: &mut PaintCtx<'_>, s: &str, rect: Rect, style: &TextStyle, color: Color) {
    let (_, text_h) = ctx.text.measure_style(s, None, style);
    let pos_y = rect.y0 + (rect.height() - f64::from(text_h)) / 2.0;
    ctx.text(s, style, color, Point::new(rect.x0, pos_y), None);
}
