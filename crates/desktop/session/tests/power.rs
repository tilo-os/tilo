//! The machine's power on the headless backend: the policy a person set reaches the idle timers,
//! the lid, the power key, a critical battery and a sleep do what it says, and what holds the
//! machine awake keeps it up.
//! logind and `UPower` are stand-ins served over a socket pair: a real D-Bus wire, no bus daemon,
//! and never this host's own daemons.

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

use std::io::Read as _;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tilo_compositor::{IdleConfig, InputEvent};
use tilo_power_mgmt::inhibit::INHIBIT_IDLE;
use tilo_power_mgmt::{PowerAction, PowerNotification, PowerPolicy, PowerSignal};
use tilo_session::surfaces::Surface;
use tilo_session::unlock::{PasswordVerifier, Verdict};
use tilo_session::{AppSocket, Options, Service, Session};
use tilo_session_power::{BATTERY, PowerLink, SLEEP, SessionPower};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};
use zbus::{Connection, Guid};

const LOGIND_PATH: &str = "/org/freedesktop/login1";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DISPLAY_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// What the stand-in daemons were asked.
#[derive(Default)]
struct Calls {
    /// The daemon's end of every inhibitor fd handed out, in the order asked: it reads EOF once the
    /// service drops the lock.
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
    /// The service over a stand-in bus, running `policy` .
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

    /// Whether the service still holds a logind lock for `what` .
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

    /// The pack drops to `percentage` .
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

/// A session answering its power through `machine` .
fn session(machine: &Machine, service: Box<dyn Service>) -> Session {
    crate::common::headless(Options {
        services: vec![service],
        power: Some(machine.link.clone()),
        user: "ada".to_owned(),
        verifier: Arc::new(Sesame),
        ..Options::default()
    })
}

/// The lock screen the session starts when it locks: the app whose window asks the compositor for
/// the session lock, which this run builds the first time a test locks into it.
fn lock_screen() -> Surface {
    let binary = tilo_test_env::sibling_binary("tilo-lock-screen", "tilo-lock-screen");
    Surface::new("lock", binary).lock()
}

/// A session answering its power through `machine` , whose lock screen the session starts when it
/// locks. The directory is the app wire's, and only this user reaches it: the broker refuses to serve
/// one anybody else could open.
fn lockable(machine: &Machine, service: Box<dyn Service>) -> (tempfile::TempDir, Session) {
    use std::os::unix::fs::PermissionsExt as _;
    let app_wire = tempfile::tempdir().expect("a directory to serve the app wire in");
    std::fs::set_permissions(app_wire.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the app wire's directory is this user's");
    let session = crate::common::headless(Options {
        services: vec![service],
        power: Some(machine.link.clone()),
        user: "ada".to_owned(),
        verifier: Arc::new(Sesame),
        app_socket: AppSocket::At(app_wire.path().join("actions.sock")),
        surfaces: vec![lock_screen()],
        ..Options::default()
    });
    (app_wire, session)
}

/// `XF86PowerOff` , as a laptop's power button sends it.
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

/// Step the session until `done` holds. The lock screen is a process of its own, so the wait is in
/// real time: a screen that has to start and draw is given as long as any app gets on a busy
/// machine.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    let at = std::panic::Location::caller();
    crate::common::wait_for(session, format!("the condition at {at}"), |session| {
        done(session).then_some(())
    });
}

/// Step the session for `time` , letting what it waits on arrive.
fn step_for(session: &mut Session, time: Duration) {
    let started = Instant::now();
    while started.elapsed() < time {
        session.step(Duration::from_millis(5)).expect("step");
    }
}

/// The policy the person set is the idle timer the session runs on, and the one it publishes.
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

/// An idle timeout sleeps the machine through logind once nothing holds it awake, and a hold keeps
/// it up until it is let go.
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

/// A change the person makes in their settings while the session runs retunes the idle timers it is
/// already running and the policy the machine acts on: there is one loader, and its watch is what
/// the session follows.
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

/// An app's `Inhibit IDLE` hold from the session's registry keeps the machine awake as well, and
/// letting go lets the next idle period sleep it.
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

/// A hold taken through the session's own action host arrives like any other reading, and keeps the
/// machine awake until its ticket is released.
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
    let (_app_wire, mut session) = lockable(&machine, service);
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
    let (_app_wire, mut session) = lockable(&machine, service);

    press(&mut session, POWER_KEY);
    assert!(session.compositor().locked(), "the policy locks");

    // The lock screen withholds the surfaces' actions; the power key is one of the keys that still
    // works there.
    machine.link.set_policy(PowerPolicy {
        power_key_action: PowerAction::Suspend,
        ..quiet()
    });
    press(&mut session, POWER_KEY);
    until(&mut session, |_| machine.suspends() == 1);
}

