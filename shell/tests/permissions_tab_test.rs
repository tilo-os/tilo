//! Sandbox Phase 6 — UI integration tests for the WS-Settings
//! Permissions tab.
//!
//! These tests exercise the chrome ↔ overlay-host wiring end-to-end:
//!
//!   1. Seed `Workspace::grants` with one or more capabilities.
//!   2. Open the Workspace Settings modal at the Permissions tab.
//!   3. Render once so `draw_ws_settings` populates the cached
//!      revoke-button hit rects.
//!   4. Read the rects via the test-only
//!      `permissions_revoke_rects()` accessor and synthesize a click
//!      on one.
//!   5. Assert the workspace's grants map has been mutated (or, for
//!      the non-owner case, has *not* been mutated).
//!
//! The test harness mirrors `e2e_shell_flows.rs` and
//! `concept_parity_spec.rs` — full build/layout/draw cycle with no
//! GPU.

use std::collections::HashMap;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use shell::shell::build_nirios_overlay_host_with_chrome;
use shell::shell::chrome_state::{Chrome, WsTab};
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::{
    Capability, GrantScope, Workspace, WorkspaceId, WorkspaceTree,
};
use shell::shell::workspace_settings;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

const W: f32 = 1600.0;
const H: f32 = 900.0;

fn synth_device() -> DeviceId {
    // SAFETY: `DeviceId` is opaque; integration-test scaffolding
    // already uses this pattern in
    // `tests/permission_prompt_pump.rs` and several other shell
    // integration tests.
    unsafe { std::mem::zeroed() }
}

fn cursor_at(p: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: synth_device(),
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    }
}

fn left_press() -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: synth_device(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    }
}

/// Build a tree with a single workspace that holds the supplied grants.
/// Mirrors `permission_prompt_pump.rs::chrome_with_pending_grant`'s
/// shape so test setup stays compact.
fn tree_with_grants(entries: Vec<(&str, Vec<Capability>)>) -> WorkspaceTree {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    let mut ws = Workspace::new(id, "perm-test");
    for (app_id, caps) in entries {
        ws.grants.insert(app_id.to_string(), caps);
    }
    tree.workspaces.push(ws);
    tree.set_active(id);
    tree
}

/// Render the host once with the chrome configured for the
/// Permissions tab. Returns the produced Scene + the host so the
/// caller can read back hit rects and dispatch synthesized events.
fn render_at_permissions_tab(
    tree: &WorkspaceTree,
) -> (ShellOverlayHost, Chrome, Scene) {
    let (mut host, chrome) = build_nirios_overlay_host_with_chrome(tree);
    {
        let mut st = chrome.borrow_mut();
        st.overlays.ws_settings = true;
        st.ws_settings_tab = WsTab::Permissions;
    }
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root_node = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(W),
                    height: Dimension::length(H),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    let mut tr = TextRenderer::new();
    let measure_map = node_map.clone();
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(W),
                height: AvailableSpace::Definite(H),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .expect("compute_layout");
    host.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    let presence = PresenceCtx::default();
    host.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    (host, chrome, scene)
}

/// Forward a `WindowEvent` through the host's handle_event with stub
/// scaffolding. Mirrors the `dispatch` helper in
/// `permission_prompt_pump.rs`.
fn dispatch_to_host(host: &mut ShellOverlayHost, event: &WindowEvent) {
    use automerge::AutoCommit;
    let mut tree = TaffyTree::<()>::new();
    let node = tree.new_leaf(Default::default()).unwrap();
    let mut handled = false;
    let mut focus_target = None;
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    host.handle_event(
        event,
        1.0,
        &winit::event::Modifiers::default(),
        &mut doc,
        &root,
        &mut handled,
        &mut focus_target,
        node,
        &tree,
    );
}

// =====================================================================
// Task 6.1 — rendering
// =====================================================================

#[test]
fn permissions_tab_renders_zero_rows_when_no_grants() {
    // Empty grants ⇒ the rect-cache populated by `draw_ws_settings`
    // for revoke buttons is empty, even after the modal renders.
    let tree = tree_with_grants(vec![]);
    let (host, _chrome, _scene) = render_at_permissions_tab(&tree);
    assert!(
        host.permissions_revoke_rects().is_empty(),
        "no grants ⇒ no revoke-button rects, got {:?}",
        host.permissions_revoke_rects().len()
    );
}

#[test]
fn permissions_tab_renders_one_row_per_capability_per_app() {
    // Two capabilities for one app ⇒ two revoke-button rects in the
    // cache. Each rect must be non-zero (the renderer painted the
    // button at a real pixel position).
    let tree = tree_with_grants(vec![(
        "firefox",
        vec![
            Capability::Camera {
                scope: GrantScope::Always,
            },
            Capability::Network {
                hosts: None,
                scope: GrantScope::Session,
            },
        ],
    )]);
    let (host, _chrome, _scene) = render_at_permissions_tab(&tree);
    let rects = host.permissions_revoke_rects();
    assert_eq!(
        rects.len(),
        2,
        "expected 2 revoke rects for 2 capabilities, got {}",
        rects.len()
    );
    for (_app, _idx, r) in rects {
        assert!(r.area() > 0.0, "revoke rect must have non-zero area");
    }
}

