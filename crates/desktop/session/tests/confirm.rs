//! An action that cannot be undone asks before it runs, as the system has always asked: the shell's
//! views press it, the session keeps the call and publishes the question, and only the person's yes
//! runs it. Saving a network and a Bluetooth accessory are the two flows here: forgetting either is
//! a call whose contract waits, and Cancel (or Escape) runs nothing.

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
use tilo_actions::{ApiError, ResourceSpec};
use tilo_connectivity_service::model::{
    Accessory, Bluetooth, Network, SavedKind, SavedNetwork, Wifi,
};
use tilo_connectivity_service::{
    BLUETOOTH, NETWORK, bluetooth_resource, network_resource,
};
use tilo_session::Service;
use tilo_session::{Options, Session};
use tilo_session_core::shell::OVERLAYS;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

const WIDTH: i32 = 1280;
/// Tall enough for the whole panel.
const TALL: i32 = 4000;

/// What a key sends, evdev codes.
const KEY_ESCAPE: u32 = 1;

/// The confirmation's resource, as the session publishes it.
const CONFIRM: &str = "desktop:confirm";

/// One call the service carried out: the resource, and the action.
type Call = (String, String);

/// The connectivity resources the panel draws, published by the test with the saved network and the
/// paired accessory it offers to forget.
struct Connectivity {
    network: Network,
    bluetooth: Bluetooth,
    calls: Rc<RefCell<Vec<Call>>>,
}

impl Service for Connectivity {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            network_resource(&self.network),
            bluetooth_resource(&self.bluetooth),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            NETWORK => serde_json::to_value(&self.network).ok(),
            BLUETOOTH => serde_json::to_value(&self.bluetooth).ok(),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<tilo_service::Started, ApiError> {
        self.calls
            .borrow_mut()
            .push((resource.to_owned(), action.to_owned()));
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }
}

/// A network this device keeps, and a paired accessory.
fn network() -> Network {
    Network {
        wifi: Wifi {
            available: true,
            on: true,
            ..Wifi::default()
        },
        saved: vec![SavedNetwork {
            address: tilo_resource::Handle::new("saved-network:uuid-home"),
            name: "Home".to_owned(),
            id: "uuid-home".to_owned(),
            kind: SavedKind::Wifi,
            ..SavedNetwork::default()
        }],
        ..Network::default()
    }
}

fn bluetooth() -> Bluetooth {
    Bluetooth {
        available: true,
        on: true,
        accessories: vec![Accessory {
            handle: tilo_resource::Handle::new("bluetooth-accessory:AA-BB"),
            name: "Headphones".to_owned(),
            address: "AA-BB".to_owned(),
            ..Accessory::default()
        }],
        ..Bluetooth::default()
    }
}

