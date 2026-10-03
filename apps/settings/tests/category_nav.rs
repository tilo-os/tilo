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

use settings::SettingsApp;
use settings::categories::Category;
use ui::headless::{Framed, viewport};
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

/// Mount `app` in the production frame host at the fixture viewport and run
/// its first frame, so events can be routed exactly as production does.
fn lay_out(app: SettingsApp) -> Framed<SettingsApp> {
    Framed::new(ui::State::new(), app, viewport((W, H)), ui::Theme::dark())
}

/// Rebuild and re-lay-out exactly as the host does on structure-dirty.
fn rebuild(app: &mut Framed<SettingsApp>) {
    Framed::harness_mut(app)
        .relayout_themed(viewport((W, H)), ui::Theme::dark())
        .expect("settings frame lays out");
}

/// Drive a real pointer click at `center` through the production event path.
fn click_at(app: &mut Framed<SettingsApp>, center: Point) {
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
    let mods = winit::event::Modifiers::default();
    for ev in [&cursor, &press, &release] {
        Framed::harness_mut(app).dispatch_to_root_with(ev, &mods);
    }
}

/// The host's `sync` against the live document.
fn sync(app: &mut Framed<SettingsApp>) {
    let (app, state) = Framed::split(app);
    app.sync(&mut state.doc, &state.root);
}

/// The on-screen rect of the navigation row carrying `label`.
fn row_rect(app: &SettingsApp, label: &str) -> Option<Rect> {
    crate::support::nav_row(app, label).map(|row| row.rect)
}

/// Is the row for `label` currently drawn selected? Read from the list's
/// published semantics.
fn row_is_active(app: &SettingsApp, label: &str) -> bool {
    crate::support::nav_row(app, label).is_some_and(|row| row.selected)
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

/// Count the selected navigation rows. After a navigation exactly one row, the
/// target, must be selected.
fn active_row_count(app: &SettingsApp) -> usize {
    crate::support::nav_rows(app)
        .iter()
        .filter(|row| row.selected)
        .count()
}

/// The shared click-and-assert routine: starting from a fresh app on the
/// default category, click `target`'s row through the production path and
/// assert navigation reached it.
fn click_navigates_to(target: Category) {
    let config = crate::support::ScratchConfig::new();
    let mut app = lay_out(config.app());

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

    let rect =
        row_rect(&app, &target.label()).unwrap_or_else(|| panic!("no sidebar row for {target:?}"));
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "row {target:?} has a real rect"
    );

    let center = rect.center();
    assert!(
        rect.y0 >= 0.0 && rect.y1 <= f64::from(H),
        "navigation fixture must show {target:?}: {rect:?}"
    );

    // Drive the real pointer click through the production event path.
    click_at(&mut app, center);

    // The host then runs `sync` (reads the select key written by the row) and
    // polls the structure-dirty flag to decide whether to rebuild.
    sync(&mut app);
    assert_eq!(
        app.selected(),
        target,
        "after click+sync the selection must be {target:?} (read back via the production path)"
    );
    assert!(
        app.take_structure_dirty(),
        "the selection change to {target:?} must request a rebuild so the pane updates"
    );

    // Rebuild exactly as the host does on structure-dirty, then re-lay-out.
    rebuild(&mut app);

    // The rebuilt sidebar highlights the clicked category and ONLY it.
    assert!(
        row_is_active(&app, &target.label()),
        "the {target:?} row must be active after navigating to it"
    );
    assert!(
        !row_is_active(&app, &Category::DEFAULT.label()),
        "the previously-active default row must no longer be highlighted after navigating to {target:?}"
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

// Every category is reachable by a real sidebar click: the selection, the
// rebuild request, the single active row and the content pane all follow
// (formerly one generated test per category). Each app build is expensive, so
// the categories are spread over a few tests that run in parallel; every case
// still builds its own app.

#[test]
fn clicking_network_bluetooth_or_sound_navigates_to_its_pane() {
    for category in [Category::Network, Category::Bluetooth, Category::Sound] {
        click_navigates_to(category);
    }
}

#[test]
fn clicking_displays_input_or_appearance_navigates_to_its_pane() {
    for category in [
        Category::Displays,
        Category::MouseKeyboard,
        Category::Appearance,
    ] {
        click_navigates_to(category);
    }
}

#[test]
fn clicking_accessibility_notifications_or_power_navigates_to_its_pane() {
    for category in [
        Category::Accessibility,
        Category::Notifications,
        Category::Power,
    ] {
        click_navigates_to(category);
    }
}

#[test]
fn clicking_shortcuts_time_or_users_navigates_to_its_pane() {
    for category in [
        Category::KeyboardShortcuts,
        Category::TimeDate,
        Category::Users,
    ] {
        click_navigates_to(category);
    }
}

#[test]
fn clicking_printers_accounts_or_region_navigates_to_its_pane() {
    for category in [
        Category::Printers,
        Category::ExternalAccounts,
        Category::RegionFormats,
    ] {
        click_navigates_to(category);
    }
}

#[test]
fn clicking_permissions_updates_or_about_navigates_to_its_pane() {
    for category in [Category::Permissions, Category::Updates, Category::About] {
        click_navigates_to(category);
    }
}

/// A second click on a *different* category after a first proves repeated
/// navigation keeps working (not just the first switch off the default).
#[test]
fn two_consecutive_clicks_navigate() {
    let config = crate::support::ScratchConfig::new();
    let mut app = lay_out(config.app());

    let click = |app: &mut Framed<SettingsApp>, target: Category| {
        let rect = row_rect(app, &target.label()).expect("row exists");
        assert!(
            rect.y0 >= 0.0 && rect.y1 <= f64::from(H),
            "navigation fixture must show {target:?}: {rect:?}"
        );
        click_at(app, rect.center());
        sync(app);
        assert!(app.take_structure_dirty());
    };

    click(&mut app, Category::Displays);
    assert_eq!(app.selected(), Category::Displays);
    rebuild(&mut app);

    click(&mut app, Category::Permissions);
    assert_eq!(app.selected(), Category::Permissions);
    rebuild(&mut app);
    assert!(row_is_active(&app, &Category::Permissions.label()));
    assert!(!row_is_active(&app, &Category::Displays.label()));
}
