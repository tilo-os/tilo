//! The live host withdraws a component's whole action surface when one of
//! its actions breaks its contract: agents, Ctrl+K and screen readers then
//! lose every Settings action at once, and the only trace is a log line per
//! frame. Open every pane through the framework host Settings really runs
//! in and require the surface it would publish to pass validation.

use settings::SettingsShell;
use settings::categories::Category;
use settings::effects::open_pane_address;
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
        let open = open_pane_address(category);
        app.call_on(&open.resource, &open.action, serde_json::json!({}))
            .unwrap_or_else(|error| panic!("{} opens: {error:?}", category.slug()));
        app.settle();
        let violations = app.harness().component_contract_violations();
        if !violations.is_empty() {
            failures.push(format!("{}: {violations:#?}", category.slug()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A sidebar row opens its pane through the pane row's open effect, the
/// one way search and agents open it too.
#[test]
fn a_sidebar_click_opens_the_pane_through_its_effect() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    let bindings = app
        .surface::<settings::SettingsApp, _>(|settings| {
            ui::widget::controls::control_action_bindings(&*settings)
        })
        .expect("the settings surface is mounted");
    let open = open_pane_address(Category::Bluetooth);
    assert!(
        bindings.iter().any(|binding| matches!(
            &binding.kind,
            ui::widget::ControlBindingKind::Action(address) if address.action == open.action
        ) && binding.control_id == open.encode()),
        "the Bluetooth row opens its pane through {}: {bindings:#?}",
        open.action
    );
    app.harness()
        .click_named(&Category::Bluetooth.label())
        .expect("the Bluetooth row is on screen");
    app.settle();
    let selected = app
        .surface::<settings::SettingsApp, _>(|settings| settings.selected())
        .expect("the settings surface is mounted");
    assert_eq!(selected, Category::Bluetooth);
}

/// A `settings://models/<id>` link opens Settings at Agent Models.
#[test]
fn a_model_link_opens_agent_models() {
    let mut app = TestApp::<SettingsShell>::launched(&["settings://models/u2net-subject"])
        .expect("settings opens on a link");
    let shown = app
        .harness()
        .wait_until(ui::time::Duration::from_secs(20), |harness| {
            harness
                .inspection_json()
                .is_ok_and(|frame| frame.contains("Private by design"))
        });
    assert!(shown, "the link opens Agent Models");
}
