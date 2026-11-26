//! The production [`PowerService`] loop against a fake logind and `UPower` served
//! over a socket pair: a real D-Bus wire, no bus daemon. "Stays asleep" is
//! measured on the loop's own wakeup counter.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath};
use zbus::{Connection, Guid};

use super::*;

const LOGIND_PATH: &str = "/org/freedesktop/login1";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DISPLAY_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";

/// What the fake daemons were asked, and the far ends of the inhibitor fds they
/// handed out.
#[derive(Default)]
struct Calls {
    /// The daemon's end of each inhibitor fd: it reads EOF once
    /// the service drops the lock.
    held: Vec<UnixStream>,
    list_inhibitors: usize,
    /// `UPower.EnumerateDevices`: one per battery-history sample.
    enumerations: usize,
    suspends: usize,
    /// When each `Suspend` arrived.
    suspended_at: Vec<Instant>,
}

type Shared = Arc<Mutex<Calls>>;

fn calls(shared: &Shared) -> std::sync::MutexGuard<'_, Calls> {
    shared.lock().expect("calls")
}

struct FakeLogind {
    calls: Shared,
    /// What `CanSuspend` answers.
    verdict: String,
    /// Another program's locks, as `ListInhibitors` reports them.
    foreign: Vec<LogindInhibitorRow>,
    /// Optional pause gate for `Inhibit` calls so bus tests can queue concurrent
    /// signals and owner transitions while the service awaits logind.
    inhibit_gate: Option<InhibitGate>,
}

#[derive(Clone)]
struct InhibitGate {
    entered: Arc<std::sync::atomic::AtomicBool>,
    release: Arc<Notify>,
    sleep_pause: Arc<std::sync::atomic::AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl FakeLogind {
    async fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> OwnedFd {
        if let Some(gate) = &self.inhibit_gate {
            if what == "idle:sleep" {
                gate.entered.store(true, Ordering::SeqCst);
                gate.release.notified().await;
            } else if what == "sleep" && gate.sleep_pause.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        }
        let (ours, theirs) = UnixStream::pair().expect("socketpair");
        ours.set_nonblocking(true).expect("nonblocking");
        assert!(!who.is_empty() && !why.is_empty() && !mode.is_empty());
        {
            let mut calls = calls(&self.calls);
            calls.held.push(ours);
        }
        OwnedFd::from(std::os::fd::OwnedFd::from(theirs))
    }

    fn list_inhibitors(&self) -> Vec<LogindInhibitorRow> {
        let mut calls = calls(&self.calls);
        calls.list_inhibitors = calls.list_inhibitors.saturating_add(1);
        drop(calls);
        self.foreign.clone()
    }

    fn can_suspend(&self) -> String {
        self.verdict.clone()
    }

    fn suspend(&self, interactive: bool) {
        assert!(!interactive, "the service never asks logind to prompt");
        let mut calls = calls(&self.calls);
        calls.suspends = calls.suspends.saturating_add(1);
        calls.suspended_at.push(Instant::now());
    }

    #[zbus(signal)]
    async fn prepare_for_sleep(emitter: &SignalEmitter<'_>, start: bool) -> zbus::Result<()>;
}

struct FakeUPower {
    calls: Shared,
    lid_closed: bool,
    lid_present: bool,
    on_battery: bool,
    devices: Vec<OwnedObjectPath>,
}

#[zbus::interface(name = "org.freedesktop.UPower")]
impl FakeUPower {
    #[zbus(property)]
    fn lid_is_closed(&self) -> bool {
        self.lid_closed
    }

    #[zbus(property)]
    fn lid_is_present(&self) -> bool {
        self.lid_present
    }

    #[zbus(property)]
    fn on_battery(&self) -> bool {
        self.on_battery
    }

    fn enumerate_devices(&self) -> Vec<OwnedObjectPath> {
        let mut calls = calls(&self.calls);
        calls.enumerations = calls.enumerations.saturating_add(1);
        drop(calls);
        self.devices.clone()
    }
}

struct FakeDisplayDevice {
    kind: u32,
    present: bool,
    percentage: f64,
    state: u32,
    capacity: f64,
}

#[zbus::interface(name = "org.freedesktop.UPower.Device")]
impl FakeDisplayDevice {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> u32 {
        self.kind
    }

    #[zbus(property)]
    fn is_present(&self) -> bool {
        self.present
    }

    #[zbus(property)]
    fn percentage(&self) -> f64 {
        self.percentage
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.state
    }

    #[zbus(property)]
    fn capacity(&self) -> f64 {
        self.capacity
    }
}

struct Session {
    server: Connection,
    calls: Shared,
    bridge: PowerBridge,
    wakeups: Arc<AtomicU64>,
    _state_home: tempfile::TempDir,
}

/// A policy with no idle timeout armed, the lid locking and a critical battery
/// locking, so nothing here suspends unless a test asks.
fn quiet_policy() -> PowerPolicy {
    PowerPolicy {
        idle_blank: None,
        idle_lock: None,
        idle_suspend: None,
        lid_close_action: PowerAction::Lock,
        critical_action: PowerAction::Lock,
        ..PowerPolicy::default()
    }
}

async fn start(policy: PowerPolicy, foreign: Vec<LogindInhibitorRow>) -> Session {
    let calls = Shared::default();
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
                foreign,
                inhibit_gate: None,
            },
        )
        .expect("logind")
        .serve_at(
            UPOWER_PATH,
            FakeUPower {
                calls: Arc::clone(&calls),
                lid_closed: false,
                lid_present: true,
                on_battery: true,
                devices: Vec::new(),
            },
        )
        .expect("upower")
        .serve_at(
            DISPLAY_PATH,
            FakeDisplayDevice {
                kind: 2,
                present: true,
                percentage: 80.0,
                state: 2,
                capacity: 90.0,
            },
        )
        .expect("display device")
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(server, client);
    let (server, client) = (server.expect("server"), client.expect("client"));

