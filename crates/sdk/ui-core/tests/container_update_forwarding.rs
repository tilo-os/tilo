//! Layout containers forward the per-frame [`Widget::update`] pass to
//! their children, exactly as they forward `sync`.
//!
//! `update` is how a widget that mirrors out-of-document state (the
//! shell's top bar reading the chrome revision, a scrollable ticking
//! momentum) notices a change between frames. A container that does not
//! forward it silently freezes every descendant at its build-time state,
//! which is how the shell's workspace chevrons went stale inside a `Stack`
//! → `Column` chain. These tests pin the forwarding for each container, and
//! for the trait's default, so that cannot regress quietly.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui::widget::TaffyTreeExt;
use tilo_ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ROOT};
use taffy::{NodeId, Style, TaffyTree};
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Column, Container, Row, Stack, Widget};

/// Counts `update` calls and reports a change on every one, so a
/// container that forwards must also surface the child's `true`.
struct Probe {
    updates: Rc<Cell<u32>>,
}

impl Widget for Probe {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.updates.set(self.updates.get().saturating_add(1));
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn name(&self) -> &'static str {
        "Probe"
    }
}

/// A widget that only exposes its child, relying on the trait's defaults.
struct Exposing(Box<dyn Widget>);

impl Widget for Exposing {
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.0.as_mut()]
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
    }

    fn name(&self) -> &'static str {
        "Exposing"
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
    let doc = AutoCommit::new();
    let changed = container.update(&doc, &ROOT);
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

/// Every container forwards `update` to its child exactly once per pass and
/// reports the child's change; forwarding also reaches through nesting.
#[test]
fn containers_forward_update_to_children() {
    type Wrap = fn(Box<dyn Widget>) -> Box<dyn Widget>;
    let cases: [(&str, Wrap); 4] = [
        ("Column", |child| Box::new(Column::new().push(child))),
        ("Row", |child| Box::new(Row::new().push(child))),
        ("Stack", |child| Box::new(Stack::new().push(child))),
        ("Container", |child| {
            Box::new(Container::new().with_child(child))
        }),
    ];
    for (label, wrap) in cases {
        let (child, updates) = probe();
        assert_forwards(label, wrap(child), &updates);
    }

    // Case `update_reaches_through_nested_containers`.
    // Forwarding has to reach through nesting — the shell's top bar is two
    // containers deep — and a quiet child must not be reported as a change.
    let (child, updates) = probe();
    let mut root: Box<dyn Widget> = Box::new(Stack::new().push(Box::new(
        Column::new().push(Box::new(Row::new().push(child))),
    )));
    let doc = AutoCommit::new();
    assert!(root.update(&doc, &ROOT));
    assert_eq!(updates.get(), 1);

    // Case `a_widget_that_only_exposes_children_forwards_by_default`.
    // A widget need not implement `update` to pass it on: exposing its
    // children is enough.
    let (child, updates) = probe();
    let mut exposing: Box<dyn Widget> = Box::new(Exposing(child));
    assert!(exposing.update(&doc, &ROOT));
    assert_eq!(updates.get(), 1);

    let mut quiet: Box<dyn Widget> = Box::new(Column::new());
    assert!(
        !quiet.update(&doc, &ROOT),
        "an empty column has nothing to report"
    );
}
