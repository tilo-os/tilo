//! Rendering for the math field: paints the laid-out boxes, the structure
//! decorations (fraction bars, radical signs, stretched parens), the caret
//! bar at the active gap, and the selection highlight band — all into the
//! Vello scene, reading colours from the theme.

use ui::text_render::TextRenderer;
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};

use crate::draw;

use super::edit::Sel;
use super::layout::{Gap, Layout};
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
        && !sel.is_empty()
    {
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

    ui::math::draw_layout(scene, t, tr, layout, origin, text_color);

    // caret bar
    if show_caret && let Some(g) = caret {
        let x = ox + g.x;
        let bar = Rect::new(x - 1.0, oy + g.top, x + 1.0, oy + g.bottom);
        scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
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
