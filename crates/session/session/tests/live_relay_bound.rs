//! A relay held at its admission bound by endpoints nobody knows, and a device of the workspace
//! that asks it for the workspace's live session meanwhile: two real sessions of one person, one
//! hosting the workspace and so relaying its live session, the other dialling it.
//!
//! Anything that reaches a session's endpoint can open the live session's protocol and say nothing,
//! and the relay holds each such session in admission for as long as a preface may take. Before
//! the relay told endpoints apart, sixteen of them held every place it admits through, and a
//! device of the workspace that asked meanwhile was told busy for as long as they kept it up.
//! The relay now holds endpoints no roster names in places of their own: they are told busy among
//! themselves, and the workspace's device is taken in at once, never told busy and never refused.
//!
//! What a device that *is* told busy reads and does is checked where a relay's places can be
//! filled by a workspace's own devices: the relay's tests hold sixteen of them in admission
//! through its served endpoint, and the session's own tests read the access and the words of a
//! live session whose relay was busy. Sixteen further sessions of one workspace are sixteen processes
//! with a shell each, more than a suite's machine starts for one test.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use tilo_live::native::ALPN;
use tilo_live::preface::CLOSE_BUSY;
use tilo_live::relay::{MAX_ADMITTING, MAX_ADMITTING_STRANGERS};

use crate::common::people::{Device, log_sessions_at, new_workspace, two_devices_of_one_person};