// =====================================================================
// Task 6.1 — revoke flow
// =====================================================================

#[test]
fn revoke_click_removes_capability_from_grants() {
    // Click on the first row's revoke button → the mapped
    // (app_id, cap_index) is removed from the workspace's grants.
    let tree = tree_with_grants(vec![(
        "firefox",
        vec![
            Capability::Camera {
                scope: GrantScope::Always,
            },
            Capability::Network {
                hosts: None,
                scope: GrantScope::Session,
            },
        ],
    )]);
    let (mut host, chrome, _scene) = render_at_permissions_tab(&tree);
    let rects = host.permissions_revoke_rects().to_vec();
    assert_eq!(rects.len(), 2);
    let target = rects[0].clone();
    let target_app = target.0.clone();
    let target_idx = target.1;
    let click_point = target.2.center();

    // Synthesize the click — cursor move first so `last_cursor` is
    // populated, then a left-press lands on the button rect.
    dispatch_to_host(&mut host, &cursor_at(click_point));
    dispatch_to_host(&mut host, &left_press());

    let st = chrome.borrow();
    let tree = st.tree(); let ws = tree.active_workspace().expect("active workspace");
    // After revoking one of two grants, the app entry still exists
    // with one capability remaining.
    let firefox = ws.grants.get(&target_app);
    if target_idx == 0 {
        // Revoked the Camera grant ⇒ Network remains.
        let caps = firefox.expect("firefox entry must remain after partial revoke");
        assert_eq!(caps.len(), 1, "1 of 2 grants must remain after revoke");
        assert!(matches!(caps[0], Capability::Network { .. }));
    } else {
        // Revoked the Network grant ⇒ Camera remains.
        let caps = firefox.expect("firefox entry must remain after partial revoke");
        assert_eq!(caps.len(), 1);
        assert!(matches!(caps[0], Capability::Camera { .. }));
    }
}

// =====================================================================
// Task 6.2 — owner-only enforcement
// =====================================================================

#[test]
fn revoke_button_disabled_for_non_owner() {
    // Flip the chrome's test-only override so
    // `is_active_workspace_owner` returns `false`. With that the
    // click router consumes the click (so the modal stays open + the
    // name-edit isn't committed) but skips the cache mutation —
    // grants must be unchanged after the click.
    let tree = tree_with_grants(vec![(
        "firefox",
        vec![Capability::Camera {
            scope: GrantScope::Always,
        }],
    )]);
    let (mut host, chrome, _scene) = render_at_permissions_tab(&tree);
    // Force non-owner *after* the first render so the rect cache is
    // already populated — the test exercises the click-router's
    // gating, not the renderer's.
    chrome.borrow_mut().test_only_owner_override = Some(false);
    let rects = host.permissions_revoke_rects().to_vec();
    assert_eq!(rects.len(), 1);
    let click_point = rects[0].2.center();
    dispatch_to_host(&mut host, &cursor_at(click_point));
    dispatch_to_host(&mut host, &left_press());

    let st = chrome.borrow();
    let tree = st.tree(); let ws = tree.active_workspace().expect("active workspace");
    let caps = ws
        .grants
        .get("firefox")
        .expect("non-owner click must NOT remove the grant");
    assert_eq!(
        caps.len(),
        1,
        "non-owner click on Revoke must be a no-op; got {} caps",
        caps.len()
    );
}

// =====================================================================
// Task 6.1 — WsTab visibility
// =====================================================================

#[test]
fn permissions_tab_appears_in_ws_tab_iter() {
    // Sandbox Phase 6 — `WsTab::all()` must include `Permissions`
    // so the focus trap and sidebar nav can reach it.
    let all = WsTab::all();
    assert!(
        all.contains(&WsTab::Permissions),
        "WsTab::all() must include Permissions; got {:?}",
        all
    );
    // And the visibility helper exposes Permissions exactly when
    // grants are non-empty.
    let mut empty_ws = Workspace::new(WorkspaceId::default(), "empty");
    assert!(!WsTab::visible(&empty_ws).contains(&WsTab::Permissions));
    empty_ws.grants.insert(
        "vscode".into(),
        vec![Capability::Camera {
            scope: GrantScope::Once,
        }],
    );
    assert!(WsTab::visible(&empty_ws).contains(&WsTab::Permissions));
}

// =====================================================================
// Task 6.1 — empty-grants cleanup
// =====================================================================

#[test]
fn revoking_last_grant_clears_workspace_grants_map() {
    // After revoking the only grant for an app, the map entry is
    // cleaned up so `Workspace::grants.is_empty()` flips back to true
    // — which in turn flips `WsTab::visible` to drop the Permissions
    // tab from the strip. Verifies the
    // `revoke_grant` ↔ `WsTab::visible` contract end-to-end.
    let mut ws = Workspace::new(WorkspaceId::default(), "single-grant");
    ws.grants.insert(
        "vscode".into(),
        vec![Capability::Camera {
            scope: GrantScope::Once,
        }],
    );
    assert!(WsTab::visible(&ws).contains(&WsTab::Permissions));
    workspace_settings::revoke_grant(&mut ws, &"vscode".to_string(), 0);
    assert!(ws.grants.is_empty());
    assert!(!WsTab::visible(&ws).contains(&WsTab::Permissions));
}
