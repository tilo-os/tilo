//! Pure data model for the Backups app.
//!
//! The *catalog* (sources, snapshots, destinations) comes from the local
//! checksummed repository in the live app.
//! Future providers can project btrfs or remote repositories here too. For the
//! offline `--shots` build (and as the demo seed) we synthesise a week of
//! activity in [`Catalog::sample`].
//!
//! User-editable / collaborative state — which view is open, which source &
//! snapshot are selected, whether the restore sheet is up, and the retention
//! policy — lives in the Automerge document (see `doc.rs`), rather than here.

/// The four top-level screens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Timeline,
    Sources,
    Destinations,
    Schedules,
}

impl View {
    pub const ALL: [View; 4] = [
        View::Timeline,
        View::Sources,
        View::Destinations,
        View::Schedules,
    ];

    pub fn title(self) -> &'static str {
        match self {
            View::Timeline => "Timeline",
            View::Sources => "Sources",
            View::Destinations => "Destinations",
            View::Schedules => "Schedules",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            View::Timeline => "timeline",
            View::Sources => "sources",
            View::Destinations => "destinations",
            View::Schedules => "schedules",
        }
    }

    pub fn from_key(s: &str) -> View {
        match s {
            "sources" => View::Sources,
            "destinations" => View::Destinations,
            "schedules" => View::Schedules,
            _ => View::Timeline,
        }
    }
}

/// What kind of thing a source is — drives the icon + default schedule.
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

/// How often a source is captured automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cadence {
    OnDemand,
    Hourly,
    Daily,
    Weekly,
}

impl Cadence {
    pub fn label(self) -> &'static str {
        match self {
            Cadence::OnDemand => "On demand",
            Cadence::Hourly => "Hourly",
            Cadence::Daily => "Daily",
            Cadence::Weekly => "Weekly",
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
            SnapType::Auto => "Auto",
            SnapType::Manual => "Manual",
        }
    }
}

/// Retention class a snapshot is currently held under. A snapshot promotes
/// through these as it ages (hourly → daily → weekly → monthly); `Keep` is a
/// user pin that exempts it from pruning entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Keep,
}

