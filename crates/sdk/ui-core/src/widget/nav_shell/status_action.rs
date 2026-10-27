//! The button beside the window's sync status (Retry): where the header row
//! that carries the status has room for it.

use vello::kurbo::Rect;

use super::{Inputs, NavGeometry, Part, measure};
use crate::widget::WidgetLayoutError;
use crate::widget::manual::ManualContext;

/// The button beside the window's sync status: at the trailing end of the
/// row that carries the status, before that row's actions and the window's
/// own controls, at its natural size. Where it would run into the row's
/// title it is not shown.
pub(super) fn place_status_action(
    inputs: &Inputs,
    g: &NavGeometry,
    ctx: &mut ManualContext<'_>,
    placed: &mut [Option<Rect>],
) -> Result<(), WidgetLayoutError> {
    let Some(index) = inputs.parts.iter().position(|p| *p == Part::StatusAction) else {
        return Ok(());
    };
    let column = g.status_column();
    let row = g.header(column);
    let at = |part: Part| {
        inputs
            .parts
            .iter()
            .position(|p| *p == part)
            .and_then(|i| placed.get(i).copied().flatten())
    };
    let in_row = |rect: &Rect| {
        rect.width() > 0.0 && rect.y0 < row.y1 && rect.y1 > row.y0 && rect.x0 >= row.x0
    };
    let count = inputs.actions.get(column.index()).copied().unwrap_or(0);
    let x1 = (0..count)
        .map(|i| Part::Action(column, i))
        .chain([Part::More(column)])
        .filter_map(at)
        .chain([g.history, g.inspector_toggle, g.window_controls])
        .filter(in_row)
        .map(|rect| rect.x0)
        .fold(row.x1, f64::min)
        - inputs.metrics.gap;
    let x0 = [Part::Title(column), Part::Leading(column)]
        .into_iter()
        .filter_map(at)
        .chain([g.toggle, g.back])
        .filter(in_row)
        .map(|rect| rect.x1)
        .fold(row.x0, f64::max)
        + inputs.metrics.gap;
    let (w, h) = measure(ctx, index, None)?;
    let rect = if w > 0.0 && x1 - w >= x0 {
        let h = h.min(row.height()).max(0.0);
        let cy = row.center().y;
        Rect::new(x1 - w, cy - h / 2.0, x1, cy + h / 2.0)
    } else {
        Rect::ZERO
    };
    if let Some(slot) = placed.get_mut(index) {
        *slot = Some(rect);
    }
    Ok(())
}
