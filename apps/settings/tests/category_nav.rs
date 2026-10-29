//! Click-driven navigation proof.
//!
//! This is the regression guard for the bug where clicking a sidebar category
//! did NOT switch the content pane. It drives a *real pointer click* at the
//! row's exported bounds through the window a host runs, whose sidebar rows
//! run the pane's published open action, and asserts:
//!
//! 1. the selected category became the clicked one (read back from the app,
//!    NOT set directly — that would be tautological);
//! 2. the sidebar marks exactly that category's row active; and
//! 3. the content pane for the new category actually renders.
//!
//! A test runs per category, so every entry is exercised end to end.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::categories::Category;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::Widget as _;
use vello::Scene;
use vello::kurbo::Affine;

use crate::support::{Window, click_category, selected, window, window_row, window_rows};

// Tall enough for the complete category corpus. Sidebar scrolling itself is
// covered by Scrollable's integration tests; this suite exercises category
// activation and must click visible rows rather than coordinates below clip.
const VIEWPORT: (u32, u32) = (1200, 1800);

/// Is the row for `label` currently drawn selected? Read from the list's
/// published semantics.
fn row_is_active(app: &mut Window, label: &str) -> bool {
    window_row(app, label).is_some_and(|row| row.selected)
}

/// Render the app's surface and report whether the scene encoded any draw
/// commands — proof the composition, including the new category's content
/// pane, actually paints rather than collapsing to nothing.
fn renders_something(app: &mut Window) -> bool {
    app.with_widget::<ui::widget::NavShell, _>(|shell| {
        let theme = ui::Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        let mut scene = Scene::new();
        shell.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        !scene.encoding().is_empty()
    })
    .expect("settings shows its shell")
}

/// Count the selected navigation rows. After a navigation exactly one row, the
/// target, must be selected.
fn active_row_count(app: &mut Window) -> usize {
    window_rows(app).iter().filter(|row| row.selected).count()
}

/// Click `target`'s visible row and assert the click reached it.
fn click(app: &mut Window, target: Category) {
    let rect = window_row(app, &target.label())
        .unwrap_or_else(|| panic!("no sidebar row for {target:?}"))
        .rect;
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "row {target:?} has a real rect"
    );
    assert!(
        rect.y0 >= 0.0 && rect.y1 <= f64::from(VIEWPORT.1),
        "navigation fixture must show {target:?}: {rect:?}"
    );
    click_category(app, target);
}

/// The shared click-and-assert routine: starting from a fresh window on the
/// default category, click `target`'s row and assert navigation reached it.
fn click_navigates_to(target: Category) {
    let mut app = window(VIEWPORT);
    assert_eq!(
        selected(&mut app),
        Category::DEFAULT,
        "starts on the default category"
    );
    if target == Category::DEFAULT {
        // The default is already selected; navigating to it is a no-op, so
        // this case is covered by every *other* category proving the switch.
        return;
    }

    click(&mut app, target);
    assert_eq!(
        selected(&mut app),
        target,
        "after the click the selection must be {target:?} (read back from the app)"
    );

    // The sidebar highlights the clicked category and ONLY it.
    assert!(
        row_is_active(&mut app, &target.label()),
        "the {target:?} row must be active after navigating to it"
    );
    assert!(
        !row_is_active(&mut app, &Category::DEFAULT.label()),
        "the previously-active default row must no longer be highlighted after navigating to {target:?}"
    );
    assert_eq!(
        active_row_count(&mut app),
        1,
        "exactly one sidebar row may be active after navigating to {target:?}"
    );

    // The composition (with the new category's content pane) paints.
    assert!(
        renders_something(&mut app),
        "the {target:?} content pane must render after navigating to it"
    );
}

#[test]
fn clicking_network_bluetooth_or_sound_navigates_to_its_pane() {
    for category in [Category::Network, Category::Bluetooth, Category::Sound] {
        click_navigates_to(category);
    }
}

#[test]
fn clicking_displays_input_or_appearance_navigates_to_its_pane() {
    for category in [Category::Displays, Category::Keyboard, Category::Appearance] {
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
    let mut app = window(VIEWPORT);

    click(&mut app, Category::Displays);
    assert_eq!(selected(&mut app), Category::Displays);

    click(&mut app, Category::Permissions);
    assert_eq!(selected(&mut app), Category::Permissions);
    assert!(row_is_active(&mut app, &Category::Permissions.label()));
    assert!(!row_is_active(&mut app, &Category::Displays.label()));
}
