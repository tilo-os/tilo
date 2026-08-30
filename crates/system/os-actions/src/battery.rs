//! Read-only battery diagnostics from the kernel power-supply class.
//!
//! Battery health is deliberately observational here.  Vendor charge-limit
//! files are not written by the desktop process: their names and semantics
//! differ between drivers, and granting a GUI arbitrary sysfs writes would be
//! a privilege boundary disguised as a setting.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A normalized description of the charge-limit control the kernel or
/// platform driver actually exposes.  Settings builds its choices from this
/// instead of assuming that every laptop accepts the same percentages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeCapCapability {
    pub backend: ChargeCapBackend,
    pub current: u32,
    pub choices: Vec<u32>,
    pub supports_start_threshold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeCapBackend {
    /// Linux power-supply attributes such as
    /// `charge_control_end_threshold`; used by ThinkPad, Framework, ASUS,
    /// TUXEDO and several other drivers.
    PowerSupply,
    /// Lenovo IdeaPad's binary conservation mode (roughly 55–60%, or off).
    ConservationMode,
    /// Huawei WMI's paired `start end` threshold control.
    HuaweiWmi,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSupplyKind {
    Battery,
    Ups,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerDevice {
    pub id: String,
    pub kind: PowerSupplyKind,
    pub capacity: u32,
    pub status: String,
    pub model: Option<String>,
    pub health_percent: Option<u32>,
}

/// Enumerate every battery and UPS.  Names and serial numbers are never
/// persisted; the id is only the kernel's current power-supply basename.
pub fn power_devices() -> Vec<PowerDevice> {
    power_devices_from(Path::new("/sys/class/power_supply"))
}

pub fn power_devices_from(root: &Path) -> Vec<PowerDevice> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let kind = match read_trim(&path.join("type")).as_deref() {
                Some("Battery") => PowerSupplyKind::Battery,
                Some("UPS") => PowerSupplyKind::Ups,
                _ => continue,
            };
            let info = BatteryInfo::from_battery_dir(&path, false);
            let health_percent = info.health_percent();
            out.push(PowerDevice {
                id: entry.file_name().to_string_lossy().to_string(),
                kind,
                capacity: info.capacity,
                status: info.status,
                model: read_trim(&path.join("model_name")),
                health_percent,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatteryHistoryPoint {
    pub unix_minute: u64,
    pub capacity: u32,
    pub health: Option<u32>,
    pub on_ac: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatteryHistory {
    pub points: Vec<BatteryHistoryPoint>,
}

impl BatteryHistory {
    const MAX_AGE_MINUTES: u64 = 30 * 24 * 60;
    const MIN_INTERVAL_MINUTES: u64 = 15;
    const MAX_POINTS: usize = 30 * 24 * 4;

    pub fn record(&mut self, point: BatteryHistoryPoint) {
        self.points
            .retain(|p| point.unix_minute.saturating_sub(p.unix_minute) <= Self::MAX_AGE_MINUTES);
        if self.points.last().is_some_and(|p| {
            point.unix_minute.saturating_sub(p.unix_minute) < Self::MIN_INTERVAL_MINUTES
        }) {
            if let Some(last) = self.points.last_mut() {
                *last = point;
            }
        } else {
            self.points.push(point);
        }
        if self.points.len() > Self::MAX_POINTS {
            self.points.drain(..self.points.len() - Self::MAX_POINTS);
        }
    }
}

fn history_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x).join("qos/power-history.json"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".local/state/qos/power-history.json"))
}

pub fn load_battery_history() -> BatteryHistory {
    history_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn record_battery_history(info: &BatteryInfo) -> Result<BatteryHistory, String> {
    let path = history_path().ok_or("no per-user state directory is available")?;
    let mut history = load_battery_history();
    let unix_minute = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 60;
    history.record(BatteryHistoryPoint {
        unix_minute,
        capacity: info.capacity,
        health: info.health_percent(),
        on_ac: info.on_ac,
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec(&history).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(history)
}

impl ChargeCapCapability {
    pub fn label(&self) -> &'static str {
        match self.backend {
            ChargeCapBackend::PowerSupply => "Hardware charge threshold",
            ChargeCapBackend::ConservationMode => "Battery conservation mode",
            ChargeCapBackend::HuaweiWmi => "Platform charge thresholds",
        }
    }
}

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

    pub fn health_explanation(&self) -> &'static str {
        match self.health_percent() {
            None => "This battery does not report comparable full and design capacity.",
            Some(90..=100) => {
                "Capacity is close to the factory design value; normal variation is expected."
            }
            Some(80..=89) => "Some capacity has been lost through normal chemical ageing.",
            Some(60..=79) => "Capacity is noticeably reduced; shorter runtime is expected.",
            Some(_) => {
                "Capacity is severely reduced. Consider service or replacement if runtime is disruptive."
            }
        }
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

/// Discover the real hardware contract.  Standard power-supply attributes
/// win; vendor paths are used only when the kernel has not normalized the
/// feature already.  `sys_root` is injectable so every backend is tested
/// without touching the host.
pub fn charge_capability() -> Option<ChargeCapCapability> {
    charge_capability_from(Path::new("/sys"))
}

pub fn charge_capability_from(sys_root: &Path) -> Option<ChargeCapCapability> {
    let supplies = sys_root.join("class/power_supply");
    let mut batteries: Vec<PathBuf> = std::fs::read_dir(&supplies)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| read_trim(&path.join("type")).as_deref() == Some("Battery"))
                .collect()
        })
        .unwrap_or_default();
    batteries.sort();
    for battery in batteries {
        for attr in ["charge_control_end_threshold", "charge_stop_threshold"] {
            if let Some(current) = read_trim(&battery.join(attr)).and_then(|v| v.parse().ok()) {
                return Some(ChargeCapCapability {
                    backend: ChargeCapBackend::PowerSupply,
                    current,
                    choices: (50..=100).step_by(5).collect(),
                    supports_start_threshold: battery
                        .join("charge_control_start_threshold")
                        .exists(),
                });
            }
        }
    }

    let mut conservation_paths = Vec::new();
    for root in [
        sys_root.join("bus/platform/drivers/ideapad_acpi"),
        sys_root.join("devices/platform"),
    ] {
        if let Ok(entries) = std::fs::read_dir(root) {
            conservation_paths.extend(entries.flatten().filter_map(|entry| {
                let device = entry.path();
                let is_ideapad = device
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("VPC"));
                let candidate = device.join("conservation_mode");
                (is_ideapad && candidate.is_file()).then_some(candidate)
            }));
        }
    }
    conservation_paths.sort();
    conservation_paths.dedup();
    for path in conservation_paths {
        if let Some(enabled) = read_trim(&path).and_then(|v| v.parse::<u32>().ok()) {
            return Some(ChargeCapCapability {
                backend: ChargeCapBackend::ConservationMode,
                current: if enabled == 0 { 100 } else { 60 },
                choices: vec![60, 100],
                supports_start_threshold: false,
            });
        }
    }

    let huawei = sys_root.join("devices/platform/huawei-wmi/charge_control_thresholds");
    if let Some(raw) = read_trim(&huawei) {
        let values: Vec<u32> = raw
            .split_ascii_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        if let Some(&current) = values.last() {
            return Some(ChargeCapCapability {
                backend: ChargeCapBackend::HuaweiWmi,
                current,
                choices: (50..=100).step_by(5).collect(),
                supports_start_threshold: true,
            });
        }
    }
    None
}

