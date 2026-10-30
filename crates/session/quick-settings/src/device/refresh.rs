//! Off-thread reads and writes of the device.
//!
//! The panel's reads run 4.5s cold / 230ms warm inline, dominated by
//! listing Wi-Fi networks — a stall here reads as the machine being broken,
//! so they run on workers; the panel opens immediately with the last known
//! values and updates as readings land.
//!
//! Tiered so a slow list never holds up the panel's switches and sliders:
//! the Wi-Fi and Bluetooth lists are their own tiers. A worker owns no `Rc`
//! and sends plain data over a channel, then wakes the session, which
//! drains it on its own thread ([`QsRefresh::drain`]).
//!
//! Opening the panel reads everything once ([`QsRefresh::kick_fast`]). A
//! write the panel makes runs on the writer, in order, and reads back only
//! what it wrote ([`QsRefresh::kick_field`]): flipping Bluetooth asks
//! `BlueZ`, not every daemon on the machine.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::reads::{
    BluetoothReading, DeviceReads, Failure, FastReading, QsField, QsReading, ReadBack, Reader,
    WifiJoinResult, WriteDone,
};

/// What wakes the session's loop when a worker is done.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

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

type PendingWork = Box<dyn FnOnce() -> QsReading + Send + 'static>;
type WriteWork = Box<dyn FnOnce() -> QsReading + Send + 'static>;

/// Per-reader count of the panel's own writes.
type WriteCounts = [u64; Reader::ALL.len()];

/// Both wakes a finished worker calls: the toolkit's, for the window it
/// was started under, and the session's loop.
#[derive(Clone)]
struct Wakes {
    ui: ui::WakeHandle,
    host: Wake,
}

impl Wakes {
    fn wake(&self) {
        self.ui.wake();
        (self.host)();
    }
}

pub(crate) struct QsRefresh {
    tx: Sender<QsReading>,
    rx: Receiver<QsReading>,
    wakes: Wakes,
    reads: Arc<dyn DeviceReads>,
    /// Which tiers currently have a worker outstanding.
    in_flight: HashSet<Tier>,
    /// The latest follow-up requested for each busy tier. Repeated input while
    /// a daemon read is outstanding collapses to one rerun, but is never
    /// discarded: the active read may have sampled state before the input.
    rerun: HashMap<Tier, PendingWork>,
    /// Counts the panel's own writes, per reader. A read started before the
    /// latest write to a field may have sampled its old value.
    writes: WriteCounts,
    /// The write counts each tier's running read started under.
    started: HashMap<Tier, WriteCounts>,
    /// The writer, started with the first write: writes run one after
    /// another, in the order the panel made them.
    writer: Option<Sender<WriteWork>>,
}

