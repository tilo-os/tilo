//! Starting a job: `clone3` into its cgroup and namespaces, then the child's
//! setup, which runs on memory [`Plan`] prepared and makes system calls only.

use std::ffi::CString;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use super::plan::{Op, Plan};
use super::sys;
use crate::job::{ExitStatus, SpawnError, Started};

/// What the job's init reports a failed setup step as: the step, the index
/// of the operation within it, and `errno`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum Step {
    Private = 1,
    Stage,
    PivotStage,
    Op,
    Pivot,
    Hostname,
    Loopback,
    Cwd,
    Caps,
    NoNewPrivs,
    LandlockRule,
    Landlock,
    Seccomp,
    Fork,
    Exec,
    Ids,
}

impl Step {
    fn from_u32(n: u32) -> Option<Self> {
        use Step::{
            Caps, Cwd, Exec, Fork, Hostname, Ids, Landlock, LandlockRule, Loopback, NoNewPrivs, Op,
            Pivot, PivotStage, Private, Seccomp, Stage,
        };
        [
            Private,
            Stage,
            PivotStage,
            Op,
            Pivot,
            Hostname,
            Loopback,
            Cwd,
            Caps,
            NoNewPrivs,
            LandlockRule,
            Landlock,
            Seccomp,
            Fork,
            Exec,
            Ids,
        ]
        .into_iter()
        .find(|s| *s as u32 == n)
    }
}

/// Exit status of the job's init when it could not report anything.
const EXIT_UNREPORTED: i32 = 125;

/// Everything the child reads, borrowed from the parent's memory (a copy of
/// it, after `clone3`).
struct Child<'a> {
    plan: &'a Plan,
    argv: &'a [*const libc::c_char],
    envp: &'a [*const libc::c_char],
    /// (descriptor in the parent, descriptor number in the child).
    moves: &'a [(RawFd, RawFd)],
    /// One slot per move, for the descriptor's temporary copy.
    scratch: &'a mut [RawFd],
    /// Duplicates go at or above this, clear of every source and target.
    above: RawFd,
    /// Descriptors at or above this are closed.
    keep_below: RawFd,
    sync: RawFd,
    status: RawFd,
    report: RawFd,
    landlock: Option<RawFd>,
    /// Where the plan's idmapped trees are, in order.
    trees_at: RawFd,
}

fn errno() -> u32 {
    // SAFETY: reads this thread's errno.
    let e = unsafe { *libc::__errno_location() };
    u32::try_from(e).unwrap_or(0)
}