/// A pack at the critical charge locks the session and tells the person, on every display, what the
/// machine does about it.
#[test]
fn a_critical_battery_locks_the_session_and_tells_the_person() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let (_app_wire, mut session) = lockable(&machine, service);
    machine.charge(3.0);
    until(&mut session, |session| session.compositor().locked());
    until(&mut session, |session| {
        session
            .notifications()
            .log()
            .active()
            .iter()
            .any(|notification| notification.title == "Battery critically low")
    });
    let live = session.notifications().log().active().to_vec();
    let critical = live
        .iter()
        .find(|notification| notification.title == "Battery critically low")
        .expect("the critical warning");
    assert!(critical.body.contains("3%"), "{}", critical.body);
    assert!(critical.machine_wide, "every display shows it");
}

/// logind's own sleep: the session locks and says the frame is drawn, the wake keeps the lock, and
/// the delay lock is taken again for the next one.
#[test]
fn a_sleep_locks_the_session_says_the_frame_is_drawn_and_keeps_it_on_the_wake() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let (_app_wire, mut session) = lockable(&machine, service);
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

/// A suspend that failed after the lock was drawn leaves the session unlocked again.
#[test]
fn a_failed_sleep_lifts_the_lock_it_installed() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let (_app_wire, mut session) = lockable(&machine, service);

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

/// The battery notices the service posts reach the person as notifications: a heads-up at low
/// charge, and one that stays and names what the machine does at critical charge.
#[test]
fn a_battery_notice_reaches_the_person_as_a_notification() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    machine
        .link
        .bridge()
        .push_signal(PowerSignal::Notify(PowerNotification {
            percent: 15,
            critical: false,
            suspending: false,
        }));
    until(&mut session, |session| {
        !session.notifications().log().active().is_empty()
    });
    let up = session.notifications().log().active().to_vec();
    assert_eq!(up[0].title, "Battery low");
    assert!(up[0].body.contains("15%"), "{}", up[0].body);
    assert!(
        !up[0].machine_wide,
        "the heads-up is for the display in use"
    );

    machine
        .link
        .bridge()
        .push_signal(PowerSignal::Notify(PowerNotification {
            percent: 4,
            critical: true,
            suspending: false,
        }));
    until(&mut session, |session| {
        session.notifications().log().active().len() == 2
    });
    let up = session.notifications().log().active().to_vec();
    assert_eq!(up[1].title, "Battery critically low");
    assert!(up[1].body.contains("plug in"), "{}", up[1].body);
    assert!(up[1].machine_wide, "every display shows it");
}

/// The same notice lies in the battery's own reading, in the session's words,
/// for the surfaces that draw the pack's state: pressing later notices over
/// the one a surface has not shown yet.
#[test]
fn a_battery_notice_reaches_the_sessions_reading() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let mut session = session(&machine, service);
    machine
        .link
        .bridge()
        .push_signal(PowerSignal::Notify(PowerNotification {
            percent: 15,
            critical: false,
            suspending: false,
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
    assert!(
        reading["notice"]["body"]
            .as_str()
            .is_some_and(|body| body.contains("15%")),
        "{reading}"
    );
    assert_eq!(
        reading["notice"]["critical"],
        serde_json::json!(false),
        "{reading}"
    );
}

/// A pack at the critical charge locks the session and publishes the notice a banner and the bar
/// read, naming what the machine does about it.
#[test]
fn a_critical_battery_locks_the_session_and_publishes_its_notice() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(quiet());
    let (_app_wire, mut session) = lockable(&machine, service);
    machine.charge(3.0);
    until(&mut session, |session| session.compositor().locked());
    until(&mut session, |session| {
        session.read(BATTERY).is_some_and(|reading| {
            reading
                .get("notice")
                .is_some_and(|notice| !notice.is_null())
        })
    });
    let reading = session.read(BATTERY).expect("the battery reading");
    assert_eq!(
        reading["notice"]["critical"],
        serde_json::json!(true),
        "{reading}"
    );
    assert!(
        reading["notice"]["body"]
            .as_str()
            .is_some_and(|body| !body.is_empty()),
        "{reading}"
    );
}

const PROFILES_PATH: &str = "/net/hadess/PowerProfiles";

struct FakePowerProfiles {
    active: String,
    profiles: Vec<String>,
    set_calls: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl FakePowerProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.active.clone()
    }

    #[zbus(property)]
    fn set_active_profile(&mut self, profile: String) -> zbus::fdo::Result<()> {
        if !self.profiles.contains(&profile) {
            return Err(zbus::fdo::Error::InvalidArgs(profile));
        }
        self.set_calls
            .lock()
            .expect("set_calls")
            .push(profile.clone());
        self.active = profile;
        Ok(())
    }

    #[zbus(property)]
    fn profiles(&self) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
        self.profiles
            .iter()
            .map(|name| {
                std::collections::HashMap::from([
                    (
                        "Profile".to_owned(),
                        zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                            name.as_str(),
                        ))
                        .expect("profile"),
                    ),
                    (
                        "Driver".to_owned(),
                        zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                            "platform_profile",
                        ))
                        .expect("driver"),
                    ),
                ])
            })
            .collect()
    }
}

struct CountingSystem {
    power_reads: std::sync::atomic::AtomicUsize,
    mode: Mutex<Option<tilo_system_service::model::PowerMode>>,
    changes: Mutex<Vec<tilo_system_service::device::Change>>,
}

