//! Helpers of a job: the programs that serve it from outside its sandbox (pasta connecting its
//! network, virtiofsd sharing its files with a VM, passt, a relay to a socket of the caller's).
//!
//! A helper runs in a mount namespace of its own built from nothing: a tmpfs root with the
//! host's `/usr` and `/etc` read-only, the usual links or directories for `/bin`, `/sbin` and
//! `/lib*`, and the caller's binds. It holds no copy of the caller's other mounts, so a
//! filesystem the caller unmounts or destroys later (a job's dataset) is never kept busy by a
//! helper that happened to start while it was mounted.
//!
//! Otherwise a helper is the caller: the same user, in the initial user namespace, with the
//! caller's capabilities, which pasta needs to enter a namespace the caller made and virtiofsd
//! to serve files of every owner; each confines itself (namespaces, seccomp, dropped
//! capabilities). No Landlock and no seccomp filter of this crate's: they would refuse what
//! those programs do to confine themselves.
//!
//! A helper is born in the job's cgroup, so it is booked with the job and `cgroup.kill` ends
//! it with the job, and is detached (see [`crate::job::Launch::detached`]): it outlives the
//! thread and process that started it, as the job it serves does, and the caller's parent
//! reaps it. Its wait status goes to `status`: a pipe for a helper that daemonizes once it is
//! ready (pasta: its exit 0 says the namespace is configured), a file otherwise.

use std::ffi::OsString;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::PathBuf;

use super::linux::{plan::Plan, runner};
use super::{JobSpec, Landlock, Mount, Root, Seccomp, SpawnError};

/// One helper to start.
pub struct Helper<'a> {
    /// The program and its arguments; a program without a `/` is looked up in `env`'s `PATH`.
    pub argv: Vec<OsString>,
    /// Its whole environment.
    pub env: Vec<(OsString, OsString)>,
    /// What it sees besides the host's system directories.
    pub binds: Vec<Mount>,
    /// Device nodes it needs at the same path (`/dev/net/tun` for pasta).
    pub devices: Vec<PathBuf>,
    /// The cgroup it is born in (its directory).
    pub cgroup: BorrowedFd<'a>,
    /// A network namespace to run in instead of the caller's.
    pub netns: Option<BorrowedFd<'a>>,
    /// Where its 4-byte wait status goes when it exits.
    pub status: OwnedFd,
    pub stdout: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
    /// Raise its open file limit to the hard limit: virtiofsd holds a descriptor per inode
    /// the guest has looked up.
    pub nofile_max: bool,
}

/// Start `h`; returns its init's pid once its program runs.
///
/// # Errors
/// [`SpawnError`] naming the step that failed: a bind whose source is missing, the program not
/// found, the cgroup or network namespace refused.
pub fn spawn(h: Helper<'_>) -> Result<libc::pid_t, SpawnError> {
    let mut spec = JobSpec::new(Root::Host { extra: vec![] }, h.argv);
    spec.env = h.env;
    spec.mounts = h.binds;
    spec.devices = h.devices;
    spec.seccomp = Seccomp::Off;
    spec.landlock = Landlock::Off;
    spec.cgroup_view = false;
    let mut plan = Plan::new(&spec)?;
    plan.helper = true;
    plan.own_net = false;
    plan.nofile_max = h.nofile_max;
    let fds = runner::Fds {
        stdin: None,
        stdout: h.stdout,
        stderr: h.stderr,
        extra: Vec::new(),
    };
    let mode = runner::Mode {
        status: Some(h.status),
        netns: h.netns,
    };
    let job = runner::spawn(&plan, spec.ids, fds, Some(h.cgroup), None, mode)?;
    Ok(job.into_detached().0)
}