/// What the devices' sessions log here: the live session says which relay it missed and why only
/// below `info`.
const LOGGED: &str = "info,tilo_session::live=debug";
/// How `device`'s session says the live session of `workspace` stands: the workspace's row in `live`
/// of its shares' reading, `Null` while the session names no live session for it.
fn live_session_of(device: &Device, workspace: &str) -> Value {
    device.state()["state"]["sharing"]["live"]
        .as_array()
        .and_then(|live| {
            live.iter()
                .find(|live_session| live_session["workspace"].as_str() == Some(workspace))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// What `device` says of who hosts `workspace`: `Null` while it does not hold it.
fn hosting_of(device: &Device, workspace: &str) -> Value {
    device.ask(&json!({"op": "keeping_devices"}))["answer"]["workspaces"]
        .as_array()
        .and_then(|held| {
            held.iter()
                .find(|row| row["workspace"].as_str() == Some(workspace))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// How each of `devices` says the live session of `workspace` stands, and what its session's live session
/// logged last: what a step that did not happen fails with.
fn report(workspace: &str, devices: &[&Device]) -> String {
    let mut out = String::new();
    for device in devices {
        let log = device.log();
        let said: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("tilo_session::live"))
            .collect();
        let _ = writeln!(
            out,
            "== {} (device {}) ==\nlive session: {}\n-- the live session, last --\n{}",
            device.label,
            hex::encode(device.cert.device_public_key),
            live_session_of(device, workspace),
            said[said.len().saturating_sub(60)..].join("\n"),
        );
    }
    out
}

/// Wait until `holds`, and if it does not within `within`, fail with how the live session stands on each
/// of `devices`.
#[track_caller]
fn eventually(
    what: &str,
    within: Duration,
    workspace: &str,
    devices: &[&Device],
    mut holds: impl FnMut() -> bool,
) {
    let deadline = Instant::now() + within;
    while !holds() {
        assert!(
            Instant::now() < deadline,
            "{what}: not within {within:?}\n{}",
            report(workspace, devices)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A workspace `maker` makes and hosts, which `other`, a further device of the same person, takes
/// from the person's list and holds: its id.
fn made_on(maker: &Device, other: &Device) -> String {
    let before = maker.on_shell();
    let made = maker.act_wait(&new_workspace(), &json!({}));
    assert_eq!(
        made["update"]["state"],
        json!("succeeded"),
        "{} makes a workspace: {made}\n{}",
        maker.label,
        maker.diagnostic()
    );
    let made_here = || {
        maker.on_shell().into_iter().find(|workspace| {
            !before.contains(workspace)
                && hosting_of(maker, workspace)["hosted_here"] == json!(true)
        })
    };
    tilo_test_env::wait_until(
        format!("{}'s shell holds the workspace it made", maker.label),
        || made_here().is_some(),
    );
    let made = made_here().expect("the workspace the device made");
    eventually(
        &format!("{} holds {made}, made on {}", other.label, maker.label),
        Duration::from_secs(90),
        &made,
        &[maker, other],
        || other.held().contains(&made),
    );
    made
}

/// How often `device`'s session said that the relay of `workspace` did not take it in because it
/// was admitting as many devices as it takes.
fn told_busy(device: &Device, workspace: &str) -> usize {
    device
        .log()
        .lines()
        .filter(|line| {
            line.contains("tilo_session::live")
                && line.contains(workspace)
                && line.contains("busy=true")
        })
        .count()
}

/// How often `device`'s session said that the relay of `workspace` refused it.
fn told_refused(device: &Device, workspace: &str) -> usize {
    device
        .log()
        .lines()
        .filter(|line| {
            line.contains("tilo_session::live")
                && line.contains(workspace)
                && line.contains("refused=true")
        })
        .count()
}

/// An endpoint of nobody's on this machine's loopback relay, as the devices' sessions are.
async fn anybody() -> iroh::Endpoint {
    let key = test_server::fresh_signing_key("an endpoint that says nothing");
    test_server::endpoint(iroh::SecretKey::from_bytes(&key.to_bytes())).await
}

/// The code the other end closed a connection with, when it closed it.
fn close_code(error: &iroh::endpoint::ConnectionError) -> Option<u64> {
    match error {
        iroh::endpoint::ConnectionError::ApplicationClosed(closed) => {
            Some(u64::from(closed.error_code))
        }
        _ => None,
    }
}

/// A device's relay held by endpoints nobody knows: [`MAX_ADMITTING`] connections, each from an
/// endpoint of its own, that open the live session's protocol on its endpoint and say nothing. The
/// relay keeps as many of them in admission as it has places for strangers
/// ([`MAX_ADMITTING_STRANGERS`]), each for as long as a preface may take, and tells the rest it is
/// busy; each is opened again the moment it is closed, so those places stay taken.
struct Held {
    runtime: tokio::runtime::Runtime,
    /// Where the relay is reached.
    at: iroh::EndpointAddr,
    /// The endpoint [`Self::told`] asks from.
    asking: iroh::Endpoint,
    /// Told when the relay is let go ([`Self::release`]).
    released: tokio::sync::watch::Sender<bool>,
    /// The connections that hold the relay, each kept open by a task of its own.
    holding: Vec<tokio::task::JoinHandle<()>>,
}

impl Held {
    /// Hold the relay `relay`'s session serves, from now until this is released.
    fn hold(relay: &Device) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the connections that hold the relay");
        // A device's session is reached on its device key, through the relay its profile names.
        let id = iroh::EndpointId::from_bytes(&relay.cert.device_public_key)
            .expect("a device key names an endpoint");
        let through: iroh::RelayUrl = test_server::loopback_url()
            .parse()
            .expect("the loopback relay's address");
        let at = iroh::EndpointAddr::new(id).with_relay_url(through);
        let (released, _) = tokio::sync::watch::channel(false);
        let holding = (0..MAX_ADMITTING)
            .map(|_| runtime.spawn(hold_one(at.clone(), released.subscribe())))
            .collect();
        let asking = runtime.block_on(anybody());
        Self {
            runtime,
            at,
            asking,
            released,
            holding,
        }
    }

    /// What the relay tells one more session that asks now: the code it closes it with at once,
    /// or `None` when it took it into admission or was not reached.
    fn told(&self) -> Option<u64> {
        self.runtime.block_on(async {
            let connection = self.asking.connect(self.at.clone(), ALPN).await.ok()?;
            let closed = tokio::time::timeout(Duration::from_secs(1), connection.closed())
                .await
                .ok()?;
            close_code(&closed)
        })
    }

    /// Let go of the relay: every connection that held it closes.
    fn release(self) {
        self.released.send_replace(true);
        self.runtime.block_on(async {
            for held in self.holding {
                held.await.expect("a connection that held the relay closes");
            }
            self.asking.close().await;
        });
    }
}

/// Keep one of the relay's places in admission taken until `released` says to let go: open the
/// live session's protocol at `at`, say nothing, and open it again whenever the relay closes it.
async fn hold_one(at: iroh::EndpointAddr, mut released: tokio::sync::watch::Receiver<bool>) {
    let endpoint = anybody().await;
    while !*released.borrow() {
        let Ok(connection) = endpoint.connect(at.clone(), ALPN).await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        tokio::select! {
            closed = connection.closed() => {
                if close_code(&closed) == Some(u64::from(CLOSE_BUSY)) {
                    // Another session holds this one's place for a moment.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            changed = released.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }
    endpoint.close().await;
}

/// A person's laptop is started while sixteen endpoints nobody knows hold the relay of the phone
/// that hosts their workspace, opening its protocol and saying nothing, again and again. They
/// take the places the relay has for strangers and are told busy among themselves. The laptop,
/// which the workspace's roster names, is taken in at once: its live session opens while the relay is
/// still held, it is never told busy and never stands as busy or refused, and the phone lists
/// nobody as turned away.
#[test]
fn endpoints_nobody_knows_holding_a_relay_do_not_keep_its_workspaces_device_out() {
    const {
        assert!(
            MAX_ADMITTING > MAX_ADMITTING_STRANGERS,
            "sixteen endpoints take every place a relay has for strangers"
        );
    }
    log_sessions_at(LOGGED);
    let (_site, _ada, mut laptop, phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    let phone_key = hex::encode(phone.cert.device_public_key);
    // Both devices are in the live session first, through the phone: what the workspace's keys and its
    // membership take is done before the relay is held.
    for device in [&laptop, &phone] {
        eventually(
            &format!("{} says the live session of {made} is open", device.label),
            Duration::from_secs(90),
            &made,
            &[&laptop, &phone],
            || {
                let live_session = live_session_of(device, &made);
                live_session["state"] == json!("open") && live_session["relay"] == json!(phone_key)
            },
        );
    }
    laptop.stop();
    let refused_before = told_refused(&laptop, &made);
    let busy_before = told_busy(&laptop, &made);

    // Sixteen endpoints nobody knows hold the phone's relay: one more of their kind is told busy.
    let held = Held::hold(&phone);
    eventually(
        "the phone's relay tells one more endpoint nobody knows that it is busy",
        Duration::from_secs(60),
        &made,
        &[&phone],
        || held.told() == Some(u64::from(CLOSE_BUSY)),
    );

    // The laptop comes back and asks the phone for the live session. It is taken in while the relay is
    // held, and on the way its live session never stands as busy or as refused.
    laptop.start();
    laptop.wait_holding(&made);
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let live_session = live_session_of(&laptop, &made);
        assert!(
            live_session["state"] != json!("refused")
                && live_session["state"] != json!("relay-busy"),
            "endpoints nobody knows kept the workspace's own device out: {live_session}\n{}",
            report(&made, &[&laptop, &phone])
        );
        if live_session["state"] == json!("open")
            && live_session["waiting_for"] == json!("")
            && live_session["relay"] == json!(phone_key)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the laptop did not come into the live session while the phone's relay was held\n{}",
            report(&made, &[&laptop, &phone])
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // The relay is still held: the strangers' places were taken all along.
    eventually(
        "the phone's relay still tells an endpoint nobody knows that it is busy",
        Duration::from_secs(60),
        &made,
        &[&laptop, &phone],
        || held.told() == Some(u64::from(CLOSE_BUSY)),
    );
    assert_eq!(
        told_busy(&laptop, &made),
        busy_before,
        "the laptop was told busy by a relay whose strangers' places alone were taken\n{}",
        report(&made, &[&laptop, &phone])
    );
    assert_eq!(
        told_refused(&laptop, &made),
        refused_before,
        "the laptop's session journalled a refusal on its way into the live session\n{}",
        report(&made, &[&laptop, &phone])
    );
    // An endpoint that says nothing is nobody the relay judged: it lists nobody as turned away.
    assert_eq!(
        live_session_of(&phone, &made)["turned_away"],
        json!([]),
        "the phone turned nobody away\n{}",
        report(&made, &[&laptop, &phone])
    );
    held.release();
    let live_session = live_session_of(&laptop, &made);
    assert_eq!(
        (&live_session["state"], &live_session["unheard"]),
        (&json!("open"), &json!(0)),
        "the laptop is in the live session and hears every device in it: {live_session}"
    );
}
