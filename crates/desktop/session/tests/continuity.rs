//! One app in front on the person's laptop is offered to their phone, and continued there: the
//! phone's bar draws the chip naming the app and the laptop, pressing it opens the same resource
//! the laptop showed, on the phone, and putting the chip away hides it until the laptop's front
//! window moves again, when the phone is offered the newer thing.
//!
//! Two real sessions of one person in one workspace, each showing the tilolang shell: the offer
//! travels through the record of their devices in their own workspace, and every step is a
//! published action — the shell's own chip, and the layout's `open` — with the checks read off
//! what the shell says and what each device's windows show.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use serde_json::json;
use tilo_actions::remote::CatalogueRow;

use crate::common::people::{
    Device, Site, seed_the_apps, two_devices_of_one_person_with_apps, waited,
};

/// The app the laptop has in front first, and whose notebook the phone continues.
const NOTES: &str = "so.tilo.notes";
/// The app the laptop moves to, so the phone is offered something newer.
const TASKS: &str = "so.tilo.tasks";
/// What the laptop calls itself in the offer.
const LAPTOP: &str = "Ada's laptop";

/// Whether the shell on `device` says `words`, as assistive technology reads it.
fn shows(device: &Device, words: &str) -> bool {
    let answer = device.ask(&json!({"op": "says", "text": words}));
    answer["answer"]["says"]
        .as_bool()
        .unwrap_or_else(|| panic!("{} answers `says` with {answer}", device.label))
}

/// The words the phone's chip carries for `app`: "Continue Notes from Ada's laptop".
fn chip(app: &str) -> String {
    format!("Continue {app} from {LAPTOP}")
}

/// The resource a window of `app` shows on `device`, as its place reads it: what a person gets
/// back when they continue it.
fn shown_resource(device: &Device, app: &str) -> Option<String> {
    let windows = device.ask(&json!({"op": "windows"}));
    windows["answer"]["windows"]
        .as_array()?
        .iter()
        .find(|row| row["app_id"] == app && row["resource"].is_string())
        .and_then(|row| row["resource"].as_str())
        .map(str::to_owned)
}

/// The root record of `app`, as the session publishes it: the note a person opens from search or
/// Files, by the same address the app's own rows carry.
fn root_of(device: &Device, app: &str) -> String {
    let wanted = format!("tl:{app}:root-");
    tilo_test_env::wait_for(format!("{}'s list of {app}'s roots", device.label), || {
        device
            .actions()
            .into_iter()
            .map(|row: CatalogueRow| row.resource.id)
            .find(|id| id.starts_with(&wanted))
    })
}

/// Open `resource` on `device`, as a person opening it from search or Files does: the layout's
/// own `open`, whose window shows the resource. A resource the layout cannot place yet is asked
/// for again, as a catalogue still filling would need.
fn open_here(device: &Device, resource: &str) {
    let open = device
        .offered("shell:layout", "open")
        .unwrap_or_else(|| panic!("{} publishes the layout's open", device.label));
    tilo_test_env::wait_for(format!("{} to open {resource}", device.label), || {
        let opened = device.act(&open, &json!({ "resource": resource }));
        opened.get("err").is_none().then_some(())
    });
}

/// Wait until a window of `app` on `device` shows `resource`.
fn waits_showing(what: &str, site: &Site, device: &Device, app: &str, resource: &str) {
    waited(what, site, &[device], || {
        shown_resource(device, app).as_deref() == Some(resource)
    });
}

/// The tab of the window of `app` on `device` that shows `resource`.
fn tab_showing(device: &Device, app: &str, resource: &str) -> Option<String> {
    let windows = device.ask(&json!({"op": "windows"}));
    windows["answer"]["windows"]
        .as_array()?
        .iter()
        .find(|row| row["app_id"] == app && row["resource"] == resource)
        .map(|row| row["tab"].to_string())
}

