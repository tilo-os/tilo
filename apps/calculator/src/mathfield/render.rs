//! Rendering for the math field's editing state: the caret bar at the active
//! gap and the selection highlight band, reading colours from the theme. The
//! laid-out boxes themselves are painted by `ui::math::draw_layout`.

use ui::paint::PaintCtx;
use vello::kurbo::{Point, Rect};

use super::edit::Sel;
use super::layout::{Gap, Layout};
use super::model::{MathExpr, Node, Slot};

/// Paint the selection band behind the glyphs. `origin` is the top-left of
/// the content area in scene coords; gaps are in content-local coords.
pub fn draw_selection(
    ctx: &mut PaintCtx<'_>,
    origin: Point,
    selection: &Sel,
    row_gaps: &[(usize, &Gap)],
    expr: &MathExpr,
) {
    for band in selection_bands(selection, row_gaps, expr) {
        ctx.fill_rect(
            band + origin.to_vec2(),
            ctx.theme.tokens.colors.intent.selection_tint,
        );
    }
}

/// Content-local selection rectangles. A logical row can occupy several
/// visual spans (the four arguments of an integral, for example). Only join
/// adjacent selected atoms whose gaps share a band and advance left to right.
/// Hidden integral commas have no painted span, even when their neighbouring
/// arguments share a baseline: highlighting them would cover the differential.
#[must_use]
pub fn selection_bands(selection: &Sel, row_gaps: &[(usize, &Gap)], expr: &MathExpr) -> Vec<Rect> {
    let hidden = hidden_integral_separators(expr, &selection.steps);
    let mut bands: Vec<Rect> = Vec::new();
    let mut last_end = None;
    for pair in row_gaps.windows(2) {
        let [(start, a), (end, b)] = pair else {
            continue;
        };
        if *start < selection.lo()
            || *end > selection.hi()
            || start.checked_add(1) != Some(*end)
            || hidden.contains(start)
            || (a.top - b.top).abs() > f64::EPSILON
            || (a.bottom - b.bottom).abs() > f64::EPSILON
            || a.x >= b.x
        {
            last_end = None;
            continue;
        }
        let band = Rect::new(a.x, a.top, b.x, a.bottom);
        if let Some(previous) = bands.last_mut()
            && last_end == Some(*start)
            && (previous.x1 - band.x0).abs() <= f64::EPSILON
            && (previous.y0 - band.y0).abs() <= f64::EPSILON
            && (previous.y1 - band.y1).abs() <= f64::EPSILON
        {
            previous.x1 = band.x1;
        } else {
            bands.push(band);
        }
        last_end = Some(*end);
    }
    bands
}

fn hidden_integral_separators(expr: &MathExpr, steps: &[(usize, Slot)]) -> Vec<usize> {
    let Some((&(index, Slot::Arg), parent)) = steps.split_last() else {
        return Vec::new();
    };
    let Some(Node::Func { name, arg }) = expr.row_at(parent).and_then(|row| row.get(index)) else {
        return Vec::new();
    };
    if !matches!(name.as_str(), "int" | "∫") {
        return Vec::new();
    }
    let commas: Vec<_> = arg
        .iter()
        .enumerate()
        .filter_map(|(index, node)| matches!(node, Node::Char(',')).then_some(index))
        .collect();
    // Other arities use ordinary function layout, where commas remain visible.
    if commas.len() == 3 {
        commas
    } else {
        Vec::new()
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
