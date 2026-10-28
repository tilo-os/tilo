//! Read-only battery diagnostics from the kernel power-supply class.
//!
//! Battery health is observational here.  Vendor charge-limit
//! files are not written by the desktop process: their names and semantics
//! differ between drivers, and granting a GUI arbitrary sysfs writes would be
//! a privilege boundary disguised as a setting.

use std::path::{Path, PathBuf};

pub use tilo_power_mgmt::{BatteryHistory, BatteryHistoryPoint, load_battery_history};

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
    /// `charge_control_end_threshold`; used by `ThinkPad`, Framework, ASUS,
    /// TUXEDO and several other drivers.
    PowerSupply,
    /// Lenovo `IdeaPad`'s binary conservation mode (roughly 55–60%, or off).
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

impl ChargeCapCapability {
    pub fn label(&self) -> &'static str {
        match self.backend {
            ChargeCapBackend::PowerSupply => "Hardware charge threshold",
            ChargeCapBackend::ConservationMode => "Battery conservation mode",
            ChargeCapBackend::HuaweiWmi => "Platform charge thresholds",
        }
    }
}

/// `amount` at `per_hour`, as a duration; `None` at no rate or past two
/// days, which a driver's momentary reading can suggest but no battery
/// lasts.
fn hours_at(amount: u64, per_hour: u64) -> Option<std::time::Duration> {
    let seconds = amount.checked_mul(3600)?.checked_div(per_hour)?;
    (seconds <= 48 * 3600).then(|| std::time::Duration::from_secs(seconds))
}

/// What a driver measures a battery's contents in: energy (`energy_*`,
/// µWh) or charge (`charge_*`, µAh).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityUnit {
    Energy,
    Charge,
}

