//! A cgroup per job, and the providers that make them.
//!
//! ```text
//! <the worker's cgroup>/     the pod's (kubelet sets memory.oom.group=1 here)
//!   worker/                  the worker; memory.min = reserve
//!   jobs/                    memory.max = budget; every controller enabled
//!     <job>/                 one per running job
//! ```
//!
//! The budget bounds what jobs hold together, in two ways. A job is admitted
//! when the reservations of the running jobs, its own included, fit in the
//! budget ([`Limits::memory_reserved`]). Its `memory.max` may be higher than
//! its reservation, so a job that briefly needs more gets it from what others
//! leave unused; `jobs/memory.max` keeps them all below the pod's limit, and
//! the caller's guard stops the job furthest past its reservation when memory
//! runs short.
//!
//! A [`CgroupProvider`] makes the job cgroups. [`Direct`] does it itself, in
//! a cgroup it may write (a privileged container, a systemd unit with
//! `Delegate=yes`); a per-node broker (not yet built) would do it for a
//! worker whose cgroupfs is read-only. Either way the job's cgroup arrives as
//! a [`JobCgroup`]: a directory descriptor `clone3` starts the job in, the
//! counters, and `cgroup.kill` when it is dropped.

use std::collections::HashMap;
use std::ffi::CString;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Controllers a job cgroup uses, when its parent offers them.
pub const CONTROLLERS: [&str; 4] = ["cpu", "memory", "io", "pids"];

/// The `pids.max` a job gets unless it asks for another.
pub const DEFAULT_PIDS_MAX: u64 = 4096;

/// What a job's cgroup bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// `memory.max`, bytes. `memory.high` is never set: with no swap, a
    /// `high` below `max` throttles a job into a hang instead of failing it.
    pub memory_max: Option<u64>,
    /// The memory the caller's scheduler reserved for the job, bytes: what
    /// admission counts against the budget. `memory_max` may be above it.
    /// Without one, the job's `memory_max` is counted.
    pub memory_reserved: Option<u64>,
    pub pids_max: Option<u64>,
    /// `cpu.weight`, 1–10000 (the kernel's default is 100): the job's share.
    pub cpu_weight: Option<u32>,
    /// `cpu.max`: at most `quota` microseconds of CPU per `period`.
    pub cpu_max: Option<CpuMax>,
    /// `io.max` lines, one per disk.
    pub io: Vec<IoMax>,
}

impl Limits {
    /// What admission counts of this job against the budget: its
    /// reservation, else its `memory.max`.
    pub fn counted(&self) -> Option<u64> {
        self.memory_reserved.or(self.memory_max)
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_max: None,
            memory_reserved: None,
            pids_max: Some(DEFAULT_PIDS_MAX),
            cpu_weight: None,
            cpu_max: None,
            io: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuMax {
    pub quota_us: u64,
    pub period_us: u64,
}

/// One disk's `io.max`. The kernel takes only a whole disk's numbers, not a
/// partition's: [`IoMax::for_path`] finds the disk a path is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IoMax {
    pub major: u32,
    pub minor: u32,
    pub rbps: Option<u64>,
    pub wbps: Option<u64>,
    pub riops: Option<u64>,
    pub wiops: Option<u64>,
}

impl IoMax {
    /// No limits yet on the whole disk holding `path`.
    pub fn for_path(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(path)?.dev();
        let (major, minor) = whole_disk(libc::major(dev), libc::minor(dev));
        Ok(Self {
            major,
            minor,
            ..Self::default()
        })
    }

