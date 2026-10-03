//! Pure data model for the Backups app.
//!
//! The *catalog* (sources, snapshots, destinations) comes from the local
//! checksummed repository in the live app. Future providers can project btrfs
//! or remote repositories here too. Screenshot captures stage the sample
//! catalog from [`Catalog::sample`]: a week of activity laid out backwards
//! from the clock, so every "last backup" and "next run" it shows is true of
//! the moment it is drawn.
//!
//! Every time here is local wall time (`NaiveDateTime`), read against
//! [`ui::clock::local_now`] where the app needs "now".

use chrono::{Datelike, Duration, NaiveDateTime, Timelike, Weekday};

/// What kind of thing a source is: drives its icon and its kind label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Home,
    System,
    Workspace,
    Custom,
}

impl SourceKind {
    /// A path-free label suitable for shared search and automation surfaces.
    /// Local paths can expose account names or mount details, so they remain in
    /// the device-local visual UI and are never published as a resource label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home folder",
            Self::System => "System files",
            Self::Workspace => "Workspace",
            Self::Custom => "Custom folder",
        }
    }
}

/// When a source is captured automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schedule {
    /// Only when the person asks.
    OnDemand,
    /// Every hour at `minute`.
    Hourly { minute: u32 },
    /// Every day at `hour:minute`.
    Daily { hour: u32, minute: u32 },
    /// Every week on `weekday` at `hour:minute`.
    Weekly {
        weekday: Weekday,
        hour: u32,
        minute: u32,
    },
}

impl Schedule {
    /// The cadence in one word.
    pub fn label(self) -> &'static str {
        match self {
            Self::OnDemand => "On demand",
            Self::Hourly { .. } => "Hourly",
            Self::Daily { .. } => "Daily",
            Self::Weekly { .. } => "Weekly",
        }
    }

    /// The next run strictly after `now`, or `None` for an on-demand source.
    pub fn next_after(self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        let at =
            |date: chrono::NaiveDate, hour: u32, minute: u32| date.and_hms_opt(hour, minute, 0);
        match self {
            Self::OnDemand => None,
            Self::Hourly { minute } => {
                let this_hour = at(now.date(), now.hour(), minute)?;
                Some(if this_hour > now {
                    this_hour
                } else {
                    this_hour + Duration::hours(1)
                })
            }
            Self::Daily { hour, minute } => {
                let today = at(now.date(), hour, minute)?;
                Some(if today > now {
                    today
                } else {
                    today + Duration::days(1)
                })
            }
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => (0..=7).find_map(|ahead| {
                let date = now.date() + Duration::days(ahead);
                let run = at(date, hour, minute)?;
                (date.weekday() == weekday && run > now).then_some(run)
            }),
        }
    }
}

/// Was the snapshot created by the scheduler or by hand?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapType {
    Auto,
    Manual,
}

impl SnapType {
    pub fn label(self) -> &'static str {
        match self {
            SnapType::Auto => "Automatic",
            SnapType::Manual => "Manual",
        }
    }
}

/// Retention class a snapshot is currently held under. A snapshot promotes
/// through these as it ages (hourly → daily → weekly → monthly); `Keep` is a
/// pin that exempts it from pruning entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Keep,
}

impl Retention {
    /// How the snapshot is kept, as a sentence-case phrase.
    pub fn label(self) -> &'static str {
        match self {
            Retention::Hourly => "Hourly",
            Retention::Daily => "Daily",
            Retention::Weekly => "Weekly",
            Retention::Monthly => "Monthly",
            Retention::Keep => "Until removed",
        }
    }
}

/// Completion state of a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapStatus {
    Complete,
    Verifying,
    Failed,
}

