//! What the sign-in screen shows about the machine before anyone signs in:
//! the battery and whether Wi-Fi is connected. Both come straight from
//! sysfs, readable by the unprivileged greeter user with no bus or service.

use std::path::Path;

/// The machine's state as the sign-in screen's status corner shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceStatus {
    /// Charge percentage and whether it is charging; `None` without a battery.
    pub battery: Option<(u8, bool)>,
    /// Whether a wireless interface is up; `None` without Wi-Fi hardware.
    pub wifi: Option<bool>,
}

impl DeviceStatus {
    /// Read the current state.
    pub fn read() -> Self {
        let battery = tilo_power_mgmt::sysfs::battery_snapshot();
        Self {
            battery: battery.present.then(|| {
                (
                    battery.percent,
                    matches!(
                        battery.state,
                        tilo_power_mgmt::battery::BatteryState::Charging
                            | tilo_power_mgmt::battery::BatteryState::PendingCharge
                            | tilo_power_mgmt::battery::BatteryState::Full
                    ) || !tilo_power_mgmt::sysfs::on_battery(),
                )
            }),
            wifi: wifi_in(Path::new("/sys/class/net")),
        }
    }
}

/// `Some(up)` when any interface under `net` is wireless.
fn wifi_in(net: &Path) -> Option<bool> {
    let mut seen = None;
    for entry in std::fs::read_dir(net).ok()?.flatten() {
        let path = entry.path();
        if !path.join("wireless").exists() && !path.join("phy80211").exists() {
            continue;
        }
        let up =
            std::fs::read_to_string(path.join("operstate")).is_ok_and(|state| state.trim() == "up");
        seen = Some(seen.unwrap_or(false) || up);
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_is_reported_only_for_wireless_interfaces() {
        let dir = tempfile::tempdir().unwrap();
        let iface = |name: &str, wireless: bool, state: &str| {
            let path = dir.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            if wireless {
                std::fs::create_dir_all(path.join("wireless")).unwrap();
            }
            std::fs::write(path.join("operstate"), state).unwrap();
        };
        iface("lo", false, "unknown\n");
        iface("eth0", false, "up\n");
        assert_eq!(
            wifi_in(dir.path()),
            None,
            "no Wi-Fi hardware, no Wi-Fi status"
        );
        iface("wlan0", true, "down\n");
        assert_eq!(wifi_in(dir.path()), Some(false));
        iface("wlan1", true, "up\n");
        assert_eq!(wifi_in(dir.path()), Some(true));
    }
}
