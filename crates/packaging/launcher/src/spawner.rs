//! The session's app spawner: every launch born in its own cgroup.
//!
//! The compositor starts one spawner (`tilo-launcher app-spawner`) per run,
//! with a private `SOCK_SEQPACKET` socket as its stdin and nothing else to
//! talk to. The spawner asks the user manager once for the delegated app
//! tree ([`crate::scope::join_app_tree_with`], [`crate::app_cgroup`]), moves
//! itself into the tree's `spawner/` leaf, enables the controllers, and says
//! where the tree is. After that a launch costs no round trip to systemd:
//!
//! 1. the compositor sends the launcher's argv, its whole environment, its
//!    working directory, and its stdin (the registration gate), stdout and
//!    stderr as descriptors;
//! 2. the spawner makes `app-<id>-<n>/`, writes the foreground tier into it,
//!    and starts the launcher with `clone3(CLONE_INTO_CGROUP | CLONE_PARENT)`:
//!    born in the app's cgroup, so the launcher, its D-Bus proxies and the
//!    sandbox never run a single instruction anywhere else, and still the
//!    compositor's own child, which reaps it as it reaped a spawned launcher;
//! 3. it answers with the pid once the exec happened, or with the exec's
//!    error;
//! 4. it removes the cgroup when `cgroup.events` says nothing is left in it.
//!
//! The compositor keeps the plain spawn for every launch the spawner cannot
//! take (not up yet, no user manager, a refusal): the launcher then asks the
//! manager for a scope of its own, as before ([`crate::scope::join`]). The
//! spawner exits when the compositor's end of the socket closes; systemd
//! removes the tree once the last app in it has exited.

use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Mutex, OnceLock};

use crate::app_cgroup;
use crate::registration_gate::RegistrationGate;

/// The `tilo-launcher` subcommand that serves as the spawner.
pub const SUBCOMMAND: &str = "app-spawner";

/// The largest request the spawner reads: an argv and a whole environment.
const MAX_MESSAGE: usize = 1 << 20;

/// The largest answer: a pid and a cgroup path, or a reason.
const MAX_REPLY: usize = 8 << 10;

/// `CLONE_INTO_CGROUP` (Linux 5.7).
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

/// `CLONE_PARENT`.
const CLONE_PARENT: u64 = 0x8000;

/// `struct clone_args`, version 2 (with `cgroup`).
#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

// --- wire ------------------------------------------------------------------

/// A message is NUL-separated fields: nothing it carries (an argv, an
/// environment, a path) can contain a NUL.
fn encode(fields: &[&[u8]]) -> io::Result<Vec<u8>> {
    if fields.iter().any(|field| field.contains(&0)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a launch field contains a NUL byte",
        ));
    }
    Ok(fields.join(&0))
}

fn decode(message: &[u8]) -> Vec<&[u8]> {
    message.split(|byte| *byte == 0).collect()
}

fn send(socket: &OwnedFd, fields: &[&[u8]], fds: &[std::os::fd::BorrowedFd<'_>]) -> io::Result<()> {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let message = encode(fields)?;
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let sent = sendmsg(
        socket,
        &[io::IoSlice::new(&message)],
        &mut control,
        SendFlags::NOSIGNAL,
    )?;
    if sent == message.len() {
        Ok(())
    } else {
        Err(io::ErrorKind::WriteZero.into())
    }
}

/// One message and the descriptors that came with it, close-on-exec.
/// `None` at end of file.
fn receive(socket: &OwnedFd, buffer: &mut [u8]) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = loop {
        match recvmsg(
            socket,
            &mut [io::IoSliceMut::new(buffer)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Err(rustix::io::Errno::INTR) => {}
            other => break other?,
        }
    };
    let mut fds = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    if received
        .flags
        .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a launch message was cut short",
        ));
    }
    if received.bytes == 0 && fds.is_empty() {
        return Ok(None);
    }
    Ok(Some((received.bytes, fds)))
}

// --- the spawner -----------------------------------------------------------

