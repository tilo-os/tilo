//! A person clicks a text field in a tilolang app's window and types: the clicked field takes
//! the keyboard, and each key arrives in the app.
//!
//! Shortcuts' trigger form keeps its fields in a record inside the workbench document the window
//! shows: the window names the document, and the text belongs to the record the form edits, so
//! this is the path where a field a view keeps for itself takes typing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the device lives until the \
              session ends"
)]

use serde_json::{Value, json};

use crate::common::people::{Device, Person};

/// The app the person types into: Shortcuts, whose draft editor draws the trigger form.
const APP: &str = "so.tilo.shortcuts";
/// What the person types into the trigger's app field.
const TYPED: &str = "so.tilo.chat";

/// Whether `target` is drawn anywhere: a control a window holds open but shows nowhere (a
/// menu's items while its menu is closed) stands nowhere, and a click aimed at it lands on
/// nothing.
fn drawn(target: &Value) -> bool {
    target["rect"].as_array().is_some_and(|rect| {
        rect.len() == 4
            && rect[2].as_f64().is_some_and(|width| width > 0.0)
            && rect[3].as_f64().is_some_and(|height| height > 0.0)
    })
}

/// The controls `device` shows whose name is `name` or starts with it, drawn or not.
fn named(device: &Device, name: &str) -> Vec<Value> {
    let answer = device.ask(&json!({"op": "list_targets", "prefixes": [name]}));
    answer["targets"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{} lists its controls: {answer}", device.label))
}

/// Wait until `device` draws a control whose name is `name` or starts with it.
#[track_caller]
fn wait_drawn(device: &Device, name: &str) -> Value {
    tilo_test_env::wait_for(format!("{} to draw {name:?}", device.label), || {
        named(device, name).into_iter().find(|target| drawn(target))
    })
}

/// Click `target` where it is drawn, as the person at `device` does.
#[track_caller]
fn click(device: &Device, target: &Value) {
    let rect = target["rect"]
        .as_array()
        .filter(|rect| rect.len() == 4)
        .unwrap_or_else(|| panic!("{}'s control has a rect: {target}", device.label));
    let at = |index: usize| {
        rect[index]
            .as_f64()
            .unwrap_or_else(|| panic!("{}'s control rect is numbers: {target}", device.label))
    };
    let (x, y, width, height) = (at(0), at(1), at(2), at(3));
    let answer = device.ask(&json!({
        "op": "click_at",
        "x": x + width / 2.0,
        "y": y + height / 2.0,
    }));
    assert!(
        answer.get("err").is_none(),
        "{} clicks {:?}: {answer}\n{}",
        device.label,
        target["name"],
        device.diagnostic()
    );
}

/// A person starts a draft in Shortcuts, picks what starts it and clicks into the app id field
/// there, then types: the words arrive in the app, and the trigger they add is for the app they
/// typed.
#[test]
fn a_person_clicks_a_text_field_in_a_tl_app_and_what_they_type_arrives() {
    let ada = Person::new("Ada");
    let mut device = ada.device("Ada's laptop");
    device.install_shortcuts();
    device.start();
    device.open(APP);
    device.wait_for_window(APP);

    // New shortcut opens a draft, and its trigger form stands in the window.
    click(&device, &wait_drawn(&device, "New shortcut"));
    wait_drawn(&device, "Add trigger");

    // What starts it: the case field's menu picks "App opened", and the form asks for the app.
    click(&device, &wait_drawn(&device, "What starts it"));
    click(&device, &wait_drawn(&device, "App opened"));
    // The person clicks into the app id field and types it.
    click(&device, &wait_drawn(&device, "App id"));
    device.type_text(TYPED);

    // Add trigger takes the form into the draft: the trigger stands with the app that was typed
    // into the field, which is the app the typing reached.
    click(&device, &wait_drawn(&device, "Add trigger"));
    tilo_test_env::wait_for(format!("{} to list its trigger", device.label), || {
        let titled = named(&device, "An app opens")
            .into_iter()
            .find(|target| drawn(target));
        let typed = named(&device, TYPED)
            .into_iter()
            .find(|target| drawn(target));
        (titled.is_some() && typed.is_some()).then_some(())
    });
}
