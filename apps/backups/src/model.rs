//! Pure data model for the Backups app.
//!
//! The *catalog* (sources, snapshots, destinations) represents the ground
//! truth that, in the live app, comes from the storage backend — btrfs /
//! snapper subvolume listings or a `restic snapshots` JSON dump. For the
//! offline `--shots` build (and as the demo seed) we synthesise a realistic
//! week of activity in [`Catalog::sample`].
//!
//! User-editable / collaborative state — which view is open, which source &
//! snapshot are selected, whether the restore sheet is up, and the retention
//! policy — lives in the Automerge document (see `doc.rs`), NOT here.

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

/// How often a source is captured automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Cadence {
    Hourly,
    Daily,
    Weekly,
}

impl Cadence {
    pub fn label(self) -> &'static str {
        match self {
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
    LocalBtrfs,
    External,
    RemoteRestic,
}

impl DestKind {
    pub fn label(self) -> &'static str {
        match self {
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
            (1, "qos/", "3.2 GB", true),
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
            (2, "quickdesktop/", "1.2 GB", true),
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
    /// until a real backend indexes the snapshot — the live app has no
    /// indexer yet, so real snapshots present an "index unavailable" state
    /// rather than fabricated files. The sample catalog fills this in.
    pub files: Vec<SnapFile>,
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
    /// An empty catalog — no sources, snapshots, or destinations. This is the
    /// real first-run state: the live app starts here and shows onboarding
    /// until the user adds a source + destination (a real btrfs/restic backend
    /// is a later task).
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
                path: "/var/workspace".into(),
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

    /// Push one sample [`Snapshot`] built from its flattened fields — the
    /// single place [`Self::sample`]'s per-day helpers construct one.
    #[allow(clippy::too_many_arguments)]
    fn push_snapshot(
        snaps: &mut Vec<Snapshot>,
        id: &str,
        src: &str,
        dest: &str,
        day: u32,
        time: &str,
        kind: SnapType,
        ret: Retention,
        size: u64,
        delta: u64,
        files: u32,
        status: SnapStatus,
    ) {
        snaps.push(Snapshot {
            id: id.into(),
            source_id: src.into(),
            dest_id: dest.into(),
            day,
            time: time.into(),
            kind,
            retention: ret,
            size_bytes: size,
            delta_bytes: delta,
            files_changed: files,
            status,
            files: Vec::new(),
        });
    }

    /// Today's (day 0) sample snapshots. Split out of [`Self::sample`] to
    /// keep it under the line budget.
    fn sample_snapshots_today(snaps: &mut Vec<Snapshot>) {
        use Retention::{Daily, Hourly, Keep};
        use SnapStatus::{Complete, Verifying};
        use SnapType::{Auto, Manual};

        Self::push_snapshot(
            snaps,
            "w-0-1700",
            "workspace",
            "pool",
            0,
            "17:00",
            Auto,
            Hourly,
            9 * GB + 700 * MB,
            240 * MB,
            312,
            Verifying,
        );
        Self::push_snapshot(
            snaps,
            "w-0-1600",
            "workspace",
            "pool",
            0,
            "16:00",
            Auto,
            Hourly,
            9 * GB + 500 * MB,
            180 * MB,
            211,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "m-0-1422",
            "home",
            "pool",
            0,
            "14:22",
            Manual,
            Keep,
            48 * GB + 200 * MB,
            GB + 100 * MB,
            1840,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "w-0-1500",
            "workspace",
            "pool",
            0,
            "15:00",
            Auto,
            Hourly,
            9 * GB + 300 * MB,
            96 * MB,
            142,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "h-0-0300",
            "home",
            "pool",
            0,
            "03:00",
            Auto,
            Daily,
            47 * GB + 800 * MB,
            820 * MB,
            1320,
            Complete,
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

        Self::push_snapshot(
            snaps,
            "h-1-0300",
            "home",
            "pool",
            1,
            "03:00",
            Auto,
            Daily,
            47 * GB,
            640 * MB,
            980,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "w-1-0900",
            "workspace",
            "usb",
            1,
            "09:00",
            Auto,
            Daily,
            9 * GB,
            120 * MB,
            88,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "e-1-0400",
            "etc",
            "pool",
            1,
            "04:00",
            Auto,
            Daily,
            220 * MB,
            4 * MB,
            23,
            Complete,
        );
    }

    /// Days 2–3 ago sample snapshots. Split out of
    /// [`Self::sample_snapshots_days_1_to_3`] to keep it under the line
    /// budget.
    fn sample_snapshots_days_2_to_3(snaps: &mut Vec<Snapshot>) {
        use Retention::{Daily, Keep, Weekly};
        use SnapStatus::Complete;
        use SnapType::{Auto, Manual};

        // --- 2 days ago ---
        Self::push_snapshot(
            snaps,
            "m-2-0910",
            "home",
            "usb",
            2,
            "09:10",
            Manual,
            Keep,
            46 * GB + 500 * MB,
            2 * GB,
            3210,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "h-2-0300",
            "home",
            "pool",
            2,
            "03:00",
            Auto,
            Daily,
            46 * GB,
            700 * MB,
            1110,
            Complete,
        );

        // --- 3 days ago ---
        Self::push_snapshot(
            snaps,
            "h-3-0300",
            "home",
            "pool",
            3,
            "03:00",
            Auto,
            Weekly,
            45 * GB + 200 * MB,
            590 * MB,
            870,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "e-3-0400",
            "etc",
            "b2",
            3,
            "04:00",
            Auto,
            Weekly,
            215 * MB,
            11 * MB,
            41,
            Complete,
        );
    }

    /// Days 4–6 ago sample snapshots. Split out of [`Self::sample`] to keep
    /// it under the line budget.
    fn sample_snapshots_days_4_to_6(snaps: &mut Vec<Snapshot>) {
        use Retention::{Daily, Monthly, Weekly};
        use SnapStatus::{Complete, Failed};
        use SnapType::Auto;

        // --- 4 days ago ---
        Self::push_snapshot(
            snaps,
            "h-4-0300",
            "home",
            "pool",
            4,
            "03:00",
            Auto,
            Weekly,
            44 * GB + 900 * MB,
            510 * MB,
            760,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "w-4-1200",
            "workspace",
            "b2",
            4,
            "12:00",
            Auto,
            Daily,
            8 * GB + 800 * MB,
            70 * MB,
            51,
            Failed,
        );

        // --- 5 days ago ---
        Self::push_snapshot(
            snaps,
            "h-5-0300",
            "home",
            "pool",
            5,
            "03:00",
            Auto,
            Monthly,
            44 * GB,
            480 * MB,
            690,
            Complete,
        );

        // --- 6 days ago ---
        Self::push_snapshot(
            snaps,
            "h-6-0300",
            "home",
            "usb",
            6,
            "03:00",
            Auto,
            Monthly,
            43 * GB + 600 * MB,
            450 * MB,
            610,
            Complete,
        );
        Self::push_snapshot(
            snaps,
            "e-6-0400",
            "etc",
            "b2",
            6,
            "04:00",
            Auto,
            Monthly,
            210 * MB,
            9 * MB,
            33,
            Complete,
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

    #[test]
    fn sample_is_populated() {
        let c = Catalog::sample();
        assert_eq!(c.sources.len(), 3);
        assert_eq!(c.destinations.len(), 3);
        assert!(c.snapshots.len() >= 15);
    }

    #[test]
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

    #[test]
    fn all_timeline_is_superset() {
        let c = Catalog::sample();
        assert_eq!(c.timeline(None).len(), c.snapshots.len());
    }

    #[test]
    fn snapshot_sizes_render_with_iec_binary_units() {
        // Base-1024, so labels must be IEC (KiB/MiB/GiB/TiB), not base-1000.
        assert_eq!(ui::format::bytes(4 * MB), "4.0 MiB");
        assert_eq!(ui::format::bytes(512 * GB), "512.0 GiB");
        assert_eq!(ui::format::bytes(2 * TB), "2.0 TiB");
        assert_eq!(ui::format::bytes(9 * GB), "9.0 GiB");
    }

    #[test]
    fn count_and_protected() {
        let c = Catalog::sample();
        assert!(c.count_for("home") > 0);
        assert!(c.protected_bytes("home") > c.protected_bytes("etc"));
    }
}
