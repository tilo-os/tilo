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
    let open = open_pane_address(Category::Bluetooth);
    let navigation = bindings
        .iter()
        .filter(|binding| {
            matches!(
                &binding.kind,
                ui::widget::ControlBindingKind::Action(address)
                    if address.resource == "settings.root" && address.action == open.action
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

#[test]
fn inspected_navigation_tracks_the_actual_selected_search_result_without_mutation() {
    let mut app = TestApp::<SettingsShell>::new().expect("mounted Settings");
    app.settle();
    let before = app
        .call_on(
            "settings.navigation",
            "inspect-navigation",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    let entries = settings::search::pane_entries();
    let hit = entries
        .iter()
        .find(|entry| entry.title == Category::MouseKeyboard.label())
        .expect("the actual keyboard search entry");
    let open = hit
        .action
        .as_ref()
        .expect("the search result owns its route");
    app.call_on(&open.resource, &open.action, serde_json::json!({}))
        .unwrap();
    app.settle();
    let shown = app
        .call_on(
            "settings.navigation",
            "inspect-navigation",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
    assert_eq!(shown["selected_pane"], "mouse_keyboard");
    assert_eq!(shown["requested_pane"], "mouse_keyboard");
    assert_eq!(shown["path"], "settings://mouse_keyboard");
    assert_eq!(shown["pending"], false);
    assert_eq!(shown["ready"], true);
    assert_eq!(shown["content_visible"], true);
    assert_eq!(shown["presentation_attribution_available"], false);
    let actual = app
        .surface::<settings::SettingsApp, _>(|settings| settings.selected())
        .unwrap();
    assert_eq!(actual, Category::MouseKeyboard);
    assert_ne!(before["selected_pane"], shown["selected_pane"]);
    assert!(
        app.call("open-pane", serde_json::json!({"pane":"not-a-pane"}))
            .is_err()
    );
    assert_eq!(
        app.call_on(
            "settings.navigation",
            "inspect-navigation",
            serde_json::json!({})
        )
        .unwrap()
        .unwrap(),
        shown
    );
    for private in ["search_text", "document", "password", "body"] {
        assert!(shown.get(private).is_none());
    }
}

#[test]
fn read_navigation_permits_viewer_but_refuses_unrelated_actor() {
    let mut app = TestApp::<SettingsShell>::with_role(ui::Role::Viewer).unwrap();
    app.settle();
    let read = |app: &mut TestApp<SettingsShell>| {
        app.call_on(
            "settings.navigation",
            "inspect-navigation",
            serde_json::json!({}),
        )
        .unwrap()
        .unwrap()
    };
    let before = read(&mut app);
    assert_eq!(read(&mut app), before);
    // Read is declared device-local. An unrelated invocation is refused by
    // the earlier private-view boundary; preserve that actual admission order
    // rather than relabeling it or widening remote-view authority.
    let action = app
        .actions()
        .unwrap()
        .into_iter()
        .find(|action| action.address.action == "inspect-navigation")
        .unwrap();
    assert_eq!(
        action.contract.permission,
        ui::tilo_actions::PermissionRequirement::Read
    );
    assert!(
        action
            .contract
            .tags
            .iter()
            .any(|tag| tag == ui::tilo_actions::LOCAL_DEVICE_ONLY)
    );
    let mut owner = TestApp::<SettingsShell>::new().unwrap();
    owner.settle();
    let owner_before = read(&mut owner);
    let denied = owner.call_by(
        ui::tilo_actions::ActionInvoker {
            actor: "unrelated-collaborator".into(),
            participant: None,
        },
        "settings.navigation",
        "inspect-navigation",
        serde_json::json!({}),
    );
    assert!(
        matches!(denied, Err(ui::tilo_actions::DoorError::Unavailable { ref code, .. }) if code == "private_view"),
        "actual denied result: {denied:?}"
    );
    assert_eq!(read(&mut app), before);
    assert_eq!(read(&mut owner), owner_before);
}
