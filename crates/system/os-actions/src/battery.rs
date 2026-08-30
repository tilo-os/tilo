//! Read-only battery diagnostics from the kernel power-supply class.
//!
//! Battery health is deliberately observational here.  Vendor charge-limit
//! files are not written by the desktop process: their names and semantics
//! differ between drivers, and granting a GUI arbitrary sysfs writes would be
//! a privilege boundary disguised as a setting.

use std::path::{Path, PathBuf};

/// A snapshot of the primary battery and external-power state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryInfo {
    pub present: bool,
    pub capacity: u32,
    pub status: String,
    pub cycle_count: Option<u32>,
    pub on_ac: bool,
    /// Current full-charge capacity in the units exported by the driver.
    pub full_capacity: Option<u64>,
    /// Factory design capacity in the same units as `full_capacity`.
    pub design_capacity: Option<u64>,
    /// Charge stop threshold reported by the platform driver.
    pub charge_limit: Option<u32>,
}

impl BatteryInfo {
    pub fn from_system() -> Self {
        Self::from_root(Path::new("/sys/class/power_supply"))
    }

    pub fn from_root(root: &Path) -> Self {
        let mut battery_dir: Option<PathBuf> = None;
        let mut on_ac = false;
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let dir = entry.path();
                match read_trim(&dir.join("type")).as_deref() {
                    Some("Battery") if battery_dir.is_none() => battery_dir = Some(dir),
                    Some("Mains") | Some("USB")
                        if read_trim(&dir.join("online")).as_deref() == Some("1") =>
                    {
                        on_ac = true;
                    }
                    _ => {}
                }
            }
        }
        battery_dir.map_or_else(
            || Self {
                present: false,
                capacity: 0,
                status: "No battery".into(),
                cycle_count: None,
                on_ac: true,
                full_capacity: None,
                design_capacity: None,
                charge_limit: None,
            },
            |dir| Self::from_battery_dir(&dir, on_ac),
        )
    }

    pub fn from_battery_dir(dir: &Path, on_ac: bool) -> Self {
        let read_num = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| read_trim(&dir.join(name)).and_then(|v| v.parse::<u64>().ok()))
        };
        Self {
            present: true,
            capacity: read_num(&["capacity"]).unwrap_or(0).min(100) as u32,
            status: read_trim(&dir.join("status")).unwrap_or_else(|| "Unknown".into()),
            cycle_count: read_num(&["cycle_count"]).and_then(|v| u32::try_from(v).ok()),
            on_ac,
            // Drivers expose either energy_* (µWh) or charge_* (µAh).
            full_capacity: read_num(&["energy_full", "charge_full"]),
            design_capacity: read_num(&["energy_full_design", "charge_full_design"]),
            charge_limit: read_num(&["charge_control_end_threshold", "charge_stop_threshold"])
                .map(|v| v.min(100) as u32),
        }
    }

    /// Remaining capacity relative to the factory design capacity.
    pub fn health_percent(&self) -> Option<u32> {
        let (full, design) = (self.full_capacity?, self.design_capacity?);
        (design > 0).then(|| ((full.saturating_mul(100) / design).min(100)) as u32)
    }

    pub fn summary(&self) -> String {
        if !self.present {
            return "Running on AC power".into();
        }
        match self.status.as_str() {
            "Full" => "Fully charged".into(),
            "Charging" => format!("{}% · Charging", self.capacity),
            "Not charging" => format!("{}% · Plugged in, not charging", self.capacity),
            _ => format!("{}% · On battery", self.capacity),
        }
    }
}

/// Apply a normalized charge cap through the narrowly-scoped system service.
pub fn set_charge_limit(percent: u32) -> Result<(), String> {
    if !(50..=100).contains(&percent) {
        return Err("battery charge cap must be between 50 and 100 percent".into());
    }
    let unit = format!("qos-battery-cap@{percent}.service");
    crate::process::run("systemctl", &["start", "--wait", &unit])
}

fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_uses_matching_kernel_capacity_units() {
        let dir = tempfile::tempdir().unwrap();
        for (name, value) in [
            ("capacity", "61"),
            ("status", "Discharging"),
            ("energy_full", "40000000"),
            ("energy_full_design", "50000000"),
        ] {
            std::fs::write(dir.path().join(name), value).unwrap();
        }
        let info = BatteryInfo::from_battery_dir(dir.path(), false);
        assert_eq!(info.health_percent(), Some(80));
        assert_eq!(info.summary(), "61% · On battery");
    }

    #[test]
    fn vendor_charge_cap_is_normalized() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("charge_stop_threshold"), "80\n").unwrap();
        let info = BatteryInfo::from_battery_dir(dir.path(), true);
        assert_eq!(info.charge_limit, Some(80));
    }

    #[test]
    fn charge_cap_rejects_values_outside_the_service_contract() {
        assert!(set_charge_limit(49).is_err());
        assert!(set_charge_limit(101).is_err());
    }
}
