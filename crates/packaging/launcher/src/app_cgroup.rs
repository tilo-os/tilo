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
//!     app-<escaped app id>-<n>/      one per launch: its tier, its accounting, its kill
//!       main/                        the launcher, its D-Bus proxies, the sandbox and
//!                                    all they start; memory.oom.group=1 for a Tilo app
//!       job-<j>/                     an independent job the app started (a terminal
//!                                    session, a language server): memory.oom.group=1
//! ```
//!
//! The spawner ([`crate::spawner`]), a child of the compositor, asks the
//! manager for the scope once and moves itself into `spawner/`. From then on
//! it makes each app's cgroup, writes its tier (`cpu.weight`, `io.weight`,
//! `io.bfq.weight`, `memory.low`), lays it out ([`lay_out`]) and starts the
//! launcher inside its `main/` leaf with `clone3(CLONE_INTO_CGROUP |
//! CLONE_PARENT)`: born there, and still the compositor's child, with no
//! round trip to anyone. It removes each cgroup once `cgroup.events` says it
//! is no longer populated. The compositor moves apps between tiers by writing
//! the same files ([`crate::priority`]) and ends a whole app with
//! `cgroup.kill`.
//!
//! **Out of memory, an app is one unit; a job is its own.** The kernel's OOM
//! killer, when it picks a process, ends every process of the highest
//! cgroup between it and the root that has `memory.oom.group` set. A Tilo
//! app's `main/` has it: an app that runs out of memory ends cleanly and
//! completely, not with a helper missing, and the spawner then ends its jobs
//! too (`cgroup.kill` on the app). A job's leaf has it as well and is
//! `main/`'s sibling, not its child, so a runaway language server or build
//! ends alone and the app that started it keeps running. The app's own
//! cgroup leaves it unset; that is what keeps the two apart. A host app
//! (Alacritty, VS Code) keeps `memory.oom.group=0`: its process structure is
//! its own business, and one victim is what it was built for. How a process
//! becomes a job is `runtime_portal::jobs`.
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

use crate::priority::{Property, Tier, Value};

/// Where the cgroup2 hierarchy is mounted.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Unit name prefix of the delegated tree.
pub const TREE_PREFIX: &str = "tilo-apps-";

/// The leaf the spawner itself lives in.
pub const SPAWNER_LEAF: &str = "spawner";

pub const APP_PREFIX: &str = "app-";

/// The leaf of an app's cgroup its launcher is born in.
pub const MAIN_LEAF: &str = "main";

/// Name prefix of an app's job leaves.
pub const JOB_PREFIX: &str = "job-";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Leaf {
    /// The app itself: its launcher and everything that was not started as
    /// a job.
    Main,
    Job,
}

/// What a launch runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    /// A Tilo app, in its sandbox: one unit when memory runs out.
    Tilo,
    /// A host application (a desktop entry): its structure is unknown.
    Host,
}

impl AppKind {
    /// How a launch request spells it.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Tilo => "tilo",
            Self::Host => "host",
        }
    }

    /// The kind a launch request names.
    #[must_use]
    pub fn from_wire(text: &[u8]) -> Option<Self> {
        match text {
            b"tilo" => Some(Self::Tilo),
            b"host" => Some(Self::Host),
            _ => None,
        }
    }
}

/// Whether a leaf of an app of `kind` is one OOM group: everything in it
/// ends together when the kernel picks any of it. Every job is; a Tilo app's
/// main leaf is; a host app's is not.
#[must_use]
pub const fn oom_group(kind: AppKind, leaf: Leaf) -> bool {
    match (kind, leaf) {
        (_, Leaf::Job) | (AppKind::Tilo, Leaf::Main) => true,
        (AppKind::Host, Leaf::Main) => false,
    }
}

#[must_use]
pub fn job_leaf_name(n: u64) -> String {
    format!("{JOB_PREFIX}{n}")
}

#[must_use]
pub fn is_job_leaf(name: &str) -> bool {
    name.strip_prefix(JOB_PREFIX)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

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

/// Whether `dir` hands the memory controller to its children.
fn memory_delegated(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("cgroup.subtree_control")).is_ok_and(|control| {
        control
            .split_whitespace()
            .any(|controller| controller.trim_start_matches('+') == "memory")
    })
}

