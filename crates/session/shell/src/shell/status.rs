//! Live system status: battery, network, volume. The shell's one copy of
//! these facts; the top bar and Quick Settings both draw from it.
//!
//! Each is followed, not polled: `NetworkManager`'s signals
//! ([`tilo_nm::watch`]), `UPower`'s battery changes, and `PipeWire`'s output
//! level and mute and its running capture streams (one connection,
//! [`media::audio_watch::watch_output_levels_and_capture`]) each ask for a
//! fresh reading of their fact, and the compositor is woken when a reading
//! changes what is shown. Drawing a frame reads nothing and starts nothing. A reading can still cost a subprocess (`wpctl`),
//! and the frame loop must never wait on one — a 200ms `fork`/`exec` is a
//! dropped frame. So reads happen on a plain OS thread (not a tokio task,
//! which would stall the session services behind the blocking
//! `Command::output`), and the frame loop only copies a mutex-guarded
//! snapshot.
//!
//! [`StatusBridge::spawn`] is called from real-session constructors
//! only — a `--shots` render must produce the same pixels everywhere,
//! so with no poller the readings stay `None`, rendered as *nothing*.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::shell::i18n::{t, t_args};

use os_actions::{LinkKind, LinkStatus, VolumeState};
use tilo_lock::MutexRecover as _;
use tilo_nm::{NetworkState, NmState};
use tilo_power_mgmt::{BatterySnapshot, BatteryState};
use ui::floating_surface::HostNetworkStatus;

/// While the link is Wi-Fi, refresh its bars between network changes:
/// `NetworkManager` does not signal signal strength. A wired or absent link
/// has no bars and is not read on a timer. A service that cannot be reached
/// (the system bus, `UPower`, `PipeWire`) is asked again once its socket is
/// created ([`wait_for_socket`]), or on this interval when the socket is
/// there but refused, or cannot be watched.
const FALLBACK_INTERVAL: Duration = Duration::from_secs(30);

/// Which fact a change asks the poller to read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Link,
    Battery,
    Volume,
    /// The number of running capture streams changed; whether any run.
    Media {
        capturing: bool,
    },
}

/// Where the poller's readings come from: the host in a session, a stand-in
/// in tests, so that what each one starts can be counted.
struct Readers {
    wifi_quality: Box<dyn Fn() -> Option<u8> + Send>,
    battery: Box<dyn Fn() -> (BatterySnapshot, Option<tilo_power_mgmt::BatteryTime>) + Send>,
    /// `wpctl`, on a change `PipeWire` reported.
    volume: Box<dyn Fn() -> Result<VolumeState, String> + Send>,
    /// `pw-dump`, which attributes capture streams to executables. Only
    /// started while one runs, and once per change in how many do.
    media: Box<dyn Fn() -> Result<Vec<os_actions::ActiveMediaUse>, String> + Send>,
}

impl Readers {
    fn host() -> Self {
        Self {
            wifi_quality: Box::new(os_actions::wireless_link_quality_pct),
            battery: Box::new(|| {
                (
                    tilo_power_mgmt::sysfs_battery_snapshot(),
                    tilo_power_mgmt::sysfs_battery_time(),
                )
            }),
            volume: Box::new(os_actions::get_volume_state),
            media: Box::new(os_actions::active_media_uses),
        }
    }
}

/// The latest `NetworkManager` reading: written by the follower on the
/// session runtime, read by the poller thread. `None` while the system bus
/// cannot be reached.
type NetworkSlot = Arc<Mutex<Option<NetworkState>>>;

/// Every field is `Option` because "not read yet" and "this machine
/// has none" are both real and both drawn as absence, not a zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    /// `Some` with `present == false` on a machine with no battery.
    pub battery: Option<BatterySnapshot>,
    /// Time to empty or to full, when the battery reports its rate.
    pub battery_time: Option<tilo_power_mgmt::BatteryTime>,
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
    /// Active capture streams, attributed from `PipeWire`'s server security
    /// PID to the kernel executable. Read again only when the number of
    /// running capture streams changes, and without a process while none
    /// runs.
    pub media: Vec<os_actions::ActiveMediaUse>,
}

/// `level` is nominally `0.0..=1.0`, but reads come from `wpctl` output and
/// aren't validated against that range, so clamp defensively: the cast
/// below is only exact because the value is pinned to `0.0..=100.0` first.
fn volume_percent(level: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=100.0 immediately above, so the cast neither truncates nor loses sign"
    )]
    let pct = (level * 100.0).round().clamp(0.0, 100.0) as u32;
    pct
}