/// A charge stop threshold as a cap: `0` is how some drivers (Qualcomm's
/// battmgr) say no threshold is set, since a battery stopped at 0 % would
/// never charge; such a battery charges to full.
fn charge_cap_of(threshold: u64) -> Option<u32> {
    (threshold > 0).then(|| u32::try_from(threshold.min(100)).unwrap_or(100))
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
    /// The unit of `full_capacity`, `design_capacity` and `remaining`.
    pub unit: CapacityUnit,
    /// Charge stop threshold reported by the platform driver.
    pub charge_limit: Option<u32>,
    /// What the battery holds now, in the units of `full_capacity`.
    pub remaining: Option<u64>,
    /// How fast it fills or drains, in those units per hour (µW for
    /// energy, µA for charge); `None` or zero when the driver does not say.
    pub rate: Option<u64>,
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
                    Some("Mains" | "USB")
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
                unit: CapacityUnit::Energy,
                charge_limit: None,
                remaining: None,
                rate: None,
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
        // The level now and its rate of change, in matching units: energy
        // (µWh, µW) or charge (µAh, µA). Some drivers sign the rate.
        let read_signed = |name: &str| {
            read_trim(&dir.join(name))
                .and_then(|v| v.parse::<i64>().ok())
                .map(i64::unsigned_abs)
        };
        let (remaining, rate, remaining_unit) = match read_num(&["energy_now"]) {
            Some(energy) => (Some(energy), read_signed("power_now"), CapacityUnit::Energy),
            None => (
                read_num(&["charge_now"]),
                read_signed("current_now"),
                CapacityUnit::Charge,
            ),
        };
        // Drivers expose either energy_* (µWh) or charge_* (µAh); full and
        // design are read in the same unit so health compares like with like.
        let (unit, full_capacity, design_capacity) =
            match (read_num(&["energy_full"]), read_num(&["charge_full"])) {
                (Some(full), _) => (
                    CapacityUnit::Energy,
                    Some(full),
                    read_num(&["energy_full_design"]),
                ),
                (None, Some(full)) => (
                    CapacityUnit::Charge,
                    Some(full),
                    read_num(&["charge_full_design"]),
                ),
                (None, None) => (remaining_unit, None, None),
            };
        Self {
            present: true,
            remaining,
            rate,
            capacity: read_num(&["capacity"])
                .or_else(|| {
                    // Some drivers (Qualcomm's battmgr) publish no
                    // `capacity`, only the charge or energy left and full.
                    [("energy_now", "energy_full"), ("charge_now", "charge_full")]
                        .iter()
                        .find_map(|(now, full)| {
                            let (now, full) = (read_num(&[now])?, read_num(&[full])?);
                            now.saturating_mul(100)
                                .saturating_add(full / 2)
                                .checked_div(full)
                        })
                })
                .unwrap_or(0)
                .min(100) as u32,
            status: read_trim(&dir.join("status")).unwrap_or_else(|| "Unknown".into()),
            cycle_count: read_num(&["cycle_count"]).and_then(|v| u32::try_from(v).ok()),
            on_ac,
            full_capacity,
            design_capacity,
            unit,
            charge_limit: read_num(&["charge_control_end_threshold", "charge_stop_threshold"])
                .and_then(charge_cap_of),
        }
    }

    /// How long until the battery is empty at the rate it drains now, while
    /// it is discharging.
    pub fn time_to_empty(&self) -> Option<std::time::Duration> {
        if !self.present || self.on_ac || self.status != "Discharging" {
            return None;
        }
        hours_at(self.remaining?, self.rate?)
    }

    /// How long until the battery is full (or at its charge limit) at the
    /// rate it charges now, while it is charging.
    pub fn time_to_full(&self) -> Option<std::time::Duration> {
        if !self.present || self.status != "Charging" {
            return None;
        }
        let full = self.full_capacity?;
        let target = match self.charge_limit {
            Some(limit) => full.saturating_mul(u64::from(limit.min(100))) / 100,
            None => full,
        };
        hours_at(target.checked_sub(self.remaining?)?, self.rate?)
    }

    /// Remaining capacity relative to the factory design capacity.
    pub fn health_percent(&self) -> Option<u32> {
        let (full, design) = (self.full_capacity?, self.design_capacity?);
        full.saturating_mul(100)
            .saturating_add(design / 2)
            .checked_div(design)
            .map(|pct| u32::try_from(pct.min(100)).unwrap_or(100))
    }

    /// What the battery holds when full now and what it held new, in watt
    /// hours; `None` for a driver that counts charge or does not say.
    #[allow(
        clippy::cast_precision_loss,
        reason = "µWh of a battery are far below f64's exact integer range"
    )]
    pub fn full_and_design_wh(&self) -> Option<(f64, f64)> {
        if self.unit != CapacityUnit::Energy {
            return None;
        }
        let (full, design) = (self.full_capacity?, self.design_capacity?);
        (design > 0).then(|| (full as f64 / 1e6, design as f64 / 1e6))
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

/// Discover the hardware contract.  Standard power-supply attributes
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
    let mut standard = Vec::new();
    for battery in batteries {
        for attr in ["charge_control_end_threshold", "charge_stop_threshold"] {
            if let Some(threshold) = read_trim(&battery.join(attr)).and_then(|v| v.parse().ok()) {
                standard.push((
                    charge_cap_of(threshold).unwrap_or(100),
                    battery.join("charge_control_start_threshold").exists(),
                ));
                break;
            }
        }
    }
    if let Some(&(current, supports_start_threshold)) = standard.first() {
        // The privileged helper applies one normalized cap to every battery.
        // A single selector is only truthful when every controllable battery
        // currently exposes the same contract; mixed state is left to a
        // specialist diagnostic tool rather than partially changing hardware.
        if standard
            .iter()
            .any(|entry| *entry != (current, supports_start_threshold))
        {
            return None;
        }
        return Some(ChargeCapCapability {
            backend: ChargeCapBackend::PowerSupply,
            current,
            choices: (50..=100).step_by(5).collect(),
            supports_start_threshold,
        });
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
///
/// # Errors
///
/// Returns an error message if `percent` is outside `50..=100`, or the `tilo-battery-cap@`
/// service is unavailable, not authorised, or fails.
pub fn set_charge_limit(percent: u32) -> Result<(), String> {
    if !(50..=100).contains(&percent) {
        return Err("battery charge cap must be between 50 and 100 percent".into());
    }
    let unit = format!("tilo-battery-cap@{percent}.service");
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
    fn battery_health_and_charge_caps_follow_the_kernel_contract() {
        {
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
        {
            let dir = tempfile::tempdir().unwrap();
            for (name, value) in [
                ("status", "Not charging"),
                ("energy_now", "53950000"),
                ("energy_full", "56160000"),
            ] {
                std::fs::write(dir.path().join(name), value).unwrap();
            }
            let info = BatteryInfo::from_battery_dir(dir.path(), true);
            assert_eq!(info.capacity, 96);
            assert_eq!(info.summary(), "96% · Plugged in, not charging");
        }
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("charge_stop_threshold"), "80\n").unwrap();
            let info = BatteryInfo::from_battery_dir(dir.path(), true);
            assert_eq!(info.charge_limit, Some(80));
        }
        {
            assert!(set_charge_limit(49).is_err());
            assert!(set_charge_limit(101).is_err());
        }
        {
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
    }

    /// Time left and time to full come from the level now and the rate the
    /// driver reports, in matching units, signed or not; a battery that is
    /// neither draining nor charging has neither.
    #[test]
    fn time_left_and_to_full_follow_the_drivers_rate() {
        let read = |files: &[(&str, &str)], on_ac: bool| {
            let dir = tempfile::tempdir().unwrap();
            for (name, value) in files {
                std::fs::write(dir.path().join(name), value).unwrap();
            }
            BatteryInfo::from_battery_dir(dir.path(), on_ac)
        };
        let draining = read(
            &[
                ("status", "Discharging"),
                ("energy_now", "30000000"),
                ("energy_full", "60000000"),
                ("power_now", "10000000"),
            ],
            false,
        );
        assert_eq!(
            draining.time_to_empty(),
            Some(std::time::Duration::from_secs(3 * 3600))
        );
        assert_eq!(draining.time_to_full(), None);
        let charging = read(
            &[
                ("status", "Charging"),
                ("charge_now", "2000000"),
                ("charge_full", "4000000"),
                ("current_now", "-1000000"),
                ("charge_control_end_threshold", "80"),
            ],
            true,
        );
        assert_eq!(
            charging.time_to_full(),
            Some(std::time::Duration::from_secs(72 * 60)),
            "to the 80 % limit at 1 A"
        );
        assert_eq!(charging.time_to_empty(), None);
        let idle = read(&[("status", "Not charging"), ("energy_now", "1")], true);
        assert_eq!((idle.time_to_empty(), idle.time_to_full()), (None, None));
        let unknown_rate = read(&[("status", "Discharging"), ("energy_now", "1")], false);
        assert_eq!(unknown_rate.time_to_empty(), None);
    }

    /// Qualcomm's battmgr (Snapdragon laptops) publishes no `capacity`, only
    /// energy now, full and design, and a stop threshold of 0 while none is
    /// set: the level is computed, the worn pack's health shows its loss in
    /// watt hours, and charging is not capped.
    #[test]
    fn a_battmgr_battery_shows_its_level_its_wear_and_no_cap() {
        let root = tempfile::tempdir().unwrap();
        let battery = root.path().join("class/power_supply/qcom-battmgr-bat");
        std::fs::create_dir_all(&battery).unwrap();
        for (name, value) in [
            ("type", "Battery"),
            ("status", "Discharging"),
            ("energy_now", "28980000"),
            ("energy_full", "44600000"),
            ("energy_full_design", "70000000"),
            ("power_now", "-19401000"),
            ("cycle_count", "334"),
            ("charge_full", ""),
            ("charge_full_design", ""),
            ("charge_control_end_threshold", "0"),
            ("charge_control_start_threshold", "0"),
        ] {
            std::fs::write(battery.join(name), format!("{value}\n")).unwrap();
        }
        let info = BatteryInfo::from_battery_dir(&battery, false);
        assert_eq!(info.capacity, 65);
        assert_eq!(info.cycle_count, Some(334));
        assert_eq!(info.health_percent(), Some(64));
        assert_eq!(info.full_and_design_wh(), Some((44.6, 70.0)));
        assert_eq!(info.charge_limit, None, "0 is no threshold, not a 0 % cap");
        assert!(info.time_to_empty().is_some());

        let cap = charge_capability_from(root.path()).unwrap();
        assert_eq!(cap.current, 100, "charging to full");
        assert!(cap.choices.contains(&cap.current));

        let charge_counted = tempfile::tempdir().unwrap();
        for (name, value) in [
            ("charge_full", "3000000"),
            ("charge_full_design", "4000000"),
        ] {
            std::fs::write(charge_counted.path().join(name), value).unwrap();
        }
        let info = BatteryInfo::from_battery_dir(charge_counted.path(), true);
        assert_eq!(info.unit, CapacityUnit::Charge);
        assert_eq!(info.health_percent(), Some(75));
        assert_eq!(info.full_and_design_wh(), None);
    }

    #[test]
    fn battery_capabilities_are_per_device_and_enumerate_every_supply() {
        {
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
        {
            let root = tempfile::tempdir().unwrap();
            for (name, limit) in [("BAT-main", "80"), ("CMB-aux", "90")] {
                let battery = root.path().join("class/power_supply").join(name);
                std::fs::create_dir_all(&battery).unwrap();
                std::fs::write(battery.join("type"), "Battery\n").unwrap();
                std::fs::write(battery.join("charge_control_end_threshold"), limit).unwrap();
            }
            assert!(charge_capability_from(root.path()).is_none());
        }
        {
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
    }
}