impl tilo_system_service::device::System for CountingSystem {
    fn power(&self) -> tilo_system_service::model::Power {
        self.power_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tilo_system_service::model::Power::default()
    }

    fn power_mode(&self) -> Option<tilo_system_service::model::PowerMode> {
        *self.mode.lock().expect("mode")
    }

    fn user_switching(&self) -> bool {
        false
    }

    fn update(&self) -> tilo_system_service::update::Update {
        tilo_system_service::update::Update::default()
    }

    fn time(&self) -> tilo_system_service::model::Time {
        tilo_system_service::model::Time::default()
    }

    fn keyboard_light(&self) -> tilo_system_service::model::KeyboardLight {
        tilo_system_service::model::KeyboardLight::default()
    }

    fn change(&self, change: &tilo_system_service::device::Change) -> Result<(), String> {
        if let tilo_system_service::device::Change::PowerMode(mode) = change {
            *self.mode.lock().expect("mode") = Some(*mode);
        }
        self.changes.lock().expect("changes").push(change.clone());
        Ok(())
    }

    fn locate(&self) -> Result<settings_store::day_schedule::Location, String> {
        Err("no fix".into())
    }

    fn nearest_zone(&self, _location: settings_store::day_schedule::Location) -> Option<String> {
        None
    }
}

fn start_bus_with_profiles(
    runtime: &tokio::runtime::Runtime,
    active: &str,
    profiles: &[&str],
) -> (Connection, Connection) {
    let active = active.to_owned();
    let profiles: Vec<String> = profiles.iter().map(|&name| name.to_owned()).collect();
    runtime.block_on(async move {
        let (server, client) = tokio::net::UnixStream::pair().expect("socketpair");
        let server = Builder::unix_stream(server)
            .server(Guid::generate())
            .expect("server")
            .p2p()
            .serve_at(
                LOGIND_PATH,
                FakeLogind {
                    calls: Shared::default(),
                    verdict: "yes".into(),
                },
            )
            .expect("logind")
            .serve_at(UPOWER_PATH, FakeUPower { lid_closed: false })
            .expect("upower")
            .serve_at(DISPLAY_PATH, FakeDisplayDevice { percentage: 80.0 })
            .expect("display device")
            .serve_at(
                PROFILES_PATH,
                FakePowerProfiles {
                    active,
                    profiles,
                    set_calls: Arc::new(Mutex::new(Vec::new())),
                },
            )
            .expect("power profiles")
            .build();
        let client = Builder::unix_stream(client).p2p().build();
        let (server, client) = tokio::join!(server, client);
        (server.expect("server"), client.expect("client"))
    })
}

fn emit_active_profile(runtime: &tokio::runtime::Runtime, server: &Connection, active: &str) {
    let active = active.to_owned();
    runtime.block_on(async move {
        let iface = server
            .object_server()
            .interface::<_, FakePowerProfiles>(PROFILES_PATH)
            .await
            .expect("power profiles iface");
        iface.get_mut().await.active = active;
        iface
            .get()
            .await
            .active_profile_changed(iface.signal_emitter())
            .await
            .expect("ActiveProfile changed");
    });
}

fn emit_profiles(runtime: &tokio::runtime::Runtime, server: &Connection, profiles: &[&str]) {
    let profiles: Vec<String> = profiles.iter().map(|&name| name.to_owned()).collect();
    runtime.block_on(async move {
        let iface = server
            .object_server()
            .interface::<_, FakePowerProfiles>(PROFILES_PATH)
            .await
            .expect("power profiles iface");
        iface.get_mut().await.profiles = profiles;
        iface
            .get()
            .await
            .profiles_changed(iface.signal_emitter())
            .await
            .expect("Profiles changed");
    });
}

