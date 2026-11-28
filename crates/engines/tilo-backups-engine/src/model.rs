//! What the device's backup repository holds, as the app reads it: the folder it protects,
//! where its snapshots are written, and the snapshots a retention policy keeps.

use serde::{Deserialize, Serialize};

/// What a source is: as much as the app says without naming a path.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Home,
    System,
    Workspace,
    Custom,
}

impl SourceKind {
    /// A path-free label for shared search and automation surfaces: a local path can expose
    /// account names or mounts, so it stays in this device's UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home folder",
            Self::System => "System files",
            Self::Workspace => "Workspace",
            Self::Custom => "Custom folder",
        }
    }
}

/// When a source backs itself up, and at what time of day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Schedule {
    /// When the person runs one.
    #[default]
    OnDemand,
    /// Every hour at `minute` past.
    Hourly { minute: u32 },
    /// Every day at `hour:minute`.
    Daily { hour: u32, minute: u32 },
    /// Every `weekday` at `hour:minute`.
    Weekly {
        weekday: Weekday,
        hour: u32,
        minute: u32,
    },
}

impl Schedule {
    /// What the schedule is called, as the catalog names it.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::OnDemand => "on_demand",
            Self::Hourly { .. } => "hourly",
            Self::Daily { .. } => "daily",
            Self::Weekly { .. } => "weekly",
        }
    }

    /// The time of day it runs at, `(hour, minute)`; none when it runs by hand.
    #[must_use]
    pub const fn at(self) -> Option<(u32, u32)> {
        match self {
            Self::OnDemand => None,
            Self::Hourly { minute } => Some((0, minute)),
            Self::Daily { hour, minute } | Self::Weekly { hour, minute, .. } => {
                Some((hour, minute))
            }
        }
    }

    /// The day a weekly schedule runs on.
    #[must_use]
    pub const fn weekday(self) -> Option<Weekday> {
        match self {
            Self::Weekly { weekday, .. } => Some(weekday),
            _ => None,
        }
    }

    /// The schedule a stored `kind` names, at the time and day given; unknown kinds run by hand.
    #[must_use]
    pub const fn from_parts(kind: &str, hour: u32, minute: u32, weekday: Weekday) -> Self {
        match kind.as_bytes() {
            b"hourly" => Self::Hourly {
                minute: if minute < 60 { minute } else { 0 },
            },
            b"daily" => Self::Daily {
                hour: if hour < 24 { hour } else { 0 },
                minute: if minute < 60 { minute } else { 0 },
            },
            b"weekly" => Self::Weekly {
                weekday,
                hour: if hour < 24 { hour } else { 0 },
                minute: if minute < 60 { minute } else { 0 },
            },
            _ => Self::OnDemand,
        }
    }

    /// The next second since 1970 this schedule runs at or after `now`.
    #[must_use]
    pub fn next_after(self, now: u64) -> Option<u64> {
        let (hour, minute) = self.at()?;
        let day_start = now.checked_sub(now.checked_rem(86_400)?)?;
        let seconds = u64::from(hour)
            .checked_mul(3_600)?
            .checked_add(u64::from(minute).checked_mul(60)?)?;
        match self {
            Self::OnDemand => None,
            Self::Hourly { .. } => {
                let hour_start = now.checked_sub(now.checked_rem(3_600)?)?;
                let at = hour_start.checked_add(seconds)?;
                Some(if at > now {
                    at
                } else {
                    hour_start.checked_add(3_600)?.checked_add(seconds)?
                })
            }
            Self::Daily { .. } => {
                let at = day_start.checked_add(seconds)?;
                Some(if at > now {
                    at
                } else {
                    day_start.checked_add(86_400)?.checked_add(seconds)?
                })
            }
            Self::Weekly { weekday, .. } => {
                let today = day_start
                    .checked_div(86_400)?
                    .checked_add(3)?
                    .checked_rem(7)?;
                let ahead = u64::from(weekday.number())
                    .checked_add(7)?
                    .checked_sub(today)?
                    .checked_rem(7)?;
                let mut when = day_start
                    .checked_add(ahead.checked_mul(86_400)?)?
                    .checked_add(seconds)?;
                if when <= now {
                    when = when.checked_add(7 * 86_400)?;
                }
                Some(when)
            }
        }
    }
}

