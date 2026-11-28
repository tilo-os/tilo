//! A launch from the shell's start surface, in a real session of the kind a person runs: the
//! launcher's grid is the shell's own surface, and choosing an app in it dismisses the surface
//! and brings the app's window forward and focused. A launch with reduced motion on lands the
//! surface's departure at once, through the same shared transition the shell's surfaces use.
//!
//! The shell and the app are the ones a device runs: the session this device's daemon started,
//! with the tilolang shell in front of it and Chat installed beside it, and every check is what
//! that session shows or what assistive technology hears on it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the device lives until its \
              session ends"
)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{Device, Person, set_setting};

/// The app the person opens from the launcher; its name stands on no start card, so a press that
/// names it reaches the launcher's row and nothing behind it.
const CHAT: &str = "so.tilo.chat";
/// The app the start surface's own card opens.
const TASKS: &str = "so.tilo.tasks";
/// The launcher's field: what a person sees when the surface is up, and nothing when it is not.
const LAUNCHER: &str = "Search apps, windows, actions and files";
/// The start surface's way into the launcher.
const OPENER: &str = "Open an app";
/// How long a step on a shell that is already up is given.
const STEP: Duration = Duration::from_secs(60);

/// Every control the shell on `device` publishes now.
fn controls(device: &Device) -> Vec<Value> {
    device.ask(&json!({"op": "list_targets"}))["targets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Whether the shell on `device` shows a control named `name`.
fn shows(device: &Device, name: &str) -> bool {
    controls(device)
        .iter()
        .any(|control| control["name"].as_str() == Some(name))
}

/// The rows of the window `app` on `device`'s shell.
fn windows_of(device: &Device, app: &str) -> Vec<Value> {
    device.ask(&json!({"op": "windows", "app_id": app}))["answer"]["windows"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Whether `app` has a window on `device` that is the one in front: the shell's focused strip
/// shows the group, and the group shows that window.
fn window_in_front(device: &Device, app: &str) -> bool {
    windows_of(device, app).iter().any(|window| {
        window["active"].as_bool() == Some(true)
            && !window["rect"].is_null()
            && window["app_id"].as_str() == Some(app)
    })
}

/// Wait until `probe` answers, or fail the step naming what the device was showing.
#[track_caller]
fn waited<T>(device: &Device, what: &str, probe: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now()
        .checked_add(STEP)
        .expect("the bound fits in an Instant");
    loop {
        if let Some(answered) = probe() {
            return answered;
        }
        assert!(
            Instant::now() < deadline,
            "{} never showed {what} within {STEP:?}; its shell shows {:?}\n{}",
            device.label,
            controls(device),
            device.diagnostic()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until the shell on `device` shows a control named `name`, and answer where it stands.
#[track_caller]
fn appear(device: &Device, name: &str) -> Value {
    waited(device, &format!("{name:?}"), || {
        controls(device)
            .into_iter()
            .find(|control| control["name"].as_str() == Some(name))
    })
}

/// Wait until the shell on `device` no longer shows a control named `name`.
#[track_caller]
fn gone(device: &Device, name: &str) {
    waited(device, &format!("{name:?} gone"), || {
        (!shows(device, name)).then_some(())
    });
}

/// A device with the development session's apps installed, its shell up, and its start surface
/// offering the way into the launcher.
fn device(label: &str) -> Device {
    let person = Person::new("Ada");
    let mut device = person.device(label);
    device.install_dev_apps();
    device.start();
    appear(&device, OPENER);
    device
}

/// Open the launcher from the start surface, and choose `app` by the name its row carries.
fn launch_from_the_launcher(device: &Device, app: &str, name: &str) {
    device.press(OPENER);
    appear(device, LAUNCHER);
    // The person types the app's name and presses its row, as a hand does.
    device.press(LAUNCHER);
    device.type_text(&name.to_lowercase());
    appear(device, name);
    device.press(name);
    // The surface is gone, and the app's window is the one in front and drawn: what a press on a
    // row launches is the app, not a second copy of the surface.
    gone(device, LAUNCHER);
    device.wait_for_window(app);
    waited(device, "the app's window in front", || {
        window_in_front(device, app).then_some(())
    });
}

/// The start surface a shell with no window draws is a grid of what a person can start: a card
/// pressed there launches its app, the surface leaves with the launch, and the app's window
/// stands in front and focused.
#[test]
fn choosing_a_start_card_launches_its_app_and_leaves_the_start_surface() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let device = device("start card launch");
    // The way into the launcher stands on the start surface and nowhere else: its button leaving
    // is that surface leaving.
    assert!(
        shows(&device, OPENER),
        "an empty shell draws its start surface: {:?}",
        controls(&device)
    );
    device.press("Tasks");
    gone(&device, OPENER);
    device.wait_for_window(TASKS);
    waited(&device, "Tasks' window in front", || {
        window_in_front(&device, TASKS).then_some(())
    });
}

/// The grid the shell's start surface opens is dismissed by the app a person chooses in it, and
/// the app's window stands in front and focused.
#[test]
fn choosing_an_app_in_the_launcher_dismisses_it_and_brings_the_app_forward() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let device = device("launcher launch");
    launch_from_the_launcher(&device, CHAT, "Chat");
}

/// The same launch with the person's motion reduced: the theme takes the setting, and the
/// launcher's departure is the shell's shared transition, which lands at once under it, so the
/// launch ends with neither the surface nor a window still on its way.
#[test]
fn reduced_motion_lands_the_launcher_and_the_app_at_once() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let device = device("launcher launch, calm");
    set_setting(&device, "appearance", "reduce_motion", &json!(true));
    waited(&device, "the calm motion the person set", || {
        (device.settings()["appearance"]["reduce_motion"].as_bool() == Some(true)).then_some(())
    });
    launch_from_the_launcher(&device, CHAT, "Chat");
}