    fn line(&self) -> String {
        let v = |o: Option<u64>| o.map_or_else(|| "max".to_owned(), |n| n.to_string());
        format!(
            "{}:{} rbps={} wbps={} riops={} wiops={}",
            self.major,
            self.minor,
            v(self.rbps),
            v(self.wbps),
            v(self.riops),
            v(self.wiops)
        )
    }
}

/// The whole disk a block device belongs to: a partition's parent, the
/// device itself otherwise (a disk, a zvol, a device-mapper volume).
pub fn whole_disk(major: u32, minor: u32) -> (u32, u32) {
    let sys = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
    if !sys.join("partition").exists() {
        return (major, minor);
    }
    std::fs::canonicalize(&sys)
        .ok()
        .and_then(|p| std::fs::read_to_string(p.parent()?.join("dev")).ok())
        .and_then(|d| {
            let (a, b) = d.trim().split_once(':')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((major, minor))
}

/// Why a cgroup could not be made.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{0}")]
    Io(String),
    #[error(
        "the job's memory reservation ({asked} bytes) would take the running jobs' reservations \
         past their budget ({used} of {budget} bytes reserved)"
    )]
    OverBudget { asked: u64, used: u64, budget: u64 },
    #[error("job name {0:?} is not a single path component")]
    BadName(String),
    #[error("the broker refused: {0}")]
    Refused(String),
}

fn io_err(what: impl std::fmt::Display) -> impl FnOnce(io::Error) -> ProviderError {
    let what = what.to_string();
    move |e| ProviderError::Io(format!("{what}: {e}"))
}

/// What an XFS project quota request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaOutcome {
    Applied,
    /// The filesystem doesn't enforce project quotas (mounted without
    /// `prjquota`, or not XFS): the job runs without a disk bound.
    Unsupported(String),
}

/// Makes job cgroups and sets disk quotas.
pub trait CgroupProvider: Send + Sync {
    /// A cgroup for job `name` (one path component), `limits` applied.
    ///
    /// # Errors
    /// See [`ProviderError`].
    fn create(&self, name: &str, limits: &Limits) -> Result<JobCgroup, ProviderError>;

    /// An XFS project quota of `bytes` on the directory `dir`, as project
    /// `project`; files already in it are tagged too.
    ///
    /// # Errors
    /// Fails only when quotas are enforced and the request still failed;
    /// a filesystem without them answers [`QuotaOutcome::Unsupported`].
    fn set_quota(
        &self,
        dir: BorrowedFd<'_>,
        project: u32,
        bytes: u64,
    ) -> Result<QuotaOutcome, ProviderError>;
}

/// Ends and removes a job cgroup; the provider's half of [`JobCgroup`].
pub trait Release: Send + Sync {
    /// Called once the cgroup's processes were sent `cgroup.kill`.
    fn release(&self, name: &str);
}

/// One job's cgroup. Dropping it kills whatever is left in it and removes it.
pub struct JobCgroup {
    name: String,
    dir: OwnedFd,
    release: Arc<dyn Release>,
}

impl std::fmt::Debug for JobCgroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCgroup")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A job cgroup's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub cpu_usec: u64,
    /// `memory.peak`, page cache included.
    pub memory_peak: Option<u64>,
    pub memory_current: Option<u64>,
    /// Bytes written to disk (`io.stat` wbytes, every device).
    pub write_bytes: Option<u64>,
    /// `memory.events` `oom_kill`: processes the kernel killed for memory.
    pub oom_kills: u64,
    /// `memory.events` `oom_group_kill`: whole-job kills (memory.oom.group).
    pub oom_group_kills: u64,
    pub pids_peak: Option<u64>,
}

impl JobCgroup {
    /// A job cgroup from its directory descriptor.
    pub fn new(name: String, dir: OwnedFd, release: Arc<dyn Release>) -> Self {
        Self { name, dir, release }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The cgroup's directory, for `clone3`'s `CLONE_INTO_CGROUP`.
    pub fn dir(&self) -> BorrowedFd<'_> {
        self.dir.as_fd()
    }

    fn read(&self, file: &str) -> Option<String> {
        read_at(self.dir.as_fd(), file).ok()
    }

