//! The machine's power on the headless backend: the policy a person set
//! reaches the idle timers, the lid, the power key, a critical battery and a
//! sleep do what it says, and what holds the machine awake keeps it up.
//!
//! logind and `UPower` are stand-ins served over a socket pair: a real D-Bus
//! wire, no bus daemon, and never this host's own daemons.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::Read as _;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tilo_compositor::{IdleConfig, InputEvent, OutputConfig};
use tilo_power_mgmt::inhibit::INHIBIT_IDLE;
use tilo_power_mgmt::{PowerAction, PowerNotification, PowerPolicy, PowerSignal};
use tilo_session::unlock::{PasswordVerifier, Verdict};
use tilo_session::{Options, Service, Session};
use tilo_session_power::{BATTERY, PowerLink, SLEEP, SessionPower};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};
use zbus::{Connection, Guid};

const LOGIND_PATH: &str = "/org/freedesktop/login1";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DISPLAY_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// What the stand-in daemons were asked.
#[derive(Default)]
struct Calls {
    /// The daemon's end of every inhibitor fd handed out, in the order asked:
    /// it reads EOF once the service drops the lock.
    held: Vec<(String, StdUnixStream)>,
    suspends: usize,
}

type Shared = Arc<Mutex<Calls>>;

fn calls(shared: &Shared) -> MutexGuard<'_, Calls> {
    shared.lock().expect("calls")
}

struct FakeLogind {
    calls: Shared,
    /// What `CanSuspend` answers.
    verdict: String,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
#[allow(
    clippy::unused_self,
    reason = "logind asks these of the object, and some answers do not depend on which call it is"
)]
impl FakeLogind {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> OwnedFd {
        let (ours, theirs) = StdUnixStream::pair().expect("socketpair");
        ours.set_nonblocking(true).expect("nonblocking");
        assert!(!who.is_empty() && !why.is_empty() && !mode.is_empty());
        calls(&self.calls).held.push((what.to_owned(), ours));
        OwnedFd::from(std::os::fd::OwnedFd::from(theirs))
    }

    fn list_inhibitors(&self) -> Vec<(String, String, String, String, u32, u32)> {
        Vec::new()
    }

    fn can_suspend(&self) -> String {
        self.verdict.clone()
    }

    fn suspend(&self, interactive: bool) {
        assert!(!interactive, "the service never asks logind to prompt");
        let mut calls = calls(&self.calls);
        calls.suspends = calls.suspends.saturating_add(1);
    }

    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}

/// A laptop with a lid on the battery.
struct FakeUPower {
    lid_closed: bool,
}

#[zbus::interface(name = "org.freedesktop.UPower")]
#[allow(
    clippy::unused_self,
    reason = "UPower reads these off the object, and the lid's presence is the machine's"
)]
impl FakeUPower {
    #[zbus(property)]
    fn lid_is_closed(&self) -> bool {
        self.lid_closed
    }

    #[zbus(property)]
    fn lid_is_present(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn on_battery(&self) -> bool {
        true
    }

    fn enumerate_devices(&self) -> Vec<OwnedObjectPath> {
        Vec::new()
    }
}

/// The composite battery, discharging.
struct FakeDisplayDevice {
    percentage: f64,
}

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
#[allow(
    clippy::unused_self,
    reason = "UPower reads these off the device, and a battery's type does not change"
)]
impl FakeDisplayDevice {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn is_present(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.percentage
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn capacity(&self) -> f64 {
        90.0
    }
}

/// The machine's daemons, the session's side of them, and what was asked.
struct Machine {
    calls: Shared,
    server: Connection,
    link: PowerLink,
    runtime: tokio::runtime::Runtime,
}

impl Machine {
    /// The service over a stand-in bus, running `policy`.
    fn start(policy: PowerPolicy) -> (Self, Box<dyn Service>) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tilo-power-test")
            .enable_all()
            .build()
            .expect("runtime");
        let calls = Shared::default();
        let (server, client) = runtime.block_on(async {
            let (server, client) = tokio::net::UnixStream::pair().expect("socketpair");
            let server = Builder::unix_stream(server)
                .server(Guid::generate())
                .expect("server")
                .p2p()
                .serve_at(
                    LOGIND_PATH,
                    FakeLogind {
                        calls: Arc::clone(&calls),
                        verdict: "yes".into(),
                    },
                )
                .expect("logind")
                .serve_at(UPOWER_PATH, FakeUPower { lid_closed: false })
                .expect("upower")
                .serve_at(DISPLAY_PATH, FakeDisplayDevice { percentage: 80.0 })
                .expect("display device")
                .build();
            let client = Builder::unix_stream(client).p2p().build();
            let (server, client) = tokio::join!(server, client);
            (server.expect("server"), client.expect("client"))
        });
        let link = PowerLink::new(policy);
        let service = SessionPower::with_connection(client, link.clone()).expect("service");
        let machine = Self {
            calls,
            server,
            link,
            runtime,
        };
        (machine, Box::new(service))
    }