/// Live `power-profiles-daemon` property changes over D-Bus reach the session's
/// `power` reading through the shared `PowerLink` without polling, and
/// `power#set-power-mode` validates against that live list.
#[test]
fn a_dbus_profile_change_reaches_the_sessions_power_reading_live() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_system_service::device::{Change, System};
    use tilo_system_service::model::PowerMode;

    let _home = tilo_test_env::TestHome::new().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tilo-power-profiles-test")
        .enable_all()
        .build()
        .expect("runtime");
    let (server, client) =
        start_bus_with_profiles(&runtime, "balanced", &["balanced", "power-saver"]);

    let link = PowerLink::new(quiet());
    let power_service = SessionPower::with_connection(client, link.clone()).expect("power service");
    let wake = tilo_service::LoopWake::default();
    let fake_system = Arc::new(CountingSystem {
        power_reads: AtomicUsize::new(0),
        mode: Mutex::new(None),
        changes: Mutex::new(Vec::new()),
    });
    let system_service = tilo_session::system_service(
        Arc::clone(&fake_system) as Arc<dyn System>,
        Some(wake.handle()),
        Some(&link),
        None,
    )
    .expect("system service");

    let mut session = crate::common::headless(Options {
        services: vec![Box::new(power_service), Box::new(system_service)],
        wake,
        power: Some(link),
        user: "ada".to_owned(),
        verifier: Arc::new(Sesame),
        ..Options::default()
    });

    until(&mut session, |session| {
        session
            .read(tilo_system_service::POWER)
            .is_some_and(|power| {
                power["mode"] == serde_json::json!("balanced")
                    && power["modes"] == serde_json::json!(["balanced", "saver"])
            })
    });
    // At most the single startup refresh may have run before PowerService primed the bridge.
    let reads_once_live = fake_system.power_reads.load(Ordering::SeqCst);
    assert!(reads_once_live <= 1, "at most one startup read before live");
    let sleep = session.read(SLEEP).expect("sleep reading");
    assert_eq!(sleep.get("profile"), None, "{sleep}");
    assert_eq!(sleep.get("profiles"), None, "{sleep}");

    emit_active_profile(&runtime, &server, "power-saver");
    until(&mut session, |session| {
        session
            .read(tilo_system_service::POWER)
            .is_some_and(|power| power["mode"] == serde_json::json!("saver"))
    });

    emit_profiles(&runtime, &server, &["performance", "balanced"]);
    until(&mut session, |session| {
        session
            .read(tilo_system_service::POWER)
            .is_some_and(|power| power["modes"] == serde_json::json!(["performance", "balanced"]))
    });

    let mut perf = serde_json::Map::new();
    perf.insert("mode".to_owned(), serde_json::json!("performance"));
    assert!(
        session
            .invoke(tilo_system_service::POWER, "set-power-mode", &perf)
            .is_ok()
    );
    session.settle_services();
    assert_eq!(
        *fake_system.changes.lock().expect("changes"),
        vec![Change::PowerMode(PowerMode::Performance)]
    );

    let mut saver = serde_json::Map::new();
    saver.insert("mode".to_owned(), serde_json::json!("saver"));
    assert_eq!(
        session.invoke(tilo_system_service::POWER, "set-power-mode", &saver),
        Err(tilo_actions::ApiError::InvalidArguments(
            "this device offers no power mode saver".to_owned()
        ))
    );
    assert_eq!(
        fake_system.power_reads.load(Ordering::SeqCst),
        reads_once_live,
        "once the live bridge takes over, System::power() is never polled again"
    );
}

/// Fake `net.hadess.PowerProfiles` whose `ActiveProfile` setter emits no
/// `PropertiesChanged` signal, so tests prove the direct read after
/// `set-power-mode` lands.
struct SilentSetPowerProfiles {
    active: String,
    profiles: Vec<String>,
    set_calls: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl SilentSetPowerProfiles {
    #[zbus(property(emits_changed_signal = "false"))]
    fn active_profile(&self) -> String {
        self.active.clone()
    }

    #[zbus(property)]
    fn set_active_profile(&mut self, profile: String) -> zbus::fdo::Result<()> {
        if !self.profiles.contains(&profile) {
            return Err(zbus::fdo::Error::InvalidArgs(profile));
        }
        self.set_calls
            .lock()
            .expect("set_calls")
            .push(profile.clone());
        self.active = profile;
        Ok(())
    }

    #[zbus(property)]
    fn profiles(&self) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
        self.profiles
            .iter()
            .map(|name| {
                std::collections::HashMap::from([
                    (
                        "Profile".to_owned(),
                        zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                            name.as_str(),
                        ))
                        .expect("profile"),
                    ),
                    (
                        "Driver".to_owned(),
                        zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                            "platform_profile",
                        ))
                        .expect("driver"),
                    ),
                ])
            })
            .collect()
    }
}

/// Fake `net.hadess.PowerProfiles` whose `Profiles` and `ActiveProfile` getters
/// fail while their switches are held, then answer normally once released.
struct GatedProfilesDaemon {
    active: String,
    profiles: Vec<String>,
    fail_profiles: Arc<std::sync::atomic::AtomicBool>,
    fail_active: Arc<std::sync::atomic::AtomicBool>,
    profiles_attempts: Arc<std::sync::atomic::AtomicUsize>,
    active_attempts: Arc<std::sync::atomic::AtomicUsize>,
    set_calls: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl GatedProfilesDaemon {
    #[zbus(property(emits_changed_signal = "false"))]
    fn active_profile(&self) -> zbus::fdo::Result<String> {
        self.active_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_active.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("active profile busy".into()));
        }
        Ok(self.active.clone())
    }

    #[zbus(property)]
    fn set_active_profile(&mut self, profile: String) -> zbus::fdo::Result<()> {
        if !self.profiles.contains(&profile) {
            return Err(zbus::fdo::Error::InvalidArgs(profile));
        }
        self.set_calls
            .lock()
            .expect("set_calls")
            .push(profile.clone());
        self.active = profile;
        Ok(())
    }

    #[zbus(property)]
    fn profiles(
        &self,
    ) -> zbus::fdo::Result<Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>> {
        self.profiles_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_profiles.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("profiles busy".into()));
        }
        Ok(self
            .profiles
            .iter()
            .map(|name| {
                std::collections::HashMap::from([
                    (
                        "Profile".to_owned(),
                        zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                            name.as_str(),
                        ))
                        .expect("profile"),
                    ),
                    (
                        "Driver".to_owned(),
                        zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from(
                            "platform_profile",
                        ))
                        .expect("driver"),
                    ),
                ])
            })
            .collect())
    }
}

