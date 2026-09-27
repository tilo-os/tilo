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
//! Multiple batteries are aggregated the way `UPower` does it: by energy,
//! so a machine with a 60 Wh and a 20 Wh pack reports one honest
//! percentage rather than the mean of two unrelated ones.

use std::fs;
use std::path::{Path, PathBuf};

use crate::battery::{BatterySnapshot, BatteryState};

/// The kernel's power-supply class directory. A parameter on the inner
/// functions so the tests can point at a fixture tree.
const POWER_SUPPLY: &str = "/sys/class/power_supply";

/// Read the machine's composite battery from sysfs.
///
/// Returns [`BatterySnapshot::absent`] on a machine with no battery — a
/// desktop, a VM — which is the same thing `UPower` reports and is what
/// the indicator renders as "no battery" rather than as "empty".
pub fn battery_snapshot() -> BatterySnapshot {
    snapshot_in(Path::new(POWER_SUPPLY))
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
    let batteries = supplies_of_type(root, "Battery");
    if batteries.is_empty() {
        return BatterySnapshot::absent();
    }

    // Energy-weighted aggregate. `energy_*` is µWh and `charge_*` is µAh;
    // either works as a weight as long as we do not mix the two units
    // within one machine, and machines do not mix them.
    let mut now_total: u64 = 0;
    let mut full_total: u64 = 0;
    let mut percent_fallback: Vec<u8> = Vec::new();
    let mut states: Vec<BatteryState> = Vec::new();

    for dir in &batteries {
        // A battery that is present but not `present` (a removed pack in
        // a dual-bay laptop) contributes nothing.
        if read_u64(dir, "present").is_some_and(|p| p == 0) {
            continue;
        }
        states.push(read_state(dir));
        let now = read_u64(dir, "energy_now").or_else(|| read_u64(dir, "charge_now"));
        let full = read_u64(dir, "energy_full").or_else(|| read_u64(dir, "charge_full"));
        match (now, full) {
            (Some(n), Some(f)) if f > 0 => {
                now_total = now_total.saturating_add(n);
                full_total = full_total.saturating_add(f);
            }
            // No energy counters: some firmware exposes only `capacity`.
            // Keep it as a fallback rather than dropping the battery.
            _ => {
                if let Some(c) = read_u64(dir, "capacity") {
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
}

fn read_u64(dir: &Path, attr: &str) -> Option<u64> {
    read_trimmed(dir, attr)?.parse().ok()
}

/// Map the kernel's `status` string onto the UPower-shaped enum the rest
/// of the crate speaks.
fn read_state(dir: &Path) -> BatteryState {
    match read_trimmed(dir, "status").as_deref() {
        Some("Charging") => BatteryState::Charging,
        Some("Discharging") => BatteryState::Discharging,
        Some("Full") => BatteryState::Full,
        Some("Not charging") => BatteryState::PendingCharge,
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
    fn sysfs_supplies_report_absence_mains_and_percentage_sources() {
        // Phase `a_machine_with_no_power_supply_class_reports_absent`.
        {
            let snap = snapshot_in(Path::new("/definitely/not/a/path"));
            assert!(!snap.present);
            assert_eq!(snap.state, BatteryState::Unknown);
        }
        // Phase `a_desktop_with_only_a_mains_supply_reports_absent_and_not_on_battery`.
        {
            let t = tree(&[("AC", &[("type", "Mains\n"), ("online", "1\n")])]);
            assert!(!snapshot_in(t.path()).present);
            assert!(!on_battery_in(t.path()));
        }
        // Phase `energy_counters_drive_the_percentage`.
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
        // Phase `charge_counters_are_accepted_when_energy_is_absent`.
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
        // Phase `capacity_is_the_fallback_when_no_counters_exist`.
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
        // Phase `two_packs_aggregate_by_energy_not_by_averaging_percentages`.
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
        // Phase `an_absent_pack_in_a_dual_bay_is_skipped`.
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
        // Phase `charging_outranks_discharging_in_the_composite_state`.
        {
            assert_eq!(
                composite_state(&[BatteryState::Discharging, BatteryState::Charging]),
                BatteryState::Charging
            );
        }
        // Phase `unplugged_mains_means_on_battery`.
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
}
