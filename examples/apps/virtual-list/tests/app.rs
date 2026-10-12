use std::time::Duration;

use example_virtual_list::{App, Feed, ITEM_COUNT};
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

/// Opening moves an activity to the front of a short recent list; ids the
/// source does not have are refused.
#[test]
fn opening_an_activity_is_remembered() {
    let mut feed = Replica::<Feed>::new();
    for id in [1, 2, 3, 4, 5, 6, 2] {
        feed.apply(Feed::OPEN_ACTIVITY.with(id))
            .expect("open an activity");
    }
    assert_eq!(feed.state().expect("feed").opened, [2, 6, 5, 4, 3]);
    let beyond = u64::try_from(ITEM_COUNT).expect("fits");
    assert!(feed.apply(Feed::OPEN_ACTIVITY.with(beyond)).is_err());
}

/// Only the viewport is mounted, rows load in the background, keys are
/// stable, heights vary, and each row's button is its own control.
#[test]
fn app_exposes_a_sparse_interactive_virtual_list() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness
        .perform_named("activity-list", &Action::Focus)
        .expect("the list remains addressable");
    harness
        .wait_for_named("open-0", Duration::from_secs(1))
        .expect("the first background page becomes ready");
    harness
        .perform_semantic_named("activity-list", Some("key:3"), &Action::Activate)
        .expect("select a stable model key");
    harness
        .expect(
            &SemanticExpectation::named("activity-list")
                .semantic_target("key:3")
                .role(Role::ListItem)
                .selected(true),
        )
        .expect("selected list row semantics match");

    let semantics =
        semantics_of(&harness.inspection().root, "activity-list").expect("list semantics");
    assert_eq!(semantics.role, Role::List);
    assert!(semantics.state.focused);
    assert!(
        semantics.children.len() < 30,
        "only the viewport window is mounted"
    );
    assert_eq!(
        semantics.children.first().and_then(|row| row.set_size),
        Some(ITEM_COUNT),
    );
    let heights = semantics
        .children
        .iter()
        .take(2)
        .filter_map(|row| row.bounds.map(|bounds| bounds.to_rect().height()))
        .collect::<Vec<_>>();
    assert!(
        heights
            .windows(2)
            .any(|pair| (pair[0] - pair[1]).abs() > 1.0),
        "delegate measurements drive variable rows: {heights:?}"
    );

    harness
        .perform_named("open-0", &Action::Activate)
        .expect("the interactive row delegate remains independently addressable");
    assert_eq!(app.state().opened, [0]);
    app.harness()
        .expect(&SemanticExpectation::named("last-opened").name_contains("Activity 0"))
        .expect("the view follows the resource");
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