impl Child<'_> {
    /// Report a failed step on the report pipe and end the init.
    fn fail(&self, step: Step, index: usize, errno: u32) -> ! {
        let mut msg = [0_u8; 12];
        let words = [step as u32, u32::try_from(index).unwrap_or(u32::MAX), errno];
        for (chunk, w) in msg.chunks_exact_mut(4).zip(words) {
            chunk.copy_from_slice(&w.to_ne_bytes());
        }
        // SAFETY: writes a stack buffer to a pipe; then `_exit`, which does
        // not unwind or run destructors.
        unsafe {
            libc::write(self.report, msg.as_ptr().cast(), msg.len());
            libc::_exit(EXIT_UNREPORTED)
        }
    }

    fn check(&self, rc: libc::c_long, step: Step, index: usize) {
        if rc < 0 {
            self.fail(step, index, errno());
        }
    }

    /// Put every descriptor the job keeps at its number, and close the rest:
    /// another job's pipe or the worker's server connection must not stay
    /// open in this one.
    ///
    /// # Safety
    /// Only in the child, before anything else.
    unsafe fn arrange_fds(&mut self) {
        for (i, (src, _)) in self.moves.iter().enumerate() {
            // SAFETY: duplicating a descriptor this process holds.
            let d = unsafe { libc::fcntl(*src, libc::F_DUPFD_CLOEXEC, self.above) };
            match self.scratch.get_mut(i) {
                Some(slot) if d >= 0 => *slot = d,
                // SAFETY: `_exit` does not unwind.
                _ => unsafe { libc::_exit(EXIT_UNREPORTED) },
            }
        }
        for ((_, target), tmp) in self.moves.iter().zip(self.scratch.iter()) {
            // SAFETY: `tmp` is a copy made above, `target` below `above`.
            if unsafe { libc::dup2(*tmp, *target) } < 0 {
                // SAFETY: `_exit` does not unwind.
                unsafe { libc::_exit(EXIT_UNREPORTED) };
            }
        }
        for fd in 3..self.keep_below {
            if !self.moves.iter().any(|(_, t)| *t == fd) {
                // SAFETY: closing a descriptor number this process may hold.
                unsafe { libc::close(fd) };
            }
        }
        let first = u32::try_from(self.keep_below).unwrap_or(u32::MAX);
        // SAFETY: closes every descriptor from `keep_below` up.
        unsafe { libc::syscall(libc::SYS_close_range, first, u32::MAX, 0_u32) };
    }

    /// Wait for the worker to map the job's ids and run its setup hook.
    fn await_parent(&self) {
        let mut byte = 0_u8;
        // SAFETY: reads one byte into a stack variable.
        let n = unsafe { libc::read(self.sync, (&raw mut byte).cast(), 1) };
        if n != 1 {
            // The worker gave up on this job (and closed the pipe).
            // SAFETY: `_exit` does not unwind.
            unsafe { libc::_exit(EXIT_UNREPORTED) };
        }
        // SAFETY: closing our own descriptor.
        unsafe { libc::close(self.sync) };
    }

    /// Take on the job's ids, where it has host ids of its own: the init
    /// starts as the worker's uid, which the job's namespace doesn't map.
    /// It keeps its capabilities in the namespace (it was never its root),
    /// and drops the worker's supplementary groups, which would otherwise
    /// still grant their access on the host.
    fn become_ids(&self) {
        let Some((uid, gid)) = self.plan.become_ids else {
            return;
        };
        // SAFETY: raw system calls with integer arguments: libc's wrappers
        // would try to change the ids of threads that exist only in the
        // worker.
        unsafe {
            self.check(
                libc::syscall(
                    libc::SYS_setgroups,
                    0_usize,
                    std::ptr::null::<libc::gid_t>(),
                ),
                Step::Ids,
                0,
            );
            self.check(
                libc::syscall(libc::SYS_setresgid, gid, gid, gid),
                Step::Ids,
                1,
            );
            self.check(
                libc::syscall(libc::SYS_setresuid, uid, uid, uid),
                Step::Ids,
                2,
            );
        }
    }

    fn reset_signals() {
        for sig in 1..=64 {
            if sig == libc::SIGKILL || sig == libc::SIGSTOP {
                continue;
            }
            // SAFETY: a zeroed sigaction is SIG_DFL with no flags; failures
            // (glibc's reserved signals) are harmless.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = libc::SIG_DFL;
                libc::sigaction(sig, &raw const sa, std::ptr::null_mut());
            }
        }
        // SAFETY: an empty set, applied to this thread.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &raw const set, std::ptr::null_mut());
        }
    }

    /// Make the staging root, run every mount operation, pivot into the new
    /// root.
    fn build_root(&self) {
        let plan = self.plan;
        // SAFETY (this block): mount and directory calls on C strings the
        // plan owns or static literals.
        unsafe {
            self.check(
                libc::c_long::from(libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    libc::MS_REC | libc::MS_PRIVATE,
                    std::ptr::null(),
                )),
                Step::Private,
                0,
            );
            self.check(
                libc::c_long::from(libc::mount(
                    c"tmpfs".as_ptr(),
                    plan.stage.as_ptr(),
                    c"tmpfs".as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    c"mode=0755,size=2m".as_ptr().cast(),
                )),
                Step::Stage,
                0,
            );
            self.check(
                libc::c_long::from(libc::chdir(plan.stage.as_ptr())),
                Step::Stage,
                1,
            );
            libc::mkdir(plan.stage_old.as_ptr(), 0o755);
            libc::mkdir(plan.stage_new.as_ptr(), 0o755);
            self.check(
                libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), plan.stage_old.as_ptr()),
                Step::PivotStage,
                0,
            );
            libc::chdir(c"/".as_ptr());
        }
        for (i, op) in plan.ops.iter().enumerate() {
            // SAFETY: each call takes C strings the plan owns.
            let rc = unsafe { run_op(op, self.trees_at) };
            if rc < 0 && op.fatal() {
                self.fail(Step::Op, i, errno());
            }
        }
        // SAFETY: the new root is a mount point (bind or tmpfs above);
        // `pivot_root(".", ".")` stacks the old root on it, and the detach
        // removes it.
        unsafe {
            self.check(
                libc::c_long::from(libc::chdir(plan.new_root.as_ptr())),
                Step::Pivot,
                0,
            );
            self.check(
                libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".".as_ptr()),
                Step::Pivot,
                1,
            );
            self.check(
                libc::c_long::from(libc::umount2(c".".as_ptr(), libc::MNT_DETACH)),
                Step::Pivot,
                2,
            );
            self.check(
                libc::c_long::from(libc::chdir(c"/".as_ptr())),
                Step::Pivot,
                3,
            );
        }
    }

    fn name_and_network(&self) {
        let host = self.plan.hostname.as_bytes();
        // SAFETY: a byte buffer the plan owns, with its length.
        let rc = unsafe { libc::sethostname(host.as_ptr().cast(), host.len()) };
        self.check(libc::c_long::from(rc), Step::Hostname, 0);
        // SAFETY: an ioctl on a socket we open and close, with a structure of
        // the layout SIOC[GS]IFFLAGS expect.
        unsafe {
            let s = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
            self.check(libc::c_long::from(s), Step::Loopback, 0);
            let mut ifr = sys::IfreqFlags {
                name: *b"lo\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
                flags: 0,
                pad: [0; 22],
            };
            self.check(
                libc::c_long::from(libc::ioctl(s, libc::SIOCGIFFLAGS, &raw mut ifr)),
                Step::Loopback,
                1,
            );
            #[allow(
                clippy::cast_possible_truncation,
                reason = "IFF_UP and IFF_RUNNING fit the 16-bit ifr_flags"
            )]
            let up = (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            ifr.flags |= up;
            self.check(
                libc::c_long::from(libc::ioctl(s, libc::SIOCSIFFLAGS, &raw const ifr)),
                Step::Loopback,
                2,
            );
            libc::close(s);
        }
        // SAFETY: a C string the plan owns.
        let rc = unsafe { libc::chdir(self.plan.cwd.as_ptr()) };
        self.check(libc::c_long::from(rc), Step::Cwd, 0);
    }

    /// Drop every capability for good: an `execve` as uid 0 regains none.
    fn drop_capabilities(&self) {
        // SAFETY (this block): prctl and capset with constant arguments and a
        // stack structure.
        unsafe {
            self.check(
                libc::c_long::from(libc::prctl(
                    libc::PR_SET_SECUREBITS,
                    sys::SECUREBITS_LOCKED_DOWN,
                    0,
                    0,
                    0,
                )),
                Step::Caps,
                0,
            );
            for cap in 0..=self.plan.cap_last {
                libc::prctl(libc::PR_CAPBSET_DROP, libc::c_ulong::from(cap), 0, 0, 0);
            }
            self.check(
                libc::c_long::from(libc::prctl(
                    libc::PR_CAP_AMBIENT,
                    libc::PR_CAP_AMBIENT_CLEAR_ALL,
                    0,
                    0,
                    0,
                )),
                Step::Caps,
                1,
            );
            let header = sys::CapHeader {
                version: sys::LINUX_CAPABILITY_VERSION_3,
                pid: 0,
            };
            let data = [sys::CapData::default(); 2];
            self.check(
                libc::syscall(libc::SYS_capset, &raw const header, data.as_ptr()),
                Step::Caps,
                2,
            );
            self.check(
                libc::c_long::from(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)),
                Step::NoNewPrivs,
                0,
            );
        }
    }

    fn landlock(&self) {
        let (Some(ruleset), Some((_, rules))) = (self.landlock, self.plan.landlock.as_ref()) else {
            return;
        };
        for (i, rule) in rules.iter().enumerate() {
            // SAFETY: opens a path the plan owns, O_PATH: no access is used.
            let fd = unsafe { libc::open(rule.path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if fd < 0 {
                if errno() == libc::ENOENT.unsigned_abs() {
                    continue;
                }
                self.fail(Step::LandlockRule, i, errno());
            }
            let attr = sys::LandlockPathBeneath {
                allowed_access: rule.access,
                parent_fd: fd,
            };
            // SAFETY: a live attribute structure for the rule type passed.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_landlock_add_rule,
                    ruleset,
                    sys::LANDLOCK_RULE_PATH_BENEATH,
                    &raw const attr,
                    0_u32,
                )
            };
            // SAFETY: closing the descriptor opened above.
            unsafe { libc::close(fd) };
            self.check(rc, Step::LandlockRule, i);
        }
        // SAFETY: the ruleset descriptor this process holds.
        let rc = unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0_u32) };
        self.check(rc, Step::Landlock, 0);
        // SAFETY: closing our own descriptor.
        unsafe { libc::close(ruleset) };
    }

    fn seccomp(&self) {
        for (i, program) in self.plan.seccomp.iter().enumerate() {
            let Ok(len) = u16::try_from(program.len()) else {
                self.fail(Step::Seccomp, i, libc::E2BIG.unsigned_abs());
            };
            let prog = libc::sock_fprog {
                len,
                filter: program.as_ptr().cast_mut(),
            };
            // SAFETY: the filter program lives in the plan for the call.
            let rc = unsafe {
                libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const prog,
                    0,
                    0,
                )
            };
            self.check(libc::c_long::from(rc), Step::Seccomp, i);
        }
    }

    /// Hide what this process inherited from the worker: not traceable and
    /// its `/proc` files unreadable to the job, its environment blanked.
    fn hide_worker(&self) {
        // SAFETY: prctl with constants; the areas are this process's own
        // stack pages (argument and environment strings), writable.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            let (start, end) = self.plan.env_area;
            if let Some(len) = end.checked_sub(start).filter(|l| *l > 0 && start > 0) {
                std::ptr::write_bytes(std::ptr::with_exposed_provenance_mut::<u8>(start), 0, len);
            }
            let (start, end) = self.plan.arg_area;
            if let Some(len) = end.checked_sub(start).filter(|l| *l > 0 && start > 0) {
                let at = std::ptr::with_exposed_provenance_mut::<u8>(start);
                std::ptr::write_bytes(at, 0, len);
                let name = b"job-init";
                if len > name.len() {
                    std::ptr::copy_nonoverlapping(name.as_ptr(), at, name.len());
                }
            }
        }
    }

    /// The command: a session of its own, its stdio, `execve`.
    fn exec(&self) -> ! {
        // SAFETY (this block): process calls on descriptors this process
        // holds and argument vectors the parent built, NULL-terminated.
        unsafe {
            libc::setsid();
            libc::close(self.status);
            libc::fcntl(self.report, libc::F_SETFD, libc::FD_CLOEXEC);
            let mut last = libc::ENOENT.unsigned_abs();
            for path in &self.plan.candidates {
                libc::execve(path.as_ptr(), self.argv.as_ptr(), self.envp.as_ptr());
                let e = errno();
                if e != libc::ENOENT.unsigned_abs() && e != libc::ENOTDIR.unsigned_abs() {
                    last = e;
                }
            }
            self.fail(Step::Exec, 0, last);
        }
    }

    /// The init's last job: reap until the command has exited, hand its
    /// status to the worker, exit. The kernel then kills whatever is left in
    /// the PID namespace.
    fn reap(&self, command: libc::pid_t) -> ! {
        // SAFETY (this block): closes descriptors the command keeps (so its
        // pipes see EOF when it exits), waits on children, writes a stack
        // buffer and exits.
        unsafe {
            libc::close(self.report);
            for (_, target) in self.moves {
                if *target != self.status {
                    libc::close(*target);
                }
            }
            let mut status = 0;
            loop {
                let mut st = 0;
                let r = libc::wait4(-1, &raw mut st, 0, std::ptr::null_mut());
                if r == command {
                    status = st;
                    break;
                }
                if r < 0 && errno() != libc::EINTR.unsigned_abs() {
                    break;
                }
            }
            libc::write(self.status, (&raw const status).cast(), 4);
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                libc::WTERMSIG(status).saturating_add(128)
            };
            libc::_exit(code)
        }
    }

    /// The job's init, from `clone3` to `_exit`.
    ///
    /// # Safety
    /// Only in the child of `clone3`. Allocates nothing and takes no lock.
    unsafe fn run(&mut self) -> ! {
        // SAFETY: the first thing the child does.
        unsafe { self.arrange_fds() };
        self.await_parent();
        // SAFETY: tie the init to the worker: if the worker dies, so does
        // the job.
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
        Self::reset_signals();
        self.become_ids();
        self.build_root();
        self.name_and_network();
        self.drop_capabilities();
        self.landlock();
        self.seccomp();
        self.hide_worker();
        // SAFETY: a plain fork (no namespace flags), as raw `clone` so no
        // libc fork handler runs in this copy of a threaded process.
        let pid = unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0) };
        self.check(pid, Step::Fork, 0);
        if pid == 0 {
            self.exec();
        }
        self.reap(libc::pid_t::try_from(pid).unwrap_or(-1))
    }
}