impl Serialize for Schedule {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.kind())
    }
}

impl<'de> Deserialize<'de> for Schedule {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let kind = String::deserialize(deserializer)?;
        Ok(Self::from_parts(&kind, 0, 0, Weekday::Monday))
    }
}

/// The day a weekly schedule runs on, Monday first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Weekday {
    #[default]
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    /// The day's number, Monday as zero.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }

    /// The day a number names, Monday as zero.
    #[must_use]
    pub const fn from_number(number: u8) -> Self {
        match number {
            1 => Self::Tuesday,
            2 => Self::Wednesday,
            3 => Self::Thursday,
            4 => Self::Friday,
            5 => Self::Saturday,
            6 => Self::Sunday,
            _ => Self::Monday,
        }
    }
}

/// Was the snapshot created by the scheduler or by hand?
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapType {
    Auto,
    Manual,
    Unknown,
}

impl SnapType {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Manual => "Manual",
            Self::Unknown => "Unknown",
        }
    }
}

/// The retention class the scheduler put a snapshot in.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Keep,
    Unknown,
}

impl Retention {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hourly => "Hourly",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Keep => "Until removed",
            Self::Unknown => "Unknown",
        }
    }
}

/// Whether a stored snapshot is whole, being checked or known broken.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapStatus {
    Complete,
    Verifying,
    Failed,
}

impl SnapStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Complete => "Complete",
            Self::Verifying => "Verifying",
            Self::Failed => "Failed",
        }
    }
}

/// Where snapshots are written.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestKind {
    LocalFolder,
    LocalBtrfs,
    External,
    RemoteRestic,
}

impl DestKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LocalFolder => "Local folder",
            Self::LocalBtrfs => "Btrfs pool on this device",
            Self::External => "External USB drive",
            Self::RemoteRestic => "Offsite restic repository",
        }
    }
}

/// Whether a source or destination needs a look.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    Warning,
    Unknown,
}

/// The folder this device protects.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: SourceKind,
    pub schedule: Schedule,
    /// The clock time it runs at, as a person reads it (`18:00`); empty when it runs by hand.
    #[serde(default)]
    pub at: String,
    /// The day a weekly source runs on (`Monday`); empty otherwise.
    #[serde(default)]
    pub weekday: String,
    /// The next second since 1970 this source backs itself up, where it runs on a schedule.
    #[serde(default)]
    pub next_run_unix_seconds: Option<u64>,
    pub enabled: bool,
    pub health: Health,
}

/// One entry in a snapshot's file index, as the restore preview shows it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SnapFile {
    /// The entry's path inside the snapshot: what tells a folder from another of its name, and
    /// what a window folds a folder's contents away by.
    #[serde(default)]
    pub path: String,
    pub depth: usize,
    pub name: String,
    /// The file's size; zero for a folder.
    pub size: u64,
    pub is_dir: bool,
}

/// One stored snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub id: String,
    pub source_id: String,
    pub dest_id: String,
    /// Seconds since 1970: what the app turns into a time on this device.
    pub taken_unix_seconds: u64,
    /// The name a person gave it, empty for none.
    pub label: String,
    pub kind: SnapType,
    pub retention: Retention,
    pub size_bytes: u64,
    pub delta_bytes: u64,
    pub files_changed: u32,
    pub status: SnapStatus,
    /// Why the backup failed, empty when it did not.
    pub failure: String,
    pub files: Vec<SnapFile>,
}

impl Snapshot {
    /// The state of the snapshot, as a search hit words it.
    #[must_use]
    pub fn status_label(&self) -> &'static str {
        match self.status {
            SnapStatus::Complete => "complete",
            SnapStatus::Verifying => "verifying",
            SnapStatus::Failed => "failed",
        }
    }
}

/// Where snapshots are written.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Destination {
    pub id: String,
    pub name: String,
    pub kind: DestKind,
    pub used_bytes: u64,
    /// The drive's capacity; zero when it does not say.
    pub total_bytes: u64,
    pub encrypted: bool,
    pub health: Health,
}

impl Destination {
    /// The share of the destination in use, `0..=1`, when its capacity is known.
    #[allow(
        clippy::cast_precision_loss,
        reason = "a share of a drive read to size a bar, where an exact byte count is not needed"
    )]
    #[must_use]
    pub fn used_fraction(&self) -> Option<f64> {
        if self.total_bytes == 0 {
            return None;
        }
        let total = self.total_bytes;
        Some((self.used_bytes as f64 / total as f64).clamp(0.0, 1.0))
    }
}