    /// The job's counters now.
    pub fn usage(&self) -> Usage {
        let field = |text: &Option<String>, key: &str| -> Option<u64> {
            text.as_deref()?.lines().find_map(|l| {
                let (k, v) = l.split_once(' ')?;
                (k == key).then(|| v.trim().parse().ok()).flatten()
            })
        };
        let number = |file: &str| self.read(file).and_then(|s| s.trim().parse().ok());
        let events = self.read("memory.events");
        let write_bytes = self.read("io.stat").map(|s| {
            s.split_whitespace()
                .filter_map(|w| w.strip_prefix("wbytes="))
                .filter_map(|n| n.parse::<u64>().ok())
                .fold(0_u64, u64::saturating_add)
        });
        Usage {
            cpu_usec: field(&self.read("cpu.stat"), "usage_usec").unwrap_or(0),
            memory_peak: number("memory.peak"),
            memory_current: number("memory.current"),
            write_bytes,
            oom_kills: field(&events, "oom_kill").unwrap_or(0),
            oom_group_kills: field(&events, "oom_group_kill").unwrap_or(0),
            pids_peak: number("pids.peak"),
        }
    }

    /// Whether the kernel killed the job for memory.
    pub fn oom_killed(&self) -> bool {
        let u = self.usage();
        u.oom_group_kills > 0 || u.oom_kills > 0
    }

    /// Kill every process in the cgroup (`cgroup.kill`): `setsid` daemons
    /// and double-forked servers included.
    pub fn kill(&self) -> io::Result<()> {
        write_at(self.dir.as_fd(), "cgroup.kill", "1")
    }

    pub fn pids(&self) -> Vec<u32> {
        self.read("cgroup.procs")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }
}

impl Drop for JobCgroup {
    fn drop(&mut self) {
        let _ = self.kill();
        self.release.release(&self.name);
    }
}

