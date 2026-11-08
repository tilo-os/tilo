//! The cgroup an app runs in.
//!
//! Every launch is one transient scope of the systemd user manager,
//! `app-tilo-<escaped app id>-<launcher pid>.scope` in `app.slice` (see
//! [`crate::scope`]). The launcher moves itself into it before it forks the
//! app, so the app and everything it starts are inside it rather than chased
//! into it afterwards. One unit per app is what makes an app one thing for
//! resource accounting (its memory is charged to its own cgroup, never the
//! compositor's), for the OOM killer, and for logout or a compositor crash,
//! which stop the app and everything it forked instead of leaving it running
//! with no display.
//!
//! The session writes the tier an app runs in as the cgroup files systemd
//! would write for the same resource-control properties
//! ([`crate::priority::tier_properties`] through [`set_tier`]), so the tier a
//! launch starts in and the one it is moved to are one write, and its memory
//! watch reclaims from and ends what it reads here ([`footprint`],
//! [`memory_events`], [`reclaim`], [`kill`]).
//!
//! Who may write what: the scope belongs to the person's uid, like everything
//! in their user manager, so only processes outside a sandbox can write it at
//! all, and those are the compositor, the session and the launcher before it
//! enters the sandbox (unconfined host applications could equally write any
//! other cgroup of the user manager, scopes included, and are not confined by
//! design). A sandboxed app sees no `/sys/fs/cgroup` ([`crate::profile`]
//! hides it), has no capability to mount one, and lives in a cgroup
//! namespace rooted at its own cgroup (`bwrap --unshare-all`), where the
//! kernel's `nsdelegate` refuses a move out of that cgroup and a write of its
//! own limits even to a process that could reach the files.

use std::io;
use std::path::{Path, PathBuf};

use crate::priority::{Property, Tier};

/// Where the cgroup2 hierarchy is mounted.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// systemd's mapping of an `IOWeight` (1..10000, default 100) onto BFQ's
/// weight (1..1000, default 100).
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
/// is a controller the machine does not have (no `io` without BFQ or iocost):
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
            // Gone meanwhile: the app ended and its scope was removed.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{file}={value}: {error}")),
        }
    }
    Ok(())
}

/// The tier the cgroup at `dir` is in, from its CPU weight.
#[must_use]
pub fn tier_of(dir: &Path) -> Option<Tier> {
    match read_u64(dir, "cpu.weight")? {
        crate::priority::STARTUP_WEIGHT => Some(Tier::Startup),
        crate::priority::SESSION_WEIGHT => Some(Tier::Session),
        crate::priority::FOREGROUND_WEIGHT => Some(Tier::Foreground),
        crate::priority::BACKGROUND_WEIGHT => Some(Tier::Background),
        _ => None,
    }
}

/// The cgroup directory the process `pid` runs in, whatever unit or scope
/// made it: where a launch's own writes go when the host gave it no app scope
/// (a session without a user manager runs the app in its own cgroup).
#[must_use]
pub fn cgroup_of_pid(pid: u32) -> Option<PathBuf> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    Some(fs_path(unified_path(&cgroup)?))
}

/// The cgroup directory this process runs in.
#[must_use]
pub fn own_cgroup() -> Option<PathBuf> {
    cgroup_of_pid(std::process::id())
}

/// Put the cgroup this process runs in at [`Tier::Session`]: the compositor
/// and the desk placing, drawing and answering a window are what a person
/// waits on when they open something, so they answer before every app.
///
/// The image sets that weight on the compositor's logind scope before the
/// session starts (`os/etc/systemd/system/session-.scope.d`); this is the same
/// tier carried by the session itself, so a session a host started without
/// those units, or one started from a terminal, is served the same way. A
/// cgroup already weighted at or above the session tier is left as it is, so
/// this never lowers what systemd set, nor a launch's own boost.
///
/// # Errors
///
/// The first failed write, as [`apply`].
pub fn claim_session_weight() -> Result<(), String> {
    match own_cgroup() {
        Some(dir) => raise_session_weight(&dir),
        None => Ok(()),
    }
}

/// [`claim_session_weight`] for the cgroup at `dir`.
fn raise_session_weight(dir: &Path) -> Result<(), String> {
    if read_u64(dir, "cpu.weight").is_some_and(|weight| weight >= crate::priority::SESSION_WEIGHT) {
        return Ok(());
    }
    set_tier(dir, Tier::Session, None)
}

/// Put the cgroup at `dir` in `tier`, writing the files a launch is born
/// with ([`crate::priority::tier_properties`] through [`apply`]).
///
/// # Errors
///
/// The first failed write, as [`apply`].
pub fn set_tier(dir: &Path, tier: Tier, total: Option<u64>) -> Result<(), String> {
    apply(dir, &crate::priority::tier_properties(tier, total))
}

