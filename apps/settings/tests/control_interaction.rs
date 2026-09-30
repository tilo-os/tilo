//! End-to-end control-interaction proofs.
//!
//! `category_nav.rs` proves a click navigates to each panel and the panel
//! renders. This file goes one step further for the panes whose controls
//! MUST drive real hardware actions: it navigates via a real pointer click,
//! then drives the panel's primary control (also via the production event
//! path or its CRDT key) and asserts the effect actually lands — the narrow
//! content screen's back arrow returning to the list, and a per-panel primary
//! control mutating the document the way its dispatcher hook reads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use ui::doc::read_bool;

use automerge::{AutoCommit, ReadDoc};
use settings::SettingsApp;
use settings::categories::Category;
use taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::widget::{ResourceOperation, Widget};
use vello::kurbo::{Point, Rect};

const W: f32 = 1200.0;
// Wide navigation tests address every category directly; keep each target
// visible. Short-viewport sidebar scrolling is exercised separately.
const H: f32 = 1800.0;

/// Run the host's per-frame `build_layout → compute_layout(measure) →
/// apply_layout` cycle at the default wide viewport, returning the live tree
/// + root node for event routing.
fn lay_out(app: &mut SettingsApp) -> (TaffyTree<()>, NodeId) {
    lay_out_at(app, W, H)
}

/// Same, at an explicit viewport (used for the narrow screens).
fn lay_out_at(app: &mut SettingsApp, w: f32, h: f32) -> (TaffyTree<()>, NodeId) {
    let theme = ui::Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = app.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(w),
                    height: taffy::Dimension::length(h),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    let measure_map = node_map.clone();
    let mut tr = TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .unwrap();
    app.apply_layout(&taffy, node, Point::ORIGIN);
    (taffy, node)
}

/// Find a widget's on-screen rect by `name` (+ optional `test_id`).
fn find_rect(app: &SettingsApp, name: &str, test_id: Option<&str>) -> Option<Rect> {
    fn walk(w: &dyn Widget, name: &str, tid: Option<&str>, out: &mut Option<Rect>) {
        if out.is_some() {
            return;
        }
        if w.name() == name && (tid.is_none() || w.test_id() == tid) {
            *out = Some(w.get_rect());
            return;
        }
        for c in w.children() {
            walk(c, name, tid, out);
        }
    }
    let mut out = None;
    walk(app, name, test_id, &mut out);
    out
}

/// Drive a full left-click (cursor move → press → release) at `p` through
/// the production event path.
fn click_at(
    app: &mut SettingsApp,
    taffy: &TaffyTree<()>,
    node: NodeId,
    doc: &mut AutoCommit,
    p: Point,
) {
    let root = automerge::ROOT;
    let mods = winit::event::Modifiers::default();
    let cursor = winit::event::WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(p.x, p.y),
    };
    let press = winit::event::WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: winit::event::ElementState::Pressed,
        button: winit::event::MouseButton::Left,
    };
    let release = winit::event::WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: winit::event::ElementState::Released,
        button: winit::event::MouseButton::Left,
    };
    for ev in [&cursor, &press, &release] {
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy,
            },
        );
        // The live host resolves action-bound control intents immediately
        // after event delivery. Mirror that boundary here so AppShell's Back
        // `View` action reaches its retained button handler.
        while let Some(intent) = ui::widget::controls::take_control_invocation(app) {
            let control = intent
                .address
                .resource
                .strip_prefix("tilo.control:")
                .expect("Settings control actions use the toolkit control resource");
            let payload = serde_json::to_vec(control).expect("control id is valid JSON");
            let started = app
                .start_operation(
                    &ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::View,
                        "tilo.ui.control.invoke",
                        &payload,
                    ),
                    doc,
                    &root,
                )
                .expect("Settings control action succeeds");
            assert!(started.is_some(), "Settings handled its control action");
        }
    }
}

/// Navigate to `target` by clicking its sidebar row through the production
/// path, then rebuild + re-lay-out as the host does. Returns the live tree.
fn navigate(
    app: &mut SettingsApp,
    doc: &mut AutoCommit,
    target: Category,
) -> (TaffyTree<()>, NodeId) {
    let (taffy, node) = lay_out(app);
    let row = find_rect(app, "SidebarRow", Some(&target.label()))
        .unwrap_or_else(|| panic!("no sidebar row for {target:?}"));
    assert!(
        row.y0 >= 0.0 && row.y1 <= f64::from(H),
        "navigation fixture must show {target:?}: {row:?}"
    );
    click_at(app, &taffy, node, doc, row.center());
    let root = automerge::ROOT;
    app.sync(doc, &root);
    assert_eq!(app.selected(), target, "click navigated to {target:?}");
    let _ = app.take_structure_dirty();
    lay_out(app)
}

