//! Quick settings' Devices section, drawn by the tilolang shell over the session's published
//! `session:devices` reading: a device asking for this one's sound is allowed or turned down where
//! it stands, a route is stopped and then dismissed, the sounds that can be sent are listed, and a
//! device this machine can drive offers its pointer row. Every press is the resource's own verb,
//! so what the panel shows and what an agent calls are one thing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_compositor::KeyRepeat;
use tilo_session::devices::{RESOURCE, resource};
use tilo_session::{Options, Service, Session};
use tilo_session_core::shell::OVERLAYS;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

const WIDTH: i32 = 1280;
/// Tall enough for the whole panel: its sections stand one below another.
const TALL: i32 = 4000;

/// One press the panel sent to the devices resource.
type Press = (String, Map<String, Value>);

/// The session's `session:devices` resource, published by the test with the remote audio service's
/// rows and one other device: what the panel draws, and what a press on it reaches.
struct Devices {
    reading: Value,
    calls: Rc<RefCell<Vec<Press>>>,
}

impl Service for Devices {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| self.reading.clone())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<tilo_service::Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        self.calls
            .borrow_mut()
            .push((action.to_owned(), arguments.clone()));
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }
}

/// The reading the panel draws: a device asking for this one's sound, two routes (one stopped), the
/// sounds this device can send, and another device that is here.
fn reading() -> Value {
    json!({
        "devices": [{
            "id": "laptop",
            "label": "Ada's laptop",
            "online": true,
            "nearby": true,
            "local": false,
            "display": true,
            "wakeable": false,
            "audio_output": true,
        }],
        "playing": "Clair de lune",
        "chosen": 7,
        "sounds": [
            {"token": 7, "label": "Everything this device plays", "application": false},
            {"token": 8, "label": "Music", "application": true}
        ],
        "routes": [
            {"id": "10", "label": "Clair de lune", "detail": "Playing on Ada's laptop",
             "stopped": false},
            {"id": "11", "label": "The news", "detail": "It stopped", "stopped": true}
        ],
        "asks": [{"id": "12", "media": "Clair de lune", "source": "Ada's laptop"}],
        "signed_in": true,
        "workspaces": [],
        "refused": ""
    })
}

/// A headless session drawing the shell, with `devices` publishing the remote audio rows.
fn session(calls: &Rc<RefCell<Vec<Press>>>) -> Session {
    let mut session = Session::headless(
        Options {
            person: "Pat".to_owned(),
            services: vec![
                Box::new(tilo_settings_service::SettingsService::new()),
                Box::new(Devices {
                    reading: reading(),
                    calls: Rc::clone(calls),
                }),
            ],
            collab: false,
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
    )
    .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

/// Open the panel through the action its shortcut runs, and settle it.
fn open_quick_settings(session: &mut Session) {
    session
        .invoke(OVERLAYS, "open-surface", &Map::from_iter([(
            "part".to_owned(),
            json!("quick-settings"),
        )]))
        .expect("quick settings opens");
    crate::common::wait_for(session, "the panel to show the devices", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        let labels = session.accessible_labels();
        labels.iter().any(|label| label == "Allow").then_some(())
    });
}

/// Press the shell's control named `name`, all of it a second time once it stops moving, as a
/// person presses what they see at rest.
#[track_caller]
fn press(session: &mut Session, name: &str) {
    crate::common::wait_for(session, format!("the control {name:?}"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        crate::common::control_on_screen(session, name).then_some(())
    });
    crate::common::press_control(session, name);
}

/// The presses `calls` took, once the last one is `part` on `row`.
#[track_caller]
fn until_pressed(session: &mut Session, calls: &Rc<RefCell<Vec<Press>>>, row: &str, part: &str) {
    crate::common::wait_for(
        session,
        format!("a press of {part} on {row}"),
        |session| {
            session.settle_services();
            session.step(REFRESH).expect("step");
            calls
                .borrow()
                .iter()
                .any(|(action, arguments)| {
                    action == "press"
                        && arguments.get("row").and_then(Value::as_str) == Some(row)
                        && arguments.get("part").and_then(Value::as_str) == Some(part)
                })
                .then_some(())
        },
    );
}

/// Whether the shell says `words` anywhere: what a screen reader hears.
fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label.contains(words))
}

#[test]
fn a_device_asking_for_the_sound_is_allowed_and_turned_down_where_it_stands() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    assert!(
        says(&session, "Play Clair de lune here?"),
        "the ask names the sound and who asks: {:?}",
        session.accessible_labels()
    );
    press(&mut session, "Allow");
    until_pressed(&mut session, &calls, "audio:ask:12", "allow");
    press(&mut session, "Decline");
    until_pressed(&mut session, &calls, "audio:ask:12", "decline");
    assert!(
        calls
            .borrow()
            .iter()
            .filter(|(action, _)| action == "press")
            .count()
            >= 2,
        "both answers reached the resource: {:?}",
        calls.borrow()
    );
}

#[test]
fn a_route_is_stopped_and_the_stopped_one_dismissed() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    assert!(
        says(&session, "Playing on Ada's laptop"),
        "the route says what it is doing: {:?}",
        session.accessible_labels()
    );
    press(&mut session, "Stop");
    until_pressed(&mut session, &calls, "audio:route:10", "action");
    press(&mut session, "Dismiss");
    until_pressed(&mut session, &calls, "audio:route:11", "action");
}