/// Set `memory.oom.group` of the leaf at `leaf`, whose parent `app` hands it
/// the memory controller; without that the file does not exist and there is
/// nothing to set.
fn set_oom_group(app: &Path, leaf: &Path, on: bool) -> io::Result<()> {
    if !memory_delegated(app) {
        return Ok(());
    }
    std::fs::write(leaf.join("memory.oom.group"), if on { "1" } else { "0" })
}

/// Lay out the fresh app cgroup at `dir` for an app of `kind`: hand the
/// memory controller to its leaves (cgroup2 then keeps processes out of
/// `dir` itself) and make the `main/` leaf its launcher is born in, one OOM
/// group for a Tilo app ([`oom_group`]). Returns the main leaf.
///
/// # Errors
///
/// When the main leaf cannot be made. A tree without the memory controller
/// costs the OOM grouping, not the launch.
pub fn lay_out(dir: &Path, kind: AppKind) -> io::Result<PathBuf> {
    let available = std::fs::read_to_string(dir.join("cgroup.controllers")).unwrap_or_default();
    if available
        .split_whitespace()
        .any(|controller| controller == "memory")
        && let Err(error) = std::fs::write(dir.join("cgroup.subtree_control"), "+memory")
    {
        eprintln!(
            "tilo-spawner: {} keeps its memory accounting whole: {error}",
            dir.display()
        );
    }
    let main = dir.join(MAIN_LEAF);
    match std::fs::create_dir(&main) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    if let Err(error) = set_oom_group(dir, &main, oom_group(kind, Leaf::Main)) {
        eprintln!("tilo-spawner: {} is no OOM group: {error}", main.display());
    }
    Ok(main)
}

/// Make the job leaf `job-<n>` of the app cgroup at `dir`: its own OOM
/// group, a sibling of the app's main leaf.
///
/// # Errors
///
/// When the leaf cannot be made or made one OOM group: a job that the
/// kernel would not end whole is no job.
pub fn create_job(dir: &Path, n: u64) -> io::Result<PathBuf> {
    let job = dir.join(job_leaf_name(n));
    std::fs::create_dir(&job)?;
    if let Err(error) = set_oom_group(dir, &job, oom_group(AppKind::Tilo, Leaf::Job)) {
        let _ = std::fs::remove_dir(&job);
        return Err(error);
    }
    Ok(job)
}

/// The job leaves of the app cgroup at `dir`, in creation order.
#[must_use]
pub fn jobs(dir: &Path) -> Vec<PathBuf> {
    let mut jobs: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let n = name.to_str()?.strip_prefix(JOB_PREFIX)?.parse().ok()?;
            Some((n, entry.path()))
        })
        .collect();
    jobs.sort();
    jobs.into_iter().map(|(_, path)| path).collect()
}

/// The kind of the app whose cgroup is `dir`, from its main leaf's OOM
/// grouping; `None` where the tree has no memory controller to tell.
#[must_use]
pub fn kind_of(dir: &Path) -> Option<AppKind> {
    match read_u64(&dir.join(MAIN_LEAF), "memory.oom.group")? {
        0 => Some(AppKind::Host),
        _ => Some(AppKind::Tilo),
    }
}

/// The focus tier the app cgroup at `dir` is in, from its CPU weight.
#[must_use]
pub fn tier_of(dir: &Path) -> Option<Tier> {
    match read_u64(dir, "cpu.weight")? {
        crate::priority::FOREGROUND_WEIGHT => Some(Tier::Foreground),
        crate::priority::BACKGROUND_WEIGHT => Some(Tier::Background),
        _ => None,
    }
}

/// What `memory.events` of a cgroup counts, for it and everything below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryEvents {
    /// Times it hit a limit and the kernel had to pick a victim.
    pub oom: u64,
    /// Processes the kernel killed for memory.
    pub oom_kill: u64,
    /// Times it was ended whole as one OOM group.
    pub oom_group_kill: u64,
}

