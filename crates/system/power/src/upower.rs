//! `UPower` integration (`org.freedesktop.UPower`) over `zbus`.
//!
//! Reads the machine's composite battery (the *display device*) and lid
//! state from the running `upowerd`. As with [`super::logind`], this is pure
//! I/O — the threshold decisions live in [`super::battery`], which this
//! module feeds with [`BatterySnapshot`]s.

use zbus::Connection;

use crate::battery::{BatterySnapshot, BatteryState};
use crate::history::PowerSourceKind;

const DEST: &str = "org.freedesktop.UPower";
const MGR_PATH: &str = "/org/freedesktop/UPower";
const MGR_IFACE: &str = "org.freedesktop.UPower";
/// The synthetic device that aggregates every battery into one — the right
/// thing to warn on for a laptop with one or several cells.
const DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";

/// `org.freedesktop.UPower.Device.Type` code for a battery.
const DEVICE_TYPE_BATTERY: u32 = 2;
const DEVICE_TYPE_UPS: u32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerHistorySample {
    pub kind: PowerSourceKind,
    pub capacity: u32,
    pub health: Option<u32>,
    pub on_ac: bool,
}

/// A connected handle to `UPower`.
#[derive(Clone)]
pub struct UPowerMonitor {
    connection: zbus::Connection,
    manager: zbus::Proxy<'static>,
    display: zbus::Proxy<'static>,
}

impl UPowerMonitor {
    /// Connect to the system bus and build the manager + display-device
    /// proxies.
    ///
    /// # Errors
    ///
    /// Returns an error if the system bus is unreachable or the `UPower` proxies cannot be
    /// built.
    pub async fn connect() -> zbus::Result<Self> {
        let conn = Connection::system().await?;
        Self::with_connection(&conn).await
    }

    /// Build proxies on an existing connection (shared with logind).
    ///
    /// # Errors
    ///
    /// Returns an error if the `UPower` manager or display-device proxy cannot be built.
    pub async fn with_connection(conn: &Connection) -> zbus::Result<Self> {
        let manager = zbus::Proxy::new(conn, DEST, MGR_PATH, MGR_IFACE).await?;
        let display = zbus::Proxy::new(conn, DEST, DISPLAY_DEVICE_PATH, DEVICE_IFACE).await?;
        Ok(Self {
            connection: conn.clone(),
            manager,
            display,
        })
    }

    /// Whether the lid is currently closed. Machines without a lid report
    /// `false`; pair with [`Self::lid_is_present`] to distinguish.
    ///
    /// # Errors
    ///
    /// Returns an error if the `LidIsClosed` property cannot be read.
    pub async fn lid_is_closed(&self) -> zbus::Result<bool> {
        self.manager.get_property("LidIsClosed").await
    }

    /// Whether the machine has a lid at all.
    ///
    /// # Errors
    ///
    /// Returns an error if the `LidIsPresent` property cannot be read.
    pub async fn lid_is_present(&self) -> zbus::Result<bool> {
        self.manager.get_property("LidIsPresent").await
    }

    /// Whether the machine is currently running from battery.
    ///
    /// # Errors
    ///
    /// Returns an error if the `OnBattery` property cannot be read.
    pub async fn on_battery(&self) -> zbus::Result<bool> {
        self.manager.get_property("OnBattery").await
    }