/// Serve the compositor on `socket` until it closes. `tree` is a cgroupfs
/// directory to use instead of asking the user manager (a test's own
/// subtree).
///
/// # Errors
///
/// When the socket fails; a tree that cannot be had is an answer, not an
/// error.
pub fn serve(socket: &OwnedFd, tree: Option<PathBuf>) -> io::Result<()> {
    let tree = match tree {
        Some(dir) => Ok(dir),
        None => join_tree(),
    };
    let mut server =
        match tree.and_then(|dir| Server::adopt(dir).map_err(|error| error.to_string())) {
            Ok(server) => server,
            Err(reason) => {
                return send(socket, &[b"unavailable", reason.as_bytes()], &[]);
            }
        };
    send(socket, &[b"ready", server.dir.as_os_str().as_bytes()], &[])?;
    server.run(socket)
}

/// Become the delegated tree's spawner through the user manager.
fn join_tree() -> Result<PathBuf, String> {
    let busctl = Path::new(crate::scope::BUSCTL);
    if !busctl.is_file() {
        return Err(format!(
            "no {} to reach the systemd user manager",
            crate::scope::BUSCTL
        ));
    }
    crate::scope::join_app_tree_with(busctl, crate::scope::JOIN_TIMEOUT)?;
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").map_err(|error| error.to_string())?;
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| "no unified cgroup hierarchy".to_string())?;
    Ok(app_cgroup::fs_path(path))
}

struct Server {
    dir: PathBuf,
    next: u64,
    total_memory: Option<u64>,
    inotify: OwnedFd,
    /// The app cgroups being watched for their end, by watch descriptor.
    watches: HashMap<i32, PathBuf>,
}

