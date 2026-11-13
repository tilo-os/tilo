//! Where a sheet's body ends: how tall it stands when what it holds is taller than the
//! window, and which row it stops on.

use super::{ModalLayout, Rect, Vec2, Widget};

/// Shorten an overflowing sheet's body by `by` from the bottom, moving the footer up with
/// it.
pub(super) fn shorten(layout: &mut ModalLayout, by: f64) {
    let by = by.min(layout.body.height()).max(0.0);
    layout.card.y1 -= by;
    layout.body.y1 -= by;
    for action in &mut layout.actions {
        if action.area() > 0.0 {
            *action = *action - Vec2::new(0.0, by);
        }
    }
}

/// The top of the row the body's lower edge falls in: where a body can end without cutting
/// a row through the middle.
pub(super) fn whole_row_top(content: &dyn Widget, body: Rect) -> Option<f64> {
    let mut level = content;
    loop {
        let cut = level.children().into_iter().find(|row| {
            let rect = row.get_rect();
            rect.y0 < body.y1 - 0.5 && rect.y1 > body.y1 + 0.5
        })?;
        let top = cut.get_rect().y0;
        if top > body.y0 + 0.5 && body.y1 - top < body.height() / 3.0 {
            return Some(top);
        }
        if cut.children().is_empty() {
            return None;
        }
        level = cut;
    }
}