struct BusPowerSystem {
    handle: tokio::runtime::Handle,
    bus: Connection,
    power_reads: std::sync::atomic::AtomicUsize,
    polled: Mutex<tilo_system_service::model::Power>,
}

impl tilo_system_service::device::System for BusPowerSystem {
    fn power(&self) -> tilo_system_service::model::Power {
        self.power_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.polled.lock().expect("polled").clone()
    }

    fn power_mode(&self) -> Option<tilo_system_service::model::PowerMode> {
        let bus = self.bus.clone();
        let active = self.handle.block_on(async move {
            let monitor = tilo_power_mgmt::PowerProfilesMonitor::with_connection(&bus)
                .await
                .ok()?;
            monitor.active_profile().await.ok()
        })?;
        os_actions::PowerProfile::from_cli(&active).map(tilo_system_service::model::power_mode)
    }

    fn user_switching(&self) -> bool {
        false
    }

    fn update(&self) -> tilo_system_service::update::Update {
        tilo_system_service::update::Update::default()
    }

    fn time(&self) -> tilo_system_service::model::Time {
        tilo_system_service::model::Time::default()
    }

    fn keyboard_light(&self) -> tilo_system_service::model::KeyboardLight {
        tilo_system_service::model::KeyboardLight::default()
    }

    fn change(&self, change: &tilo_system_service::device::Change) -> Result<(), String> {
        let tilo_system_service::device::Change::PowerMode(mode) = change else {
            return Ok(());
        };
        let cli = match mode {
            tilo_system_service::model::PowerMode::Saver => "power-saver",
            tilo_system_service::model::PowerMode::Balanced => "balanced",
            tilo_system_service::model::PowerMode::Performance => "performance",
        };
        let bus = self.bus.clone();
        self.handle.block_on(async move {
            let monitor = tilo_power_mgmt::PowerProfilesMonitor::with_connection(&bus)
                .await
                .map_err(|e| e.to_string())?;
            monitor
                .set_active_profile(cli)
                .await
                .map_err(|e| e.to_string())
        })?;
        self.polled.lock().expect("polled").mode = Some(*mode);
        Ok(())
    }

    fn locate(&self) -> Result<settings_store::day_schedule::Location, String> {
        Err("no fix".into())
    }

    fn nearest_zone(&self, _location: settings_store::day_schedule::Location) -> Option<String> {
        None
    }
}

fn spawn_base_daemons_on_bus(runtime: &tokio::runtime::Runtime, address: &str) -> Connection {
    let address = address.to_owned();
    runtime.block_on(async move {
        Builder::address(address.as_str())
            .expect("bus address")
            .serve_at(
                LOGIND_PATH,
                FakeLogind {
                    calls: Shared::default(),
                    verdict: "yes".into(),
                },
            )
            .expect("logind")
            .serve_at(UPOWER_PATH, FakeUPower { lid_closed: false })
            .expect("upower")
            .serve_at(DISPLAY_PATH, FakeDisplayDevice { percentage: 80.0 })
            .expect("display")
            .name("org.freedesktop.login1")
            .expect("login1 name")
            .name("org.freedesktop.UPower")
            .expect("upower name")
            .build()
            .await
            .expect("base daemons")
    })
}

fn spawn_profiles_daemon(
    runtime: &tokio::runtime::Runtime,
    address: &str,
    active: &str,
    profiles: &[&str],
) -> (Connection, Arc<Mutex<Vec<String>>>) {
    let address = address.to_owned();
    let active = active.to_owned();
    let profiles: Vec<String> = profiles.iter().map(|&name| name.to_owned()).collect();
    let set_calls = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::clone(&set_calls);
    let conn = runtime.block_on(async move {
        Builder::address(address.as_str())
            .expect("bus address")
            .serve_at(
                PROFILES_PATH,
                SilentSetPowerProfiles {
                    active,
                    profiles,
                    set_calls: calls,
                },
            )
            .expect("serve profiles")
            .name("net.hadess.PowerProfiles")
            .expect("profiles bus name")
            .build()
            .await
            .expect("profiles daemon conn")
    });
    (conn, set_calls)
}

#[track_caller]
fn wait_power(
    session: &mut Session,
    label: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    crate::common::wait_for(session, label, |session| {
        session
            .read(tilo_system_service::POWER)
            .filter(|power| pred(power))
    })
}

fn session_on_bus(
    runtime: &tokio::runtime::Runtime,
    address: &str,
    polled: tilo_system_service::model::Power,
) -> (PowerLink, Arc<BusPowerSystem>, Session) {
    use std::sync::atomic::AtomicUsize;
    use tilo_system_service::device::System;

    let address = address.to_owned();
    let (power_conn, sys_conn) = runtime.block_on(async move {
        let a = Builder::address(address.as_str())
            .expect("addr")
            .build()
            .await
            .expect("power client");
        let b = Builder::address(address.as_str())
            .expect("addr")
            .build()
            .await
            .expect("sys client");
        (a, b)
    });
    let link = PowerLink::new(quiet());
    let power_service =
        SessionPower::with_connection(power_conn, link.clone()).expect("power service");
    let wake = tilo_service::LoopWake::default();
    let fake_system = Arc::new(BusPowerSystem {
        handle: runtime.handle().clone(),
        bus: sys_conn,
        power_reads: AtomicUsize::new(0),
        polled: Mutex::new(polled),
    });
    let system_service = tilo_session::system_service(
        Arc::clone(&fake_system) as Arc<dyn System>,
        Some(wake.handle()),
        Some(&link),
        None,
    )
    .expect("system service");
    let session = crate::common::headless(Options {
        services: vec![Box::new(power_service), Box::new(system_service)],
        wake,
        power: Some(link.clone()),
        user: "ada".to_owned(),
        verifier: Arc::new(Sesame),
        ..Options::default()
    });
    (link, fake_system, session)
}

