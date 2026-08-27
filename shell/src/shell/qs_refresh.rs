//! Off-thread quick-settings reads.
//!
//! The panel's seven reads run inline and in series cost 4.5s cold / 230ms
//! warm, dominated by `nmcli device wifi list`. They happen on a worker
//! instead: chrome is the OS's own UI, so a stall in it reads as the
//! machine being broken. The panel opens immediately with the last known
//! values and updates as readings land.
//!
//! Reads are tiered by what the surface shows — the network and device
//! lists are only visible after a drill-in to Wi-Fi or Bluetooth, so that
//! scan is dispatched by the drill-in rather than paid for on open.
//!
//! Nothing here touches chrome state: a worker owns no `Rc` and sends plain
//! data back over a channel, which is what keeps it safe alongside the
//! shell's single-threaded `RefCell` tree.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use crate::shell::qs_actions as qs;

/// One batch of readings from a worker.
#[derive(Debug)]
pub enum QsReading {
    /// Everything the root panel displays. Each field is independently
    /// optional: a missing daemon leaves that one value alone rather than
    /// blanking the panel.
    Fast {
        brightness: Option<f32>,
        volume: Option<f32>,
        wifi: Option<bool>,
        bluetooth: Option<bool>,
        power: Option<qs::PowerProfile>,
    },
    /// Visible Wi-Fi networks — dispatched by the Wi-Fi drill-in.
    WifiNetworks(Option<Vec<qs::WifiNetworkInfo>>),
    /// Paired Bluetooth devices — dispatched by the Bluetooth drill-in.
    BtDevices(Option<Vec<qs::BtDeviceInfo>>),
}

/// Which tier a dispatch belongs to. Tracked so a second open while a
/// read is still running does not pile up another worker behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Fast,
    Wifi,
    Bt,
}

/// Owns the channel back from the workers and remembers which tiers are
/// still outstanding.
pub struct QsRefresh {
    tx: Sender<QsReading>,
    rx: Receiver<QsReading>,
    fast_in_flight: bool,
    wifi_in_flight: bool,
    bt_in_flight: bool,
    /// Whether dispatches actually reach the OS. Turned off when the host's
    /// action dispatcher has been replaced (see
    /// `ShellOverlayHost::with_qs_actions`): a caller that substituted where
    /// quick settings *writes* must not still be reading from this machine,
    /// or a seeded Wi-Fi list gets overwritten by a real `nmcli` scan.
    enabled: bool,
}

impl Default for QsRefresh {
    fn default() -> Self {
        Self::new()
    }
}

