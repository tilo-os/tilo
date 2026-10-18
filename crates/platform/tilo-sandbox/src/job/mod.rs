//! Run a command as an isolated job on a shared kernel.
//!
//! A job is born inside its own cgroup (`clone3` with `CLONE_INTO_CGROUP`)
//! with new user, mount, PID, network, IPC, UTS and cgroup namespaces. Its
//! root is read-only, with what it may write mounted on top; it holds no
//! capability, and runs under `no_new_privs`, Landlock and the CI seccomp
//! filter. `docs/job-sandbox.md` has the design, the threat model and every
//! setting.
//!
//! ```no_run
//! # #[cfg(target_os = "linux")] fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use tilo_sandbox::job::{JobSpec, Launch, Mount, Root};
//! let mut spec = JobSpec::new(Root::Host { extra: vec![] }, ["cargo", "test"]);
//! spec.mount(Mount::read_write("/work/src", "/src"));
//! spec.cwd = "/src".into();
//! let mut job = Launch::new(&spec).spawn()?;
//! let status = job.wait()?;
//! # let _ = status; Ok(()) }
//! # #[cfg(not(target_os = "linux"))] fn main() {}
//! ```
//!
//! The job's init is this program again, run as `tilo-job-init`: a program
//! that starts jobs puts `tilo_sandbox::job_init!();` in its crate root, which
//! runs the init before `main` when the process is one.
//!
//! The module exists on Linux only. `JobSpec` is portable data, so a macOS
//! backend (Seatbelt) can be added behind the same call.

#[cfg(target_os = "linux")]
pub mod cgroup;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub mod quota;
pub mod spec;
#[cfg(target_os = "linux")]
pub mod vm;

use std::os::fd::{BorrowedFd, OwnedFd, RawFd};

pub use spec::{
    CiProfile, IdBlock, IdMap, JobSpec, Landlock, Mount, MountKind, NESTING_ID, Root, Seccomp, Tmp,
};

#[cfg(target_os = "linux")]
pub use linux::runner::Job;

/// Run the job's init if this process was started as one, and never return;
/// return at once otherwise. [`crate::job_init`] calls it before `main`.
#[cfg(target_os = "linux")]
pub fn init_if_started_as_one() {
    linux::runner::init_if_started_as_one();
}

/// Make this program able to be a job's init, which every program that
/// starts jobs must be: a job's init is the program that started it, run
/// again as `tilo-job-init` (a new, small process rather than a copy of the
/// caller, whose memory may be large). Put it once in the crate root of the
/// binary (or test) that calls [`Launch::spawn`]: it runs the init before
/// `main` when the process is one (and before a test harness's, so tests
/// can start jobs too), and costs a read of `/proc/self/cmdline` otherwise.
#[macro_export]
macro_rules! job_init {
    () => {
        #[cfg(target_os = "linux")]
        #[used]
        #[unsafe(link_section = ".init_array")]
        static TILO_JOB_INIT: extern "C" fn() = {
            extern "C" fn tilo_job_init() {
                $crate::job::init_if_started_as_one();
            }
            tilo_job_init
        };
    };
}

/// Why a job could not start.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The spec itself is wrong (a relative path, a missing source).
    #[error("invalid job: {0}")]
    Invalid(String),
    /// This machine can't run jobs this way.
    #[error("not supported here: {0}")]
    Unsupported(String),
    /// A setup step failed, in words naming the step.
    #[error("could not set up the job: {0}")]
    Setup(String),
    /// The program could not be executed inside the sandbox.
    #[error("could not run `{program}` in the job: {error}")]
    Exec {
        program: String,
        error: std::io::Error,
    },
}

/// How a job ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// A job whose namespaces exist but whose setup waits for the caller: its
/// ids are mapped, `/proc/<pid>/ns/net` names its network namespace (for
/// pasta and nftables), nothing of the job has run yet.
#[derive(Debug, Clone, Copy)]
pub struct Started {
    pub pid: i32,
}

type SetupHook<'a> = Box<dyn FnMut(&Started) -> Result<(), String> + 'a>;

/// Starting one job.
pub struct Launch<'a> {
    spec: &'a JobSpec,
    stdin: Option<OwnedFd>,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
    extra: Vec<(OwnedFd, RawFd)>,
    cgroup: Option<BorrowedFd<'a>>,
    setup: Option<SetupHook<'a>>,
}