/// The named controls on `device`'s shell whose name ends with `suffix`.
fn targets_ending(device: &Device, suffix: &str) -> Vec<String> {
    let listed = device.ask(&json!({"op": "list_targets"}));
    listed["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|target| target["name"].as_str())
        .filter(|name| name.ends_with(suffix))
        .map(str::to_owned)
        .collect()
}

/// A window moved from the laptop onto a screen of the phone: the window's menu offers the phone's
/// screen, the phone opens the same app on the same record there, and the laptop's window closes.
/// With the phone off, the window stays on the laptop and the person is told why.
#[test]
fn a_window_moves_onto_a_screen_of_another_device() {
    seed_the_apps();
    let (site, _ada, laptop, mut phone) = two_devices_of_one_person_with_apps();

    let notebook = root_of(&laptop, NOTES);
    open_here(&laptop, &notebook);
    waits_showing(
        "the laptop shows its notebook",
        &site,
        &laptop,
        NOTES,
        &notebook,
    );
    let tab = tab_showing(&laptop, NOTES, &notebook).expect("the notebook's window");

    // The window's menu offers the phone's screen, by its name and the phone's.
    let open = laptop
        .offered("desktop:window-menu", "open")
        .expect("the laptop publishes the window menu");
    // The laptop learns the phone's screens from the record of the person's devices, so the menu
    // offers them once that has reached it: until then it is opened again.
    let screen = tilo_test_env::wait_for("the menu offers the phone's screen", || {
        let opened = laptop.act(&open, &json!({ "tab": tab }));
        assert!(opened.get("err").is_none(), "the menu opens: {opened}");
        if targets_ending(&laptop, "Move to display").is_empty() {
            return None;
        }
        laptop.press("Move to display");
        targets_ending(&laptop, " on Ada's phone")
            .into_iter()
            .next()
    });
    laptop.press(&screen);

    // The phone opens the same notebook, and the laptop's window goes.
    waits_showing(
        "the phone shows the notebook moved to it",
        &site,
        &phone,
        NOTES,
        &notebook,
    );
    waited(
        "the laptop's window closes once the phone shows it",
        &site,
        &[&laptop, &phone],
        || tab_showing(&laptop, NOTES, &notebook).is_none(),
    );

    // With the phone off, a move leaves the window where it is and says why.
    let tasks = root_of(&laptop, TASKS);
    open_here(&laptop, &tasks);
    waits_showing("the laptop shows its tasks", &site, &laptop, TASKS, &tasks);
    let tab = tab_showing(&laptop, TASKS, &tasks).expect("the tasks' window");
    let phone_publisher = phone.publisher().expect("the phone's publisher");
    phone.stop();
    waited("the laptop sees the phone go", &site, &[&laptop], || {
        !laptop
            .present(&site.workspace)
            .iter()
            .any(|(publisher, _)| *publisher == phone_publisher)
    });
    let move_tab = laptop
        .offered("desktop:chrome", "move-tab")
        .expect("the laptop publishes moving a window");
    let phone_key = hex::encode(phone.cert.device_public_key);
    let moved = laptop.act(&move_tab, &json!({"tab": tab, "to": {"device": phone_key}}));
    assert!(
        moved.get("err").is_none(),
        "an agent moves the window as a person does: {moved}"
    );
    waited(
        "the laptop says the window stayed, as the phone is offline",
        &site,
        &[&laptop],
        || shows(&laptop, "Ada's phone is offline"),
    );
    assert!(
        tab_showing(&laptop, TASKS, &tasks).is_some(),
        "the window stays on the laptop"
    );
}

/// Another of the person's devices has an app in front, and this one is offered to continue it
/// where it was: taken once, put away once, and offered again when that device's front moves.
#[test]
fn an_app_in_front_on_one_device_is_continued_on_another() {
    // Each session runs the development session's apps itself, seeded as `tilolang run` starts
    // them: a window of one is a place on a resource its own service holds.
    seed_the_apps();
    let (site, _ada, laptop, phone) = two_devices_of_one_person_with_apps();

    // The laptop opens its notebook: what it has in front is offered to the phone.
    let notebook = root_of(&laptop, NOTES);
    open_here(&laptop, &notebook);
    waits_showing(
        "the laptop shows its notebook",
        &site,
        &laptop,
        NOTES,
        &notebook,
    );

    // The phone's bar draws the chip, naming the app and the laptop.
    waited(
        "the phone's bar offers to continue the notebook",
        &site,
        &[&laptop, &phone],
        || shows(&phone, &chip("Notes")),
    );

    // Pressing it opens the same notebook here, through the layout's own open.
    phone.press(&chip("Notes"));
    waits_showing(
        "the phone opens the laptop's notebook",
        &site,
        &phone,
        NOTES,
        &notebook,
    );

    // The laptop moves to its tasks: the phone is offered the newer app, and puts it away.
    let tasks = root_of(&laptop, TASKS);
    open_here(&laptop, &tasks);
    waited(
        "the phone's bar offers the newer app",
        &site,
        &[&laptop, &phone],
        || shows(&phone, &chip("Tasks")),
    );
    phone.press(&format!("Dismiss Tasks from {LAPTOP}"));
    waited(
        "the chip the person put away goes",
        &site,
        &[&laptop, &phone],
        || !shows(&phone, &chip("Tasks")),
    );

    // The laptop's front moves again, back to the notebook: the phone is offered it again, since
    // a newer offer is a new one to answer.
    open_here(&laptop, &notebook);
    waited(
        "a newer offer of the notebook shows again",
        &site,
        &[&laptop, &phone],
        || shows(&phone, &chip("Notes")),
    );
}
