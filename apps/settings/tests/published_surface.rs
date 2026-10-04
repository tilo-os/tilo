//! The live host withdraws a component's whole action surface when one of
//! its actions breaks its contract: agents, Ctrl+K and screen readers then
//! lose every Settings action at once, and the only trace is a log line per
//! frame. Open every pane through the framework host Settings really runs
//! in and require the surface it would publish to pass validation.

use settings::SettingsShell;
use settings::actions::{OPEN_PANE_ACTION, pane_resource_id};
use settings::categories::Category;
use ui::framework::testing::TestApp;

#[test]
fn every_pane_publishes_a_surface_that_passes_contract_validation() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let mut failures = Vec::new();
    let violations = app.harness().component_contract_violations();
    if !violations.is_empty() {
        failures.push(format!("on open: {violations:#?}"));
    }
    for category in Category::ALL.iter().copied() {
        app.call_on(
            &pane_resource_id(category),
            OPEN_PANE_ACTION,
            serde_json::json!({}),
        )
        .unwrap_or_else(|error| panic!("{} opens: {error:?}", category.slug()));
        app.settle();
        let violations = app.harness().component_contract_violations();
        if !violations.is_empty() {
            failures.push(format!("{}: {violations:#?}", category.slug()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
