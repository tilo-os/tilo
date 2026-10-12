//! The notes window's two columns, as the slots [`ui::widget::NavShell`]
//! places.
//!
//! The shell owns everything around the columns: the header row, the sidebar
//! toggle, the regions, the drawer and its scrim. The page tree and the note
//! panel stay with `NotesApp`, which drives them from the same state every
//! frame; a [`ColumnSlot`] is the empty child the shell sizes for each, and the
//! app hangs the real column under it in the layout tree. That way both are
//! laid out by the shell's own placement, in the same pass, at the size the
//! shell gave their column.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

use ui::Widget;
use ui::theme::Theme;
use ui::widget::TaffyTreeExt;

/// Where the shell put the two columns this layout pass.
#[derive(Clone, Default)]
pub(crate) struct Columns {
    pub sidebar: ColumnPlace,
    pub content: ColumnPlace,
}

/// One column's layout node and rect, written by its [`ColumnSlot`].
#[derive(Clone, Default)]
pub(crate) struct ColumnPlace {
    node: Rc<Cell<Option<NodeId>>>,
    rect: Rc<Cell<Rect>>,
}

impl ColumnPlace {
    /// The slot's layout node, while the shell shows the column.
    pub fn node(&self) -> Option<NodeId> {
        self.node.get()
    }

    /// The column's body, in window coordinates, as of the last layout.
    pub fn rect(&self) -> Rect {
        self.rect.get()
    }

    /// Forget the node before the shell builds its tree again.
    pub fn reset(&self) {
        self.node.set(None);
    }

    pub fn slot(&self) -> ColumnSlot {
        ColumnSlot {
            place: self.clone(),
            rect: Rect::ZERO,
        }
    }
}

/// The empty child a shell column holds: it takes the column's size and says
/// where it is, and paints nothing.
pub(crate) struct ColumnSlot {
    place: ColumnPlace,
    rect: Rect,
}

impl Widget for ColumnSlot {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.place.node.set(Some(node));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.place.rect.set(self.rect);
    }

    fn name(&self) -> &'static str {
        "NotesColumn"
    }

    /// It paints nothing, so it never has anything to repaint: the page tree
    /// and the note drawn where it sits report their own damage. (A leaf's
    /// default would claim its whole column on every hover in the shell.)
    fn needs_redraw(&self) -> bool {
        false
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// What the window's header buttons asked for since the app last looked.
#[derive(Default)]
pub(crate) struct HeaderRequests {
    pub new_note: Cell<bool>,
    pub note_menu: Cell<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_reports_its_node_and_rect_and_forgets_the_node_on_reset() {
        let place = ColumnPlace::default();
        assert_eq!(place.node(), None);
        let mut slot = place.slot();
        let mut taffy = TaffyTree::new();
        let mut nodes = HashMap::new();
        let node = slot.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        assert_eq!(place.node(), Some(node));
        assert!(
            nodes.contains_key(&node),
            "the slot is registered for layout"
        );

        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(200.0),
                },
            )
            .unwrap();
        slot.apply_layout(&taffy, node, Point::new(10.0, 20.0));
        assert_eq!(place.rect(), Rect::new(10.0, 20.0, 310.0, 220.0));
        assert_eq!(slot.get_rect(), place.rect());

        place.reset();
        assert_eq!(place.node(), None, "a column the shell hides has no node");
    }

    #[test]
    fn header_requests_are_taken_once() {
        let requests = HeaderRequests::default();
        requests.new_note.set(true);
        assert!(requests.new_note.take());
        assert!(!requests.new_note.take());
        assert!(!requests.note_menu.take());
    }
}