/// Removing `net.hadess.PowerProfiles` from the bus clears the session's `power` reading,
/// and a new daemon claiming the name is read directly without waiting for a signal —
/// including when the person then chooses a mode and the daemon emits no signal.
#[test]
fn a_power_profiles_daemon_leaving_and_returning_resets_and_re_reads_power() {
    use std::sync::atomic::Ordering;

    let _home = tilo_test_env::TestHome::new().unwrap();
    let bus = tilo_test_env::PrivateBus::start();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tilo-ppd-owner-test")
        .enable_all()
        .build()
        .expect("runtime");

    let _base_daemons = spawn_base_daemons_on_bus(&runtime, bus.address());
    let (daemon1, _) = spawn_profiles_daemon(
        &runtime,
        bus.address(),
        "power-saver",
        &["balanced", "power-saver"],
    );
    let (_link, fake_system, mut session) = session_on_bus(
        &runtime,
        bus.address(),
        tilo_system_service::model::Power::default(),
    );

    wait_power(&mut session, "initial saver reading", |power| {
        power["mode"] == serde_json::json!("saver")
            && power["modes"] == serde_json::json!(["balanced", "saver"])
    });
    let reads_once_live = fake_system.power_reads.load(Ordering::SeqCst);
    assert!(reads_once_live <= 1, "at most one startup read before live");

    // Daemon leaves the bus: both modes and active mode clear.
    runtime
        .block_on(daemon1.close())
        .expect("close first daemon");
    wait_power(&mut session, "cleared reading after daemon exit", |power| {
        power["mode"].is_null() && power["modes"] == serde_json::json!([])
    });

    // A replacement daemon claims the name at `balanced`, sending no PropertiesChanged signal.
    let (_daemon2, daemon2_calls) = spawn_profiles_daemon(
        &runtime,
        bus.address(),
        "balanced",
        &["performance", "balanced", "power-saver"],
    );
    wait_power(&mut session, "returned balanced reading", |power| {
        power["mode"] == serde_json::json!("balanced")
            && power["modes"] == serde_json::json!(["performance", "balanced", "saver"])
    });

    // Choosing `balanced` succeeds and stays `balanced` even though the daemon emits no signal.
    set_power_mode_ok(&mut session, "balanced");
    let reading = wait_power(&mut session, "settled balanced", |power| {
        power["pending"] == serde_json::json!([])
    });
    assert_eq!(reading["mode"], serde_json::json!("balanced"));
    assert_eq!(reading["problem"], serde_json::Value::Null);

    // Choosing `performance` (daemon emits no signal) already carries `performance` in the
    // first reading where `pending` clears.
    set_power_mode_ok(&mut session, "performance");
    let reading = wait_power(&mut session, "settled performance", |power| {
        power["pending"] == serde_json::json!([])
    });
    assert_eq!(reading["mode"], serde_json::json!("performance"));
    assert_eq!(reading["problem"], serde_json::Value::Null);
    assert_eq!(
        *daemon2_calls.lock().expect("calls"),
        vec!["balanced", "performance"]
    );
    assert_eq!(
        fake_system.power_reads.load(Ordering::SeqCst),
        reads_once_live,
        "once the live bridge takes over, System::power() is never polled again"
    );
}

struct GatedProfilesHandle {
    _conn: Connection,
    fail_profiles: Arc<std::sync::atomic::AtomicBool>,
    fail_active: Arc<std::sync::atomic::AtomicBool>,
    profiles_attempts: Arc<std::sync::atomic::AtomicUsize>,
    active_attempts: Arc<std::sync::atomic::AtomicUsize>,
}

fn spawn_gated_profiles_daemon(
    runtime: &tokio::runtime::Runtime,
    address: &str,
    active: &str,
    profiles: &[&str],
    fail_profiles: bool,
    fail_active: bool,
) -> GatedProfilesHandle {
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    let address = address.to_owned();
    let active = active.to_owned();
    let profiles: Vec<String> = profiles.iter().map(|&name| name.to_owned()).collect();
    let fail_profiles = Arc::new(AtomicBool::new(fail_profiles));
    let fail_active = Arc::new(AtomicBool::new(fail_active));
    let profiles_attempts = Arc::new(AtomicUsize::new(0));
    let active_attempts = Arc::new(AtomicUsize::new(0));
    let daemon = GatedProfilesDaemon {
        active,
        profiles,
        fail_profiles: Arc::clone(&fail_profiles),
        fail_active: Arc::clone(&fail_active),
        profiles_attempts: Arc::clone(&profiles_attempts),
        active_attempts: Arc::clone(&active_attempts),
        set_calls: Arc::new(Mutex::new(Vec::new())),
    };
    let conn = runtime.block_on(async move {
        Builder::address(address.as_str())
            .expect("bus address")
            .serve_at(PROFILES_PATH, daemon)
            .expect("serve gated profiles")
            .name("net.hadess.PowerProfiles")
            .expect("profiles bus name")
            .build()
            .await
            .expect("gated daemon conn")
    });
    GatedProfilesHandle {
        _conn: conn,
        fail_profiles,
        fail_active,
        profiles_attempts,
        active_attempts,
    }
}

