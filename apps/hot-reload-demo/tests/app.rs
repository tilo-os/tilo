#![allow(clippy::expect_used, reason = "test assertions")]

use hot_reload_demo::{App, Document};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Invoke, Replica, TestApp};

fn write_body(text: &str) -> Invoke<Document> {
    Invoke::edit(&Document::paths().body.replace(text)).expect("an edit")
}

#[test]
fn collaborators_edit_concurrently_and_converge() {
    let mut left = Replica::<Document>::new();
    left.apply(write_body("hello world")).expect("seed");
    let mut right = left.fork();
    left.apply(write_body("hello brave world"))
        .expect("left edit");
    right.apply(write_body("hello world!")).expect("right edit");
    left.sync_with(&mut right).expect("sync");
    assert_eq!(left.state().expect("document").body, "hello brave world!");
    assert_eq!(right.state().expect("document").body, "hello brave world!");
}

#[test]
fn document_fields_are_synchronized_and_undoable() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .perform_named(
            "document-title-input",
            &Action::SetValue("Project Atlas".into()),
        )
        .expect("the title input remains addressable");
    app.harness()
        .expect(
            &SemanticExpectation::named("document-title-preview")
                .role(Role::Heading { level: 3 })
                .name("Project Atlas"),
        )
        .expect("the preview follows the document");
    assert!(app.undo(), "the edit is undoable");
    assert_eq!(app.state().title, "");
}

#[test]
fn history_names_each_field_and_the_clear() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Invoke::edit(&Document::paths().title.replace("Plan")).expect("an edit"))
        .expect("retitle");
    app.invoke(write_body("Ship it")).expect("write");
    app.invoke_confirmed(Document::CLEAR.invoke())
        .expect("clear");
    let catalogue = tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .history()
        .into_iter()
        .filter(|entry| entry.what.is_some())
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        [
            "Retitled Document",
            "Edited the body of Document",
            "Cleared Document"
        ]
    );
}

#[test]
fn clearing_asks_for_confirmation_and_the_document_survives_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Invoke::edit(&Document::paths().title.replace("Plan")).expect("an edit"))
        .expect("set the title");
    app.invoke(write_body("Ship it")).expect("set the body");
    assert!(
        app.invoke(Document::CLEAR.invoke()).is_err(),
        "a destructive action needs confirmation"
    );
    let mut app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().body, "Ship it");
    app.invoke_confirmed(Document::CLEAR.invoke())
        .expect("a confirmed clear");
    assert_eq!(app.state(), Document::default());
    assert!(app.undo(), "clearing is one undo step");
    assert_eq!(app.state().title, "Plan");
}
/// The app satisfies the Tilo-native contract: every control that can cause
/// an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = ui::prelude::TestApp::<hot_reload_demo::App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
