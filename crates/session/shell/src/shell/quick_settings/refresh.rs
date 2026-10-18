//! Off-thread quick-settings reads.
//!
//! The panel's reads run 4.5s cold / 230ms warm inline, dominated by
//! `nmcli device wifi list` — a stall here reads as the machine being
//! broken, so they run on a worker; the panel opens immediately with the
//! last known values and updates as readings land.
//!
//! Tiered so a slow list never holds up the panel's switches and sliders:
//! the Wi-Fi and Bluetooth lists are their own tiers, read on open because
//! the tiles name the connected network and accessory. A worker owns no
//! `Rc` and sends plain data over a channel, keeping it safe alongside the
//! shell's single-threaded `RefCell` tree.
//!
//! Opening the panel reads everything once ([`QsRefresh::kick_fast`]). A
//! write the panel makes reads back only what it wrote
//! ([`QsRefresh::kick_field`]): flipping Bluetooth asks `BlueZ`, not every
//! daemon on the machine.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};

use os_actions as qs;
use tilo_power_mgmt::BatterySnapshot;

use crate::shell::chrome_state::DisplayBrightness;

/// Every field independently optional: `None` means "not read", which
/// leaves the panel's value alone rather than blanking it — a missing
/// daemon, or a field the panel wrote after this read had started. Boxed in
/// [`QsReading::Fast`] since it dwarfs the other variants' payloads.
#[derive(Debug)]
pub struct FastReading {
    pub brightness: Option<f32>,
    /// Every monitor whose brightness can be set, named for display.
    pub displays: Option<Vec<DisplayBrightness>>,
    /// Whether the default sink is a real output with a readable level.
    pub volume_available: Option<bool>,
    pub volume: Option<qs::VolumeState>,
    pub bluetooth: Option<bool>,
    /// A successful `BlueZ` state query proves that an adapter exists.
    pub bluetooth_available: bool,
    pub power: Option<qs::PowerProfile>,
    /// The profiles the daemon advertises, when it answered.
    pub power_profiles: Option<Vec<qs::PowerProfile>>,
    pub wired: Option<qs::WiredInfo>,
    /// The same reading, from the same parser, as the top bar's battery.
    pub battery: BatterySnapshot,
    /// One `nmcli` radio snapshot; Wi-Fi on/off is derived from it.
    pub radios: Option<qs::RadioState>,
    pub privacy: qs::PrivacyStatus,
    pub vpns: Option<Vec<qs::VpnConnectionInfo>>,
}

impl FastReading {
    /// Drop what `reader` produced: the panel wrote that state after this
    /// read started, so landing it would flip the control back, and the next
    /// press would then repeat the change instead of reversing it.
    fn forget(&mut self, reader: Reader) {
        match reader {
            Reader::Radios => self.radios = None,
            Reader::Bluetooth => self.bluetooth = None,
            Reader::Wired => self.wired = None,
            Reader::Power => self.power = None,
            Reader::Vpns => self.vpns = None,
            Reader::Brightness => {
                self.brightness = None;
                self.displays = None;
            }
            Reader::Volume => {
                self.volume = None;
                self.volume_available = None;
            }
        }
    }

    /// A reading from a machine with none of these devices.
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            brightness: None,
            displays: Some(Vec::new()),
            volume_available: Some(false),
            volume: None,
            bluetooth: None,
            bluetooth_available: false,
            power: None,
            power_profiles: None,
            wired: None,
            battery: BatterySnapshot::absent(),
            radios: None,
            privacy: qs::PrivacyStatus::default(),
            vpns: None,
        }
    }
}

/// A control the panel writes, named so the read-back after the write
/// covers that control and nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QsField {
    Wifi,
    Bluetooth,
    Wired,
    Airplane,
    Hotspot,
    Mobile,
    Power,
    Vpn,
    Brightness,
    Volume,
}

impl QsField {
    /// Wi-Fi, airplane mode, the hotspot and mobile broadband are all one
    /// `nmcli` radio snapshot.
    const fn reader(self) -> Reader {
        match self {
            Self::Wifi | Self::Airplane | Self::Hotspot | Self::Mobile => Reader::Radios,
            Self::Bluetooth => Reader::Bluetooth,
            Self::Wired => Reader::Wired,
            Self::Power => Reader::Power,
            Self::Vpn => Reader::Vpns,
            Self::Brightness => Reader::Brightness,
            Self::Volume => Reader::Volume,
        }
    }
}

/// The OS read behind one or more [`QsField`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Reader {
    Radios,
    Bluetooth,
    Wired,
    Power,
    Vpns,
    Brightness,
    Volume,
}

