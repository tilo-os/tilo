use example_graph_screens::{App, Projects, components, screens};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Replica, TestApp};
use ui::{UiGraphHandle, UiNode};

#[test]
fn creating_a_project_needs_a_name_and_clears_the_draft() {
    let mut projects = Replica::<Projects>::new();
    assert!(projects.apply(Projects::CREATE.invoke()).is_err());
    projects
        .apply(Projects::SET_DRAFT.with("Atlas"))
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

#[test]
fn serialized_graph_edits_have_shared_undo_history() {
    let graph = UiGraphHandle::from_json(include_str!("../ui/main.json"))
        .expect("the generated graph is valid");
    for next_label in ["Publ", "Publish"] {
        graph
            .update_node_grouped("graph-introduction", "intro-text", |node| {
                if let UiNode::Text {
                    text: ui::UiTextSource::Static { value },
                    ..
                } = node
                {
                    *value = next_label.into();
                }
            })
            .expect("a valid grouped runtime edit is published");
    }
    assert!(matches!(
        graph.node("graph-introduction"),
        Ok(UiNode::Text { text: ui::UiTextSource::Static { value }, .. }) if value == "Publish"
    ));

    assert!(graph.undo().expect("graph history remains available"));
    assert!(matches!(
        graph.node("graph-introduction"),
        Ok(UiNode::Text { text: ui::UiTextSource::Static { value }, .. })
            if value == "graph-introduction"
    ));
    assert!(!graph.can_undo(), "continuous typing created one undo step");
    assert!(graph.redo().expect("the undone edit can be reapplied"));
}
/// A component node in the graph is built by the Rust factory registered
/// under its name, and every registered screen builds.
#[test]
fn screens_mount_rust_components_and_all_build() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.harness()
        .expect(
            &SemanticExpectation::named("project-summary-title")
                .role(Role::Heading { level: 2 })
                .name("A component from Rust"),
        )
        .expect("the mounted component renders its catalog props");
    assert_eq!(screens::NAMES, ["about", "main"]);
    for name in screens::NAMES {
        let router =
            screens::router_with(name, &ui::Theme::default(), components::ui_graph_context());
        assert!(router.is_ok(), "{name}: {:?}", router.err());
    }
}

/// A navigation button in a serialized screen is a published action: a click
/// and an agent's invocation both change the screen, and the native contract
/// holds on every screen.
#[test]
fn the_about_link_navigates_through_a_published_action() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let about_title = SemanticExpectation::named("title")
        .role(Role::Heading { level: 1 })
        .name("About");
    let harness = app.harness();
    harness
        .perform_named("open-about", &Action::Activate)
        .expect("the About link is activatable");
    harness
        .expect(&about_title)
        .expect("the About screen replaces the main one");
    let audit = harness.native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");

    harness
        .perform_named("back", &Action::Activate)
        .expect("the About screen has a Back button");
    harness
        .expect(&SemanticExpectation::named("open-about"))
        .expect("Back returns to the main screen");

    harness
        .invoke_action(
            &ui::prelude::tilo_actions::ActionAddress::new("tilo.control:open-about", "invoke"),
            &ui::serde_json::Map::new(),
            false,
            None,
        )
        .expect("the About link is published as an action");
    harness
        .expect(&about_title)
        .expect("invoking the action navigates as a click does");
}

/// The app satisfies the Tilo-native contract: every control that can cause
/// an effect is bound to a published action.
#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
