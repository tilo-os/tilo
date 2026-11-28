//! The controls the session lists of a tilolang app's window are the controls of the page it
//! draws now: a press by name, aimed at a control's rect, or made by assistive technology after
//! the window switched page reaches the page on screen, never the one before it.
//!
//! The session is the real one its device's daemon starts, and the app is a real tilolang package
//! in a window process of its own, so what it lists is what the app published of the frame it
//! presented.

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

/// The tilolang apps the walks open, whose windows are pages a sidebar switches.
const CLOCKS: &str = "so.tilo.clocks";
const TASKS: &str = "so.tilo.tasks";

/// Install `app` in `device`'s store, as a device the person installed it on has it: the package
/// this run built, and the runtimes its window starts on.
fn install(device: &Device, app: &str) {
    let built = crate::common::dev_apps::install_named(&device.store(), &[app]);
    assert_eq!(built, [app], "{} installs {app}", device.label);
    crate::common::dev_apps::install_runtimes(&device.store().join("runtimes"));
}

/// Whether `target` is drawn anywhere: a control a window holds open but shows nowhere stands
/// nowhere, and a click aimed at it lands on nothing.
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

/// Wait until `device` draws a control whose name is `name` or starts with it, and answer it.
#[track_caller]
fn wait_drawn(device: &Device, name: &str) -> Value {
    tilo_test_env::wait_for(format!("{} to draw {name:?}", device.label), || {
        named(device, name).into_iter().find(drawn)
    })
}

/// Press the drawn control `name` or the first one whose name starts with it, as assistive
/// technology activates it: the way a tool or a screen reader presses a button.
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

/// Click where `target` is drawn, as the person at `device` does: a rect read from a listing of
/// the page on screen lands on that page.
#[track_caller]
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

/// Type `text` as the person does, into whatever holds the keyboard.
fn type_text(device: &Device, text: &str) {
    let answer = device.ask(&json!({"op": "type", "text": text}));
    assert!(
        answer.get("err").is_none(),
        "{} types {text:?}: {answer}",
        device.label
    );
}

/// A window that switched page lists the controls of the page it drew, where it drew them, and a
/// press by name reaches the new page's control.
#[test]
fn a_window_that_switched_page_lists_and_presses_the_page_it_drew() {
    let ada = Person::new("Ada");
    let mut device = ada.device("Ada's laptop");
    install(&device, CLOCKS);
    device.start();
    device.open(CLOCKS);
    device.wait_for_window(CLOCKS);

    // The window opens on a page whose sidebar offers the others.
    wait_drawn(&device, "Alarms");
    // The person picks Alarms: the alarm page's own field stands where the timers page's
    // controls did.
    press(&device, "Alarms");
    let field = wait_drawn(&device, "What is this alarm for?");
    assert!(
        named(&device, "Tea")
            .into_iter()
            .all(|target| !drawn(&target)),
        "the timers page's start is not listed as drawn on the alarm page: {}\n{}",
        serde_json::to_string(&named(&device, "Tea")).unwrap_or_default(),
        device.diagnostic()
    );

    // The field's rect is of the page on screen: a press aimed there puts the keyboard in it,
    // and what the person types is the alarm's.
    click(&device, &field);
    type_text(&device, "Wake up");
    // A control only the alarm page draws, pressed by name: the press reaches this page's
    // button, and the alarm it added shows in the window it drew next.
    press(&device, "Add alarm");
    wait_drawn(&device, "Wake up");
}

/// A press by name made after the window switched view reaches a control only the new view
/// draws, and the app keeps what it made there.
#[test]
fn a_press_by_name_after_a_view_switch_lands_on_the_new_views_control() {
    let ada = Person::new("Ada");
    let mut device = ada.device("Ada's laptop");
    install(&device, TASKS);
    device.start();
    device.open(TASKS);
    device.wait_for_window(TASKS);

    // The window opens on the list of today, whose field says what a task added there is for.
    wait_drawn(&device, "New task for today");
    // The person picks Done: the page draws the same entry as one for a task that is done, and
    // no field of today's is on it.
    press(&device, "Done");
    let entry = wait_drawn(&device, "New task");
    assert_eq!(
        entry["name"].as_str(),
        Some("New task"),
        "the done page's entry is named for what it makes: {entry}"
    );

    // The entry's place on the page on screen takes the person's words: a rect read from the list
    // of the page before it would put the keyboard elsewhere.
    click(&device, &entry);
    type_text(&device, "Sweep the porch");
    let answer = device.ask(&json!({"op": "key", "key": "Enter"}));
    assert!(
        answer.get("err").is_none(),
        "{} presses Enter: {answer}",
        device.label
    );
    // The app kept the task: it stands in the inbox, where a task no place gave a status goes.
    press(&device, "Inbox");
    wait_drawn(&device, "Sweep the porch");
    // The row is named by its title and answers a press by it: the task's own page stands.
    press(&device, "Sweep the porch");
    wait_drawn(&device, "Add a subtask");
}