/// Make the app cgroup at `dir` one OOM group (`on`) or one victim (`off`):
/// the kernel then ends everything in it together when it picks any of it.
/// A cgroup without the memory controller has no such file and nothing to
/// set.
pub fn set_oom_group(dir: &Path, on: bool) -> io::Result<()> {
    let path = dir.join("memory.oom.group");
    if !path.exists() {
        return Ok(());
    }
    std::fs::write(path, if on { "1" } else { "0" })
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
pub fn reclaim(dir: &Path, bytes: u64) -> io::Result<()> {
    match std::fs::write(dir.join("memory.reclaim"), bytes.to_string()) {
        Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => Ok(()),
        other => other,
    }
}

/// The unified-hierarchy path of a `/proc/<pid>/cgroup` text.
fn unified_path(cgroup: &str) -> Option<&str> {
    cgroup.lines().find_map(|line| line.strip_prefix("0::"))
}

/// Whether a cgroup directory name is a launch's own scope
/// ([`crate::scope::unit_name`]).
fn is_app_scope(name: &str) -> bool {
    name.strip_prefix(crate::scope::UNIT_PREFIX)
        .and_then(|rest| rest.strip_suffix(".scope"))
        .is_some_and(|rest| {
            rest.rsplit_once('-')
                .is_some_and(|(id, pid)| !id.is_empty() && pid.parse::<u32>().is_ok())
        })
}

/// The app id and pid a scope's directory name carries.
fn app_of_scope(name: &str) -> Option<(String, u32)> {
    let rest = name
        .strip_prefix(crate::scope::UNIT_PREFIX)?
        .strip_suffix(".scope")?;
    let (escaped, pid) = rest.rsplit_once('-')?;
    Some((crate::scope::unescape_name(escaped), pid.parse().ok()?))
}

/// The `app-tilo-<app id>-<pid>.scope` a `/proc/<pid>/cgroup` text places a
/// process in, as a path below [`CGROUP_ROOT`]; `None` for a process outside
/// the app slice's own scopes, and for one in another unit of the slice (a
/// service, the manager itself).
#[must_use]
pub fn scope_of(cgroup: &str) -> Option<String> {
    let path = unified_path(cgroup)?;
    let mut end = 0_usize;
    let mut in_slice = false;
    for part in path.split('/') {
        end = end.saturating_add(part.len()).saturating_add(1);
        if in_slice {
            return is_app_scope(part)
                .then(|| path.get(..end.saturating_sub(1)))
                .flatten()
                .map(str::to_string);
        }
        in_slice = part == crate::scope::APP_SLICE;
    }
    None
}

/// The app cgroup a `/proc/<pid>/cgroup` text places a process in: the scope
/// of its own a launch asked for. A process in a cgroup an app created below
/// its own counts as in that app.
#[must_use]
pub fn app_cgroup_at(cgroup: &str) -> Option<String> {
    scope_of(cgroup)
}

/// Whether this process is in an app's cgroup already: then it has one and
/// must not ask the manager for a second.
#[must_use]
pub fn current_is_app_cgroup() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .is_some_and(|cgroup| app_cgroup_at(&cgroup).is_some())
}

/// The cgroupfs directory of a unified-hierarchy path.
#[must_use]
pub fn fs_path(cgroup: &str) -> PathBuf {
    Path::new(CGROUP_ROOT).join(cgroup.trim_start_matches('/'))
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

/// End every process in the app cgroup at `dir` at once, those that left
/// its process tree (`setsid`, a double fork) included.
pub fn kill(dir: &Path) -> io::Result<()> {
    std::fs::write(dir.join("cgroup.kill"), "1")
}

/// What an app cgroup's counters say.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Stats {
    /// Processes in it now.
    pub pids: Option<u64>,
    pub memory_current: Option<u64>,
    /// The most it was ever charged, in bytes.
    pub memory_peak: Option<u64>,
    /// Its protected floor, in bytes: the foreground tier has one.
    pub memory_low: Option<u64>,
    /// CPU time used, in microseconds.
    pub cpu_usec: Option<u64>,
    /// Its CPU weight: 10 000 while it opens, 1 000 for the session, 800 in
    /// the foreground tier, 50 in the background.
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
/// the machine lacks is `None`.
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

/// The user manager's `app.slice` of `uid`, where the app scopes live.
#[must_use]
pub fn user_app_slice(uid: u32) -> PathBuf {
    fs_path(&format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"
    ))
}

