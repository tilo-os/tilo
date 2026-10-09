//! The live host withdraws a component's whole action surface when one of
//! its actions breaks its contract: agents, Ctrl+K and screen readers then
//! lose every Settings action at once, and the only trace is a log line per
//! frame. Open every pane through the framework host Settings really runs
//! in and require the surface it would publish to pass validation.

use settings::SettingsShell;
use settings::categories::Category;
use settings::effects::{open_pane, pane_row_id};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Effect as _;

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
        app.effect_on(&pane_row_id(category), &open_pane(category))
            .unwrap_or_else(|error| panic!("{} opens: {error:?}", category.slug()));
        app.settle();
        let violations = app.harness().component_contract_violations();
        if !violations.is_empty() {
            failures.push(format!("{}: {violations:#?}", category.slug()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn navigation_semantics(node: &ui::headless::InspectionNode) -> Option<&ui::a11y::AccessNode> {
    if node.test_id.as_deref() == Some("settings.nav") {
        return node.semantics.as_ref();
    }
    node.children.iter().find_map(navigation_semantics)
}

fn admitted_category_rows(node: &ui::a11y::AccessNode, rows: &mut Vec<(String, String)>) {
    if node.state.hidden {
        return;
    }
    if node.role == ui::a11y::Role::TreeItem
        && Category::ALL
            .iter()
            .any(|category| node.name.as_deref() == Some(category.label().as_str()))
    {
        rows.push((
            node.name.clone().expect("a category has its actual label"),
            node.action_target
                .clone()
                .expect("an admitted row has its actual target"),
        ));
    }
    for child in &node.children {
        admitted_category_rows(child, rows);
    }
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
    let open = open_pane(Category::Bluetooth);
    let navigation = bindings
        .iter()
        .filter(|binding| {
            matches!(
                &binding.kind,
                ui::widget::ControlBindingKind::Action(address)
                    if address.resource == "settings.root" && address.action == open.id()
            )
        })
        .collect::<Vec<_>>();
    // The sidebar virtualizes its control inventory. Match the actual
    // admitted semantic rows, rather than expecting offscreen categories to
    // occupy controls in this frame.
    let frame = app.harness().inspection();
    let tree = navigation_semantics(&frame.root).expect("the retained navigation list");
    let mut visible = Vec::new();
    admitted_category_rows(tree, &mut visible);
    assert!(!visible.is_empty(), "the sidebar admits category rows");
    assert_eq!(
        navigation.len(),
        visible.len(),
        "every admitted category row has one typed open-pane binding: {bindings:#?}"
    );
    for (label, target) in &visible {
        assert_eq!(
            navigation
                .iter()
                .filter(|binding| &binding.control_id == target)
                .count(),
            1,
            "{label}'s actual row {target} shares the canonical open-pane door"
        );
    }
    assert!(
        navigation
            .iter()
            .all(|binding| binding.control_id.starts_with("node:")),
        "shared navigation binds the actual retained row controls"
    );
    let bluetooth = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.role() == ui::accesskit::Role::TreeItem
                && node.label() == Some(Category::Bluetooth.label().as_str()))
            .then(|| node.bounds())
            .flatten()
        })
        .expect("the retained Bluetooth row exposes its actual pointer bounds");
    app.harness().click_at(
        f64::midpoint(bluetooth.x0, bluetooth.x1),
        f64::midpoint(bluetooth.y0, bluetooth.y1),
    );
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
                .is_ok_and(|frame| frame.contains("Privacy"))
        });
    assert!(shown, "the link opens Agent Models");
    let category = Category::AgentModels.label();
    assert!(
        app.harness().a11y_nodes().iter().any(|(_, node)| {
            node.label() == Some(category.as_str()) && node.is_selected() == Some(true)
        }),
        "the model link selects the canonical Agent Models category"
    );
}