impl<'a> Launch<'a> {
    /// A launch of `spec`, with stdio on `/dev/null` and no cgroup.
    pub fn new(spec: &'a JobSpec) -> Self {
        Self {
            spec,
            stdin: None,
            stdout: None,
            stderr: None,
            extra: Vec::new(),
            cgroup: None,
            setup: None,
        }
    }

    #[must_use]
    pub fn stdin(mut self, fd: OwnedFd) -> Self {
        self.stdin = Some(fd);
        self
    }

    #[must_use]
    pub fn stdout(mut self, fd: OwnedFd) -> Self {
        self.stdout = Some(fd);
        self
    }

    #[must_use]
    pub fn stderr(mut self, fd: OwnedFd) -> Self {
        self.stderr = Some(fd);
        self
    }

    /// Start the job inside this cgroup (its directory descriptor).
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn cgroup(mut self, cgroup: &'a cgroup::JobCgroup) -> Self {
        self.cgroup = Some(cgroup.dir());
        self
    }

    /// Start the job inside the cgroup named by this directory descriptor,
    /// for a caller that makes and limits its own cgroups and only wants the
    /// atomic placement (`CLONE_INTO_CGROUP`): t, whose jobs (micro-VM
    /// steps' QEMU among them) have cgroups it meters itself. The descriptor
    /// must stay open until [`Launch::spawn`] returns.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn cgroup_fd(mut self, dir: BorrowedFd<'a>) -> Self {
        self.cgroup = Some(dir);
        self
    }

    /// Run `hook` after the job's namespaces exist and before any of it
    /// runs: attach its network, for instance. An error stops the job.
    #[must_use]
    pub fn before_start(mut self, hook: impl FnMut(&Started) -> Result<(), String> + 'a) -> Self {
        self.setup = Some(Box::new(hook));
        self
    }

    /// Start the job. Returns once the command runs (its `execve`
    /// succeeded), or with the step that failed. The calling program must
    /// be able to be the job's init: see [`crate::job_init`].
    ///
    /// # Errors
    /// See [`SpawnError`].
    #[cfg(target_os = "linux")]
    pub fn spawn(mut self) -> Result<Job, SpawnError> {
        let plan = linux::plan::Plan::new(self.spec)?;
        let fds = linux::runner::Fds {
            stdin: self.stdin,
            stdout: self.stdout,
            stderr: self.stderr,
            extra: self.extra,
        };
        let hook = self
            .setup
            .as_mut()
            .map(|h| h.as_mut() as &mut dyn FnMut(&Started) -> Result<(), String>);
        linux::runner::spawn(&plan, self.spec.ids, fds, self.cgroup, hook)
    }

    /// # Errors
    /// Always: only Linux runs jobs.
    #[cfg(not(target_os = "linux"))]
    pub fn spawn(self) -> Result<std::convert::Infallible, SpawnError> {
        let _ = (
            self.spec,
            self.stdin,
            self.stdout,
            self.stderr,
            self.extra,
            self.cgroup,
            self.setup,
        );
        Err(SpawnError::Unsupported(format!(
            "jobs run only on Linux; this is {}",
            std::env::consts::OS
        )))
    }
}

/// Create the mount points `spec` needs in an image root, as the caller,
/// before the job starts: the job itself can't write into its read-only
/// root. Directories for directory sources, empty files for file sources.
///
/// # Errors
/// Returns the first mount point that could not be made.
pub fn prepare_mount_points(spec: &JobSpec) -> std::io::Result<()> {
    let Root::Image(root) = &spec.root else {
        return Ok(());
    };
    let mut points: Vec<(std::path::PathBuf, bool)> = ["/proc", "/dev", "/tmp"]
        .iter()
        .map(|p| (p.into(), false))
        .collect();
    if spec.cgroup_view {
        points.push(("/sys".into(), false));
    }
    for m in &spec.mounts {
        let file = m
            .source()
            .is_some_and(|s| s.metadata().is_ok_and(|md| !md.is_dir()));
        points.push((m.dst.clone(), file));
    }
    for (inside, file) in points {
        let at = root.join(inside.strip_prefix("/").unwrap_or(&inside));
        // A mount point under another mount is made in that mount, by the job.
        if spec
            .mounts
            .iter()
            .any(|m| inside != m.dst && inside.starts_with(&m.dst))
        {
            continue;
        }
        if at.symlink_metadata().is_ok() {
            continue;
        }
        if file {
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&at, "")?;
        } else {
            std::fs::create_dir_all(&at)?;
        }
    }
    Ok(())
}
