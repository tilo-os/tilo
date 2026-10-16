use std::time::Duration;

use example_data_table::{App, ROW_COUNT, Review};
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
fn opening_rows_keeps_the_most_recent_first() {
    let mut review = Replica::<Review>::new();
    for id in [3, 7, 3] {
        review.apply(Review::OPEN_ROW.with(id)).expect("open a row");
    }
    assert_eq!(review.state().expect("review").opened, [3, 7]);
    assert!(review.apply(Review::OPEN_ROW.with(u64::MAX)).is_err());
}

#[test]
fn app_exposes_a_virtualized_table_whose_rows_open_through_the_resource() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness
        .perform_named("rows-table", &Action::Focus)
        .expect("the table remains addressable");
    harness
        .wait_for_named("open-3", Duration::from_secs(1))
        .expect("the first background page becomes ready");
    harness
        .perform_semantic_named("rows-table", Some("row:3:0"), &Action::Activate)
        .expect("select a stable row id");
    harness
        .expect(
            &SemanticExpectation::named("rows-table")
                .semantic_target("row:3:0")
                .role(Role::Row)
                .selected(true),
        )
        .expect("selected row semantics match");

    let semantics =
        semantics_of(&harness.inspection().root, "rows-table").expect("table semantics");
    assert_eq!(semantics.role, Role::Table);
    assert!(semantics.state.focused);
    let selected = semantics
        .children
        .iter()
        .find(|row| row.action_target.as_deref() == Some("row:3:0"))
        .expect("the selected row remains in the semantic window");
    assert!(selected.state.selected);
    let cells = selected
        .children
        .iter()
        .map(|cell| cell.value.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        cells.get(..2),
        Some([Some("Row 3"), Some("Closed")].as_slice())
    );
    assert!(
        semantics.children.len() < 40,
        "only the sparse viewport is semantic"
    );
    assert_eq!(
        semantics.children.first().and_then(|row| row.set_size),
        Some(ROW_COUNT),
    );

    harness
        .perform_named("open-3", &Action::Activate)
        .expect("the retained cell button remains addressable");
    assert_eq!(app.state().opened, [3]);
    app.harness()
        .expect(&SemanticExpectation::named("recently-opened").name_contains("Row 3"))
        .expect("the view follows the resource");
    assert!(app.undo(), "opening a row is undoable");
    assert!(app.state().opened.is_empty());
}

#[test]
fn rows_select_through_direct_touch() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness
        .wait_for_named("open-3", Duration::from_secs(1))
        .expect("the first background page becomes ready");
    harness.set_interaction_environment(ui::InteractionEnvironment::TOUCH);
    let semantics =
        semantics_of(&harness.inspection().root, "rows-table").expect("table semantics");
    let bounds = semantics
        .children
        .first()
        .and_then(|row| row.bounds)
        .expect("a visible touch row")
        .to_rect();
    assert!((bounds.height() - 48.0).abs() < f64::EPSILON);
    let center = bounds.center();
    harness.tap_at(center.x, center.y);
    let semantics =
        semantics_of(&harness.inspection().root, "rows-table").expect("table semantics");
    assert!(semantics.children.iter().any(|row| row.state.selected));
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
