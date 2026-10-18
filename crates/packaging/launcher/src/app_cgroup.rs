//! The session's own cgroup tree for apps.
//!
//! The compositor runs in logind's `session-N.scope` and has to stay there
//! (logind and polkit find a process's session by its cgroup). That scope also
//! holds the display manager's root session worker, so it can be neither
//! delegated to the person nor split into leaves. The apps' tree therefore
//! lives in the user manager instead, as one transient scope per compositor
//! run that delegates its subtree:
//!
//! ```text
//! user@UID.service/app.slice/
//!   tilo-apps-<spawner pid>.scope/   Delegate=yes, OOMPolicy=continue, PartOf the session
//!     spawner/                       the app spawner (cgroup2 has no internal processes)
//!     app-<escaped app id>-<n>/      one per launch: the launcher, its D-Bus
//!                                    proxies, the sandbox and all they start
//! ```
//!
//! The spawner ([`crate::spawner`]), a child of the compositor, asks the
//! manager for the scope once and moves itself into `spawner/`. From then on
//! it makes each app's cgroup, writes its tier (`cpu.weight`, `io.weight`,
//! `io.bfq.weight`, `memory.low`), and starts the launcher inside it with
//! `clone3(CLONE_INTO_CGROUP | CLONE_PARENT)`: born there, and still the
//! compositor's child, with no round trip to anyone. It removes each cgroup
//! once `cgroup.events` says it is no longer populated. The compositor moves
//! apps between tiers by writing the same files ([`crate::priority`]) and
//! ends a whole app with `cgroup.kill`.
//!
//! Who may write what: the tree belongs to the person's uid, like everything
//! in their user manager, so only processes outside a sandbox can write it at
//! all, and those are the compositor, the spawner and the launcher before it
//! enters the sandbox (unconfined host applications could equally write any
//! other cgroup of the user manager, scopes included, and are not confined by
//! design). A sandboxed app sees no `/sys/fs/cgroup` ([`crate::profile`]
//! hides it), has no capability to mount one, and lives in a cgroup
//! namespace rooted at its own cgroup (`bwrap --unshare-all`), where the
//! kernel's `nsdelegate` refuses a move out of that cgroup and a write of its
//! own limits even to a process that could reach the files.

use std::io;
use std::path::{Path, PathBuf};

use crate::priority::{Property, Value};

/// Where the cgroup2 hierarchy is mounted.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Unit name prefix of the delegated tree.
pub const TREE_PREFIX: &str = "tilo-apps-";

/// The leaf the spawner itself lives in.
pub const SPAWNER_LEAF: &str = "spawner";

/// Name prefix of an app's cgroup.
pub const APP_PREFIX: &str = "app-";

/// Controllers the tree hands its apps, when the manager has them.
pub const CONTROLLERS: [&str; 4] = ["cpu", "io", "memory", "pids"];

/// The longest name a cgroup directory may have.
const NAME_MAX: usize = 255;

/// `tilo-apps-<pid>.scope`, the tree the spawner `pid` holds.
#[must_use]
pub fn tree_unit_name(pid: u32) -> String {
    format!("{TREE_PREFIX}{pid}.scope")
}

