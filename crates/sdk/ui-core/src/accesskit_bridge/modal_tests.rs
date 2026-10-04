use super::*;
use crate::headless::FrameHarness;
use crate::widget::{AppModel, Button, Column, Decorator, ModalDialog, ModelPath, Text};
use std::cell::Cell;
use std::rc::Rc;

struct ControlledDialog {
    dialog: ModalDialog<Column>,
    shown: Rc<Cell<bool>>,
}
impl Decorator for ControlledDialog {
    type Inner = ModalDialog<Column>;
    fn inner(&self) -> &Self::Inner {
        &self.dialog
    }
    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.dialog
    }
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.dialog.set_open(self.shown.get());
        self.dialog.sync(state, path);
    }
}
fn dialog(
    title: &str,
    shown: &Rc<Cell<bool>>,
    clicked: &Rc<Cell<usize>>,
    content: Column,
) -> ControlledDialog {
    let clicked = Rc::clone(clicked);
    ControlledDialog {
        dialog: ModalDialog::new(title, content)
            .open(shown.get())
            .primary_action("Connect", move || clicked.set(clicked.get() + 1)),
        shown: Rc::clone(shown),
    }
}
fn connect_buttons(frame: &FrameHarness) -> Vec<AkNodeId> {
    frame
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == AkRole::Button && node.label() == Some("Connect"))
        .map(|(id, _)| id)
        .collect()
}

#[test]
fn an_active_modal_excludes_background_actions_and_restores_them_on_close() {
    let shown = Rc::new(Cell::new(false));
    let background = Rc::new(Cell::new(0));
    let confirmed = Rc::new(Cell::new(0));
    let clicked = Rc::clone(&background);
    let root = Column::new()
        .push(Box::new(
            Button::new("background-connect", "Connect", None)
                .on_view(move || clicked.set(clicked.get() + 1)),
        ))
        .push(Box::new(dialog(
            "Confirm connection",
            &shown,
            &confirmed,
            Column::new().push(Box::new(Text::new("Review connection"))),
        )));
    let mut frame =
        FrameHarness::for_root(crate::State::new(), root, (800, 600), crate::Theme::dark());
    let before = connect_buttons(&frame);
    assert_eq!(before.len(), 1);
    shown.set(true);
    frame.relayout();
    let modal = connect_buttons(&frame);
    assert_eq!(
        modal.len(),
        1,
        "the same-named background action is unavailable"
    );
    assert!(
        frame
            .perform_a11y_action(before[0], &Action::Activate)
            .is_err(),
        "a previously exported background id cannot cross the modal boundary"
    );
    frame
        .perform_a11y_action(modal[0], &Action::Activate)
        .unwrap();
    assert_eq!(confirmed.get(), 1);
    assert_eq!(background.get(), 0);
    shown.set(false);
    frame.relayout();
    let restored = connect_buttons(&frame);
    assert_eq!(restored.len(), 1);
    frame
        .perform_a11y_action(restored[0], &Action::Activate)
        .unwrap();
    assert_eq!(background.get(), 1);
    assert_eq!(confirmed.get(), 1);
}

#[test]
fn the_deepest_modal_owns_accessibility_and_resource_publication_is_unchanged() {
    let outer_shown = Rc::new(Cell::new(true));
    let inner_shown = Rc::new(Cell::new(true));
    let outer_clicked = Rc::new(Cell::new(0));
    let inner_clicked = Rc::new(Cell::new(0));
    let inner = dialog(
        "Inner confirmation",
        &inner_shown,
        &inner_clicked,
        Column::new(),
    );
    let outer = dialog(
        "Outer confirmation",
        &outer_shown,
        &outer_clicked,
        Column::new().push(Box::new(inner)),
    );
    let mut frame =
        FrameHarness::for_root(crate::State::new(), outer, (800, 600), crate::Theme::dark());
    assert!(frame.a11y_node_named("Inner confirmation").is_some());
    assert!(frame.a11y_node_named("Outer confirmation").is_none());
    let buttons = connect_buttons(&frame);
    assert_eq!(buttons.len(), 1);
    frame
        .perform_a11y_action(buttons[0], &Action::Activate)
        .unwrap();
    assert_eq!(inner_clicked.get(), 1);
    assert_eq!(outer_clicked.get(), 0);
    inner_shown.set(false);
    frame.relayout();
    assert!(frame.a11y_node_named("Outer confirmation").is_some());
    let buttons = connect_buttons(&frame);
    assert_eq!(buttons.len(), 1);
    frame
        .perform_a11y_action(buttons[0], &Action::Activate)
        .unwrap();
    assert_eq!(outer_clicked.get(), 1);

    let mut root = dialog("Active modal", &outer_shown, &outer_clicked, Column::new());
    let mut tree = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    Widget::build_layout(&mut root, &mut tree, &crate::Theme::dark(), &mut nodes);
    let resource = tilo_actions::Resource::new("test.authority", "test", "Authoritative resource");
    let update = build_tree_update_with_resources(&root, &nodes, None, 1.0, &[resource]);
    assert!(
        update
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Authoritative resource")),
        "modal widget scoping never removes the canonical resource forest"
    );
}

#[test]
fn the_last_open_sibling_modal_wins_and_closing_it_restores_the_previous_one() {
    let first_shown = Rc::new(Cell::new(true));
    let last_shown = Rc::new(Cell::new(true));
    let first_clicked = Rc::new(Cell::new(0));
    let last_clicked = Rc::new(Cell::new(0));
    let root = Column::new()
        .push(Box::new(dialog(
            "First modal",
            &first_shown,
            &first_clicked,
            Column::new(),
        )))
        .push(Box::new(dialog(
            "Last modal",
            &last_shown,
            &last_clicked,
            Column::new(),
        )));
    let mut frame =
        FrameHarness::for_root(crate::State::new(), root, (800, 600), crate::Theme::dark());
    assert!(frame.a11y_node_named("Last modal").is_some());
    assert!(frame.a11y_node_named("First modal").is_none());
    let buttons = connect_buttons(&frame);
    assert_eq!(buttons.len(), 1);
    frame
        .perform_a11y_action(buttons[0], &Action::Activate)
        .unwrap();
    assert_eq!(last_clicked.get(), 1);
    assert_eq!(first_clicked.get(), 0);
    last_shown.set(false);
    frame.relayout();
    assert!(frame.a11y_node_named("First modal").is_some());
    let buttons = connect_buttons(&frame);
    assert_eq!(buttons.len(), 1);
    frame
        .perform_a11y_action(buttons[0], &Action::Activate)
        .unwrap();
    assert_eq!(first_clicked.get(), 1);
    assert_eq!(last_clicked.get(), 1);
}