impl Reader {
    const ALL: [Self; 7] = [
        Self::Radios,
        Self::Bluetooth,
        Self::Wired,
        Self::Power,
        Self::Vpns,
        Self::Brightness,
        Self::Volume,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

/// What one field's read-back found. `None` leaves the panel's value alone.
#[derive(Debug)]
pub enum FieldReading {
    Radios(Option<qs::RadioState>),
    Bluetooth(Option<bool>),
    Wired(Option<qs::WiredInfo>),
    Power(Option<qs::PowerProfile>),
    Vpns(Option<Vec<qs::VpnConnectionInfo>>),
    Brightness(Option<f32>, Vec<DisplayBrightness>),
    Volume(Option<qs::VolumeState>),
}

impl FieldReading {
    const fn reader(&self) -> Reader {
        match self {
            Self::Radios(_) => Reader::Radios,
            Self::Bluetooth(_) => Reader::Bluetooth,
            Self::Wired(_) => Reader::Wired,
            Self::Power(_) => Reader::Power,
            Self::Vpns(_) => Reader::Vpns,
            Self::Brightness(..) => Reader::Brightness,
            Self::Volume(_) => Reader::Volume,
        }
    }
}

#[derive(Debug)]
pub enum QsReading {
    Fast(Box<FastReading>),
    Field(FieldReading),
    Firmware(Option<Vec<qs::FirmwareUpdate>>),
    WifiNetworks(Result<Vec<qs::WifiNetworkInfo>, String>),
    BtDevices(Option<Vec<qs::BtDeviceInfo>>),
    WifiJoin(WifiJoinResult),
}

#[derive(Debug)]
pub struct WifiJoinResult {
    pub ssid: String,
    pub password: Option<String>,
    pub shared_credential: bool,
    pub result: Result<(), String>,
}

/// The OS reads behind the panel, one function each. Production uses
/// [`ReadFns::OS`]; tests substitute counting stubs to prove which reads a
/// kick performs, without depending on which tools the host has.
#[derive(Clone, Copy)]
pub(crate) struct ReadFns {
    pub(crate) brightness: fn() -> Option<f32>,
    pub(crate) displays: fn() -> Vec<DisplayBrightness>,
    pub(crate) output_device: fn() -> bool,
    pub(crate) volume: fn() -> Option<qs::VolumeState>,
    pub(crate) bluetooth: fn() -> Option<bool>,
    pub(crate) power: fn() -> Option<qs::PowerProfile>,
    pub(crate) power_profiles: fn() -> Option<Vec<qs::PowerProfile>>,
    pub(crate) wired: fn() -> Option<qs::WiredInfo>,
    pub(crate) battery: fn() -> BatterySnapshot,
    pub(crate) radios: fn() -> Option<qs::RadioState>,
    pub(crate) privacy: fn() -> qs::PrivacyStatus,
    pub(crate) vpns: fn() -> Option<Vec<qs::VpnConnectionInfo>>,
}

impl ReadFns {
    pub(crate) const OS: Self = Self {
        brightness: os_brightness,
        displays: os_displays,
        output_device: qs::has_output_device,
        volume: os_volume,
        bluetooth: os_bluetooth,
        power: os_power,
        power_profiles: os_power_profiles,
        wired: os_wired,
        battery: tilo_power_mgmt::sysfs_battery_snapshot,
        radios: os_radios,
        privacy: qs::get_privacy_status,
        vpns: os_vpns,
    };

    /// Everything the panel shows, read once each.
    fn fast(&self) -> FastReading {
        let bluetooth = (self.bluetooth)();
        let volume = (self.output_device)().then(self.volume).flatten();
        FastReading {
            brightness: (self.brightness)(),
            displays: Some((self.displays)()),
            volume_available: Some(volume.is_some()),
            volume,
            bluetooth_available: bluetooth.is_some(),
            bluetooth,
            power: (self.power)(),
            power_profiles: (self.power_profiles)(),
            wired: (self.wired)(),
            battery: (self.battery)(),
            radios: (self.radios)(),
            privacy: (self.privacy)(),
            vpns: (self.vpns)(),
        }
    }

    fn field(&self, reader: Reader) -> FieldReading {
        match reader {
            Reader::Radios => FieldReading::Radios((self.radios)()),
            Reader::Bluetooth => FieldReading::Bluetooth((self.bluetooth)()),
            Reader::Wired => FieldReading::Wired((self.wired)()),
            Reader::Power => FieldReading::Power((self.power)()),
            Reader::Vpns => FieldReading::Vpns((self.vpns)()),
            Reader::Brightness => FieldReading::Brightness((self.brightness)(), (self.displays)()),
            Reader::Volume => FieldReading::Volume((self.volume)()),
        }
    }
}

fn os_brightness() -> Option<f32> {
    qs::log_get("get_brightness", qs::get_brightness())
}

fn os_displays() -> Vec<DisplayBrightness> {
    display_brightness(
        qs::BrightnessService::global().rescan_blocking(),
        settings_store::hardware::DisplayHardwareSnapshot::load(),
    )
}

fn os_volume() -> Option<qs::VolumeState> {
    qs::log_get("get_volume_state", qs::get_volume_state())
}

fn os_bluetooth() -> Option<bool> {
    qs::log_get("get_bluetooth", qs::get_bluetooth())
}

fn os_power() -> Option<qs::PowerProfile> {
    qs::log_get("get_power_profile", qs::get_power_profile())
}

fn os_power_profiles() -> Option<Vec<qs::PowerProfile>> {
    qs::log_get("available_power_profiles", qs::available_power_profiles())
}

fn os_wired() -> Option<qs::WiredInfo> {
    qs::log_get("get_wired", qs::get_wired())
}

fn os_radios() -> Option<qs::RadioState> {
    qs::log_get("get_radio_state", qs::get_radio_state())
}

fn os_vpns() -> Option<Vec<qs::VpnConnectionInfo>> {
    qs::log_get("list_vpn_connections", qs::list_vpn_connections())
}

/// Tracked so a second open while a read is running doesn't pile up
/// another worker behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tier {
    Fast,
    Field(Reader),
    Wifi,
    Bt,
    Firmware,
    WifiJoin,
}

/// A device-state read the panel asked for, recorded in tests so a caller
/// can assert *which* reconcile it requested.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kick {
    Fast,
    Field(QsField),
}