impl Server {
    /// Move this process into `dir`'s spawner leaf and hand the controllers
    /// to the app cgroups below `dir`.
    fn adopt(dir: PathBuf) -> io::Result<Self> {
        let leaf = dir.join(app_cgroup::SPAWNER_LEAF);
        match std::fs::create_dir(&leaf) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        // cgroup2 keeps processes out of a cgroup that distributes resources
        // to children: the tree's root must hold no process, this one included.
        std::fs::write(leaf.join("cgroup.procs"), "0")?;
        let available = std::fs::read_to_string(dir.join("cgroup.controllers")).unwrap_or_default();
        for controller in app_cgroup::CONTROLLERS {
            if !available.split_whitespace().any(|have| have == controller) {
                continue;
            }
            if let Err(error) =
                std::fs::write(dir.join("cgroup.subtree_control"), format!("+{controller}"))
            {
                eprintln!("tilo-spawner: apps get no {controller} controller: {error}");
            }
        }
        // SAFETY: plain syscall.
        let inotify = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if inotify < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            dir,
            next: 0,
            total_memory: crate::priority::mem_total_bytes(),
            // SAFETY: a descriptor this process just created and owns alone.
            inotify: unsafe { OwnedFd::from_raw_fd(inotify) },
            watches: HashMap::new(),
        })
    }

    fn run(&mut self, socket: &OwnedFd) -> io::Result<()> {
        let mut buffer = vec![0_u8; MAX_MESSAGE];
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: socket.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.inotify.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two valid pollfd entries on the stack.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let [request, ended] = fds;
            if ended.revents != 0 {
                self.collect_ended();
            }
            if request.revents == 0 {
                continue;
            }
            let Some((length, fds)) = receive(socket, &mut buffer)? else {
                return Ok(());
            };
            let message = buffer.get(..length).unwrap_or_default();
            let reply = self.handle(&decode(message), fds);
            let reply: Vec<&[u8]> = reply.iter().map(Vec::as_slice).collect();
            send(socket, &reply, &[])?;
        }
    }

    fn handle(&mut self, fields: &[&[u8]], fds: Vec<OwnedFd>) -> Vec<Vec<u8>> {
        let failed = |reason: &str| vec![b"failed".to_vec(), reason.as_bytes().to_vec()];
        let Some(launch) = Launch::parse(fields) else {
            return failed("malformed launch request");
        };
        let Ok(io) = <[OwnedFd; 3]>::try_from(fds) else {
            return failed("a launch request carries stdin, stdout and stderr");
        };
        match self.spawn(&launch, &io) {
            Ok(Spawned::Running { pid, cgroup }) => vec![
                b"ok".to_vec(),
                pid.to_string().into_bytes(),
                cgroup.into_os_string().into_vec(),
            ],
            Ok(Spawned::ExecFailed { pid, errno }) => vec![
                b"exec-failed".to_vec(),
                pid.to_string().into_bytes(),
                errno.to_string().into_bytes(),
            ],
            Err(error) => failed(&error.to_string()),
        }
    }

    /// Make the app's cgroup and start the launcher in it.
    fn spawn(&mut self, launch: &Launch<'_>, io: &[OwnedFd; 3]) -> io::Result<Spawned> {
        let cgroup = self.create_cgroup(launch.app_id)?;
        let started = start_in(&cgroup, launch, io);
        match started {
            Ok(spawned) => {
                self.watch(&cgroup);
                Ok(spawned)
            }
            Err(error) => {
                let _ = app_cgroup::remove(&cgroup);
                Err(error)
            }
        }
    }

    fn create_cgroup(&mut self, app_id: &str) -> io::Result<PathBuf> {
        loop {
            self.next = self.next.saturating_add(1);
            let name = app_cgroup::leaf_name(app_id, self.next)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "app id"))?;
            let dir = self.dir.join(name);
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    // The person just asked for this app: it starts in the
                    // foreground tier, and the compositor demotes it when focus
                    // moves on (see `priority`).
                    let tier = crate::priority::tier_properties(
                        crate::priority::Tier::Foreground,
                        self.total_memory,
                    );
                    if let Err(error) = app_cgroup::apply(&dir, &tier) {
                        eprintln!("tilo-spawner: {app_id} starts without its tier: {error}");
                    }
                    return Ok(dir);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Learn of the end of `cgroup` from its `cgroup.events`.
    fn watch(&mut self, cgroup: &Path) {
        let Ok(events) = CString::new(cgroup.join("cgroup.events").into_os_string().into_vec())
        else {
            return;
        };
        // SAFETY: a valid inotify descriptor and a NUL-terminated path.
        let watch = unsafe {
            libc::inotify_add_watch(self.inotify.as_raw_fd(), events.as_ptr(), libc::IN_MODIFY)
        };
        if watch < 0 {
            eprintln!(
                "tilo-spawner: {} stays until the session ends: {}",
                cgroup.display(),
                io::Error::last_os_error()
            );
            return;
        }
        self.watches.insert(watch, cgroup.to_path_buf());
        // It may have ended before the watch existed.
        self.reap_if_empty(watch);
    }

    fn collect_ended(&mut self) {
        let mut buffer = [0_u8; 4096];
        let mut changed = Vec::new();
        loop {
            // SAFETY: reading into a stack buffer of the given length.
            let read = unsafe {
                libc::read(
                    self.inotify.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            let Ok(read) = usize::try_from(read) else {
                break;
            };
            if read == 0 {
                break;
            }
            let mut at = 0_usize;
            // struct inotify_event { int wd; u32 mask, cookie, len; char name[len]; }
            while let Some(header) = buffer.get(at..at.saturating_add(16)).filter(|_| at < read) {
                let field = |offset: usize| {
                    header
                        .get(offset..offset.saturating_add(4))
                        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
                        .unwrap_or_default()
                };
                changed.push(i32::from_ne_bytes(field(0)));
                let name_len = u32::from_ne_bytes(field(12)) as usize;
                at = at.saturating_add(16).saturating_add(name_len);
            }
        }
        changed.sort_unstable();
        changed.dedup();
        for watch in changed {
            self.reap_if_empty(watch);
        }
    }

    /// Remove the cgroup behind `watch` once nothing runs in it any more.
    fn reap_if_empty(&mut self, watch: i32) {
        let Some(cgroup) = self.watches.get(&watch) else {
            return;
        };
        if app_cgroup::populated(cgroup).unwrap_or(false) {
            return;
        }
        if let Err(error) = app_cgroup::remove(cgroup) {
            eprintln!("tilo-spawner: removing {}: {error}", cgroup.display());
        }
        // SAFETY: a watch this process added to its own inotify descriptor.
        unsafe { libc::inotify_rm_watch(self.inotify.as_raw_fd(), watch) };
        self.watches.remove(&watch);
    }
}

/// A decoded launch request.
struct Launch<'a> {
    app_id: &'a str,
    program: CString,
    cwd: CString,
    argv: Vec<CString>,
    env: Vec<CString>,
}