    fn suspends(&self) -> usize {
        calls(&self.calls).suspends
    }

    /// Whether the service still holds a logind lock for `what`.
    fn lock_held(&self, what: &str) -> bool {
        let mut calls = calls(&self.calls);
        calls
            .held
            .iter_mut()
            .filter(|(held, _)| held == what)
            .any(|(_, fd)| {
                let mut byte = [0u8; 1];
                match fd.read(&mut byte) {
                    Ok(0) => false,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
                    other => panic!("unexpected read on an inhibitor fd: {other:?}"),
                }
            })
    }

    /// logind says the machine is about to sleep, or woke from it.
    fn sleep(&self, start: bool) {
        let server = self.server.clone();
        self.runtime.block_on(async {
            let iface = server
                .object_server()
                .interface::<_, FakeLogind>(LOGIND_PATH)
                .await
                .expect("logind iface");
            FakeLogind::prepare_for_sleep(iface.signal_emitter(), start)
                .await
                .expect("PrepareForSleep");
        });
    }

    /// The lid closes, or opens.
    fn lid(&self, closed: bool) {
        let server = self.server.clone();
        self.runtime.block_on(async {
            let iface = server
                .object_server()
                .interface::<_, FakeUPower>(UPOWER_PATH)
                .await
                .expect("upower iface");
            iface.get_mut().await.lid_closed = closed;
            iface
                .get()
                .await
                .lid_is_closed_changed(iface.signal_emitter())
                .await
                .expect("LidIsClosed changed");
        });
    }

    /// The pack drops to `percentage`.
    fn charge(&self, percentage: f64) {
        let server = self.server.clone();
        self.runtime.block_on(async {
            let iface = server
                .object_server()
                .interface::<_, FakeDisplayDevice>(DISPLAY_PATH)
                .await
                .expect("display iface");
            iface.get_mut().await.percentage = percentage;
            iface
                .get()
                .await
                .percentage_changed(iface.signal_emitter())
                .await
                .expect("Percentage changed");
        });
    }
}

/// The policy these tests start from: nothing on its own unless a test asks,
/// the lid and the power key locking, a critical battery locking.
fn quiet() -> PowerPolicy {
    PowerPolicy {
        idle_blank: None,
        idle_lock: None,
        idle_suspend: None,
        lock_on_sleep: true,
        lid_close_action: PowerAction::Lock,
        power_key_action: PowerAction::Lock,
        critical_action: PowerAction::Lock,
        ..PowerPolicy::default()
    }
}

/// Unlocks with "sesame".
struct Sesame;

impl PasswordVerifier for Sesame {
    fn verify(&self, _user: &str, password: &str) -> Verdict {
        if password == "sesame" {
            Verdict::Unlocks
        } else {
            Verdict::Wrong
        }
    }
}

/// A session answering its power through `machine`.
fn session(machine: &Machine, service: Box<dyn Service>) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services: vec![service],
            power: Some(machine.link.clone()),
            user: "ada".to_owned(),
            verifier: Arc::new(Sesame),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// `XF86PowerOff`, as a laptop's power button sends it.
const POWER_KEY: u32 = 116;

/// Press and release a key, as the keyboard does.
fn press(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .expect("the key");
        session.step(Duration::from_millis(2)).expect("step");
    }
}

/// Step the session until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..800 {
        session.step(Duration::from_millis(2)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("condition not reached");
}

/// Step the session for `time`, letting what it waits on arrive.
fn step_for(session: &mut Session, time: Duration) {
    let started = Instant::now();
    while started.elapsed() < time {
        session.step(Duration::from_millis(5)).expect("step");
    }
}

