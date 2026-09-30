//! Accessibility + keyboard proof for the retained API-tester tree.
//!
//! These tests drive the **real widget tree** through `ui::headless::Driver`
//! — the same event dispatch, layout, and `Widget::accessibility()` the live
//! app uses, with no display — and assert on the accessibility nodes the
//! AccessKit bridge exports and on keyboard focus traversal. They are the
//! reusable pattern every converted app should copy: build a `Driver`, assert
//! the focusable controls exist with the right roles/labels, Tab through them,
//! and activate one with Enter to prove the real handler runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use api_tester::ApiTesterApp;
use ui::State;
use ui::a11y::Role;
use ui::headless::Driver;

fn driver() -> Driver {
    // The demo build populates collections / environments / history / a
    // response so every control class is present.
    Driver::new(
        State::new(),
        Box::new(ApiTesterApp::new_demo()),
        (1280, 800),
    )
}

/// Every surface (full demo, empty and error responses, the narrow overlay
/// rail, contextual forms) exposes its controls as named accessibility nodes;
/// each case mounts its own Driver.
#[test]
fn every_surface_exposes_named_accessible_controls() {
    every_control_is_a_focusable_accessible_node();
    empty_and_error_response_surfaces_expose_recovery_actions();
    narrow_workspace_menu_is_keyboard_accessible();
    contextual_forms_expose_only_their_named_fields();
}

