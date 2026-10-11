//! A sync that replaces widgets leaves no layout node pointing at a freed
//! one: a pointer move before the next layout (its cursor and tooltip
//! lookups) reached a dropped colour picker and crashed Canvas.

use crate::widget::TaffyTreeExt;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::kurbo::{Point, Rect};

use crate::app::App;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::{AppModel, ModelPath, Text, Widget};

/// Shows one child, replaced by a new one on the sync after `swap` is set,
/// as a view switching what it shows does.
struct Swapper {
    child: Box<dyn Widget>,
    swap: Rc<Cell<bool>>,
    replaced: bool,
    replace_on_poll: bool,
    rect: Rect,
}

impl Widget for Swapper {
    crate::testing::inert_widget_methods!(draw);
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        if !self.replace_on_poll && self.swap.replace(false) {
            self.child = Box::new(Text::new("after"));
            self.replaced = true;
        }
    }
    fn take_structure_dirty(&mut self) -> bool {
        // Like ViewCx::switch, the topology poll can reconcile local state
        // and drop the old child before returning its dirty signal.
        if self.replace_on_poll && self.swap.replace(false) {
            self.child = Box::new(Text::new("after"));
            self.replaced = true;
        }
        std::mem::take(&mut self.replaced)
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.child.build_layout(taffy, theme, node_map);
        taffy.widget_node(taffy::Style::default(), &[child], node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = Rect::from_origin_size(origin, (100.0, 40.0));
        if let Ok(children) = taffy.children(node)
            && let Some(child) = children.first()
        {
            self.child.apply_layout(taffy, *child, origin);
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }
    fn name(&self) -> &'static str {
        "Swapper"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn address(widget: &dyn Widget) -> usize {
    std::ptr::from_ref(widget).cast::<()>().addr()
}

fn live(widget: &dyn Widget, out: &mut HashSet<usize>) {
    out.insert(address(widget));
    for child in widget.children() {
        live(child, out);
    }
}

#[test]
fn a_sync_that_replaces_widgets_leaves_no_node_pointing_at_them() {
    let swap = Rc::new(Cell::new(false));
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Swapper {
            child: Box::new(Text::new("before")),
            swap: Rc::clone(&swap),
            replaced: false,
            replace_on_poll: false,
            rect: Rect::ZERO,
        }),
    );
    app.init_layout();
    assert!(app.layout.node_map.len() >= 2, "the child is laid out");

    swap.set(true);
    assert!(app.sync_model(true));
    let mut alive = HashSet::new();
    live(app.root_widget.as_ref(), &mut alive);
    for pointer in app.layout.node_map.values() {
        assert!(
            alive.contains(&pointer.cast::<()>().addr()),
            "a layout node still points at a replaced widget"
        );
    }
    assert!(
        app.layout.structure_dirty,
        "the next frame lays the new widgets out"
    );
}

#[test]
fn a_topology_poll_that_replaces_widgets_retires_nodes_before_pointer_queries() {
    let swap = Rc::new(Cell::new(false));
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Swapper {
            child: Box::new(Text::new("before")),
            swap: Rc::clone(&swap),
            replaced: false,
            replace_on_poll: true,
            rect: Rect::ZERO,
        }),
    );
    app.init_layout();
    assert!(app.layout.node_map.len() >= 2);
    swap.set(true);
    // This is the idle runner's topology boundary, before a queued move
    // looks up its tooltip and before the redraw rebuilds the layout.
    assert!(app.forget_replaced_widgets());
    assert!(app.layout.node_map.is_empty());
    assert!(app.layout.structure_dirty);
    assert!(app.tooltip_target_at(Point::new(10.0, 10.0)).is_none());
    assert!(app.resolve_cursor(Point::new(10.0, 10.0)).is_none());
    app.rebuild_structure();
    assert!(!app.layout.node_map.is_empty());
    let mut alive = HashSet::new();
    live(app.root_widget.as_ref(), &mut alive);
    assert!(
        app.layout
            .node_map
            .values()
            .all(|p| alive.contains(&p.cast::<()>().addr()))
    );
}