/// One mount operation; the plan's idmapped trees are at `trees_at` on.
///
/// # Safety
/// Takes the C strings `op` owns; for the child only.
unsafe fn run_op(op: &Op, trees_at: RawFd) -> libc::c_long {
    let opt = |c: &Option<CString>| c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    // SAFETY: every pointer is a live C string or null where the call
    // allows it.
    unsafe {
        match op {
            Op::Mkdir(p) => libc::c_long::from(libc::mkdir(p.as_ptr(), 0o755)),
            Op::Touch(p) => {
                let fd = libc::open(
                    p.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0o644,
                );
                if fd >= 0 {
                    libc::close(fd);
                }
                libc::c_long::from(fd)
            }
            Op::Symlink { target, path } => {
                libc::c_long::from(libc::symlink(target.as_ptr(), path.as_ptr()))
            }
            Op::Mount {
                src,
                dst,
                fstype,
                flags,
                data,
            } => libc::c_long::from(libc::mount(
                opt(src),
                dst.as_ptr(),
                opt(fstype),
                *flags,
                opt(data).cast(),
            )),
            Op::SetAttr {
                path,
                set,
                recursive,
            } => {
                let attr = sys::MountAttr {
                    attr_set: *set,
                    ..sys::MountAttr::default()
                };
                libc::syscall(
                    libc::SYS_mount_setattr,
                    libc::AT_FDCWD,
                    path.as_ptr(),
                    if *recursive { sys::AT_RECURSIVE } else { 0 },
                    &raw const attr,
                    std::mem::size_of::<sys::MountAttr>(),
                )
            }
            Op::Write { path, contents } => {
                let fd = libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                    0o644,
                );
                if fd < 0 {
                    return libc::c_long::from(fd);
                }
                let mut rest: &[u8] = contents;
                while !rest.is_empty() {
                    let n = libc::write(fd, rest.as_ptr().cast(), rest.len());
                    let Ok(n) = usize::try_from(n) else {
                        libc::close(fd);
                        return -1;
                    };
                    rest = rest.get(n..).unwrap_or_default();
                }
                // Root's where the job's namespace maps root (a job with a
                // block of ids); the job's own otherwise.
                libc::fchown(fd, 0, 0);
                libc::c_long::from(libc::close(fd))
            }
            Op::Attach { tree, dst } => {
                let fd = RawFd::try_from(*tree)
                    .ok()
                    .and_then(|t| trees_at.checked_add(t))
                    .unwrap_or(-1);
                libc::syscall(
                    libc::SYS_move_mount,
                    fd,
                    c"".as_ptr(),
                    libc::AT_FDCWD,
                    dst.as_ptr(),
                    sys::MOVE_MOUNT_F_EMPTY_PATH,
                )
            }
        }
    }
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 fills `fds` with two new descriptors on success.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors are new and ours alone.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn dev_null() -> io::Result<OwnedFd> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?
        .into())
}