/// Whether a cgroup directory name is a delegated tree's scope.
fn is_tree(name: &str) -> bool {
    name.strip_prefix(TREE_PREFIX)
        .and_then(|rest| rest.strip_suffix(".scope"))
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// `app-<escaped app id>-<n>`: the id spelled as in a unit name
/// ([`crate::scope::escape_name`]), so `-` only ever separates the fields.
/// `None` when the id is empty or the name would be too long.
#[must_use]
pub fn leaf_name(app_id: &str, n: u64) -> Option<String> {
    let escaped = crate::scope::escape_name(app_id)?;
    let name = format!("{APP_PREFIX}{escaped}-{n}");
    (name.len() <= NAME_MAX).then_some(name)
}

/// The app id an app cgroup's name carries.
#[must_use]
pub fn app_id_of_leaf(leaf: &str) -> Option<String> {
    let (escaped, n) = leaf.strip_prefix(APP_PREFIX)?.rsplit_once('-')?;
    (!escaped.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| crate::scope::unescape_name(escaped))
}

/// The unified-hierarchy path of a `/proc/<pid>/cgroup` text.
fn unified_path(cgroup: &str) -> Option<&str> {
    cgroup.lines().find_map(|line| line.strip_prefix("0::"))
}

/// The delegated app cgroup a `/proc/<pid>/cgroup` text places a process in,
/// as a path below [`CGROUP_ROOT`] (`/user.slice/…/tilo-apps-7.scope/app-x-3`).
/// A process in a cgroup an app created below its own counts as in that app.
#[must_use]
pub fn app_cgroup_of(cgroup: &str) -> Option<String> {
    let path = unified_path(cgroup)?;
    let mut end = 0_usize;
    let mut parts = path.split('/');
    let mut in_tree = false;
    for part in parts.by_ref() {
        end = end.saturating_add(part.len()).saturating_add(1);
        if in_tree {
            return part
                .starts_with(APP_PREFIX)
                .then(|| path.get(..end.saturating_sub(1)))
                .flatten()
                .map(str::to_string);
        }
        in_tree = is_tree(part);
    }
    None
}

/// Whether this process was born in (or moved into) a delegated app cgroup:
/// then it has its cgroup and must not ask the manager for a scope.
#[must_use]
pub fn current_is_app_cgroup() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .is_some_and(|cgroup| app_cgroup_of(&cgroup).is_some())
}

/// The cgroupfs directory of a unified-hierarchy path.
#[must_use]
pub fn fs_path(cgroup: &str) -> PathBuf {
    Path::new(CGROUP_ROOT).join(cgroup.trim_start_matches('/'))
}

/// systemd's mapping of an `IOWeight` (1..10000, default 100) onto BFQ's
/// weight (1..1000, default 100), so a tier reads the same to BFQ here as it
/// did through a scope.
#[must_use]
pub fn bfq_weight(io_weight: u64) -> u64 {
    const DEFAULT: u64 = 100;
    const MIN: u64 = 1;
    const MAX: u64 = 10_000;
    const BFQ_DEFAULT: u64 = 100;
    const BFQ_MIN: u64 = 1;
    const BFQ_MAX: u64 = 1000;
    let weight = io_weight.clamp(MIN, MAX);
    let scaled = |distance: u64, bfq_span: u64, span: u64| {
        distance
            .saturating_mul(bfq_span)
            .checked_div(span)
            .unwrap_or(0)
    };
    if weight <= DEFAULT {
        BFQ_DEFAULT.saturating_sub(scaled(
            DEFAULT.saturating_sub(weight),
            BFQ_DEFAULT.saturating_sub(BFQ_MIN),
            DEFAULT.saturating_sub(MIN),
        ))
    } else {
        BFQ_DEFAULT.saturating_add(scaled(
            weight.saturating_sub(DEFAULT),
            BFQ_MAX.saturating_sub(BFQ_DEFAULT),
            MAX.saturating_sub(DEFAULT),
        ))
    }
}

/// The cgroup files and values that carry systemd resource properties. A
/// property with no file here is not one an app cgroup takes.
#[must_use]
pub fn property_writes(props: &[Property]) -> Vec<(&'static str, String)> {
    let mut writes = Vec::new();
    for (name, value) in props {
        let Value::U64(value) = value else {
            continue;
        };
        match *name {
            "CPUWeight" => writes.push(("cpu.weight", value.to_string())),
            "IOWeight" => {
                writes.push(("io.weight", format!("default {value}")));
                writes.push(("io.bfq.weight", bfq_weight(*value).to_string()));
            }
            "MemoryLow" => writes.push(("memory.low", value.to_string())),
            _ => {}
        }
    }
    writes
}