impl<'a> Launch<'a> {
    /// `spawn, app id, program, cwd, argc, argv…, env…`
    fn parse(fields: &[&'a [u8]]) -> Option<Self> {
        let [kind, app_id, program, cwd, argc, rest @ ..] = fields else {
            return None;
        };
        if *kind != b"spawn" || !program.starts_with(b"/") {
            return None;
        }
        let argc: usize = std::str::from_utf8(argc).ok()?.parse().ok()?;
        let arguments = rest.get(..argc)?;
        let env = rest.get(argc..)?;
        let owned = |field: &[u8]| CString::new(field).ok();
        Some(Self {
            app_id: std::str::from_utf8(app_id).ok()?,
            program: owned(program)?,
            cwd: owned(cwd)?,
            argv: arguments
                .iter()
                .map(|field| owned(field))
                .collect::<Option<_>>()?,
            env: env
                .iter()
                .filter(|field| !field.is_empty())
                .map(|field| owned(field))
                .collect::<Option<_>>()?,
        })
    }
}

enum Spawned {
    Running {
        pid: u32,
        cgroup: PathBuf,
    },
    /// The child exists (the compositor must reap it) but its exec failed.
    ExecFailed {
        pid: u32,
        errno: i32,
    },
}

/// `clone3(CLONE_INTO_CGROUP | CLONE_PARENT)` into `cgroup`, then exec the
/// launch in the child. Returns once the exec happened or failed.
fn start_in(cgroup: &Path, launch: &Launch<'_>, io: &[OwnedFd; 3]) -> io::Result<Spawned> {
    let directory = File::open(cgroup)?;
    let mut report = [0; 2];
    // SAFETY: a two-element array for the pipe's ends.
    if unsafe { libc::pipe2(report.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned here alone.
    let (report_read, report_write) = unsafe {
        (
            OwnedFd::from_raw_fd(report[0]),
            OwnedFd::from_raw_fd(report[1]),
        )
    };
    // Everything the child touches exists before the clone: it only makes
    // system calls until exec.
    let argv: Vec<*const libc::c_char> = launch
        .argv
        .iter()
        .map(|arg| arg.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp: Vec<*const libc::c_char> = launch
        .env
        .iter()
        .map(|var| var.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let stdio = [io[0].as_raw_fd(), io[1].as_raw_fd(), io[2].as_raw_fd()];
    let mut clone_args = CloneArgs {
        // CLONE_PARENT: the compositor, this spawner's parent, becomes the
        // launcher's parent, so it reaps and signals it as its own child.
        flags: CLONE_PARENT | CLONE_INTO_CGROUP,
        // Required with CLONE_PARENT; the child inherits this process's
        // exit signal (SIGCHLD).
        exit_signal: 0,
        cgroup: u64::try_from(directory.as_raw_fd())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
        ..CloneArgs::default()
    };
    // SAFETY: a valid clone_args of the size passed. This process is single
    // threaded, and the child below only makes system calls before it execs
    // or exits.
    let pid = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &raw mut clone_args,
            std::mem::size_of::<CloneArgs>(),
        )
    };
    if pid == 0 {
        // SAFETY: in the freshly cloned child; every pointer refers to memory
        // prepared above, copied into the child with the address space.
        unsafe {
            exec_child(
                &stdio,
                launch.cwd.as_ptr(),
                launch.program.as_ptr(),
                &argv,
                &envp,
                report_write.as_raw_fd(),
            )
        }
    }
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    let pid = u32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    drop(report_write);
    let mut errno = [0_u8; 4];
    let mut have = 0_usize;
    while have < errno.len() {
        let rest = errno.get_mut(have..).unwrap_or_default();
        // SAFETY: reading into the rest of a stack buffer.
        let read = unsafe {
            libc::read(
                report_read.as_raw_fd(),
                rest.as_mut_ptr().cast(),
                rest.len(),
            )
        };
        match usize::try_from(read) {
            Ok(0) => break,
            Ok(read) => have = have.saturating_add(read),
            Err(_) if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if have == 0 {
        Ok(Spawned::Running {
            pid,
            cgroup: cgroup.to_path_buf(),
        })
    } else {
        Ok(Spawned::ExecFailed {
            pid,
            errno: i32::from_ne_bytes(errno),
        })
    }
}

/// The child's side: stdio in place, signals as a fresh process has them,
/// the working directory, then exec. On failure the errno goes to `report`
/// and the child exits 127.
///
/// # Safety
///
/// Call only in the child of a fork-like clone, with NUL-terminated strings
/// and null-terminated pointer arrays.
unsafe fn exec_child(
    stdio: &[RawFd; 3],
    cwd: *const libc::c_char,
    program: *const libc::c_char,
    argv: &[*const libc::c_char],
    envp: &[*const libc::c_char],
    report: RawFd,
) -> ! {
    // SAFETY: async-signal-safe system calls only; see the function's contract.
    unsafe {
        for (target, fd) in (0..).zip(stdio) {
            if libc::dup2(*fd, target) < 0 {
                fail_child(report);
            }
        }
        let mut empty: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut empty);
        libc::sigprocmask(libc::SIG_SETMASK, &raw const empty, std::ptr::null_mut());
        // As `Command` leaves it: the compositor ignores SIGPIPE, an app must not.
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        if libc::chdir(cwd) != 0 {
            fail_child(report);
        }
        libc::execve(program, argv.as_ptr(), envp.as_ptr());
        fail_child(report)
    }
}

/// # Safety
///
/// As [`exec_child`].
unsafe fn fail_child(report: RawFd) -> ! {
    let errno = io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO);
    let bytes = errno.to_ne_bytes();
    // SAFETY: async-signal-safe; the child ends here.
    unsafe {
        libc::write(report, bytes.as_ptr().cast(), bytes.len());
        libc::_exit(127)
    }
}

// --- the compositor's side ---------------------------------------------------

/// A launch for the spawner, as `Command` would have run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub app_id: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// The whole environment, not changes to the compositor's.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: PathBuf,
}

impl Request {
    /// What `command` would run: its program, arguments, this process's
    /// environment with the command's changes, and its working directory.
    ///
    /// # Errors
    ///
    /// A program named without a path (the spawner does not search `PATH`),
    /// or no working directory.
    pub fn from_command(command: &Command, app_id: &str) -> io::Result<Self> {
        let program = PathBuf::from(command.get_program());
        if !program.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the spawner runs programs by absolute path",
            ));
        }
        let mut env: std::collections::BTreeMap<OsString, OsString> = std::env::vars_os().collect();
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => env.insert(key.to_os_string(), value.to_os_string()),
                None => env.remove(key),
            };
        }
        let cwd = match command.get_current_dir() {
            Some(dir) => dir.to_path_buf(),
            None => std::env::current_dir()?,
        };
        Ok(Self {
            app_id: app_id.to_string(),
            program,
            args: std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(OsStr::to_os_string)
                .collect(),
            env: env.into_iter().collect(),
            cwd,
        })
    }

    fn encode_fields(&self) -> Vec<Vec<u8>> {
        let mut fields = vec![
            b"spawn".to_vec(),
            self.app_id.as_bytes().to_vec(),
            self.program.as_os_str().as_bytes().to_vec(),
            self.cwd.as_os_str().as_bytes().to_vec(),
            self.args.len().to_string().into_bytes(),
        ];
        fields.extend(self.args.iter().map(|arg| arg.as_bytes().to_vec()));
        fields.extend(self.env.iter().map(|(key, value)| {
            let mut var = key.as_bytes().to_vec();
            var.push(b'=');
            var.extend_from_slice(value.as_bytes());
            var
        }));
        fields
    }
}

