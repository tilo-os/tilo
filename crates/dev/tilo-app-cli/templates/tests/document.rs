use generated_app::{App, Document};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, ResourceApp as _, TestApp};

/// Two collaborators typing in the same field at once converge on one text.
#[test]
fn concurrent_edits_converge() {
    let mut left = Replica::<Document>::new();
    left.apply(Document::SET_BODY.with("hello world".into()))
        .expect("seed");
    let mut right = left.fork();
    left.apply(Document::SET_BODY.with("hello brave world".into()))
        .expect("left edit");
    right
        .apply(Document::SET_BODY.with("hello world!".into()))
        .expect("right edit");
    left.sync_with(&mut right).expect("sync");
    assert_eq!(left.state().expect("document").body, "hello brave world!");
    assert_eq!(right.state().expect("document").body, "hello brave world!");
}

/// The inputs invoke the actions, the preview follows, and undo reverts.
#[test]
fn the_preview_follows_the_inputs_and_undo_reverts() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .perform_named(
            "document-title-input",
            &Action::SetValue("Project Atlas".into()),
        )
        .expect("the title input is addressable");
    app.harness()
        .expect(
            &SemanticExpectation::named("document-title-preview")
                .role(Role::Heading { level: 2 })
                .name("Project Atlas"),
        )
        .expect("the preview follows the document");
    assert_eq!(App::label(&app.state()), "Project Atlas");
    assert!(app.undo(), "the edit is undoable");
    assert_eq!(app.state().title, "");
    assert_eq!(App::label(&app.state()), "Untitled document");
}

/// Clearing is destructive, so it needs confirmation; the document is stored
/// and survives a relaunch.
#[test]
fn clearing_needs_confirmation_and_the_document_survives_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Document::SET_CONTENT.with(("Plan".into(), "Ship it".into())))
        .expect("set content");
    assert!(
        app.invoke(Document::CLEAR.invoke()).is_err(),
        "a destructive action needs confirmation"
    );
    let mut app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().body, "Ship it");
    app.invoke_confirmed(Document::CLEAR.invoke())
        .expect("a confirmed clear");
    assert_eq!(app.state(), Document::default());
}
