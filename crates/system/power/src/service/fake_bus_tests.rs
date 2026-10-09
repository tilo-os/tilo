//! The production [`PowerService`] loop against a fake logind and `UPower` served over a socket
//! pair: a real D-Bus wire, no bus daemon. "Stays asleep" is measured on the loop's own wakeup
//! counter; the old loop ticked every 250 ms, so any idle window of a second tells them apart.

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

/// What the fake daemons were asked, and the far ends of the inhibitor fds they handed out.
#[derive(Default)]
struct Calls {
    /// Every `Inhibit`.
    inhibits: Vec<LogindInhibitorRequest>,
    /// The daemon's end of each inhibitor fd, in `inhibits` order: it reads EOF once the
    /// service drops the lock.
    held: Vec<UnixStream>,
    list_inhibitors: usize,
    suspends: usize,
    /// When each `Suspend` arrived, on the paused clock.
    suspended_at: Vec<Instant>,
}

/// The arguments of one `Inhibit` call.
struct LogindInhibitorRequest {
    what: String,
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
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl FakeLogind {
    fn inhibit(&self, what: &str, who: &str, why: &str, mode: &str) -> OwnedFd {
        let (ours, theirs) = UnixStream::pair().expect("socketpair");
        ours.set_nonblocking(true).expect("nonblocking");
        assert!(!who.is_empty() && !why.is_empty() && !mode.is_empty());
        {
            let mut calls = calls(&self.calls);
            calls.inhibits.push(LogindInhibitorRequest {
                what: what.to_owned(),
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

/// A laptop on battery with its lid open.
struct FakeUPower {
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
        self.devices.clone()
    }
}

/// The composite battery: present and discharging.
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

/// A policy with no idle timeout armed, the lid locking and a critical battery locking — so
/// nothing here suspends unless a test asks.
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
            },
        )
        .expect("logind")
        .serve_at(
            UPOWER_PATH,
            FakeUPower {
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
                // Discharging.
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

/// Longer than the old loop's 250 ms tick several times over.
const IDLE_WINDOW: Duration = Duration::from_millis(1_500);

/// Run until `done` holds, or fail after five seconds.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting: {what}"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

impl Session {
    fn wakeups(&self) -> u64 {
        self.wakeups.load(Ordering::SeqCst)
    }

    /// Wait until the service has finished starting: its wakeup count holds still for 200 ms.
    async fn settle(&self) {
        let start = Instant::now();
        loop {
            let seen = self.wakeups();
            tokio::time::sleep(Duration::from_millis(200)).await;
            if self.wakeups() == seen {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "the service never settled"
            );
        }
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
    let suspended = calls(&session.calls).suspended_at[0];
    assert!(
        suspended.duration_since(requested) < Duration::from_secs(1),
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