    let bridge = PowerBridge::new();
    let registry = InhibitRegistry::new();
    let state_home = tempfile::tempdir().expect("state home");
    let mut service =
        PowerService::with_connection(&client, bridge.clone(), registry.clone(), policy)
            .await
            .expect("service");
    service.history = PowerHistoryWriter::in_state_home(Some(state_home.path().to_path_buf()));
    let wakeups = service.wakeup_counter();
    tokio::spawn(async move {
        let _client = client;
        service.run().await;
    });
    let session = Session {
        server,
        calls,
        bridge,
        wakeups,
        _state_home: state_home,
    };
    session.settle().await;
    session
}

const IDLE_WINDOW: Duration = Duration::from_millis(1_500);

/// Run until `done` holds.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    tilo_test_env::within(what, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
}

impl Session {
    fn wakeups(&self) -> u64 {
        self.wakeups.load(Ordering::SeqCst)
    }

    /// Wait until the service is running its loop and has gone quiet: its wakeup
    /// count is past zero and holds still for 200 ms.
    async fn settle(&self) {
        tilo_test_env::within("the service to settle", async {
            loop {
                let seen = self.wakeups();
                tokio::time::sleep(Duration::from_millis(200)).await;
                if seen > 0 && self.wakeups() == seen {
                    return;
                }
            }
        })
        .await;
    }

    async fn set_lid(&self, closed: bool) {
        let iface = self
            .server
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
    }

    async fn set_percentage(&self, percentage: f64) {
        let iface = self
            .server
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
    }

