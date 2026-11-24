//! An alarm the person sets on another device of theirs for this phone rings on the phone at its
//! moment from the phone's own platform, with no network: the phone, signed in by ticket, keeps
//! the workspace in its daemon, reads Clocks' trigger (`at a.at on a.rings_on`) from its copy of
//! the record, and arms the platform's alarm with what the ring will say. Moving the alarm moves
//! it, turning it off takes it back, an alarm for another device arms nothing here, and once the
//! phone is cut off the platform still rings it.
//!
//! The daemon reads its places from the process's environment, so this binary is one test, as
//! `ticket.rs` is. The platform's notifications are the one stand-in: what Android's
//! `AlarmManager` and `NotificationManager` do, behind the interface the phone mounts them by.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

// The server admits the phone's records through an Automerge worker: this binary is that
// worker, as the server host's own is.
tilo_automerge_isolate::test_worker!();

mod server;
mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use server::Server;
use tilo_workspace_service::apps::wakeups::{
    Notification, Notifier, NotifyError, Scheduled, SystemTime,
};
use tilolang_workspace::records::{Packages, member, with_tl_record};
use tilolang_workspace::runtime::{ResourceRef, Value};

const CLOCKS: &str = "so.tilo.clocks";

/// The phone's notifications, as Android keeps them: what is armed for later, and what it
/// showed. [`Platform::ring`] is the alarm's moment coming, which Android rings by itself.
#[derive(Default)]
struct Platform {
    armed: Mutex<BTreeMap<String, (Notification, SystemTime)>>,
    shown: Mutex<Vec<Notification>>,
}

impl Platform {
    fn armed(&self) -> Vec<Notification> {
        self.armed
            .lock()
            .unwrap()
            .values()
            .map(|(notification, _)| notification.clone())
            .collect()
    }

    fn ring(&self, tag: &str) {
        let (notification, _) = self.armed.lock().unwrap().remove(tag).expect("it is armed");
        self.shown.lock().unwrap().push(notification);
    }
}

impl Notifier for Platform {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.shown.lock().unwrap().push(notification.clone());
        Ok(())
    }

    fn withdraw(&self, _tag: &str) -> Result<(), NotifyError> {
        Ok(())
    }

    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        self.armed
            .lock()
            .unwrap()
            .insert(notification.tag.clone(), (notification.clone(), at));
        Ok(())
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        Ok(self
            .armed
            .lock()
            .unwrap()
            .iter()
            .map(|(tag, (_, at))| Scheduled {
                tag: tag.clone(),
                at: *at,
            })
            .collect())
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        self.armed.lock().unwrap().remove(tag);
        Ok(())
    }
}