impl QsRefresh {
    pub(crate) fn new(reads: Arc<dyn DeviceReads>, wake: Wake) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            wakes: Wakes {
                ui: ui::WakeHandle::new(),
                host: wake,
            },
            reads,
            in_flight: HashSet::new(),
            rerun: HashMap::new(),
            writes: [0; Reader::ALL.len()],
            started: HashMap::new(),
            writer: None,
        }
    }

    /// Whether a read is outstanding.
    #[cfg(test)]
    fn is_in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

    fn spawn<F>(&mut self, tier: Tier, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
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
        let wakes = self.wakes.clone();
        // Detached: bounded by the OS reads' own timeouts, and a send into
        // a dropped receiver is a no-op.
        let spawned = std::thread::Builder::new()
            .name("quick-settings-read".into())
            .spawn(move || {
                if tx.send(work()).is_ok() {
                    wakes.wake();
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "a Quick Settings read could not start");
            self.in_flight.remove(&tier);
            self.started.remove(&tier);
        }
    }

    /// Read everything the panel shows. Only the panel opening asks for
    /// this; a write reads back its own field through [`Self::kick_field`].
    pub(crate) fn kick_fast(&mut self) {
        let reads = Arc::clone(&self.reads);
        self.spawn(Tier::Fast, move || {
            QsReading::Fast(Box::new(FastReading::read(reads.as_ref())))
        });
    }

    /// Read back `field` after the panel changed it, since even a successful
    /// daemon request can be normalized by hardware or policy. A reading of
    /// that field already under way is superseded: it is dropped when it
    /// lands, and this read follows it.
    pub(crate) fn kick_field(&mut self, field: QsField) {
        let reader = field.reader();
        self.note_write(reader);
        let reads = Arc::clone(&self.reads);
        self.spawn(Tier::Field(reader), move || {
            QsReading::Field(reader.read(reads.as_ref()))
        });
    }

    fn note_write(&mut self, reader: Reader) {
        if let Some(count) = self.writes.get_mut(reader.index()) {
            *count = count.wrapping_add(1);
        }
    }

    pub(crate) fn kick_firmware(&mut self) {
        let reads = Arc::clone(&self.reads);
        self.spawn(Tier::Firmware, move || {
            QsReading::Firmware(reads.firmware_updates())
        });
    }

    /// Read the networks `NetworkManager` already knows. Never waits for a
    /// radio scan, which takes over ten seconds on some adapters: the tile's
    /// "connected to" must not depend on one.
    pub(crate) fn kick_wifi_networks(&mut self) {
        let reads = Arc::clone(&self.reads);
        self.spawn(Tier::Wifi, move || {
            QsReading::WifiNetworks(reads.wifi_networks())
        });
    }

    /// The panel looks again: scan, then read. A failed scan still reads
    /// the known list; the failure is logged.
    pub(crate) fn kick_wifi_rescan(&mut self) {
        let reads = Arc::clone(&self.reads);
        self.spawn(Tier::Wifi, move || {
            if let Err(error) = reads.scan_wifi() {
                tracing::warn!(target: "tilo::os_actions", op = "scan_wifi", %error, "Wi-Fi scan failed");
            }
            QsReading::WifiNetworks(reads.wifi_networks())
        });
    }

    pub(crate) fn kick_bt_devices(&mut self) {
        let reads = Arc::clone(&self.reads);
        self.spawn(Tier::Bt, move || {
            QsReading::BtDevices(BluetoothReading {
                paired: reads.bt_devices(),
                nearby: reads.nearby_bluetooth(),
                scanning: reads.scanning_bluetooth(),
            })
        });
    }

    /// Join without ever waiting on `NetworkManager` on the session's
    /// thread. The visible prompt or row is already marked busy before this
    /// returns.
    pub(crate) fn kick_wifi_join<F>(&mut self, ssid: String, password: Option<String>, join: F)
    where
        F: FnOnce(&str, Option<&str>) -> Result<(), String> + Send + 'static,
    {
        self.spawn(Tier::WifiJoin, move || {
            let result = join(&ssid, password.as_deref());
            QsReading::WifiJoin(WifiJoinResult {
                ssid,
                password,
                result,
            })
        });
    }

    /// Carry out `write` on the writer, after every write made before it.
    /// A read of what it writes that is under way now lands without it.
    pub(crate) fn write<F>(
        &mut self,
        operation: &'static str,
        failure: Failure,
        read_back: ReadBack,
        write: F,
    ) where
        F: FnOnce() -> Result<(), String> + Send + 'static,
    {
        if let ReadBack::Field(field) = read_back {
            self.note_write(field.reader());
        }
        let work: WriteWork = Box::new(move || {
            QsReading::Wrote(WriteDone {
                operation,
                failure,
                read_back,
                result: write(),
            })
        });
        let work = match self.writer.as_ref() {
            Some(writer) => match writer.send(work) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(work)) => work,
            },
            None => work,
        };
        let (writer, jobs) = channel::<WriteWork>();
        let tx = self.tx.clone();
        let wakes = self.wakes.clone();
        let spawned = std::thread::Builder::new()
            .name("quick-settings-write".into())
            .spawn(move || {
                for job in jobs {
                    if tx.send(job()).is_err() {
                        break;
                    }
                    wakes.wake();
                }
            });
        match spawned {
            Ok(_) => {
                let _ = writer.send(work);
                self.writer = Some(writer);
            }
            Err(error) => {
                tracing::warn!(%error, operation, "a Quick Settings change could not start");
            }
        }
    }

    /// Every reading arrived since the last call. Never blocks.
    pub(crate) fn drain(&mut self) -> Vec<QsReading> {
        let mut out = Vec::new();
        while let Ok(reading) = self.rx.try_recv() {
            let tier = match &reading {
                QsReading::Fast(_) => Tier::Fast,
                QsReading::Field(field) => Tier::Field(field.reader()),
                QsReading::WifiNetworks(_) => Tier::Wifi,
                QsReading::BtDevices(_) => Tier::Bt,
                QsReading::Firmware(_) => Tier::Firmware,
                QsReading::WifiJoin(_) => Tier::WifiJoin,
                QsReading::Wrote(_) => {
                    out.push(reading);
                    continue;
                }
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
    fn kick_fast_with<F>(&mut self, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
        self.spawn(Tier::Fast, work);
    }

    /// [`Self::kick_field`] with substitute work, so a test controls when
    /// the read lands.
    #[cfg(test)]
    fn kick_field_with<F>(&mut self, field: QsField, work: F)
    where
        F: FnOnce() -> super::reads::FieldReading + Send + 'static,
    {
        let reader = field.reader();
        self.note_write(reader);
        self.spawn(Tier::Field(reader), move || QsReading::Field(work()));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::DisplayBrightness;
    use crate::actions::QsActionAvailability;
    use crate::device::reads::FieldReading;
    use media::audio::default_node::VolumeState;
    use os_actions as qs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use tilo_power_mgmt::BatterySnapshot;

    /// A machine with nothing to read.
    pub(crate) struct NoReads;

    impl DeviceReads for NoReads {
        fn brightness(&self) -> Option<f32> {
            None
        }
        fn displays(&self) -> Vec<DisplayBrightness> {
            Vec::new()
        }
        fn output_device(&self) -> bool {
            false
        }
        fn volume(&self) -> Option<VolumeState> {
            None
        }
        fn bluetooth(&self) -> Option<bool> {
            None
        }
        fn power(&self) -> Option<qs::PowerProfile> {
            None
        }
        fn power_profiles(&self) -> Option<Vec<qs::PowerProfile>> {
            None
        }
        fn wired(&self) -> Option<qs::WiredInfo> {
            None
        }
        fn battery(&self) -> BatterySnapshot {
            BatterySnapshot::absent()
        }
        fn battery_time(&self) -> Option<tilo_power_mgmt::BatteryTime> {
            None
        }
        fn radios(&self) -> Option<qs::RadioState> {
            None
        }
        fn privacy(&self) -> qs::PrivacyStatus {
            qs::PrivacyStatus::default()
        }
        fn vpns(&self) -> Option<Vec<qs::VpnConnectionInfo>> {
            None
        }
        fn link(&self) -> Option<qs::LinkStatus> {
            None
        }
        fn wifi_networks(&self) -> Result<Vec<qs::WifiNetworkInfo>, String> {
            Ok(Vec::new())
        }
        fn scan_wifi(&self) -> Result<(), String> {
            Ok(())
        }
        fn bt_devices(&self) -> Option<Vec<qs::BtDeviceInfo>> {
            None
        }
        fn firmware_updates(&self) -> Option<usize> {
            None
        }
        fn availability(&self) -> QsActionAvailability {
            QsActionAvailability::default()
        }
    }

    fn refresh(reads: impl DeviceReads + 'static) -> QsRefresh {
        QsRefresh::new(Arc::new(reads), Arc::new(|| {}))
    }

    fn empty_fast() -> QsReading {
        QsReading::Fast(Box::new(FastReading::empty()))
    }

    /// A read that blocks until the test releases it, and records whether
    /// it had finished — so "did the caller wait for it" is a fact, not a
    /// wall-clock guess.
    fn gated_read() -> (
        Sender<()>,
        Arc<std::sync::atomic::AtomicBool>,
        impl FnOnce() + Send + 'static,
    ) {
        let (release_tx, release_rx) = channel::<()>();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_finished = Arc::clone(&finished);
        let wait = move || {
            let _ = release_rx.recv_timeout(tilo_test_env::hang_bound());
            worker_finished.store(true, Ordering::SeqCst);
        };
        (release_tx, finished, wait)
    }

    fn land(r: &mut QsRefresh, until: impl Fn(&[QsReading]) -> bool) -> Vec<QsReading> {
        let deadline = tilo_test_env::hang_deadline();
        let mut got = Vec::new();
        while !until(&got) || r.is_in_flight() {
            got.extend(r.drain());
            assert!(Instant::now() < deadline, "the reading never arrived");
            std::thread::sleep(Duration::from_millis(5));
        }
        got
    }

    #[test]
    fn dispatched_reads_and_joins_never_block_the_caller_and_deliver_results() {
        let mut r = refresh(NoReads);
        let (release, finished, wait) = gated_read();
        r.kick_fast_with(move || {
            wait();
            empty_fast()
        });
        assert!(
            !finished.load(Ordering::SeqCst),
            "kick returned only after the read finished: it was performed inline"
        );
        assert!(r.is_in_flight(), "the tier should be marked outstanding");
        assert!(
            r.drain().is_empty(),
            "no reading can have landed yet — draining must not block waiting for one"
        );
        release.send(()).expect("release the read");
        land(&mut r, |got| !got.is_empty());

        let (release, finished, wait) = gated_read();
        r.kick_wifi_join("Cafe".into(), Some("secret".into()), move |_, _| {
            wait();
            Ok(())
        });
        assert!(!finished.load(Ordering::SeqCst), "the join ran inline");
        assert!(r.drain().is_empty());
        release.send(()).expect("release the join");
        let got = land(&mut r, |got| !got.is_empty());
        let [QsReading::WifiJoin(join)] = got.as_slice() else {
            panic!("the join lands: {got:?}");
        };
        assert_eq!(join.ssid, "Cafe");
        assert_eq!(join.password.as_deref(), Some("secret"));
        assert!(join.result.is_ok());

        let (wake_tx, wake_rx) = channel();
        let mut woken = QsRefresh::new(
            Arc::new(NoReads),
            Arc::new(move || {
                let _ = wake_tx.send(());
            }),
        );
        woken.kick_fast_with(empty_fast);
        tilo_test_env::recv("a finished read wakes the session", &wake_rx);
        assert_eq!(woken.drain().len(), 1);
    }

    #[test]
    fn writes_run_in_order_off_the_callers_thread_and_land_with_their_result() {
        let mut r = refresh(NoReads);
        let (release, finished, wait) = gated_read();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let first = Arc::clone(&order);
        r.write(
            "first",
            Failure::State("first".into()),
            ReadBack::Field(QsField::Bluetooth),
            move || {
                wait();
                first.lock().unwrap().push(1);
                Ok(())
            },
        );
        let second = Arc::clone(&order);
        r.write(
            "second",
            Failure::State("second".into()),
            ReadBack::Accessories,
            move || {
                second.lock().unwrap().push(2);
                Err("refused".into())
            },
        );
        assert!(!finished.load(Ordering::SeqCst), "the write ran inline");
        release.send(()).expect("release the first write");
        let got = land(&mut r, |got| got.len() == 2);
        assert_eq!(*order.lock().unwrap(), [1, 2], "in the order made");
        let results: Vec<_> = got
            .iter()
            .map(|reading| match reading {
                QsReading::Wrote(done) => (done.operation, done.result.clone()),
                other => panic!("a write lands as written: {other:?}"),
            })
            .collect();
        assert_eq!(
            results,
            [("first", Ok(())), ("second", Err("refused".to_owned()))]
        );
    }

    #[test]
    fn refresh_tiers_collapse_in_flight_dispatches_and_track_independently() {
        // Dispatches while in flight collapse to one, the latest.
        let mut r = refresh(NoReads);
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let runs = Arc::new(AtomicUsize::new(0));
        let first_runs = Arc::clone(&runs);
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
        let selected = Arc::new(AtomicUsize::new(0));
        for marker in 1..=3 {
            let rerun_runs = Arc::clone(&runs);
            let selected = Arc::clone(&selected);
            r.kick_fast_with(move || {
                rerun_runs.fetch_add(1, Ordering::SeqCst);
                selected.store(marker, Ordering::SeqCst);
                empty_fast()
            });
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1, "no overlap");
        release_tx.send(()).expect("release first worker");
        let got = land(&mut r, |got| got.len() >= 2);
        assert_eq!(got.len(), 2, "one active read plus one coalesced rerun");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        assert_eq!(selected.load(Ordering::SeqCst), 3, "the latest rerun");

        // A read started before a write never lands the written field.
        let mut r = refresh(NoReads);
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
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
        r.kick_field_with(QsField::Bluetooth, || FieldReading::Bluetooth(Some(false)));
        release_tx.send(()).expect("release the stale read");
        let got = land(&mut r, |got| got.len() >= 2);
        let mut fast = Vec::new();
        let mut fields = Vec::new();
        for reading in got {
            match reading {
                QsReading::Fast(reading) => fast.push((reading.bluetooth, reading.power)),
                QsReading::Field(FieldReading::Bluetooth(on)) => fields.push(on),
                other => panic!("unexpected reading: {other:?}"),
            }
        }
        assert_eq!(fast, [(None, Some(qs::PowerProfile::Balanced))]);
        assert_eq!(fields, [Some(false)]);

        // A field read started before a second write never lands.
        let mut r = refresh(NoReads);
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
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
        let landed: Vec<_> = land(&mut r, |got| !got.is_empty())
            .into_iter()
            .map(|reading| match reading {
                QsReading::Field(FieldReading::Power(profile)) => profile,
                other => panic!("unexpected reading: {other:?}"),
            })
            .collect();
        assert_eq!(landed, [Some(qs::PowerProfile::Performance)]);
    }

    /// Counts each read it is asked for.
    #[derive(Default)]
    struct Counting {
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    impl Counting {
        fn hit(&self, name: &'static str) {
            self.calls.lock().unwrap().push(name);
        }
    }

    impl DeviceReads for Arc<Counting> {
        fn brightness(&self) -> Option<f32> {
            self.hit("brightness");
            None
        }
        fn displays(&self) -> Vec<DisplayBrightness> {
            self.hit("displays");
            Vec::new()
        }
        fn output_device(&self) -> bool {
            self.hit("output_device");
            true
        }
        fn volume(&self) -> Option<VolumeState> {
            self.hit("volume");
            Some(VolumeState {
                level: 0.5,
                muted: false,
            })
        }
        fn bluetooth(&self) -> Option<bool> {
            self.hit("bluetooth");
            Some(true)
        }
        fn power(&self) -> Option<qs::PowerProfile> {
            self.hit("power");
            None
        }
        fn power_profiles(&self) -> Option<Vec<qs::PowerProfile>> {
            self.hit("power_profiles");
            None
        }
        fn wired(&self) -> Option<qs::WiredInfo> {
            self.hit("wired");
            None
        }
        fn battery(&self) -> BatterySnapshot {
            self.hit("battery");
            BatterySnapshot::absent()
        }
        fn battery_time(&self) -> Option<tilo_power_mgmt::BatteryTime> {
            self.hit("battery_time");
            None
        }
        fn radios(&self) -> Option<qs::RadioState> {
            self.hit("radios");
            Some(qs::RadioState {
                wifi_available: true,
                wifi_enabled: true,
                ..qs::RadioState::default()
            })
        }
        fn privacy(&self) -> qs::PrivacyStatus {
            self.hit("privacy");
            qs::PrivacyStatus::default()
        }
        fn vpns(&self) -> Option<Vec<qs::VpnConnectionInfo>> {
            self.hit("vpns");
            None
        }
        fn link(&self) -> Option<qs::LinkStatus> {
            self.hit("link");
            None
        }
        fn wifi_networks(&self) -> Result<Vec<qs::WifiNetworkInfo>, String> {
            self.hit("wifi_networks");
            Ok(Vec::new())
        }
        fn scan_wifi(&self) -> Result<(), String> {
            self.hit("scan_wifi");
            Ok(())
        }
        fn bt_devices(&self) -> Option<Vec<qs::BtDeviceInfo>> {
            self.hit("bt_devices");
            None
        }
        fn firmware_updates(&self) -> Option<usize> {
            self.hit("firmware_updates");
            None
        }
        fn availability(&self) -> QsActionAvailability {
            self.hit("availability");
            QsActionAvailability::default()
        }
    }

    #[test]
    fn opening_reads_each_device_once_and_a_write_reads_back_only_its_field() {
        let counting = Arc::new(Counting::default());
        let take = || std::mem::take(&mut *counting.calls.lock().unwrap());
        let mut r = refresh(Arc::clone(&counting));
        r.kick_fast();
        let got = land(&mut r, |got| !got.is_empty());
        let [QsReading::Fast(reading)] = got.as_slice() else {
            panic!("a fast kick lands a fast reading");
        };
        assert!(
            reading
                .radios
                .as_ref()
                .is_some_and(|radios| radios.wifi_enabled)
        );
        let mut calls = take();
        calls.sort_unstable();
        let mut once = calls.clone();
        once.dedup();
        assert_eq!(calls, once, "every reading exactly once: {calls:?}");
        assert!(
            calls.contains(&"radios") && !calls.contains(&"wifi_networks"),
            "Wi-Fi is taken from the radio snapshot, not a network list: {calls:?}"
        );

        let expected: [(QsField, &[&str]); 9] = [
            (QsField::Wifi, &["radios"]),
            (QsField::Airplane, &["radios"]),
            (QsField::Mobile, &["radios"]),
            (QsField::Bluetooth, &["bluetooth"]),
            (QsField::Wired, &["wired"]),
            (QsField::Power, &["power"]),
            (QsField::Vpn, &["vpns"]),
            (QsField::Brightness, &["brightness", "displays"]),
            (QsField::Volume, &["volume"]),
        ];
        for (field, reads) in expected {
            r.kick_field(field);
            let got = land(&mut r, |got| !got.is_empty());
            let [QsReading::Field(reading)] = got.as_slice() else {
                panic!("a field kick lands a field reading");
            };
            assert_eq!(reading.reader(), field.reader());
            assert_eq!(take(), reads, "{field:?}");
        }
    }
}