fn set_power_mode_ok(session: &mut Session, mode: &str) {
    let mut args = serde_json::Map::new();
    args.insert("mode".to_owned(), serde_json::json!(mode));
    assert!(
        session
            .invoke(tilo_system_service::POWER, "set-power-mode", &args)
            .is_ok(),
        "set-power-mode({mode}) should be accepted"
    );
}

/// While the daemon's `Profiles` read is held failing and `ActiveProfile`
/// succeeds, `PowerBridge` records the active profile for policy without
/// announcing a half state: `SystemService` keeps polling `System::power()`
/// (and `set-power-mode` stays available), then switches to live with the full
/// list once `Profiles` is released and the retry succeeds.
#[test]
fn a_failed_profiles_read_keeps_polling_until_retry_succeeds() {
    use std::sync::atomic::Ordering;
    use tilo_system_service::model::{Power, PowerMode};

    let _home = tilo_test_env::TestHome::new().unwrap();
    let bus = tilo_test_env::PrivateBus::start();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tilo-ppd-gated-profiles-test")
        .enable_all()
        .build()
        .expect("runtime");

    let _base_daemons = spawn_base_daemons_on_bus(&runtime, bus.address());
    let daemon = spawn_gated_profiles_daemon(
        &runtime,
        bus.address(),
        "power-saver",
        &["performance", "balanced", "power-saver"],
        true,
        false,
    );
    let polled = Power {
        modes: vec![PowerMode::Saver, PowerMode::Balanced],
        mode: Some(PowerMode::Balanced),
        ..Power::default()
    };
    let (link, fake_system, mut session) = session_on_bus(&runtime, bus.address(), polled);

    // Wait for the initial half-answer prime while `Profiles` stays held failing.
    until(&mut session, |_| {
        daemon.profiles_attempts.load(Ordering::SeqCst) >= 1
            && link.bridge().active_profile().as_deref() == Some("power-saver")
    });
    assert_eq!(link.bridge().profiles_moved(), 0);

    wait_power(
        &mut session,
        "polled reading while Profiles fails",
        |power| {
            power["modes"] == serde_json::json!(["saver", "balanced"])
                && power["mode"] == serde_json::json!("balanced")
        },
    );
    let reads_before_action = fake_system.power_reads.load(Ordering::SeqCst);
    assert!(reads_before_action >= 1);

    set_power_mode_ok(&mut session, "saver");
    wait_power(&mut session, "settled saver on polling path", |power| {
        power["pending"] == serde_json::json!([]) && power["mode"] == serde_json::json!("saver")
    });
    assert!(fake_system.power_reads.load(Ordering::SeqCst) > reads_before_action);
    assert_eq!(link.bridge().profiles_moved(), 0);

    // Release `Profiles`: the next retry completes and SystemService goes live.
    daemon.fail_profiles.store(false, Ordering::SeqCst);
    wait_power(
        &mut session,
        "live full list after Profiles is released",
        |power| {
            power["modes"] == serde_json::json!(["performance", "balanced", "saver"])
                && power["mode"] == serde_json::json!("saver")
        },
    );
    let reads_once_live = fake_system.power_reads.load(Ordering::SeqCst);

    set_power_mode_ok(&mut session, "performance");
    wait_power(&mut session, "settled performance on live path", |power| {
        power["pending"] == serde_json::json!([])
            && power["mode"] == serde_json::json!("performance")
    });
    assert_eq!(
        fake_system.power_reads.load(Ordering::SeqCst),
        reads_once_live
    );
}

