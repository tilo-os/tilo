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

#[test]
fn physical_navigation_observer_follows_mounted_keyboard_activation_and_actual_draw() {
    use winit::event::ElementState;
    use winit::keyboard::{Key, NamedKey};
    let mut app = TestApp::<SettingsShell>::new().expect("mounted Settings");
    app.settle();
    let node = app
        .harness()
        .a11y_node_named(&Category::Bluetooth.label())
        .expect("actual destination row");
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Focus)
        .expect("focus actual sidebar destination without activation");
    app.settle();
    let before = app
        .harness()
        .native_input_snapshot()
        .expect("Settings snapshot");
    assert_eq!(before["navigation"]["selected_pane"], "network");
    assert_eq!(before["navigation"], before["rendered_navigation"]);
    let (width, height) = app.harness().frame_output().device_size;
    let mut renderer =
        ui::offscreen::OffscreenRenderer::try_new_with_cpu_preprocessing(width, height)
            .expect("native stable-preprocessing GPU renderer");
    let background = ui::Theme::dark().tokens.colors.surfaces.content;
    let old_pixels = app.harness().render_rgba_with(&mut renderer, background);
    let key = Key::Named(NamedKey::Enter);
    let event = winit::event::WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        event: winit::event::KeyEvent::synthetic(
            winit::event::RawKeyEvent {
                physical_key: winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                ),
                state: ElementState::Pressed,
            },
            key.clone(),
            None,
            winit::keyboard::KeyLocation::Standard,
            false,
            None,
            key,
        ),
        is_synthetic: false,
    };
    let mut trace = ui::NativePhysicalInputTrace::default();
    let input = trace.begin(&event, Some(before)).expect("input witness");
    app.harness().send_host_event(&event);
    let effect = ui::NativePhysicalInputTrace::effect(input, app.harness().native_input_snapshot());
    app.settle();
    let drawn = trace
        .drawn(Some(71), app.harness().native_input_snapshot())
        .expect("draw witness");
    assert_eq!(effect["after"]["navigation"]["requested_pane"], "bluetooth");
    assert_eq!(drawn["state"]["navigation"]["selected_pane"], "bluetooth");
    assert_eq!(drawn["state"]["navigation"]["ready"], true);
    assert_eq!(
        drawn["state"]["navigation"],
        drawn["state"]["rendered_navigation"]
    );
    assert_eq!(effect["observer_session"], drawn["observer_session"]);
    assert_eq!(effect["process_id"], drawn["process_id"]);
    assert_eq!(drawn["input_ids"], serde_json::json!([1]));
    let new_pixels = app.harness().render_rgba_with(&mut renderer, background);
    assert!(!new_pixels.is_empty());
    assert_ne!(
        old_pixels, new_pixels,
        "actual composed destination changes GPU pixels"
    );
    assert_eq!(
        new_pixels,
        app.harness().render_rgba_with(&mut renderer, background)
    );
    // Native key delivery and KMS must independently qualify real measurements.
    assert!(effect["wayland_serial"].is_null());
    for row in [effect, drawn] {
        assert!(row["state"].get("local_search_text").is_none());
        assert!(!row.to_string().contains("document_actor"));
    }
}

#[test]
fn physical_hover_observer_follows_mounted_pointer_and_actual_draw_without_selecting() {
    let mut app = TestApp::<SettingsShell>::new().expect("mounted Settings");
    app.settle();
    let node = app
        .harness()
        .a11y_node_named(&Category::Bluetooth.label())
        .expect("actual destination row");
    let rect = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(id, node_info)| (id == node).then(|| node_info.bounds()).flatten())
        .expect("actual sidebar row geometry");
    let before = app
        .harness()
        .native_input_snapshot()
        .expect("Settings snapshot");
    assert_eq!(before["hover"], before["rendered_hover"]);
    let (width, height) = app.harness().frame_output().device_size;
    let mut renderer =
        ui::offscreen::OffscreenRenderer::try_new_with_cpu_preprocessing(width, height)
            .expect("native stable-preprocessing GPU renderer");
    let background = ui::Theme::dark().tokens.colors.surfaces.content;
    let old_pixels = app.harness().render_rgba_with(&mut renderer, background);
    let event = winit::event::WindowEvent::CursorMovedObserved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(
            f64::midpoint(rect.x0, rect.x1),
            f64::midpoint(rect.y0, rect.y1),
        ),
        pointer_id: 17,
        enter_serial: 29,
        motion_ordinal: 3,
        time_ms: Some(31),
    };
    let mut trace = ui::NativePhysicalInputTrace::default();
    let input = trace
        .begin(&event, Some(before))
        .expect("immutable motion observation");
    let ordinary = ui::NativePhysicalInputTrace::ordinary_pointer_event(event);
    assert!(matches!(
        ordinary,
        winit::event::WindowEvent::CursorMoved { .. }
    ));
    app.harness().send_host_event(&ordinary);
    app.settle();
    let after = app
        .harness()
        .native_input_snapshot()
        .expect("actual hovered Settings");
    let effect = ui::NativePhysicalInputTrace::effect(input, Some(after.clone()));
    let drawn = trace
        .drawn(Some(43), Some(after))
        .expect("actual draw witness");
    assert_eq!(effect["pointer_id"], 17);
    assert_eq!(effect["pointer_motion_ordinal"], 3);
    assert!(effect["wayland_serial"].is_null());
    let state = &drawn["state"];
    assert_eq!(state["navigation"]["selected_pane"], "network");
    assert_eq!(state["hover"]["pane"], "bluetooth");
    assert_eq!(state["hover"], state["rendered_hover"]);
    let pixels = app.harness().render_rgba_with(&mut renderer, background);
    assert_ne!(pixels, old_pixels, "ordinary pointer paints a real hover");
    assert_eq!(
        pixels,
        app.harness().render_rgba_with(&mut renderer, background)
    );
    // This fixture supplies a wire identity, never a trusted physical origin/KMS.
    assert!(effect.get("input_time_ns").is_none());
    let leave = winit::event::WindowEvent::CursorLeft {
        device_id: winit::event::DeviceId::dummy(),
    };
    app.harness().send_host_event(&leave);
    app.settle();
    let left = app
        .harness()
        .native_input_snapshot()
        .expect("actual cleared hover");
    assert!(left["hover"]["pane"].is_null());
    assert_eq!(left["hover"], left["rendered_hover"]);
}
