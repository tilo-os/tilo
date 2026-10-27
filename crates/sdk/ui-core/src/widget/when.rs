//! Content that is sometimes there: [`When`] and [`optional`].
//!
//! A `Column` or `Row` spaces its children with one gap between each pair.
//! A child that is absent must take that gap with it, or a missing caption
//! leaves a doubled hole. An absent child is not laid out at all
//! (`Display::None`), so the containers skip it the way they skip a
//! zero-size spacer, and nothing of it is drawn, focused or read out.

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect};

use super::{AppModel, Element, ModelPath, TaffyTreeExt, Widget};
use crate::theme::Theme;

/// A placeholder that takes no room: no layout box, no gap, no paint.
#[derive(Default)]
pub struct Absent {
    rect: Rect,
}

impl Absent {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// The style of a node that is removed from its container's flow.
pub(crate) fn removed_from_flow() -> Style {
    Style {
        display: taffy::Display::None,
        ..Style::default()
    }
}

impl Widget for Absent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(removed_from_flow(), node_map, self)
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (0.0, 0.0));
    }

    fn needs_redraw(&self) -> bool {
        false
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "Absent"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// `child` when there is one, otherwise an [`Absent`] that takes no room
/// and no gap: a caption, a badge or a help line that only some items
/// have.
#[must_use]
pub fn optional(child: Option<Element>) -> Element {
    child.unwrap_or_else(|| Box::new(Absent::new()))
}

/// A child shown only while a condition holds. While it does not, the
/// child is out of its container's flow: no box, no gap around it, no
/// paint, no focus and no accessibility node. The condition is read on
/// every synchronization, so state that changes it (a `LocalState`, the
/// document) shows or hides the child on the next frame; the child keeps
/// its own state while hidden.
pub struct When {
    child: Element,
    absent: Absent,
    visible: Box<dyn FnMut() -> bool>,
    shown: bool,
    structure_dirty: bool,
}

impl When {
    /// `child` while `visible()` returns true.
    pub fn new(child: Element, mut visible: impl FnMut() -> bool + 'static) -> Self {
        let shown = visible();
        Self {
            child,
            absent: Absent::new(),
            visible: Box::new(visible),
            shown,
            structure_dirty: false,
        }
    }

    /// `child` while `visible(&state)` holds.
    pub fn from_state<T: 'static>(
        state: &crate::LocalState<T>,
        mut visible: impl FnMut(&T) -> bool + 'static,
        child: Element,
    ) -> Self {
        let state = state.clone();
        Self::new(child, move || state.with(&mut visible))
    }

    /// Whether the child is shown now.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The wrapped child, shown or not.
    #[must_use]
    pub fn child(&self) -> &dyn Widget {
        self.child.as_ref()
    }

    /// The wrapped child, shown or not.
    pub fn child_mut(&mut self) -> &mut dyn Widget {
        self.child.as_mut()
    }

    fn observe(&mut self) {
        let shown = (self.visible)();
        if shown != self.shown {
            self.shown = shown;
            // Showing or hiding changes which nodes exist (and the focus
            // order), so the host rebuilds the tree rather than re-solving it.
            self.structure_dirty = true;
        }
    }
}

