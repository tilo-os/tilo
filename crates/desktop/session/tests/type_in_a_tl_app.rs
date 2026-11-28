//! A tilolang app's own write is kept: a person picks what starts a Shortcut in the draft's
//! trigger form, and the record the form edits holds the case they picked.
//!
//! Shortcuts' trigger form keeps its fields in a record inside the workbench document the window
//! shows: the window names the document, and the pick belongs to the record the form edits. A
//! store's document is decided against the kinds of the records a change writes, so a control's
//! own edit has to name the record it wrote.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the device lives until the \
              session ends"
)]

use serde_json::{Value, json};

use crate::common::people::{Device, Person};

/// The app the person picks a trigger in.
const APP: &str = "so.tilo.shortcuts";
/// The case the person picks for what starts the shortcut.
const CASE: &str = "App opened";
/// The case's own name, as the record holds it.
const CASE_ID: &str = "app_opened";

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
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(target) = named(device, name).into_iter().find(drawn) {
            return target;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{} never drew {name:?}\n{}",
            device.label,
            device.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Press the first drawn control whose name is `name` or starts with it, as assistive
/// technology activates it: the way a tool or a screen reader presses a button or a menu row.
#[track_caller]
fn press(device: &Device, name: &str) {
    let target = wait_drawn(device, name);
    let named_by = target["name"]
        .as_str()
        .unwrap_or_else(|| panic!("{}'s control has a name: {target}", device.label));
    let answer = device.ask(&json!({"op": "click_named", "name": named_by}));
    assert!(
        answer.get("err").is_none(),
        "{} presses {named_by:?}: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
}

/// Click the first drawn control whose name is `name` or starts with it, where the session
/// draws it, as the person at `device` does with the pointer.
#[track_caller]
fn click(device: &Device, name: &str) -> Value {
    let target = wait_drawn(device, name);
    let rect = target["rect"]
        .as_array()
        .unwrap_or_else(|| panic!("{}'s control has a place: {target}", device.label));
    let at = |index: usize| {
        rect[index]
            .as_f64()
            .unwrap_or_else(|| panic!("a rect of numbers: {target}"))
    };
    let answer = device.ask(&json!({
        "op": "click_at",
        "x": at(0) + at(2) / 2.0,
        "y": at(1) + at(3) / 2.0,
    }));
    assert!(
        answer.get("err").is_none(),
        "{} clicks {:?} where it stands: {answer}\n{}",
        device.label,
        target["name"],
        device.diagnostic()
    );
    target
}

/// What the app's window shows, as the app reads its own records to draw it: the app's
/// `outline` action, the text an agent reads of the window.
#[track_caller]
fn outline(device: &Device) -> String {
    let answer = device.app_act(APP, "outline", &json!({}));
    answer
        .pointer("/update/output/value/outline")
        .and_then(Value::as_str)
        .map_or_else(
            || {
                panic!(
                    "{} reads {APP}'s outline: {answer}\n{}",
                    device.label,
                    device.diagnostic()
                )
            },
            str::to_owned,
        )
}

/// A device where Ada's Shortcuts is open, its window drawn: what each press below starts from.
fn opened_shortcuts() -> Device {
    let ada = Person::new("Ada");
    let mut device = ada.device("Ada's laptop");
    device.install_shortcuts();
    device.start();
    device.open(APP);
    device.wait_for_window(APP);
    device
}

/// A person starts a draft in Shortcuts and picks what starts it: the pick is a control's own
/// edit of the trigger record, it is kept there, and the workspace's admission does not refuse
/// it for naming no kind of the record it wrote.
#[test]
fn a_pick_in_a_tl_apps_trigger_form_is_kept_in_its_record() {
    let device = opened_shortcuts();

    // New shortcut opens a draft, and its trigger form stands in the window.
    press(&device, "New shortcut");
    wait_drawn(&device, "Add trigger");

    // What starts it: the case field's menu picks "App opened", and the field shows the case it
    // took, which the app draws from the record its pick was kept in.
    press(&device, "What starts it");
    press(&device, CASE);
    wait_drawn(&device, &format!("What starts it, {CASE}"));

    // The record the form edits holds the case: the app reads its records to draw its window, so
    // the window it outlines for an agent carries the case field holding the case it took.
    let shown = outline(&device);
    assert!(
        shown.contains(CASE),
        "the form's record holds {CASE_ID:?} ({CASE}): {shown}"
    );

    // And the workspace kept that write: no change of the app's own record was refused.
    let log = device.log();
    assert!(
        !log.contains("a change was refused"),
        "{}: the app's write reached its record\n{}",
        device.label,
        log.lines().rev().take(40).collect::<Vec<_>>().join("\n")
    );
}

/// A person clicks New shortcut where the session draws it, and the draft it starts stands in
/// the window: the control a window's content offers is drawn where it says it is, so a pointer
/// aimed at its place acts.
#[test]
fn a_pointer_click_on_new_shortcut_opens_its_draft() {
    let device = opened_shortcuts();

    click(&device, "New shortcut");
    wait_drawn(&device, "Add trigger");
}

/// A person sets what starts a shortcut the way the window asks: a click opens the trigger
/// form, a click picks what starts it, a click puts the cursor in the app id field and what they
/// type reaches the app, and a click on Add trigger puts the trigger in the draft with the app
/// they named.
#[test]
fn a_person_types_an_app_id_and_adds_a_trigger() {
    let device = opened_shortcuts();

    click(&device, "New shortcut");
    wait_drawn(&device, "Add trigger");

    // Add trigger opens the form the person fills.
    click(&device, "Add trigger");
    wait_drawn(&device, "What starts it");

    // What starts it: the field's menu, opened and picked where it stands.
    click(&device, "What starts it");
    click(&device, CASE);
    wait_drawn(&device, &format!("What starts it, {CASE}"));

    // The app the trigger is for: the field takes the keyboard from the click, and what the
    // person types reaches the app's own tree, which its window draws.
    click(&device, "App id");
    device.type_text("so.tilo.chat");
    tilo_test_env::wait_for("the field to hold the app id", || {
        outline(&device).contains("so.tilo.chat").then_some(())
    });
    let shown = outline(&device);
    assert!(
        shown.contains("so.tilo.chat"),
        "the field holds the typed app id: {shown}"
    );

    // Add trigger takes the form into the draft: the trigger stands in it with the app.
    click(&device, "Add trigger");
    wait_drawn(&device, "An app opens");
    let shown = outline(&device);
    assert!(
        shown.contains("so.tilo.chat"),
        "the draft's trigger carries the app the person named: {shown}"
    );
}
