//! A sync that replaces widgets leaves no layout node pointing at a freed
//! one: a pointer move before the next layout (its cursor and tooltip
//! lookups) reached a dropped colour picker and crashed Canvas.

use crate::widget::TaffyTreeExt;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::app::App;
use crate::presence::PresenceCtx;
use crate::state::State;
use crate::text_render::TextRenderer;
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
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
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

fn replacing_app() -> (App, Rc<Cell<bool>>) {
    use crate::widget::replaced_tripwire::{ReplaceAt, Replacing};
    let (root, armed) = Replacing::new(ReplaceAt::Sync);
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.init_layout();
    let child = app.focusable_order[0];
    app.set_focused_node(Some(child));
    armed.set(true);
    // A sync outside the model pass (installing History syncs the root
    // directly) replaces the focused control; the map still names it.
    app.root_widget.sync(&app.state.doc, &app.state.root);
    assert!(!armed.get(), "the sync replaced the control");
    (app, armed)
}

/// Capturing where focus is after a replacement reads the live tree, never
/// the layout's pointer to the replaced control.
#[test]
fn focus_captured_after_a_replacement_reads_the_live_tree() {
    let (mut app, _armed) = replacing_app();
    assert!(
        app.capture_focus_identity().is_none(),
        "the replacement was never focused"
    );
    assert!(app.layout.node_map.is_empty(), "the stale map is forgotten");
    assert!(app.layout.structure_dirty);
}

/// The history sweep visits mapped nodes: replaced ones are forgotten
/// before it.
#[test]
fn the_history_sweep_after_a_replacement_visits_no_replaced_control() {
    let (mut app, _armed) = replacing_app();
    let _ = app.service_history();
    let mut alive = HashSet::new();
    live(app.root_widget.as_ref(), &mut alive);
    assert!(
        app.layout
            .node_map
            .values()
            .all(|pointer| alive.contains(&pointer.cast::<()>().addr()))
    );
}

/// An assistive technology's action that arrives after a replacement and
/// before the next layout reaches no replaced control.
#[test]
fn an_accessibility_action_after_a_replacement_reaches_no_replaced_control() {
    use crate::a11y::Action;
    let (root, armed) = crate::widget::replaced_tripwire::Replacing::new(
        crate::widget::replaced_tripwire::ReplaceAt::Sync,
    );
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.init_layout();
    let target = app
        .build_a11y_tree_update(1.0)
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some("Tripwire"))
        .map(|(id, _)| *id)
        .expect("the control is exported");
    armed.set(true);
    app.root_widget.sync(&app.state.doc, &app.state.root);
    assert!(!armed.get(), "the sync replaced the control");
    assert!(app.dispatch_a11y_action(target, &Action::Focus).is_none());
    app.rebuild_structure();
    assert_eq!(app.focusable_order.len(), 1);
}
