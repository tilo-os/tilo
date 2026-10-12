use generated_app::{App, Greeting};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Invoke, Replica, TestApp};

/// The edit of the name: a field edit at its path, as the name field makes it.
fn name(text: &str) -> Invoke<Greeting> {
    Invoke::edit(&Greeting::paths().name.replace(text)).expect("an edit")
}

/// The resource on its own: edits and verbs applied the way every surface
/// applies them, with no window.
#[test]
fn greeting_actions_edit_the_name() {
    let mut greeting = Replica::<Greeting>::new();
    greeting.apply(name("Ada")).expect("rename");
    assert_eq!(greeting.state().expect("greeting").name, "Ada");
    greeting.apply(Greeting::CLEAR.invoke()).expect("clear");
    assert_eq!(greeting.state().expect("greeting").name, "");
}

/// The app as it runs: the field edits the name, the greeting follows the
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
    assert!(app.invoke(name("Eve")).is_err());
    assert_eq!(app.state().name, "");
}

/// State is stored: it survives a relaunch with no save step.
#[test]
fn the_name_survives_a_relaunch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(name("Grace")).expect("rename");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().name, "Grace");
}

/// The same resource syncs in both directions, updates the live view, and
/// keeps a collaborator's edit when the local participant undoes theirs.
#[test]
fn collaboration_updates_the_view_and_undo_preserves_peer_edits() {
    let mut app = TestApp::<App>::for_person("ada").expect("the app opens");
    app.invoke(name("Ada")).expect("initial name");
    let mut peer = Replica::<Greeting>::from_doc(app.peer_document()).as_author("grace");

    app.invoke(name("Countess Ada")).expect("local prefix");
    let envelope = peer.apply(name("Ada Byron")).expect("peer suffix");
    assert_eq!(envelope.actor.as_deref(), Some(b"grace".as_slice()));
    let remote_actor = envelope.actor().to_hex_string();
    app.merge_peer(peer.doc_mut()).expect("exchange edits");

    assert_eq!(app.state().name, "Countess Ada Byron");
    assert_eq!(peer.state().expect("peer state").name, "Countess Ada Byron");
    app.harness()
        .expect(&SemanticExpectation::named("greeting").name_contains("Countess Ada Byron"))
        .expect("the view follows sync without app-specific glue");

    assert!(app.undo(), "undo the local prefix");
    assert_eq!(app.state().name, "Ada Byron", "the peer suffix remains");
    assert!(app.redo(), "redo the local prefix");
    assert_eq!(app.state().name, "Countess Ada Byron");
    let versions = app.versions();
    assert!(versions.len() >= 3, "local and remote edits have history");
    assert!(
        versions
            .iter()
            .any(|version| version.actor == remote_actor && version.context.is_some()),
        "the peer edit keeps its author and action context in history"
    );
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().name, "Countess Ada Byron");
}