impl SnapStatus {
    pub fn label(self) -> &'static str {
        match self {
            SnapStatus::Complete => "Complete",
            SnapStatus::Verifying => "Verifying",
            SnapStatus::Failed => "Failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestKind {
    LocalFolder,
    LocalBtrfs,
    External,
    RemoteRestic,
}

impl DestKind {
    pub fn label(self) -> &'static str {
        match self {
            DestKind::LocalFolder => "Local folder",
            DestKind::LocalBtrfs => "Btrfs pool on this device",
            DestKind::External => "External USB drive",
            DestKind::RemoteRestic => "Offsite restic repository",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Ok,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    /// What the person calls it ("Home", "Workspace").
    pub name: String,
    pub path: String,
    pub kind: SourceKind,
    pub schedule: Schedule,
    pub enabled: bool,
    pub health: Health,
}

/// One entry in a snapshot's file index, as shown in the restore preview.
/// `depth` is the indent level; a directory carries the bytes beneath it.
#[derive(Clone, Debug)]
pub struct SnapFile {
    pub depth: usize,
    pub name: String,
    pub size: Option<u64>,
    pub is_dir: bool,
}

/// A representative listing for a sample source, so restore previews in
/// captures have content. The live catalog reads its listing from the
/// repository manifest.
fn sample_files(source_id: &str) -> Vec<SnapFile> {
    let rows: &[(usize, &str, u64, bool)] = match source_id {
        "workspace" => &[
            (0, "workspace", 9_700 * MB, true),
            (1, "shared", 6_400 * MB, true),
            (2, "design-review.excalidraw", 8_100 * KB, false),
            (2, "brand", 2_100 * MB, true),
            (1, "tilo", 3_200 * MB, true),
            (2, "src", 1_900 * MB, true),
            (1, "notes", 180 * MB, true),
            (2, "planning-2026-q3.md", 2_400 * KB, false),
        ],
        "etc" => &[
            (0, "etc", 215 * MB, true),
            (1, "ssh", 48 * KB, true),
            (2, "sshd_config", 6_100, false),
            (1, "NetworkManager", 112 * KB, true),
            (1, "fstab", 1_300, false),
            (1, "hostname", 24, false),
        ],
        // Home and any unknown source fall back to a home-shaped tree.
        _ => &[
            (0, "home", 48_200 * MB, true),
            (1, "Documents", 2_100 * MB, true),
            (2, "Quarterly report.odt", 4_200 * KB, false),
            (1, "Pictures", 18_400 * MB, true),
            (2, "Lisbon 2026", 3_100 * MB, true),
            (1, "Projects", 9_800 * MB, true),
            (2, "garden-planner", 1_200 * MB, true),
            (1, ".config", 240 * MB, true),
        ],
    };
    rows.iter()
        .map(|(depth, name, size, is_dir)| SnapFile {
            depth: *depth,
            name: (*name).into(),
            size: Some(*size),
            is_dir: *is_dir,
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: String,
    pub source_id: String,
    pub dest_id: String,
    /// When the snapshot was taken, local time.
    pub taken: NaiveDateTime,
    /// The name a person gave a manual snapshot ("Before system upgrade").
    pub label: Option<String>,
    pub kind: SnapType,
    pub retention: Retention,
    /// Total on-disk size the snapshot pins.
    pub size_bytes: u64,
    /// New/changed bytes captured in this snapshot (delta from parent).
    pub delta_bytes: u64,
    pub files_changed: u32,
    pub status: SnapStatus,
    /// The snapshot's indexed contents, shown in the restore preview. Empty
    /// until the repository manifest is read. The local engine records every
    /// entry while copying, so its snapshots populate this list.
    pub files: Vec<SnapFile>,
}

/// Named input for sample snapshots, placed `minutes_ago` before the clock.
/// Keeping the seed shape separate from the runtime model makes fixture rows
/// compact without relying on a long, transposition-prone positional list.
struct SampleSnapshot<'a> {
    id: &'a str,
    source_id: &'a str,
    dest_id: &'a str,
    minutes_ago: i64,
    label: Option<&'a str>,
    kind: SnapType,
    retention: Retention,
    size_bytes: u64,
    delta_bytes: u64,
    files_changed: u32,
    status: SnapStatus,
}

impl SampleSnapshot<'_> {
    fn at(self, now: NaiveDateTime) -> Snapshot {
        Snapshot {
            id: self.id.into(),
            source_id: self.source_id.into(),
            dest_id: self.dest_id.into(),
            taken: now - Duration::minutes(self.minutes_ago),
            label: self.label.map(Into::into),
            kind: self.kind,
            retention: self.retention,
            size_bytes: self.size_bytes,
            delta_bytes: self.delta_bytes,
            files_changed: self.files_changed,
            status: self.status,
            files: sample_files(self.source_id),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Destination {
    pub id: String,
    pub name: String,
    pub kind: DestKind,
    pub used_bytes: u64,
    /// The destination's capacity, when its provider reports one.
    pub total_bytes: Option<u64>,
    pub encrypted: bool,
    pub health: Health,
}

impl Destination {
    /// The share of the destination in use, `0..=1`, when its capacity is
    /// known.
    pub fn used_fraction(&self) -> Option<f64> {
        let total = self.total_bytes.filter(|total| *total > 0)?;
        Some((self.used_bytes as f64 / total as f64).clamp(0.0, 1.0))
    }
}

/// Retention policy (counts to keep per class). Lives in the CRDT so edits
/// undo/redo and replicate; seeded from these defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub hourly: u32,
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}

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
    /// The most snapshots the policy keeps per source, pins aside.
    pub fn total(&self) -> u32 {
        self.hourly + self.daily + self.weekly + self.monthly
    }
}

#[derive(Clone, Debug)]
pub struct Catalog {
    pub sources: Vec<Source>,
    pub snapshots: Vec<Snapshot>,
    pub destinations: Vec<Destination>,
}

const KB: u64 = 1_000;
const MB: u64 = 1_000 * KB;
const GB: u64 = 1_000 * MB;
const TB: u64 = 1_000 * GB;

/// Local wall time of a Unix timestamp.
fn local_time(unix_seconds: u64) -> Option<NaiveDateTime> {
    let seconds = i64::try_from(unix_seconds).ok()?;
    let utc = chrono::DateTime::from_timestamp(seconds, 0)?;
    Some(utc.with_timezone(&chrono::Local).naive_local())
}

impl Catalog {
    /// Project a real local repository into the dashboard model.
    pub fn from_local(
        source: &std::path::Path,
        destination: &std::path::Path,
        manifests: &[crate::engine::BackupManifest],
    ) -> Self {
        let source_label = source.to_string_lossy().into_owned();
        let source_name = source.file_name().map_or_else(
            || source_label.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let destination_label = destination.file_name().map_or_else(
            || destination.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let now = ui::clock::local_now().naive_local();
        let snapshots = manifests
            .iter()
            .map(|manifest| Snapshot {
                id: manifest.id.clone(),
                source_id: "local-source".into(),
                dest_id: "local-destination".into(),
                taken: local_time(manifest.created_unix_seconds).unwrap_or(now),
                label: None,
                kind: SnapType::Manual,
                retention: Retention::Daily,
                size_bytes: manifest.bytes,
                delta_bytes: manifest.bytes,
                files_changed: manifest.files.min(u64::from(u32::MAX)) as u32,
                status: SnapStatus::Complete,
                files: manifest
                    .entries
                    .iter()
                    .map(|entry| SnapFile {
                        depth: entry.path.components().count().saturating_sub(1),
                        name: entry
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        size: matches!(entry.kind, crate::engine::EntryKind::File)
                            .then_some(entry.size),
                        is_dir: matches!(entry.kind, crate::engine::EntryKind::Directory),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let used = manifests.iter().map(|manifest| manifest.bytes).sum::<u64>();
        Self {
            sources: vec![Source {
                id: "local-source".into(),
                name: source_name,
                path: source_label,
                kind: SourceKind::Custom,
                schedule: Schedule::OnDemand,
                enabled: true,
                health: Health::Ok,
            }],
            snapshots,
            destinations: vec![Destination {
                id: "local-destination".into(),
                name: destination_label,
                kind: DestKind::LocalFolder,
                used_bytes: used,
                // A local folder reports no capacity of its own.
                total_bytes: None,
                encrypted: false,
                health: Health::Ok,
            }],
        }
    }

    /// An empty catalog — no sources, snapshots, or destinations. This is the
    /// real first-run state: the live app starts here and shows onboarding
    /// until the person selects a source and destination.
    pub fn empty() -> Self {
        Self {
            sources: Vec::new(),
            snapshots: Vec::new(),
            destinations: Vec::new(),
        }
    }

    /// A realistic week of backups across three sources and three
    /// destinations, laid out backwards from `now`. Deterministic for a given
    /// clock, so a capture (whose clock is pinned) is stable.
    pub fn sample(now: NaiveDateTime) -> Self {
        let sources = vec![
            Source {
                id: "home".into(),
                name: "Home".into(),
                path: "/home".into(),
                kind: SourceKind::Home,
                schedule: Schedule::Daily {
                    hour: 18,
                    minute: 0,
                },
                enabled: true,
                health: Health::Ok,
            },
            Source {
                id: "etc".into(),
                name: "System settings".into(),
                path: "/etc".into(),
                kind: SourceKind::System,
                schedule: Schedule::Weekly {
                    weekday: Weekday::Sun,
                    hour: 4,
                    minute: 0,
                },
                enabled: true,
                health: Health::Ok,
            },
            Source {
                id: "workspace".into(),
                name: "Workspace".into(),
                path: tilo_paths::WORKSPACE_MOUNT.into(),
                kind: SourceKind::Workspace,
                schedule: Schedule::Hourly { minute: 0 },
                enabled: true,
                health: Health::Warning,
            },
        ];

        let destinations = vec![
            Destination {
                id: "pool".into(),
                name: "Internal pool".into(),
                kind: DestKind::LocalBtrfs,
                used_bytes: 312 * GB,
                total_bytes: Some(500 * GB),
                encrypted: true,
                health: Health::Ok,
            },
            Destination {
                id: "usb".into(),
                name: "Seagate 2 TB".into(),
                kind: DestKind::External,
                used_bytes: 1_740 * GB,
                total_bytes: Some(2 * TB),
                encrypted: true,
                health: Health::Ok,
            },
            Destination {
                id: "b2".into(),
                name: "Backblaze B2".into(),
                kind: DestKind::RemoteRestic,
                used_bytes: 88 * GB,
                total_bytes: Some(200 * GB),
                encrypted: true,
                health: Health::Warning,
            },
        ];

        let snapshots = sample_snapshots()
            .into_iter()
            .map(|seed| seed.at(now))
            .collect();

        Self {
            sources,
            snapshots,
            destinations,
        }
    }

    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn destination(&self, id: &str) -> Option<&Destination> {
        self.destinations.iter().find(|d| d.id == id)
    }

    pub fn snapshot(&self, id: &str) -> Option<&Snapshot> {
        self.snapshots.iter().find(|s| s.id == id)
    }

    /// Snapshots matching `filter`, newest first.
    pub fn timeline(&self, filter: Filter<'_>) -> Vec<&Snapshot> {
        let mut v: Vec<&Snapshot> = self.snapshots.iter().filter(|s| filter.admits(s)).collect();
        v.sort_by(|a, b| b.taken.cmp(&a.taken).then_with(|| a.id.cmp(&b.id)));
        v
    }

    pub fn count_for(&self, source_id: &str) -> usize {
        self.snapshots
            .iter()
            .filter(|s| s.source_id == source_id)
            .count()
    }

    /// Total bytes pinned by the newest snapshot of a source (a proxy for the
    /// live subvolume size).
    pub fn protected_bytes(&self, source_id: &str) -> u64 {
        self.snapshots
            .iter()
            .filter(|s| s.source_id == source_id)
            .max_by_key(|s| s.taken)
            .map_or(0, |s| s.size_bytes)
    }

    pub fn total_snapshots(&self) -> usize {
        self.snapshots.len()
    }

    pub fn total_protected(&self) -> u64 {
        self.sources
            .iter()
            .map(|s| self.protected_bytes(&s.id))
            .sum()
    }

    /// Snapshots under `filter` that failed.
    pub fn failed(&self, filter: Filter<'_>) -> usize {
        self.snapshots
            .iter()
            .filter(|s| filter.admits(s) && s.status == SnapStatus::Failed)
            .count()
    }

    /// The newest completed snapshot under `filter`.
    pub fn last_backup(&self, filter: Filter<'_>) -> Option<&Snapshot> {
        self.snapshots
            .iter()
            .filter(|s| filter.admits(s) && s.status == SnapStatus::Complete)
            .max_by_key(|s| s.taken)
    }

    /// The next scheduled run of any enabled source under `filter`.
    pub fn next_run(&self, filter: Filter<'_>, now: NaiveDateTime) -> Option<NaiveDateTime> {
        self.sources
            .iter()
            .filter(|s| s.enabled && filter.admits_source(&s.id))
            .filter_map(|s| s.schedule.next_after(now))
            .min()
    }
}

/// Which snapshots a page lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter<'a> {
    All,
    Source(&'a str),
    Destination(&'a str),
}

impl Filter<'_> {
    pub fn admits(self, snapshot: &Snapshot) -> bool {
        match self {
            Filter::All => true,
            Filter::Source(id) => snapshot.source_id == id,
            Filter::Destination(id) => snapshot.dest_id == id,
        }
    }

    fn admits_source(self, source_id: &str) -> bool {
        match self {
            Filter::All | Filter::Destination(_) => true,
            Filter::Source(id) => source_id == id,
        }
    }
}

/// A week of sample snapshots. Offsets are minutes before the clock; at the
/// pinned capture time (10:42) they land on the hour the scheduler would
/// have used.
#[allow(
    clippy::too_many_lines,
    reason = "one readable fixture table; splitting it by day only scatters it"
)]
fn sample_snapshots() -> Vec<SampleSnapshot<'static>> {
    use Retention::{Daily, Hourly, Keep, Monthly, Weekly};
    use SnapStatus::{Complete, Failed, Verifying};
    use SnapType::{Auto, Manual};
    const HOUR: i64 = 60;
    const DAY: i64 = 24 * HOUR;
    let auto =
        |id, source_id, dest_id, minutes_ago, retention, size, delta, files| SampleSnapshot {
            id,
            source_id,
            dest_id,
            minutes_ago,
            label: None,
            kind: Auto,
            retention,
            size_bytes: size,
            delta_bytes: delta,
            files_changed: files,
            status: Complete,
        };
    vec![
        // Today: the workspace every hour, and a pinned manual snapshot taken
        // before an upgrade.
        SampleSnapshot {
            status: Verifying,
            ..auto(
                "w-0-1000",
                "workspace",
                "pool",
                42,
                Hourly,
                9_700 * MB,
                240 * MB,
                312,
            )
        },
        auto(
            "w-0-0900",
            "workspace",
            "pool",
            42 + HOUR,
            Hourly,
            9_500 * MB,
            180 * MB,
            211,
        ),
        SampleSnapshot {
            label: Some("Before system upgrade"),
            kind: Manual,
            ..auto(
                "m-0-0824",
                "home",
                "pool",
                138,
                Keep,
                48_200 * MB,
                1_100 * MB,
                1_840,
            )
        },
        auto(
            "w-0-0800",
            "workspace",
            "pool",
            42 + 2 * HOUR,
            Hourly,
            9_300 * MB,
            96 * MB,
            142,
        ),
        auto(
            "w-0-0700",
            "workspace",
            "pool",
            42 + 3 * HOUR,
            Hourly,
            9_200 * MB,
            64 * MB,
            97,
        ),
        // Yesterday: the nightly home run and the workspace's offsite copy.
        auto(
            "h-1-1800",
            "home",
            "pool",
            DAY - 7 * HOUR - 18,
            Daily,
            47_800 * MB,
            820 * MB,
            1_320,
        ),
        auto(
            "w-1-1200",
            "workspace",
            "b2",
            DAY - HOUR - 18,
            Daily,
            9_000 * MB,
            120 * MB,
            88,
        ),
        // Sunday: the weekly system run and photos pinned after an import.
        auto(
            "h-2-1800",
            "home",
            "pool",
            2 * DAY - 7 * HOUR - 18,
            Daily,
            47_000 * MB,
            640 * MB,
            980,
        ),
        SampleSnapshot {
            label: Some("Photos imported"),
            kind: Manual,
            ..auto(
                "m-2-0910",
                "home",
                "usb",
                2 * DAY + HOUR + 32,
                Keep,
                46_500 * MB,
                2_000 * MB,
                3_210,
            )
        },
        auto(
            "e-2-0400",
            "etc",
            "pool",
            2 * DAY + 6 * HOUR + 42,
            Weekly,
            220 * MB,
            4 * MB,
            23,
        ),
        // Earlier in the week; Saturday's offsite copy failed.
        auto(
            "h-3-1800",
            "home",
            "pool",
            3 * DAY - 7 * HOUR - 18,
            Daily,
            46_000 * MB,
            700 * MB,
            1_110,
        ),
        SampleSnapshot {
            status: Failed,
            ..auto(
                "w-3-1200",
                "workspace",
                "b2",
                3 * DAY - HOUR - 18,
                Daily,
                8_800 * MB,
                70 * MB,
                51,
            )
        },
        auto(
            "h-4-1800",
            "home",
            "pool",
            4 * DAY - 7 * HOUR - 18,
            Weekly,
            45_200 * MB,
            590 * MB,
            870,
        ),
        auto(
            "h-4-1830",
            "home",
            "usb",
            4 * DAY - 7 * HOUR - 48,
            Weekly,
            45_200 * MB,
            590 * MB,
            870,
        ),
        auto(
            "h-5-1800",
            "home",
            "pool",
            5 * DAY - 7 * HOUR - 18,
            Weekly,
            44_900 * MB,
            510 * MB,
            760,
        ),
        auto(
            "h-6-1800",
            "home",
            "pool",
            6 * DAY - 7 * HOUR - 18,
            Monthly,
            44_000 * MB,
            480 * MB,
            690,
        ),
        auto(
            "e-9-0400",
            "etc",
            "b2",
            9 * DAY + 6 * HOUR + 42,
            Monthly,
            215 * MB,
            11 * MB,
            41,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_now() -> NaiveDateTime {
        let (y, mo, d, h, mi) = ui::clock::CAPTURE_LOCAL_TIME;
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|date| date.and_hms_opt(h, mi, 0))
            .expect("the capture time is a valid date")
    }

    /// The sample catalog is populated and internally consistent, and every
    /// snapshot lies in the past of the clock it was laid out from.
    #[test]
    fn sample_catalog_is_populated_with_unique_known_references() {
        use std::collections::HashSet;

        let now = capture_now();
        let catalog = Catalog::sample(now);
        assert_eq!(catalog.sources.len(), 3);
        assert_eq!(catalog.destinations.len(), 3);
        assert!(catalog.snapshots.len() >= 15);
        let mut ids = HashSet::new();
        for snapshot in &catalog.snapshots {
            assert!(ids.insert(&snapshot.id), "duplicate {}", snapshot.id);
            assert!(catalog.source(&snapshot.source_id).is_some());
            assert!(catalog.destination(&snapshot.dest_id).is_some());
            assert!(snapshot.delta_bytes <= snapshot.size_bytes);
            assert!(!snapshot.files.is_empty());
            assert!(snapshot.taken < now, "{} is in the future", snapshot.id);
        }
    }

    /// Timelines filter and sort newest first; the summary figures derive
    /// from the data and the schedules rather than from fixed copy.
    #[test]
    fn timelines_and_summaries_derive_from_the_catalog() {
        let now = capture_now();
        let c = Catalog::sample(now);
        let home = c.timeline(Filter::Source("home"));
        assert!(home.iter().all(|s| s.source_id == "home"));
        for pair in home.windows(2) {
            assert!(pair[0].taken >= pair[1].taken);
        }
        assert_eq!(c.timeline(Filter::All).len(), c.snapshots.len());
        assert!(
            c.timeline(Filter::Destination("b2"))
                .iter()
                .all(|s| s.dest_id == "b2")
        );
        assert!(c.protected_bytes("home") > c.protected_bytes("etc"));

        // The newest completed snapshot, not the one still verifying.
        let last = c.last_backup(Filter::All).expect("a completed snapshot");
        assert_eq!(last.id, "w-0-0900");
        assert_eq!(c.failed(Filter::All), 1);
        assert_eq!(c.failed(Filter::Source("home")), 0);

        // The hourly workspace run is next at 11:00; home runs at 18:00.
        let next = c.next_run(Filter::All, now).expect("a scheduled run");
        assert_eq!((next.hour(), next.minute()), (11, 0));
        let home_next = c.next_run(Filter::Source("home"), now).expect("daily");
        assert_eq!((home_next.hour(), home_next.date()), (18, now.date()));
    }

    /// A weekly schedule finds the next matching weekday, never the same
    /// instant.
    #[test]
    fn weekly_schedule_runs_on_its_weekday() {
        let now = capture_now(); // Tuesday 30 June 2026
        let sunday = Schedule::Weekly {
            weekday: Weekday::Sun,
            hour: 4,
            minute: 0,
        }
        .next_after(now)
        .expect("weekly");
        assert_eq!(sunday.weekday(), Weekday::Sun);
        assert!(sunday > now && sunday - now < Duration::days(7));
        assert_eq!(Schedule::OnDemand.next_after(now), None);
    }
}