impl SystemStatus {
    /// Matches `PowerPolicy`'s default low threshold; a charging
    /// battery is never "low" however empty, because it's filling.
    pub fn battery_is_low(&self) -> bool {
        self.battery
            .is_some_and(|b| b.present && b.state.is_discharging() && b.percent <= 20)
    }

    pub fn battery_is_critical(&self) -> bool {
        self.battery
            .is_some_and(|b| b.present && b.state.is_discharging() && b.percent <= 5)
    }

    pub fn battery_is_charging(&self) -> bool {
        self.battery
            .is_some_and(|b| b.present && matches!(b.state, BatteryState::Charging))
    }

    pub fn battery_percent(&self) -> Option<u8> {
        self.battery.filter(|b| b.present).map(|b| b.percent)
    }

    /// Muted and zero-volume both draw the crossed-out speaker.
    pub fn audio_is_silent(&self) -> bool {
        self.volume
            .is_some_and(|v| v.muted || v.level <= f32::EPSILON)
    }

    /// For the tray's accessible name and tests asserting on meaning.
    pub fn summary(&self) -> String {
        self.reading().unwrap_or_else(|| t("status-summary-empty"))
    }

    /// The readings the machine has, joined, or `None` while it has none:
    /// for a surface that would rather show nothing than a stand-in line.
    pub fn reading(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        match &self.link {
            Some(l) if l.kind == LinkKind::Wired => parts.push(l.name.clone().map_or_else(
                || t("status-wired"),
                |n| t_args("status-wired-named", &[("name", n.into())]),
            )),
            Some(l) if l.kind == LinkKind::Wifi => parts.push(l.name.clone().map_or_else(
                || t("status-wifi"),
                |n| t_args("status-wifi-named", &[("name", n.into())]),
            )),
            Some(_) => parts.push(t("status-offline")),
            None => {}
        }
        if let Some(v) = self.volume {
            parts.push(if v.muted {
                t("status-muted")
            } else {
                t_args(
                    "status-volume",
                    &[("percent", volume_percent(v.level).into())],
                )
            });
        }
        if let Some(pct) = self.battery_percent() {
            parts.push(if self.battery_is_charging() {
                t_args("status-battery-charging", &[("percent", pct.into())])
            } else {
                t_args("status-battery", &[("percent", pct.into())])
            });
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// Cloning shares the same reading (`Arc` bump), so the media-key
/// handler can push a fresh volume in without waiting for the next
/// poll — a volume key must move the indicator *now*.
#[derive(Clone, Debug, Default)]
pub struct StatusBridge {
    inner: Arc<Mutex<SystemStatus>>,
    network: Arc<NetworkStatusSource>,
    /// So a second `spawn` is a no-op, not a second thread racing the first.
    polling: Arc<AtomicBool>,
    /// Wakes the compositor when a followed reading changes what is shown.
    wake: Arc<Mutex<Option<CompositorWake>>>,
}

/// The compositor's wake, held so a followed change can ask for a frame.
#[derive(Clone)]
struct CompositorWake(crate::session_runtime::Wake);

impl std::fmt::Debug for CompositorWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompositorWake")
    }
}

#[derive(Debug)]
struct NetworkStatusSource(tokio::sync::watch::Sender<HostNetworkStatus>);
impl Default for NetworkStatusSource {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(HostNetworkStatus::default()).0)
    }
}

