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

/// When a source backs itself up.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    OnDemand,
    Hourly,
    Daily,
    Weekly,
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
    pub enabled: bool,
    pub health: Health,
}

/// One entry in a snapshot's file index, as the restore preview shows it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SnapFile {
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