impl QsRefresh {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            fast_in_flight: false,
            wifi_in_flight: false,
            bt_in_flight: false,
            enabled: true,
        }
    }

    /// Whether any tier is still outstanding. Feeds `needs_animation_frame`
    /// so a reading landing after the open animation settles still reaches
    /// the screen, and the loop sleeps again once the last tier reports.
    pub fn is_in_flight(&self) -> bool {
        self.fast_in_flight || self.wifi_in_flight || self.bt_in_flight
    }

    /// Stop this refresher from reaching the OS. See [`Self::enabled`].
    pub fn disable_os_reads(&mut self) {
        self.enabled = false;
    }

    fn spawn<F>(&mut self, tier: Tier, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
        if !self.enabled {
            return;
        }
        let busy = match tier {
            Tier::Fast => &mut self.fast_in_flight,
            Tier::Wifi => &mut self.wifi_in_flight,
            Tier::Bt => &mut self.bt_in_flight,
        };
        // Already reading this tier: coalesce. Re-opening the panel while
        // a scan is running should not queue a second scan behind it.
        if *busy {
            return;
        }
        *busy = true;
        let tx = self.tx.clone();
        // A detached thread is the right shape here: the work is bounded
        // by `os_actions::DEFAULT_TIMEOUT`, it happens at most once per
        // tier at a time, and a send into a dropped receiver is a
        // no-op — so a panel closed mid-read costs nothing.
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    /// Dispatch the readings the root panel displays.
    pub fn kick_fast(&mut self) {
        self.spawn(Tier::Fast, || QsReading::Fast {
            brightness: qs::log_qs_get("get_brightness", qs::get_brightness()),
            volume: qs::log_qs_get("get_volume", qs::get_volume()),
            wifi: qs::log_qs_get("get_wifi", qs::get_wifi()),
            bluetooth: qs::log_qs_get("get_bluetooth", qs::get_bluetooth()),
            power: qs::log_qs_get("get_power_profile", qs::get_power_profile()),
        });
    }

    /// Dispatch the Wi-Fi scan. Called by the drill-in that shows it.
    pub fn kick_wifi_networks(&mut self) {
        self.spawn(Tier::Wifi, || {
            QsReading::WifiNetworks(qs::log_qs_get(
                "list_wifi_networks",
                qs::list_wifi_networks(),
            ))
        });
    }

    /// Dispatch the paired-device enumeration. Called by the drill-in
    /// that shows it.
    pub fn kick_bt_devices(&mut self) {
        self.spawn(Tier::Bt, || {
            QsReading::BtDevices(qs::log_qs_get("list_bt_devices", qs::list_bt_devices()))
        });
    }

    /// Take every reading that has arrived since the last call. Never
    /// blocks.
    pub fn drain(&mut self) -> Vec<QsReading> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(reading) => {
                    match &reading {
                        QsReading::Fast { .. } => self.fast_in_flight = false,
                        QsReading::WifiNetworks(_) => self.wifi_in_flight = false,
                        QsReading::BtDevices(_) => self.bt_in_flight = false,
                    }
                    out.push(reading);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    /// Test seam: dispatch arbitrary work on the `Fast` tier so the
    /// asynchrony itself can be asserted without depending on which OS
    /// tools happen to exist on the machine running the suite.
    #[cfg(test)]
    fn kick_fast_with<F>(&mut self, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
        self.spawn(Tier::Fast, work);
    }

    /// Whether dispatches reach the OS.
    pub fn os_reads_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn empty_fast() -> QsReading {
        QsReading::Fast {
            brightness: None,
            volume: None,
            wifi: None,
            bluetooth: None,
            power: None,
        }
    }

    /// The dispatch returns immediately even when the underlying read is
    /// slow. This is the whole point of the module: the frame that opens
    /// the panel must not wait on the OS.
    #[test]
    fn dispatching_a_slow_read_does_not_block_the_caller() {
        let mut r = QsRefresh::new();
        let read_cost = Duration::from_millis(400);
        let started = Instant::now();
        r.kick_fast_with(move || {
            std::thread::sleep(read_cost);
            empty_fast()
        });
        let dispatch = started.elapsed();
        assert!(
            dispatch < read_cost / 4,
            "kick returned after {dispatch:?}; a {read_cost:?} read was performed inline"
        );
        assert!(r.is_in_flight(), "the tier should be marked outstanding");
        assert!(
            r.drain().is_empty(),
            "no reading can have landed yet — draining must not block waiting for one"
        );
    }

    /// The reading does arrive, and clears its tier when it does.
    #[test]
    fn a_dispatched_reading_lands_and_clears_its_tier() {
        let mut r = QsRefresh::new();
        r.kick_fast_with(|| QsReading::Fast {
            brightness: Some(0.42),
            volume: None,
            wifi: Some(true),
            bluetooth: None,
            power: None,
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.is_empty() && Instant::now() < deadline {
            got = r.drain();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(got.len(), 1, "the reading never arrived");
        match &got[0] {
            QsReading::Fast { brightness, wifi, .. } => {
                assert_eq!(*brightness, Some(0.42));
                assert_eq!(*wifi, Some(true));
            }
            other => panic!("wrong tier: {other:?}"),
        }
        assert!(!r.is_in_flight(), "the tier should be clear once its reading lands");
    }

    /// Re-opening the panel while a read is outstanding coalesces onto
    /// the running one instead of queueing another.
    #[test]
    fn a_second_dispatch_while_in_flight_is_coalesced() {
        let mut r = QsRefresh::new();
        r.kick_fast_with(move || {
            std::thread::sleep(Duration::from_millis(250));
            empty_fast()
        });
        // Three more opens while the first is still running.
        for _ in 0..3 {
            r.kick_fast_with(empty_fast);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut total = 0;
        while total == 0 && Instant::now() < deadline {
            total += r.drain().len();
            std::thread::sleep(Duration::from_millis(5));
        }
        // Give any wrongly-queued duplicates a chance to show up.
        std::thread::sleep(Duration::from_millis(120));
        total += r.drain().len();
        assert_eq!(total, 1, "expected one reading for one outstanding tier, got {total}");
    }

    /// The tiers are independent: a Wi-Fi scan in flight must not block
    /// the root panel's own readings from being dispatched.
    #[test]
    fn tiers_are_tracked_independently() {
        let mut r = QsRefresh::new();
        assert!(!r.is_in_flight());
        r.kick_wifi_networks();
        assert!(r.is_in_flight());
        // Fast is still free to dispatch even though Wi-Fi is busy.
        r.kick_fast_with(empty_fast);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut saw_fast = false;
        while !saw_fast && Instant::now() < deadline {
            for reading in r.drain() {
                if matches!(reading, QsReading::Fast { .. }) {
                    saw_fast = true;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_fast, "the fast tier never reported while Wi-Fi was outstanding");
    }
}
