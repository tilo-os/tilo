//! Click-driven navigation proof.
//!
//! This is the regression guard for the bug where clicking a sidebar category
//! did NOT switch the content pane. It drives a *real pointer click* — a
//! `CursorMoved` to the row's computed centre, then `MouseInput` press +
//! release — through `SettingsApp::handle_event` (the production event path),
//! then runs the production `sync` and the host's structure-dirty rebuild, and
//! asserts:
//!
//! 1. the selected category became the clicked one (read back from the app,
//!    NOT set directly — that would be tautological);
//! 2. the rebuilt sidebar marks exactly that category's row active; and
//! 3. the content pane for the new category actually renders.
//!
//! A test runs per category, so every entry is exercised end to end.

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

use automerge::AutoCommit;
use settings::SettingsApp;
use settings::categories::Category;
use taffy::{NodeId, TaffyTree};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

// `Rect`/`Scene`/`Affine` are used by the layout + render helpers below.

const W: f32 = 1200.0;
// Tall enough for the complete category corpus. Sidebar scrolling itself is
// covered by Scrollable's integration tests; this suite exercises category
// activation and must click visible rows rather than coordinates below clip.
const H: f32 = 1800.0;

/// Run the full `build_layout → compute_layout(measure) → apply_layout` cycle
/// the host runs each frame, returning the app's root taffy node + the live
/// tree so events can be routed exactly as production does.
fn lay_out(app: &mut SettingsApp) -> (TaffyTree<()>, NodeId) {
    let theme = ui::Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = app.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(W),
                    height: taffy::Dimension::length(H),
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
                width: taffy::AvailableSpace::Definite(W),
                height: taffy::AvailableSpace::Definite(H),
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

/// Walk the live tree for the `SidebarRow` carrying `label`, returning its
/// computed on-screen rect.
fn row_rect(app: &SettingsApp, label: &str) -> Option<Rect> {
    fn walk(w: &dyn Widget, label: &str, out: &mut Option<Rect>) {
        if w.name() == "SidebarRow" && w.test_id() == Some(label) {
            *out = Some(w.get_rect());
        }
        for c in w.children() {
            walk(c, label, out);
        }
    }
    let mut out = None;
    walk(app, label, &mut out);
    out
}

/// Is the `SidebarRow` for `label` currently drawn active? Read from the
/// row's `get_state` snapshot in the live tree.
fn row_is_active(app: &SettingsApp, label: &str) -> bool {
    fn walk(w: &dyn Widget, label: &str, out: &mut bool) {
        if w.name() == "SidebarRow" && w.test_id() == Some(label) {
            *out = w.get_state().as_deref() == Some("active");
        }
        for c in w.children() {
            walk(c, label, out);
        }
    }
    let mut out = false;
    walk(app, label, &mut out);
    out
}

/// Render the laid-out app and report whether the scene encoded any draw
/// commands — proof the (rebuilt) composition, including the new category's
/// content pane, actually paints rather than collapsing to nothing.
fn renders_something(app: &mut SettingsApp) -> bool {
    let theme = ui::Theme::dark();
    let mut tr = TextRenderer::new();
    let perms = PermissionContext::permissive();
    let presence = PresenceCtx::empty();
    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &presence,
        &perms,
    );
    !scene.encoding().is_empty()
}

/// Count the `SidebarRow` widgets whose snapshot reads "active". After a
/// navigation exactly one row — the target — must be active.
fn active_row_count(app: &SettingsApp) -> usize {
    fn walk(w: &dyn Widget, n: &mut usize) {
        if w.name() == "SidebarRow" && w.get_state().as_deref() == Some("active") {
            *n = n.saturating_add(1);
        }
        for c in w.children() {
            walk(c, n);
        }
    }
    let mut n = 0;
    walk(app, &mut n);
    n
}

