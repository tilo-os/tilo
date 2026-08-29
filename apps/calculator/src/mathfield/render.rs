//! Rendering for the math field: paints the laid-out boxes, the structure
//! decorations (fraction bars, radical signs, stretched parens), the caret
//! bar at the active gap, and the selection highlight band — all into the
//! Vello scene, reading colours from the theme.

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

use crate::draw;

use super::edit::Sel;
use super::layout::{Box2d, BoxKind, Gap, Layout};
use super::model::Slot;

/// Paint the field's content. `origin` is the top-left of the content area in
/// scene coords; layout is in content-local coords.
#[allow(clippy::too_many_arguments)]
pub fn draw_field(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    layout: &Layout,
    origin: Point,
    caret: Option<&Gap>,
    selection: Option<&Sel>,
    sel_gaps: &[(usize, &Gap)],
    text_color: Color,
    show_caret: bool,
) {
    let c = &theme.tokens.colors;
    let ox = origin.x;
    let oy = origin.y;

    // selection band first (behind glyphs)
    if let (Some(sel), false) = (selection, sel_gaps.is_empty())
        && !sel.is_empty() {
            let lo = sel.lo();
            let hi = sel.hi();
            let mut x0 = None;
            let mut x1 = None;
            let mut top = f64::INFINITY;
            let mut bot = f64::NEG_INFINITY;
            for (idx, g) in sel_gaps {
                if *idx == lo {
                    x0 = Some(g.x);
                }
                if *idx == hi {
                    x1 = Some(g.x);
                }
                if *idx >= lo && *idx <= hi {
                    top = top.min(g.top);
                    bot = bot.max(g.bottom);
                }
            }
            if let (Some(a), Some(b)) = (x0, x1) {
                let band = Rect::new(ox + a, oy + top, ox + b, oy + bot);
                let col = draw::with_alpha(c.accent[5], 90);
                scene.fill(Fill::NonZero, t, col, None, &band);
            }
        }

    // boxes
    for b in &layout.boxes {
        draw_box(scene, t, tr, theme, b, ox, oy, text_color);
    }

    // caret bar
    if show_caret
        && let Some(g) = caret {
            let x = ox + g.x;
            let bar = Rect::new(x - 1.0, oy + g.top, x + 1.0, oy + g.bottom);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
        }
}

#[allow(clippy::too_many_arguments)]
fn draw_box(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    b: &Box2d,
    ox: f64,
    oy: f64,
    text_color: Color,
) {
    let c = &theme.tokens.colors;
    match &b.kind {
        BoxKind::Glyph { ch, size } => {
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            let pos = Point::new(ox + b.x, oy + b.y);
            draw::text_at(scene, tr, t, pos, s, *size, text_color, TextWeight::Medium);
        }
        BoxKind::Name { text, size } => {
            let pos = Point::new(ox + b.x, oy + b.y);
            draw::text_at(
                scene,
                tr,
                t,
                pos,
                text,
                *size,
                text_color,
                TextWeight::Medium,
            );
        }
        BoxKind::Paren { open, size } => {
            // Draw a parenthesis glyph scaled vertically to the box height by
            // stretching the font glyph via an affine on the baseline point.
            let glyph = if *open { "(" } else { ")" };
            let nat_h = f64::from(*size) * 1.25;
            let scale = (b.h / nat_h).clamp(1.0, 3.0);
            let pos = Point::new(ox + b.x, oy + b.y + b.baseline);
            let xf = t
                * Affine::translate((pos.x, pos.y))
                * Affine::scale_non_uniform(1.0, scale)
                * Affine::translate((0.0, -f64::from(*size) * 1.0));
            tr.draw_text_weighted(
                scene,
                glyph,
                *size,
                text_color,
                xf,
                None,
                TextWeight::Regular,
            );
        }
        BoxKind::FracBar => {
            let y = oy + b.y + b.h * 0.5;
            draw::line(
                scene,
                t,
                Point::new(ox + b.x, y),
                Point::new(ox + b.x + b.w, y),
                text_color,
                b.h.max(1.2),
            );
        }
        BoxKind::SqrtSign { over_w, size } => {
            // A clean radical: a short entry stroke dipping to the nib, one
            // straight rise to the apex, then a level vinculum spanning the
            // radicand. Round joins/caps keep the acute apex from spiking.
            let _ = c;
            let left = ox + b.x;
            let top = oy + b.y;
            let sw = b.w;
            let thick = (f64::from(*size) * 0.085).max(1.4);
            // Vinculum sits just below the box top, its centreline at `vy`.
            let vy = top + thick * 0.5 + 1.0;
            let apex_x = left + sw;
            // Entry comes in at ~half height; the nib bottoms out near the
            // radicand baseline (a touch above the very bottom edge).
            let entry = Point::new(left + sw * 0.04, top + b.h * 0.52);
            let nib = Point::new(left + sw * 0.42, top + b.h * 0.9);
            let apex = Point::new(apex_x, vy);
            let vinc_end = Point::new(apex_x + over_w, vy);
            let mut path = BezPath::new();
            path.move_to(entry);
            path.line_to(nib);
            path.line_to(apex);
            path.line_to(vinc_end);
            let stroke = Stroke::new(thick)
                .with_join(Join::Round)
                .with_caps(Cap::Round);
            scene.stroke(&stroke, t, text_color, None, &path);
        }
    }
}

/// Build the `(index, gap)` list for the caret's current row so the renderer
/// can draw the selection band and locate the caret.
pub fn row_gaps<'a>(layout: &'a Layout, steps: &[(usize, Slot)]) -> Vec<(usize, &'a Gap)> {
    layout
        .gaps
        .iter()
        .filter(|g| g.steps == steps)
        .map(|g| (g.index, g))
        .collect()
}

/// The caret gap for a given path, if present in the layout.
pub fn caret_gap<'a>(
    layout: &'a Layout,
    steps: &[(usize, Slot)],
    offset: usize,
) -> Option<&'a Gap> {
    layout
        .gaps
        .iter()
        .find(|g| g.steps == steps && g.index == offset)
}
