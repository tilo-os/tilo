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

/// A crisp 1 px edge on `rect`'s outermost pixel row: strokes
/// [`ui::surface::edge_outline`] (half a pixel *inside* the rect — kurbo's
/// `Rect::inset(+x)` grows a rect, so this is the one place that geometry
/// lives). For a surface whose fill and edge cannot go through
/// [`ui::surface::card`] in one call (the code card paints its gutter
/// between fill and edge; the modal strokes `panel_border`).
pub fn stroke_edge(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    scene.stroke(
        &Stroke::new(1.0),
        t,
        color,
        None,
        &ui::surface::edge_outline(rect, radius),
    );
}

/// Stroke a rounded-rect border (focus rings and the selected row's accent
/// outline; rest edges come from [`ui::surface::card`] / [`stroke_edge`]).
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
#[allow(
    clippy::too_many_arguments,
    reason = "text painting forwards scene resources, string, origin, size, color and weight to the shared text renderer"
)]
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
#[allow(
    clippy::too_many_arguments,
    reason = "monospace painting forwards scene resources, string, origin, size, color and weight to the shared text renderer"
)]
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

/// Fit a monospace line into `max_w` px with a trailing ellipsis. Every
/// glyph of a monospace face shares one advance, so one measurement gives
/// the exact character budget — no per-candidate re-measuring. Proportional
/// text uses the shared [`TextRenderer::elide_to_width_weighted`] instead.
pub fn mono_fit(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let advance = mono_width(tr, "0", size);
    if advance <= 0.0 {
        return s.to_string();
    }
    let budget = (max_w / advance).floor() as usize;
    let len = s.chars().count();
    if len <= budget {
        return s.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    let mut out: String = s
        .chars()
        .take(budget - 1)
        .collect::<String>()
        .trim_end()
        .to_string();
    out.push('…');
    out
}

/// Measured line box of one line at `size` as the renderer paints it (the
/// accessibility scale included) — what stacked lines step by and what a
/// band centres, never the bare font size.
pub fn line_box(tr: &mut TextRenderer, size: f32) -> f64 {
    let (_, h) = tr.measure("0", size, None);
    f64::from(h)
}

/// Measured line box of one monospace line at `size`.
pub fn mono_line_box(tr: &mut TextRenderer, size: f32) -> f64 {
    let (_, h) = tr.measure_styled("0", size, None, TextWeight::Regular, false, true);
    f64::from(h)
}

/// How much one line of `size` has grown over its 1× box at the current
/// accessibility scale — what a 1×-designed row or control adds to keep
/// seating its text. Zero at 1×.
pub fn line_growth(tr: &mut TextRenderer, size: f32) -> f64 {
    let line = line_box(tr, size);
    let scale = f64::from(tr.text_scale()).max(f32::EPSILON.into());
    (line - line / scale).max(0.0)
}

/// Draw a small filled pill (rounded capsule) with a centred label.
/// Returns the pill's outer rect so callers can lay the next pill out.
#[allow(
    clippy::too_many_arguments,
    reason = "pill painting combines scene resources, label, position and height with font size and foreground/background colors"
)]
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
    let (text_w, text_h) = tr.measure(label, size, None);
    let w = f64::from(text_w) + pad_x * 2.0;
    let rect = Rect::new(x, y, x + w, y + height);
    // A capsule with its derived rest edge: a tinted pill on the same
    // ground as its neighbours otherwise reads as bare text on a wash.
    ui::surface::pill(scene, t, rect, bg, Some(ui::surface::edge_on(bg)), None);
    // Centre the measured line box in the capsule, not the bare font size.
    let ty = y + (height - f64::from(text_h)) / 2.0;
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
