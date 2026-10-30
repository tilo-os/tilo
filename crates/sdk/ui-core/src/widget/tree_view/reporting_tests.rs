//! A row's rename, `Delete` and buttons as published actions
//! ([`TreeImpl::on_rename_action`], [`TreeImpl::on_delete_action`],
//! [`TreeImpl::on_row_actions_action`]): what the tree reports, and the
//! callbacks it reports to instead where the app bound none.

use std::cell::RefCell;
use std::rc::Rc;

use super::tests::navigator_tree;
use super::*;
use crate::widget::controls::ActionControls;

fn see<I: serde::Serialize + 'static>(
    kind: tilo_actions::ParamType,
) -> tilo_actions::ActionBinding<I> {
    crate::widget::test_value_action_binding(kind)
}

fn flat() -> TreeImpl<(), TreeNode<()>> {
    let mut view = TreeImpl::from_model(navigator_tree());
    view.set_expanded(2, true);
    view
}

fn take<T: TreeModel + 'static>(view: &mut TreeImpl<(), T>) -> Option<serde_json::Value> {
    view.take_control_invocation()
        .and_then(|invocation| invocation.arguments.get("value").cloned())
}

#[test]
fn a_rename_reports_its_rows_key_to_the_bound_action() {
    let mut view = flat().on_rename_action(see(tilo_actions::ParamType::Array));
    view.report_rename(3, "renamed.rs".to_owned());
    assert_eq!(
        take(&mut view),
        Some(serde_json::json!(["3", "renamed.rs"]))
    );
}

#[test]
fn delete_reports_its_rows_key_to_the_bound_action() {
    let reported = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&reported);
    let mut view = flat()
        .on_delete(move |id| seen.borrow_mut().push(id))
        .on_delete_action(see(tilo_actions::ParamType::Text));
    view.report_delete(3);
    assert_eq!(take(&mut view), Some(serde_json::json!("3")));
    assert!(
        reported.borrow().is_empty(),
        "a bound delete reports once, to its action"
    );
}

#[test]
fn a_delete_without_a_binding_reports_to_the_callback() {
    let reported = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&reported);
    let mut view = flat().on_delete(move |id| seen.borrow_mut().push(id));
    view.report_delete(3);
    assert_eq!(*reported.borrow(), [3]);
    assert_eq!(take(&mut view), None);
}

#[test]
fn a_rows_button_reports_its_key_to_that_buttons_binding() {
    let unbound = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&unbound);
    let mut view = flat()
        .with_row_actions(vec![
            TreeRowAction::new(Icon::Plus, "New child"),
            TreeRowAction::new(Icon::Trash2, "Delete"),
        ])
        .on_row_action(move |id, index| seen.borrow_mut().push((id, index)))
        .on_row_actions_action([(0, see::<String>(tilo_actions::ParamType::Text))]);
    view.report_row_action(3, 0);
    assert_eq!(take(&mut view), Some(serde_json::json!("3")));
    assert!(unbound.borrow().is_empty(), "the bound button reports once");
    view.report_row_action(4, 1);
    assert_eq!(
        *unbound.borrow(),
        [(4, 1)],
        "an unbound button keeps its callback"
    );
}
