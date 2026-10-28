//! Which pointer shape a laid-out surface shows at a point: one rule for
//! every host (the native `App`, the harness the browser page drives, the
//! installed shell's chrome), so a window edge, a text field or a link
//! reads the same everywhere.
//!
//! Each widget names the shape it wants ([`Widget::cursor_shape`]). Among
//! the widgets that do:
//!
//! 1. one carrying a gesture ([`Widget::wants_pointer_capture`]) keeps its
//!    shape wherever the pointer goes, so a resize shows its cursor for the
//!    whole drag, even off the edge it started on;
//! 2. else a resize shape wins over what lies under it: a window edge is
//!    grabbed before the content beneath it is reached, so the cursor says
//!    what a press there does;
//! 3. else the smallest widget under the pointer, so a nested child beats
//!    its enclosing container.

use vello::kurbo::{Point, Rect};
use winit::window::CursorIcon;

use super::Widget;

/// Whether `icon` says a press drags an edge or a divider.
fn resizes(icon: CursorIcon) -> bool {
    use CursorIcon as C;
    matches!(
        icon,
        C::EwResize
            | C::NsResize
            | C::NeswResize
            | C::NwseResize
            | C::ColResize
            | C::RowResize
            | C::EResize
            | C::WResize
            | C::NResize
            | C::SResize
            | C::NeResize
            | C::NwResize
            | C::SeResize
            | C::SwResize
    )
}

/// The shapes offered at one point, folded to the one shown.
#[derive(Clone, Copy, Debug, Default)]
pub struct CursorPick {
    /// The best offer so far: its rank (higher wins), area, and shape.
    best: Option<(u8, f64, CursorIcon)>,
}

impl CursorPick {
    /// Consider what `widget` asks for at `point`.
    pub fn offer(&mut self, widget: &dyn Widget, point: Point) {
        let rect = widget.get_rect();
        let captured = widget.wants_pointer_capture();
        if !captured && !rect.contains(point) {
            return;
        }
        let Some(icon) = widget.cursor_shape(point) else {
            return;
        };
        self.offer_shape(rect, captured, icon);
    }

    fn offer_shape(&mut self, rect: Rect, captured: bool, icon: CursorIcon) {
        let rank = if captured { 2 } else { u8::from(resizes(icon)) };
        let area = rect.area().max(0.0);
        if self
            .best
            .is_none_or(|(r, a, _)| rank > r || (rank == r && area < a))
        {
            self.best = Some((rank, area, icon));
        }
    }

    /// Consider `root` and everything shown within it. Every child is
    /// visited, not only those under the point: a popover may paint past
    /// its parent's rect, and a gesture keeps its shape off it.
    pub fn offer_tree(&mut self, root: &dyn Widget, point: Point) {
        self.offer(root, point);
        for child in root.children() {
            self.offer_tree(child, point);
        }
    }

    /// The shape shown, if any widget asked for one.
    #[must_use]
    pub fn icon(self) -> Option<CursorIcon> {
        self.best.map(|(_, _, icon)| icon)
    }
}

/// The shape `root`'s shown tree asks for at `point`.
#[must_use]
pub fn in_tree(root: &dyn Widget, point: Point) -> Option<CursorIcon> {
    let mut pick = CursorPick::default();
    pick.offer_tree(root, point);
    pick.icon()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gesture_then_an_edge_then_the_smallest_widget_wins() {
        let window = Rect::new(0.0, 0.0, 400.0, 300.0);
        let field = Rect::new(0.0, 100.0, 200.0, 130.0);
        let mut pick = CursorPick::default();
        pick.offer_shape(window, false, CursorIcon::EwResize);
        pick.offer_shape(field, false, CursorIcon::Text);
        assert_eq!(
            pick.icon(),
            Some(CursorIcon::EwResize),
            "the edge is grabbed before the field under it"
        );

        let mut pick = CursorPick::default();
        pick.offer_shape(window, false, CursorIcon::Pointer);
        pick.offer_shape(field, false, CursorIcon::Text);
        assert_eq!(pick.icon(), Some(CursorIcon::Text), "the nested field");

        let mut pick = CursorPick::default();
        pick.offer_shape(field, false, CursorIcon::Text);
        pick.offer_shape(window, true, CursorIcon::EwResize);
        assert_eq!(
            pick.icon(),
            Some(CursorIcon::EwResize),
            "a resize in flight keeps its cursor over a field"
        );
    }
}