#[test]
fn the_sounds_to_send_are_listed_and_one_is_chosen() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    assert!(
        says(&session, "Sounds you can send"),
        "the section lists what can go: {:?}",
        session.accessible_labels()
    );
    press(&mut session, "Music");
    until_pressed(&mut session, &calls, "audio:sound:8", "row");
    assert!(
        says(&session, "Takes the sound you send"),
        "the device that can take it says so: {:?}",
        session.accessible_labels()
    );
}

#[test]
fn a_device_offers_the_row_that_drives_it_with_this_pointer() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    press(&mut session, "Drive Ada's laptop with this pointer");
    until_pressed(&mut session, &calls, "laptop", "control");
}

/// The session's slot for a panel beside the caret that asked for it: quick settings opens clear
/// of the line the person types on, and the shell draws it there rather than under the bar's own
/// corner.
#[test]
fn quick_settings_stands_beside_the_caret_that_asked_for_it() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    session
        .invoke(
            OVERLAYS,
            "open-surface",
            &Map::from_iter([
                ("part".to_owned(), json!("quick-settings")),
                (
                    "anchor".to_owned(),
                    json!("beside:600,300,602,320,6"),
                ),
            ]),
        )
        .expect("quick settings opens beside the caret");

    // What the session settled: clear of the caret's line (y1 320 plus the gap) and to its right.
    let place = crate::common::wait_for(&mut session, "the stack to place the panel", |session| {
        session.step(REFRESH).expect("step");
        session
            .read(OVERLAYS)
            .and_then(|reading| reading.get("place").cloned())
    });
    let x = place["x"].as_f64().expect("x");
    let y = place["y"].as_f64().expect("y");
    let w = place["w"].as_f64().expect("w");
    assert!((x - 608.0).abs() < 0.5, "clear of the caret: {place}");
    assert!(y >= 326.0, "under the line it types on: {place}");

    // The shell draws it there: the panel's heading stands inside the room the stack settled,
    // beside the caret, not under the bar's trailing corner.
    let (bx, by, bw, _) = crate::common::wait_for(&mut session, "the panel drawn", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        session
            .shell_control("Quick settings")
            .or_else(|| session.shell_control("Dark mode"))
    });
    assert!(
        bx >= x - 0.5 && bx + bw <= x + w + 0.5 && by >= y - 0.5,
        "the panel stands in the room the session placed: {bx},{by} outside {place}"
    );
    assert!(
        by < y + 80.0,
        "at the top of that room, not under the bar: {by} vs {y}"
    );
}

/// What a key sends, evdev codes.
const KEY_TAB: u32 = 15;
const KEY_ENTER: u32 = 28;
const KEY_V: u32 = 47;
const KEY_LEFT_META: u32 = 125;

fn key(session: &mut Session, keycode: u32, pressed: bool) {
    session
        .compositor()
        .input(tilo_compositor::InputEvent::Key { keycode, pressed })
        .expect("the key");
    session.step(REFRESH).expect("step");
}

fn tap(session: &mut Session, keycode: u32) {
    key(session, keycode, true);
    key(session, keycode, false);
}

/// Super+V, the chord that opens the panel to paste.
fn press_paste(session: &mut Session) {
    key(session, KEY_LEFT_META, true);
    tap(session, KEY_V);
    key(session, KEY_LEFT_META, false);
}

/// The clipboard's recent copies are the panel's first section: choosing one pastes it where the
/// person types — the panel closes as it goes — and the keyboard chooses one the same way.
#[test]
fn the_clipboard_pastes_a_copy_and_the_keyboard_chooses_one() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    let copied = "the copied line";
    session
        .compositor()
        .set_selection(
            tilo_compositor::Selection::Clipboard,
            vec![(
                tilo_compositor::clipboard_history::TEXT_MIME.to_owned(),
                copied.as_bytes().to_vec(),
            )],
        )
        .expect("the app copies");
    crate::common::wait_for(&mut session, "the copy in the history", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        session
            .read("session:clipboard")
            .and_then(|reading| reading["copies"].as_array()?.iter().find(|copy| {
                copy["text"].as_str() == Some(copied)
            }).cloned())
    });

    // Super+V opens the panel where the person types, and the copy stands first.
    press_paste(&mut session);
    crate::common::wait_for(&mut session, "the panel with the copy", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        (here(session) == "quick-settings"
            && crate::common::control_on_screen(session, copied).is_some())
        .then_some(())
    });
    press(&mut session, copied);
    // Choosing it pastes it: the panel goes down so the paste lands in the field it was opened
    // for.
    crate::common::wait_for(&mut session, "the paste to take the panel down", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        (here(session) != "quick-settings").then_some(())
    });

    // Opened again, the keyboard reaches the same copy and Enter chooses it.
    press_paste(&mut session);
    crate::common::wait_for(&mut session, "the panel with the copy again", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        (here(session) == "quick-settings"
            && crate::common::control_on_screen(session, copied).is_some())
        .then_some(())
    });
    tap(&mut session, KEY_TAB);
    tap(&mut session, KEY_ENTER);
    crate::common::wait_for(
        &mut session,
        "the keyboard's paste to take the panel down",
        |session| {
            session.settle_services();
            session.step(REFRESH).expect("step");
            (here(session) != "quick-settings").then_some(())
        },
    );
}

/// The surface the shell shows over the windows now, as the session publishes it.
fn here(session: &Session) -> String {
    session
        .read(OVERLAYS)
        .and_then(|reading| reading["here"].as_str().map(str::to_owned))
        .unwrap_or_default()
}