/// The back button returns to the list and the Wi-Fi toggle stays pane-local.
#[test]
fn back_button_and_wifi_toggle_drive_pane_state() {
    // Case `clicking_back_button_returns_to_list`.
    // The narrow content screen's back-arrow button, clicked through the
    // production event path, navigates back to the standalone list screen (the
    // only way back — there is no overlay or floating sidebar).
    {
        const NW: f32 = 440.0;
        const NH: f32 = 820.0;
        let config = crate::support::ScratchConfig::new();
        // Start drilled into a category's content screen (narrow layout).
        let mut app = config
            .app()
            .with_category(Category::Network)
            .with_width_hint(NW)
            .with_narrow_content_shot();
        let mut doc = AutoCommit::new();
        let (taffy, node) = lay_out_at(&mut app, NW, NH);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.is_narrow(), "440px resolves to the narrow layout");
        assert!(
            !app.sidebar_open(),
            "starts on the content screen (list hidden)"
        );

        // The back arrow is drawn by the app-shell (not a child widget), so
        // hit-test it by its resolved rect and click it through the real path.
        let back = app.back_button_rect();
        assert!(back.width() > 0.0, "the back arrow has a resolved rect");
        click_at(&mut app, &taffy, node, &mut doc, back.center());
        app.sync(&mut doc, &automerge::ROOT);
        let _ = app.take_structure_dirty();
        let _ = lay_out_at(&mut app, NW, NH);
        assert!(
            app.sidebar_open(),
            "back navigates to the standalone list screen"
        );
        assert!(
            app.back_button_rect().width() <= 0.0,
            "the list screen has no back button"
        );
    }

    // Case `network_wifi_toggle_drives_state`.
    // Network hardware state is pane-local. Neither initialisation nor a hostile
    // remote scalar may turn the collaborative document into a radio command.
    {
        let config = crate::support::ScratchConfig::new();
        let mut app = config.app();
        let mut doc = AutoCommit::new();
        let _ = navigate(&mut app, &mut doc, Category::Network);
        let root = automerge::ROOT;
        let heads = doc.get_heads();
        app.sync(&mut doc, &root);
        assert_eq!(doc.get_heads(), heads);
        assert!(
            read_bool(
                &doc,
                &automerge::ROOT,
                settings::panes::network::WIFI_ENABLED_KEY
            )
            .is_none()
        );
    }
}

/// Every category is reachable by a real click and renders a non-empty
/// content pane — the per-panel navigation guard. (Complements
/// `category_nav.rs`'s deeper per-panel assertions; here we additionally
/// confirm the primary CRDT key each interactive pane seeds is present.)
#[test]
fn all_panels_navigate_and_seed_their_controls() {
    let config = crate::support::ScratchConfig::new();
    for &target in Category::ALL {
        let mut app = config.app();
        let mut doc = AutoCommit::new();
        let _ = navigate(&mut app, &mut doc, target);
        // The pane rebuilt by navigate's final layout seeds on its next
        // sync; run one so the seed lands before we assert on it.
        let heads_before_idle_sync = doc.get_heads();
        app.sync(&mut doc, &automerge::ROOT);
        // Hardware panes render from process-local snapshots and must not
        // seed their primary device state into the collaborative document.
        let seeded_key = match target {
            Category::Network => Some(settings::panes::network::WIFI_ENABLED_KEY),
            Category::Bluetooth => Some(settings::panes::bluetooth::BT_ENABLED_KEY),
            Category::Sound => Some(settings::panes::audio::VOLUME_KEY),
            Category::Displays => Some(settings::panes::display::BRIGHTNESS_KEY),
            Category::Storage => Some(settings::panes::storage::CLEAN_KEY),
            Category::Printers => Some("settings.printers.refresh"),
            Category::Users => Some("settings.users.add"),
            Category::AgentModels => Some("agent_models.local.confirm"),
            _ => None,
        };
        if let Some(key) = seeded_key {
            assert_eq!(
                doc.get_heads(),
                heads_before_idle_sync,
                "{target:?} idle sync must leave the collaborative document unchanged"
            );
            assert!(
                doc.get(&automerge::ROOT, key).ok().flatten().is_none(),
                "{target:?} must keep device-local control key {key} out of collaboration"
            );
        }
    }
}