    async fn resume(&self) {
        let iface = self
            .server
            .object_server()
            .interface::<_, FakeLogind>(LOGIND_PATH)
            .await
            .expect("logind iface");
        FakeLogind::prepare_for_sleep(iface.signal_emitter(), false)
            .await
            .expect("PrepareForSleep(false)");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_service_does_not_wake() {
    let session = start(quiet_policy(), Vec::new()).await;
    let before = session.wakeups();
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!(
        session.wakeups() - before,
        0,
        "with nothing happening and no idle timeout armed, the service sleeps"
    );
    assert_eq!(
        calls(&session.calls).list_inhibitors,
        0,
        "logind's inhibitor list is not read while no idle timeout is armed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspend_request_is_handled_without_waiting_for_a_tick() {
    let session = start(quiet_policy(), Vec::new()).await;
    let before = session.wakeups();
    let requested = Instant::now();
    session.bridge.request_suspend();
    eventually("the request reaches logind", || {
        calls(&session.calls).suspends == 1
    })
    .await;
    // That no tick was waited for is in the wakeup count below; how soon Suspend
    // arrived is the machine's to report.
    let suspended = calls(&session.calls).suspended_at[0];
    eprintln!(
        "Suspend arrived {:?} after the request",
        suspended.duration_since(requested)
    );
    assert!(
        session
            .bridge
            .drain_signals()
            .contains(&PowerSignal::PreparingForSleep),
        "the compositor is told to lock before the machine sleeps"
    );
    session.settle().await;
    assert!(
        session.wakeups() - before <= 2,
        "the command and the finished Suspend call wake the loop, nothing else: {}",
        session.wakeups() - before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lid_close_signal_triggers_policy() {
    let session = start(quiet_policy(), Vec::new()).await;
    assert!(session.bridge.drain_signals().is_empty());
    session.set_lid(true).await;
    eventually("closing the lid locks the session", || {
        session.bridge.drain_signals().contains(&PowerSignal::Lock)
    })
    .await;
    // Staying closed does not act again; reopening does not act at all.
    session.set_lid(true).await;
    session.set_lid(false).await;
    session.settle().await;
    assert!(session.bridge.drain_signals().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_battery_change_is_acted_on_when_upower_announces_it() {
    let session = start(quiet_policy(), Vec::new()).await;
    assert!(session.bridge.drain_signals().is_empty(), "80 % is quiet");
    session.set_percentage(4.0).await;
    let mut signals = Vec::new();
    eventually("a critical battery locks per policy", || {
        signals.extend(session.bridge.drain_signals());
        signals.contains(&PowerSignal::Lock)
    })
    .await;
    assert!(
        signals
            .iter()
            .any(|signal| matches!(signal, PowerSignal::Notify(n) if n.critical)),
        "{signals:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resuming_samples_what_came_due_during_sleep() {
    let session = start(quiet_policy(), Vec::new()).await;
    assert_eq!(
        calls(&session.calls).enumerations,
        1,
        "one history sample at start"
    );
    session.resume().await;
    eventually("the history is sampled again after resume", || {
        calls(&session.calls).enumerations == 2
    })
    .await;
    assert!(
        session
            .bridge
            .drain_signals()
            .contains(&PowerSignal::ResumedFromSleep)
    );
}

const PROFILES_PATH: &str = "/net/hadess/PowerProfiles";

fn profile_entries(
    profiles: &[String],
) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
    profiles
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

struct FakePowerProfiles {
    active: String,
    profiles: Vec<String>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl FakePowerProfiles {
    #[zbus(property)]
    fn active_profile(&self) -> String {
        self.active.clone()
    }

    #[zbus(property)]
    fn profiles(&self) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
        profile_entries(&self.profiles)
    }
}

async fn p2p_with_profiles_iface<I>(iface: I) -> (Connection, Connection)
where
    I: zbus::object_server::Interface,
{
    let calls = Shared::default();
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
                foreign: Vec::new(),
                inhibit_gate: None,
            },
        )
        .expect("logind")
        .serve_at(
            UPOWER_PATH,
            FakeUPower {
                calls: Arc::clone(&calls),
                lid_closed: false,
                lid_present: true,
                on_battery: true,
                devices: Vec::new(),
            },
        )
        .expect("upower")
        .serve_at(
            DISPLAY_PATH,
            FakeDisplayDevice {
                kind: 2,
                present: true,
                percentage: 80.0,
                state: 2,
                capacity: 90.0,
            },
        )
        .expect("display device")
        .serve_at(PROFILES_PATH, iface)
        .expect("power profiles")
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(server, client);
    (server.expect("server"), client.expect("client"))
}

async fn start_with_profiles(
    active: &str,
    profiles: &[&str],
) -> (Connection, PowerBridge, tempfile::TempDir) {
    let (server, client) = p2p_with_profiles_iface(FakePowerProfiles {
        active: active.to_owned(),
        profiles: profiles.iter().map(|&name| name.to_owned()).collect(),
    })
    .await;

    let bridge = PowerBridge::new();
    let registry = InhibitRegistry::new();
    let state_home = tempfile::tempdir().expect("state home");
    let mut service =
        PowerService::with_connection(&client, bridge.clone(), registry, quiet_policy())
            .await
            .expect("service");
    service.history = PowerHistoryWriter::in_state_home(Some(state_home.path().to_path_buf()));
    tokio::spawn(async move {
        let _client = client;
        service.run().await;
    });
    (server, bridge, state_home)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn power_profiles_and_active_profile_follow_dbus_signals_live() {
    let (server, bridge, _state_home) =
        start_with_profiles("balanced", &["balanced", "power-saver"]).await;

    eventually("startup primes profiles and active profile", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("balanced".to_owned()),
            )
    })
    .await;

    let iface = server
        .object_server()
        .interface::<_, FakePowerProfiles>(PROFILES_PATH)
        .await
        .expect("power profiles iface");
    iface.get_mut().await.active = "power-saver".into();
    iface
        .get()
        .await
        .active_profile_changed(iface.signal_emitter())
        .await
        .expect("ActiveProfile changed");
    eventually("ActiveProfile signal updates bridge live", || {
        bridge.active_profile().as_deref() == Some("power-saver")
    })
    .await;

    iface.get_mut().await.profiles = vec!["performance".into(), "balanced".into()];
    iface
        .get()
        .await
        .profiles_changed(iface.signal_emitter())
        .await
        .expect("Profiles changed");
    eventually("Profiles signal updates bridge live", || {
        bridge.profiles_snapshot().0 == ["performance", "balanced"]
    })
    .await;
}

async fn serve_base_daemons_on_bus(address: &str) -> Connection {
    serve_base_daemons_with_gate(address, None).await.0
}

async fn serve_base_daemons_with_gate(
    address: &str,
    inhibit_gate: Option<InhibitGate>,
) -> (Connection, Shared) {
    let calls = Shared::default();
    let conn = Builder::address(address)
        .expect("addr")
        .serve_at(
            LOGIND_PATH,
            FakeLogind {
                calls: Arc::clone(&calls),
                verdict: "yes".into(),
                foreign: Vec::new(),
                inhibit_gate,
            },
        )
        .expect("logind")
        .serve_at(
            UPOWER_PATH,
            FakeUPower {
                calls: Arc::clone(&calls),
                lid_closed: false,
                lid_present: true,
                on_battery: true,
                devices: Vec::new(),
            },
        )
        .expect("upower")
        .serve_at(
            DISPLAY_PATH,
            FakeDisplayDevice {
                kind: 2,
                present: true,
                percentage: 80.0,
                state: 2,
                capacity: 90.0,
            },
        )
        .expect("display")
        .name("org.freedesktop.login1")
        .expect("login1")
        .name("org.freedesktop.UPower")
        .expect("upower")
        .build()
        .await
        .expect("base daemons");
    (conn, calls)
}

async fn spawn_service_on_bus(
    address: &str,
) -> (
    PowerBridge,
    InhibitRegistry,
    Arc<AtomicU64>,
    tempfile::TempDir,
    tokio::task::JoinHandle<()>,
) {
    let client = Builder::address(address)
        .expect("addr")
        .build()
        .await
        .expect("client");
    let bridge = PowerBridge::new();
    let registry = InhibitRegistry::new();
    let state_home = tempfile::tempdir().expect("state home");
    let mut service =
        PowerService::with_connection(&client, bridge.clone(), registry.clone(), quiet_policy())
            .await
            .expect("service");
    service.history = PowerHistoryWriter::in_state_home(Some(state_home.path().to_path_buf()));
    let wakeups = service.wakeup_counter();
    let task = tokio::spawn(async move {
        let _client = client;
        service.run().await;
    });
    (bridge, registry, wakeups, state_home, task)
}

async fn close_lid_on(conn: &Connection) {
    let upower = conn
        .object_server()
        .interface::<_, FakeUPower>(UPOWER_PATH)
        .await
        .expect("upower iface");
    upower.get_mut().await.lid_closed = true;
    upower
        .get()
        .await
        .lid_is_closed_changed(upower.signal_emitter())
        .await
        .expect("LidIsClosed");
}

/// Fake `net.hadess.PowerProfiles` that owns the bus name but never answers
/// property reads.
struct SilentPowerProfiles {
    reads_started: Arc<std::sync::atomic::AtomicUsize>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl SilentPowerProfiles {
    #[zbus(property)]
    async fn active_profile(&self) -> String {
        self.reads_started.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }

    #[zbus(property)]
    async fn profiles(&self) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
        self.reads_started.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_power_profiles_daemon_does_not_block_lid_or_sleep_handling() {
    let bus = tilo_test_env::PrivateBus::start();
    let base = serve_base_daemons_on_bus(bus.address()).await;
    let (bridge, _registry, wakeups, _state_home, task) = spawn_service_on_bus(bus.address()).await;

    // Wait until start() has finished subscribing to lid/sleep/owner streams and
    // the loop is running in select!, then bring up a silent PowerProfiles daemon.
    bridge.update_policy(quiet_policy());
    eventually("service loop enters select!", || {
        wakeups.load(Ordering::SeqCst) >= 1
    })
    .await;

    let reads_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _silent = Builder::address(bus.address())
        .expect("addr")
        .serve_at(
            PROFILES_PATH,
            SilentPowerProfiles {
                reads_started: Arc::clone(&reads_started),
            },
        )
        .expect("silent daemon")
        .name("net.hadess.PowerProfiles")
        .expect("ppd name")
        .build()
        .await
        .expect("silent conn");

    eventually("priming starts against the silent daemon", || {
        reads_started.load(Ordering::SeqCst) >= 1
    })
    .await;

    close_lid_on(&base).await;
    let mut signals = Vec::new();
    eventually(
        "lid close locks while power-profiles-daemon is silent",
        || {
            signals.extend(bridge.drain_signals());
            signals.contains(&PowerSignal::Lock)
        },
    )
    .await;

    let logind = base
        .object_server()
        .interface::<_, FakeLogind>(LOGIND_PATH)
        .await
        .expect("logind iface");
    FakeLogind::prepare_for_sleep(logind.signal_emitter(), true)
        .await
        .expect("PrepareForSleep");

    eventually(
        "PrepareForSleep locks while power-profiles-daemon is silent",
        || {
            signals.extend(bridge.drain_signals());
            signals.contains(&PowerSignal::PreparingForSleep)
        },
    )
    .await;
    task.abort();
}
