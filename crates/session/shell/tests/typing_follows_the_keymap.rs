//! Test control types text, whatever the session's keyboard layout.
//!
//! A Wayland client reads each keycode through the keymap the seat sent it,
//! so typing `y` means pressing the key that layout puts `y` on: evdev 21 on
//! a US keymap, evdev 44 on a German one, with Shift or `AltGr` held where the
//! keymap needs them. A character no key types on the keymap reaches the
//! window's text field as text, the way an input method commits it. The
//! client here decodes its keys with the keymap it was sent, exactly as a
//! winit (and so a tilolang) window does, and reports the text it got.
//!
//! Shortcuts follow the keysym the way the toolkit matches them
//! (`ui::platform::HostShortcut::resolve` takes the character): Ctrl+Z is
//! the key that types `z`, which on a German keyboard is where a US one has Y.

use serde_json::{Value, json};
use shell::shell::state::WorkspaceTree;

use crate::support::desktop::{Client, Desktop};

const SCREEN: (i32, i32) = (1280, 800);
const CLIENT_APP_ID: &str = "click-test";
const TEXT: &str = "xyzäö@€";

/// A desktop whose machine layout is `layout` (the person chose none, so
/// the seat follows the machine's, here as the installer exports it), with
/// the test client open and holding the keyboard.
fn typing_into(layout: &str) -> (Desktop, Client) {
    let desktop = Desktop::start_with_env(
        SCREEN,
        &WorkspaceTree::bootstrap_default(),
        &[("XKB_DEFAULT_LAYOUT", layout), ("XKB_DEFAULT_VARIANT", "")],
    );
    desktop.ok(&json!({"op": "place_next_window", "app_id": CLIENT_APP_ID}));
    let client = desktop.spawn_client(&crate::support::click_client::path());
    let mut rect = None;
    desktop.wait_for_with(
        "the client to paint",
        || client.fate(),
        || {
            rect = desktop
                .foreign_frames()
                .iter()
                .filter(|frame| frame["app_id"] == CLIENT_APP_ID && frame["presented"] == true)
                .find_map(|frame| {
                    let rect = &frame["rect"];
                    Some((
                        rect["x"].as_f64()? + rect["w"].as_f64()? / 2.0,
                        rect["y"].as_f64()? + rect["h"].as_f64()? / 2.0,
                    ))
                });
            rect.is_some()
        },
    );
    let (x, y) = rect.expect("a painted client");
    desktop.click_at(x, y);
    desktop.wait_for_with(
        "the client to hold the keyboard",
        || format!("{}\n{}", client.fate(), client.output()),
        || client.count("EVENT:KEYBOARD ENTER") > client.count("EVENT:KEYBOARD LEAVE"),
    );
    (desktop, client)
}

/// The text the client was typed, in order: each key press's text (a bare
/// modifier types none) and each text commit.
fn typed(client: &Client) -> String {
    let output = client.output();
    let mut text = String::new();
    for event in output.split("EVENT:").skip(1) {
        let event = event.trim();
        let quoted = if let Some(rest) = event.strip_prefix("KEY PRESS Some(") {
            rest.split_once(") sym=").map(|(quoted, _)| quoted)
        } else {
            event.strip_prefix("TEXT ")
        };
        if let Some(quoted) = quoted {
            let value: Value = serde_json::from_str(quoted)
                .unwrap_or_else(|error| panic!("{quoted} is not a quoted string: {error}"));
            text.push_str(value.as_str().expect("a string"));
        }
    }
    text
}

fn types_exactly_the_text(layout: &str) {
    let (desktop, client) = typing_into(layout);
    desktop.type_text(TEXT);
    desktop.wait_for_with(
        "the client to have the typed text",
        || format!("{}\nit was typed {:?}", client.output(), typed(&client)),
        || typed(&client).chars().count() >= TEXT.chars().count(),
    );
    assert_eq!(typed(&client), TEXT, "{}", client.output());
}

#[test]
fn typing_through_test_control_types_the_text_on_a_german_keymap() {
    types_exactly_the_text("de");
}

#[test]
fn typing_through_test_control_types_the_text_on_a_us_keymap() {
    types_exactly_the_text("us");
}

#[test]
fn ctrl_z_is_the_key_that_types_z_on_a_german_keymap() {
    let (desktop, client) = typing_into("de");
    desktop.chord_ctrl("z");
    desktop.wait_for_with(
        "the client to get the chord",
        || client.output(),
        || client.count("ctrl") > 0,
    );
    let output = client.output();
    assert!(
        output.contains("sym=Some('z') ctrl"),
        "Ctrl+Z reaches the window as the keysym z, which undoes; a US position \
         (Ctrl+Y on this keymap) would redo:\n{output}"
    );
    assert!(!output.contains("sym=Some('y')"), "{output}");
}
