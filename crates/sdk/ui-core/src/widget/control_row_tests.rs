//! [`ControlRow`]: rows of one list following one choice.

use super::*;

#[test]
fn control_rows_follow_one_shared_choice() {
    let chosen = crate::LocalState::new("ink".to_owned());
    let mut ink = ControlRow::new(crate::widget::Row::new())
        .choosing(&chosen, "ink".to_owned())
        .with_accessible_name("Ink");
    let mut oil = ControlRow::new(crate::widget::Row::new())
        .choosing_any(&chosen, vec!["oil".to_owned(), "oil-mask".to_owned()]);
    assert!(ink.is_selected() && !oil.is_selected());

    // Activating a row chooses it; the other follows on sync.
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    assert!(Widget::perform_action(
        &mut oil,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &root
    ));
    assert_eq!(chosen.with(Clone::clone), "oil");
    Widget::sync(&mut ink, &doc, &root);
    assert!(!ink.is_selected() && oil.is_selected());

    // A part of the row (its mask) keeps the row chosen, and choosing
    // the row again does not take the part's place.
    chosen.set("oil-mask".to_owned());
    Widget::sync(&mut oil, &doc, &root);
    assert!(oil.is_selected());
    Widget::perform_action(
        &mut oil,
        &crate::a11y::Action::Activate,
        &[],
        &mut crate::widget::Edits::lend(&mut doc),
        &root,
    );
    assert_eq!(chosen.with(Clone::clone), "oil-mask");

    let node = Widget::accessibility(&ink).expect("a list item");
    assert_eq!(node.role, Role::ListItem);
    assert_eq!(node.name.as_deref(), Some("Ink"));
}