/// The shared click-and-assert routine: starting from a fresh app on the
/// default category, click `target`'s row through the production path and
/// assert navigation reached it.
fn click_navigates_to(target: Category) {
    let config = crate::support::ScratchConfig::new();
    let mut app = config.app();
    let (taffy, node) = lay_out(&mut app);

    assert_eq!(
        app.selected(),
        Category::DEFAULT,
        "starts on the default category"
    );
    if target == Category::DEFAULT {
        // The default is already selected; navigating to it is a no-op, so
        // this case is covered by every *other* category proving the switch.
        return;
    }

    let mut doc = AutoCommit::new();
    let rect =
        row_rect(&app, target.label()).unwrap_or_else(|| panic!("no sidebar row for {target:?}"));
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "row {target:?} has a real rect"
    );

    let root = automerge::ROOT;
    let mods = winit::event::Modifiers::default();
    let center = rect.center();
    assert!(
        rect.y0 >= 0.0 && rect.y1 <= f64::from(H),
        "navigation fixture must show {target:?}: {rect:?}"
    );

    let cursor = winit::event::WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: winit::dpi::PhysicalPosition::new(center.x, center.y),
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

    // Drive the real pointer click through the production event path.
    for ev in [&cursor, &press, &release] {
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    // The host then runs `sync` (reads the select key written by the row) and
    // polls the structure-dirty flag to decide whether to rebuild.
    app.sync(&mut doc, &root);
    assert_eq!(
        app.selected(),
        target,
        "after click+sync the selection must be {target:?} (read back via the production path)"
    );
    assert!(
        app.take_structure_dirty(),
        "the selection change must request a rebuild so the pane updates"
    );

    // Rebuild exactly as the host does on structure-dirty, then re-lay-out.
    let _ = lay_out(&mut app);

    // The rebuilt sidebar highlights the clicked category and ONLY it.
    assert!(
        row_is_active(&app, target.label()),
        "the {target:?} row must be active after navigating to it"
    );
    assert!(
        !row_is_active(&app, Category::DEFAULT.label()),
        "the previously-active default row must no longer be highlighted"
    );
    assert_eq!(
        active_row_count(&app),
        1,
        "exactly one sidebar row may be active after navigating to {target:?}"
    );

    // The rebuilt composition (with the new category's content pane) paints.
    assert!(
        renders_something(&mut app),
        "the {target:?} content pane must render after navigating to it"
    );
}

macro_rules! nav_test {
    ($name:ident, $cat:expr) => {
        #[test]
        fn $name() {
            click_navigates_to($cat);
        }
    };
}

nav_test!(click_navigates_network, Category::Network);
nav_test!(click_navigates_bluetooth, Category::Bluetooth);
nav_test!(click_navigates_sound, Category::Sound);
nav_test!(click_navigates_displays, Category::Displays);
nav_test!(click_navigates_mouse_keyboard, Category::MouseKeyboard);
nav_test!(click_navigates_appearance, Category::Appearance);
nav_test!(click_navigates_accessibility, Category::Accessibility);
nav_test!(click_navigates_notifications, Category::Notifications);
nav_test!(click_navigates_power, Category::Power);
nav_test!(
    click_navigates_keyboard_shortcuts,
    Category::KeyboardShortcuts
);
nav_test!(click_navigates_time_date, Category::TimeDate);
nav_test!(click_navigates_users, Category::Users);
nav_test!(click_navigates_printers, Category::Printers);
nav_test!(
    click_navigates_external_accounts,
    Category::ExternalAccounts
);
nav_test!(click_navigates_region_formats, Category::RegionFormats);
nav_test!(click_navigates_permissions, Category::Permissions);
nav_test!(click_navigates_updates, Category::Updates);
nav_test!(click_navigates_about, Category::About);

/// A second click on a *different* category after a first proves repeated
/// navigation keeps working (not just the first switch off the default).
#[test]
fn two_consecutive_clicks_navigate() {
    let config = crate::support::ScratchConfig::new();
    let mut app = config.app();
    let (taffy, node) = lay_out(&mut app);
    let mut doc = AutoCommit::new();
    let root = automerge::ROOT;
    let mods = winit::event::Modifiers::default();

    let click = |app: &mut SettingsApp,
                 taffy: &TaffyTree<()>,
                 node,
                 doc: &mut AutoCommit,
                 target: Category| {
        let rect = row_rect(app, target.label()).expect("row exists");
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= f64::from(H),
            "navigation fixture must show {target:?}: {rect:?}"
        );
        let center = rect.center();
        let cursor = winit::event::WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(center.x, center.y),
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
                1.0,
                &mods,
                doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                taffy,
            );
        }
        app.sync(doc, &root);
        assert!(app.take_structure_dirty());
    };

    click(&mut app, &taffy, node, &mut doc, Category::Displays);
    assert_eq!(app.selected(), Category::Displays);
    let (taffy, node) = lay_out(&mut app);

    click(&mut app, &taffy, node, &mut doc, Category::Permissions);
    assert_eq!(app.selected(), Category::Permissions);
    let _ = lay_out(&mut app);
    assert!(row_is_active(&app, Category::Permissions.label()));
    assert!(!row_is_active(&app, Category::Displays.label()));
}