fn write_proc(pid: libc::pid_t, file: &str, content: &str) -> Result<(), SpawnError> {
    std::fs::write(format!("/proc/{pid}/{file}"), content).map_err(|e| {
        SpawnError::Setup(format!(
            "writing the job's {file} ({}): {e}",
            content.trim()
        ))
    })
}

/// A started job's init, until it is reaped.
#[derive(Debug)]
pub struct Job {
    pid: libc::pid_t,
    pidfd: OwnedFd,
    /// The init writes the command's wait status here before it exits.
    status: std::fs::File,
    exit: Option<ExitStatus>,
    unmapped: Vec<std::path::PathBuf>,
}

/// `P_PIDFD` for `waitid`.
const P_PIDFD: libc::idtype_t = 3;

/// A caller hook run after the job's namespaces exist, before it runs.
pub type SetupFn<'a> = &'a mut dyn FnMut(&Started) -> Result<(), String>;

impl Job {
    /// Writable host paths bound without an idmap because their filesystem
    /// can't take one, for a job with a block of host ids: it runs anyway, but
    /// may not write their owner's files. Empty without a block.
    pub fn unmapped(&self) -> &[std::path::PathBuf] {
        &self.unmapped
    }

    /// The init's pid, in the caller's PID namespace.
    pub fn pid(&self) -> libc::pid_t {
        self.pid
    }

