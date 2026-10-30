//! Reading the battery straight off `/sys/class/power_supply`.
//!
//! [`super::upower`] is the richer source and stays the one the power
//! *service* uses — it aggregates every battery into one composite
//! device and it pushes changes rather than being asked. But it is async
//! and needs a system-bus connection, and the compositor's status
//! indicator needs a number it can read from a plain polling thread with
//! no runtime, no bus, and no subprocess. A file read is that.
//!
//! The kernel's power-supply class is a stable ABI (`Documentation/ABI/
//! testing/sysfs-class-power`), present on every laptop, and readable by
//! anyone — so this works before logind is up, inside the initrd, and in
//! a container without a bus, all cases where `UPower` answers nothing.
//!
//! This is also the one reader of that class: the Settings power pane, the
//! installer's low-battery check and the shell's device health all go
//! through the types below rather than parsing the class themselves.
//!
//! Multiple batteries are aggregated the way `UPower` does it: by energy,
//! so a machine with a 60 Wh and a 20 Wh pack reports one honest
//! percentage rather than the mean of two unrelated ones.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::battery::{BatterySnapshot, BatteryState};

/// The kernel's power-supply class directory. A parameter on the inner
/// functions so the tests can point at a fixture tree.
const POWER_SUPPLY: &str = "/sys/class/power_supply";