/// The memory events of the cgroup at `dir`; zero where there are none.
#[must_use]
pub fn memory_events(dir: &Path) -> MemoryEvents {
    let Ok(text) = std::fs::read_to_string(dir.join("memory.events")) else {
        return MemoryEvents::default();
    };
    let count = |key: &str| {
        text.lines()
            .find_map(|line| {
                line.strip_prefix(key)?
                    .strip_prefix(' ')?
                    .trim()
                    .parse()
                    .ok()
            })
            .unwrap_or(0)
    };
    MemoryEvents {
        oom: count("oom"),
        oom_kill: count("oom_kill"),
        oom_group_kill: count("oom_group_kill"),
    }
}

/// What the cgroup at `dir` holds that only it can give back: anonymous
/// memory in RAM (`memory.stat` `anon`) plus what of it went to swap.
/// `memory.current` where the breakdown is missing.
#[must_use]
pub fn footprint(dir: &Path) -> Option<u64> {
    let anon = read_keyed(dir, "memory.stat", "anon");
    let swapped = read_u64(dir, "memory.swap.current").unwrap_or(0);
    anon.map(|anon| anon.saturating_add(swapped))
        .or_else(|| read_u64(dir, "memory.current"))
}

/// Ask the kernel to reclaim `bytes` from the cgroup at `dir` now. It may
/// reclaim less (`EAGAIN`); what it got is still freed.
///
/// # Errors
///
/// The kernel's refusal other than falling short.
pub fn reclaim(dir: &Path, bytes: u64) -> io::Result<()> {
    match std::fs::write(dir.join("memory.reclaim"), bytes.to_string()) {
        Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => Ok(()),
        other => other,
    }
}

/// Every process in the cgroup at `dir` and below it.
#[must_use]
pub fn processes(dir: &Path) -> Vec<u32> {
    let mut pids: Vec<u32> = std::fs::read_to_string(dir.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            pids.extend(processes(&entry.path()));
        }
    }
    pids
}

/// Give every process of the app cgroup at `dir` the `oom_score_adj` of
/// `tier` ([`crate::priority::oom_score_adj`]): its main leaf the app's, its
/// jobs a little more. A process that ended meanwhile is skipped.
///
/// # Errors
///
/// How many processes refused, with the first reason.
pub fn set_oom_score_adj(dir: &Path, tier: Tier) -> Result<(), String> {
    let mut leaves = vec![(dir.join(MAIN_LEAF), Leaf::Main)];
    leaves.extend(jobs(dir).into_iter().map(|job| (job, Leaf::Job)));
    let mut refused = 0_usize;
    let mut first = None;
    for (leaf, kind) in leaves {
        let value = crate::priority::oom_score_adj(tier, kind).to_string();
        for pid in processes(&leaf) {
            match std::fs::write(format!("/proc/{pid}/oom_score_adj"), &value) {
                Ok(()) => {}
                Err(error)
                    if matches!(error.kind(), io::ErrorKind::NotFound)
                        || error.raw_os_error() == Some(libc::ESRCH) => {}
                Err(error) => {
                    refused = refused.saturating_add(1);
                    first.get_or_insert_with(|| format!("pid {pid}: {error}"));
                }
            }
        }
    }
    match first {
        None => Ok(()),
        Some(first) => Err(format!(
            "{refused} processes kept their OOM score ({first})"
        )),
    }
}

