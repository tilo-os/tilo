//! Click-driven navigation proof.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::categories::Category;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::Affine;

use crate::support::{Window, click_category, selected, window, window_row, window_rows};
use ui::widget::DrawChild;

// Tall enough for the complete category corpus. Sidebar scrolling itself is
// covered by Scrollable's integration tests; this suite exercises category
// activation and must click visible rows rather than coordinates below clip.
const VIEWPORT: (u32, u32) = (1200, 1800);

fn row_is_active(app: &mut Window, label: &str) -> bool {
    window_row(app, label).is_some_and(|row| row.selected)
}

fn renders_something(app: &mut Window) -> bool {
    app.with_widget::<ui::widget::NavShell, _>(|shell| {
        let theme = ui::Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        let mut scene = Scene::new();
        shell.draw_child(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        !scene.encoding().is_empty()
    })
    .expect("settings shows its shell")
}

/// Count the selected navigation rows. After a navigation exactly one row, the
/// target, must be selected.
fn active_row_count(app: &mut Window) -> usize {
    window_rows(app).iter().filter(|row| row.selected).count()
}

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
