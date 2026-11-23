//! `UPower` (`org.freedesktop.UPower`) over `zbus`: reads the machine's
//! composite battery and lid state from the running `upowerd` and feeds
//! [`super::battery`], which holds the decisions. The lid and the battery are
//! watched, not polled.

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use zbus::Connection;

use crate::battery::{BatterySnapshot, BatteryState};
use crate::history::PowerSourceKind;

const DEST: &str = "org.freedesktop.UPower";
const MGR_PATH: &str = "/org/freedesktop/UPower";
const MGR_IFACE: &str = "org.freedesktop.UPower";
/// The synthetic device that aggregates every battery into one: the right thing
/// to warn on for a laptop with one or several cells.
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

#[derive(Clone)]
pub struct UPowerMonitor {
    connection: zbus::Connection,
    manager: zbus::Proxy<'static>,
    display: zbus::Proxy<'static>,
}

impl UPowerMonitor {
    /// Connect to the system bus and read the machine's own `upowerd`.
    ///
    /// # Errors
    ///
    /// Returns an error if this session owns no machine
    /// ([`tilo_paths::machine`]), if the system bus is unreachable, or if a
    /// proxy cannot be built.
    pub async fn connect() -> zbus::Result<Self> {
        if !tilo_paths::machine::owns_the_machine() {
            return Err(zbus::Error::Failure(
                "this session reaches no machine: UPower belongs to the desktop it runs in".into(),
            ));
        }
        let conn = Connection::system().await?;
        Self::with_connection(&conn).await
    }

    pub async fn with_connection(conn: &Connection) -> zbus::Result<Self> {
        let manager = zbus::Proxy::new(conn, DEST, MGR_PATH, MGR_IFACE).await?;
        let display = zbus::Proxy::new(conn, DEST, DISPLAY_DEVICE_PATH, DEVICE_IFACE).await?;
        Ok(Self {
            connection: conn.clone(),
            manager,
            display,
        })
    }

    /// Whether the lid is currently closed. Machines without a lid report `false`;
    /// pair with [`Self::lid_is_present`] to tell them apart.
    pub async fn lid_is_closed(&self) -> zbus::Result<bool> {
        self.manager.get_property("LidIsClosed").await
    }

    /// Every change of `LidIsClosed`, then [`Self::lid_is_closed`] reads the new
    /// value from the proxy's cache. Yields once at once when the value is already
    /// cached.
    pub async fn receive_lid_changes(&self) -> zbus::proxy::PropertyStream<'static, bool> {
        self.manager.receive_property_changed("LidIsClosed").await
    }

    /// One item per change of the display device's charge, charging state or
    /// presence: everything [`Self::battery_snapshot`] reads.
    pub async fn receive_battery_changes(&self) -> BoxStream<'static, ()> {
        let percentage = self
            .display
            .receive_property_changed::<f64>("Percentage")
            .await
            .map(|_| ());
        let state = self
            .display
            .receive_property_changed::<u32>("State")
            .await
            .map(|_| ());
        let present = self
            .display
            .receive_property_changed::<bool>("IsPresent")
            .await
            .map(|_| ());
        futures_util::stream::select(futures_util::stream::select(percentage, state), present)
            .boxed()
    }

    pub async fn lid_is_present(&self) -> zbus::Result<bool> {
        self.manager.get_property("LidIsPresent").await
    }

    pub async fn on_battery(&self) -> zbus::Result<bool> {
        self.manager.get_property("OnBattery").await
    }

    /// Read the composite battery. A machine with no battery reports the display
    /// device as not a battery or not present, which is surfaced as
    /// [`BatterySnapshot::absent`] so the threshold monitor stays quiet. A present
    /// battery whose charge or state cannot be read is an error: a reading made of
    /// defaults (`0%`) would look empty and take the critical action.
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
        battery_reading(
            self.display.get_property("Percentage").await,
            self.display.get_property("State").await,
        )
    }

    /// Read the aggregate power source using `UPower`'s display-device contract; no
    /// model, serial, process or application metadata reaches the durable history.
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
        let percentage: f64 = self.display.get_property("Percentage").await?;
        let health: f64 = self.display.get_property("Capacity").await.unwrap_or(0.0);
        Ok(Some(PowerHistorySample {
            kind,
            capacity: percentage.round().clamp(0.0, 100.0) as u32,
            health: (health > 0.0).then(|| health.round().clamp(0.0, 100.0) as u32),
            on_ac: !self.on_battery().await.unwrap_or(false),
        }))
    }

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

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=100.0 immediately before the cast, so it always fits a u8"
)]
fn battery_reading(
    percentage: zbus::Result<f64>,
    state: zbus::Result<u32>,
) -> zbus::Result<BatterySnapshot> {
    Ok(BatterySnapshot {
        present: true,
        percent: percentage?.round().clamp(0.0, 100.0) as u8,
        state: BatteryState::from_upower(state?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_charge_that_cannot_be_read_is_no_reading_and_never_a_critical_battery() {
        fn failed<T>() -> zbus::Result<T> {
            Err(zbus::Error::Failure("UPower did not answer".into()))
        }
        let policy = crate::policy::PowerPolicy::default();
        let discharging = 2;
        assert!(battery_reading(failed(), Ok(discharging)).is_err());
        assert!(battery_reading(Ok(50.0), failed()).is_err());

        let mut monitor = crate::battery::BatteryMonitor::new();
        let read = battery_reading(Ok(4.4), Ok(discharging)).expect("both read");
        assert_eq!(read.percent, 4);
        assert_eq!(
            monitor.evaluate(&read, &policy),
            vec![crate::battery::BatteryEvent::Critical(4)],
            "a real 4% still takes the critical action"
        );
    }
}
