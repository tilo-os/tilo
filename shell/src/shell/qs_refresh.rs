//! Off-thread quick-settings reads.
//!
//! The panel's seven reads run 4.5s cold / 230ms warm inline, dominated
//! by `nmcli device wifi list` — a stall here reads as the machine
//! being broken, so they run on a worker; the panel opens immediately
//! with the last known values and updates as readings land.
//!
//! Tiered by what's visible: network/device lists only show after a
//! drill-in, so that scan is dispatched there rather than paid for on
//! open. A worker owns no `Rc` and sends plain data over a channel,
//! keeping it safe alongside the shell's single-threaded `RefCell` tree.

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::shell::qs_actions as qs;

#[derive(Debug)]
pub enum QsReading {
    /// Each field independently optional: a missing daemon leaves that
    /// value alone rather than blanking the panel.
    Fast {
        brightness: Option<f32>,
        volume: Option<f32>,
        wifi: Option<bool>,
        bluetooth: Option<bool>,
        power: Option<qs::PowerProfile>,
    },
    WifiNetworks(Option<Vec<qs::WifiNetworkInfo>>),
    BtDevices(Option<Vec<qs::BtDeviceInfo>>),
}

/// Tracked so a second open while a read is running doesn't pile up
/// another worker behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Tier {
    Fast,
    Wifi,
    Bt,
}

pub struct QsRefresh {
    tx: Sender<QsReading>,
    rx: Receiver<QsReading>,
    /// Which tiers currently have a worker outstanding.
    in_flight: HashSet<Tier>,
    /// Turned off when the host's action dispatcher has been replaced,
    /// so a seeded Wi-Fi list can't get overwritten by a real scan.
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
            in_flight: HashSet::new(),
            enabled: true,
        }
    }

    /// Feeds `needs_animation_frame` so a reading landing after the
    /// open animation settles still reaches the screen.
    pub fn is_in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

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
        if !self.in_flight.insert(tier) {
            return;
        }
        let tx = self.tx.clone();
        // Detached: bounded by `os_actions::DEFAULT_TIMEOUT`, and a
        // send into a dropped receiver is a no-op.
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    pub fn kick_fast(&mut self) {
        self.spawn(Tier::Fast, || QsReading::Fast {
            brightness: qs::log_qs_get("get_brightness", qs::get_brightness()),
            volume: qs::log_qs_get("get_volume", qs::get_volume()),
            wifi: qs::log_qs_get("get_wifi", qs::get_wifi()),
            bluetooth: qs::log_qs_get("get_bluetooth", qs::get_bluetooth()),
            power: qs::log_qs_get("get_power_profile", qs::get_power_profile()),
        });
    }

    pub fn kick_wifi_networks(&mut self) {
        self.spawn(Tier::Wifi, || {
            QsReading::WifiNetworks(qs::log_qs_get(
                "list_wifi_networks",
                qs::list_wifi_networks(),
            ))
        });
    }

    pub fn kick_bt_devices(&mut self) {
        self.spawn(Tier::Bt, || {
            QsReading::BtDevices(qs::log_qs_get("list_bt_devices", qs::list_bt_devices()))
        });
    }

    /// Every reading arrived since the last call. Never blocks.
    pub fn drain(&mut self) -> Vec<QsReading> {
        let mut out = Vec::new();
        while let Ok(reading) = self.rx.try_recv() {
            let tier = match &reading {
                QsReading::Fast { .. } => Tier::Fast,
                QsReading::WifiNetworks(_) => Tier::Wifi,
                QsReading::BtDevices(_) => Tier::Bt,
            };
            self.in_flight.remove(&tier);
            out.push(reading);
        }
        out
    }

    /// Asserts the asynchrony itself without depending on which OS
    /// tools exist.
    #[cfg(test)]
    fn kick_fast_with<F>(&mut self, work: F)
    where
        F: FnOnce() -> QsReading + Send + 'static,
    {
        self.spawn(Tier::Fast, work);
    }

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
            QsReading::Fast {
                brightness, wifi, ..
            } => {
                assert_eq!(*brightness, Some(0.42));
                assert_eq!(*wifi, Some(true));
            }
            other => panic!("wrong tier: {other:?}"),
        }
        assert!(
            !r.is_in_flight(),
            "the tier should be clear once its reading lands"
        );
    }

    #[test]
    fn a_second_dispatch_while_in_flight_is_coalesced() {
        let mut r = QsRefresh::new();
        r.kick_fast_with(move || {
            std::thread::sleep(Duration::from_millis(250));
            empty_fast()
        });
        for _ in 0..3 {
            r.kick_fast_with(empty_fast);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut total = 0;
        while total == 0 && Instant::now() < deadline {
            total += r.drain().len();
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(120));
        total += r.drain().len();
        assert_eq!(
            total, 1,
            "expected one reading for one outstanding tier, got {total}"
        );
    }

    #[test]
    fn tiers_are_tracked_independently() {
        let mut r = QsRefresh::new();
        assert!(!r.is_in_flight());
        r.kick_wifi_networks();
        assert!(r.is_in_flight());
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
        assert!(
            saw_fast,
            "the fast tier never reported while Wi-Fi was outstanding"
        );
    }
}