    /// Becomes readable when the job has ended; then [`Job::try_wait`]
    /// returns its status.
    pub fn pidfd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.pidfd.as_fd()
    }

    /// Kill the job: its init, and with it every process in its PID
    /// namespace.
    pub fn kill(&self) {
        // SAFETY: signals the process the pidfd names; harmless once it has
        // exited.
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            );
        }
    }

    fn reap(&mut self, flags: libc::c_int) -> io::Result<Option<ExitStatus>> {
        if let Some(e) = self.exit {
            return Ok(Some(e));
        }
        loop {
            // SAFETY: a zeroed siginfo for waitid to fill.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: waits on our own child through its pidfd.
            let rc = unsafe {
                libc::waitid(
                    P_PIDFD,
                    libc::id_t::try_from(self.pidfd.as_raw_fd()).unwrap_or_default(),
                    &raw mut info,
                    libc::WEXITED | flags,
                )
            };
            if rc != 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            // SAFETY: waitid filled `info` (si_pid 0 means nothing exited).
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            let exit = self.status_after(&info);
            self.exit = Some(exit);
            return Ok(Some(exit));
        }
    }

    fn status_after(&self, info: &libc::siginfo_t) -> ExitStatus {
        let mut raw = [0_u8; 4];
        // The init has exited, so the pipe holds the status or is at EOF.
        let n = (&self.status).read(&mut raw).unwrap_or(0);
        if n == 4 {
            let st = i32::from_ne_bytes(raw);
            if libc::WIFEXITED(st) {
                return ExitStatus {
                    code: Some(libc::WEXITSTATUS(st)),
                    signal: None,
                };
            }
            if libc::WIFSIGNALED(st) {
                return ExitStatus {
                    code: None,
                    signal: Some(libc::WTERMSIG(st)),
                };
            }
        }
        // The init itself was killed (an OOM, `kill`, `cgroup.kill`).
        // SAFETY: waitid filled `info`.
        let status = unsafe { info.si_status() };
        if info.si_code == libc::CLD_EXITED {
            ExitStatus {
                code: Some(status),
                signal: None,
            }
        } else {
            ExitStatus {
                code: None,
                signal: Some(status),
            }
        }
    }

    /// The job's status if it has ended, without waiting.
    ///
    /// # Errors
    /// Returns the error `waitid` gave.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.reap(libc::WNOHANG)
    }

    /// Wait for the job to end.
    ///
    /// # Errors
    /// Returns the error `waitid` gave.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.reap(0)?
            .ok_or_else(|| io::Error::other("waitid returned without a status"))
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.exit.is_none() {
            self.kill();
            let _ = self.wait();
        }
    }
}

/// Descriptors the job gets.
pub struct Fds {
    pub stdin: Option<OwnedFd>,
    pub stdout: Option<OwnedFd>,
    pub stderr: Option<OwnedFd>,
    /// (descriptor, its number in the job), numbers 3 and up.
    pub extra: Vec<(OwnedFd, RawFd)>,
}

/// How long the job's setup may wait for anything but a CPU before the worker
/// gives up on it: a mount that hangs, a lock that is never released.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a setup that has not reported is looked at.
const SETUP_CHECK_EVERY: Duration = Duration::from_secs(1);