/// Clocks' `action` on what `args` makes, run by the person on another device of theirs whose
/// writes reach the workspace through its server: the resource it answered, if it answered one. A
/// record's values stay on the thread that opened it, so the arguments are made there.
fn clocks(
    server: &Server,
    person: &str,
    action: &'static str,
    args: impl FnOnce() -> Vec<Value> + Send + 'static,
) -> Option<ResourceRef> {
    let packages: Packages = tilolang_workspace::records::built_in(CLOCKS)
        .unwrap()
        .expect("Clocks is built in");
    let who = member(tilo_actions::MemberId::person(person.to_owned()));
    server
        .runtime
        .block_on(with_tl_record(
            &server.workspaces(),
            &server.workspace,
            &packages,
            (who.clone(), person),
            move |opened| {
                let args = if action == "add_alarm" {
                    let root = opened.open_root()?;
                    std::iter::once(Value::resource(&root))
                        .chain(args())
                        .collect()
                } else {
                    args()
                };
                let answered = opened.invoke(&who, (CLOCKS, action), args, None)?;
                Ok(match answered {
                    Value::Ref(made) => Some(ResourceRef::clone(&made.target)),
                    _ => None,
                })
            },
        ))
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// An alarm at `hour`:`minute` that rings once, on `rings_on`.
fn alarm(
    server: &Server,
    person: &str,
    label: &str,
    (hour, minute): (i64, i64),
    rings_on: &str,
) -> ResourceRef {
    let (label, rings_on) = (label.to_owned(), rings_on.to_owned());
    clocks(server, person, "add_alarm", move || {
        vec![
            Value::text(label),
            Value::Int(hour),
            Value::Int(minute),
            Value::List(Vec::new()),
            Value::text(rings_on),
        ]
    })
    .expect("add_alarm answers the alarm")
}

/// What the phone's platform holds armed, once `holds` says it is what the phone should have
/// armed by now.
fn armed_once(
    platform: &Platform,
    what: &str,
    holds: impl Fn(&[Notification]) -> bool,
) -> Vec<Notification> {
    support::until(what, || {
        let armed = platform.armed();
        if holds(&armed) {
            Ok(armed)
        } else {
            Err(format!("the phone armed {armed:#?}"))
        }
    })
}

#[test]
fn an_alarm_set_for_the_phone_rings_there_with_no_network() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    // The person's server runs the language its program links, as a computer's daemon does.
    let tilolang = tilolang_workspace::host();
    server.workspaces().provide_tilolang(tilolang.clone());
    tilolang.records.serve(&server.workspaces());
    // The phone mounts on its daemon what its app does, its platform's notifications among it.
    let platform = Arc::new(Platform::default());
    tilo_runtime::device::capabilities::mount(tilo_phone::worker::capabilities(
        Arc::clone(&platform) as Arc<dyn Notifier>,
    ));
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    );
    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    support::until("the phone lists the person's workspace", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        if devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == server.workspace)
        {
            Ok(())
        } else {
            Err(format!("{devices:?}"))
        }
    });
    let (key, _) =
        tilo_device_identity::persist::load_or_create_device_key(&tilo_api::calls::device_keys())
            .expect("the phone's key");
    let phone_id = hex::encode(key.verifying_key().to_bytes());
    let person = hex::encode(server.person.verifying_key().to_bytes());

    // The person sets an alarm for another device of theirs, and one for the phone: the phone
    // arms the one for itself, saying what its ring will say, with its snooze.
    alarm(&server, &person, "Gym", (6, 0), &"ab".repeat(32));
    let standup = alarm(&server, &person, "Standup", (7, 0), &phone_id);
    let armed = armed_once(&platform, "the phone arms the alarm set for it", |armed| {
        armed
            .iter()
            .any(|notification| notification.title == "Standup")
    });
    assert_eq!(armed.len(), 1, "the gym alarm rings elsewhere: {armed:#?}");
    let first = armed[0].clone();
    assert_eq!(
        (first.body.as_str(), first.app.as_deref()),
        ("It is 7:00", Some("Clocks"))
    );
    assert_eq!(
        first
            .buttons
            .iter()
            .map(|button| button.label.as_str())
            .collect::<Vec<_>>(),
        ["Snooze 5 minutes"]
    );

    // Moved to half past, the phone moves the same alarm.
    clocks(&server, &person, "edit_alarm", {
        let standup = standup.clone();
        move || {
            vec![
                Value::resource(&standup),
                Value::text("Standup"),
                Value::Int(7),
                Value::Int(30),
                Value::List(Vec::new()),
            ]
        }
    });
    let moved = armed_once(&platform, "the phone moves the alarm", |armed| {
        armed.len() == 1 && armed[0].body == "It is 7:30"
    });
    assert_eq!(moved[0].tag, first.tag, "the same alarm, moved");

    // Turned off, the phone takes it back; on again, it arms it again.
    clocks(&server, &person, "set_alarm_enabled", {
        let standup = standup.clone();
        move || vec![Value::resource(&standup), Value::Bool(false)]
    });
    armed_once(
        &platform,
        "the phone takes the alarm back",
        <[Notification]>::is_empty,
    );
    clocks(&server, &person, "set_alarm_enabled", {
        let standup = standup.clone();
        move || vec![Value::resource(&standup), Value::Bool(true)]
    });
    let armed = armed_once(&platform, "the phone arms it again", |armed| {
        armed.len() == 1
    });

    // The phone is cut off from every other device: its server and the laptop go away. The
    // alarm's moment comes, and the phone's platform rings it by itself.
    drop(laptop);
    drop(server);
    platform.ring(&armed[0].tag);
    let shown = platform.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1, "{shown:#?}");
    assert_eq!(
        (shown[0].title.as_str(), shown[0].body.as_str()),
        ("Standup", "It is 7:30"),
        "the phone shows the alarm with no network"
    );

    phone.stop().expect("the phone's daemon stops");
}
