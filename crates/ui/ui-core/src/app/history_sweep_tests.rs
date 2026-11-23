//! The frame's history sweep visits the widgets that take part in history,
//! found once per admitted layout, not every row of a long list.

use crate::app::framework_history_tests::{app_with_root, layout};
use crate::presence::PresenceCtx;
use crate::state::State;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::undo::{UndoRequest, UndoStatus};
use crate::widget::{Column, Text, Widget};
use automerge::{ROOT, ReadDoc, transaction::Transactable};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// A row that asks for undo when told to and counts the statuses it is shown.
struct Asker {
    text: Text,
    request: Rc<Cell<Option<UndoRequest>>>,
    can_undo: Rc<Cell<Option<bool>>>,
}

impl Widget for Asker {
    crate::provides!(dyn crate::undo::UndoControl);

    fn name(&self) -> &str {
        "Asker"
    }
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.text.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.text.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.text.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> Rect {
        self.text.get_rect()
    }
}

impl crate::undo::UndoControl for Asker {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> UndoStatus) {
        self.can_undo.set(Some(status().can_undo));
    }
    fn take_undo_request(&mut self) -> Option<UndoRequest> {
        self.request.take()
    }
}

fn body(state: &State) -> String {
    state
        .doc
        .get(ROOT, "body")
        .unwrap()
        .unwrap()
        .0
        .into_string()
        .unwrap()
}

#[test]
fn a_long_list_sweeps_only_the_widgets_in_history_and_finds_them_after_a_rebuild() {
    let mut state = State::new();
    state.doc.put(ROOT, "body", "old").unwrap();
    state.commit_action(None, None);
    state.doc.put(ROOT, "body", "live").unwrap();
    state.commit_action(None, None);
    let request = Rc::new(Cell::new(None));
    let can_undo = Rc::new(Cell::new(None));
    let rows = (0..500).fold(Column::new().with_intrinsic_height(), |column, row| {
        column.push(Box::new(Text::new(&format!("Row {row}"))))
    });
    let column = rows.push(Box::new(Asker {
        text: Text::new("Undo"),
        request: request.clone(),
        can_undo: can_undo.clone(),
    }));
    let mut app = app_with_root(state, Box::new(column));
    layout(&mut app);

    app.service_history();
    assert_eq!(can_undo.get(), Some(true), "the asker is shown the status");
    let swept = app.layout.history_nodes.nodes.len();
    assert!(
        swept < 10 && app.layout.node_map.len() > 500,
        "{swept} of {} mapped nodes swept",
        app.layout.node_map.len()
    );

    request.set(Some(UndoRequest::Undo));
    assert!(app.service_history(), "the request is taken and performed");
    assert_eq!(body(&app.state), "old");

    // A rebuild maps the tree again: the sweep finds its widgets again.
    let found_for = app.layout.history_nodes.found_for;
    app.layout.request_rebuild();
    layout(&mut app);
    app.service_history();
    assert_ne!(app.layout.history_nodes.found_for, found_for);
    request.set(Some(UndoRequest::Redo));
    assert!(app.service_history(), "found again after the rebuild");
    assert_eq!(body(&app.state), "live");
}
