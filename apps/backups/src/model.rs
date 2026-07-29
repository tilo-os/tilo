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
        Self { hourly: 24, daily: 7, weekly: 4, monthly: 12 }
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

        // Build a week of snapshots. Helper closure keeps the table compact.
        let mut snaps: Vec<Snapshot> = Vec::new();
        let mut push = |id: &str,
                        src: &str,
                        dest: &str,
                        day: u32,
                        time: &str,
                        kind: SnapType,
                        ret: Retention,
                        size: u64,
                        delta: u64,
                        files: u32,
                        status: SnapStatus| {
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
            });
        };

        use Retention::*;
        use SnapStatus::*;
        use SnapType::*;

        // --- Today (day 0) ---
        push("w-0-1700", "workspace", "pool", 0, "17:00", Auto, Hourly, 9 * GB + 700 * MB, 240 * MB, 312, Verifying);
        push("w-0-1600", "workspace", "pool", 0, "16:00", Auto, Hourly, 9 * GB + 500 * MB, 180 * MB, 211, Complete);
        push("m-0-1422", "home", "pool", 0, "14:22", Manual, Keep, 48 * GB + 200 * MB, 1 * GB + 100 * MB, 1840, Complete);
        push("w-0-1500", "workspace", "pool", 0, "15:00", Auto, Hourly, 9 * GB + 300 * MB, 96 * MB, 142, Complete);
        push("h-0-0300", "home", "pool", 0, "03:00", Auto, Daily, 47 * GB + 800 * MB, 820 * MB, 1320, Complete);

        // --- Yesterday (day 1) ---
        push("h-1-0300", "home", "pool", 1, "03:00", Auto, Daily, 47 * GB, 640 * MB, 980, Complete);
        push("w-1-0900", "workspace", "usb", 1, "09:00", Auto, Daily, 9 * GB, 120 * MB, 88, Complete);
        push("e-1-0400", "etc", "pool", 1, "04:00", Auto, Daily, 220 * MB, 4 * MB, 23, Complete);

        // --- 2 days ago ---
        push("m-2-0910", "home", "usb", 2, "09:10", Manual, Keep, 46 * GB + 500 * MB, 2 * GB, 3210, Complete);
        push("h-2-0300", "home", "pool", 2, "03:00", Auto, Daily, 46 * GB, 700 * MB, 1110, Complete);

        // --- 3 days ago ---
        push("h-3-0300", "home", "pool", 3, "03:00", Auto, Weekly, 45 * GB + 200 * MB, 590 * MB, 870, Complete);
        push("e-3-0400", "etc", "b2", 3, "04:00", Auto, Weekly, 215 * MB, 11 * MB, 41, Complete);

        // --- 4 days ago ---
        push("h-4-0300", "home", "pool", 4, "03:00", Auto, Weekly, 44 * GB + 900 * MB, 510 * MB, 760, Complete);
        push("w-4-1200", "workspace", "b2", 4, "12:00", Auto, Daily, 8 * GB + 800 * MB, 70 * MB, 51, Failed);

        // --- 5 days ago ---
        push("h-5-0300", "home", "pool", 5, "03:00", Auto, Monthly, 44 * GB, 480 * MB, 690, Complete);

        // --- 6 days ago ---
        push("h-6-0300", "home", "usb", 6, "03:00", Auto, Monthly, 43 * GB + 600 * MB, 450 * MB, 610, Complete);
        push("e-6-0400", "etc", "b2", 6, "04:00", Auto, Monthly, 210 * MB, 9 * MB, 33, Complete);

        Self { sources, snapshots: snaps, destinations }
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
            .filter(|s| source_id.map(|id| s.source_id == id).unwrap_or(true))
            .collect();
        v.sort_by(|a, b| a.day.cmp(&b.day).then(b.time.cmp(&a.time)));
        v
    }

    pub fn count_for(&self, source_id: &str) -> usize {
        self.snapshots.iter().filter(|s| s.source_id == source_id).count()
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
        self.sources.iter().map(|s| self.protected_bytes(&s.id)).sum()
    }

    pub fn day_label(day: u32) -> String {
        match day {
            0 => "Today".to_string(),
            1 => "Yesterday".to_string(),
            n => format!("{n} days ago"),
        }
    }
}

/// Format a byte count as a compact human string (binary units, decimal-ish
/// rounding): 1.2 TB, 312 GB, 820 MB, 4 MB.
pub fn human_bytes(b: u64) -> String {
    let bf = b as f64;
    if b >= TB {
        format!("{:.1} TB", bf / TB as f64)
    } else if b >= GB {
        let v = bf / GB as f64;
        if v >= 100.0 { format!("{v:.0} GB") } else { format!("{v:.1} GB") }
    } else if b >= MB {
        format!("{:.0} MB", bf / MB as f64)
    } else if b >= 1024 {
        format!("{:.0} KB", bf / 1024.0)
    } else {
        format!("{b} B")
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
    fn human_bytes_units() {
        assert_eq!(human_bytes(4 * MB), "4 MB");
        assert_eq!(human_bytes(512 * GB), "512 GB");
        assert_eq!(human_bytes(2 * TB), "2.0 TB");
        assert_eq!(human_bytes(9 * GB), "9.0 GB");
    }

    #[test]
    fn count_and_protected() {
        let c = Catalog::sample();
        assert!(c.count_for("home") > 0);
        assert!(c.protected_bytes("home") > c.protected_bytes("etc"));
    }
}