/// Every app cgroup below `app_slice` (a cgroupfs directory), oldest first.
#[must_use]
pub fn running_apps(app_slice: &Path) -> Vec<RunningApp> {
    let mut scopes: Vec<(u32, String, PathBuf)> = std::fs::read_dir(app_slice)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let (_, pid) = app_of_scope(name.to_str()?)?;
            Some((pid, name.to_str()?.to_owned(), entry.path()))
        })
        .collect();
    scopes.sort();
    let mut apps = Vec::new();
    for (_, name, dir) in scopes {
        let Some((app_id, _)) = app_of_scope(&name) else {
            continue;
        };
        let cgroup = dir.strip_prefix(CGROUP_ROOT).map_or_else(
            |_| dir.display().to_string(),
            |path| format!("/{}", path.display()),
        );
        apps.push(RunningApp {
            app_id,
            cgroup,
            stats: stats(&dir),
        });
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

    const SLICE: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice";
    const NOTES: &str =
        "/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-so.tilo.notes-4126.scope";

    #[test]
    fn an_app_cgroup_is_found_from_a_process_cgroup() {
        for text in [
            format!("0::{NOTES}\n"),
            // A process in a cgroup an app made below its own is in that app.
            format!("0::{NOTES}/inner\n"),
        ] {
            assert_eq!(scope_of(&text).as_deref(), Some(NOTES), "{text}");
            assert_eq!(app_cgroup_at(&text).as_deref(), Some(NOTES), "{text}");
            let dir = fs_path(scope_of(&text).unwrap().as_str());
            assert_eq!(
                dir,
                Path::new(CGROUP_ROOT)
                    .join(SLICE.trim_start_matches('/'))
                    .join("app-tilo-so.tilo.notes-4126.scope")
            );
        }
        // The app slice holds more than app scopes: a service, the manager
        // itself, and the session.
        for other in [
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/cc-model-router.service\n",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice\n",
            "0::/user.slice/user-1000.slice/user@1000.service\n",
            "0::/user.slice/user-1000.slice/session-2.scope\n",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-4126.scope\n",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-other-4126.scope\n",
        ] {
            assert_eq!(scope_of(other), None, "{other}");
        }
        assert_eq!(
            app_of_scope("app-tilo-so.tilo.notes-4126.scope"),
            Some(("so.tilo.notes".to_string(), 4126))
        );
        assert_eq!(
            app_of_scope("app-tilo-org.gnome.Text\\x2dEditor-7.scope"),
            Some(("org.gnome.Text-Editor".to_string(), 7))
        );
        assert_eq!(app_of_scope("tilo-apps-41.scope"), None);
        assert_eq!(app_of_scope("app-tilo-4126.scope"), None);
    }

    #[test]
    fn putting_an_app_in_a_tier_writes_what_a_launch_is_born_with() {
        let app = tempfile::tempdir().unwrap();
        for file in ["cpu.weight", "io.weight", "io.bfq.weight", "memory.low"] {
            std::fs::write(app.path().join(file), "").unwrap();
        }
        set_tier(app.path(), Tier::Background, Some(32 << 30)).unwrap();
        assert_eq!(tier_of(app.path()), Some(Tier::Background));
        assert_eq!(
            std::fs::read_to_string(app.path().join("io.weight")).unwrap(),
            "default 50"
        );
        set_tier(app.path(), Tier::Foreground, Some(32 << 30)).unwrap();
        assert_eq!(tier_of(app.path()), Some(Tier::Foreground));
        assert_eq!(
            std::fs::read_to_string(app.path().join("memory.low")).unwrap(),
            (4_u64 << 30).to_string()
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
    fn memory_events_footprint_and_pids_are_read_from_the_cgroup() {
        let app = tempfile::tempdir().unwrap();
        std::fs::write(
            app.path().join("memory.events"),
            "low 0\nhigh 0\nmax 0\noom 2\noom_kill 3\noom_group_kill 1\n",
        )
        .unwrap();
        assert_eq!(
            memory_events(app.path()),
            MemoryEvents {
                oom: 2,
                oom_kill: 3,
                oom_group_kill: 1
            }
        );
        std::fs::write(
            app.path().join("memory.stat"),
            "anon 1048576\nfile 99999999\n",
        )
        .unwrap();
        std::fs::write(app.path().join("memory.swap.current"), "2048\n").unwrap();
        assert_eq!(
            footprint(app.path()),
            Some(1_048_576 + 2048),
            "page cache is no footprint"
        );
        std::fs::write(app.path().join("memory.events"), "oom_kill 0\n").unwrap();
        assert_eq!(memory_events(app.path()).oom_kill, 0);
        std::fs::write(app.path().join("cgroup.procs"), "10\n11\n").unwrap();
        assert_eq!(processes(app.path()), [10, 11]);
    }

    #[test]
    fn the_session_raises_its_cgroups_weight_and_never_lowers_it() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["cpu.weight", "io.weight", "io.bfq.weight"] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        std::fs::write(dir.path().join("cpu.weight"), "100").unwrap();
        raise_session_weight(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cpu.weight")).unwrap(),
            "1000",
            "a cgroup nothing weighted carries the session's own weight"
        );
        assert_eq!(tier_of(dir.path()), Some(Tier::Session));
        // What the image's units already set is left as it is, and a launch
        // that boosted its cgroup is never lowered by the session.
        for weight in ["1000", "10000"] {
            std::fs::write(dir.path().join("cpu.weight"), weight).unwrap();
            raise_session_weight(dir.path()).unwrap();
            assert_eq!(
                std::fs::read_to_string(dir.path().join("cpu.weight")).unwrap(),
                weight
            );
        }
    }

    #[test]
    fn the_app_list_reads_every_scope_and_skips_what_is_not_an_app() {
        let slice = tempfile::tempdir().unwrap();
        let notes = slice.path().join("app-tilo-so.tilo.notes-3.scope");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::create_dir_all(slice.path().join("app-tilo-broken.scope")).unwrap();
        std::fs::create_dir_all(slice.path().join("cc-model-router.service")).unwrap();
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
