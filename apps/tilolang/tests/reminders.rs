//! Reminders as a person meets them in the running app: a task the workspace holds a reminder
//! for shows up on the platform's notifications at its instant, a click opens it in the window,
//! and the action the notification offers runs on it — for the person it names, once.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tilolang_app::app::TlApp;
use tilolang_app::window::Window;
use tilolang_tilo::personal::{self, Personal};
use tilolang_tilo::runtime::{Principal, Value};
use ui::framework::testing::TestApp;
use ui::notify::conformance::Recording;

const APP: &str = "t.remind";

/// The workspace the window is open in.
const ROOM: &str = "00000000-0000-4000-8000-00000000000a";

/// The person the window runs as: the one the task is assigned to, and the only one reminded.
fn person() -> String {
    "ab".repeat(32)
}

/// The 27th of October 2026, at `hour`:`minute`, in UTC: a time of the morning of the 27th.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "minutes of one fixed morning"
)]
fn instant(hour: u32, minute: u32) -> SystemTime {
    UNIX_EPOCH
        + Duration::from_secs(1_793_091_600 + u64::from(hour - 9) * 3_600 + u64::from(minute) * 60)
}

/// The platform's services with a recording notifier, so the test reads what the window showed.
fn services(notes: &Arc<Recording>) -> ui::host::HostServices {
    let mut services = ui::host::HostServices::null();
    services.notifications = Arc::clone(notes) as Arc<dyn ui::notify::Notifier>;
    services
}

/// The workspace's board at the window, with a task due at 10:00: "Call the landlord", assigned
/// to the person at the window, reminding them half an hour before.
fn add_task(app: &mut TestApp<TlApp>) {
    app.with_surface::<Window, _>(|window| {
        let host = window.host().expect("the window has the record");
        let action = host
            .package(APP)
            .expect("the package is loaded")
            .meta
            .actions
            .iter()
            .find(|action| action.name == "new_task")
            .expect("the package declares it")
            .id
            .clone();
        let root = Value::resource(&host.root_of(APP).expect("the package has a root"));
        let me = person();
        host.runtime()
            .invoke_action(
                &Principal::new(&me),
                (
                    action.as_str(),
                    vec![
                        root,
                        Value::text("Call the landlord"),
                        Value::text("2026-10-27"),
                        Value::some(Value::text("2026-10-27T10:00:00Z")),
                        Value::some(Value::Principal(Principal::new(&me))),
                        Value::text("-PT30M"),
                    ],
                ),
            )
            .unwrap();
        window.commit_planned();
    })
    .expect("the window is the app's surface");
    app.settle();
}

#[test]
fn a_reminder_of_the_workspace_comes_as_a_notification_the_person_can_act_on() {
    support::installed("remind");
    let notes = Arc::new(Recording::default());
    let services = services(&notes);
    let _host = ui::host::enter(&services);
    let mut app =
        TestApp::<TlApp>::for_person_in(&person(), ROOM).expect("the app opens in the workspace");
    app.settle();
    ui::time::testing::with_wall(instant(9, 0), || {
        add_task(&mut app);
        assert!(notes.shown().is_empty(), "half an hour early");
    });
    let (tag, open, done) = ui::time::testing::with_wall(instant(9, 30), || {
        app.settle();
        let shown = notes.shown();
        assert_eq!(shown.len(), 1, "one reminder at its instant: {shown:#?}");
        let shown = &shown[0];
        assert_eq!(shown.title, "Call the landlord");
        assert!(
            shown.body.ends_with("· Remind"),
            "from the app: {}",
            shown.body
        );
        assert_eq!(
            shown.buttons.len(),
            1,
            "the one action the type declares that asks nothing"
        );
        assert_eq!(shown.buttons[0].label, "Done");
        (
            shown.tag.clone(),
            shown.activation.clone().unwrap(),
            shown.buttons[0].activation.clone(),
        )
    });
    // The window does not show it again while it stays due.
    ui::time::testing::with_wall(instant(9, 30), || {
        app.settle();
        assert_eq!(notes.shown().len(), 1, "told once");
    });
    // A click opens the task in the window that runs the app.
    ui::time::testing::with_wall(instant(9, 31), || {
        ui::notify::report_activation(&open);
        app.settle();
        let shown = app
            .with_surface::<Window, _>(|window| window.instance().detail_target().cloned())
            .expect("the window is the app's surface");
        let shown = shown.expect("the click opened the task");
        assert!(
            shown.item.is_some(),
            "the click opened the task, not the board"
        );
        // The button runs the action the type declared, on the task.
        ui::notify::report_activation(&done);
        app.settle();
        // The window runs the action through the door, the record carries it, and the walk that
        // follows takes the notification back.
        app.settle();
        assert_eq!(
            notes.withdrawn(),
            vec![tag],
            "the done task reminds nobody, so its notification goes"
        );
    });
    // The window kept the task as one this person was told of: their other devices read the mark
    // and stay quiet.
    let record = app
        .shared_workspace::<Personal>(personal::OWNER)
        .expect("the workspace keeps each person's own state");
    let told = record.get().of(&person()).told.len();
    assert_eq!(told, 1, "one mark, one reminder told once");
}