type PendingWork = Box<dyn FnOnce() -> QsReading + Send + 'static>;

/// Per-reader count of the panel's own writes.
type WriteCounts = [u64; Reader::ALL.len()];

pub struct QsRefresh {
    tx: Sender<QsReading>,
    rx: Receiver<QsReading>,
    /// Wakes the owning UI instance when a detached read finishes. Without
    /// this edge an idle compositor can leave "Scanning…" or stale slider
    /// values on screen until the next unrelated input event.
    wake: ui::WakeHandle,
    host_wake: Option<crate::session_runtime::Wake>,
    /// Which tiers currently have a worker outstanding.
    in_flight: HashSet<Tier>,
    /// The latest follow-up requested for each busy tier. Repeated input while
    /// a daemon read is outstanding collapses to one rerun, but is never
    /// discarded: the active read may have sampled state before the input.
    rerun: HashMap<Tier, PendingWork>,
    /// Turned off when the host's action dispatcher has been replaced,
    /// so a seeded Wi-Fi list can't get overwritten by a real scan.
    enabled: bool,
    reads: ReadFns,
    /// Counts the panel's own writes, per reader. A read started before the
    /// latest write to a field may have sampled its old value.
    writes: WriteCounts,
    /// The write counts each tier's running read started under.
    started: HashMap<Tier, WriteCounts>,
    #[cfg(test)]
    kicks: Vec<Kick>,
}

impl Default for QsRefresh {
    fn default() -> Self {
        Self::new()
    }
}

impl QsRefresh {
    pub fn new() -> Self {
        Self::with_os_reads(true)
    }

    pub(crate) fn without_os_reads() -> Self {
        Self::with_os_reads(false)
    }

