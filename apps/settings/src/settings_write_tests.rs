//! A setting changed in the window is stored off the UI thread.
//!
//! Storing a change locks the settings file, rewrites it and syncs it to
//! disk. On a busy disk, or while the compositor held the lock, that took
//! hundreds of milliseconds, and the window waited for it on every toggle
//! and every step of a slider. Changes are queued now
//! (`ui::storage::flush_settings_writes`) and the background stores them;
//! writing the file on the UI thread panics in a test build. These tests run
//! the window on a thread marked as the UI thread, with a background
//! executor they pump themselves, so the file is provably untouched until
//! the background runs.

use crate::SettingsShell;
use crate::categories::Category;
use crate::host::testing::{NOTIFICATIONS, edit_field};
use crate::prefs::NotificationPrefs;
use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::vello::kurbo::Rect;
use ui::lock::MutexRecover as _;
use ui::widget::Widget;

/// Held by every test that queues settings changes from a thread marked as
/// the UI thread. The queue is the process's own
/// ([`ui::storage::flush_settings_writes`]) and one background job stores
/// all of it: the first test to queue a change schedules that job on its
/// own executor, so another test's queued change would wait for, or be
/// stored by, an executor that test does not run.
pub(crate) static SETTINGS_QUEUE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The first toggle in the content pane, right of the sidebar.
fn first_toggle(w: &dyn Widget, content: Rect) -> Option<Rect> {
    let r = w.get_rect();
    if w.name() == "Toggle" && r.x0 >= content.x0 && r.y0 >= content.y0 && r.area() > 0.0 {
        return Some(r);
    }
    w.children()
        .into_iter()
        .find_map(|child| first_toggle(child, content))
}

/// Run the background's queued work on a thread that is not the UI
/// thread, as the executor's own threads do.
fn run_background(work: &ui::background::Pumped) {
    std::thread::scope(|scope| {
        scope.spawn(|| work.run_pending());
    });
}

fn object(value: Value) -> serde_json::Map<String, Value> {
    serde_json::from_value(value).unwrap()
}

fn notifications(path: &std::path::Path) -> Option<Value> {
    ui::storage::settings_at(path).get("notifications").cloned()
}

#[test]
fn a_setting_change_never_writes_the_settings_file_on_the_ui_thread() {
    let _queue = SETTINGS_QUEUE.lock_recover();
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    let path = app
        .settings::<NotificationPrefs>()
        .document()
        .path()
        .expect("the profile's settings file");
    app.harness().resize((1472, 920)).unwrap();
    crate::effects::show_pane(&mut app, Category::Notifications);
    app.settle();
    let toggle = first_toggle(
        app.harness().root_widget(),
        Rect::new(400.0, 80.0, 1472.0, 900.0),
    )
    .expect("the Notifications pane has a toggle");
    let before = notifications(&path);

    ui::rt::mark_ui_thread();
    let work = ui::background::Pumped::new();
    let _work = work.background().enter();
    let shown = app.settings::<NotificationPrefs>().get();
    app.harness().click_at(toggle.center().x, toggle.center().y);
    app.harness()
        .poll_frame()
        .expect("the window draws the change");
    assert_ne!(
        app.settings::<NotificationPrefs>().get(),
        shown,
        "the window shows the change at once"
    );
    assert_eq!(
        notifications(&path),
        before,
        "the click wrote nothing on the UI thread"
    );

    run_background(&work);
    ui::storage::flush_settings_writes();
    assert_ne!(
        notifications(&path),
        before,
        "the background stored the change"
    );
    assert_eq!(
        serde_json::from_value::<NotificationPrefs>(notifications(&path).unwrap()).ok(),
        Some(app.settings::<NotificationPrefs>().get()),
        "what is stored is what the window shows"
    );
}

#[test]
fn queued_changes_are_stored_in_order_and_keep_what_others_stored() {
    let _queue = SETTINGS_QUEUE.lock_recover();
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts");
    let path = app
        .settings::<NotificationPrefs>()
        .document()
        .path()
        .expect("the profile's settings file");
    ui::rt::mark_ui_thread();
    let work = ui::background::Pumped::new();
    let _work = work.background().enter();

    // A slider dragged and a toggle flipped back and forth, faster than the
    // disk keeps up.
    for on in [true, false, true, false, true] {
        edit_field(&mut app, NOTIFICATIONS, "do_not_disturb", on).unwrap();
    }
    edit_field(&mut app, NOTIFICATIONS, "show_banners", false).unwrap();
    let prefs = app.settings::<NotificationPrefs>().get();
    assert!(
        prefs.do_not_disturb && !prefs.show_banners,
        "the last change shows at once"
    );
    assert_eq!(notifications(&path), None, "nothing written yet");

    // Meanwhile the compositor stores a section of its own and a key of the
    // same section that Settings does not edit.
    settings_store::update_section_at(&path, "power", object(json!({ "blank_minutes": 7 })))
        .unwrap();
    settings_store::update_section_at(
        &path,
        "notifications",
        object(json!({ "lock_screen_seen": 3 })),
    )
    .unwrap();
    // And a thread of this process changes the toggle once more, after the
    // queued changes: it waits for them, so its change is the last.
    let later = std::thread::spawn({
        let document = app.settings::<NotificationPrefs>().document().clone();
        move || {
            document
                .update_section("notifications", object(json!({ "do_not_disturb": false })))
                .unwrap();
        }
    });
    run_background(&work);
    later.join().unwrap();
    ui::storage::flush_settings_writes();

    let stored = ui::storage::settings_at(&path);
    let section = &stored["notifications"];
    assert_eq!(
        section["do_not_disturb"],
        json!(false),
        "changes land in the order they were made: the last one wins"
    );
    assert_eq!(section["show_banners"], json!(false));
    assert_eq!(
        section["lock_screen_seen"],
        json!(3),
        "another writer's key in the same section is kept"
    );
    assert_eq!(stored["power"]["blank_minutes"], json!(7));
    assert_eq!(
        app.settings::<NotificationPrefs>().get(),
        serde_json::from_value::<NotificationPrefs>(section.clone()).unwrap(),
        "the window reads what is stored"
    );
}