impl super::Decorator for When {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        if self.shown {
            self.child.as_ref()
        } else {
            &self.absent
        }
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        if self.shown {
            self.child.as_mut()
        } else {
            &mut self.absent
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.observe();
        if self.shown {
            self.child.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let was = self.shown;
        self.observe();
        let changed = was != self.shown;
        if self.shown {
            self.child.update(state, path) || changed
        } else {
            changed
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        if self.shown {
            self.child.take_structure_dirty() || own
        } else {
            own
        }
    }

    fn name(&self) -> &str {
        "When"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::{Column, Row, Spacer, Text};

    fn solve(root: &mut dyn Widget, width: f32) {
        let theme = Theme::dark();
        let mut tree = TaffyTree::new();
        let mut map = HashMap::new();
        let node = root.build_layout(&mut tree, &theme, &mut map);
        crate::text_render::with_layout_renderer(1.0, |text| {
            tree.compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::MaxContent,
                },
                |known, available, node, _, _| {
                    map.get(&node).map_or(taffy::Size::ZERO, |&widget| {
                        // SAFETY: the map points into `root`, alive and not
                        // otherwise borrowed for the duration of this solve.
                        unsafe { (*widget).measure(known, available, text) }
                    })
                },
            )
            .expect("layout solves");
        });
        root.apply_layout(&tree, node, Point::ZERO);
    }

    fn line(id: &str) -> Element {
        Box::new(Spacer::fixed(10.0).with_id(id))
    }

    fn rect_of(root: &dyn Widget, id: &str) -> Rect {
        crate::widget::find_test_id(root, id)
            .expect("child present")
            .get_rect()
    }

    #[test]
    fn an_absent_child_takes_no_gap_in_a_column_or_a_row() {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(line("a"))
            .push(optional(None))
            .push(Box::new(When::new(line("hidden"), || false)))
            .push(Box::new(Spacer::fixed(0.0)))
            .push(line("b"));
        solve(&mut column, 200.0);
        let (a, b) = (rect_of(&column, "a"), rect_of(&column, "b"));
        assert!(
            (b.y0 - a.y1 - 8.0).abs() < 0.01,
            "one gap between a and b, got {}",
            b.y0 - a.y1
        );

        let mut row = Row::new()
            .with_gap(8.0)
            .no_wrap()
            .push(line("a"))
            .push(optional(None))
            .push(Box::new(When::new(line("hidden"), || false)))
            .push(line("b"));
        solve(&mut row, 200.0);
        let (a, b) = (rect_of(&row, "a"), rect_of(&row, "b"));
        assert!((b.x0 - a.x1 - 8.0).abs() < 0.01, "got {}", b.x0 - a.x1);
    }

    #[test]
    fn modal_dialogs_and_popovers_take_no_room_in_the_flow() {
        let mut column = Column::new()
            .with_no_padding()
            .with_gap(8.0)
            .push(line("a"))
            .push(Box::new(crate::widget::Popover::new()))
            .push(Box::new(crate::widget::ModalDialog::new(
                "Rename",
                Text::new("Body"),
            )))
            .push(line("b"));
        solve(&mut column, 300.0);
        let (a, b) = (rect_of(&column, "a"), rect_of(&column, "b"));
        assert!((b.y0 - a.y1 - 8.0).abs() < 0.01, "got {}", b.y0 - a.y1);
    }

    #[test]
    fn bound_text_with_nothing_to_say_takes_no_gap() {
        let caption = crate::LocalState::new(String::new());
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0)
            .push(line("a"))
            .push(Box::new(Text::from_state(&caption, Clone::clone)))
            .push(line("b"));
        solve(&mut column, 200.0);
        // The collapse lands through the layout hook the host runs.
        let mut tree = TaffyTree::new();
        let mut map = HashMap::new();
        let theme = Theme::dark();
        let node = column.build_layout(&mut tree, &theme, &mut map);
        let caption_node = tree.children(node).expect("children")[1];
        assert_eq!(
            tree.style(caption_node).expect("style").display,
            taffy::Display::None
        );
        let (a, b) = (rect_of(&column, "a"), rect_of(&column, "b"));
        assert!((b.y0 - a.y1 - 8.0).abs() < 0.01, "got {}", b.y0 - a.y1);
        // A static empty text stays an (empty) line in the flow.
        let mut tree = TaffyTree::new();
        let mut text = Text::new("");
        let node = text.build_layout(&mut tree, &theme, &mut map);
        assert_ne!(
            tree.style(node).expect("style").display,
            taffy::Display::None
        );
    }

    #[test]
    fn when_follows_its_state_and_asks_for_a_rebuild_on_change() {
        let state = crate::LocalState::new(false);
        let mut when = When::from_state(&state, |shown| *shown, Box::new(Text::new("Caption")));
        assert!(!when.is_shown());
        assert!(
            Widget::children(&when).is_empty(),
            "hidden: nothing to visit"
        );
        state.set(true);
        let doc = automerge::AutoCommit::new();
        Widget::sync(&mut when, &doc, &automerge::ROOT);
        assert!(when.is_shown());
        assert!(Widget::take_structure_dirty(&mut when));
        assert!(
            !Widget::take_structure_dirty(&mut when),
            "reading clears it"
        );
    }
}