impl StatusBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn network_status(&self) -> HostNetworkStatus {
        *self.network.0.borrow()
    }

    /// Wake the session loop when a followed reading changes the status,
    /// and on sanitized network hint changes.
    pub fn wake_on_changes(&self, wake: crate::session_runtime::Wake) {
        *self.wake.lock_recover() = Some(CompositorWake(Arc::clone(&wake)));
        let mut changes = self.network.0.subscribe();
        if let Ok(runtime) = crate::session_runtime::runtime() {
            runtime.spawn(async move {
                while changes.changed().await.is_ok() {
                    wake();
                }
            });
        }
    }

    /// Follow an owned source in compile-time native acceptance fixtures.
    /// Native production still acquires only the system `NetworkManager` client.
    /// No control socket, environment override or runtime injection is added.
    ///
    /// # Errors
    /// Returns the source subscription error.
    #[cfg(feature = "test-support")]
    pub async fn follow_owned_network_for_test(
        &self,
        client: tilo_nm::NmClient,
    ) -> Result<(), tilo_nm::NmError> {
        let watch = client.watch().await?;
        let (wake, poller_keepalive) = mpsc::channel::<Read>();
        follow_network_readings(&NetworkSlot::default(), &wake, &self.network, watch).await;
        drop(poller_keepalive);
        Ok(())
    }

    pub fn snapshot(&self) -> SystemStatus {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Change the status through `change`, and wake the compositor when
    /// that changed anything.
    fn update(&self, change: impl FnOnce(&mut SystemStatus)) {
        let changed = {
            let mut status = self.inner.lock_recover();
            let before = status.clone();
            change(&mut status);
            *status != before
        };
        if changed && let Some(CompositorWake(wake)) = self.wake.lock_recover().clone() {
            wake();
        }
    }

    pub fn set_link(&self, link: Option<LinkStatus>) {
        self.update(|status| status.link = link);
    }

    /// The battery as read now, and its time to empty or full.
    pub fn set_battery(
        &self,
        battery: BatterySnapshot,
        time: Option<tilo_power_mgmt::BatteryTime>,
    ) {
        self.update(|status| {
            status.battery = Some(battery);
            status.battery_time = time;
        });
    }

    /// A media key or a slider sets it out of band, so the indicator moves
    /// the same frame it was pressed; `PipeWire`'s report of the change
    /// then reads the same state back.
    pub fn set_volume(&self, volume: VolumeState) {
        self.update(|status| status.volume = Some(volume));
    }

    fn set_volume_reading(&self, volume: Option<VolumeState>) {
        self.update(|status| status.volume = volume);
    }

    /// Wakes the compositor when it changes, so the indicator appears
    /// while an app records even on a desktop nobody is touching.
    pub fn set_media(&self, media: Vec<os_actions::ActiveMediaUse>) {
        self.update(|status| status.media = media);
    }

    pub fn is_polling(&self) -> bool {
        self.polling.load(Ordering::Relaxed)
    }

    pub fn spawn(&self) {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("status worker");
        if self.polling.swap(true, Ordering::SeqCst) {
            return;
        }
        let (wake_tx, wake_rx) = mpsc::channel::<Read>();
        let network = NetworkSlot::default();
        spawn_network_follower(
            Arc::clone(&network),
            wake_tx.clone(),
            Arc::clone(&self.network),
        );
        spawn_battery_follower(wake_tx.clone());
        spawn_audio_follower(wake_tx.clone());
        let bridge = self.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-status".to_string())
            .spawn(move || {
                // Held so a follower that never started cannot disconnect the
                // channel and turn the wait in `poll` into a spin.
                let _wake_tx = wake_tx;
                poll(&bridge, &network, &wake_rx, &Readers::host());
            });
        if let Err(e) = spawned {
            tracing::warn!(target: "tilo::status", error = %e, "status poller not started");
            self.polling.store(false, Ordering::SeqCst);
        }
    }
}