    /// Read the composite battery as a [`BatterySnapshot`].
    ///
    /// A machine with no battery reports the display device as
    /// `Type != Battery` (or `IsPresent == false`); we surface that as
    /// [`BatterySnapshot::absent`] so the threshold monitor stays quiet.
    ///
    /// # Errors
    ///
    /// Property read failures fall back to defaults (`0`/`false`), so a flaky
    /// D-Bus read yields a stale reading rather than an error.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=100.0 immediately before the cast, so it always fits a u8"
    )]
    pub async fn battery_snapshot(&self) -> zbus::Result<BatterySnapshot> {
        let dev_type: u32 = self.display.get_property("Type").await.unwrap_or(0);
        let present: bool = self
            .display
            .get_property("IsPresent")
            .await
            .unwrap_or(false);
        if !matches!(dev_type, DEVICE_TYPE_BATTERY | DEVICE_TYPE_UPS) || !present {
            return Ok(BatterySnapshot::absent());
        }
        let percentage: f64 = self.display.get_property("Percentage").await.unwrap_or(0.0);
        let state_code: u32 = self.display.get_property("State").await.unwrap_or(0);
        Ok(BatterySnapshot {
            present: true,
            percent: percentage.round().clamp(0.0, 100.0) as u8,
            state: BatteryState::from_upower(state_code),
        })
    }

    /// Read the aggregate power source using `UPower`'s display-device
    /// contract. No model, serial, process, or application metadata crosses
    /// into the durable history record.
    ///
    /// # Errors
    ///
    /// Property read failures fall back to defaults rather than propagating a
    /// D-Bus error.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=100.0 immediately before the cast, so it always fits a u32"
    )]
    pub async fn history_sample(&self) -> zbus::Result<Option<PowerHistorySample>> {
        let dev_type: u32 = self.display.get_property("Type").await.unwrap_or(0);
        let kind = match dev_type {
            DEVICE_TYPE_BATTERY => PowerSourceKind::Battery,
            DEVICE_TYPE_UPS => PowerSourceKind::Ups,
            _ => return Ok(None),
        };
        let present: bool = self
            .display
            .get_property("IsPresent")
            .await
            .unwrap_or(false);
        if !present {
            return Ok(None);
        }
        let percentage: f64 = self.display.get_property("Percentage").await.unwrap_or(0.0);
        let health: f64 = self.display.get_property("Capacity").await.unwrap_or(0.0);
        Ok(Some(PowerHistorySample {
            kind,
            capacity: percentage.round().clamp(0.0, 100.0) as u32,
            health: (health > 0.0).then(|| health.round().clamp(0.0, 100.0) as u32),
            on_ac: !self.on_battery().await.unwrap_or(false),
        }))
    }

    /// Return one anonymous aggregate for each power-source class. `UPower`'s
    /// display device is the canonical aggregate battery; UPS devices are
    /// averaged without persisting their object paths or vendor metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if enumerating devices or building a per-device proxy fails; a
    /// per-property read on a successfully-proxied device falls back to a default instead.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "capacity/health are clamped to 0.0..=100.0 before rounding, and the device \
                  count averaged over is at most a handful of UPS units, never near f64's \
                  precision limit"
    )]
    pub async fn history_samples(&self) -> zbus::Result<Vec<PowerHistorySample>> {
        let mut samples = Vec::new();
        if let Some(display) = self.history_sample().await? {
            samples.push(display);
        }
        if samples
            .iter()
            .any(|sample| sample.kind == PowerSourceKind::Ups)
        {
            return Ok(samples);
        }

        let paths: Vec<zbus::zvariant::OwnedObjectPath> =
            self.manager.call("EnumerateDevices", &()).await?;
        let mut percentages = Vec::new();
        let mut health_values = Vec::new();
        let mut draining = false;
        for path in paths {
            let device = zbus::Proxy::new(&self.connection, DEST, path, DEVICE_IFACE).await?;
            let kind: u32 = device.get_property("Type").await.unwrap_or(0);
            let present: bool = device.get_property("IsPresent").await.unwrap_or(false);
            if kind != DEVICE_TYPE_UPS || !present {
                continue;
            }
            let percentage: f64 = device.get_property("Percentage").await.unwrap_or(0.0);
            percentages.push(percentage.clamp(0.0, 100.0));
            let health: f64 = device.get_property("Capacity").await.unwrap_or(0.0);
            if health > 0.0 {
                health_values.push(health.clamp(0.0, 100.0));
            }
            let state: u32 = device.get_property("State").await.unwrap_or(0);
            draining |= BatteryState::from_upower(state).is_discharging();
        }
        if !percentages.is_empty() {
            let mean =
                |values: &[f64]| (values.iter().sum::<f64>() / values.len() as f64).round() as u32;
            samples.push(PowerHistorySample {
                kind: PowerSourceKind::Ups,
                capacity: mean(&percentages),
                health: (!health_values.is_empty()).then(|| mean(&health_values)),
                on_ac: !draining,
            });
        }
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn monitor_or_skip(ctx: &str) -> Option<UPowerMonitor> {
        match UPowerMonitor::connect().await {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("[skip] {ctx}: UPower unreachable on system bus: {e}");
                None
            }
        }
    }

    #[tokio::test]
    async fn upower_snapshots_lid_and_history_are_coherent() {
        // Phase `reads_a_coherent_battery_snapshot`.
        async fn reads_a_coherent_battery_snapshot() {
            let Some(m) = monitor_or_skip("snapshot").await else {
                return;
            };
            let snap = m.battery_snapshot().await.expect("battery_snapshot");
            // Percentage is always in range; a present battery has a real state.
            assert!(snap.percent <= 100, "percent in range: {snap:?}");
            if snap.present {
                assert_ne!(
                    snap.state,
                    BatteryState::Unknown,
                    "a present battery reports a concrete UPower state"
                );
            }
        }
        // Phase `lid_query_is_consistent`.
        async fn lid_query_is_consistent() {
            let Some(m) = monitor_or_skip("lid").await else {
                return;
            };
            let present = m.lid_is_present().await.expect("LidIsPresent");
            let closed = m.lid_is_closed().await.expect("LidIsClosed");
            // A machine with no lid can never report the lid closed.
            if !present {
                assert!(!closed, "no lid ⇒ not closed");
            }
            // OnBattery is a plain bool either way.
            let _ = m.on_battery().await.expect("OnBattery");
        }
        // Phase `history_samples_are_anonymous_and_in_range`.
        async fn history_samples_are_anonymous_and_in_range() {
            let Some(monitor) = monitor_or_skip("history samples").await else {
                return;
            };
            let samples = monitor.history_samples().await.expect("history samples");
            for sample in samples {
                assert!(sample.capacity <= 100);
                assert!(sample.health.is_none_or(|health| health <= 100));
            }
        }
        // Phase `reads_a_coherent_battery_snapshot` (defined above).
        reads_a_coherent_battery_snapshot().await;
        // Phase `lid_query_is_consistent` (defined above).
        lid_query_is_consistent().await;
        // Phase `history_samples_are_anonymous_and_in_range` (defined above).
        history_samples_are_anonymous_and_in_range().await;
    }
}