/// How long the job's setup has spent waiting for a CPU, so far: the time
/// `pid` and its children spent ready to run (`/proc/<pid>/schedstat`), or,
/// if longer, the time some task of their cgroup did (its `cpu.pressure`).
/// A setup step can wait on a lock that a task of the same cgroup holds while
/// it waits for a CPU: that wait is for a CPU too. Zero where the kernel keeps
/// neither statistic.
fn cpu_wait(pid: libc::pid_t, cgroup: Option<&std::path::Path>) -> Duration {
    let one = |p: &str| {
        std::fs::read_to_string(format!("/proc/{p}/schedstat"))
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let children =
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    let ready = children
        .split_whitespace()
        .map(one)
        .fold(one(&pid.to_string()), u64::saturating_add);
    let pressure = cgroup
        .and_then(|dir| std::fs::read_to_string(dir.join("cpu.pressure")).ok())
        .and_then(|text| {
            let some = text.lines().find(|l| l.starts_with("some "))?;
            some.split_whitespace()
                .find_map(|f| f.strip_prefix("total="))?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0);
    Duration::from_nanos(ready).max(Duration::from_micros(pressure))
}

/// Wait for the report pipe to close (the command is running) or to say
/// which step failed: `Ok(None)` when it closes, `Ok(Some(_))` with the
/// 12-byte failure record.
///
/// On a busy machine the job's setup waits for a CPU, at the share its cgroup
/// gets: the time it spends ready to run doesn't count against
/// `SETUP_TIMEOUT`, only the time it spends waiting for anything else.
fn read_report_bytes(report: OwnedFd, pid: libc::pid_t) -> Result<Option<[u8; 12]>, SpawnError> {
    let start = Instant::now();
    let cgroup =
        crate::job::cgroup::cgroup_of(&pid.to_string(), std::path::Path::new("/sys/fs/cgroup"))
            .ok();
    let cgroup = cgroup.as_deref();
    let before = cpu_wait(pid, cgroup);
    let mut f = std::fs::File::from(report);
    let mut buf = [0_u8; 12];
    let mut got = 0_usize;
    loop {
        let runnable = cpu_wait(pid, cgroup).saturating_sub(before);
        let waited = start.elapsed().saturating_sub(runnable);
        if waited >= SETUP_TIMEOUT {
            return Err(SpawnError::Setup(format!(
                "the job did not start within {SETUP_TIMEOUT:?} (besides {:.1}s waiting for a CPU)",
                runnable.as_secs_f64()
            )));
        }
        let left = SETUP_TIMEOUT.saturating_sub(waited).min(SETUP_CHECK_EVERY);
        let mut pfd = libc::pollfd {
            fd: f.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(left.as_millis())
            .unwrap_or(libc::c_int::MAX)
            .max(1);
        // SAFETY: one pollfd structure on the stack.
        let rc = unsafe { libc::poll(&raw mut pfd, 1, ms) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(SpawnError::Setup(format!("waiting for the job: {e}")));
        }
        if rc == 0 {
            continue;
        }
        let Some(rest) = buf.get_mut(got..) else {
            break;
        };
        match f.read(rest) {
            Ok(0) => break,
            Ok(n) => {
                got = got.saturating_add(n);
                if got >= buf.len() {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(SpawnError::Setup(format!("reading the job's report: {e}"))),
        }
    }
    Ok((got > 0).then_some(buf))
}

/// Turn a failed step into a message naming the step and its path.
fn describe_failure(buf: [u8; 12], plan: &Plan) -> SpawnError {
    let word = |i: usize| {
        buf.get(i..i.saturating_add(4))
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, u32::from_ne_bytes)
    };
    let (step, index, errno) = (word(0), word(4), word(8));
    let os = io::Error::from_raw_os_error(i32::try_from(errno).unwrap_or(libc::EIO));
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    let what = match Step::from_u32(step) {
        Some(Step::Op) => plan
            .ops
            .get(index)
            .map_or_else(|| "building the root".to_owned(), Op::describe),
        Some(Step::Private) => "making the mount tree private".into(),
        Some(Step::Stage | Step::PivotStage) => "staging the new root".into(),
        Some(Step::Pivot) => "entering the new root".into(),
        Some(Step::Hostname) => "setting the hostname".into(),
        Some(Step::Loopback) => "bringing up lo".into(),
        Some(Step::Cwd) => format!("entering {}", plan.cwd.to_string_lossy()),
        Some(Step::Caps) => "dropping capabilities".into(),
        Some(Step::NoNewPrivs) => "setting no_new_privs".into(),
        Some(Step::LandlockRule) => plan
            .landlock
            .as_ref()
            .and_then(|(_, r)| r.get(index))
            .map_or_else(
                || "adding a Landlock rule".to_owned(),
                |r| format!("adding the Landlock rule for {}", r.path.to_string_lossy()),
            ),
        Some(Step::Landlock) => "enforcing Landlock".into(),
        Some(Step::Seccomp) => "installing the seccomp filter".into(),
        Some(Step::Fork) => "starting the command".into(),
        Some(Step::Ids) => "taking on the job's own ids".into(),
        Some(Step::Exec) => {
            let program = plan
                .argv
                .first()
                .map(|a| a.to_string_lossy().into_owned())
                .unwrap_or_default();
            return SpawnError::Exec { program, error: os };
        }
        None => "setting up the job".into(),
    };
    let hint = match (Step::from_u32(step), os.raw_os_error()) {
        (Some(Step::Op), Some(libc::EPERM)) if what.contains("(proc)") => {
            " (the kernel refuses a new /proc while the host's is partly covered: \
             a pod needs `procMount: Unmasked`)"
        }
        (Some(Step::Op), Some(libc::ENOENT)) => {
            " (a mount point missing from a read-only image root: see prepare_mount_points)"
        }
        _ => "",
    };
    SpawnError::Setup(format!("{what}: {os}{hint}"))
}

fn read_report(report: OwnedFd, pid: libc::pid_t, plan: &Plan) -> Result<(), SpawnError> {
    match read_report_bytes(report, pid)? {
        None => Ok(()),
        Some(buf) => Err(describe_failure(buf, plan)),
    }
}

/// Start `plan` as a job. See [`crate::job::Launch::spawn`].
/// Where each descriptor the job keeps goes, and the control fd numbers.
struct Layout {
    /// (descriptor in the parent, number in the child).
    moves: Vec<(RawFd, RawFd)>,
    sync: RawFd,
    status: RawFd,
    report: RawFd,
    landlock: Option<RawFd>,
    trees_at: RawFd,
    keep_below: RawFd,
    above: RawFd,
}

fn plan_layout(
    plan: &Plan,
    stdio: [RawFd; 3],
    extra: &[(OwnedFd, RawFd)],
    control: [RawFd; 3],
) -> Result<Layout, SpawnError> {
    let [in_fd, out_fd, err_fd] = stdio;
    let mut moves = vec![(in_fd, 0), (out_fd, 1), (err_fd, 2)];
    let mut highest = 2;
    for (fd, target) in extra {
        if *target < 3 || moves.iter().any(|(_, t)| t == target) {
            return Err(SpawnError::Invalid(format!(
                "descriptor number {target} is taken or below 3"
            )));
        }
        moves.push((fd.as_raw_fd(), *target));
        highest = highest.max(*target);
    }
    let next = |n: RawFd, by: RawFd| {
        n.checked_add(by)
            .ok_or_else(|| SpawnError::Invalid("descriptor numbers out of range".into()))
    };
    let (sync, status, report) = (next(highest, 1)?, next(highest, 2)?, next(highest, 3)?);
    moves.push((control[0], sync));
    moves.push((control[1], status));
    moves.push((control[2], report));
    let landlock = match &plan.landlock {
        Some((fd, _)) => {
            let at = next(highest, 4)?;
            moves.push((fd.as_raw_fd(), at));
            Some(at)
        }
        None => None,
    };
    let trees_at = next(highest, 5)?;
    for (i, tree) in plan.trees.iter().enumerate() {
        let i = RawFd::try_from(i)
            .map_err(|_| SpawnError::Invalid("too many idmapped mounts".into()))?;
        moves.push((tree.as_raw_fd(), next(trees_at, i)?));
    }
    let keep_below = next(moves.iter().map(|(_, t)| *t).max().unwrap_or(2), 1)?;
    let above = next(
        moves
            .iter()
            .map(|(s, t)| (*s).max(*t))
            .max()
            .unwrap_or(keep_below)
            .max(keep_below),
        1,
    )?;
    Ok(Layout {
        moves,
        sync,
        status,
        report,
        landlock,
        trees_at,
        keep_below,
        above,
    })
}

/// The clone3 flags and arguments, plus where the pidfd is stored.
fn clone_args(cgroup: Option<RawFd>, pidfd: &mut RawFd) -> sys::CloneArgs {
    let bits = |f: libc::c_int| u64::from(f.unsigned_abs());
    let mut flags = bits(
        libc::CLONE_NEWUSER
            | libc::CLONE_NEWNS
            | libc::CLONE_NEWPID
            | libc::CLONE_NEWNET
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWUTS
            | libc::CLONE_NEWCGROUP
            | libc::CLONE_PIDFD,
    );
    let mut args = sys::CloneArgs {
        exit_signal: bits(libc::SIGCHLD),
        pidfd: std::ptr::from_mut(pidfd) as u64,
        ..sys::CloneArgs::default()
    };
    if let Some(cg) = cgroup {
        flags |= sys::CLONE_INTO_CGROUP;
        args.cgroup = u64::try_from(cg).unwrap_or_default();
    }
    args.flags = flags;
    args
}

/// The clone3 failure, with a hint at the usual cause.
fn clone_error(e: &io::Error, has_cgroup: bool) -> SpawnError {
    let hint = match e.raw_os_error() {
        Some(libc::EPERM | libc::ENOSPC | libc::EUSERS) => {
            " (user namespaces are disabled or exhausted here; in a container the \
             runtime's seccomp profile may refuse them: `seccompProfile: Unconfined`)"
        }
        Some(libc::EACCES) if has_cgroup => {
            " (no write permission on the cgroup's common ancestor's cgroup.procs)"
        }
        Some(libc::ENOSYS) => " (the kernel is older than 5.7)",
        _ => "",
    };
    SpawnError::Setup(format!("clone3: {e}{hint}"))
}

/// Map the job's ids, run the caller's hook, release it, wait for the
/// command to be running (or the step that failed).
fn finish(
    job: &Job,
    plan: &Plan,
    ids: crate::job::spec::IdMap,
    sync_w: OwnedFd,
    report_r: OwnedFd,
    setup: Option<SetupFn<'_>>,
) -> Result<(), SpawnError> {
    let pid = job.pid();
    if let Some(block) = ids.block {
        // The worker may map any ids (it holds CAP_SETUID and CAP_SETGID),
        // so `setgroups` stays allowed: the init drops the worker's groups.
        write_proc(
            pid,
            "uid_map",
            &format!("0 {} {}\n", block.uid, block.count),
        )?;
        write_proc(
            pid,
            "gid_map",
            &format!("0 {} {}\n", block.gid, block.count),
        )?;
    } else {
        write_proc(
            pid,
            "uid_map",
            &format!("{} {} 1\n", ids.inside_uid, ids.outside_uid),
        )?;
        write_proc(pid, "setgroups", "deny\n")?;
        write_proc(
            pid,
            "gid_map",
            &format!("{} {} 1\n", ids.inside_gid, ids.outside_gid),
        )?;
    }
    if let Some(setup) = setup {
        setup(&Started { pid }).map_err(SpawnError::Setup)?;
    }
    let mut go = std::fs::File::from(sync_w);
    go.write_all(b"g")
        .map_err(|e| SpawnError::Setup(format!("releasing the job: {e}")))?;
    drop(go);
    read_report(report_r, job.pid(), plan)
}

/// Make the anonymous pipes the job is given its own user's, where it has a
/// host user of its own: a pipe belongs to whoever made it (the worker),
/// mode 0600, and opening it again by path (`/dev/stdout`, which QEMU and
/// shell scripts do) checks that. Descriptors of anything else (a file, a
/// terminal, a named FIFO) stay as they are.
fn give_pipes<'a>(
    fds: impl Iterator<Item = &'a OwnedFd>,
    ids: &crate::job::spec::IdMap,
    block: crate::job::spec::IdBlock,
) -> Result<(), SpawnError> {
    let (Some(uid), Some(gid)) = (
        block.uid.checked_add(ids.inside_uid),
        block.gid.checked_add(ids.inside_gid),
    ) else {
        return Err(SpawnError::Invalid(
            "the job's ids overflow its block".into(),
        ));
    };
    for fd in fds {
        // An anonymous pipe reads as `pipe:[<inode>]`, a named FIFO as its path.
        let pipe = std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
            .is_ok_and(|to| to.as_os_str().as_encoded_bytes().starts_with(b"pipe:["));
        // SAFETY: fchown on a descriptor we hold.
        if pipe && unsafe { libc::fchown(fd.as_raw_fd(), uid, gid) } != 0 {
            return Err(SpawnError::Setup(format!(
                "giving the job its output pipe: {}",
                io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

/// Start `plan` as a job. See [`crate::job::Launch::spawn`].
pub fn spawn(
    plan: &Plan,
    ids: crate::job::spec::IdMap,
    fds: Fds,
    cgroup: Option<BorrowedFd<'_>>,
    setup: Option<SetupFn<'_>>,
) -> Result<Job, SpawnError> {
    let pipe_err = |e: io::Error| SpawnError::Setup(format!("a pipe: {e}"));
    let argv: Vec<*const libc::c_char> = plan
        .argv
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp: Vec<*const libc::c_char> = plan
        .envp
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let (sync_r, sync_w) = pipe().map_err(pipe_err)?;
    let (status_r, status_w) = pipe().map_err(pipe_err)?;
    let (report_r, report_w) = pipe().map_err(pipe_err)?;
    let null = dev_null().map_err(|e| SpawnError::Setup(format!("/dev/null: {e}")))?;
    let raw = |fd: &Option<OwnedFd>| {
        fd.as_ref()
            .map_or_else(|| null.as_raw_fd(), AsRawFd::as_raw_fd)
    };

    if let Some(block) = ids.block {
        let given = [&fds.stdin, &fds.stdout, &fds.stderr]
            .into_iter()
            .flatten()
            .chain(fds.extra.iter().map(|(fd, _)| fd));
        give_pipes(given, &ids, block)?;
    }
    let layout = plan_layout(
        plan,
        [raw(&fds.stdin), raw(&fds.stdout), raw(&fds.stderr)],
        &fds.extra,
        [
            sync_r.as_raw_fd(),
            status_w.as_raw_fd(),
            report_w.as_raw_fd(),
        ],
    )?;
    let mut scratch = vec![-1; layout.moves.len()];
    let mut pidfd: RawFd = -1;
    let clone_cfg = clone_args(cgroup.map(|c| c.as_raw_fd()), &mut pidfd);

    let mut child = Child {
        plan,
        argv: &argv,
        envp: &envp,
        moves: &layout.moves,
        scratch: &mut scratch,
        above: layout.above,
        keep_below: layout.keep_below,
        sync: layout.sync,
        status: layout.status,
        report: layout.report,
        landlock: layout.landlock,
        trees_at: layout.trees_at,
    };
    // SAFETY: clone3 without CLONE_VM: the child gets a copy of this
    // address space and continues on a copy of this stack, like fork. It
    // runs only `Child::run`, which never returns.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &raw const clone_cfg,
            std::mem::size_of::<sys::CloneArgs>(),
        )
    };
    if ret == 0 {
        // SAFETY: we are the child of clone3.
        unsafe { child.run() };
    }
    if ret < 0 {
        return Err(clone_error(&io::Error::last_os_error(), cgroup.is_some()));
    }
    let pid =
        libc::pid_t::try_from(ret).map_err(|_| SpawnError::Setup("clone3: bad pid".into()))?;
    drop((sync_r, status_w, report_w, null, fds));
    // SAFETY: CLONE_PIDFD stored a new descriptor here; nothing else owns it.
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
    let job = Job {
        pid,
        pidfd,
        status: status_r.into(),
        exit: None,
        unmapped: plan.unmapped.clone(),
    };
    // From here on, an error drops `job`, which kills and reaps the init.
    finish(&job, plan, ids, sync_w, report_r, setup)?;
    Ok(job)
}