/// Read every fact once, then each fact a follower asks for, until every
/// sender of `wake` is gone.
fn poll(bridge: &StatusBridge, network: &NetworkSlot, wake: &Receiver<Read>, readers: &Readers) {
    let mut reads = vec![Read::Link, Read::Battery, Read::Volume];
    loop {
        for read in std::mem::take(&mut reads) {
            apply_read(bridge, network, read, readers);
        }
        // Only the Wi-Fi bars are read on the interval, and only while
        // there are bars to read.
        let on_wifi = bridge
            .snapshot()
            .link
            .is_some_and(|link| link.kind == LinkKind::Wifi);
        let first = if on_wifi {
            match wake.recv_timeout(FALLBACK_INTERVAL) {
                Ok(read) => read,
                Err(mpsc::RecvTimeoutError::Timeout) => Read::Link,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match wake.recv() {
                Ok(read) => read,
                Err(_) => return,
            }
        };
        queue_read(&mut reads, first);
        // A burst (a headset brings a sink, a default switch and a level)
        // is read once.
        while let Ok(read) = wake.recv_timeout(Duration::from_millis(50)) {
            queue_read(&mut reads, read);
        }
    }
}

/// Queue `read` once; of capture changes, only the latest counts.
fn queue_read(reads: &mut Vec<Read>, read: Read) {
    if matches!(read, Read::Media { .. }) {
        reads.retain(|queued| !matches!(queued, Read::Media { .. }));
    }
    if !reads.contains(&read) {
        reads.push(read);
    }
}

fn apply_read(bridge: &StatusBridge, network: &NetworkSlot, read: Read, readers: &Readers) {
    match read {
        Read::Link => {
            let reading = network.lock().ok().and_then(|slot| slot.clone());
            bridge.set_link(link_from_network_state(
                reading.as_ref(),
                (readers.wifi_quality)(),
            ));
        }
        Read::Battery => {
            let (battery, time) = (readers.battery)();
            bridge.set_battery(battery, time);
        }
        Read::Volume => bridge.set_volume_reading((readers.volume)().ok()),
        // Nothing records: nothing to attribute, and no process to start.
        Read::Media { capturing: false } => bridge.set_media(Vec::new()),
        Read::Media { capturing: true } => match (readers.media)() {
            Ok(media) => bridge.set_media(media),
            Err(error) => {
                tracing::debug!(target: "tilo::status", %error, "capture streams not attributed");
            }
        },
    }
}

/// Follow `UPower`'s battery changes on the session runtime and ask the
/// poller to read the battery on each: the same reading, from the same
/// parser, whoever shows it.
fn spawn_battery_follower(wake: Sender<Read>) {
    match crate::session_runtime::runtime() {
        Ok(runtime) => {
            runtime.spawn(follow_battery(wake));
        }
        Err(error) => {
            tracing::warn!(target: "tilo::status", %error, "battery not followed");
        }
    }
}

async fn follow_battery(wake: Sender<Read>) {
    use futures_util::StreamExt as _;
    loop {
        match tilo_power_mgmt::upower::UPowerMonitor::connect().await {
            Ok(upower) => {
                let mut changes = upower.receive_battery_changes().await;
                while changes.next().await.is_some() {
                    if wake.send(Read::Battery).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                tracing::debug!(target: "tilo::status", %error, "UPower not followed");
            }
        }
        if wake.send(Read::Battery).is_err() {
            return;
        }
        wait_for_socket_async(system_bus_socket()).await;
    }
}

/// Follow every output's level and mute, and the running capture streams,
/// in `PipeWire` on a thread of its own: ask the poller to read the volume
/// on each level change, whoever made it, and the capture streams' owners
/// each time the number running changes.
fn spawn_audio_follower(wake: Sender<Read>) {
    let spawned = std::thread::Builder::new()
        .name("tilo-status-audio".to_string())
        .spawn(move || {
            loop {
                let (events, changes) = mpsc::channel();
                let capture = wake.clone();
                match media::audio_watch::watch_output_levels_and_capture(
                    move |event| {
                        let _ = events.send(event);
                    },
                    move |running| {
                        let _ = capture.send(Read::Media {
                            capturing: running > 0,
                        });
                    },
                ) {
                    Ok(watch) => {
                        for event in changes {
                            if event == media::audio_watch::GraphWatchEvent::Lost {
                                break;
                            }
                            if wake.send(Read::Volume).is_err() {
                                return;
                            }
                        }
                        drop(watch);
                    }
                    Err(error) => {
                        tracing::debug!(target: "tilo::status", %error, "output level not followed");
                    }
                }
                // PipeWire went away or never answered: read what is there
                // (nothing, drawn as no glyph; no stream records), then ask
                // again once it can answer.
                if wake.send(Read::Volume).is_err()
                    || wake.send(Read::Media { capturing: false }).is_err()
                {
                    return;
                }
                wait_for_socket(crate::shell::session_audio_policy::pipewire_socket().as_deref());
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(target: "tilo::status", %error, "output level follower not started");
    }
}

/// The system bus's socket, from `DBUS_SYSTEM_BUS_ADDRESS` (its first
/// `unix:path=` address) or the standard path. `None` for an address that
/// names no file (abstract or TCP).
fn system_bus_socket() -> Option<std::path::PathBuf> {
    match std::env::var("DBUS_SYSTEM_BUS_ADDRESS") {
        Ok(address) if !address.is_empty() => system_bus_socket_in(&address),
        _ => Some(std::path::PathBuf::from("/run/dbus/system_bus_socket")),
    }
}

fn system_bus_socket_in(address: &str) -> Option<std::path::PathBuf> {
    address.split(';').find_map(|entry| {
        entry
            .strip_prefix("unix:")?
            .split(',')
            .find_map(|pair| pair.strip_prefix("path="))
            .map(std::path::PathBuf::from)
    })
}

/// Block until a service whose `socket` refused a connection may answer:
/// until the socket is created, when it is missing. When it exists (the
/// service is there and refused), or nothing can be watched, wait
/// [`FALLBACK_INTERVAL`] instead. A missing directory on the way is waited
/// for the same way, one level at a time.
///
/// An idle session without the service (a headless or minimal one) then
/// never wakes for it, where a timer woke it twice a minute.
fn wait_for_socket(socket: Option<&std::path::Path>) {
    let Some(socket) = socket else {
        std::thread::sleep(FALLBACK_INTERVAL);
        return;
    };
    loop {
        if socket.exists() {
            // Created while we waited, or there all along and refusing.
            return;
        }
        // The deepest directory on the way that exists, and its entry
        // toward the socket.
        let mut missing = socket;
        let directory = loop {
            match missing.parent() {
                Some(parent) if parent.is_dir() => break Some(parent),
                Some(parent) => missing = parent,
                None => break None,
            }
        };
        let Some(directory) = directory else {
            std::thread::sleep(FALLBACK_INTERVAL);
            return;
        };
        let (created, appeared) = mpsc::channel();
        let entry = missing.to_owned();
        let watch = tilo_files::native::watch_changes(
            directory,
            move |changed| changed == entry,
            move || {
                let _ = created.send(());
            },
        );
        let Ok(_watch) = watch else {
            std::thread::sleep(FALLBACK_INTERVAL);
            return;
        };
        // Watched from here on: an entry created before this is seen by
        // the check.
        if !missing.exists() && appeared.recv().is_err() {
            std::thread::sleep(FALLBACK_INTERVAL);
            return;
        }
    }
}

/// [`wait_for_socket`] off the session runtime's workers.
async fn wait_for_socket_async(socket: Option<std::path::PathBuf>) {
    if tokio::task::spawn_blocking(move || wait_for_socket(socket.as_deref()))
        .await
        .is_err()
    {
        tokio::time::sleep(FALLBACK_INTERVAL).await;
    }
}

/// Follow `NetworkManager` on the session runtime and wake the poller on
/// each change. Never on the compositor thread: the frame loop only reads
/// the poller's snapshot.
fn spawn_network_follower(slot: NetworkSlot, wake: Sender<Read>, status: Arc<NetworkStatusSource>) {
    match crate::session_runtime::runtime() {
        Ok(runtime) => {
            runtime.spawn(follow_network(slot, wake, status));
        }
        Err(error) => {
            tracing::warn!(target: "tilo::status", %error, "network status not followed");
        }
    }
}

async fn follow_network(slot: NetworkSlot, wake: Sender<Read>, status: Arc<NetworkStatusSource>) {
    loop {
        match tilo_nm::watch().await {
            Ok(watch) => {
                if !follow_network_readings(&slot, &wake, &status, watch).await {
                    return;
                }
            }
            Err(error) => {
                tracing::debug!(target: "tilo::status", %error, "NetworkManager not followed");
            }
        }
        // No system bus, or it went away: draw no link rather than a guess.
        if !publish_network(&slot, &wake, &status, None) {
            return;
        }
        wait_for_socket_async(system_bus_socket()).await;
    }
}

/// The system-bus follower and owned acceptance source share consumption,
/// loss handling and projection; only connection acquisition differs.
async fn follow_network_readings(
    slot: &NetworkSlot,
    wake: &Sender<Read>,
    status: &NetworkStatusSource,
    mut watch: tilo_nm::NmWatch,
) -> bool {
    if !publish_network(slot, wake, status, Some(watch.current())) {
        return false;
    }
    while let Some(state) = watch.changed().await {
        if !publish_network(slot, wake, status, Some(state)) {
            return false;
        }
    }
    publish_network(slot, wake, status, None)
}

/// Store a reading and wake the poller. `false` once the poller is gone.
fn publish_network(
    slot: &NetworkSlot,
    wake: &Sender<Read>,
    status: &NetworkStatusSource,
    state: Option<NetworkState>,
) -> bool {
    let hints = network_hints(state.as_ref());
    let changed = status.0.send_if_modified(|last| {
        if *last == hints {
            false
        } else {
            *last = hints;
            true
        }
    });
    // A certificate renewal that failed while offline tries again.
    if changed {
        tilo_identity_host::provisioning::RENEWAL_CAUSES.changed();
    }
    if let Ok(mut current) = slot.lock() {
        *current = state;
    }
    wake.send(Read::Link).is_ok()
}

/// Project only connectivity and lossless metering, never primary link metadata.
fn network_hints(state: Option<&NetworkState>) -> HostNetworkStatus {
    HostNetworkStatus {
        available: state.and_then(|state| match state.state {
            NmState::Unknown | NmState::Connecting => None,
            NmState::ConnectedSite | NmState::ConnectedGlobal => Some(true),
            _ => Some(false),
        }),
        metered: state.and_then(|state| state.metered),
    }
}

/// The top bar's link for a `NetworkManager` reading: the primary
/// connection, with Wi-Fi bars from `wireless_quality_pct`. `None` when
/// `NetworkManager` cannot be read — the glyph is omitted rather than
/// claiming a state.
pub(crate) fn link_from_network_state(
    state: Option<&NetworkState>,
    wireless_quality_pct: Option<u8>,
) -> Option<LinkStatus> {
    let state = state.filter(|state| state.state != NmState::Unknown)?;
    Some(os_actions::link_status_from_state(
        state,
        wireless_quality_pct,
    ))
}

/// `0` when offline or wired (a cable has no bars), `1..=4` on Wi-Fi.
pub fn link_bars(link: &LinkStatus) -> u8 {
    match link.kind {
        LinkKind::Wifi => link.signal.clamp(1, 4),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_bus_socket_is_the_first_unix_path_of_its_address() {
        assert_eq!(
            system_bus_socket_in("unix:path=/run/dbus/system_bus_socket"),
            Some(std::path::PathBuf::from("/run/dbus/system_bus_socket"))
        );
        assert_eq!(
            system_bus_socket_in("unix:abstract=/tmp/x;unix:guid=1,path=/srv/bus"),
            Some(std::path::PathBuf::from("/srv/bus"))
        );
        assert_eq!(system_bus_socket_in("tcp:host=localhost,port=1"), None);
    }

    /// The wait for a missing service socket on a thread of its own, so the
    /// test can create the socket while it blocks.
    fn waiting_for(socket: std::path::PathBuf) -> mpsc::Receiver<()> {
        let (done, ended) = mpsc::channel();
        std::thread::spawn(move || {
            wait_for_socket(Some(&socket));
            let _ = done.send(());
        });
        ended
    }

    #[test]
    fn a_service_socket_that_exists_is_tried_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("bus");
        std::fs::write(&socket, b"").unwrap();
        tilo_test_env::recv("the wait to end", &waiting_for(socket));
    }

    #[test]
    fn a_missing_service_socket_is_waited_for_until_it_and_its_directory_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run").join("pipewire-0");
        let ended = waiting_for(socket.clone());
        // Neither unrelated entries nor the directory on the way end the
        // wait: it watches one level deeper once that directory exists.
        std::fs::write(dir.path().join("other"), b"").unwrap();
        std::fs::create_dir(dir.path().join("run")).unwrap();
        std::fs::write(dir.path().join("run").join("other"), b"").unwrap();
        assert!(
            ended
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "the wait ended before its socket existed"
        );
        std::fs::write(&socket, b"").unwrap();
        tilo_test_env::recv("the wait to end once the socket exists", &ended);
    }

    #[test]
    fn network_hints_follow_lossless_host_state_and_transport_loss() {
        let slot = NetworkSlot::default();
        let (wake, _receiver) = mpsc::channel();
        let status = NetworkStatusSource::default();
        let mut changes = status.0.subscribe();
        for metered in [None, Some(true), Some(false)] {
            let mut state = NetworkState::unknown();
            state.state = NmState::ConnectedGlobal;
            state.metered = metered;
            assert!(publish_network(&slot, &wake, &status, Some(state)));
            assert_eq!(
                *changes.borrow_and_update(),
                HostNetworkStatus {
                    available: Some(true),
                    metered
                }
            );
        }
        assert!(publish_network(&slot, &wake, &status, None));
        assert_eq!(*changes.borrow_and_update(), HostNetworkStatus::default());
        assert!(publish_network(&slot, &wake, &status, None));
        assert!(!changes.has_changed().unwrap(), "no change means no wake");
        let mut state = NetworkState::unknown();
        state.state = NmState::Disconnected;
        assert_eq!(network_hints(Some(&state)).available, Some(false));
        state.state = NmState::ConnectedLocal;
        assert_eq!(network_hints(Some(&state)).available, Some(false));
    }

    fn battery(percent: u8, state: BatteryState) -> BatterySnapshot {
        BatterySnapshot {
            present: true,
            percent,
            state,
        }
    }

    #[test]
    fn battery_status_reports_only_real_low_or_critical_charge() {
        // Phase `an_unread_status_reports_nothing_rather_than_a_default`.
        {
            let s = SystemStatus::default();
            assert_eq!(s.battery_percent(), None);
            assert!(!s.battery_is_low());
            assert!(!s.audio_is_silent(), "unknown volume is not silence");
            assert_eq!(s.summary(), "System status");
            assert_eq!(s.reading(), None, "nothing read means nothing to show");
        }

        // Phase `a_machine_with_no_battery_draws_no_battery`.
        {
            let s = SystemStatus {
                battery: Some(BatterySnapshot::absent()),
                ..Default::default()
            };
            assert_eq!(
                s.battery_percent(),
                None,
                "absent is not 100% — a desktop shows no gauge at all"
            );
        }

        // Phase `low_is_only_low_while_discharging`.
        {
            let draining = SystemStatus {
                battery: Some(battery(12, BatteryState::Discharging)),
                ..Default::default()
            };
            assert!(draining.battery_is_low());
            assert!(!draining.battery_is_critical());

            let filling = SystemStatus {
                battery: Some(battery(12, BatteryState::Charging)),
                ..Default::default()
            };
            assert!(
                !filling.battery_is_low(),
                "a battery that is filling is not a warning, however empty"
            );
            assert!(filling.battery_is_charging());
        }

        // Phase `critical_is_also_low`.
        {
            let s = SystemStatus {
                battery: Some(battery(3, BatteryState::Discharging)),
                ..Default::default()
            };
            assert!(s.battery_is_critical());
            assert!(s.battery_is_low(), "critical never skips the low styling");
        }
    }

    #[test]
    fn volume_and_network_summaries_read_truthfully() {
        // Phase `zero_volume_reads_as_silent_just_like_mute`.
        {
            let muted = SystemStatus {
                volume: Some(VolumeState {
                    level: 0.6,
                    muted: true,
                }),
                ..Default::default()
            };
            let zeroed = SystemStatus {
                volume: Some(VolumeState {
                    level: 0.0,
                    muted: false,
                }),
                ..Default::default()
            };
            assert!(muted.audio_is_silent());
            assert!(zeroed.audio_is_silent());
        }

        // Phase `summary_names_the_connection_and_the_charge`.
        {
            let s = SystemStatus {
                battery: Some(battery(64, BatteryState::Charging)),
                battery_time: None,
                link: Some(LinkStatus {
                    kind: LinkKind::Wifi,
                    name: Some("Cafe WiFi".into()),
                    signal: 3,
                }),
                volume: Some(VolumeState {
                    level: 0.35,
                    muted: false,
                }),
                media: Vec::new(),
            };
            assert_eq!(
                s.summary(),
                "Wi-Fi: Cafe WiFi, Volume 35%, Battery 64%, charging"
            );
        }

        // Phase `a_wired_link_has_no_bars`.
        {
            let wired = LinkStatus {
                kind: LinkKind::Wired,
                name: Some("eth0".into()),
                signal: 4,
            };
            assert_eq!(link_bars(&wired), 0);
            assert_eq!(link_bars(&LinkStatus::offline()), 0);
        }

        // Phase `a_connected_radio_always_shows_at_least_one_bar`.
        {
            let weak = LinkStatus {
                kind: LinkKind::Wifi,
                name: Some("Far".into()),
                signal: 0,
            };
            assert_eq!(
                link_bars(&weak),
                1,
                "connected-but-weak must not render identically to offline"
            );
        }
    }

    #[test]
    fn status_bridge_publishes_volume_immediately_and_polls_only_when_asked() {
        // Phase `the_bridge_publishes_a_volume_without_waiting_for_a_poll`.
        {
            let b = StatusBridge::new();
            assert_eq!(b.snapshot().volume, None);
            b.set_volume(VolumeState {
                level: 0.8,
                muted: false,
            });
            assert!((b.snapshot().volume.unwrap().level - 0.8).abs() < f32::EPSILON);
        }

        // Phase `a_bridge_starts_no_poller_until_asked`.
        {
            let b = StatusBridge::new();
            assert!(
                !b.is_polling(),
                "a screenshot render must not depend on the host's hardware"
            );
        }
    }

    #[test]
    fn link_from_network_state() {
        use tilo_nm::{LinkKind as NmLinkKind, NmConnectivity, PrimaryLink};
        let home = NetworkState {
            state: NmState::ConnectedGlobal,
            connectivity: NmConnectivity::Full,
            metered: Some(false),
            primary: Some(PrimaryLink {
                kind: NmLinkKind::Wifi,
                name: Some("Home".into()),
                device: Some("wlp2s0".into()),
            }),
        };
        let link = super::link_from_network_state(Some(&home), Some(49));
        assert_eq!(
            link,
            Some(LinkStatus {
                kind: LinkKind::Wifi,
                name: Some("Home".into()),
                signal: 3,
            })
        );
        let status = SystemStatus {
            link,
            ..Default::default()
        };
        assert_eq!(status.summary(), "Wi-Fi: Home");

        let disconnected = NetworkState {
            state: NmState::Disconnected,
            ..NetworkState::unknown()
        };
        assert_eq!(
            super::link_from_network_state(Some(&disconnected), None),
            Some(LinkStatus::offline()),
            "NetworkManager saying nothing is connected reads as offline"
        );
        assert_eq!(
            super::link_from_network_state(Some(&NetworkState::unknown()), None),
            None,
            "an unanswering NetworkManager omits the glyph instead of claiming offline"
        );
        assert_eq!(super::link_from_network_state(None, None), None);
    }

    // The probe observes Linux task scheduling through /proc/self/task.
    #[cfg(target_os = "linux")]
    #[test]
    fn idle_status_thread_sleeps_between_reads() {
        let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
        let (_bridge, tid) = tilo_test_env::idle::spawned_thread("tilo-stat-wait", || {
            let bridge = StatusBridge::new();
            let poller = bridge.clone();
            std::thread::Builder::new()
                .name("tilo-stat-wait".into())
                .spawn(move || {
                    poller.set_link(None);
                    let _ = wake_rx.recv_timeout(FALLBACK_INTERVAL);
                })
                .expect("status idle thread");
            bridge
        });
        tilo_test_env::idle::assert_stays_asleep(&tid, "status fallback wait");
        drop(wake_tx);
    }

    /// Readers that start nothing, and a `pw-dump` stand-in that counts
    /// each process it would have started.
    fn counting_readers(started: &Arc<std::sync::atomic::AtomicUsize>) -> Readers {
        let started = Arc::clone(started);
        Readers {
            wifi_quality: Box::new(|| None),
            battery: Box::new(|| (battery(80, BatteryState::Discharging), None)),
            volume: Box::new(|| Err("no PipeWire".into())),
            media: Box::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
                Ok(vec![os_actions::ActiveMediaUse {
                    client_id: 41,
                    pid: 4100,
                    app_id: "org.example.Recorder".into(),
                    label: "Recorder".into(),
                    binary: "/usr/bin/recorder".into(),
                    kind: os_actions::MediaKind::Microphone,
                }])
            }),
        }
    }

    #[test]
    fn frames_on_an_idle_desktop_start_no_process_for_the_privacy_indicator() {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let readers = counting_readers(&started);
        let bridge = StatusBridge::new();
        let network = NetworkSlot::default();
        let (wake, reads) = mpsc::channel();
        let poller = {
            let bridge = bridge.clone();
            let network = Arc::clone(&network);
            std::thread::spawn(move || poll(&bridge, &network, &reads, &readers))
        };
        // The watch's baseline on an idle desktop: nothing records.
        wake.send(Read::Media { capturing: false }).unwrap();
        // The compositor goes from idle to drawing, again and again (the
        // clock's minute turn, a hover): each frame reads the status.
        for _ in 0..120 {
            assert!(bridge.snapshot().media.is_empty());
        }
        // An app starts recording, as the watch reports it.
        wake.send(Read::Media { capturing: true }).unwrap();
        tilo_test_env::wait_until("the recording app to be attributed", || {
            !bridge.snapshot().media.is_empty()
        });
        for _ in 0..120 {
            assert_eq!(bridge.snapshot().media.len(), 1);
        }
        // It stops: the indicator clears without a process.
        wake.send(Read::Media { capturing: false }).unwrap();
        tilo_test_env::wait_until("the indicator to clear", || {
            bridge.snapshot().media.is_empty()
        });
        drop(wake);
        poller.join().unwrap();
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "only an app starting to record may start pw-dump; idle frames and the stop must not"
        );
    }

    #[test]
    fn a_burst_of_capture_changes_is_attributed_once_at_its_latest() {
        let mut reads = Vec::new();
        queue_read(&mut reads, Read::Media { capturing: true });
        queue_read(&mut reads, Read::Volume);
        queue_read(&mut reads, Read::Media { capturing: false });
        queue_read(&mut reads, Read::Volume);
        assert_eq!(reads, [Read::Volume, Read::Media { capturing: false }]);
    }
}
