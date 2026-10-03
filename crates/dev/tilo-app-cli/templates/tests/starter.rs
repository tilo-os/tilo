use generated_app::{App, Greeting};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};

/// The resource on its own: actions applied the way every surface applies
/// them, with no window.
#[test]
fn greeting_actions_edit_the_name() {
    let mut greeting = Replica::<Greeting>::new();
    greeting
        .apply(Greeting::SET_NAME.with("Ada".into()))
        .expect("rename");
    assert_eq!(greeting.state().expect("greeting").name, "Ada");
    greeting.apply(Greeting::CLEAR.invoke()).expect("clear");
    assert_eq!(greeting.state().expect("greeting").name, "");
}

/// The app as it runs: the field invokes the action, the greeting follows the
/// resource, and the edit is one undo step.
#[test]
fn the_name_field_edits_the_greeting() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .expect(&SemanticExpectation::named("greeting").name("Who should we greet?"))
        .expect("the app starts empty");
    app.harness()
        .perform_named("name-input", &Action::SetValue("Ada".into()))
        .expect("the name field is addressable");
    app.harness()
        .expect(
            &SemanticExpectation::named("greeting")
                .role(Role::StaticText)
                .name_contains("Ada"),
        )
        .expect("the greeting follows the resource");
    assert!(app.undo(), "the edit is one undo step");
    assert_eq!(app.state().name, "");
}

/// Permissions apply to every action: a viewer's edit is refused.
#[test]
fn a_viewer_cannot_edit() {
    let mut app = TestApp::<App>::with_role(ui::Role::Viewer).expect("the app opens");
    assert!(app.invoke(Greeting::SET_NAME.with("Eve".into())).is_err());
    assert_eq!(app.state().name, "");
}

/// State is stored: it survives a relaunch with no save step.
#[test]
fn the_name_survives_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Greeting::SET_NAME.with("Grace".into()))
        .expect("rename");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().name, "Grace");
}
