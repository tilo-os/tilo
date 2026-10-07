use hot_reload_demo::{App, Document};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};

#[test]
fn collaborators_edit_concurrently_and_converge() {
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
fn clearing_asks_for_confirmation_and_the_document_survives_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Document::SET_CONTENT.with(("Plan".into(), "Ship it".into())))
        .expect("set content");
    assert!(
        app.invoke(Document::CLEAR.invoke()).is_err(),
        "a destructive action needs confirmation"
    );
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().body, "Ship it");
}
/// The app satisfies the Tilo-native contract: every control that can cause
/// an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = ui::prelude::TestApp::<hot_reload_demo::App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