/// Read a file beneath a directory descriptor.
pub fn read_at(dir: BorrowedFd<'_>, file: &str) -> io::Result<String> {
    let mut f = open_at(dir, file, libc::O_RDONLY)?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

/// Write a file beneath a directory descriptor.
pub fn write_at(dir: BorrowedFd<'_>, file: &str, content: &str) -> io::Result<()> {
    open_at(dir, file, libc::O_WRONLY)?.write_all(content.as_bytes())
}

fn open_at(dir: BorrowedFd<'_>, file: &str, mode: libc::c_int) -> io::Result<std::fs::File> {
    let c = CString::new(file).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: openat on a live directory descriptor with a C string.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), mode | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor nobody else owns.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: open with a C string; the result is checked.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor nobody else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

pub fn apply_limits(
    dir: &Path,
    limits: &Limits,
    controllers: &[String],
) -> Result<(), ProviderError> {
    let has = |c: &str| controllers.iter().any(|x| x == c);
    let put = |file: &str, value: &str| {
        std::fs::write(dir.join(file), value).map_err(io_err(format_args!("{file} = {value}")))
    };
    if has("memory") {
        // An OOM ends the whole job, not one compiler in it.
        put("memory.oom.group", "1")?;
        // Containment means the limit holds; not every kernel has swap
        // accounting, so a missing file is fine.
        let _ = std::fs::write(dir.join("memory.swap.max"), "0");
        if let Some(max) = limits.memory_max {
            put("memory.max", &max.to_string())?;
        }
    }
    if has("pids")
        && let Some(max) = limits.pids_max
    {
        put("pids.max", &max.to_string())?;
    }
    if has("cpu") {
        if let Some(w) = limits.cpu_weight {
            put("cpu.weight", &w.clamp(1, 10_000).to_string())?;
        }
        if let Some(m) = limits.cpu_max {
            put("cpu.max", &format!("{} {}", m.quota_us, m.period_us))?;
        }
    }
    if has("io") {
        for line in &limits.io {
            put("io.max", &line.line())?;
        }
    }
    Ok(())
}

/// Kill everything in the cgroup at `path`, wait until it is empty, remove
/// it. Best effort: a cgroup that won't empty within a few seconds stays.
pub fn kill_and_remove(path: &Path) {
    let _ = std::fs::write(path.join("cgroup.kill"), "1");
    let deadline = Instant::now().checked_add(Duration::from_secs(5));
    loop {
        if std::fs::remove_dir(path).is_ok() || !path.exists() {
            return;
        }
        if deadline.is_none_or(|d| Instant::now() >= d) {
            return;
        }
        // Children (a job that made sub-cgroups through its delegated view
        // can't: its view is read-only) or processes still exiting.
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The cgroup v2 directory of process `pid` (`/proc/<pid>/cgroup`'s `0::`
/// line), under `mount`.
pub fn cgroup_of(pid: &str, mount: &Path) -> io::Result<PathBuf> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    let rel = text
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .ok_or_else(|| io::Error::other("no cgroup v2 hierarchy (a 0:: line)"))?;
    Ok(mount.join(rel.trim().trim_start_matches('/')))
}

/// A validated job name.
fn check_name(name: &str) -> Result<(), ProviderError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\0', '\n'])
        || name.len() > 200
    {
        return Err(ProviderError::BadName(name.to_owned()));
    }
    Ok(())
}

/// How [`Direct`] lays out the worker's cgroup.
#[derive(Debug, Clone)]
pub struct DirectOptions {
    /// The worker's cgroup; its own (`/proc/self/cgroup`) when `None`.
    pub root: Option<PathBuf>,
    /// The leaf the worker's processes move into.
    pub worker_leaf: String,
    pub jobs: String,
    /// Memory kept for the worker: `worker/memory.min`, and subtracted from
    /// the root's `memory.max` for the jobs' budget.
    pub reserve: u64,
}

impl Default for DirectOptions {
    fn default() -> Self {
        Self {
            root: None,
            worker_leaf: "worker".into(),
            jobs: "jobs".into(),
            reserve: 1 << 30,
        }
    }
}

/// Job cgroups made directly, in a cgroup this process may write.
pub struct Direct {
    jobs: PathBuf,
    controllers: Vec<String>,
    /// What the jobs together may hold (`jobs/memory.max`).
    budget: Option<u64>,
    state: Arc<Budget>,
}

impl std::fmt::Debug for Direct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Direct")
            .field("jobs", &self.jobs)
            .field("controllers", &self.controllers)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

/// The running jobs' `memory.max`, by name.
#[derive(Default)]
struct Budget {
    jobs: PathBuf,
    held: Mutex<HashMap<String, u64>>,
}

impl Release for Budget {
    fn release(&self, name: &str) {
        kill_and_remove(&self.jobs.join(name));
        if let Ok(mut held) = self.held.lock() {
            held.remove(name);
        }
    }
}

impl Direct {
    /// Take over the worker's cgroup: its processes move into
    /// `worker_leaf`, controllers are enabled for `jobs`, and job cgroups a
    /// previous worker left behind are killed and removed.
    ///
    /// # Errors
    /// Returns why the cgroup can't be managed here (not cgroup v2, not
    /// writable, no controllers delegated).
    pub fn init(options: &DirectOptions) -> Result<Self, ProviderError> {
        let root = match &options.root {
            Some(r) => r.clone(),
            None => cgroup_of("self", Path::new("/sys/fs/cgroup"))
                .map_err(io_err("finding this process's cgroup"))?,
        };
        let worker = root.join(&options.worker_leaf);
        let jobs = root.join(&options.jobs);
        for dir in [&worker, &jobs] {
            if !dir.is_dir() {
                std::fs::create_dir(dir)
                    .map_err(io_err(format_args!("creating {}", dir.display())))?;
            }
        }
        // cgroup v2 keeps processes in leaves only: everything in the root
        // (the worker, an init it runs under) moves to the worker's leaf.
        let procs = std::fs::read_to_string(root.join("cgroup.procs")).map_err(io_err(
            format_args!("reading {}/cgroup.procs", root.display()),
        ))?;
        for pid in procs.lines().filter(|l| !l.trim().is_empty()) {
            // A process may exit meanwhile; kernel threads can't move.
            let _ = std::fs::write(worker.join("cgroup.procs"), pid.trim());
        }
        let offered = std::fs::read_to_string(root.join("cgroup.controllers")).unwrap_or_default();
        let controllers: Vec<String> = CONTROLLERS
            .iter()
            .filter(|c| offered.split_whitespace().any(|o| o == **c))
            .map(|c| (*c).to_owned())
            .collect();
        let mut enable = String::new();
        for c in &controllers {
            enable.push('+');
            enable.push_str(c);
            enable.push(' ');
        }
        for dir in [&root, &jobs] {
            std::fs::write(dir.join("cgroup.subtree_control"), enable.trim()).map_err(io_err(
                format_args!("enabling {} in {}", enable.trim(), dir.display()),
            ))?;
        }
        let has_memory = controllers.iter().any(|c| c == "memory");
        let limit = std::fs::read_to_string(root.join("memory.max"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok());
        let budget = limit.map(|l| l.saturating_sub(options.reserve));
        if has_memory {
            let _ = std::fs::write(worker.join("memory.min"), options.reserve.to_string());
            if let Some(b) = budget {
                std::fs::write(jobs.join("memory.max"), b.to_string())
                    .map_err(io_err("setting the jobs' memory budget"))?;
            }
        }
        // Leftovers of a worker that died with jobs running.
        for entry in std::fs::read_dir(&jobs).into_iter().flatten().flatten() {
            if entry.path().is_dir() {
                kill_and_remove(&entry.path());
            }
        }
        Ok(Self {
            state: Arc::new(Budget {
                jobs: jobs.clone(),
                held: Mutex::default(),
            }),
            jobs,
            controllers,
            budget,
        })
    }

    pub fn controllers(&self) -> &[String] {
        &self.controllers
    }

    /// The memory the jobs may hold together, if the worker's cgroup has a
    /// limit.
    pub fn budget(&self) -> Option<u64> {
        self.budget
    }

    /// Admit a job's reservation against the budget, and record it.
    fn admit(&self, name: &str, limits: &Limits) -> Result<(), ProviderError> {
        let mut held = self
            .state
            .held
            .lock()
            .map_err(|_| ProviderError::Io("the budget lock is poisoned".into()))?;
        if let (Some(budget), Some(asked)) = (self.budget, limits.counted()) {
            let used = held
                .iter()
                .filter(|(n, _)| n.as_str() != name)
                .map(|(_, m)| *m)
                .fold(0_u64, u64::saturating_add);
            if used.saturating_add(asked) > budget {
                return Err(ProviderError::OverBudget {
                    asked,
                    used,
                    budget,
                });
            }
        }
        held.insert(name.to_owned(), limits.counted().unwrap_or(0));
        drop(held);
        Ok(())
    }
}

impl CgroupProvider for Direct {
    fn create(&self, name: &str, limits: &Limits) -> Result<JobCgroup, ProviderError> {
        check_name(name)?;
        let path = self.jobs.join(name);
        if path.exists() {
            kill_and_remove(&path);
        }
        self.admit(name, limits)?;
        let made = (|| {
            std::fs::create_dir(&path)
                .map_err(io_err(format_args!("creating {}", path.display())))?;
            apply_limits(&path, limits, &self.controllers)?;
            open_dir(&path).map_err(io_err(format_args!("opening {}", path.display())))
        })();
        match made {
            Ok(dir) => Ok(JobCgroup::new(
                name.to_owned(),
                dir,
                Arc::clone(&self.state) as Arc<dyn Release>,
            )),
            Err(e) => {
                self.state.release(name);
                Err(e)
            }
        }
    }

    fn set_quota(
        &self,
        dir: BorrowedFd<'_>,
        project: u32,
        bytes: u64,
    ) -> Result<QuotaOutcome, ProviderError> {
        crate::job::quota::set_project_quota(dir, project, bytes)
            .map_err(io_err("setting the project quota"))
    }
}

#[cfg(test)]
mod tests {
    use super::Limits;

    #[test]
    fn admission_counts_the_reservation_not_the_limit_above_it() {
        let gib = 1_u64 << 30;
        let reserved = Limits {
            memory_max: Some(6 * gib),
            memory_reserved: Some(2 * gib),
            ..Limits::default()
        };
        assert_eq!(reserved.counted(), Some(2 * gib));
        let unreserved = Limits {
            memory_max: Some(3 * gib),
            ..Limits::default()
        };
        assert_eq!(unreserved.counted(), Some(3 * gib));
    }
}
