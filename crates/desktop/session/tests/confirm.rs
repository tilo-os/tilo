//! An action that cannot be undone asks before it runs, as the system has always asked: the
//! shell's views press it, the device's dispatch keeps the call as the person's asked record,
//! the shell draws the question from that record, and only their yes runs it. Saving a network
//! and a Bluetooth accessory are the two flows here: forgetting either is a call whose contract
//! waits, and declining runs nothing.

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
use tilo_compositor::OutputConfig;
use tilo_connectivity_service::model::{
    Accessory, Bluetooth, Network, SavedKind, SavedNetwork, Wifi,
};
use tilo_connectivity_service::{BLUETOOTH, NETWORK, bluetooth_resource, network_resource};
use tilo_session::Service;
use tilo_session::{Options, Session};
use tilo_session_core::shell::OVERLAYS;

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

const WIDTH: i32 = 1280;
/// Tall enough for the whole panel and the questions that stand with it.
const TALL: i32 = 4000;

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
            profile: "uuid-home".to_owned(),
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

/// A headless session as the shell of a device daemon of the test's own, so a press that waits for
/// the person reaches the device's one dispatch and is kept as their asked record, over the
/// connectivity reading and the calls the service carried out.
fn session(calls: &Rc<RefCell<Vec<Call>>>) -> Session {
    let mut session = crate::common::on_its_daemon(
        &[OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
        Options {
            services: vec![
                Box::new(settings_sections::service::SettingsService::new()),
                Box::new(Connectivity {
                    network: network(),
                    bluetooth: bluetooth(),
                    calls: Rc::clone(calls),
                }),
            ],
            collab: false,
            ..Options::default()
        },
    );
    session
        .compositor()
        .set_key_repeat(tilo_compositor::KeyRepeat {
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
        crate::common::on_screen_at(session, name, WIDTH, TALL).map(|_| ())
    });
}

/// Run frames until the shell shows no control named `name`.
#[track_caller]
fn until_gone(session: &mut Session, name: &str) {
    crate::common::wait_for(session, format!("{name} to go"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        crate::common::on_screen_at(session, name, WIDTH, TALL)
            .is_none()
            .then_some(())
    });
}

/// Run frames until the service carried out `call`.
#[track_caller]
fn until_called(
    session: &mut Session,
    calls: &Rc<RefCell<Vec<Call>>>,
    resource: &str,
    action: &str,
) {
    crate::common::wait_for(session, format!("{action} on {resource}"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        calls
            .borrow()
            .iter()
            .any(|(held, done)| held == resource && done == action)
            .then_some(())
    });
}

#[test]
fn forgetting_a_saved_network_asks_first_and_declining_runs_nothing() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = tilo_ui_base::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    // The saved network's own menu offers to forget it.
    press(&mut session, "This network");
    press(&mut session, "Forget this network");

    // The call waits for the person as their asked record, drawn by the shell's one question: it
    // names the action and offers the way out.
    until_control(&mut session, "Don't allow");
    assert!(
        calls.borrow().is_empty(),
        "nothing ran before the person answered: {:?}",
        calls.borrow()
    );

    // Declining runs nothing, and the question goes.
    press(&mut session, "Don't allow");
    until_gone(&mut session, "Don't allow");
    assert!(
        calls.borrow().is_empty(),
        "declining ran nothing: {:?}",
        calls.borrow()
    );

    // Asked again, the person's yes runs the call the press made.
    press(&mut session, "This network");
    press(&mut session, "Forget this network");
    // The yes is named for what the call does, as the service declares the action.
    press(&mut session, "Forget the network");
    until_called(&mut session, &calls, "saved-network:uuid-home", "forget");
    assert_eq!(
        calls.borrow().as_slice(),
        &[("saved-network:uuid-home".to_owned(), "forget".to_owned())],
        "the call ran once, on the row the press named"
    );
}

#[test]
fn forgetting_a_bluetooth_accessory_asks_first_and_declining_runs_nothing() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = tilo_ui_base::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);
    open_quick_settings(&mut session);

    // The accessory's row offers to forget it.
    press(&mut session, "Forget this device");
    until_control(&mut session, "Don't allow");
    assert!(calls.borrow().is_empty(), "nothing runs unasked");

    // Declining puts the question away and runs nothing.
    press(&mut session, "Don't allow");
    until_gone(&mut session, "Don't allow");
    assert!(
        calls.borrow().is_empty(),
        "declining ran nothing: {:?}",
        calls.borrow()
    );

    // Asked again, the person's yes runs it.
    press(&mut session, "Forget this device");
    press(&mut session, "Forget the device");
    until_called(&mut session, &calls, "bluetooth-accessory:AA-BB", "forget");
    assert_eq!(
        calls.borrow().as_slice(),
        &[("bluetooth-accessory:AA-BB".to_owned(), "forget".to_owned())],
        "the accessory the press named is the one forgotten"
    );
}

thread_local! {
    static TAKEN: RefCell<Vec<tilo_session::SessionAction>> = const { RefCell::new(Vec::new()) };
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "the session asks its recorder for a result, as it asks every app's own"
)]
fn record_session_action(action: tilo_session::SessionAction) -> Result<(), String> {
    TAKEN.with_borrow_mut(|taken| taken.push(action));
    Ok(())
}

/// A session in a window of another desktop asks before it shuts down, and the person's yes on
/// the system's question ends that session and nothing else: no power action reaches the machine
/// the window runs on.
#[test]
fn shutting_down_a_session_in_a_window_asks_and_ends_only_the_session() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = tilo_ui_base::motion::testing::settled();
    let mut session = crate::common::on_its_daemon(
        &[OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
        Options {
            machine: tilo_session::Machine::Window,
            session_actions: record_session_action,
            collab: false,
            ..Options::default()
        },
    );
    session
        .invoke(tilo_session::SESSION_RESOURCE, "power-menu", &Map::new())
        .expect("the power menu opens");
    until_control(&mut session, "Shut down");
    let row = crate::common::on_screen_at(&session, "Shut down", WIDTH, TALL).expect("the row");
    press(&mut session, "Shut down");
    until_control(&mut session, "Don't allow");
    assert!(!session.ended(), "the question alone shuts nothing down");

    // The yes stands in the question, named for the action as the session declares it.
    crate::common::press_where(&mut session, "the question's Shut down", |session| {
        session
            .shell_controls("Shut down")
            .into_iter()
            .find(|&rect| rect != row)
    });
    crate::common::wait_for(&mut session, "the session to end", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        session.ended().then_some(())
    });
    assert!(
        TAKEN.with_borrow(Vec::is_empty),
        "shutting down did not reach the host's machine"
    );
}
