//! The math field's caret bar and selection band, coloured from the theme; the
//! laid-out boxes are painted by `ui::math::draw_layout`.

use ui::paint::PaintCtx;
use vello::kurbo::{Point, Rect};

use super::edit::Sel;
use super::layout::{Gap, Layout};
use super::model::{MathExpr, Node, Slot};

/// `origin` is the content area's top-left in scene coords; gaps are
/// content-local.
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

/// Content-local selection rectangles. A logical row can span several visual
/// spans, so adjacent selected atoms are joined only when their gaps share a
/// band. Hidden integral commas get no highlight: it would cover the
/// differential.
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

pub fn draw_caret(ctx: &mut PaintCtx<'_>, origin: Point, gap: &Gap) {
    let x = origin.x + gap.x;
    let bar = Rect::new(x - 1.0, origin.y + gap.top, x + 1.0, origin.y + gap.bottom);
    ctx.fill_rect(bar, ctx.theme.tokens.colors.intent.you);
}

pub fn row_gaps<'a>(layout: &'a Layout, steps: &[(usize, Slot)]) -> Vec<(usize, &'a Gap)> {
    layout
        .gaps
        .iter()
        .filter(|g| g.steps == steps)
        .map(|g| (g.index, g))
        .collect()
}

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
