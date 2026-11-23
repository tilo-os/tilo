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
    inhibits: Vec<LogindInhibitorRequest>,
    /// The daemon's end of each inhibitor fd, in `inhibits` order: it reads EOF once
    /// the service drops the lock.
    held: Vec<UnixStream>,
    list_inhibitors: usize,
    /// `UPower.EnumerateDevices`: one per battery-history sample.
    enumerations: usize,
    suspends: usize,
    /// When each `Suspend` arrived.
    suspended_at: Vec<Instant>,
}

/// The arguments of one `Inhibit` call.
#[derive(Clone)]
struct LogindInhibitorRequest {
    what: String,
    mode: String,
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
            calls.inhibits.push(LogindInhibitorRequest {
                what: what.to_owned(),
                mode: mode.to_owned(),
            });
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
    registry: InhibitRegistry,
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
        registry,
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

    /// Whether the service still holds the `i`th lock it took.
    fn lock_held(&self, i: usize) -> bool {
        use std::io::Read;
        let calls = calls(&self.calls);
        let mut byte = [0u8; 1];
        match (&calls.held[i]).read(&mut byte) {
            Ok(0) => false,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
            other => panic!("unexpected read on an inhibitor fd: {other:?}"),
        }
    }

    fn inhibit_index(&self, what: &str) -> Option<usize> {
        calls(&self.calls)
            .inhibits
            .iter()
            .position(|request| request.what == what)
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
async fn an_armed_idle_timeout_reads_logind_locks_at_start_then_on_its_poll_only() {
    let policy = PowerPolicy {
        idle_blank: Some(Duration::from_secs(600)),
        ..quiet_policy()
    };
    let session = start(policy, Vec::new()).await;
    assert_eq!(
        calls(&session.calls).list_inhibitors,
        1,
        "read once at start"
    );
    let before = session.wakeups();
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!(
        calls(&session.calls).list_inhibitors,
        1,
        "the next read waits for the {FOREIGN_INHIBITOR_POLL:?} poll"
    );
    assert_eq!(session.wakeups() - before, 0);
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
async fn an_idle_suspend_rereads_logind_locks_first() {
    let policy = PowerPolicy {
        idle_suspend: Some(Duration::from_secs(600)),
        ..quiet_policy()
    };
    let foreign: LogindInhibitorRow = (
        "sleep".into(),
        "backup".into(),
        "copying".into(),
        "block".into(),
        1000,
        4242,
    );
    let session = start(policy, Vec::new()).await;
    // The lock appears between two polls: only the pre-suspend read can see it.
    session
        .server
        .object_server()
        .interface::<_, FakeLogind>(LOGIND_PATH)
        .await
        .expect("logind iface")
        .get_mut()
        .await
        .foreign = vec![foreign];
    let polls = calls(&session.calls).list_inhibitors;
    session.bridge.request_idle_suspend();
    let mut signals = Vec::new();
    eventually(
        "the compositor hears the idle suspend did not happen",
        || {
            signals.extend(session.bridge.drain_signals());
            signals.contains(&PowerSignal::SuspendFailed)
        },
    )
    .await;
    assert!(
        !signals.contains(&PowerSignal::PreparingForSleep),
        "{signals:?}"
    );
    assert_eq!(calls(&session.calls).list_inhibitors, polls + 1);
    assert_eq!(calls(&session.calls).suspends, 0);
    assert!(session.registry.is_inhibited_for(INHIBIT_SUSPEND));

    // With nothing held, the same request suspends.
    session
        .server
        .object_server()
        .interface::<_, FakeLogind>(LOGIND_PATH)
        .await
        .expect("logind iface")
        .get_mut()
        .await
        .foreign = Vec::new();
    session.bridge.request_idle_suspend();
    eventually("an unheld idle suspend reaches logind", || {
        calls(&session.calls).suspends == 1
    })
    .await;
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
async fn the_logind_mirror_follows_the_registry_without_a_tick() {
    let session = start(quiet_policy(), Vec::new()).await;
    assert_eq!(session.inhibit_index("idle:sleep"), None);
    let hold = session.registry.acquire("media playback");
    eventually("acquiring mirrors into a logind lock", || {
        session.inhibit_index("idle:sleep").is_some()
    })
    .await;
    let mirrored = session.inhibit_index("idle:sleep").expect("mirror lock");
    assert!(session.lock_held(mirrored));
    drop(hold);
    eventually("releasing drops the logind lock", || {
        !session.lock_held(mirrored)
    })
    .await;
    let before = session.wakeups();
    tokio::time::sleep(IDLE_WINDOW).await;
    assert_eq!(session.wakeups() - before, 0, "and then it sleeps again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arming_an_idle_timeout_reads_logind_locks_at_once() {
    let session = start(quiet_policy(), Vec::new()).await;
    assert_eq!(calls(&session.calls).list_inhibitors, 0);
    session.bridge.update_policy(PowerPolicy {
        idle_lock: Some(Duration::from_secs(60)),
        ..quiet_policy()
    });
    eventually(
        "the newly armed timeout reads the locks without a poll's delay",
        || calls(&session.calls).list_inhibitors == 1,
    )
    .await;
    session.settle().await;
    assert_eq!(calls(&session.calls).list_inhibitors, 1);
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

/// The `LogindManager` proxy on the same fake daemon, apart from the service: what it
/// asks logind for reads back as it was asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_proxy_takes_its_locks_and_reads_loginds_verdict() {
    let recorded: Shared = Shared::default();
    let (server, client) = tokio::net::UnixStream::pair().expect("socketpair");
    let server = Builder::unix_stream(server)
        .server(Guid::generate())
        .expect("server")
        .p2p()
        .serve_at(
            LOGIND_PATH,
            FakeLogind {
                calls: Arc::clone(&recorded),
                verdict: "challenge".into(),
                foreign: Vec::new(),
                inhibit_gate: None,
            },
        )
        .expect("logind")
        .build();
    let client = Builder::unix_stream(client).p2p().build();
    let (server, client) = tokio::join!(server, client);
    let (_server, client) = (server.expect("server"), client.expect("client"));
    let manager = LogindManager::with_connection(&client)
        .await
        .expect("proxy");

    assert_eq!(
        manager.can_suspend().await.expect("CanSuspend"),
        "challenge"
    );

    let holder = manager
        .inhibit("idle:sleep", "Tilo power test", "playing back", "block")
        .await
        .expect("Inhibit");
    assert_eq!(holder.what(), "idle:sleep");
    drop(holder);

    let delegated = manager.delegate_handled_keys().await.expect("delegate");
    let asked = calls(&recorded)
        .inhibits
        .last()
        .cloned()
        .expect("the delegate request");
    assert!(
        asked.what.contains("handle-lid-switch") && asked.mode == "block",
        "the lid and power key move to the compositor: {:?} {:?}",
        asked.what,
        asked.mode
    );
    assert_eq!(delegated.what(), asked.what);
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
    serve_base_daemons_with_gate(address, None).await
}

async fn serve_base_daemons_with_gate(
    address: &str,
    inhibit_gate: Option<InhibitGate>,
) -> Connection {
    let calls = Shared::default();
    Builder::address(address)
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
        .expect("base daemons")
}

async fn serve_profiles_on_bus(address: &str, active: &str, profiles: &[&str]) -> Connection {
    Builder::address(address)
        .expect("addr")
        .serve_at(
            PROFILES_PATH,
            FakePowerProfiles {
                active: active.to_owned(),
                profiles: profiles.iter().map(|&name| name.to_owned()).collect(),
            },
        )
        .expect("profiles daemon")
        .name("net.hadess.PowerProfiles")
        .expect("ppd name")
        .build()
        .await
        .expect("profiles daemon conn")
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

async fn apply_quiet_policy(bridge: &PowerBridge, wakeups: &AtomicU64) {
    let before = wakeups.load(Ordering::SeqCst);
    bridge.update_policy(quiet_policy());
    eventually("quiet policy applied", || {
        wakeups.load(Ordering::SeqCst) > before
    })
    .await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn power_profiles_follow_daemon_removal_and_return_without_signals() {
    let bus = tilo_test_env::PrivateBus::start();
    let _base = serve_base_daemons_on_bus(bus.address()).await;
    let daemon1 =
        serve_profiles_on_bus(bus.address(), "power-saver", &["balanced", "power-saver"]).await;
    let (bridge, _registry, _wakeups, _state_home, task) =
        spawn_service_on_bus(bus.address()).await;

    eventually("startup primes power-saver", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("power-saver".to_owned()),
            )
    })
    .await;

    daemon1.close().await.expect("close daemon1");
    eventually("owner exit clears both profiles and active_profile", || {
        bridge.profiles_snapshot() == (Vec::new(), None)
    })
    .await;

    let _daemon2 = serve_profiles_on_bus(
        bus.address(),
        "balanced",
        &["performance", "balanced", "power-saver"],
    )
    .await;
    eventually("replacement owner is read uncached without signals", || {
        bridge.profiles_snapshot()
            == (
                vec![
                    "performance".to_owned(),
                    "balanced".to_owned(),
                    "power-saver".to_owned(),
                ],
                Some("balanced".to_owned()),
            )
    })
    .await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_reads_power_profiles_even_without_an_owner_and_does_not_retry() {
    let bus = tilo_test_env::PrivateBus::start();
    let base = serve_base_daemons_on_bus(bus.address()).await;

    // Verify `name_owner` distinguishes NameHasNoOwner (Ok(None)) from a
    // bus-level failure (Err) on a disconnected connection.
    let probe = Builder::address(bus.address())
        .expect("addr")
        .build()
        .await
        .expect("probe");
    let monitor = PowerProfilesMonitor::with_connection(&probe)
        .await
        .expect("monitor");
    assert_eq!(monitor.name_owner().await.expect("name_owner"), None);
    probe.close().await.expect("close probe");
    assert!(monitor.name_owner().await.is_err());

    // Monitor method calls addressed to net.hadess.PowerProfiles before starting
    // the service so we observe the startup activation read directly.
    let tap = Builder::address(bus.address())
        .expect("addr")
        .build()
        .await
        .expect("tap");
    let mut tap_stream = zbus::MessageStream::from(&tap);
    tap.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus.Monitoring"),
        "BecomeMonitor",
        &(
            &["type='method_call',destination='net.hadess.PowerProfiles'"][..],
            0u32,
        ),
    )
    .await
    .expect("BecomeMonitor");
    let requested = Arc::new(Mutex::new(Vec::<String>::new()));
    let tap_task = tokio::spawn({
        let requested = Arc::clone(&requested);
        async move {
            let _tap = tap;
            while let Some(Ok(msg)) = tap_stream.next().await {
                if msg.header().member().map(zbus::names::MemberName::as_str) == Some("Get")
                    && let Ok((_iface, prop)) = msg.body().deserialize::<(String, String)>()
                {
                    requested.lock().expect("requested").push(prop);
                }
            }
        }
    });

    let (bridge, _registry, wakeups, _state_home, task) = spawn_service_on_bus(bus.address()).await;
    apply_quiet_policy(&bridge, &wakeups).await;

    // Wait for the failed startup read to settle, then confirm startup sent
    // both reads and did not arm a retry past SIGNAL_STREAM_RETRY_DELAY.
    tilo_test_env::within("startup loop settles without daemon", async {
        loop {
            let seen = wakeups.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(200)).await;
            if seen > 0 && wakeups.load(Ordering::SeqCst) == seen {
                return;
            }
        }
    })
    .await;
    let props = requested.lock().expect("requested").clone();
    assert!(
        props.iter().any(|p| p == "Profiles") && props.iter().any(|p| p == "ActiveProfile"),
        "startup must send initial Profiles and ActiveProfile reads even with no owner: {props:?}"
    );
    let settled_wakeups = wakeups.load(Ordering::SeqCst);
    tokio::time::sleep(SIGNAL_STREAM_RETRY_DELAY + Duration::from_millis(500)).await;
    let wakeups_after_window = wakeups.load(Ordering::SeqCst);
    close_lid_on(&base).await;
    eventually("lid close is handled after idle window", || {
        bridge.drain_signals().contains(&PowerSignal::Lock)
    })
    .await;
    assert_eq!(
        wakeups_after_window, settled_wakeups,
        "a failed startup read with no owner must not arm a retry timer"
    );
    assert_eq!(
        requested.lock().expect("requested").len(),
        2,
        "only the initial startup reads are sent when no daemon exists"
    );
    assert_eq!(bridge.profiles_snapshot(), (Vec::new(), None));
    task.abort();
    tap_task.abort();
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

async fn pause_service_in_inhibit(
    registry: &InhibitRegistry,
    gate: &InhibitGate,
    reason: &str,
) -> InhibitToken {
    let hold = registry.acquire(reason);
    eventually("service awaits logind Inhibit", || {
        gate.entered.load(Ordering::SeqCst)
    })
    .await;
    hold
}

async fn emit_active_burst(daemon: &Connection, active: &str, count: usize) {
    let iface = daemon
        .object_server()
        .interface::<_, FakePowerProfiles>(PROFILES_PATH)
        .await
        .expect("profiles iface");
    iface.get_mut().await.active = active.to_owned();
    for _ in 0..count {
        iface
            .get()
            .await
            .active_profile_changed(iface.signal_emitter())
            .await
            .expect("ActiveProfile changed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn property_signals_while_power_profiles_has_no_owner_are_ignored() {
    let bus = tilo_test_env::PrivateBus::start();
    let gate = InhibitGate {
        entered: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        release: Arc::new(Notify::new()),
        sleep_pause: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let base = serve_base_daemons_with_gate(bus.address(), Some(gate.clone())).await;
    let daemon1 =
        serve_profiles_on_bus(bus.address(), "balanced", &["balanced", "power-saver"]).await;
    let (bridge, registry, wakeups, _state_home, task) = spawn_service_on_bus(bus.address()).await;

    eventually("startup primes balanced", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("balanced".to_owned()),
            )
    })
    .await;

    // Hold the service inside logind.Inhibit while daemon1 emits a burst of
    // PropertiesChanged signals and releases the bus name, so both streams are
    // ready together when the loop returns to select!.
    let hold = pause_service_in_inhibit(&registry, &gate, "queue signals across owner exit").await;
    emit_active_burst(&daemon1, "power-saver", 32).await;
    daemon1
        .release_name("net.hadess.PowerProfiles")
        .await
        .expect("release daemon1 name");
    gate.release.notify_one();
    drop(hold);

    eventually("owner exit clears bridge", || {
        bridge.profiles_snapshot() == (Vec::new(), None)
    })
    .await;

    // Confirm with a later event that the trailing signals left no trace.
    apply_quiet_policy(&bridge, &wakeups).await;
    close_lid_on(&base).await;
    eventually("lid close is processed after daemon exit", || {
        bridge.drain_signals().contains(&PowerSignal::Lock)
    })
    .await;
    assert_eq!(bridge.profiles_snapshot(), (Vec::new(), None));
    assert_eq!(
        bridge.active_profile(),
        None,
        "a PropertiesChanged signal arriving after owner exit must be dropped"
    );

    // Once a replacement owner takes the name, both its prime and its later
    // property signals apply again.
    let daemon2 = serve_profiles_on_bus(
        bus.address(),
        "balanced",
        &["performance", "balanced", "power-saver"],
    )
    .await;
    eventually("replacement daemon is primed", || {
        bridge.profiles_snapshot()
            == (
                vec![
                    "performance".to_owned(),
                    "balanced".to_owned(),
                    "power-saver".to_owned(),
                ],
                Some("balanced".to_owned()),
            )
    })
    .await;

    emit_active_burst(&daemon2, "performance", 1).await;
    eventually("later signal from replacement owner applies", || {
        bridge.active_profile().as_deref() == Some("performance")
    })
    .await;
    task.abort();
}

async fn emit_prepare_for_sleep_burst(base: &Connection, count: usize) {
    let logind = base
        .object_server()
        .interface::<_, FakeLogind>(LOGIND_PATH)
        .await
        .expect("logind iface");
    for _ in 0..count {
        FakeLogind::prepare_for_sleep(logind.signal_emitter(), false)
            .await
            .expect("PrepareForSleep(false)");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_signal_from_departed_owner_does_not_overwrite_new_owner() {
    let bus = tilo_test_env::PrivateBus::start();
    let gate = InhibitGate {
        entered: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        release: Arc::new(Notify::new()),
        sleep_pause: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let base = serve_base_daemons_with_gate(bus.address(), Some(gate.clone())).await;
    let daemon1 =
        serve_profiles_on_bus(bus.address(), "balanced", &["balanced", "power-saver"]).await;
    let daemon2 = Builder::address(bus.address())
        .expect("addr")
        .serve_at(
            PROFILES_PATH,
            FakePowerProfiles {
                active: "performance".into(),
                profiles: vec![
                    "performance".into(),
                    "balanced".into(),
                    "power-saver".into(),
                ],
            },
        )
        .expect("daemon2")
        .build()
        .await
        .expect("daemon2 conn");
    let (bridge, registry, _wakeups, _state_home, task) = spawn_service_on_bus(bus.address()).await;

    eventually("startup primes daemon1", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("balanced".to_owned()),
            )
    })
    .await;

    let hold =
        pause_service_in_inhibit(&registry, &gate, "queue signals across owner handoff").await;
    emit_active_burst(&daemon1, "power-saver", 28).await;
    daemon1
        .release_name("net.hadess.PowerProfiles")
        .await
        .expect("release daemon1 name");
    daemon2
        .request_name("net.hadess.PowerProfiles")
        .await
        .expect("daemon2 claims bus name");

    // Queue PrepareForSleep(false) signals whose logind inhibit takes a couple
    // of milliseconds, letting daemon2's prime complete while daemon1's
    // buffered PropertiesChanged signals are still queued on their stream.
    gate.sleep_pause.store(true, Ordering::SeqCst);
    emit_prepare_for_sleep_burst(&base, 24).await;
    gate.release.notify_one();
    drop(hold);

    eventually("daemon2 initial prime lands in bridge", || {
        bridge.profiles_snapshot().0 == ["performance", "balanced", "power-saver"]
    })
    .await;
    gate.sleep_pause.store(false, Ordering::SeqCst);

    // Emit a later Profiles-only signal from daemon2 on the same stream so
    // waiting for it guarantees every queued daemon1 signal has been drained
    // without overwriting ActiveProfile.
    let iface2 = daemon2
        .object_server()
        .interface::<_, FakePowerProfiles>(PROFILES_PATH)
        .await
        .expect("daemon2 iface");
    iface2.get_mut().await.profiles = vec!["performance".into(), "balanced".into()];
    iface2
        .get()
        .await
        .profiles_changed(iface2.signal_emitter())
        .await
        .expect("daemon2 Profiles changed");

    eventually("daemon2 later Profiles signal reaches bridge", || {
        bridge.profiles_snapshot().0 == ["performance", "balanced"]
    })
    .await;
    assert_eq!(
        bridge.active_profile().as_deref(),
        Some("performance"),
        "a departed owner's buffered signal must not overwrite the new owner's active profile"
    );
    task.abort();
}

/// Fake `net.hadess.PowerProfiles` whose properties emit invalidation-only
/// `PropertiesChanged` signals without values, and can optionally stall reads.
struct InvalidatingPowerProfiles {
    active: String,
    profiles: Vec<String>,
    stall_reads: Arc<std::sync::atomic::AtomicBool>,
    stalled_count: Arc<std::sync::atomic::AtomicUsize>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl InvalidatingPowerProfiles {
    #[zbus(property(emits_changed_signal = "invalidates"))]
    async fn active_profile(&self) -> String {
        if self.stall_reads.load(Ordering::SeqCst) {
            self.stalled_count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        self.active.clone()
    }

    #[zbus(property(emits_changed_signal = "invalidates"))]
    async fn profiles(&self) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
        if self.stall_reads.load(Ordering::SeqCst) {
            self.stalled_count.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        profile_entries(&self.profiles)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalidated_power_profile_properties_are_reread_into_bridge() {
    let (server, client) = p2p_with_profiles_iface(InvalidatingPowerProfiles {
        active: "balanced".into(),
        profiles: vec!["balanced".into(), "power-saver".into()],
        stall_reads: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        stalled_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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
    let task = tokio::spawn(async move {
        let _client = client;
        service.run().await;
    });

    eventually("startup primes invalidating daemon", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("balanced".to_owned()),
            )
    })
    .await;

    let iface = server
        .object_server()
        .interface::<_, InvalidatingPowerProfiles>(PROFILES_PATH)
        .await
        .expect("power profiles iface");
    iface.get_mut().await.active = "power-saver".into();
    iface
        .get()
        .await
        .active_profile_invalidate(iface.signal_emitter())
        .await
        .expect("ActiveProfile invalidated");
    eventually("invalidated ActiveProfile is re-read into bridge", || {
        bridge.active_profile().as_deref() == Some("power-saver")
    })
    .await;

    iface.get_mut().await.profiles = vec!["performance".into(), "balanced".into()];
    iface
        .get()
        .await
        .profiles_invalidate(iface.signal_emitter())
        .await
        .expect("Profiles invalidated");
    eventually("invalidated Profiles is re-read into bridge", || {
        bridge.profiles_snapshot().0 == ["performance", "balanced"]
    })
    .await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalidated_property_reread_against_a_silent_daemon_does_not_block_lid_handling() {
    let bus = tilo_test_env::PrivateBus::start();
    let base = serve_base_daemons_on_bus(bus.address()).await;
    let stall_reads = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stalled_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let daemon = Builder::address(bus.address())
        .expect("addr")
        .serve_at(
            PROFILES_PATH,
            InvalidatingPowerProfiles {
                active: "balanced".into(),
                profiles: vec!["balanced".into(), "power-saver".into()],
                stall_reads: Arc::clone(&stall_reads),
                stalled_count: Arc::clone(&stalled_count),
            },
        )
        .expect("invalidating daemon")
        .name("net.hadess.PowerProfiles")
        .expect("ppd name")
        .build()
        .await
        .expect("invalidating conn");

    let (bridge, _registry, wakeups, _state_home, task) = spawn_service_on_bus(bus.address()).await;
    eventually("startup primes invalidating daemon", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("balanced".to_owned()),
            )
    })
    .await;
    apply_quiet_policy(&bridge, &wakeups).await;

    stall_reads.store(true, Ordering::SeqCst);
    let iface = daemon
        .object_server()
        .interface::<_, InvalidatingPowerProfiles>(PROFILES_PATH)
        .await
        .expect("power profiles iface");
    iface
        .get()
        .await
        .active_profile_invalidate(iface.signal_emitter())
        .await
        .expect("ActiveProfile invalidated");
    eventually("invalidation triggers a stalled re-read", || {
        stalled_count.load(Ordering::SeqCst) >= 1
    })
    .await;

    close_lid_on(&base).await;
    eventually(
        "lid close still locks while invalidated re-read is stalled",
        || bridge.drain_signals().contains(&PowerSignal::Lock),
    )
    .await;
    task.abort();
}

/// Fake `net.hadess.PowerProfiles` whose property reads wait for `release`
/// after recording that a read began, so tests can observe intermediate bridge
/// state before a prime completes.
struct GatedPowerProfiles {
    active: String,
    profiles: Vec<String>,
    reads_started: Arc<std::sync::atomic::AtomicUsize>,
    release: tokio::sync::watch::Receiver<bool>,
}

#[zbus::interface(name = "net.hadess.PowerProfiles")]
impl GatedPowerProfiles {
    #[zbus(property)]
    async fn active_profile(&self) -> String {
        self.reads_started.fetch_add(1, Ordering::SeqCst);
        let mut release = self.release.clone();
        let _ = release.wait_for(|&open| open).await;
        self.active.clone()
    }

    #[zbus(property)]
    async fn profiles(&self) -> Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
        self.reads_started.fetch_add(1, Ordering::SeqCst);
        let mut release = self.release.clone();
        let _ = release.wait_for(|&open| open).await;
        profile_entries(&self.profiles)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacing_the_power_profiles_owner_clears_bridge_before_reading_the_new_daemon() {
    let bus = tilo_test_env::PrivateBus::start();
    let _base = serve_base_daemons_on_bus(bus.address()).await;

    let daemon1 = Builder::address(bus.address())
        .expect("addr")
        .serve_at(
            PROFILES_PATH,
            FakePowerProfiles {
                active: "power-saver".into(),
                profiles: vec!["balanced".into(), "power-saver".into()],
            },
        )
        .expect("daemon1")
        .build()
        .await
        .expect("daemon1 conn");
    let reply1 = daemon1
        .request_name_with_flags(
            "net.hadess.PowerProfiles",
            zbus::fdo::RequestNameFlags::AllowReplacement | zbus::fdo::RequestNameFlags::DoNotQueue,
        )
        .await
        .expect("daemon1 claims bus name allowing replacement");
    assert_eq!(reply1, zbus::fdo::RequestNameReply::PrimaryOwner);

    let reads_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let daemon2 = Builder::address(bus.address())
        .expect("addr")
        .serve_at(
            PROFILES_PATH,
            GatedPowerProfiles {
                active: "performance".into(),
                profiles: vec![
                    "performance".into(),
                    "balanced".into(),
                    "power-saver".into(),
                ],
                reads_started: Arc::clone(&reads_started),
                release: release_rx,
            },
        )
        .expect("daemon2")
        .build()
        .await
        .expect("daemon2 conn");

    let observer = Builder::address(bus.address())
        .expect("addr")
        .build()
        .await
        .expect("observer");
    let mut owner_stream = zbus::fdo::DBusProxy::builder(&observer)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .expect("dbus proxy")
        .receive_name_owner_changed_with_args(&[(0, "net.hadess.PowerProfiles")])
        .await
        .expect("owner stream");

    let (bridge, _registry, _wakeups, _state_home, task) =
        spawn_service_on_bus(bus.address()).await;

    eventually("startup primes first owner", || {
        bridge.profiles_snapshot()
            == (
                vec!["balanced".to_owned(), "power-saver".to_owned()],
                Some("power-saver".to_owned()),
            )
    })
    .await;

    let observed_snapshots = Arc::new(Mutex::new(Vec::new()));
    bridge.on_profiles_change(Arc::new({
        let bridge = bridge.clone();
        let observed = Arc::clone(&observed_snapshots);
        move || {
            observed
                .lock()
                .expect("snapshots")
                .push(bridge.profiles_snapshot());
        }
    }));

    let reply2 = daemon2
        .request_name_with_flags(
            "net.hadess.PowerProfiles",
            zbus::fdo::RequestNameFlags::ReplaceExisting | zbus::fdo::RequestNameFlags::DoNotQueue,
        )
        .await
        .expect("daemon2 replaces existing owner");
    assert_eq!(reply2, zbus::fdo::RequestNameReply::PrimaryOwner);

    let transition =
        tilo_test_env::within("direct NameOwnerChanged transition", owner_stream.next())
            .await
            .expect("owner transition signal");
    let args = transition.args().expect("NameOwnerChanged args");
    assert_eq!(
        args.old_owner().as_ref(),
        daemon1.unique_name().map(std::ops::Deref::deref),
        "the previous owner must still hold the name when replaced"
    );
    assert_eq!(
        args.new_owner().as_ref(),
        daemon2.unique_name().map(std::ops::Deref::deref),
        "the new owner must take the name directly without the name becoming free"
    );

    eventually("replacement prime starts against second owner", || {
        reads_started.load(Ordering::SeqCst) >= 1
    })
    .await;

    assert_eq!(
        bridge.profiles_snapshot(),
        (Vec::new(), None),
        "replacing the bus owner must clear the previous owner's profiles before reading the new owner"
    );
    assert_eq!(
        bridge.active_profile(),
        None,
        "replacing the bus owner must clear the previous owner's active profile"
    );

    let _ = release_tx.send(true);
    eventually("second owner's profile state lands in bridge", || {
        bridge.profiles_snapshot()
            == (
                vec![
                    "performance".to_owned(),
                    "balanced".to_owned(),
                    "power-saver".to_owned(),
                ],
                Some("performance".to_owned()),
            )
    })
    .await;

    assert_eq!(
        *observed_snapshots.lock().expect("snapshots"),
        vec![
            (Vec::new(), None),
            (
                vec![
                    "performance".to_owned(),
                    "balanced".to_owned(),
                    "power-saver".to_owned(),
                ],
                Some("performance".to_owned()),
            ),
        ],
        "bridge listeners must see the cleared state before the replacement owner's state"
    );
    task.abort();
}
