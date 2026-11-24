//! A shortcut's device triggers start it on this device without anyone invoking it: a chord the
//! person presses, an app that starts, a focus mode switching on. Each runs it once. Triggers on the
//! workspace (records, schedules, folders) fire in the daemon, which its own tests cover.
//!
//! A shortcut is one of its workspace's settings: every test saves it the way a person's save does
//! — the workspace's shortcut library signs it by this device and commits it into the workspace's
//! document — and what the shell shows of it is that document. The runs are what the device's
//! daemon keeps of the calls it was asked for, read as the person's own program reads them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use serde_json::json;

use crate::common::linked::{Builder, DEFAULT_APP, shortcut};

/// What a key sends, evdev codes.
const KEY_J: u32 = 36;
const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_ALT: u32 = 56;

/// A chord the person presses starts its shortcut through the device's dispatch, as the person the
/// session's link was made for.
#[test]
fn a_chord_pressed_runs_its_shortcut() {
    let mut fixture = Builder::without_an_app().start();
    fixture.save_shortcut(shortcut(
        "tidy",
        "Tidy",
        json!([{"trigger": "keyboard", "chord": "Ctrl+Alt+J"}]),
    ));
    fixture.wait_armed("tidy");
    fixture.chord(&[KEY_LEFT_CTRL, KEY_LEFT_ALT, KEY_J]);
    let runs = fixture.until_runs("tidy", 1);
    assert_eq!(runs.len(), 1, "the chord started the shortcut once");
    // The run says what started it: the chord the person pressed.
    let run = &runs[0];
    assert_eq!(run.by.kind, "person", "the person's own key started it");
    fixture.pass(std::time::Duration::from_millis(600));
    assert_eq!(
        fixture.runs("tidy").len(),
        1,
        "one press, one run: the shortcut does not run again"
    );
}

/// A shortcut armed on an app runs once when the app starts.
#[test]
fn a_shortcut_armed_on_an_app_runs_once_when_it_starts() {
    let mut fixture = Builder::installing(DEFAULT_APP).start();
    fixture.save_shortcut(shortcut(
        "tidy",
        "Tidy",
        json!([{"trigger": "app_opened", "app_id": DEFAULT_APP}]),
    ));
    fixture.wait_armed("tidy");
    // The app opening is the session's own launch of it.
    fixture.open(DEFAULT_APP);
    let runs = fixture.until_runs("tidy", 1);
    assert_eq!(runs.len(), 1, "the app starting started the shortcut");
    fixture.pass(std::time::Duration::from_millis(1200));
    assert_eq!(
        fixture.runs("tidy").len(),
        1,
        "one start, one run: the app keeps running and starts nothing more"
    );
}

/// A shortcut armed on a focus mode runs once when it switches on.
#[test]
fn a_shortcut_armed_on_a_focus_mode_runs_once_when_it_switches_on() {
    let mut fixture = Builder::without_an_app().start();
    fixture.save_shortcut(shortcut(
        "tidy",
        "Tidy",
        json!([{
            "trigger": "focus_mode",
            "mode": tilo_automation::FOCUS_DO_NOT_DISTURB,
            "transition": "enter",
        }]),
    ));
    fixture.wait_armed("tidy");
    // The person turns Do Not Disturb on, from quick settings or the menu.
    settings_store::notification_policy::NotificationPolicy::save_do_not_disturb(true)
        .expect("do not disturb saved");
    let runs = fixture.until_runs("tidy", 1);
    assert_eq!(
        runs.len(),
        1,
        "Do Not Disturb switching on started the shortcut"
    );
    fixture.pass(std::time::Duration::from_millis(1200));
    assert_eq!(
        fixture.runs("tidy").len(),
        1,
        "one switch, one run: Do Not Disturb staying on starts nothing more"
    );
}
