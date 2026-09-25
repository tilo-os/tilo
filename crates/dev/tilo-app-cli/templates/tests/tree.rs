use generated_app::{App, Browser};
use ui::a11y::{AccessNode, Action, Role};
use ui::headless::{InspectionNode, SemanticExpectation};
use ui::prelude::{Replica, TestApp};

fn semantics_of(node: &InspectionNode, id: &str) -> Option<AccessNode> {
    if node.test_id.as_deref() == Some(id) {
        return node.semantics.clone();
    }
    node.children
        .iter()
        .find_map(|child| semantics_of(child, id))
}

#[test]
fn pinning_toggles_and_refuses_unknown_nodes() {
    let mut browser = Replica::<Browser>::new();
    browser.apply(Browser::TOGGLE_PIN.with(4)).expect("pin");
    assert_eq!(browser.state().expect("browser").pinned, [4]);
    browser.apply(Browser::TOGGLE_PIN.with(4)).expect("unpin");
    assert!(browser.state().expect("browser").pinned.is_empty());
    assert!(browser.apply(Browser::TOGGLE_PIN.with(99)).is_err());
}

#[test]
fn app_exposes_an_accessible_tree() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness
        .perform_named("workspace-tree", &Action::Focus)
        .expect("the tree remains addressable");
    harness
        .perform_semantic_named("workspace-tree", Some("node:2"), &Action::Collapse)
        .expect("collapse a stable model node");
    harness
        .perform_semantic_named("workspace-tree", Some("node:2"), &Action::Expand)
        .expect("expand a stable model node");
    harness
        .perform_semantic_named("workspace-tree", Some("node:3"), &Action::Activate)
        .expect("select a stable model node");
    harness
        .expect(
            &SemanticExpectation::named("workspace-tree")
                .semantic_target("node:3")
                .role(Role::TreeItem)
                .selected(true),
        )
        .expect("selected tree row semantics match");

    let semantics =
        semantics_of(&harness.inspection().root, "workspace-tree").expect("tree semantics");
    assert_eq!(semantics.role, Role::Tree);
    assert!(semantics.state.focused);
    assert_eq!(semantics.name.as_deref(), Some("Workspace items"));
    assert_eq!(semantics.value.as_deref(), Some("Desktop app"));
    let row = |id: &str| {
        semantics
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("row {id}"))
    };
    assert_eq!(row("node:2").state.expanded, Some(true));
    assert!(row("node:3").state.selected);
    assert!(!row("node:4").state.selected);

    harness
        .perform_named("more-4", &Action::Activate)
        .expect("the retained row action remains independently addressable");
    assert_eq!(app.state().pinned, [4]);
    app.harness()
        .expect(&SemanticExpectation::named("pinned-count").name_contains("1"))
        .expect("the view follows the resource");
}