/// Apply a normalized charge cap through the narrowly-scoped system service.
pub fn set_charge_limit(percent: u32) -> Result<(), String> {
    if !(50..=100).contains(&percent) {
        return Err("battery charge cap must be between 50 and 100 percent".into());
    }
    let unit = format!("qos-battery-cap@{percent}.service");
    crate::process::run("systemctl", &["start", "--wait", &unit])
}

/// Apply and then re-read the hardware contract.  A successful systemd start
/// is not enough: firmware may accept the write while clamping or ignoring it.
pub fn set_charge_limit_checked(percent: u32) -> Result<ChargeCapCapability, String> {
    set_charge_limit(percent)?;
    let actual =
        charge_capability().ok_or("charge-cap control disappeared after applying the change")?;
    if actual.current != percent {
        return Err(format!(
            "firmware kept the charge cap at {}% instead of accepting {percent}%",
            actual.current
        ));
    }
    Ok(actual)
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

    #[test]
    fn capability_does_not_depend_on_battery_name() {
        let root = tempfile::tempdir().unwrap();
        let battery = root.path().join("class/power_supply/CMB1");
        std::fs::create_dir_all(&battery).unwrap();
        std::fs::write(battery.join("type"), "Battery\n").unwrap();
        std::fs::write(battery.join("charge_control_end_threshold"), "85\n").unwrap();
        std::fs::write(battery.join("charge_control_start_threshold"), "80\n").unwrap();
        let cap = charge_capability_from(root.path()).unwrap();
        assert_eq!(cap.current, 85);
        assert_eq!(cap.backend, ChargeCapBackend::PowerSupply);
        assert!(cap.supports_start_threshold);
        assert!(cap.choices.contains(&85));
    }

    #[test]
    fn ideapad_conservation_mode_is_exposed_as_discrete_choices() {
        let root = tempfile::tempdir().unwrap();
        let path = root
            .path()
            .join("devices/platform/VPC2004:00/conservation_mode");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "1\n").unwrap();
        let cap = charge_capability_from(root.path()).unwrap();
        assert_eq!(cap.backend, ChargeCapBackend::ConservationMode);
        assert_eq!(cap.current, 60);
        assert_eq!(cap.choices, vec![60, 100]);
    }

    #[test]
    fn enumerates_multiple_batteries_and_ups_without_serials() {
        let root = tempfile::tempdir().unwrap();
        for (id, kind, cap) in [("BAT1", "Battery", "70"), ("ups0", "UPS", "95")] {
            let p = root.path().join(id);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("type"), kind).unwrap();
            std::fs::write(p.join("capacity"), cap).unwrap();
            std::fs::write(p.join("status"), "Discharging").unwrap();
        }
        let d = power_devices_from(root.path());
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].kind, PowerSupplyKind::Ups);
    }

    #[test]
    fn history_is_bounded_coalesced_and_privacy_safe() {
        let mut h = BatteryHistory::default();
        h.record(BatteryHistoryPoint {
            unix_minute: 100,
            capacity: 50,
            health: Some(90),
            on_ac: false,
        });
        h.record(BatteryHistoryPoint {
            unix_minute: 105,
            capacity: 49,
            health: Some(90),
            on_ac: false,
        });
        assert_eq!(h.points.len(), 1);
        assert_eq!(h.points[0].capacity, 49);
        let json = serde_json::to_string(&h).unwrap();
        assert!(!json.contains("serial"));
    }
}