/// Write `props` into the app cgroup at `dir`. A file that does not exist
/// is a controller the tree does not have (no `io` without BFQ or iocost):
/// skipped, as systemd skips it.
///
/// # Errors
///
/// The first failed write, with the file it was for.
pub fn apply(dir: &Path, props: &[Property]) -> Result<(), String> {
    for (file, value) in property_writes(props) {
        let path = dir.join(file);
        // cgroupfs refuses to create a file (EACCES) rather than report it
        // missing, so look before writing.
        if !path.exists() {
            continue;
        }
        match std::fs::write(&path, &value) {
            Ok(()) => {}
            // Gone meanwhile: the app ended and its cgroup was removed.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{file}={value}: {error}")),
        }
    }
    Ok(())
}

/// End every process in the app cgroup at `dir` at once, those that left
/// its process tree (`setsid`, a double fork) included.
///
/// # Errors
///
/// When the kernel refuses the write (an older kernel without `cgroup.kill`).
pub fn kill(dir: &Path) -> io::Result<()> {
    std::fs::write(dir.join("cgroup.kill"), "1")
}

/// Whether any process is left in the cgroup at `dir` or below it.
///
/// # Errors
///
/// When `cgroup.events` cannot be read (the cgroup is gone).
pub fn populated(dir: &Path) -> io::Result<bool> {
    let events = std::fs::read_to_string(dir.join("cgroup.events"))?;
    Ok(events.lines().any(|line| line.trim() == "populated 1"))
}

/// Remove the empty cgroup at `dir` and any cgroups an app made below it.
///
/// # Errors
///
/// The first directory the kernel refuses to remove (one still populated).
pub fn remove(dir: &Path) -> io::Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                remove(&entry.path())?;
            }
        }
    }
    std::fs::remove_dir(dir)
}

/// What an app cgroup's counters say.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Stats {
    /// Processes in it now.
    pub pids: Option<u64>,
    /// Memory charged to it now, in bytes.
    pub memory_current: Option<u64>,
    /// The most it was ever charged, in bytes.
    pub memory_peak: Option<u64>,
    /// Its protected floor, in bytes: the focused app's tier has one.
    pub memory_low: Option<u64>,
    /// CPU time used, in microseconds.
    pub cpu_usec: Option<u64>,
    /// Its CPU weight: 800 in the foreground tier, 50 in the background.
    pub cpu_weight: Option<u64>,
    /// Processes in it the kernel killed for memory.
    pub oom_kills: Option<u64>,
}

fn read_u64(dir: &Path, file: &str) -> Option<u64> {
    std::fs::read_to_string(dir.join(file))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn read_keyed(dir: &Path, file: &str, key: &str) -> Option<u64> {
    std::fs::read_to_string(dir.join(file))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix(key)?
                .strip_prefix(' ')?
                .trim()
                .parse()
                .ok()
        })
}

/// Read the counters of the app cgroup at `dir`. A counter whose controller
/// the tree lacks is `None`.
#[must_use]
pub fn stats(dir: &Path) -> Stats {
    Stats {
        pids: read_u64(dir, "pids.current").or_else(|| {
            std::fs::read_to_string(dir.join("cgroup.procs"))
                .ok()
                .and_then(|procs| u64::try_from(procs.lines().count()).ok())
        }),
        memory_current: read_u64(dir, "memory.current"),
        memory_peak: read_u64(dir, "memory.peak"),
        memory_low: read_u64(dir, "memory.low"),
        cpu_usec: read_keyed(dir, "cpu.stat", "usage_usec"),
        cpu_weight: read_u64(dir, "cpu.weight"),
        oom_kills: read_keyed(dir, "memory.events", "oom_kill"),
    }
}

/// One running app, as its cgroup shows it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunningApp {
    pub app_id: String,
    /// The cgroup's path below [`CGROUP_ROOT`]: what the journal records as
    /// `_SYSTEMD_CGROUP` for the app's own entries.
    pub cgroup: String,
    pub stats: Stats,
}

/// The user manager's `app.slice` of `uid`, where the trees live.
#[must_use]
pub fn user_app_slice(uid: u32) -> PathBuf {
    fs_path(&format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ))
}

