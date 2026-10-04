use super::*;
use crate::a11y::Action;
use crate::headless::FrameHarness;
use std::cell::Cell;
use std::rc::Rc;

fn harness(
    disabled: bool,
) -> (
    FrameHarness,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
) {
    let row = Rc::new(Cell::new(0));
    let edit = Rc::new(Cell::new(0));
    let pin = Rc::new(Cell::new(0));
    let (r, e, p) = (row.clone(), edit.clone(), pin.clone());
    let mut widget = ListRow::new("machine", "Build Box")
        .on_activate(move || r.set(r.get() + 1))
        .action(Icon::Pencil, "Edit connection", move || e.set(e.get() + 1))
        .action(Icon::Pin, "Pin connection", move || p.set(p.get() + 1));
    widget.activation.state.disabled = disabled;
    (
        FrameHarness::new(crate::State::new(), Box::new(widget), (400, 200)),
        row,
        edit,
        pin,
    )
}

#[test]
fn hosted_trailing_actions_have_bounds_stable_identity_and_separate_callbacks() {
    let (mut h, row, edit, pin) = harness(false);
    let edit_id = h.a11y_node_named("Edit connection").unwrap();
    let pin_id = h.a11y_node_named("Pin connection").unwrap();
    let bounds = h
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == edit_id)
        .unwrap()
        .1
        .bounds()
        .unwrap();
    assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
    assert!(bounds.x0 >= 0.0 && bounds.x1 <= 400.0);
    h.perform_a11y_action(edit_id, &Action::Activate).unwrap();
    h.perform_a11y_action(pin_id, &Action::Activate).unwrap();
    assert_eq!((row.get(), edit.get(), pin.get()), (0, 1, 1));
    assert_eq!(h.a11y_node_named("Edit connection"), Some(edit_id));
    let main = h.a11y_node_named("Build Box").unwrap();
    h.perform_a11y_action(main, &Action::Activate).unwrap();
    assert_eq!((row.get(), edit.get(), pin.get()), (1, 1, 1));
    h.click_at((bounds.x0 + bounds.x1) / 2.0, (bounds.y0 + bounds.y1) / 2.0);
    assert_eq!((row.get(), edit.get(), pin.get()), (1, 2, 1));
}

#[test]
fn hosted_keyboard_focus_reaches_each_action_and_returns_to_row() {
    let (mut h, row, edit, pin) = harness(false);
    h.key("Tab");
    h.key("ArrowRight");
    h.key("Enter");
    assert_eq!((row.get(), edit.get(), pin.get()), (0, 1, 0));
    h.key("ArrowRight");
    h.key("Space");
    assert_eq!((row.get(), edit.get(), pin.get()), (0, 1, 1));
    h.key("ArrowRight");
    h.key("Enter");
    assert_eq!((row.get(), edit.get(), pin.get()), (1, 1, 1));
    let edit_id = h.a11y_node_named("Edit connection").unwrap();
    h.perform_a11y_action(edit_id, &Action::Focus).unwrap();
    assert_eq!(h.a11y_focus().unwrap().0, edit_id);
    h.key("Enter");
    assert_eq!((row.get(), edit.get(), pin.get()), (1, 2, 1));
    let main = h.a11y_node_named("Build Box").unwrap();
    h.perform_a11y_action(main, &Action::Focus).unwrap();
    h.key("Enter");
    assert_eq!((row.get(), edit.get(), pin.get()), (2, 2, 1));
}

#[test]
fn hosted_disabled_row_cannot_run_trailing_actions() {
    let (mut h, row, edit, pin) = harness(true);
    let edit_id = h.a11y_node_named("Edit connection").unwrap();
    let _ = h.perform_a11y_action(edit_id, &Action::Activate);
    let _ = h.perform_a11y_action(edit_id, &Action::Focus);
    h.key("Tab");
    h.key("ArrowRight");
    h.key("Enter");
    assert_eq!((row.get(), edit.get(), pin.get()), (0, 0, 0));
}
