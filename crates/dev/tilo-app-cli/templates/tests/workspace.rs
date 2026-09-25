use generated_app::App;
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::TestApp;

#[test]
fn workspace_keeps_master_detail_views_isolated_and_synchronized() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    let targets = harness.targets();
    for target in [
        "workspace-selected-item",
        "workspace-title-input",
        "workspace-notes-input",
        "workspace-share",
    ] {
        let rect = targets
            .get(target)
            .unwrap_or_else(|| panic!("missing {target}"));
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "{target}: {rect:?}"
        );
    }
    harness
        .perform_semantic_named("workspace-tree", Some("node:4"), &Action::Activate)
        .expect("select the design-system project");
    harness
        .expect(
            &SemanticExpectation::named("workspace-selected-item")
                .role(Role::Heading { level: 1 })
                .name("Design system"),
        )
        .expect("the detail pane follows the selection");
    harness
        .perform_named(
            "workspace-title-input",
            &Action::SetValue("Design tokens".into()),
        )
        .expect("edit the selected project's title");
    harness
        .expect(
            &SemanticExpectation::named("workspace-title-preview")
                .role(Role::Heading { level: 2 })
                .name("Design tokens"),
        )
        .expect("the preview shares the resource binding");
    harness
        .perform_semantic_named("workspace-tree", Some("node:3"), &Action::Activate)
        .expect("switch to another project");
    harness
        .expect(
            &SemanticExpectation::named("workspace-title-input")
                .role(Role::TextInput)
                .value(""),
        )
        .expect("each project has its own fields");
    harness
        .perform_named(
            "workspace-title-input",
            &Action::SetValue("Desktop refresh".into()),
        )
        .expect("edit the second project");
    assert!(app.undo(), "detail edits are undoable actions");
    let workspace = app.state();
    assert_eq!(
        workspace.project(4).map(|project| project.title.as_str()),
        Some("Design tokens")
    );
    assert_eq!(
        workspace
            .project(3)
            .map_or("", |project| project.title.as_str()),
        ""
    );

    let harness = app.harness();
    harness
        .perform_semantic_named("workspace-tree", Some("node:4"), &Action::Activate)
        .expect("return to the first project");
    harness
        .expect(
            &SemanticExpectation::named("workspace-title-input")
                .role(Role::TextInput)
                .value("Design tokens"),
        )
        .expect("the first project's fields are intact");
    harness.click_named("workspace-share").expect("share");
    assert!(app.state().project(4).is_some_and(|project| project.shared));
}

#[test]
fn compact_windows_navigate_between_full_width_panes() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness.resize((460, 820)).expect("compact layout");
    harness
        .expect(
            &SemanticExpectation::named("workspace-shell-back")
                .role(Role::Button)
                .name("Back"),
        )
        .expect("the compact detail exposes an accessible Back");
    harness
        .perform_named("workspace-shell-back", &Action::Activate)
        .expect("return to compact navigation");
    harness
        .expect(&SemanticExpectation::named("workspace-tree").role(Role::Tree))
        .expect("compact navigation shows the full-width tree pane");
    harness
        .perform_semantic_named("workspace-tree", Some("node:4"), &Action::Activate)
        .expect("choose a compact navigation row");
    harness
        .expect(
            &SemanticExpectation::named("workspace-selected-item")
                .role(Role::Heading { level: 1 })
                .name("Design system"),
        )
        .expect("compact selection navigates to the full-width detail");
    let issues = harness.accessibility_audit();
    assert!(
        issues.issues.is_empty(),
        "compact workspace audit: {issues:#?}"
    );
}