/// What a driver measures a battery's contents in: energy (`energy_*`,
/// µWh) or charge (`charge_*`, µAh).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityUnit {
    Energy,
    Charge,
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
        Self::from_root(Path::new(POWER_SUPPLY))
    }

    pub fn from_root(root: &Path) -> Self {
        let mut battery_dir: Option<PathBuf> = None;
        let mut on_ac = false;
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let dir = entry.path();
                match read_trimmed(&dir, "type").as_deref() {
                    Some("Battery") if battery_dir.is_none() => battery_dir = Some(dir),
                    Some("Mains" | "USB")
                        if read_trimmed(&dir, "online").as_deref() == Some("1") =>
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
        let read_num = |names: &[&str]| names.iter().find_map(|name| read_u64(dir, name));
        // The level now and its rate of change, in matching units: energy
        // (µWh, µW) or charge (µAh, µA). Some drivers sign the rate.
        let read_signed = |name: &str| {
            read_trimmed(dir, name)
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
            status: read_trimmed(dir, "status").unwrap_or_else(|| "Unknown".into()),
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
    pub fn time_to_empty(&self) -> Option<Duration> {
        if !self.present || self.on_ac || self.status != "Discharging" {
            return None;
        }
        hours_at(self.remaining?, self.rate?)
    }

    /// How long until the battery is full (or at its charge limit) at the
    /// rate it charges now, while it is charging.
    pub fn time_to_full(&self) -> Option<Duration> {
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

    /// One line for a status card.
    #[must_use]
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

/// A charge stop threshold as a cap: `0` is how some drivers (Qualcomm's
/// battmgr) say no threshold is set, since a battery stopped at 0 % would
/// never charge; such a battery charges to full.
fn charge_cap_of(threshold: u64) -> Option<u32> {
    (threshold > 0).then(|| u32::try_from(threshold.min(100)).unwrap_or(100))
}

/// `amount` at `per_hour`, as a duration; `None` at no rate or past two
/// days, which a driver's momentary reading can suggest but no battery
/// lasts.
fn hours_at(amount: u64, per_hour: u64) -> Option<Duration> {
    let seconds = amount.checked_mul(3600)?.checked_div(per_hour)?;
    (seconds <= 48 * 3600).then(|| Duration::from_secs(seconds))
}

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

impl ChargeCapCapability {
    pub fn label(&self) -> &'static str {
        match self.backend {
            ChargeCapBackend::PowerSupply => "Hardware charge threshold",
            ChargeCapBackend::ConservationMode => "Battery conservation mode",
            ChargeCapBackend::HuaweiWmi => "Platform charge thresholds",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSupplyKind {
    Battery,
    Ups,
}

/// One battery or UPS the kernel lists, as the charge-limit discovery and the
/// Settings battery card see it.  Names and serial numbers are never
/// persisted; the id is only the kernel's current power-supply basename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerDevice {
    pub id: String,
    pub kind: PowerSupplyKind,
    pub capacity: u32,
    pub status: String,
    pub model: Option<String>,
    pub health_percent: Option<u32>,
}

/// Enumerate every battery and UPS.
pub fn power_devices() -> Vec<PowerDevice> {
    power_devices_from(Path::new(POWER_SUPPLY))
}

pub fn power_devices_from(root: &Path) -> Vec<PowerDevice> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = match read_trimmed(&path, "type").as_deref() {
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
            model: read_trimmed(&path, "model_name"),
            health_percent,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Discover the hardware charge-limit contract.  Standard power-supply
/// attributes win; vendor paths are used only when the kernel has not
/// normalized the feature already.  `sys_root` is injectable so every backend
/// is tested without touching the host.
pub fn charge_capability() -> Option<ChargeCapCapability> {
    charge_capability_from(Path::new("/sys"))
}

pub fn charge_capability_from(sys_root: &Path) -> Option<ChargeCapCapability> {
    let supplies = sys_root.join("class/power_supply");
    let mut standard = Vec::new();
    for battery in &supplies_of_type(&supplies, "Battery") {
        for attr in ["charge_control_end_threshold", "charge_stop_threshold"] {
            if let Some(threshold) = read_u64(battery, attr) {
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
        if let Ok(entries) = fs::read_dir(root) {
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
        if let Some(enabled) = fs::read_to_string(&path)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
        {
            return Some(ChargeCapCapability {
                backend: ChargeCapBackend::ConservationMode,
                current: if enabled == 0 { 100 } else { 60 },
                choices: vec![60, 100],
                supports_start_threshold: false,
            });
        }
    }

    let huawei = sys_root.join("devices/platform/huawei-wmi/charge_control_thresholds");
    if let Ok(raw) = fs::read_to_string(&huawei) {
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

/// Read the machine's composite battery from sysfs.
///
/// Returns [`BatterySnapshot::absent`] on a machine with no battery — a
/// desktop, a VM — which is the same thing `UPower` reports and is what
/// the indicator renders as "no battery" rather than as "empty".
pub fn battery_snapshot() -> BatterySnapshot {
    snapshot_in(Path::new(POWER_SUPPLY))
}

/// How long the battery has left to run, or to charge, at the rate it is
/// going now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryTime {
    /// Minutes until it is empty.
    ToEmpty(u32),
    /// Minutes until it is full.
    ToFull(u32),
}

/// [`BatteryTime`] from sysfs: the packs' energy (or charge) and the rate
/// it is flowing at. `None` while the rate is not known — just after the
/// cable moved, on firmware that does not report it, at rest when full.
#[must_use]
pub fn battery_time() -> Option<BatteryTime> {
    time_in(Path::new(POWER_SUPPLY))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "energy and power are tens of watt-hours and watts in micro units, far inside f64's \
              exact range; the minutes are clamped to a day before the cast"
)]
fn time_in(root: &Path) -> Option<BatteryTime> {
    let mut now_total: u64 = 0;
    let mut full_total: u64 = 0;
    let mut rate_total: u64 = 0;
    let mut states = Vec::new();
    for dir in supplies_of_type(root, "Battery") {
        // A battery that is present but not `present` (a removed pack in
        // a dual-bay laptop) contributes nothing.
        if read_u64(&dir, "present").is_some_and(|p| p == 0) {
            continue;
        }
        let info = BatteryInfo::from_battery_dir(&dir, false);
        states.push(state_of(&info.status));
        // Energy (µWh) over power (µW), or charge (µAh) over current
        // (µA): never one of each, so the ratio is hours either way.
        now_total = now_total.saturating_add(info.remaining?);
        full_total = full_total.saturating_add(info.full_capacity?);
        rate_total = rate_total.saturating_add(info.rate?);
    }
    if rate_total == 0 {
        return None;
    }
    let minutes = |amount: u64| -> u32 {
        ((amount as f64 / rate_total as f64) * 60.0)
            .round()
            .clamp(0.0, 24.0 * 60.0) as u32
    };
    match composite_state(&states) {
        BatteryState::Discharging => Some(BatteryTime::ToEmpty(minutes(now_total))),
        BatteryState::Charging => Some(BatteryTime::ToFull(minutes(
            full_total.saturating_sub(now_total),
        ))),
        _ => None,
    }
}

/// Whether the machine is running on battery (no mains supply online).
///
/// Read separately from the battery because it is also true on a machine
/// whose battery is absent or unreadable, and because the AC adapter's
/// `online` flag changes the instant the cable moves, while a battery's
/// `status` can lag a poll behind.
pub fn on_battery() -> bool {
    on_battery_in(Path::new(POWER_SUPPLY))
}

// Battery energy values are always tiny relative to f64's precision limit
// (tens of watt-hours in µWh), and the percentage ratio below is clamped to
// 0.0..=100.0 immediately before its cast to u8, so it always fits.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "battery energy values are always tiny relative to f64's precision limit, and the \
              percentage ratio is clamped to 0.0..=100.0 immediately before the cast"
)]
fn snapshot_in(root: &Path) -> BatterySnapshot {
    // Energy-weighted aggregate. `energy_*` is µWh and `charge_*` is µAh;
    // either works as a weight as long as we do not mix the two units
    // within one machine, and machines do not mix them.
    let mut now_total: u64 = 0;
    let mut full_total: u64 = 0;
    let mut percent_fallback: Vec<u8> = Vec::new();
    let mut states: Vec<BatteryState> = Vec::new();

    for dir in supplies_of_type(root, "Battery") {
        // A battery that is present but not `present` (a removed pack in
        // a dual-bay laptop) contributes nothing.
        if read_u64(&dir, "present").is_some_and(|p| p == 0) {
            continue;
        }
        let info = BatteryInfo::from_battery_dir(&dir, false);
        states.push(state_of(&info.status));
        match (info.remaining, info.full_capacity) {
            (Some(n), Some(f)) if f > 0 => {
                now_total = now_total.saturating_add(n);
                full_total = full_total.saturating_add(f);
            }
            // No energy counters: some firmware exposes only `capacity`.
            // Keep it as a fallback rather than dropping the battery.
            _ => {
                if let Some(c) = read_u64(&dir, "capacity") {
                    percent_fallback.push(c.min(100) as u8);
                }
            }
        }
    }

    if states.is_empty() {
        return BatterySnapshot::absent();
    }

    let percent = if full_total > 0 {
        // Round rather than truncate: a 99.6%-full battery reading "99"
        // for minutes before it says "100" looks like a stuck charge.
        ((now_total as f64 / full_total as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    } else if !percent_fallback.is_empty() {
        let sum = percent_fallback.iter().map(|p| u32::from(*p)).sum::<u32>();
        let count = u32::try_from(percent_fallback.len()).unwrap_or(u32::MAX);
        let avg = sum.checked_div(count).unwrap_or(0);
        u8::try_from(avg.min(100)).unwrap_or(100)
    } else {
        return BatterySnapshot::absent();
    };

    BatterySnapshot {
        present: true,
        percent,
        state: composite_state(&states),
    }
}

fn on_battery_in(root: &Path) -> bool {
    let mains = supplies_of_type(root, "Mains");
    if mains.is_empty() {
        // No AC adapter node at all. If there is a battery, we are on it;
        // if there is not, this is a desktop and it is not on battery.
        return !supplies_of_type(root, "Battery").is_empty();
    }
    !mains
        .iter()
        .any(|d| read_u64(d, "online").is_some_and(|v| v == 1))
}

/// Every `/sys/class/power_supply/*` entry whose `type` matches.
fn supplies_of_type(root: &Path, want: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| read_trimmed(p, "type").as_deref() == Some(want))
        .collect();
    // Deterministic order so a two-battery machine aggregates the same
    // way on every poll (readdir order is not stable).
    out.sort();
    out
}

fn read_trimmed(dir: &Path, attr: &str) -> Option<String> {
    fs::read_to_string(dir.join(attr))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_u64(dir: &Path, attr: &str) -> Option<u64> {
    read_trimmed(dir, attr)?.parse().ok()
}

/// Map the kernel's `status` string onto the UPower-shaped enum the rest
/// of the crate speaks.
fn state_of(status: &str) -> BatteryState {
    match status {
        "Charging" => BatteryState::Charging,
        "Discharging" => BatteryState::Discharging,
        "Full" => BatteryState::Full,
        "Not charging" => BatteryState::PendingCharge,
        _ => BatteryState::Unknown,
    }
}

/// Collapse per-pack states into one.
///
/// Any pack charging means the machine is gaining runtime, which is the
/// signal the indicator and the low-battery latch both care about; that
/// is why charging outranks discharging rather than the majority winning.
fn composite_state(states: &[BatteryState]) -> BatteryState {
    if states.contains(&BatteryState::Charging) {
        return BatteryState::Charging;
    }
    if states.contains(&BatteryState::Discharging) {
        return BatteryState::Discharging;
    }
    if states.iter().all(|s| *s == BatteryState::Full) {
        return BatteryState::Full;
    }
    states.first().copied().unwrap_or(BatteryState::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a fixture power-supply tree. Each supply is
    /// `(name, [(attr, value)])`.
    fn tree(supplies: &[(&str, &[(&str, &str)])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (name, attrs) in supplies {
            let d = dir.path().join(name);
            fs::create_dir_all(&d).unwrap();
            for (k, v) in *attrs {
                fs::write(d.join(k), v).unwrap();
            }
        }
        dir
    }

    #[test]
    fn time_left_is_energy_over_the_rate_it_flows_at() {
        let draining = tree(&[(
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("status", "Discharging\n"),
                ("energy_now", "30000000\n"),
                ("energy_full", "60000000\n"),
                ("power_now", "10000000\n"),
            ],
        )]);
        assert_eq!(time_in(draining.path()), Some(BatteryTime::ToEmpty(180)));
        let charging = tree(&[(
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("status", "Charging\n"),
                ("charge_now", "2000000\n"),
                ("charge_full", "4000000\n"),
                ("current_now", "1000000\n"),
            ],
        )]);
        assert_eq!(time_in(charging.path()), Some(BatteryTime::ToFull(120)));
        let resting = tree(&[(
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("status", "Full\n"),
                ("energy_now", "60000000\n"),
                ("energy_full", "60000000\n"),
                ("power_now", "0\n"),
            ],
        )]);
        assert_eq!(time_in(resting.path()), None, "no rate, no estimate");
    }

    #[test]
    fn sysfs_supplies_report_absence_mains_and_percentage_sources() {
        {
            let snap = snapshot_in(Path::new("/definitely/not/a/path"));
            assert!(!snap.present);
            assert_eq!(snap.state, BatteryState::Unknown);
        }
        {
            let t = tree(&[("AC", &[("type", "Mains\n"), ("online", "1\n")])]);
            assert!(!snapshot_in(t.path()).present);
            assert!(!on_battery_in(t.path()));
        }
        {
            let t = tree(&[(
                "BAT0",
                &[
                    ("type", "Battery\n"),
                    ("status", "Discharging\n"),
                    ("energy_now", "21000000\n"),
                    ("energy_full", "50000000\n"),
                ],
            )]);
            let snap = snapshot_in(t.path());
            assert!(snap.present);
            assert_eq!(snap.percent, 42, "21 of 50 Wh");
            assert_eq!(snap.state, BatteryState::Discharging);
        }
        {
            let t = tree(&[(
                "BAT0",
                &[
                    ("type", "Battery\n"),
                    ("status", "Charging\n"),
                    ("charge_now", "3000000\n"),
                    ("charge_full", "4000000\n"),
                ],
            )]);
            let snap = snapshot_in(t.path());
            assert_eq!(snap.percent, 75);
            assert_eq!(snap.state, BatteryState::Charging);
        }
        {
            let t = tree(&[(
                "BAT0",
                &[
                    ("type", "Battery\n"),
                    ("status", "Full\n"),
                    ("capacity", "97\n"),
                ],
            )]);
            let snap = snapshot_in(t.path());
            assert_eq!(snap.percent, 97);
            assert_eq!(snap.state, BatteryState::Full);
        }
    }

    #[test]
    fn multiple_packs_aggregate_by_energy_and_composite_state() {
        {
            // 10/10 Wh full + 0/40 Wh empty is 20% of the machine's energy,
            // not the 50% a naive mean of the two percentages would report.
            let t = tree(&[
                (
                    "BAT0",
                    &[
                        ("type", "Battery\n"),
                        ("status", "Full\n"),
                        ("energy_now", "10000000\n"),
                        ("energy_full", "10000000\n"),
                    ],
                ),
                (
                    "BAT1",
                    &[
                        ("type", "Battery\n"),
                        ("status", "Discharging\n"),
                        ("energy_now", "0\n"),
                        ("energy_full", "40000000\n"),
                    ],
                ),
            ]);
            let snap = snapshot_in(t.path());
            assert_eq!(snap.percent, 20);
            assert_eq!(
                snap.state,
                BatteryState::Discharging,
                "one pack draining means the machine is draining"
            );
        }
        {
            let t = tree(&[
                (
                    "BAT0",
                    &[
                        ("type", "Battery\n"),
                        ("present", "1\n"),
                        ("status", "Discharging\n"),
                        ("energy_now", "5000000\n"),
                        ("energy_full", "10000000\n"),
                    ],
                ),
                (
                    "BAT1",
                    &[
                        ("type", "Battery\n"),
                        ("present", "0\n"),
                        ("status", "Unknown\n"),
                        ("energy_now", "0\n"),
                        ("energy_full", "90000000\n"),
                    ],
                ),
            ]);
            let snap = snapshot_in(t.path());
            assert_eq!(snap.percent, 50, "the removed pack contributes nothing");
        }
        {
            assert_eq!(
                composite_state(&[BatteryState::Discharging, BatteryState::Charging]),
                BatteryState::Charging
            );
        }
        {
            let t = tree(&[
                ("AC", &[("type", "Mains\n"), ("online", "0\n")]),
                (
                    "BAT0",
                    &[
                        ("type", "Battery\n"),
                        ("status", "Discharging\n"),
                        ("capacity", "55\n"),
                    ],
                ),
            ]);
            assert!(on_battery_in(t.path()));
        }
    }

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
                fs::write(dir.path().join(name), value).unwrap();
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
                fs::write(dir.path().join(name), value).unwrap();
            }
            let info = BatteryInfo::from_battery_dir(dir.path(), true);
            assert_eq!(info.capacity, 96);
            assert_eq!(info.summary(), "96% · Plugged in, not charging");
        }
        {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("charge_stop_threshold"), "80\n").unwrap();
            let info = BatteryInfo::from_battery_dir(dir.path(), true);
            assert_eq!(info.charge_limit, Some(80));
        }
        {
            let root = tempfile::tempdir().unwrap();
            let path = root
                .path()
                .join("devices/platform/VPC2004:00/conservation_mode");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "1\n").unwrap();
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
                fs::write(dir.path().join(name), value).unwrap();
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
            Some(Duration::from_secs(3 * 3600))
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
            Some(Duration::from_secs(72 * 60)),
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
        fs::create_dir_all(&battery).unwrap();
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
            fs::write(battery.join(name), format!("{value}\n")).unwrap();
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
            fs::write(charge_counted.path().join(name), value).unwrap();
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
            fs::create_dir_all(&battery).unwrap();
            fs::write(battery.join("type"), "Battery\n").unwrap();
            fs::write(battery.join("charge_control_end_threshold"), "85\n").unwrap();
            fs::write(battery.join("charge_control_start_threshold"), "80\n").unwrap();
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
                fs::create_dir_all(&battery).unwrap();
                fs::write(battery.join("type"), "Battery\n").unwrap();
                fs::write(battery.join("charge_control_end_threshold"), limit).unwrap();
            }
            assert!(charge_capability_from(root.path()).is_none());
        }
        {
            let root = tempfile::tempdir().unwrap();
            for (id, kind, cap) in [("BAT1", "Battery", "70"), ("ups0", "UPS", "95")] {
                let p = root.path().join(id);
                fs::create_dir_all(&p).unwrap();
                fs::write(p.join("type"), kind).unwrap();
                fs::write(p.join("capacity"), cap).unwrap();
                fs::write(p.join("status"), "Discharging").unwrap();
            }
            let d = power_devices_from(root.path());
            assert_eq!(d.len(), 2);
            assert_eq!(d[1].kind, PowerSupplyKind::Ups);
        }
    }
}
