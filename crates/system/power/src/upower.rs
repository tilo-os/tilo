//! The real `UPower` glue (`org.freedesktop.UPower`) over `zbus`.
//!
//! Reads the machine's composite battery (the *display device*) and lid
//! state from the running `upowerd`. As with [`super::logind`], this is pure
//! I/O — the threshold decisions live in [`super::battery`], which this
//! module feeds with real [`BatterySnapshot`]s.

use zbus::Connection;

use crate::battery::{BatterySnapshot, BatteryState};

const DEST: &str = "org.freedesktop.UPower";
const MGR_PATH: &str = "/org/freedesktop/UPower";
const MGR_IFACE: &str = "org.freedesktop.UPower";
/// The synthetic device that aggregates every battery into one — the right
/// thing to warn on for a laptop with one or several cells.
const DISPLAY_DEVICE_PATH: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";

/// `org.freedesktop.UPower.Device.Type` code for a battery.
const DEVICE_TYPE_BATTERY: u32 = 2;

/// A connected handle to `UPower`.
#[derive(Clone)]
pub struct UPowerMonitor {
    manager: zbus::Proxy<'static>,
    display: zbus::Proxy<'static>,
}

impl UPowerMonitor {
    /// Connect to the system bus and build the manager + display-device
    /// proxies.
    pub async fn connect() -> zbus::Result<Self> {
        let conn = Connection::system().await?;
        Self::with_connection(&conn).await
    }

    /// Build proxies on an existing connection (shared with logind).
    pub async fn with_connection(conn: &Connection) -> zbus::Result<Self> {
        let manager = zbus::Proxy::new(conn, DEST, MGR_PATH, MGR_IFACE).await?;
        let display = zbus::Proxy::new(conn, DEST, DISPLAY_DEVICE_PATH, DEVICE_IFACE).await?;
        Ok(Self { manager, display })
    }

    /// Whether the lid is currently closed. Machines without a lid report
    /// `false`; pair with [`Self::lid_is_present`] to distinguish.
    pub async fn lid_is_closed(&self) -> zbus::Result<bool> {
        self.manager.get_property("LidIsClosed").await
    }

    /// Whether the machine has a lid at all.
    pub async fn lid_is_present(&self) -> zbus::Result<bool> {
        self.manager.get_property("LidIsPresent").await
    }

    /// Whether the machine is currently running from battery.
    pub async fn on_battery(&self) -> zbus::Result<bool> {
        self.manager.get_property("OnBattery").await
    }

    /// Read the composite battery as a [`BatterySnapshot`].
    ///
    /// A machine with no battery reports the display device as
    /// `Type != Battery` (or `IsPresent == false`); we surface that as
    /// [`BatterySnapshot::absent`] so the threshold monitor stays quiet.
    pub async fn battery_snapshot(&self) -> zbus::Result<BatterySnapshot> {
        let dev_type: u32 = self.display.get_property("Type").await.unwrap_or(0);
        let present: bool = self
            .display
            .get_property("IsPresent")
            .await
            .unwrap_or(false);
        if dev_type != DEVICE_TYPE_BATTERY || !present {
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

    #[tokio::test]
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
}
