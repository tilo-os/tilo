use generated_app::{App, Projects};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Invoke, Replica, TestApp};

#[test]
fn creating_a_project_needs_a_name_and_clears_the_draft() {
    let mut projects = Replica::<Projects>::new();
    assert!(projects.apply(Projects::CREATE.invoke()).is_err());
    projects
        .apply(Invoke::edit(&Projects::paths().draft.replace("Atlas")).expect("an edit"))
        .expect("draft");
    projects.apply(Projects::CREATE.invoke()).expect("create");
    let state = projects.state().expect("projects");
    assert!(state.draft.is_empty());
    assert_eq!(
        state
            .projects
            .values()
            .map(|project| project.name.as_str())
            .collect::<Vec<_>>(),
        ["Atlas"]
    );
}

#[test]
fn serialized_screen_and_native_controls_share_one_resource() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .perform_named("name-input", &Action::SetValue("Atlas".into()))
        .expect("the draft input remains addressable");
    app.harness()
        .expect(
            &SemanticExpectation::named("name-preview")
                .role(Role::StaticText)
                .name("Atlas"),
        )
        .expect("the preview follows the draft");
    app.harness()
        .click_named("create")
        .expect("the create button remains addressable");
    assert_eq!(app.state().projects.len(), 1);
    assert!(app.undo(), "creating a project is undoable");
    assert!(app.state().projects.is_empty());
}