/// While `ActiveProfile` is held failing and `Profiles` succeeds,
/// `PowerBridge` does not announce the partial list, so `SystemService` keeps
/// its polled mode until `ActiveProfile` is released and the retry completes.
#[test]
fn a_failed_active_profile_read_keeps_polling_until_retry_succeeds() {
    use std::sync::atomic::Ordering;
    use tilo_system_service::model::{Power, PowerMode};

    let _home = tilo_test_env::TestHome::new().unwrap();
    let bus = tilo_test_env::PrivateBus::start();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("tilo-ppd-gated-active-test")
        .enable_all()
        .build()
        .expect("runtime");

    let _base_daemons = spawn_base_daemons_on_bus(&runtime, bus.address());
    let daemon = spawn_gated_profiles_daemon(
        &runtime,
        bus.address(),
        "power-saver",
        &["performance", "balanced", "power-saver"],
        false,
        true,
    );
    let polled = Power {
        modes: vec![PowerMode::Saver, PowerMode::Balanced],
        mode: Some(PowerMode::Balanced),
        ..Power::default()
    };
    let (link, fake_system, mut session) = session_on_bus(&runtime, bus.address(), polled);

    // Waiting for the second attempt proves the first partial prime finished
    // and scheduled a retry while `ActiveProfile` remained held failing.
    until(&mut session, |_| {
        daemon.active_attempts.load(Ordering::SeqCst) >= 2
            && daemon.profiles_attempts.load(Ordering::SeqCst) >= 2
    });
    assert_eq!(link.bridge().profiles_moved(), 0);
    assert_eq!(link.bridge().active_profile(), None);

    wait_power(
        &mut session,
        "polled mode stays intact while ActiveProfile fails",
        |power| {
            power["modes"] == serde_json::json!(["saver", "balanced"])
                && power["mode"] == serde_json::json!("balanced")
        },
    );
    let reads_before_action = fake_system.power_reads.load(Ordering::SeqCst);
    assert!(reads_before_action >= 1);

    set_power_mode_ok(&mut session, "saver");
    wait_power(&mut session, "settled saver on polling path", |power| {
        power["pending"] == serde_json::json!([]) && power["mode"] == serde_json::json!("saver")
    });
    assert!(fake_system.power_reads.load(Ordering::SeqCst) > reads_before_action);
    assert_eq!(link.bridge().profiles_moved(), 0);

    // Release `ActiveProfile`: the next retry completes and SystemService goes live.
    daemon.fail_active.store(false, Ordering::SeqCst);
    wait_power(
        &mut session,
        "live full list after ActiveProfile is released",
        |power| {
            power["modes"] == serde_json::json!(["performance", "balanced", "saver"])
                && power["mode"] == serde_json::json!("saver")
        },
    );
    let reads_once_live = fake_system.power_reads.load(Ordering::SeqCst);

    set_power_mode_ok(&mut session, "performance");
    wait_power(&mut session, "settled performance on live path", |power| {
        power["pending"] == serde_json::json!([])
            && power["mode"] == serde_json::json!("performance")
    });
    assert_eq!(
        fake_system.power_reads.load(Ordering::SeqCst),
        reads_once_live
    );
}

/// A first login whose person was never made has no setup to go through and nobody to unlock as:
/// "Tilo didn't start" stands in front with the words founding failed with, and nothing locks it,
/// not the idle timer, a sleep, the lid or the power key, though the same policy locks a session
/// with a person behind it.
#[test]
fn a_first_login_that_made_no_person_says_why_and_never_locks() {
    use std::os::unix::fs::PermissionsExt as _;
    let _settled = tilo_ui_base::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (machine, service) = Machine::start(PowerPolicy {
        idle_lock: Some(Duration::from_millis(60)),
        ..quiet()
    });
    let why = "Tilo could not set up this computer's person: this device's key is another \
               process's";
    let app_wire = tempfile::tempdir().expect("a directory to serve the app wire in");
    std::fs::set_permissions(app_wire.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the app wire's directory is this user's");
    // What startup answers a login on the computer's own daemon whose founding failed.
    let setup = tilo_session::startup::before_shell(
        &tilo_session::startup::Identity::NotStarted(why.to_owned()),
        false,
    );
    let mut session = crate::common::headless(Options {
        services: vec![service],
        power: Some(machine.link.clone()),
        user: "ada".to_owned(),
        verifier: Arc::new(Sesame),
        app_socket: AppSocket::At(app_wire.path().join("actions.sock")),
        surfaces: vec![lock_screen()],
        setup,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).unwrap();
    assert!(
        session.setup_up(),
        "the screen stands in front of the desktop"
    );
    assert!(
        !session.compositor().locked(),
        "nothing locks it as it starts"
    );
    // What every window reads to assistive technology: the lock screen's app the session keeps
    // ready is among them, and stays out of sight.
    let read: String = session
        .a11y_windows()
        .into_iter()
        .flat_map(|(_, _, tree)| tree.nodes)
        .flat_map(|(_, node)| {
            [
                node.label().map(str::to_owned),
                node.description().map(str::to_owned),
            ]
        })
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        read.contains("Tilo didn't start"),
        "it says Tilo didn't start, not setup's first step: {read}"
    );
    assert!(read.contains(why), "it reads why founding failed: {read}");

    step_for(&mut session, Duration::from_millis(400));
    assert!(
        !session.compositor().locked(),
        "the idle timer locks nothing in front of setup"
    );
    machine.sleep(true);
    until(&mut session, |_| !machine.lock_held("sleep"));
    assert!(
        !session.compositor().locked(),
        "a sleep goes ahead at once and locks nothing"
    );
    machine.sleep(false);
    until(&mut session, |_| machine.lock_held("sleep"));
    machine.lid(true);
    press(&mut session, POWER_KEY);
    step_for(&mut session, Duration::from_millis(200));
    assert!(
        !session.compositor().locked(),
        "neither the lid nor the power key locks it"
    );
    assert!(session.setup_up(), "and the screen still stands");
}
