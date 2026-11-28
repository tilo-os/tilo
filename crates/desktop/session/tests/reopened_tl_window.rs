//! A person closes a tilolang app's window and reopens it: the window comes back on the page they
//! left, because which page a window shows is the person's view state of that window, kept in
//! their own document under the window's tab, and a reopened window takes the tab it had.
//!
//! The session is the real one its device's daemon starts; the app is a real tilolang package in
//! a window process of its own, which ends with its last window, so the page shown again is read
//! back from what was kept, not from a process that outlived the window.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion, and the device lives until the \
              session ends"
)]

use serde_json::{Value, json};

use crate::common::people::{Device, Person};

/// A tilolang app with a sidebar of pages, which opens on its first.
const APP: &str = crate::common::people::GALLERY;
/// The shell's own resource, whose action closes a window, and the chrome's, whose action reopens
/// the window closed last.
const SHELL: &str = "desktop:shell";
const CHROME: &str = "desktop:chrome";
/// What the gallery's first page shows, and what its Controls page shows.
const BASICS: &str = "Rich text and a quote";
const CONTROLS: &str = "Boxes and switches";

/// Whether `target` is drawn anywhere.
fn drawn(target: &Value) -> bool {
    target["rect"].as_array().is_some_and(|rect| {
        rect.len() == 4
            && rect[2].as_f64().is_some_and(|width| width > 0.0)
            && rect[3].as_f64().is_some_and(|height| height > 0.0)
    })
}

/// The control `device` draws named `name`, if it draws one now.
fn shown(device: &Device, name: &str) -> Option<Value> {
    let answer = device.ask(&json!({"op": "list_targets", "prefixes": [name]}));
    answer["targets"]
        .as_array()
        .unwrap_or_else(|| panic!("{} lists its controls: {answer}", device.label))
        .iter()
        .find(|target| target["name"] == name && drawn(target))
        .cloned()
}

/// Wait until `device` draws `name`.
#[track_caller]
fn wait_shown(device: &Device, name: &str) -> Value {
    tilo_test_env::wait_for(format!("{} to draw {name:?}", device.label), || {
        shown(device, name)
    })
}

/// Click `target` where it is drawn, as the person at `device` does.
fn click(device: &Device, target: &Value) {
    let rect = &target["rect"];
    let at = |index: usize| rect[index].as_f64().expect("a rect of numbers");
    let answer = device.ask(&json!({
        "op": "click_at",
        "x": at(0) + at(2) / 2.0,
        "y": at(1) + at(3) / 2.0,
    }));
    assert!(
        answer.get("err").is_none(),
        "{} clicks {}: {answer}",
        device.label,
        target["name"]
    );
}

/// The tab of the app's window, while the device shows one.
fn tab(device: &Device) -> Option<u64> {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()?
        .iter()
        .find(|row| row["app_id"] == APP)
        .and_then(|row| row["tab"].as_u64())
}

/// Run `resource`'s `action` with `arguments`, as the person's own call of it.
fn shell(device: &Device, resource: &str, action: &str, arguments: &Value) {
    let target = tilo_test_env::wait_for(format!("{resource}'s {action}"), || {
        device.offered(resource, action)
    });
    let answer = device.act_wait(&target, arguments);
    assert!(
        answer.get("err").is_none(),
        "{} runs the shell's {action}: {answer}",
        device.label
    );
}

#[test]
fn a_reopened_tl_window_shows_the_page_the_person_left_it_on() {
    let ada = Person::new("Ada");
    let mut device = ada.device("Ada's laptop");
    device.install_gallery();
    device.start();
    device.open(APP);
    device.wait_for_window(APP);
    wait_shown(&device, BASICS);

    // The person picks Controls in the sidebar.
    click(&device, &wait_shown(&device, "Controls"));
    wait_shown(&device, CONTROLS);
    let closed = tab(&device).expect("the window has a tab");

    // They close the window, and the app ends with it.
    shell(
        &device,
        SHELL,
        "close-tab",
        &json!({"tab": closed.to_string()}),
    );
    tilo_test_env::wait_for("the window to close", || {
        tab(&device).is_none().then_some(())
    });

    // They reopen it: the window takes its tab again, on the page they left.
    shell(&device, CHROME, "reopen-closed", &json!({}));
    let reopened = tilo_test_env::wait_for("the window to come back", || tab(&device));
    assert_eq!(reopened, closed, "the reopened window takes the tab it had");
    wait_shown(&device, CONTROLS);
    assert!(
        shown(&device, BASICS).is_none(),
        "the reopened window shows Controls, not the page an app opens on"
    );
}