/// A headless session drawing the shell over the connectivity reading, with the calls the service
/// carried out.
fn session(calls: &Rc<RefCell<Vec<Call>>>) -> Session {
    let mut session = Session::headless(
        Options {
            person: "Pat".to_owned(),
            services: vec![
                Box::new(tilo_settings_service::SettingsService::new()),
                Box::new(Connectivity {
                    network: network(),
                    bluetooth: bluetooth(),
                    calls: Rc::clone(calls),
                }),
            ],
            collab: false,
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
    )
    .expect("session");
    session.compositor().set_key_repeat(tilo_compositor::KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

/// Open quick settings through the action its shortcut runs.
fn open_quick_settings(session: &mut Session) {
    session
        .invoke(
            OVERLAYS,
            "open-surface",
            &Map::from_iter([("part".to_owned(), json!("quick-settings"))]),
        )
        .expect("quick settings opens");
    until_control(session, "Home");
}

/// Press the shell's control named `name`, once it stands still.
#[track_caller]
fn press(session: &mut Session, name: &str) {
    until_control(session, name);
    crate::common::press_at(session, name, WIDTH, TALL);
}

/// Run frames until the shell shows the control `name`.
#[track_caller]
fn until_control(session: &mut Session, name: &str) {
    crate::common::wait_for(session, format!("the control {name:?}"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        crate::common::on_screen_at(session, name, WIDTH, TALL).then_some(())
    });
}

/// The confirmation as the session publishes it.
fn confirm(session: &Session) -> Value {
    session
        .read(CONFIRM)
        .expect("the session publishes the confirmation")
}

/// Run frames until a question waits for the person, and answer with its reading.
#[track_caller]
fn until_asked(session: &mut Session) -> Value {
    crate::common::wait_for(session, "the question to stand", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        let reading = confirm(session);
        (reading["open"] == json!(true)).then_some(reading)
    })
}

/// Run frames until the question is gone.
#[track_caller]
fn until_answered(session: &mut Session) {
    crate::common::wait_for(session, "the question to go", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        (confirm(session)["open"] == json!(false)).then_some(())
    });
}

/// Run frames until the service carried out `call`.
#[track_caller]
fn until_called(session: &mut Session, calls: &Rc<RefCell<Vec<Call>>>, resource: &str, action: &str) {
    crate::common::wait_for(
        session,
        format!("{action} on {resource}"),
        |session| {
            session.settle_services();
            session.step(REFRESH).expect("step");
            calls
                .borrow()
                .iter()
                .any(|(held, done)| held == resource && done == action)
                .then_some(())
        },
    );
}

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

#[test]
fn forgetting_a_saved_network_asks_first_and_cancel_runs_nothing() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    // The saved network's own menu offers to forget it.
    press(&mut session, "This network");
    press(&mut session, "Forget this network");

    // The call waits for the person: the question names the action, what it acts on, and that
    // nothing takes it back.
    let asked = until_asked(&mut session);
    let accept = asked["accept"].as_str().expect("the answer that runs it").to_owned();
    assert_eq!(
        asked["title"].as_str(),
        Some(accept.as_str()),
        "the question is the action, by its own label: {asked}"
    );
    assert!(
        asked["message"]
            .as_str()
            .is_some_and(|message| message.contains("Home")),
        "what it acts on is named: {asked}"
    );
    assert!(
        calls.borrow().is_empty(),
        "nothing ran before the person answered: {:?}",
        calls.borrow()
    );

    // Cancel runs nothing, and the question goes.
    let cancel = asked["cancel"].as_str().expect("the way out").to_owned();
    press(&mut session, &cancel);
    until_answered(&mut session);
    assert!(
        calls.borrow().is_empty(),
        "Cancel ran nothing: {:?}",
        calls.borrow()
    );

    // Asked again, the person's yes runs the call the press made.
    press(&mut session, "This network");
    press(&mut session, "Forget this network");
    let asked = until_asked(&mut session);
    let accept = asked["accept"].as_str().expect("the answer that runs it").to_owned();
    press(&mut session, &accept);
    until_called(
        &mut session,
        &calls,
        "saved-network:uuid-home",
        "forget",
    );
    assert_eq!(
        calls.borrow().as_slice(),
        &[("saved-network:uuid-home".to_owned(), "forget".to_owned())],
        "the call ran once, on the row the press named"
    );
}

#[test]
fn forgetting_a_bluetooth_accessory_asks_first_and_escape_runs_nothing() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    // The accessory's row offers to forget it.
    press(&mut session, "Forget this device");
    let asked = until_asked(&mut session);
    assert!(
        asked["message"]
            .as_str()
            .is_some_and(|message| message.contains("Headphones")),
        "the question names the accessory: {asked}"
    );
    assert!(calls.borrow().is_empty(), "nothing runs unasked");

    // Escape puts the question away and runs nothing.
    tap(&mut session, KEY_ESCAPE);
    until_answered(&mut session);
    assert!(
        calls.borrow().is_empty(),
        "Escape ran nothing: {:?}",
        calls.borrow()
    );

    // Asked again, the person's yes runs it.
    press(&mut session, "Forget this device");
    let asked = until_asked(&mut session);
    let accept = asked["accept"].as_str().expect("the answer that runs it").to_owned();
    press(&mut session, &accept);
    until_called(&mut session, &calls, "bluetooth-accessory:AA-BB", "forget");
    assert_eq!(
        calls.borrow().as_slice(),
        &[(
            "bluetooth-accessory:AA-BB".to_owned(),
            "forget".to_owned()
        )],
        "the accessory the press named is the one forgotten"
    );
}
