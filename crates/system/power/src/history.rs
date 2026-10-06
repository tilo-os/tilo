//! Privacy-minimal, bounded power history shared by the compositor sampler
//! and Settings reader.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_AGE_MINUTES: u64 = 30 * 24 * 60;
const MIN_INTERVAL_MINUTES: u64 = 15;
const MAX_POINTS: usize = 30 * 24 * 4;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PowerSourceKind {
    #[default]
    Battery,
    Ups,
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
    /// Add a sample if the cadence allows it. Returns whether durable state
    /// changed, so a frequent `UPower` poll does not become a frequent disk
    /// write.
    pub fn record(&mut self, point: BatteryHistoryPoint) -> bool {
        self.points
            .retain(|p| point.unix_minute.saturating_sub(p.unix_minute) <= MAX_AGE_MINUTES);
        if self
            .points
            .last()
            .is_some_and(|p| point.unix_minute.saturating_sub(p.unix_minute) < MIN_INTERVAL_MINUTES)
        {
            return false;
        }
        self.points.push(point);
        if self.points.len() > MAX_POINTS {
            self.points
                .drain(..self.points.len().saturating_sub(MAX_POINTS));
        }
        true
    }
}

pub fn load_battery_history() -> BatteryHistory {
    history_path(PowerSourceKind::Battery)
        .map(|path| load_history_at(&path))
        .unwrap_or_default()
}

pub fn load_ups_history() -> BatteryHistory {
    history_path(PowerSourceKind::Ups)
        .map(|path| load_history_at(&path))
        .unwrap_or_default()
}

/// # Errors
///
/// Returns an error if no private per-user state directory is available (no `XDG_STATE_HOME`
/// and no resolvable home directory), or if serialising or writing the history file fails.
pub fn record_power_history(
    kind: PowerSourceKind,
    capacity: u32,
    health: Option<u32>,
    on_ac: bool,
) -> Result<bool, String> {
    let now = tilo_clock::unix_now() / 60;
    let path = history_path(kind).ok_or("no private per-user state directory is available")?;
    record_power_history_at(
        &path,
        BatteryHistoryPoint {
            unix_minute: now,
            capacity: capacity.min(100),
            health: health.map(|value| value.min(100)),
            on_ac,
        },
    )
}

fn history_path(kind: PowerSourceKind) -> Option<PathBuf> {
    let base = xdg_paths::xdg::state_home()?;
    let name = match kind {
        PowerSourceKind::Battery => "power-history.json",
        PowerSourceKind::Ups => "ups-history.json",
    };
    Some(base.join("tilo").join(name))
}

fn load_history_at(path: &Path) -> BatteryHistory {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn record_power_history_at(path: &Path, point: BatteryHistoryPoint) -> Result<bool, String> {
    let mut history = load_history_at(path);
    if !history.record(point) {
        return Ok(false);
    }
    let mut bytes = serde_json::to_vec(&history).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_private_atomic_and_anonymous() {
        // Phase `cadence_and_retention_are_bounded`.
        {
            let mut history = BatteryHistory::default();
            assert!(history.record(BatteryHistoryPoint {
                unix_minute: 100,
                capacity: 50,
                health: Some(90),
                on_ac: false,
            }));
            assert!(!history.record(BatteryHistoryPoint {
                unix_minute: 105,
                capacity: 49,
                health: Some(90),
                on_ac: false,
            }));
            assert_eq!(history.points.len(), 1);
            for minute in (120..=50_000).step_by(15) {
                history.record(BatteryHistoryPoint {
                    unix_minute: minute,
                    capacity: 50,
                    health: None,
                    on_ac: true,
                });
            }
            assert!(history.points.len() <= MAX_POINTS);
            assert!(
                history.points.iter().all(|point| {
                    50_000u64.saturating_sub(point.unix_minute) <= MAX_AGE_MINUTES
                })
            );
        }
        // Phase `durable_file_is_private_atomic_and_schema_contains_no_identity`.
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tilo/history.json");
            assert!(
                record_power_history_at(
                    &path,
                    BatteryHistoryPoint {
                        unix_minute: 100,
                        capacity: 72,
                        health: Some(88),
                        on_ac: false,
                    }
                )
                .unwrap()
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let text = std::fs::read_to_string(path).unwrap();
            assert!(!text.contains("serial"));
            assert!(!text.contains("application"));
        }
    }
}