/// Retention policy: counts to keep per class.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RetentionPolicy {
    /// Hourly snapshots kept
    pub hourly: u32,
    /// Daily snapshots kept
    pub daily: u32,
    /// Weekly snapshots kept
    pub weekly: u32,
    /// Monthly snapshots kept
    pub monthly: u32,
}

pub const MAX_KEPT: u32 = 999;

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            hourly: 24,
            daily: 7,
            weekly: 4,
            monthly: 12,
        }
    }
}

impl RetentionPolicy {
    /// No class keeps more than [`MAX_KEPT`] snapshots.
    pub fn valid(&self) -> Result<(), String> {
        if [self.hourly, self.daily, self.weekly, self.monthly]
            .into_iter()
            .any(|kept| kept > MAX_KEPT)
        {
            return Err(format!(
                "a retention class keeps at most {MAX_KEPT} snapshots"
            ));
        }
        Ok(())
    }

    /// The most snapshots the policy keeps per source, pins aside.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "four counts the policy reads, each capped at MAX_KEPT"
    )]
    #[must_use]
    pub fn total(&self) -> u32 {
        self.hourly + self.daily + self.weekly + self.monthly
    }
}

/// Everything a window shows of the device's repository.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Catalog {
    pub sources: Vec<Source>,
    pub snapshots: Vec<Snapshot>,
    pub destinations: Vec<Destination>,
}

impl Catalog {
    #[must_use]
    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|source| source.id == id)
    }

    #[must_use]
    pub fn destination(&self, id: &str) -> Option<&Destination> {
        self.destinations.iter().find(|dest| dest.id == id)
    }

    #[must_use]
    pub fn snapshot(&self, id: &str) -> Option<&Snapshot> {
        self.snapshots.iter().find(|snapshot| snapshot.id == id)
    }

    #[must_use]
    pub fn protected_bytes(&self, source_id: &str) -> u64 {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.source_id == source_id)
            .max_by_key(|snapshot| snapshot.taken_unix_seconds)
            .map_or(0, |snapshot| snapshot.size_bytes)
    }

    #[must_use]
    pub fn total_protected(&self) -> u64 {
        self.sources
            .iter()
            .map(|source| self.protected_bytes(&source.id))
            .sum()
    }

    #[must_use]
    pub fn failed(&self) -> usize {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.status == SnapStatus::Failed)
            .count()
    }

    #[must_use]
    pub fn last_backup(&self) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .filter(|snapshot| snapshot.status == SnapStatus::Complete)
            .max_by_key(|snapshot| snapshot.taken_unix_seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A daily schedule runs at its own time of day, and a weekly one on its own day.
    #[test]
    fn a_schedule_runs_at_its_time_and_day() {
        let now = 1_782_000_000;
        let daily = Schedule::Daily {
            hour: 18,
            minute: 0,
        };
        let next = daily.next_after(now).expect("a daily run");
        assert!(next > now);
        assert_eq!(
            next.checked_rem(86_400).expect("a day's remainder"),
            18 * 3_600,
            "at six in the evening"
        );
        let weekly = Schedule::Weekly {
            weekday: Weekday::Friday,
            hour: 4,
            minute: 0,
        };
        let next = weekly.next_after(now).expect("a weekly run");
        assert!(next > now && next.checked_sub(now).expect("a later run") <= 7 * 86_400);
        assert_eq!(
            next.checked_div(86_400)
                .expect("a day")
                .checked_add(3)
                .expect("shifted")
                .checked_rem(7)
                .expect("a weekday"),
            4,
            "on a Friday"
        );
        assert_eq!(
            next.checked_rem(86_400).expect("a day's remainder"),
            4 * 3_600,
            "at four in the morning"
        );
        assert_eq!(Schedule::OnDemand.next_after(now), None);
        assert_eq!(
            Schedule::from_parts("daily", 18, 0, Weekday::Monday).at(),
            Some((18, 0))
        );
        assert_eq!(
            Schedule::from_parts("sometimes", 1, 2, Weekday::Monday),
            Schedule::OnDemand
        );
    }
}