/// Every app cgroup in every delegated tree below `app_slice` (a cgroupfs
/// directory), oldest tree first.
#[must_use]
pub fn running_apps(app_slice: &Path) -> Vec<RunningApp> {
    let mut trees: Vec<PathBuf> = std::fs::read_dir(app_slice)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_str().is_some_and(is_tree))
        .map(|entry| entry.path())
        .collect();
    trees.sort();
    let mut apps = Vec::new();
    for tree in trees {
        let mut leaves: Vec<PathBuf> = std::fs::read_dir(&tree)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        leaves.sort();
        for leaf in leaves {
            let Some(app_id) = leaf
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(app_id_of_leaf)
            else {
                continue;
            };
            let cgroup = leaf.strip_prefix(CGROUP_ROOT).map_or_else(
                |_| leaf.display().to_string(),
                |path| format!("/{}", path.display()),
            );
            apps.push(RunningApp {
                app_id,
                cgroup,
                stats: stats(&leaf),
            });
        }
    }
    apps
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "-".to_string(),
        |bytes| bytes.checked_shr(20).unwrap_or(0).to_string(),
    )
}

/// `apps` as a table for a terminal: id, tier, processes, memory, CPU time
/// and OOM kills, then the cgroup to match the journal on.
#[must_use]
pub fn format_table(apps: &[RunningApp]) -> String {
    use std::fmt::Write as _;
    let width = apps
        .iter()
        .map(|app| app.app_id.len())
        .max()
        .unwrap_or(0)
        .max(3);
    let mut out = format!(
        "{:<width$}  {:<4}  {:>4}  {:>7}  {:>8}  {:>9}  {:>3}  CGROUP\n",
        "APP", "TIER", "PIDS", "MEM MiB", "PEAK MiB", "CPU s", "OOM"
    );
    for app in apps {
        let tier = match app.stats.cpu_weight {
            Some(crate::priority::FOREGROUND_WEIGHT) => "fg",
            Some(crate::priority::BACKGROUND_WEIGHT) => "bg",
            Some(_) => "?",
            None => "-",
        };
        let cpu = app.stats.cpu_usec.map_or_else(
            || "-".to_string(),
            |usec| {
                let centis = usec.checked_div(10_000).unwrap_or(0);
                format!(
                    "{}.{:02}",
                    centis.checked_div(100).unwrap_or(0),
                    centis.checked_rem(100).unwrap_or(0)
                )
            },
        );
        let _ = writeln!(
            out,
            "{:<width$}  {tier:<4}  {:>4}  {:>7}  {:>8}  {cpu:>9}  {:>3}  {}",
            app.app_id,
            app.stats
                .pids
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            mib(app.stats.memory_current),
            mib(app.stats.memory_peak),
            app.stats
                .oom_kills
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            app.cgroup,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice/tilo-apps-41.scope";

    #[test]
    fn an_app_cgroup_names_its_app_and_is_found_from_a_process_cgroup() {
        assert_eq!(tree_unit_name(41), "tilo-apps-41.scope");
        assert_eq!(
            leaf_name("so.tilo.notes", 3).as_deref(),
            Some("app-so.tilo.notes-3")
        );
        assert_eq!(
            leaf_name("org.gnome.Text-Editor", 12).as_deref(),
            Some("app-org.gnome.Text\\x2dEditor-12")
        );
        // No id may reach outside its own directory or name another file.
        assert_eq!(leaf_name("../x", 1).as_deref(), Some("app-..\\x2fx-1"));
        assert_eq!(leaf_name("", 1), None);
        assert_eq!(leaf_name(&"x".repeat(250), 1), None);
        for id in ["so.tilo.notes", "org.gnome.Text-Editor", "a b/c-9", "../x"] {
            let leaf = leaf_name(id, 7).unwrap();
            assert!(!leaf.contains('/'), "{leaf}");
            assert_eq!(app_id_of_leaf(&leaf).as_deref(), Some(id));
        }
        assert_eq!(app_id_of_leaf(SPAWNER_LEAF), None);
        assert_eq!(app_id_of_leaf("app-x"), None);

        let cgroup = format!("0::{TREE}/app-so.tilo.notes-3\n");
        assert_eq!(
            app_cgroup_of(&cgroup).as_deref(),
            Some(format!("{TREE}/app-so.tilo.notes-3").as_str())
        );
        // A cgroup an app made below its own still counts as the app.
        let nested = format!("0::{TREE}/app-so.tilo.notes-3/inner\n");
        assert_eq!(
            app_cgroup_of(&nested).as_deref(),
            Some(format!("{TREE}/app-so.tilo.notes-3").as_str())
        );
        // The spawner, the tree itself, a legacy scope and the session are not apps.
        for other in [
            format!("0::{TREE}/spawner\n"),
            format!("0::{TREE}\n"),
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-a-7.scope\n"
                .to_string(),
            "0::/user.slice/user-1000.slice/session-2.scope\n".to_string(),
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/tilo-apps-x.scope/app-a-1\n"
                .to_string(),
        ] {
            assert_eq!(app_cgroup_of(&other), None, "{other}");
        }
        assert_eq!(
            fs_path(TREE),
            Path::new(
                "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/tilo-apps-41.scope"
            )
        );
    }

    #[test]
    fn a_tier_becomes_the_files_its_scope_properties_set() {
        // As systemd writes them for IOWeight: the BFQ weight scaled.
        assert_eq!(bfq_weight(100), 100);
        assert_eq!(bfq_weight(800), 163);
        assert_eq!(bfq_weight(50), 50);
        assert_eq!(bfq_weight(1), 1);
        assert_eq!(bfq_weight(10_000), 1000);
        let foreground =
            crate::priority::tier_properties(crate::priority::Tier::Foreground, Some(32 << 30));
        assert_eq!(
            property_writes(&foreground),
            [
                ("cpu.weight", "800".to_string()),
                ("io.weight", "default 800".to_string()),
                ("io.bfq.weight", "163".to_string()),
                ("memory.low", (4_u64 << 30).to_string()),
            ]
        );
        let background =
            crate::priority::tier_properties(crate::priority::Tier::Background, Some(32 << 30));
        assert_eq!(
            property_writes(&background),
            [
                ("cpu.weight", "50".to_string()),
                ("io.weight", "default 50".to_string()),
                ("io.bfq.weight", "50".to_string()),
                ("memory.low", "0".to_string()),
            ]
        );
    }

    #[test]
    fn the_app_list_reads_every_tree_and_skips_what_is_not_an_app() {
        let slice = tempfile::tempdir().unwrap();
        let tree = slice.path().join("tilo-apps-41.scope");
        let notes = tree.join("app-so.tilo.notes-3");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::create_dir_all(tree.join(SPAWNER_LEAF)).unwrap();
        std::fs::create_dir_all(slice.path().join("app-tilo-x-1.scope")).unwrap();
        std::fs::write(notes.join("pids.current"), "4\n").unwrap();
        std::fs::write(notes.join("memory.current"), format!("{}\n", 300 << 20)).unwrap();
        std::fs::write(notes.join("memory.peak"), format!("{}\n", 512 << 20)).unwrap();
        std::fs::write(notes.join("cpu.weight"), "800\n").unwrap();
        std::fs::write(notes.join("cpu.stat"), "usage_usec 2500000\nuser_usec 1\n").unwrap();
        std::fs::write(
            notes.join("memory.events"),
            "low 0\nhigh 0\nmax 0\noom 1\noom_kill 1\noom_group_kill 0\n",
        )
        .unwrap();
        let apps = running_apps(slice.path());
        assert_eq!(apps.len(), 1, "{apps:?}");
        let app = &apps[0];
        assert_eq!(app.app_id, "so.tilo.notes");
        assert_eq!(
            app.stats,
            Stats {
                pids: Some(4),
                memory_current: Some(300 << 20),
                memory_peak: Some(512 << 20),
                memory_low: None,
                cpu_usec: Some(2_500_000),
                cpu_weight: Some(800),
                oom_kills: Some(1),
            }
        );
        let table = format_table(&apps);
        let row = table.lines().nth(1).unwrap();
        for cell in [
            "so.tilo.notes",
            "fg",
            " 4 ",
            " 300 ",
            " 512 ",
            "2.50",
            " 1 ",
        ] {
            assert!(row.contains(cell), "{cell:?} in {row:?}");
        }
    }
}