    fn with_os_reads(enabled: bool) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            wake: ui::WakeHandle::new(),
            host_wake: None,
            in_flight: HashSet::new(),
            rerun: HashMap::new(),
            enabled,
            reads: ReadFns::OS,
            writes: [0; Reader::ALL.len()],
            started: HashMap::new(),
            #[cfg(test)]
            kicks: Vec::new(),
        }
    }

    /// Read through `reads` instead of the OS.
    #[cfg(test)]
    pub(crate) fn with_reads(reads: ReadFns) -> Self {
        Self {
            reads,
            ..Self::new()
        }
    }

    /// Every device-state read requested since the last call, whether or
    /// not OS reads are enabled.
    #[cfg(test)]
    pub(crate) fn take_kicks(&mut self) -> Vec<Kick> {
        std::mem::take(&mut self.kicks)
    }

    /// Whether an OS read is outstanding. This is operational state, not an
    /// animation signal: a slow or wedged daemon must never force the entire
    /// desktop to repaint continuously while a worker waits for its bounded
    /// timeout. The compositor's normal tick/keepalive drains completed
    /// readings and their chrome revision bump schedules the visible frame.
    pub fn is_in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Native Shell owns a calloop wake rather than an App wake scope.
    pub(crate) fn set_host_wake(&mut self, wake: crate::session_runtime::Wake) {
        self.host_wake = Some(wake);
    }

    pub fn disable_os_reads(&mut self) {
        self.enabled = false;
        self.rerun.clear();
    }

    fn spawn<F>(&mut self, tier: Tier, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
        if !self.enabled {
            return;
        }
        #[cfg(test)]
        crate::shell::test_support::ambient_start("QS worker");
        if self.in_flight.contains(&tier) {
            self.rerun.insert(tier, Box::new(work));
            return;
        }
        self.spawn_boxed(tier, Box::new(work));
    }

    fn spawn_boxed(&mut self, tier: Tier, work: PendingWork) {
        self.in_flight.insert(tier);
        self.started.insert(tier, self.writes);
        let tx = self.tx.clone();
        let wake = self.wake.clone();
        let host_wake = self.host_wake.clone();
        // Detached: bounded by `os_actions::DEFAULT_TIMEOUT`, and a
        // send into a dropped receiver is a no-op.
        std::thread::spawn(move || {
            if tx.send(work()).is_ok() {
                wake.wake();
                if let Some(host_wake) = host_wake {
                    host_wake();
                }
            }
        });
    }

    /// Read everything the panel shows. Only the panel opening asks for
    /// this; a write reads back its own field through [`Self::kick_field`].
    pub fn kick_fast(&mut self) {
        #[cfg(test)]
        self.kicks.push(Kick::Fast);
        let reads = self.reads;
        self.spawn(Tier::Fast, move || QsReading::Fast(Box::new(reads.fast())));
    }

    /// Read back `field` after the panel changed it, since even a successful
    /// daemon request can be normalized by hardware or policy. A reading of
    /// that field already under way is superseded: it is dropped when it
    /// lands, and this read follows it.
    pub fn kick_field(&mut self, field: QsField) {
        #[cfg(test)]
        self.kicks.push(Kick::Field(field));
        let reader = field.reader();
        self.note_write(reader);
        let reads = self.reads;
        self.spawn(Tier::Field(reader), move || {
            QsReading::Field(reads.field(reader))
        });
    }

    fn note_write(&mut self, reader: Reader) {
        if let Some(count) = self.writes.get_mut(reader.index()) {
            *count = count.wrapping_add(1);
        }
    }

    pub fn kick_firmware(&mut self) {
        self.spawn(Tier::Firmware, || {
            QsReading::Firmware(qs::log_get(
                "get_firmware_updates",
                qs::get_firmware_updates(),
            ))
        });
    }

    /// Read the networks `NetworkManager` already knows. Never waits for a
    /// radio scan, which takes over ten seconds on some adapters: the tile's
    /// "connected to" must not depend on one.
    pub fn kick_wifi_networks(&mut self) {
        self.spawn(Tier::Wifi, || {
            QsReading::WifiNetworks(qs::list_wifi_networks())
        });
    }

    /// The person asked to look again: scan, then read. A failed scan still
    /// reads the known list; the failure is logged.
    pub fn kick_wifi_rescan(&mut self) {
        self.spawn(Tier::Wifi, || {
            if let Err(error) = qs::scan_wifi() {
                tracing::warn!(target: "tilo::os_actions", op = "scan_wifi", %error, "Wi-Fi scan failed");
            }
            QsReading::WifiNetworks(qs::list_wifi_networks())
        });
    }

    pub fn kick_bt_devices(&mut self) {
        self.spawn(Tier::Bt, || {
            QsReading::BtDevices(qs::log_get("list_bt_devices", qs::list_bt_devices()))
        });
    }

    /// Join without ever waiting on `NetworkManager` from the compositor/input
    /// thread. Only one join is admitted at a time; the visible prompt/row is
    /// already marked busy before this method returns.
    pub fn kick_wifi_join(
        &mut self,
        ssid: String,
        password: Option<String>,
        shared_credential: bool,
    ) {
        self.spawn(Tier::WifiJoin, move || {
            let result = qs::connect_wifi(&ssid, password.as_deref());
            QsReading::WifiJoin(WifiJoinResult {
                ssid,
                password,
                shared_credential,
                result,
            })
        });
    }

    /// Every reading arrived since the last call. Never blocks.
    pub fn drain(&mut self) -> Vec<QsReading> {
        let mut out = Vec::new();
        while let Ok(reading) = self.rx.try_recv() {
            let tier = match &reading {
                QsReading::Fast(_) => Tier::Fast,
                QsReading::Field(field) => Tier::Field(field.reader()),
                QsReading::WifiNetworks(_) => Tier::Wifi,
                QsReading::BtDevices(_) => Tier::Bt,
                QsReading::Firmware(_) => Tier::Firmware,
                QsReading::WifiJoin(_) => Tier::WifiJoin,
            };
            self.in_flight.remove(&tier);
            let started = self.started.remove(&tier);
            let written_since = |reader: Reader| {
                started.is_some_and(|counts| {
                    counts.get(reader.index()) != self.writes.get(reader.index())
                })
            };
            let reading = match reading {
                QsReading::Fast(mut fast) => {
                    for reader in Reader::ALL {
                        if written_since(reader) {
                            fast.forget(reader);
                        }
                    }
                    Some(QsReading::Fast(fast))
                }
                // The write that superseded it also queued its rerun.
                QsReading::Field(field) if written_since(field.reader()) => None,
                other => Some(other),
            };
            if let Some(work) = self.rerun.remove(&tier) {
                self.spawn_boxed(tier, work);
            }
            out.extend(reading);
        }
        out
    }

    #[cfg(test)]
    pub(crate) fn kick_fast_with<F>(&mut self, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
        self.spawn(Tier::Fast, work);
    }

    /// [`Self::kick_field`] with substitute work, so a test controls when
    /// the read lands.
    #[cfg(test)]
    pub(crate) fn kick_field_with<F>(&mut self, field: QsField, work: F)
    where
        F: FnOnce() -> FieldReading + Send + 'static,
    {
        let reader = field.reader();
        self.note_write(reader);
        self.spawn(Tier::Field(reader), move || QsReading::Field(work()));
    }

    #[cfg(test)]
    pub(crate) fn kick_wifi_networks_with<F>(&mut self, work: F)
    where
        F: FnOnce() -> Result<Vec<qs::WifiNetworkInfo>, String> + Send + 'static,
    {
        self.spawn(Tier::Wifi, move || QsReading::WifiNetworks(work()));
    }

    #[cfg(test)]
    pub(crate) fn kick_wifi_join_with<F>(
        &mut self,
        ssid: String,
        password: Option<String>,
        shared_credential: bool,
        work: F,
    ) where
        F: FnOnce(&str, Option<&str>) -> Result<(), String> + Send + 'static,
    {
        self.spawn(Tier::WifiJoin, move || {
            let result = work(&ssid, password.as_deref());
            QsReading::WifiJoin(WifiJoinResult {
                ssid,
                password,
                shared_credential,
                result,
            })
        });
    }

    pub fn os_reads_enabled(&self) -> bool {
        self.enabled
    }
}

