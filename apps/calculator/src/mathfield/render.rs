//! Rendering for the math field's editing state: the caret bar at the active
//! gap and the selection highlight band, reading colours from the theme. The
//! laid-out boxes themselves are painted by `ui::math::draw_layout`.

use ui::paint::PaintCtx;
use vello::kurbo::{Point, Rect};

use super::edit::Sel;
use super::layout::{Gap, Layout};
use super::model::Slot;

/// Paint the selection band behind the glyphs. `origin` is the top-left of
/// the content area in scene coords; gaps are in content-local coords.
pub fn draw_selection(
    ctx: &mut PaintCtx<'_>,
    origin: Point,
    selection: &Sel,
    row_gaps: &[(usize, &Gap)],
) {
    if row_gaps.is_empty() || selection.is_empty() {
        return;
    }
    let lo = selection.lo();
    let hi = selection.hi();
    let mut x0 = None;
    let mut x1 = None;
    let mut top = f64::INFINITY;
    let mut bot = f64::NEG_INFINITY;
    for (idx, g) in row_gaps {
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
        let band = Rect::new(origin.x + a, origin.y + top, origin.x + b, origin.y + bot);
        ctx.fill_rect(band, ctx.theme.tokens.colors.intent.selection_tint);
    }
}

/// Paint the caret bar at `gap`, relative to the content `origin`.
pub fn draw_caret(ctx: &mut PaintCtx<'_>, origin: Point, gap: &Gap) {
    let x = origin.x + gap.x;
    let bar = Rect::new(x - 1.0, origin.y + gap.top, x + 1.0, origin.y + gap.bottom);
    ctx.fill_rect(bar, ctx.theme.tokens.colors.intent.you);
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
