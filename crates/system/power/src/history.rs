//! Privacy-minimal, bounded power history shared by the compositor sampler
//! and Settings reader.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_AGE_MINUTES: u64 = 30 * 24 * 60;
/// The history's resolution: at most one point per this many minutes.
pub const MIN_INTERVAL_MINUTES: u64 = 15;
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
    Some(history_path_in(&tilo_paths::xdg::state_home()?, kind))
}

fn history_path_in(state_home: &Path, kind: PowerSourceKind) -> PathBuf {
    let name = match kind {
        PowerSourceKind::Battery => "power-history.json",
        PowerSourceKind::Ups => "ups-history.json",
    };
    state_home.join("tilo").join(name)
}

/// The power service's history writer. It reads each history file once,
/// keeps it, and rewrites a file only when a point lands in it — so sampling
/// costs no disk I/O between points.
#[derive(Debug)]
pub struct PowerHistoryWriter {
    state_home: Option<PathBuf>,
    battery: Option<BatteryHistory>,
    ups: Option<BatteryHistory>,
}

impl Default for PowerHistoryWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl PowerHistoryWriter {
    /// Write under the user's private state directory.
    pub fn new() -> Self {
        Self::in_state_home(tilo_paths::xdg::state_home())
    }

    /// Write under `state_home` (`None`: nowhere; every record fails).
    pub fn in_state_home(state_home: Option<PathBuf>) -> Self {
        Self {
            state_home,
            battery: None,
            ups: None,
        }
    }

    /// Record a point for `kind` at `point.unix_minute` when the cadence
    /// allows it; `Ok(true)` when it landed and the file was rewritten.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no private state directory or the file
    /// cannot be written; the point stays in memory and reaches the disk with
    /// the next one that lands.
    pub fn record(
        &mut self,
        kind: PowerSourceKind,
        point: BatteryHistoryPoint,
    ) -> Result<bool, String> {
        let path = self
            .state_home
            .as_deref()
            .map(|home| history_path_in(home, kind))
            .ok_or("no private per-user state directory is available")?;
        let history = self.loaded(kind, &path);
        if !history.record(point) {
            return Ok(false);
        }
        write_history_at(&path, history)?;
        Ok(true)
    }

    /// The first minute at which a point for `kind` can land, or `None`
    /// before one has been recorded or read.
    pub fn next_due_minute(&self, kind: PowerSourceKind) -> Option<u64> {
        let history = match kind {
            PowerSourceKind::Battery => self.battery.as_ref(),
            PowerSourceKind::Ups => self.ups.as_ref(),
        }?;
        history
            .points
            .last()
            .map(|last| last.unix_minute.saturating_add(MIN_INTERVAL_MINUTES))
    }

    fn loaded(&mut self, kind: PowerSourceKind, path: &Path) -> &mut BatteryHistory {
        let slot = match kind {
            PowerSourceKind::Battery => &mut self.battery,
            PowerSourceKind::Ups => &mut self.ups,
        };
        slot.get_or_insert_with(|| load_history_at(path))
    }
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
    write_history_at(path, &history)?;
    Ok(true)
}

fn write_history_at(path: &Path, history: &BatteryHistory) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(history).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
        .map_err(|error| error.to_string())
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

    #[test]
    fn the_writer_reads_once_and_rewrites_only_when_a_point_lands() {
        let dir = tempfile::tempdir().unwrap();
        let path = history_path_in(dir.path(), PowerSourceKind::Battery);
        let point = |unix_minute| BatteryHistoryPoint {
            unix_minute,
            capacity: 60,
            health: None,
            on_ac: false,
        };
        // A point from an earlier session is on disk.
        assert!(record_power_history_at(&path, point(1_000)).unwrap());

        let mut writer = PowerHistoryWriter::in_state_home(Some(dir.path().to_path_buf()));
        assert_eq!(writer.next_due_minute(PowerSourceKind::Battery), None);
        assert!(
            !writer
                .record(PowerSourceKind::Battery, point(1_005))
                .unwrap(),
            "the earlier session's point sets the cadence"
        );
        assert_eq!(
            writer.next_due_minute(PowerSourceKind::Battery),
            Some(1_000 + MIN_INTERVAL_MINUTES)
        );

        // Kept in memory: a file that vanished is not re-read, and a sample
        // that does not land writes nothing.
        std::fs::remove_file(&path).unwrap();
        assert!(
            !writer
                .record(PowerSourceKind::Battery, point(1_010))
                .unwrap()
        );
        assert!(!path.exists(), "no point landed, so nothing was written");

        assert!(
            writer
                .record(PowerSourceKind::Battery, point(1_015))
                .unwrap()
        );
        let written: BatteryHistory =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written
                .points
                .iter()
                .map(|p| p.unix_minute)
                .collect::<Vec<_>>(),
            vec![1_000, 1_015],
            "the rewrite carries the history read at start plus the new point"
        );
        assert_eq!(
            writer.next_due_minute(PowerSourceKind::Battery),
            Some(1_015 + MIN_INTERVAL_MINUTES)
        );
        assert_eq!(writer.next_due_minute(PowerSourceKind::Ups), None);

        let mut nowhere = PowerHistoryWriter::in_state_home(None);
        assert!(nowhere.record(PowerSourceKind::Ups, point(1)).is_err());
    }
}
