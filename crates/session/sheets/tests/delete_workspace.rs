//! "Delete \<name\>?" as a person, a screen reader and an agent meet it:
//! what goes with the workspace, and the answer being the person's own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::Seat as _;
use tilo_sheets::delete_workspace::{
    Asked, DeleteWorkspaceApp, DeleteWorkspaceEffect, DeleteWorkspaceSeat,
};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;

const VIEWPORT: (u32, u32) = (1280, 800);

/// The sheet as the shell lends it, asking about the workspace called
/// `name`.
fn seated(name: &str) -> (HeadlessApp<DeleteWorkspaceApp>, DeleteWorkspaceSeat) {
    let seat = DeleteWorkspaceSeat::default();
    seat.show(name.to_owned());
    seat.presentation().open(true);
    let mut app =
        HeadlessApp::<DeleteWorkspaceApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

fn says(app: &mut HeadlessApp<DeleteWorkspaceApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn press(app: &mut HeadlessApp<DeleteWorkspaceApp>, id: &str) {
    app.harness()
        .perform_named(id, &A11yAction::Activate)
        .unwrap();
    app.settle();
}

#[test]
fn the_question_names_the_workspace_and_what_goes_with_it() {
    let (mut app, _seat) = seated("Design studio");
    for text in [
        "Delete Design studio?",
        "Everything in this workspace goes with it",
        "Delete",
        "Keep",
    ] {
        assert!(says(&mut app, text), "{text:?} is shown");
    }
}

#[test]
fn enter_keeps_the_workspace() {
    let (mut app, seat) = seated("Design studio");
    app.harness().key("Enter");
    app.settle();
    assert_eq!(
        seat.take_answers(),
        [DeleteWorkspaceEffect::Keep],
        "the keyboard starts on the safe answer of a deletion"
    );
}

#[test]
fn escape_and_closing_keep_the_workspace() {
    let (mut app, seat) = seated("Design studio");
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_answers(), [DeleteWorkspaceEffect::Keep]);

    let (mut app, seat) = seated("Design studio");
    press(&mut app, "workspace-delete.secondary");
    assert_eq!(seat.take_answers(), [DeleteWorkspaceEffect::Keep]);
}

#[test]
fn the_delete_button_is_the_only_one_that_deletes() {
    let (mut app, seat) = seated("Design studio");
    press(&mut app, "workspace-delete.primary");
    assert_eq!(seat.take_answers(), [DeleteWorkspaceEffect::Delete]);

    let (mut app, seat) = seated("Design studio");
    press(&mut app, "workspace-delete.secondary");
    assert_eq!(seat.take_answers(), [DeleteWorkspaceEffect::Keep]);
}

#[test]
fn a_new_question_replaces_the_one_before_it() {
    let (mut app, seat) = seated("Design studio");
    seat.show("Notes".to_owned());
    app.settle();
    assert_eq!(
        seat.asked(),
        Asked {
            name: "Notes".to_owned()
        }
    );
    assert!(says(&mut app, "Delete Notes?"));
    assert!(!says(&mut app, "Delete Design studio?"));
}