/// The policy the person set is the idle timer the session runs on, and the
/// one it publishes.
#[test]
fn the_policy_is_the_idle_timer_the_session_runs_on() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(PowerPolicy {
        idle_blank: Some(Duration::from_secs(600)),
        idle_lock: Some(Duration::from_secs(300)),
        idle_suspend: Some(Duration::from_secs(900)),
        ..quiet()
    });
    let mut session = session(&machine, service);
    assert_eq!(
        session.compositor().idle_config(),
        IdleConfig::never()
            .with_blank(Some(Duration::from_secs(600)))
            .with_lock(Some(Duration::from_secs(300)))
            .with_suspend(Some(Duration::from_secs(900)))
    );
    assert_eq!(
        session.read(SLEEP),
        Some(serde_json::json!({
            "blank_minutes": 10,
            "lock_minutes": 5,
            "suspend_minutes": 15,
            "lock_on_sleep": true,
            "lid_close": "lock",
            "power_key": "lock",
            "held_awake": false,
            "holds": [],
        })),
        "what a person reads is the policy in force"
    );
}

/// An idle timeout sleeps the machine through logind once nothing holds it
/// awake, and a hold keeps it up until it is let go.
#[test]
fn an_idle_timeout_sleeps_the_machine_unless_something_holds_it_awake() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(PowerPolicy {
        idle_suspend: Some(Duration::from_millis(60)),
        ..quiet()
    });
    let hold = machine.link.keep_awake("a backup runs");
    let mut session = session(&machine, service);
    step_for(&mut session, Duration::from_millis(400));
    assert_eq!(machine.suspends(), 0, "a hold keeps the machine awake");
    assert_eq!(
        session.read(SLEEP).expect("reading")["holds"],
        serde_json::json!(["a backup runs"])
    );

    assert!(machine.link.release(hold));
    until(&mut session, |_| machine.suspends() == 1);
}

/// A change the person makes in their settings while the session runs
/// retunes the idle timers it is already running and the policy the machine
/// acts on: there is one loader, and its watch is what the session follows.
#[test]
fn a_settings_change_retunes_the_policy_while_the_session_runs() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    assert_eq!(session.compositor().idle_config(), IdleConfig::never());
    assert_eq!(session.read(SLEEP).expect("reading")["lid_close"], "lock");

    settings_store::update_section(
        settings_store::power::SECTION,
        serde_json::Map::from_iter([
            ("blank_minutes".to_owned(), serde_json::json!(7)),
            ("lock_minutes".to_owned(), serde_json::json!(3)),
            ("lock_on_sleep".to_owned(), serde_json::Value::Bool(false)),
            ("lid_close_action".to_owned(), serde_json::json!("ignore")),
        ]),
    )
    .expect("the person's settings are kept");

    until(&mut session, |session| {
        session.compositor().idle_config()
            == IdleConfig::never()
                .with_blank(Some(Duration::from_secs(420)))
                .with_lock(Some(Duration::from_secs(180)))
    });
    let reading = session.read(SLEEP).expect("reading");
    assert_eq!(
        (
            reading["blank_minutes"].clone(),
            reading["lock_minutes"].clone(),
            reading["lock_on_sleep"].clone(),
            reading["lid_close"].clone()
        ),
        (
            serde_json::json!(7),
            serde_json::json!(3),
            serde_json::json!(false),
            serde_json::json!("ignore")
        ),
        "the policy the machine acts on is the one the settings now say: {reading}"
    );
}

/// An app's `Inhibit IDLE` hold from the session's registry keeps the
/// machine awake as well, and letting go lets the next idle period sleep it.
#[test]
fn an_apps_hold_keeps_the_machine_awake_too() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(PowerPolicy {
        idle_suspend: Some(Duration::from_millis(60)),
        ..quiet()
    });
    let hold = machine
        .link
        .registry()
        .acquire_for("Playing a film", INHIBIT_IDLE);
    let mut session = session(&machine, service);
    step_for(&mut session, Duration::from_millis(400));
    assert_eq!(machine.suspends(), 0);

    drop(hold);
    until(&mut session, |_| machine.suspends() == 1);
}