/// Why a launch did not start through the spawner.
#[derive(Debug)]
pub enum SpawnError {
    /// The spawner could not take it (gone, refused, a request it cannot
    /// carry): start it the plain way.
    Unavailable(String),
    /// The launcher was created in its cgroup but could not be executed: the
    /// launch failed, as a failed `Command::spawn` does.
    Exec(io::Error),
}

/// A launcher started by the spawner: the compositor's child, born in its
/// app cgroup.
#[derive(Debug)]
pub struct DelegatedChild {
    pid: u32,
    cgroup: PathBuf,
    stdout: Option<File>,
    stderr: Option<File>,
    status: Option<ExitStatus>,
}

impl DelegatedChild {
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.pid
    }

    /// The app's cgroup directory.
    #[must_use]
    pub fn cgroup(&self) -> &Path {
        &self.cgroup
    }

    /// The read end of the launcher's stdout.
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    /// The read end of the launcher's stderr.
    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    /// `SIGKILL` the launcher, as `Child::kill` does; nothing once reaped.
    ///
    /// # Errors
    ///
    /// The kernel's refusal.
    pub fn kill(&mut self) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        let pid = libc::pid_t::try_from(self.pid).map_err(|_| io::ErrorKind::InvalidData)?;
        // SAFETY: an unreaped child of this process: the pid cannot be reused.
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Wait for the launcher to exit and reap it, as `Child::wait` does.
    ///
    /// # Errors
    ///
    /// The kernel's error other than an interruption.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let pid = libc::pid_t::try_from(self.pid).map_err(|_| io::ErrorKind::InvalidData)?;
        let mut raw = 0;
        loop {
            // SAFETY: waiting on this process's own child.
            if unsafe { libc::waitpid(pid, &raw mut raw, 0) } == pid {
                let status = ExitStatus::from_raw(raw);
                self.status = Some(status);
                return Ok(status);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn pipe() -> io::Result<(File, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: a two-element array for the pipe's ends.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned here alone.
    Ok(unsafe { (File::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The compositor's connection to its spawner.
#[derive(Debug)]
pub struct Spawner {
    socket: Mutex<OwnedFd>,
    tree: PathBuf,
    process: Mutex<std::process::Child>,
}

impl Spawner {
    /// Start `launcher_bin app-spawner` and wait for its answer: the tree
    /// it serves, or why there is none. `tree` hands it a cgroupfs directory
    /// instead of the user manager's.
    ///
    /// # Errors
    ///
    /// The reason there is no spawner, for the log.
    pub fn start(launcher_bin: &Path, tree: Option<&Path>) -> Result<Self, String> {
        use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
        let (ours, theirs) = socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|error| format!("socket pair: {error}"))?;
        let mut command = Command::new(launcher_bin);
        command.arg(SUBCOMMAND);
        if let Some(tree) = tree {
            command.arg("--tree").arg(tree);
        }
        // Its stdin is the socket: no other descriptor needs passing, so no
        // pre_exec hook, and `Command` keeps using posix_spawn.
        let mut process = command
            .stdin(Stdio::from(theirs))
            .spawn()
            .map_err(|error| format!("starting {}: {error}", launcher_bin.display()))?;
        let mut buffer = vec![0_u8; MAX_REPLY];
        let answer = match receive(&ours, &mut buffer) {
            Ok(Some((length, _))) => buffer.get(..length).unwrap_or_default().to_vec(),
            Ok(None) => Vec::new(),
            Err(error) => format!("unavailable\0{error}").into_bytes(),
        };
        match decode(&answer).as_slice() {
            [b"ready", tree] => Ok(Self {
                socket: Mutex::new(ours),
                tree: PathBuf::from(OsStr::from_bytes(tree)),
                process: Mutex::new(process),
            }),
            fields => {
                drop(ours);
                let _ = process.wait();
                Err(match fields {
                    [b"unavailable", reason] => String::from_utf8_lossy(reason).into_owned(),
                    _ => "the spawner exited without an answer".to_string(),
                })
            }
        }
    }

    /// The tree's cgroupfs directory.
    #[must_use]
    pub fn tree(&self) -> &Path {
        &self.tree
    }

    /// Start `request` in a cgroup of its own, with `stdin` as its stdin and
    /// fresh pipes for its stdout and stderr.
    ///
    /// # Errors
    ///
    /// [`SpawnError::Unavailable`] when the spawner did not start it,
    /// [`SpawnError::Exec`] when it started but could not be executed.
    pub fn spawn(&self, request: &Request, stdin: &OwnedFd) -> Result<DelegatedChild, SpawnError> {
        let unavailable = |what: &str, error: &dyn std::fmt::Display| {
            SpawnError::Unavailable(format!("{what}: {error}"))
        };
        let (stdout, stdout_child) = pipe().map_err(|error| unavailable("pipe", &error))?;
        let (stderr, stderr_child) = pipe().map_err(|error| unavailable("pipe", &error))?;
        let fields = request.encode_fields();
        let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
        let mut buffer = vec![0_u8; MAX_REPLY];
        let answer = {
            let socket = self
                .socket
                .lock()
                .map_err(|_| SpawnError::Unavailable("spawner lock poisoned".into()))?;
            let exchanged = send(
                &socket,
                &fields,
                &[stdin.as_fd(), stdout_child.as_fd(), stderr_child.as_fd()],
            )
            .and_then(|()| receive(&socket, &mut buffer));
            drop(socket);
            match exchanged {
                Ok(Some((length, _))) => buffer.get(..length).unwrap_or_default().to_vec(),
                Ok(None) => {
                    self.reap_exited();
                    return Err(SpawnError::Unavailable("the spawner exited".into()));
                }
                Err(error) => {
                    self.reap_exited();
                    return Err(unavailable("the spawner", &error));
                }
            }
        };
        drop((stdout_child, stderr_child));
        let field_u32 = |field: &[u8]| std::str::from_utf8(field).ok()?.parse::<u32>().ok();
        match decode(&answer).as_slice() {
            [b"ok", pid, cgroup] => {
                let pid = field_u32(pid)
                    .ok_or_else(|| SpawnError::Unavailable("malformed answer".into()))?;
                Ok(DelegatedChild {
                    pid,
                    cgroup: PathBuf::from(OsStr::from_bytes(cgroup)),
                    stdout: Some(stdout),
                    stderr: Some(stderr),
                    status: None,
                })
            }
            [b"exec-failed", pid, errno] => {
                // The child is ours and exits at once: reap it.
                if let Some(pid) = field_u32(pid) {
                    let mut child = DelegatedChild {
                        pid,
                        cgroup: PathBuf::new(),
                        stdout: None,
                        stderr: None,
                        status: None,
                    };
                    let _ = child.wait();
                }
                let errno = std::str::from_utf8(errno)
                    .ok()
                    .and_then(|errno| errno.parse().ok())
                    .unwrap_or(libc::EIO);
                Err(SpawnError::Exec(io::Error::from_raw_os_error(errno)))
            }
            [b"failed", reason] => Err(SpawnError::Unavailable(
                String::from_utf8_lossy(reason).into_owned(),
            )),
            _ => Err(SpawnError::Unavailable("malformed answer".into())),
        }
    }

    /// Close the socket, which ends the spawner, and reap it.
    pub fn shutdown(self) {
        let Self {
            socket, process, ..
        } = self;
        drop(socket);
        if let Ok(mut process) = process.into_inner() {
            let _ = process.wait();
        }
    }

    /// A spawner that went away is the compositor's child: reap it.
    fn reap_exited(&self) {
        if let Ok(mut process) = self.process.lock() {
            let _ = process.try_wait();
        }
    }
}

static SESSION: OnceLock<Spawner> = OnceLock::new();

/// Start this compositor's spawner on a thread of its own; launches take the
/// plain path until it answers, and for good when it cannot run. `tree` is a
/// cgroupfs directory to serve instead of the user manager's (a development
/// harness's). Call once, from the compositor.
pub fn enable(launcher_bin: PathBuf, tree: Option<PathBuf>) {
    let started = std::thread::Builder::new()
        .name("tilo-app-spawner".into())
        .spawn(
            move || match Spawner::start(&launcher_bin, tree.as_deref()) {
                Ok(spawner) => {
                    eprintln!(
                        "tilo-spawner: apps start in their own cgroup below {}",
                        spawner.tree().display()
                    );
                    let _ = SESSION.set(spawner);
                }
                Err(reason) => {
                    eprintln!(
                        "tilo-spawner: apps ask the user manager for a scope instead: {reason}"
                    );
                }
            },
        );
    if let Err(error) = started {
        eprintln!("tilo-spawner: no thread to start the spawner on: {error}");
    }
}

/// Start `command` for `app_id` through this compositor's spawner, gated as
/// [`crate::registration_gate::install`] gates a spawned launcher. `None`
/// when the spawner is not running or could not take it: then spawn
/// `command` the plain way.
pub fn spawn_gated(
    command: &mut Command,
    app_id: &str,
) -> Option<io::Result<(DelegatedChild, RegistrationGate)>> {
    let spawner = SESSION.get()?;
    let (gate, wait_end) = match crate::registration_gate::install_external(command) {
        Ok(pair) => pair,
        Err(error) => return Some(Err(error)),
    };
    let request = match Request::from_command(command, app_id) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("tilo-spawner: {app_id} starts the plain way: {error}");
            return None;
        }
    };
    match spawner.spawn(&request, &wait_end) {
        Ok(child) => Some(Ok((child, gate))),
        Err(SpawnError::Exec(error)) => Some(Err(error)),
        Err(SpawnError::Unavailable(reason)) => {
            eprintln!("tilo-spawner: {app_id} starts the plain way: {reason}");
            None
        }
    }
}

/// The tree this compositor's spawner serves, once it runs.
#[must_use]
pub fn session_tree() -> Option<&'static Path> {
    SESSION.get().map(Spawner::tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_request_round_trips_and_refuses_what_it_cannot_carry() {
        let request = Request {
            app_id: "so.tilo.notes".into(),
            program: "/usr/bin/tilo-launcher".into(),
            args: vec![
                "/usr/bin/tilo-launcher".into(),
                "run".into(),
                String::new().into(),
            ],
            env: vec![("A".into(), "1".into()), ("B".into(), "x=y".into())],
            cwd: "/".into(),
        };
        let fields = request.encode_fields();
        let refs: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
        let wire = encode(&refs).unwrap();
        let decoded = decode(&wire);
        let launch = Launch::parse(&decoded).expect("parses");
        assert_eq!(launch.app_id, "so.tilo.notes");
        assert_eq!(launch.program.as_bytes(), b"/usr/bin/tilo-launcher");
        assert_eq!(launch.cwd.as_bytes(), b"/");
        // An empty argument survives; it is not an empty environment entry.
        assert_eq!(
            launch
                .argv
                .iter()
                .map(CString::as_bytes)
                .collect::<Vec<_>>(),
            [b"/usr/bin/tilo-launcher".as_slice(), b"run", b""]
        );
        assert_eq!(
            launch.env.iter().map(CString::as_bytes).collect::<Vec<_>>(),
            [b"A=1".as_slice(), b"B=x=y"]
        );
        // A NUL cannot be smuggled in to shift the fields.
        assert!(encode(&[b"spawn", b"a\0b"]).is_err());
        // Relative programs, short and wrong requests are refused.
        let mut relative = fields.clone();
        relative[2] = b"tilo-launcher".to_vec();
        let refs: Vec<&[u8]> = relative.iter().map(Vec::as_slice).collect();
        assert!(Launch::parse(&refs).is_none());
        assert!(Launch::parse(&[b"spawn", b"a", b"/x", b"/"]).is_none());
        assert!(Launch::parse(&[b"spawn", b"a", b"/x", b"/", b"3", b"only-one"]).is_none());
        assert!(Launch::parse(&[b"kill", b"a", b"/x", b"/", b"0"]).is_none());
    }

    #[test]
    fn a_request_is_what_the_command_would_have_run() {
        let mut command = Command::new("/usr/bin/tilo-launcher");
        command
            .args(["run", "--app-id", "so.tilo.notes"])
            .env("TILO_SPAWNER_TEST_SET", "1")
            .env_remove("PATH")
            .current_dir("/tmp");
        let request = Request::from_command(&command, "so.tilo.notes").unwrap();
        assert_eq!(
            request.args,
            ["/usr/bin/tilo-launcher", "run", "--app-id", "so.tilo.notes"]
                .map(OsString::from)
                .to_vec()
        );
        let has = |key: &str| request.env.iter().any(|(k, _)| k == key);
        assert!(has("TILO_SPAWNER_TEST_SET"));
        assert!(!has("PATH"), "a removed variable stays removed");
        assert_eq!(request.cwd, Path::new("/tmp"));
        assert!(Request::from_command(&Command::new("tilo-launcher"), "x").is_err());
    }
}
