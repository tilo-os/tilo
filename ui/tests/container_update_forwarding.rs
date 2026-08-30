//! Layout containers forward the per-frame [`Widget::update`] pass to
//! their children, exactly as they forward `sync`.
//!
//! `update` is how a widget that mirrors out-of-document state (the
//! shell's top bar reading the chrome revision, a scrollable ticking
//! momentum) notices a change between frames. A container that keeps the
//! trait's default no-op silently freezes every descendant at its
//! build-time state, which is how the shell's workspace chevrons went
//! stale inside a `Stack` → `Column` chain. These tests pin the
//! forwarding for each container so that cannot regress quietly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId, ROOT};
use taffy::{NodeId, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Column, Container, Row, Stack, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

/// Counts `update` calls and reports a change on every one, so a
/// container that forwards must also surface the child's `true`.
struct Probe {
    updates: Rc<Cell<u32>>,
}

impl Widget for Probe {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        self.updates.set(self.updates.get() + 1);
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn get_rect(&self) -> Rect {
        Rect::ZERO
    }
}

fn probe() -> (Box<dyn Widget>, Rc<Cell<u32>>) {
    let updates = Rc::new(Cell::new(0));
    let w: Box<dyn Widget> = Box::new(Probe {
        updates: updates.clone(),
    });
    (w, updates)
}

fn assert_forwards(label: &str, mut container: Box<dyn Widget>, updates: &Rc<Cell<u32>>) {
    let mut doc = AutoCommit::new();
    let changed = container.update(&mut doc, &ROOT);
    assert_eq!(
        updates.get(),
        1,
        "{label} must forward `update` to its child exactly once per pass"
    );
    assert!(
        changed,
        "{label} must report a change when a child's `update` does"
    );
}

#[test]
fn column_forwards_update_to_children() {
    let (child, updates) = probe();
    assert_forwards("Column", Box::new(Column::new().push(child)), &updates);
}

#[test]
fn row_forwards_update_to_children() {
    let (child, updates) = probe();
    assert_forwards("Row", Box::new(Row::new().push(child)), &updates);
}

#[test]
fn stack_forwards_update_to_children() {
    let (child, updates) = probe();
    assert_forwards("Stack", Box::new(Stack::new().push(child)), &updates);
}

#[test]
fn container_forwards_update_to_child() {
    let (child, updates) = probe();
    assert_forwards(
        "Container",
        Box::new(Container::new().with_child(child)),
        &updates,
    );
}

/// Forwarding has to reach through nesting — the shell's top bar is two
/// containers deep — and a quiet child must not be reported as a change.
#[test]
fn update_reaches_through_nested_containers() {
    let (child, updates) = probe();
    let mut root: Box<dyn Widget> = Box::new(Stack::new().push(Box::new(
        Column::new().push(Box::new(Row::new().push(child))),
    )));
    let mut doc = AutoCommit::new();
    assert!(root.update(&mut doc, &ROOT));
    assert_eq!(updates.get(), 1);

    let mut quiet: Box<dyn Widget> = Box::new(Column::new());
    assert!(
        !quiet.update(&mut doc, &ROOT),
        "an empty column has nothing to report"
    );
}