impl Retention {
    pub fn label(self) -> &'static str {
        match self {
            Retention::Hourly => "hourly",
            Retention::Daily => "daily",
            Retention::Weekly => "weekly",
            Retention::Monthly => "monthly",
            Retention::Keep => "kept",
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
            DestKind::LocalBtrfs => "Local · btrfs",
            DestKind::External => "External · USB",
            DestKind::RemoteRestic => "Offsite · restic",
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
    pub path: String,
    pub kind: SourceKind,
    pub cadence: Cadence,
    pub enabled: bool,
    /// Human label for the next scheduled run, e.g. "Today 18:00".
    pub next_run: String,
    pub health: Health,
}

/// One entry in a snapshot's file index, as shown in the restore browser.
/// `depth` is the indent level; a directory carries no size label.
#[derive(Clone, Debug)]
pub struct SnapFile {
    pub depth: usize,
    pub name: String,
    pub size: String,
    pub is_dir: bool,
}

/// A representative sample listing for a source, used only by the demo
/// catalog so restore-browser screenshots have content. The live catalog
/// leaves `Snapshot::files` empty.
fn sample_files(source_id: &str) -> Vec<SnapFile> {
    let rows: &[(usize, &str, &str, bool)] = match source_id {
        "workspace" => &[
            (0, "workspace/", "", true),
            (1, "shared/", "6.4 GB", true),
            (2, "design.excalidraw", "8.1 MB", false),
            (1, "tilo/", "3.2 GB", true),
            (2, "src/", "1.9 GB", true),
            (1, "notes/", "180 MB", true),
            (2, "meeting-2026-07.qdoc", "2.4 MB", false),
        ],
        "etc" => &[
            (0, "etc/", "", true),
            (1, "ssh/", "48 KB", true),
            (2, "sshd_config", "6.1 KB", false),
            (1, "NetworkManager/", "112 KB", true),
            (1, "fstab", "1.3 KB", false),
            (1, "hostname", "24 B", false),
        ],
        // Home and any unknown source fall back to a home-shaped tree.
        _ => &[
            (0, "home/", "", true),
            (1, "Documents/", "2.1 GB", true),
            (2, "report-q2.qdoc", "4.2 MB", false),
            (1, "Pictures/", "18.4 GB", true),
            (2, "2026-trip/", "3.1 GB", true),
            (1, "Projects/", "9.8 GB", true),
            (2, "tilo/", "1.2 GB", true),
            (1, ".config/", "240 MB", false),
        ],
    };
    rows.iter()
        .map(|(depth, name, size, is_dir)| SnapFile {
            depth: *depth,
            name: (*name).into(),
            size: (*size).into(),
            is_dir: *is_dir,
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: String,
    pub source_id: String,
    pub dest_id: String,
    /// 0 == today, 1 == yesterday, …
    pub day: u32,
    /// Clock label, e.g. "03:00".
    pub time: String,
    pub kind: SnapType,
    pub retention: Retention,
    /// Total on-disk size the snapshot pins.
    pub size_bytes: u64,
    /// New/changed bytes captured in this snapshot (delta from parent).
    pub delta_bytes: u64,
    pub files_changed: u32,
    pub status: SnapStatus,
    /// The snapshot's indexed contents, shown in the restore browser. Empty
    /// until the repository manifest is read. The local engine records every
    /// entry while copying, so its snapshots populate this list.
    pub files: Vec<SnapFile>,
}

/// Named input for demo snapshots. Keeping the seed shape separate from the
/// runtime model makes fixture rows compact without relying on a long,
/// transposition-prone positional argument list.
struct SampleSnapshot<'a> {
    id: &'a str,
    source_id: &'a str,
    dest_id: &'a str,
    day: u32,
    time: &'a str,
    kind: SnapType,
    retention: Retention,
    size_bytes: u64,
    delta_bytes: u64,
    files_changed: u32,
    status: SnapStatus,
}

impl From<SampleSnapshot<'_>> for Snapshot {
    fn from(seed: SampleSnapshot<'_>) -> Self {
        Self {
            id: seed.id.into(),
            source_id: seed.source_id.into(),
            dest_id: seed.dest_id.into(),
            day: seed.day,
            time: seed.time.into(),
            kind: seed.kind,
            retention: seed.retention,
            size_bytes: seed.size_bytes,
            delta_bytes: seed.delta_bytes,
            files_changed: seed.files_changed,
            status: seed.status,
            files: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Destination {
    pub id: String,
    pub name: String,
    pub kind: DestKind,
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub encrypted: bool,
    pub last_sync: String,
    pub health: Health,
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

#[derive(Clone, Debug)]
pub struct Catalog {
    pub sources: Vec<Source>,
    pub snapshots: Vec<Snapshot>,
    pub destinations: Vec<Destination>,
}

const GB: u64 = 1024 * 1024 * 1024;
const MB: u64 = 1024 * 1024;
const TB: u64 = 1024 * GB;

impl Catalog {
    /// Project a real local repository into the dashboard model.
    pub fn from_local(
        source: &std::path::Path,
        destination: &std::path::Path,
        manifests: &[crate::engine::BackupManifest],
    ) -> Self {
        let source_label = source.to_string_lossy().into_owned();
        let destination_label = destination.file_name().map_or_else(
            || destination.to_string_lossy().into_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let snapshots = manifests
            .iter()
            .map(|manifest| Snapshot {
                id: manifest.id.clone(),
                source_id: "local-source".into(),
                dest_id: "local-destination".into(),
                day: 0,
                time: "saved".into(),
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
                        size: if matches!(entry.kind, crate::engine::EntryKind::File) {
                            ui::format::bytes(entry.size)
                        } else {
                            String::new()
                        },
                        is_dir: matches!(entry.kind, crate::engine::EntryKind::Directory),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let used = manifests.iter().map(|manifest| manifest.bytes).sum::<u64>();
        Self {
            sources: vec![Source {
                id: "local-source".into(),
                path: source_label,
                kind: SourceKind::Custom,
                cadence: Cadence::OnDemand,
                enabled: true,
                next_run: "On demand".into(),
                health: Health::Ok,
            }],
            snapshots,
            destinations: vec![Destination {
                id: "local-destination".into(),
                name: destination_label,
                kind: DestKind::LocalFolder,
                used_bytes: used,
                total_bytes: used.saturating_mul(2).max(1),
                encrypted: false,
                last_sync: "local".into(),
                health: Health::Ok,
            }],
        }
    }

    /// An empty catalog — no sources, snapshots, or destinations. This is the
    /// real first-run state: the live app starts here and shows onboarding
    /// until the user selects a source and destination.
    pub fn empty() -> Self {
        Self {
            sources: Vec::new(),
            snapshots: Vec::new(),
            destinations: Vec::new(),
        }
    }

    /// Synthesise a realistic week of backups across three sources and three
    /// destinations. Deterministic, so the offline screenshot is stable.
    pub fn sample() -> Self {
        let sources = vec![
            Source {
                id: "home".into(),
                path: "/home".into(),
                kind: SourceKind::Home,
                cadence: Cadence::Daily,
                enabled: true,
                next_run: "Today 18:00".into(),
                health: Health::Ok,
            },
            Source {
                id: "etc".into(),
                path: "/etc".into(),
                kind: SourceKind::System,
                cadence: Cadence::Weekly,
                enabled: true,
                next_run: "Sun 04:00".into(),
                health: Health::Ok,
            },
            Source {
                id: "workspace".into(),
                path: tilo_paths::WORKSPACE_MOUNT.into(),
                kind: SourceKind::Workspace,
                cadence: Cadence::Hourly,
                enabled: true,
                next_run: "In 38 min".into(),
                health: Health::Warning,
            },
        ];

        let destinations = vec![
            Destination {
                id: "pool".into(),
                name: "Internal pool".into(),
                kind: DestKind::LocalBtrfs,
                used_bytes: 312 * GB,
                total_bytes: 500 * GB,
                encrypted: true,
                last_sync: "live".into(),
                health: Health::Ok,
            },
            Destination {
                id: "usb".into(),
                name: "Seagate 2TB".into(),
                kind: DestKind::External,
                used_bytes: 1280 * GB,
                total_bytes: 2 * TB,
                encrypted: true,
                last_sync: "2h ago".into(),
                health: Health::Ok,
            },
            Destination {
                id: "b2".into(),
                name: "Backblaze B2".into(),
                kind: DestKind::RemoteRestic,
                used_bytes: 88 * GB,
                total_bytes: 200 * GB,
                encrypted: true,
                last_sync: "14 min ago".into(),
                health: Health::Warning,
            },
        ];

        // Build a week of snapshots, split across a few helpers to keep this
        // constructor under the line budget; each pushes one day's worth.
        let mut snaps: Vec<Snapshot> = Vec::new();
        Self::sample_snapshots_today(&mut snaps);
        Self::sample_snapshots_days_1_to_3(&mut snaps);
        Self::sample_snapshots_days_4_to_6(&mut snaps);

        // Attach a representative indexed listing per source, so the restore
        // browser has real (sample) contents to show in screenshots.
        for s in &mut snaps {
            s.files = sample_files(&s.source_id);
        }

        Self {
            sources,
            snapshots: snaps,
            destinations,
        }
    }

    /// Today's (day 0) sample snapshots. Split out of [`Self::sample`] to
    /// keep it under the line budget.
    fn sample_snapshots_today(snaps: &mut Vec<Snapshot>) {
        use Retention::{Daily, Hourly, Keep};
        use SnapStatus::{Complete, Verifying};
        use SnapType::{Auto, Manual};

        snaps.extend(
            [
                SampleSnapshot {
                    id: "w-0-1700",
                    source_id: "workspace",
                    dest_id: "pool",
                    day: 0,
                    time: "17:00",
                    kind: Auto,
                    retention: Hourly,
                    size_bytes: 9 * GB + 700 * MB,
                    delta_bytes: 240 * MB,
                    files_changed: 312,
                    status: Verifying,
                },
                SampleSnapshot {
                    id: "w-0-1600",
                    source_id: "workspace",
                    dest_id: "pool",
                    day: 0,
                    time: "16:00",
                    kind: Auto,
                    retention: Hourly,
                    size_bytes: 9 * GB + 500 * MB,
                    delta_bytes: 180 * MB,
                    files_changed: 211,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "m-0-1422",
                    source_id: "home",
                    dest_id: "pool",
                    day: 0,
                    time: "14:22",
                    kind: Manual,
                    retention: Keep,
                    size_bytes: 48 * GB + 200 * MB,
                    delta_bytes: GB + 100 * MB,
                    files_changed: 1840,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "w-0-1500",
                    source_id: "workspace",
                    dest_id: "pool",
                    day: 0,
                    time: "15:00",
                    kind: Auto,
                    retention: Hourly,
                    size_bytes: 9 * GB + 300 * MB,
                    delta_bytes: 96 * MB,
                    files_changed: 142,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "h-0-0300",
                    source_id: "home",
                    dest_id: "pool",
                    day: 0,
                    time: "03:00",
                    kind: Auto,
                    retention: Daily,
                    size_bytes: 47 * GB + 800 * MB,
                    delta_bytes: 820 * MB,
                    files_changed: 1320,
                    status: Complete,
                },
            ]
            .map(Snapshot::from),
        );
    }

    /// Days 1–3 ago sample snapshots. Split out of [`Self::sample`] to keep
    /// it under the line budget.
    fn sample_snapshots_days_1_to_3(snaps: &mut Vec<Snapshot>) {
        Self::sample_snapshots_day_1(snaps);
        Self::sample_snapshots_days_2_to_3(snaps);
    }

    /// Yesterday's (day 1) sample snapshots. Split out of
    /// [`Self::sample_snapshots_days_1_to_3`] to keep it under the line
    /// budget.
    fn sample_snapshots_day_1(snaps: &mut Vec<Snapshot>) {
        use Retention::Daily;
        use SnapStatus::Complete;
        use SnapType::Auto;

        snaps.extend(
            [
                SampleSnapshot {
                    id: "h-1-0300",
                    source_id: "home",
                    dest_id: "pool",
                    day: 1,
                    time: "03:00",
                    kind: Auto,
                    retention: Daily,
                    size_bytes: 47 * GB,
                    delta_bytes: 640 * MB,
                    files_changed: 980,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "w-1-0900",
                    source_id: "workspace",
                    dest_id: "usb",
                    day: 1,
                    time: "09:00",
                    kind: Auto,
                    retention: Daily,
                    size_bytes: 9 * GB,
                    delta_bytes: 120 * MB,
                    files_changed: 88,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "e-1-0400",
                    source_id: "etc",
                    dest_id: "pool",
                    day: 1,
                    time: "04:00",
                    kind: Auto,
                    retention: Daily,
                    size_bytes: 220 * MB,
                    delta_bytes: 4 * MB,
                    files_changed: 23,
                    status: Complete,
                },
            ]
            .map(Snapshot::from),
        );
    }

    /// Days 2–3 ago sample snapshots. Split out of
    /// [`Self::sample_snapshots_days_1_to_3`] to keep it under the line
    /// budget.
    fn sample_snapshots_days_2_to_3(snaps: &mut Vec<Snapshot>) {
        use Retention::{Daily, Keep, Weekly};
        use SnapStatus::Complete;
        use SnapType::{Auto, Manual};

        snaps.extend(
            [
                SampleSnapshot {
                    id: "m-2-0910",
                    source_id: "home",
                    dest_id: "usb",
                    day: 2,
                    time: "09:10",
                    kind: Manual,
                    retention: Keep,
                    size_bytes: 46 * GB + 500 * MB,
                    delta_bytes: 2 * GB,
                    files_changed: 3210,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "h-2-0300",
                    source_id: "home",
                    dest_id: "pool",
                    day: 2,
                    time: "03:00",
                    kind: Auto,
                    retention: Daily,
                    size_bytes: 46 * GB,
                    delta_bytes: 700 * MB,
                    files_changed: 1110,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "h-3-0300",
                    source_id: "home",
                    dest_id: "pool",
                    day: 3,
                    time: "03:00",
                    kind: Auto,
                    retention: Weekly,
                    size_bytes: 45 * GB + 200 * MB,
                    delta_bytes: 590 * MB,
                    files_changed: 870,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "e-3-0400",
                    source_id: "etc",
                    dest_id: "b2",
                    day: 3,
                    time: "04:00",
                    kind: Auto,
                    retention: Weekly,
                    size_bytes: 215 * MB,
                    delta_bytes: 11 * MB,
                    files_changed: 41,
                    status: Complete,
                },
            ]
            .map(Snapshot::from),
        );
    }

    /// Days 4–6 ago sample snapshots. Split out of [`Self::sample`] to keep
    /// it under the line budget.
    fn sample_snapshots_days_4_to_6(snaps: &mut Vec<Snapshot>) {
        use Retention::{Daily, Monthly, Weekly};
        use SnapStatus::{Complete, Failed};
        use SnapType::Auto;

        snaps.extend(
            [
                SampleSnapshot {
                    id: "h-4-0300",
                    source_id: "home",
                    dest_id: "pool",
                    day: 4,
                    time: "03:00",
                    kind: Auto,
                    retention: Weekly,
                    size_bytes: 44 * GB + 900 * MB,
                    delta_bytes: 510 * MB,
                    files_changed: 760,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "w-4-1200",
                    source_id: "workspace",
                    dest_id: "b2",
                    day: 4,
                    time: "12:00",
                    kind: Auto,
                    retention: Daily,
                    size_bytes: 8 * GB + 800 * MB,
                    delta_bytes: 70 * MB,
                    files_changed: 51,
                    status: Failed,
                },
                SampleSnapshot {
                    id: "h-5-0300",
                    source_id: "home",
                    dest_id: "pool",
                    day: 5,
                    time: "03:00",
                    kind: Auto,
                    retention: Monthly,
                    size_bytes: 44 * GB,
                    delta_bytes: 480 * MB,
                    files_changed: 690,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "h-6-0300",
                    source_id: "home",
                    dest_id: "usb",
                    day: 6,
                    time: "03:00",
                    kind: Auto,
                    retention: Monthly,
                    size_bytes: 43 * GB + 600 * MB,
                    delta_bytes: 450 * MB,
                    files_changed: 610,
                    status: Complete,
                },
                SampleSnapshot {
                    id: "e-6-0400",
                    source_id: "etc",
                    dest_id: "b2",
                    day: 6,
                    time: "04:00",
                    kind: Auto,
                    retention: Monthly,
                    size_bytes: 210 * MB,
                    delta_bytes: 9 * MB,
                    files_changed: 33,
                    status: Complete,
                },
            ]
            .map(Snapshot::from),
        );
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

    /// Snapshots for `source_id` (or all when `None`), newest first.
    /// Ordering: smaller day first, then later clock time first.
    pub fn timeline(&self, source_id: Option<&str>) -> Vec<&Snapshot> {
        let mut v: Vec<&Snapshot> = self
            .snapshots
            .iter()
            .filter(|s| source_id.is_none_or(|id| s.source_id == id))
            .collect();
        v.sort_by(|a, b| a.day.cmp(&b.day).then(b.time.cmp(&a.time)));
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
            .map(|s| s.size_bytes)
            .max()
            .unwrap_or(0)
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

    pub fn day_label(day: u32) -> String {
        match day {
            0 => "Today".to_string(),
            1 => "Yesterday".to_string(),
            n => format!("{n} days ago"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo catalog is populated and internally consistent.
    #[test]
    fn sample_catalog_is_populated_with_unique_known_references() {
        sample_is_populated();
        sample_snapshots_reference_unique_known_entities();
    }

    /// Case of `sample_catalog_is_populated_with_unique_known_references`.
    fn sample_is_populated() {
        let c = Catalog::sample();
        assert_eq!(c.sources.len(), 3);
        assert_eq!(c.destinations.len(), 3);
        assert!(c.snapshots.len() >= 15);
    }

    /// Case of `sample_catalog_is_populated_with_unique_known_references`.
    fn sample_snapshots_reference_unique_known_entities() {
        use std::collections::HashSet;

        let catalog = Catalog::sample();
        let mut ids = HashSet::new();
        for snapshot in &catalog.snapshots {
            assert!(
                ids.insert(&snapshot.id),
                "duplicate snapshot {}",
                snapshot.id
            );
            assert!(
                catalog.source(&snapshot.source_id).is_some(),
                "snapshot {} references missing source {}",
                snapshot.id,
                snapshot.source_id
            );
            assert!(
                catalog.destination(&snapshot.dest_id).is_some(),
                "snapshot {} references missing destination {}",
                snapshot.id,
                snapshot.dest_id
            );
            assert!(snapshot.delta_bytes <= snapshot.size_bytes);
            assert!(!snapshot.files.is_empty());
        }
    }

    /// Per-source timelines sort newest first, the unfiltered timeline covers
    /// every snapshot, per-source totals count and weigh, and sizes render in
    /// IEC units.
    #[test]
    fn timelines_and_totals_filter_by_source_and_render_iec_sizes() {
        timeline_filters_and_sorts();
        all_timeline_is_superset();
        count_and_protected();
        snapshot_sizes_render_with_iec_binary_units();
    }

    /// Case of `timelines_and_totals_filter_by_source_and_render_iec_sizes`.
    fn timeline_filters_and_sorts() {
        let c = Catalog::sample();
        let home = c.timeline(Some("home"));
        assert!(home.iter().all(|s| s.source_id == "home"));
        // newest (day 0) first
        assert_eq!(home.first().unwrap().day, 0);
        // sorted non-decreasing by day
        for w in home.windows(2) {
            assert!(w[0].day <= w[1].day);
        }
    }

    /// Case of `timelines_and_totals_filter_by_source_and_render_iec_sizes`.
    fn all_timeline_is_superset() {
        let c = Catalog::sample();
        assert_eq!(c.timeline(None).len(), c.snapshots.len());
    }

    /// Case of `timelines_and_totals_filter_by_source_and_render_iec_sizes`.
    fn snapshot_sizes_render_with_iec_binary_units() {
        // Base-1024, so labels must be IEC (KiB/MiB/GiB/TiB), not base-1000.
        assert_eq!(ui::format::bytes(4 * MB), "4.0 MiB");
        assert_eq!(ui::format::bytes(512 * GB), "512.0 GiB");
        assert_eq!(ui::format::bytes(2 * TB), "2.0 TiB");
        assert_eq!(ui::format::bytes(9 * GB), "9.0 GiB");
    }

    /// Case of `timelines_and_totals_filter_by_source_and_render_iec_sizes`.
    fn count_and_protected() {
        let c = Catalog::sample();
        assert!(c.count_for("home") > 0);
        assert!(c.protected_bytes("home") > c.protected_bytes("etc"));
    }
}