/// End every process in the app cgroup at `dir` at once, those that left
/// its process tree (`setsid`, a double fork) included.
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
        pids: read_u64(dir, "pids.current").or_else(|| u64::try_from(processes(dir).len()).ok()),
        memory_current: read_u64(dir, "memory.current"),
        memory_peak: read_u64(dir, "memory.peak"),
        memory_low: read_u64(dir, "memory.low"),
        cpu_usec: read_keyed(dir, "cpu.stat", "usage_usec"),
        cpu_weight: read_u64(dir, "cpu.weight"),
        oom_kills: read_keyed(dir, "memory.events", "oom_kill"),
    }
}

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
    fn a_tilo_app_is_one_oom_group_and_each_job_is_its_own() {
        assert!(oom_group(AppKind::Tilo, Leaf::Main));
        assert!(oom_group(AppKind::Tilo, Leaf::Job));
        assert!(!oom_group(AppKind::Host, Leaf::Main));
        assert!(oom_group(AppKind::Host, Leaf::Job));
        for kind in [AppKind::Tilo, AppKind::Host] {
            assert_eq!(AppKind::from_wire(kind.wire().as_bytes()), Some(kind));
        }
        assert_eq!(AppKind::from_wire(b"other"), None);

        for (kind, main_group) in [(AppKind::Tilo, "1"), (AppKind::Host, "0")] {
            let app = tempfile::tempdir().unwrap();
            std::fs::write(
                app.path().join("cgroup.controllers"),
                "cpu io memory pids\n",
            )
            .unwrap();
            let read = |path: &Path| std::fs::read_to_string(path).unwrap();
            let main = lay_out(app.path(), kind).unwrap();
            assert_eq!(main, app.path().join(MAIN_LEAF));
            // Memory goes to the leaves, so the app's own cgroup holds no
            // process and sets no OOM group of its own.
            assert_eq!(read(&app.path().join("cgroup.subtree_control")), "+memory");
            assert!(!app.path().join("memory.oom.group").exists());
            assert_eq!(read(&main.join("memory.oom.group")), main_group, "{kind:?}");
            assert_eq!(kind_of(app.path()), Some(kind));

            // Each job is a sibling of the main leaf, its own OOM group: the
            // kernel ends the highest grouped cgroup above its victim, which
            // for a job's process is the job and nothing above it.
            let first = create_job(app.path(), 1).unwrap();
            let second = create_job(app.path(), 2).unwrap();
            assert_eq!(first, app.path().join("job-1"));
            assert_eq!(first.parent(), Some(app.path()));
            assert_eq!(read(&first.join("memory.oom.group")), "1");
            assert_eq!(read(&second.join("memory.oom.group")), "1");
            assert_eq!(jobs(app.path()), [first, second]);
            assert!(
                create_job(app.path(), 1).is_err(),
                "a job leaf is never reused"
            );
        }
        assert!(is_job_leaf("job-12"));
        assert!(!is_job_leaf("job-"));
        assert!(!is_job_leaf(MAIN_LEAF));
        let in_job = format!("0::{TREE}/app-so.tilo.terminal-3/job-2\n");
        assert_eq!(
            app_cgroup_of(&in_job).as_deref(),
            Some(format!("{TREE}/app-so.tilo.terminal-3").as_str())
        );
        // The delegated tree is named the same way on both sides of the job door.
        #[cfg(target_os = "linux")]
        {
            assert_eq!(
                runtime_portal::jobs::tree_in(&in_job),
                Some("tilo-apps-41.scope")
            );
            assert!(is_tree(runtime_portal::jobs::tree_in(&in_job).unwrap()));
            assert_eq!(runtime_portal::jobs::TREE_PREFIX, TREE_PREFIX);
        }
    }

    #[test]
    fn memory_events_footprint_and_tier_are_read_from_the_cgroup() {
        let app = tempfile::tempdir().unwrap();
        let main = app.path().join(MAIN_LEAF);
        std::fs::create_dir(&main).unwrap();
        std::fs::write(
            main.join("memory.events"),
            "low 0\nhigh 0\nmax 0\noom 2\noom_kill 3\noom_group_kill 1\n",
        )
        .unwrap();
        assert_eq!(
            memory_events(&main),
            MemoryEvents {
                oom: 2,
                oom_kill: 3,
                oom_group_kill: 1
            }
        );
        assert_eq!(memory_events(app.path()), MemoryEvents::default());
        std::fs::write(main.join("memory.stat"), "anon 1048576\nfile 99999999\n").unwrap();
        std::fs::write(main.join("memory.swap.current"), "2048\n").unwrap();
        assert_eq!(
            footprint(&main),
            Some(1_048_576 + 2048),
            "page cache is no footprint"
        );
        std::fs::write(app.path().join("cpu.weight"), "50\n").unwrap();
        assert_eq!(tier_of(app.path()), Some(Tier::Background));
        std::fs::write(app.path().join("cpu.weight"), "800\n").unwrap();
        assert_eq!(tier_of(app.path()), Some(Tier::Foreground));
        std::fs::write(main.join("cgroup.procs"), "10\n11\n").unwrap();
        let job = app.path().join("job-1");
        std::fs::create_dir(&job).unwrap();
        std::fs::write(job.join("cgroup.procs"), "12\n").unwrap();
        let mut pids = processes(app.path());
        pids.sort_unstable();
        assert_eq!(pids, [10, 11, 12]);
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