/// A hold taken through the session's own action host arrives like any
/// other reading, and keeps the machine awake until its ticket is released.
#[test]
fn a_published_hold_keeps_the_machine_awake_until_it_is_released() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(PowerPolicy {
        idle_suspend: Some(Duration::from_millis(60)),
        ..quiet()
    });
    let mut session = session(&machine, service);
    let started = session
        .invoke(
            SLEEP,
            "keep-awake",
            &serde_json::json!({ "reason": "a long download" })
                .as_object()
                .cloned()
                .expect("arguments"),
        )
        .expect("a hold");
    let ticket = started
        .completed()
        .and_then(|output| output.value)
        .and_then(|value| {
            value
                .get("ticket")
                .and_then(|ticket| ticket.as_str())
                .map(str::to_owned)
        })
        .expect("the ticket");
    step_for(&mut session, Duration::from_millis(400));
    assert_eq!(machine.suspends(), 0, "nothing sleeps while it is held");
    assert_eq!(
        session.read(SLEEP).expect("reading")["holds"],
        serde_json::json!(["a long download"])
    );

    session
        .invoke(
            SLEEP,
            "release",
            &serde_json::json!({ "ticket": ticket })
                .as_object()
                .cloned()
                .expect("arguments"),
        )
        .expect("released");
    until(&mut session, |_| machine.suspends() == 1);
}

/// Closing the lid does what the policy says; opening it does nothing.
#[test]
fn the_lid_follows_its_policy() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    assert!(!session.compositor().locked());

    machine.lid(true);
    until(&mut session, |session| session.compositor().locked());
    machine.lid(false);
    step_for(&mut session, Duration::from_millis(200));
    assert!(
        session.compositor().locked(),
        "opening the lid lifts nothing"
    );

    machine.link.set_policy(PowerPolicy {
        lid_close_action: PowerAction::Suspend,
        ..quiet()
    });
    machine.lid(true);
    until(&mut session, |_| machine.suspends() == 1);
}

/// The power key does what the policy says, on the lock screen too.
#[test]
fn the_power_key_follows_its_policy() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);

    press(&mut session, POWER_KEY);
    assert!(session.compositor().locked(), "the policy locks");

    // The lock screen withholds the surfaces' actions; the power key is one
    // of the keys that still works there.
    machine.link.set_policy(PowerPolicy {
        power_key_action: PowerAction::Suspend,
        ..quiet()
    });
    press(&mut session, POWER_KEY);
    until(&mut session, |_| machine.suspends() == 1);
}

/// A pack at the critical charge locks the session and publishes the notice
/// a banner and the bar read.
#[test]
fn a_critical_battery_locks_the_session_and_publishes_its_notice() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    machine.charge(3.0);
    until(&mut session, |session| session.compositor().locked());
    let reading = session.read(BATTERY).expect("the battery reading");
    let notice = reading.get("notice").cloned().unwrap_or_default();
    assert_eq!(
        notice.get("critical").and_then(serde_json::Value::as_bool),
        Some(true),
        "{reading}"
    );
    assert!(
        notice
            .get("body")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|body| !body.is_empty()),
        "{reading}"
    );
}

/// logind's own sleep: the session locks and says the frame is drawn, the
/// wake keeps the lock, and the delay lock is taken again for the next one.
#[test]
fn a_sleep_locks_the_session_says_the_frame_is_drawn_and_keeps_it_on_the_wake() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    assert!(machine.lock_held("sleep"), "the sleep delay is taken");

    machine.sleep(true);
    until(&mut session, |session| session.compositor().locked());
    until(&mut session, |_| !machine.lock_held("sleep"));

    machine.sleep(false);
    until(&mut session, |_| machine.lock_held("sleep"));
    assert!(
        session.compositor().locked(),
        "the wake keeps the lock until the person unlocks"
    );
}

/// A suspend that failed after the lock was drawn leaves the session
/// unlocked again.
#[test]
fn a_failed_sleep_lifts_the_lock_it_installed() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);

    machine
        .link
        .bridge()
        .push_signal(PowerSignal::PreparingForSleep);
    until(&mut session, |session| session.compositor().locked());

    machine
        .link
        .bridge()
        .push_signal(PowerSignal::SuspendFailed);
    until(&mut session, |session| !session.compositor().locked());
}

/// A battery notice the service posts is what a banner shows, and what a
/// hold reads of the machine does not change because of it.
#[test]
fn a_battery_notice_reaches_the_sessions_reading() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    machine
        .link
        .bridge()
        .push_signal(PowerSignal::Notify(PowerNotification {
            summary: "Battery low".into(),
            body: "15% remaining".into(),
            critical: false,
        }));
    until(&mut session, |session| {
        session.read(BATTERY).is_some_and(|reading| {
            reading
                .get("notice")
                .is_some_and(|notice| !notice.is_null())
        })
    });
    let reading = session.read(BATTERY).expect("the battery reading");
    assert_eq!(
        reading["notice"]["summary"],
        serde_json::json!("Battery low")
    );
}