/// Every interactive control is a real focusable widget with an accessibility
/// node carrying the correct role, name, and value — the thing a single
/// painted canvas could never provide.
/// Case of `every_surface_exposes_named_accessible_controls`.
fn every_control_is_a_focusable_accessible_node() {
    let d = driver();
    let nodes = d.focus_access_nodes();
    let roles: Vec<Role> = nodes.iter().map(|n| n.role).collect();

    // Method + environment pickers are combo boxes.
    assert!(
        roles.iter().filter(|r| **r == Role::ComboBox).count() >= 2,
        "method + environment pickers are ComboBoxes, got {roles:?}"
    );
    // The URL editor is a real text field.
    assert!(
        roles.contains(&Role::TextInput),
        "URL editor is a TextInput: {roles:?}"
    );
    // The request tabs + the response-view switch are segmented groups.
    assert!(
        roles.iter().filter(|r| **r == Role::Group).count() >= 2,
        "request tabs + response view are Groups, got {roles:?}"
    );
    // Send + every sidebar/history row are buttons.
    assert!(
        roles.contains(&Role::Button),
        "Send + rows are Buttons: {roles:?}"
    );

    // Labels / values a screen reader announces.
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Button && n.name.as_deref() == Some("Send")),
        "the Send button carries its label"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::ComboBox && n.value.as_deref() == Some("GET")),
        "the method picker announces the selected verb"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::ComboBox && n.value.as_deref() == Some("Production")),
        "the environment picker announces the active environment"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Group && n.value.as_deref() == Some("Params")),
        "the request-tab group announces the active tab"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Group && n.value.as_deref() == Some("Pretty")),
        "the response-view group announces the active view"
    );
    // A saved-request row is an addressable, labelled button.
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Button && n.name.as_deref() == Some("GET List users")),
        "a saved request is a focusable, labelled row"
    );
    // A collection header exposes its expanded state.
    let coll = nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Users API, collection, 4 requests"))
        .expect("the collection header is an accessible row");
    assert_eq!(
        coll.state.expanded,
        Some(true),
        "an expanded collection reports it"
    );

    for name in [
        "New collection",
        "Save request",
        "Collection actions: Users API",
        "Request actions: List users",
        "Environment actions: Production",
    ] {
        assert!(
            nodes
                .iter()
                .any(|node| node.role == Role::Button && node.name.as_deref() == Some(name)),
            "visible workspace-management control {name:?} is an accessible button"
        );
    }
    assert!(
        !nodes.iter().any(|node| node.role == Role::TextInput
            && matches!(
                node.name.as_deref(),
                Some("Collection or environment name" | "Environment variable as key=value")
            )),
        "idle UI has no ambiguous generic management fields"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn empty_and_error_response_surfaces_expose_recovery_actions() {
    let empty = Driver::new(State::new(), Box::new(ApiTesterApp::new()), (1280, 800));
    assert!(
        empty.focus_access_nodes().iter().any(|node| {
            node.role == Role::Group
                && node.name.as_deref() == Some("No response yet")
                && node.children.iter().any(|child| {
                    child.role == Role::Button && child.name.as_deref() == Some("Send request")
                })
        }),
        "the empty response state exposes its primary Send action"
    );

    let mut app = ApiTesterApp::new();
    app.shot_error();
    let error = Driver::new(State::new(), Box::new(app), (390, 844));
    assert!(
        error.focus_access_nodes().iter().any(|node| {
            node.role == Role::Group
                && node.name.as_deref() == Some("Request failed")
                && node.children.iter().any(|child| {
                    child.role == Role::Button && child.name.as_deref() == Some("Retry request")
                })
        }),
        "the error response state exposes a retry action"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn narrow_workspace_menu_is_keyboard_accessible() {
    let d = Driver::new(State::new(), Box::new(ApiTesterApp::new()), (390, 844));
    assert!(
        d.focus_access_nodes().iter().any(|node| {
            node.role == Role::Button && node.name.as_deref() == Some("Open workspace")
        }),
        "the overlay rail has a retained menu button instead of painted-only chrome"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn contextual_forms_expose_only_their_named_fields() {
    let mut variable = ApiTesterApp::new_demo();
    variable.shot_form("new-variable");
    variable.snap_rail(true);
    let variable = Driver::new(State::new(), Box::new(variable), (390, 844));
    let nodes = variable.focus_access_nodes();
    for name in ["Variable key", "Variable value", "Cancel", "Confirm"] {
        assert!(
            nodes.iter().any(|node| node.name.as_deref() == Some(name)),
            "{name}"
        );
    }
    assert!(
        !nodes
            .iter()
            .any(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Name")),
        "variable form has distinct Key and Value fields, not an irrelevant Name input"
    );

    let mut rename = ApiTesterApp::new_demo();
    rename.shot_form("rename-request");
    let rename = Driver::new(State::new(), Box::new(rename), (1280, 800));
    assert!(
        rename
            .focus_access_nodes()
            .iter()
            .any(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Name")),
        "rename form has its single named field"
    );
}

/// Tab order, Enter activation, Escape dismissal and focus return drive the
/// real widget handlers; each case mounts its own Driver.
#[test]
fn keyboard_focus_traverses_activates_and_returns_through_real_handlers() {
    tab_traverses_every_control_in_order();
    enter_activates_a_focused_row_through_the_real_handler();
    composition_disclosure_hides_maintenance_and_returns_focus();
    composition_narrow_escape_paging_and_open_resize_are_reachable();
}

/// Tab / Shift+Tab move keyboard focus between the real controls in tree
/// order — the traversal the compositor's `App` performs, exercised here
/// through the same focus walk.
/// Case of `keyboard_focus_traverses_activates_and_returns_through_real_handlers`.
fn tab_traverses_every_control_in_order() {
    let mut d = driver();
    let stops = d.tab_through();

    // The builder chrome comes first, top-left to top-right: method picker,
    // URL field, Send, the explicit Save affordance, then the request-tab
    // group and its contextual action menu.
    let roles: Vec<Role> = stops.iter().map(|(r, _)| *r).collect();
    assert_eq!(
        roles.first(),
        Some(&Role::ComboBox),
        "first stop is the method picker"
    );
    assert_eq!(
        roles.get(1),
        Some(&Role::TextInput),
        "second stop is the URL field"
    );
    assert_eq!(roles.get(2), Some(&Role::Button), "third stop is Send");
    assert_eq!(stops[2].1.as_deref(), Some("Send"));
    assert_eq!(roles.get(3), Some(&Role::Button), "fourth stop is Save");
    assert_eq!(stops[3].1.as_deref(), Some("Save"));
    assert_eq!(
        roles.get(4),
        Some(&Role::Group),
        "fifth stop is the request tabs"
    );
    assert_eq!(
        roles.get(5),
        Some(&Role::ComboBox),
        "sixth stop is the request actions menu"
    );

    // Every focusable control is reached exactly once.
    assert_eq!(
        stops.len(),
        d.focus_order().len(),
        "Tab visits every focusable node"
    );

    // Focus reaches into the sidebar rows.
    assert!(
        stops
            .iter()
            .any(|(r, n)| *r == Role::Button && n.as_deref() == Some("GET List users")),
        "Tab reaches the saved-request rows"
    );

    // Shift+Tab reverses.
    d.set_modifiers(&["shift".into()]);
    let back = d.tab(true);
    assert!(back.is_some(), "Shift+Tab moves focus backward");
}

/// Enter on a focused row activates it through the widget's real handler —
/// the same path a screen reader's "activate" takes — and the effect lands in
/// the app (the collection collapses, hiding its request rows).
/// Case of `keyboard_focus_traverses_activates_and_returns_through_real_handlers`.
fn enter_activates_a_focused_row_through_the_real_handler() {
    let mut d = driver();
    let target = "Users API, collection, 4 requests";

    // Tab until the collection header holds focus.
    let mut reached = false;
    for _ in 0..=d.focus_order().len() {
        d.tab(false);
        if d.focused_access_node().and_then(|n| n.name).as_deref() == Some(target) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab can land on the collection header");

    // Expanded: its request rows are present.
    let before = d.focus_access_nodes();
    assert!(
        before
            .iter()
            .any(|n| n.name.as_deref() == Some("GET List users")),
        "before activation the collection is expanded"
    );

    // Activate with the keyboard.
    d.key("Enter");

    // Collapsed: the request rows are gone and the header reports it.
    let after = d.focus_access_nodes();
    assert!(
        !after
            .iter()
            .any(|n| n.name.as_deref() == Some("GET List users")),
        "Enter collapsed the collection, hiding its request rows"
    );
    let coll = after
        .iter()
        .find(|n| n.name.as_deref() == Some(target))
        .expect("the collection header is still present");
    assert_eq!(
        coll.state.expanded,
        Some(false),
        "the header now reports collapsed"
    );
}

/// Case of `keyboard_focus_traverses_activates_and_returns_through_real_handlers`.
fn composition_disclosure_hides_maintenance_and_returns_focus() {
    let mut d = driver();
    assert!(
        !d.targets().contains_key("api.rename-request"),
        "maintenance is hidden by default"
    );
    assert!(d.targets()["api.nav.1"].y0 < d.targets()["api.request-actions"].y0);
    d.click_named("api.request-actions").unwrap();
    assert!(d.targets().contains_key("api.rename-request"));
    assert!(!d.targets().contains_key("api.new-variable"));
    d.perform_named("api.rename-request", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Escape");
    assert!(!d.targets().contains_key("api.rename-request"));
    assert!(
        d.focused_access_node()
            .unwrap()
            .name
            .unwrap()
            .starts_with("Request actions")
    );
}

/// The request divider responds to keyboard and pointer, its manual
/// preference survives resizes and tab changes, and 2x text keeps every
/// control on screen; each case mounts its own Driver.
#[test]
fn request_divider_and_controls_survive_resize_and_text_scale() {
    composition_params_fit_rows_and_divider_adjusts_with_keyboard();
    composition_pointer_drag_body_auth_and_preference_survive_resize();
    composition_real_two_x_retains_all_controls_and_response_space();
}

/// Case of `request_divider_and_controls_survive_resize_and_text_scale`.
fn composition_params_fit_rows_and_divider_adjusts_with_keyboard() {
    let mut d = driver();
    let before = d.targets()["api.request-divider"];
    assert!(
        before.y0 < 310.0,
        "three parameter rows fit above the response"
    );
    d.perform_named("api.request-divider", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("ArrowDown");
    assert!(d.targets()["api.request-divider"].y0 > before.y0);
    d.resize((800, 480)).unwrap();
    assert!(d.targets()["api.request-divider"].y1 < 380.0);
    d.resize((1280, 800)).unwrap();
    assert!(d.targets()["api.request-divider"].y0 > before.y0);
    d.click_named("api.request-actions").unwrap();
    d.click_named("api.reset-request-area").unwrap();
    assert_eq!(d.targets()["api.request-divider"], before);
}

fn click_tab(d: &mut Driver, index: u32) {
    let rect = d.targets()["api.request-tabs"];
    d.click_at(
        rect.x0 + rect.width() * (f64::from(index) + 0.5) / 4.0,
        rect.center().y,
    );
}

fn move_pointer(d: &mut Driver, x: f64, y: f64) {
    d.send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    });
}

fn assert_sidebar_targets(d: &Driver, height: f64) {
    let targets = d.targets();
    let mut sidebar = Vec::new();
    for (id, rect) in &targets {
        if id.starts_with("api.nav.")
            || [
                "api.env",
                "api.new-collection",
                "api.save-request",
                "api.collection-actions",
                "api.request-actions",
                "api.environment-actions",
                "api.rename-request",
                "api.delete-request",
                "api.increase-request-area",
                "api.decrease-request-area",
                "api.reset-request-area",
                "api.new-environment",
                "api.rename-environment",
                "api.delete-environment",
                "api.new-variable",
                "api.delete-variable",
                "api.previous-saved",
                "api.next-saved",
            ]
            .contains(&id.as_str())
        {
            assert!(
                rect.x0 >= 0.0
                    && rect.y0 >= 0.0
                    && rect.y1 <= height
                    && rect.width() > 0.0
                    && rect.height() > 0.0,
                "{id}: {rect:?} inside {height}"
            );
            for (other, other_rect) in &sidebar {
                assert!(
                    rect.intersect(*other_rect).area() <= 0.0,
                    "{id} overlaps {other}"
                );
            }
            sidebar.push((id, *rect));
        }
    }
}

/// Case of `request_divider_and_controls_survive_resize_and_text_scale`.
#[expect(
    clippy::float_cmp,
    reason = "the divider moves by exactly the dragged whole-pixel distance"
)]
fn composition_pointer_drag_body_auth_and_preference_survive_resize() {
    let mut d = driver();
    let divider = d.targets()["api.request-divider"];
    move_pointer(&mut d, divider.center().x, divider.center().y);
    d.send(&winit::event::WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: winit::event::ElementState::Pressed,
        button: winit::event::MouseButton::Left,
    });
    move_pointer(&mut d, divider.center().x, divider.center().y + 200.0);
    d.send(&winit::event::WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: winit::event::ElementState::Released,
        button: winit::event::MouseButton::Left,
    });
    let manual = d.targets()["api.request-divider"];
    assert_eq!(manual.y0, divider.y0 + 200.0);
    d.click_named("api.nav.3").unwrap();
    click_tab(&mut d, 3);
    assert!(d.targets().contains_key("api.body"));
    assert_eq!(d.targets()["api.request-divider"], manual);
    d.click_named("api.body").unwrap();
    d.type_text("x");
    assert_eq!(d.targets()["api.request-divider"], manual);
    click_tab(&mut d, 2);
    assert!(!d.targets().contains_key("api.body"));
    assert_eq!(d.targets()["api.request-divider"], manual);
    let modes = d.targets()["api.response-view"];
    d.click_at(modes.x0 + modes.width() * 5.0 / 6.0, modes.center().y);
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|node| node.value.as_deref() == Some("Preview"))
    );
    d.resize((800, 480)).unwrap();
    assert!(d.targets()["api.request-divider"].y1 <= 380.0);
    d.resize((1280, 800)).unwrap();
    assert_eq!(d.targets()["api.request-divider"], manual);
}

/// Real management buttons and Send (pointer and keyboard) drive the same
/// mutations and requests as the live app; each case mounts its own Driver.
#[test]
fn real_buttons_perform_canonical_mutations_and_live_sends() {
    composition_management_real_buttons_keep_canonical_mutations();
    composition_pointer_and_keyboard_send_use_the_live_request();
}

/// Case of `real_buttons_perform_canonical_mutations_and_live_sends`.
fn composition_management_real_buttons_keep_canonical_mutations() {
    let mut d = driver();
    d.perform_named("api.collection-actions", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Enter");
    d.click_named("api.rename-collection").unwrap();
    assert!(d.targets().contains_key("api.form-name"));
    d.click_named("api.form-name").unwrap();
    d.key("Home");
    d.type_text("Edited ");
    d.click_named("api.form-confirm").unwrap();
    assert!(
        d.focus_access_nodes().iter().any(|n| n
            .name
            .as_deref()
            .is_some_and(|s| s.contains("Edited") && s.contains("collection"))),
        "{}",
        d.dump_json()
    );
    d.click_named("api.environment-actions").unwrap();
    d.click_named("api.new-variable").unwrap();
    d.click_named("api.form-variable-key").unwrap();
    d.type_text("region");
    d.click_named("api.form-variable-value").unwrap();
    d.type_text("eu-west-1");
    d.click_named("api.form-confirm").unwrap();
    let json = d.dump_json();
    assert!(
        json.contains("eu-west-1") && json.contains("region"),
        "{json}"
    );
}

/// Case of `keyboard_focus_traverses_activates_and_returns_through_real_handlers`.
fn composition_narrow_escape_paging_and_open_resize_are_reachable() {
    let mut d = driver();
    d.resize((480, 900)).unwrap();
    d.perform_named("api.workspace-menu", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Enter");
    // Settle the drawer's existing animation through real redraw ticks.
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        d.relayout();
    }
    d.click_named("api.environment-actions").unwrap();
    assert_sidebar_targets(&d, 900.0);
    d.resize((800, 480)).unwrap();
    assert_sidebar_targets(&d, 480.0);
    d.key("Escape");
    assert!(!d.targets().contains_key("api.new-variable"));
    d.click_named("api.next-saved").unwrap();
    assert!(
        !d.targets()
            .into_keys()
            .filter(|id| id.starts_with("api.nav."))
            .any(|id| id == "api.nav.0")
    );
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|node| node.name.as_deref() == Some("POST Login"))
    );
    d.perform_named("api.nav.5", &ui::a11y::Action::Focus)
        .unwrap();
    assert_eq!(
        d.focused_access_node().unwrap().name.as_deref(),
        Some("Auth, collection, 2 requests")
    );
    d.key("Enter");
    assert!(
        !d.focus_access_nodes()
            .iter()
            .any(|node| node.name.as_deref() == Some("POST Login")),
        "the paged collection row toggled its own children"
    );
    d.resize((480, 900)).unwrap();
    d.key("Escape");
    assert!(!d.targets().contains_key("api.env"));
    assert_eq!(
        d.focused_access_node().unwrap().name.as_deref(),
        Some("Open workspace")
    );
}

/// Case of `request_divider_and_controls_survive_resize_and_text_scale`.
fn composition_real_two_x_retains_all_controls_and_response_space() {
    let mut app = ApiTesterApp::new_demo();
    app.snap_rail(true);
    let mut d = Driver::new_with_theme(
        State::new(),
        Box::new(app),
        (800, 900),
        ui::theme::Theme::dark().with_text_scale(2.0),
    );
    assert_sidebar_targets(&d, 900.0);
    d.click_named("api.environment-actions").unwrap();
    assert_sidebar_targets(&d, 900.0);
    let targets = d.targets();
    assert!(targets["api.request-divider"].y1 < 700.0);
    assert!(targets["api.request-tabs"].x1 <= 800.0);
}

/// Case of `real_buttons_perform_canonical_mutations_and_live_sends`.
fn composition_pointer_and_keyboard_send_use_the_live_request() {
    use std::io::{Read, Write};
    for keyboard in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = tilo_test_env::hang_deadline();
            loop {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = [0; 4096];
                    let count = stream.read(&mut bytes).unwrap();
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                    return String::from_utf8_lossy(&bytes[..count]).to_string();
                }
                assert!(
                    ui::time::now() < deadline,
                    "Send never reached the HTTP server"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let mut d = driver();
        d.perform_named(
            "api.url",
            &ui::a11y::Action::SetValue(format!("http://{address}/verify")),
        )
        .unwrap();
        if keyboard {
            d.perform_named("api.send", &ui::a11y::Action::Focus)
                .unwrap();
            d.key("Enter");
        } else {
            d.click_named("api.send").unwrap();
        }
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /verify?page=1&per_page=25 HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer prod_sk_")
        );
    }
}

/// Every effect-capable control is bound to an action: the sidebar
/// disclosures and the request-area divider publish semantic action targets
/// of their own, in the empty workspace and in the populated demo.
#[test]
fn native_app_contract_is_certified() {
    for app in [ApiTesterApp::new, ApiTesterApp::new_demo] {
        let d = Driver::new(State::new(), Box::new(app()), (1280, 800));
        let audit = d.native_app_audit();
        assert!(audit.is_certified(), "{audit:#?}");
    }
}