/// Name each controllable monitor from the compositor's display snapshot:
/// its EDID model, "Built-in display" for the laptop panel, else its
/// connector.
fn display_brightness(
    levels: Vec<qs::MonitorLevel>,
    snapshot: Option<settings_store::hardware::DisplayHardwareSnapshot>,
) -> Vec<crate::shell::chrome_state::DisplayBrightness> {
    let outputs = snapshot
        .map(|snapshot| snapshot.outputs)
        .unwrap_or_default();
    levels
        .into_iter()
        .map(|level| {
            let output = outputs
                .iter()
                .find(|output| output.descriptor.label() == level.connector);
            let name = match output {
                Some(output) if output.builtin => crate::shell::i18n::t("display-identify-builtin"),
                Some(output) => output
                    .descriptor
                    .monitor
                    .as_ref()
                    .map(settings_store::hardware::MonitorIdentity::display_name)
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| level.connector.clone()),
                None if qs::brightness::is_builtin_connector(&level.connector) => {
                    crate::shell::i18n::t("display-identify-builtin")
                }
                None => level.connector.clone(),
            };
            crate::shell::chrome_state::DisplayBrightness {
                connector: level.connector,
                name,
                level: level.level,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn monitors_are_named_by_edid_model_or_else_connector() {
        use settings_store::hardware::{
            DisplayHardwareSnapshot, DisplayOutputSnapshot, MonitorIdentity, OutputDescriptor,
            OutputMode,
        };
        let output = |connector: &str, builtin: bool, monitor: Option<MonitorIdentity>| {
            let mode = OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            };
            DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: connector.into(),
                    connector_label: connector.into(),
                    modes: vec![mode],
                    hdr: false,
                    vrr: false,
                    color_profiles: Vec::new(),
                    monitor,
                },
                current_mode: mode,
                scale: 1.0,
                builtin,
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                unavailable_reason: None,
                color_profile_labels: std::collections::BTreeMap::new(),
            }
        };
        let snapshot = DisplayHardwareSnapshot {
            outputs: vec![
                output("eDP-1", true, None),
                output(
                    "DP-2",
                    false,
                    Some(MonitorIdentity {
                        make: "Dell".into(),
                        model: "DELL U2720Q".into(),
                        ..MonitorIdentity::default()
                    }),
                ),
            ],
            ..DisplayHardwareSnapshot::default()
        };
        let level = |connector: &str| qs::MonitorLevel {
            connector: connector.into(),
            level: 0.5,
            kind: qs::BrightnessKind::Ddc,
        };
        let names: Vec<String> = display_brightness(
            vec![level("eDP-1"), level("DP-2"), level("HDMI-A-1")],
            Some(snapshot),
        )
        .into_iter()
        .map(|display| display.name)
        .collect();
        assert_eq!(names, ["Built-in display", "Dell U2720Q", "HDMI-A-1"]);
    }

    fn empty_fast() -> QsReading {
        QsReading::Fast(Box::new(FastReading::empty()))
    }

    /// A read that blocks until the test releases it, and records whether
    /// it had finished — so "did the caller wait for it" is a fact, not a
    /// wall-clock guess. An inline read would only finish after the hang
    /// guard expires, and then report itself finished.
    fn gated_read() -> (
        std::sync::mpsc::Sender<()>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        impl FnOnce() + Send + 'static,
    ) {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_finished = std::sync::Arc::clone(&finished);
        let wait = move || {
            let _ = release_rx.recv_timeout(tilo_test_env::hang_bound());
            worker_finished.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        (release_tx, finished, wait)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn dispatched_reads_and_joins_never_block_the_caller_and_deliver_results() {
        fn dispatching_a_slow_read_does_not_block_the_caller() {
            let mut r = QsRefresh::new();
            let (release, finished, wait) = gated_read();
            r.kick_fast_with(move || {
                wait();
                empty_fast()
            });
            assert!(
                !finished.load(std::sync::atomic::Ordering::SeqCst),
                "kick returned only after the read finished: it was performed inline"
            );
            assert!(r.is_in_flight(), "the tier should be marked outstanding");
            assert!(
                r.drain().is_empty(),
                "no reading can have landed yet — draining must not block waiting for one"
            );
            release.send(()).expect("release the read");
        }

        fn dispatching_a_slow_wifi_join_does_not_block_and_delivers_its_result() {
            let mut refresh = QsRefresh::without_os_reads();
            // Explicit test work must remain enabled even though ambient host
            // reads are disabled for this deterministic fixture.
            refresh.enabled = true;
            let (release_tx, finished, wait) = gated_read();
            refresh.kick_wifi_join_with(
                "Cafe".into(),
                Some("secret".into()),
                false,
                move |_, _| {
                    wait();
                    Ok(())
                },
            );
            assert!(
                !finished.load(std::sync::atomic::Ordering::SeqCst),
                "the join ran inline"
            );
            assert!(refresh.is_in_flight());
            assert!(refresh.drain().is_empty());
            release_tx.send(()).expect("release wifi worker");

            let deadline = tilo_test_env::hang_deadline();
            loop {
                let readings = refresh.drain();
                if let Some(QsReading::WifiJoin(result)) = readings.into_iter().next() {
                    assert_eq!(result.ssid, "Cafe");
                    assert_eq!(result.password.as_deref(), Some("secret"));
                    assert!(result.result.is_ok());
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "wifi result timed out"
                );
                std::thread::yield_now();
            }
            assert!(!refresh.is_in_flight());
        }

        fn a_dispatched_reading_lands_and_clears_its_tier() {
            let mut r = QsRefresh::new();
            r.kick_fast_with(|| {
                QsReading::Fast(Box::new(FastReading {
                    brightness: Some(0.42),
                    bluetooth: Some(true),
                    ..FastReading::empty()
                }))
            });
            let deadline = tilo_test_env::hang_deadline();
            let mut got = Vec::new();
            while got.is_empty() && Instant::now() < deadline {
                got = r.drain();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(got.len(), 1, "the reading never arrived");
            match &got[0] {
                QsReading::Fast(reading) => {
                    assert_eq!(reading.brightness, Some(0.42));
                    assert_eq!(reading.bluetooth, Some(true));
                }
                other => panic!("wrong tier: {other:?}"),
            }
            assert!(
                !r.is_in_flight(),
                "the tier should be clear once its reading lands"
            );
        }

        fn completed_read_wakes_its_owning_ui_instance() {
            let (wake_tx, wake_rx) = std::sync::mpsc::channel();
            let mut scope = ui::WakeScope::new();
            scope
                .bind(move |_| wake_tx.send(()).expect("wake receiver alive"))
                .expect("bind wake destination");
            let mut refresh = {
                let _entered = scope.enter();
                QsRefresh::new()
            };

            refresh.kick_fast_with(empty_fast);

            tilo_test_env::recv("worker completion must wake an otherwise-idle UI", &wake_rx);
            assert_eq!(refresh.drain().len(), 1);
        }

        dispatching_a_slow_read_does_not_block_the_caller();
        dispatching_a_slow_wifi_join_does_not_block_and_delivers_its_result();
        a_dispatched_reading_lands_and_clears_its_tier();
        completed_read_wakes_its_owning_ui_instance();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn refresh_tiers_collapse_in_flight_dispatches_and_track_independently() {
        fn dispatches_while_in_flight_collapse_to_one_latest_rerun() {
            use std::sync::atomic::{AtomicUsize, Ordering};

            let mut r = QsRefresh::new();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let runs = std::sync::Arc::new(AtomicUsize::new(0));
            let first_runs = std::sync::Arc::clone(&runs);
            r.kick_fast_with(move || {
                first_runs.fetch_add(1, Ordering::SeqCst);
                started_tx.send(()).expect("test is waiting for worker");
                release_rx.recv().expect("test releases first worker");
                empty_fast()
            });

            tilo_test_env::recv(
                "first worker must be active before reruns are queued",
                &started_rx,
            );
            let selected_rerun = std::sync::Arc::new(AtomicUsize::new(0));
            for marker in 1..=3 {
                let rerun_runs = std::sync::Arc::clone(&runs);
                let selected_rerun = std::sync::Arc::clone(&selected_rerun);
                r.kick_fast_with(move || {
                    rerun_runs.fetch_add(1, Ordering::SeqCst);
                    selected_rerun.store(marker, Ordering::SeqCst);
                    empty_fast()
                });
            }
            assert_eq!(
                runs.load(Ordering::SeqCst),
                1,
                "queued reruns must not overlap the active worker"
            );

            release_tx.send(()).expect("release first worker");
            let deadline = tilo_test_env::hang_deadline();
            let mut total = 0;
            while (total < 2 || r.is_in_flight()) && Instant::now() < deadline {
                total += r.drain().len();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                total, 2,
                "one active read plus one coalesced rerun must complete"
            );
            assert_eq!(runs.load(Ordering::SeqCst), 2, "only two workers may run");
            assert_eq!(
                selected_rerun.load(Ordering::SeqCst),
                3,
                "the latest queued read must provide the authoritative state"
            );
            assert!(!r.is_in_flight());
        }

        fn tiers_are_tracked_independently() {
            let mut r = QsRefresh::new();
            assert!(!r.is_in_flight());
            r.kick_wifi_networks();
            assert!(r.is_in_flight());
            r.kick_fast_with(empty_fast);
            let deadline = tilo_test_env::hang_deadline();
            let mut saw_fast = false;
            while !saw_fast && Instant::now() < deadline {
                for reading in r.drain() {
                    if matches!(reading, QsReading::Fast(_)) {
                        saw_fast = true;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                saw_fast,
                "the fast tier never reported while Wi-Fi was outstanding"
            );
        }

        fn a_read_started_before_a_write_never_lands_the_written_field() {
            let mut r = QsRefresh::new();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            // Opening the panel samples Bluetooth as on...
            r.kick_fast_with(move || {
                started_tx.send(()).expect("test is waiting for worker");
                release_rx.recv().expect("test releases the read");
                QsReading::Fast(Box::new(FastReading {
                    bluetooth: Some(true),
                    power: Some(qs::PowerProfile::Balanced),
                    ..FastReading::empty()
                }))
            });
            tilo_test_env::recv("the stale read is under way", &started_rx);
            // ...while the person turns it off.
            r.kick_field_with(QsField::Bluetooth, || FieldReading::Bluetooth(Some(false)));
            release_tx.send(()).expect("release the stale read");

            let deadline = tilo_test_env::hang_deadline();
            let mut fast = Vec::new();
            let mut fields = Vec::new();
            while (fast.is_empty() || fields.is_empty() || r.is_in_flight())
                && Instant::now() < deadline
            {
                for reading in r.drain() {
                    match reading {
                        QsReading::Fast(reading) => fast.push((reading.bluetooth, reading.power)),
                        QsReading::Field(FieldReading::Bluetooth(on)) => fields.push(on),
                        other => panic!("unexpected reading: {other:?}"),
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                fast,
                [(None, Some(qs::PowerProfile::Balanced))],
                "the open read still lands what nobody wrote, but not the switch written after it started"
            );
            assert_eq!(
                fields,
                [Some(false)],
                "only the read after the write may set the control; the earlier one would flip it back"
            );
        }

        fn a_field_read_started_before_a_second_write_never_lands() {
            let mut r = QsRefresh::new();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            r.kick_field_with(QsField::Power, move || {
                started_tx.send(()).expect("test is waiting for worker");
                release_rx.recv().expect("test releases the read");
                FieldReading::Power(Some(qs::PowerProfile::Saver))
            });
            tilo_test_env::recv("the first read-back is under way", &started_rx);
            r.kick_field_with(QsField::Power, || {
                FieldReading::Power(Some(qs::PowerProfile::Performance))
            });
            release_tx.send(()).expect("release the stale read");

            let deadline = tilo_test_env::hang_deadline();
            let mut landed = Vec::new();
            while (landed.is_empty() || r.is_in_flight()) && Instant::now() < deadline {
                for reading in r.drain() {
                    if let QsReading::Field(FieldReading::Power(profile)) = reading {
                        landed.push(profile);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                landed,
                [Some(qs::PowerProfile::Performance)],
                "a read-back sampled before the second write would undo it"
            );
        }

        dispatches_while_in_flight_collapse_to_one_latest_rerun();
        tiers_are_tracked_independently();
        a_read_started_before_a_write_never_lands_the_written_field();
        a_field_read_started_before_a_second_write_never_lands();
    }

    /// Counting stand-ins for the OS reads. Only the one test below installs
    /// them, and its phases run one after another, so the counts are its own.
    mod counting {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub(super) const NAMES: [&str; 12] = [
            "brightness",
            "displays",
            "output_device",
            "volume",
            "bluetooth",
            "power",
            "power_profiles",
            "wired",
            "battery",
            "radios",
            "privacy",
            "vpns",
        ];
        static CALLS: [AtomicUsize; NAMES.len()] = [const { AtomicUsize::new(0) }; NAMES.len()];

        fn hit(i: usize) {
            CALLS[i].fetch_add(1, Ordering::SeqCst);
        }

        /// The reads made since the last call, by name, with their counts.
        pub(super) fn take() -> Vec<(&'static str, usize)> {
            NAMES
                .iter()
                .zip(&CALLS)
                .map(|(name, calls)| (*name, calls.swap(0, Ordering::SeqCst)))
                .filter(|(_, calls)| *calls > 0)
                .collect()
        }

        pub(super) const READS: ReadFns = ReadFns {
            brightness: || {
                hit(0);
                None
            },
            displays: || {
                hit(1);
                Vec::new()
            },
            output_device: || {
                hit(2);
                true
            },
            volume: || {
                hit(3);
                Some(qs::VolumeState {
                    level: 0.5,
                    muted: false,
                })
            },
            bluetooth: || {
                hit(4);
                Some(true)
            },
            power: || {
                hit(5);
                None
            },
            power_profiles: || {
                hit(6);
                None
            },
            wired: || {
                hit(7);
                None
            },
            battery: || {
                hit(8);
                BatterySnapshot::absent()
            },
            radios: || {
                hit(9);
                Some(qs::RadioState {
                    wifi_available: true,
                    wifi_enabled: true,
                    ..qs::RadioState::default()
                })
            },
            privacy: || {
                hit(10);
                qs::PrivacyStatus::default()
            },
            vpns: || {
                hit(11);
                None
            },
        };
    }

    fn land_one(r: &mut QsRefresh) -> QsReading {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(reading) = r.drain().into_iter().next() {
                assert!(!r.is_in_flight());
                return reading;
            }
            assert!(Instant::now() < deadline, "the reading never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn fast_reading_reads_radios_once_and_a_write_reads_back_only_its_field() {
        fn fast_reading_reads_radios_once() {
            let mut r = QsRefresh::with_reads(counting::READS);
            r.kick_fast();
            let QsReading::Fast(reading) = land_one(&mut r) else {
                panic!("a fast kick lands a fast reading");
            };
            assert!(reading.radios.is_some_and(|radios| radios.wifi_enabled));
            let calls = counting::take();
            assert!(
                calls.contains(&("radios", 1)),
                "Wi-Fi is derived from the one radio snapshot, not a second nmcli read: {calls:?}"
            );
            assert!(
                calls.iter().all(|(_, n)| *n == 1),
                "the open read takes every device reading exactly once: {calls:?}"
            );
            assert_eq!(calls.len(), counting::NAMES.len(), "{calls:?}");
            assert_eq!(r.take_kicks(), [Kick::Fast]);
        }

        fn a_write_reads_back_only_its_field() {
            let expected: [(QsField, &[&str]); 10] = [
                (QsField::Wifi, &["radios"]),
                (QsField::Airplane, &["radios"]),
                (QsField::Hotspot, &["radios"]),
                (QsField::Mobile, &["radios"]),
                (QsField::Bluetooth, &["bluetooth"]),
                (QsField::Wired, &["wired"]),
                (QsField::Power, &["power"]),
                (QsField::Vpn, &["vpns"]),
                (QsField::Brightness, &["brightness", "displays"]),
                (QsField::Volume, &["volume"]),
            ];
            let mut r = QsRefresh::with_reads(counting::READS);
            for (field, reads) in expected {
                r.kick_field(field);
                let QsReading::Field(reading) = land_one(&mut r) else {
                    panic!("a field kick lands a field reading");
                };
                assert_eq!(reading.reader(), field.reader());
                let calls = counting::take();
                let names: Vec<&str> = calls.iter().map(|(name, _)| *name).collect();
                assert_eq!(names, reads, "{field:?} read back {calls:?}");
                assert!(calls.iter().all(|(_, n)| *n == 1), "{calls:?}");
            }
            assert_eq!(
                r.take_kicks(),
                expected.map(|(field, _)| Kick::Field(field)),
                "no write asks for the whole panel again"
            );
        }

        counting::take();
        fast_reading_reads_radios_once();
        a_write_reads_back_only_its_field();
    }
}
