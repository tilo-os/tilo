//! No pane makes the window wait on the device.
//!
//! Settings reads and changes the device through commands, D-Bus calls and
//! walks of large trees, which take seconds on a slow or wedged service.
//! Every such call marks itself ([`crate::device_read::off_ui_thread`]),
//! which panics on a UI thread in a test build. This test runs the window
//! on a thread marked as the UI thread, reading as a live window does, and
//! opens every pane from the sidebar and changes a control on each: a pane
//! or a control that reaches the device on the UI thread fails it, and each
//! open and each change must be answered and its frame drawn. How long each
//! took is logged: a debug build on a loaded test machine spends up to a few
//! seconds of its own computing on some, so the device calls, not the time,
//! are what this asserts on. Each area's stalled-device test asserts that a
//! device that does not answer holds nothing.

use crate::{SettingsShell, categories::Category};
use ui::framework::testing::TestApp;
use ui::lock::MutexRecover as _;
use ui::vello::kurbo::Rect;
use ui::widget::Widget;

/// The first toggle or choice in the content pane, right of the sidebar.
fn first_control(w: &dyn Widget, content: Rect) -> Option<(String, Rect)> {
    if matches!(w.name(), "Toggle" | "Dropdown") {
        let r = w.get_rect();
        if r.x0 >= content.x0 && r.y0 >= content.y0 && r.y1 <= content.y1 && r.area() > 0.0 {
            return Some((w.name().to_owned(), r));
        }
    }
    w.children()
        .into_iter()
        .find_map(|child| first_control(child, content))
}

/// Run `step` and draw the next frame; how long the window was held.
fn held(
    app: &mut TestApp<SettingsShell>,
    step: impl FnOnce(&mut TestApp<SettingsShell>),
) -> std::time::Duration {
    let started = std::time::Instant::now();
    step(app);
    let stepped = started.elapsed();
    app.harness()
        .poll_frame()
        .expect("the window draws its next frame");
    eprintln!(
        "  step {stepped:?}, frame {:?}",
        started.elapsed().saturating_sub(stepped)
    );
    started.elapsed()
}

#[test]
fn every_pane_opens_and_takes_a_change_without_waiting_on_the_device() {
    // Its changes are queued on a thread marked as the UI thread.
    let _queue = crate::settings_write_tests::SETTINGS_QUEUE.lock_recover();
    ui::i18n::I18nManager::init("en-US");
    crate::effects::defer_area_reads_in_tests(true);
    ui::rt::mark_ui_thread();
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    app.harness().resize((1472, 920)).unwrap();
    let _ = app.harness().poll_frame();
    let reading = crate::i18n::t("settings-pane-reading");
    for &category in Category::ALL {
        let opened = held(&mut app, |app| crate::effects::show_pane(app, category));
        eprintln!("{category:?}: opened in {opened:?}");
        // What the pane reads lands off the UI thread, and the pane fills in.
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| !frame.contains(&reading))),
            "{category:?}: what the pane reads is shown"
        );
        app.settle();
        let control = first_control(
            app.harness().root_widget(),
            Rect::new(400.0, 80.0, 1472.0, 900.0),
        );
        let Some((name, rect)) = control else {
            continue;
        };
        let changed = held(&mut app, |app| {
            app.harness().click_at(rect.center().x, rect.center().y);
            if name == "Dropdown" {
                app.harness().key("ArrowDown");
                app.harness().key("Enter");
            }
        });
        eprintln!("{category:?}: changed its first {name} in {changed:?}");
        app.settle();
        // A dropdown left open would take the next pane's first click.
        app.harness().key("Escape");
    }
}
