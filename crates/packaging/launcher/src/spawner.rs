//! The session's app spawner: every launch born in its own cgroup.
//!
//! The compositor starts one spawner (`tilo-launcher app-spawner`) per run,
//! with a private `SOCK_SEQPACKET` socket as its stdin and another as its
//! stdout, and nothing else to talk to. The spawner asks the user manager
//! once for the delegated app tree ([`crate::scope::join_app_tree_with`],
//! [`crate::app_cgroup`]), moves itself into the tree's `spawner/` leaf,
//! enables the controllers, and says where the tree is. After that a launch
//! costs no round trip to systemd:
//!
//! 1. the compositor sends the launcher's argv, its whole environment, its
//!    working directory, whether it runs a Tilo app or a host app, and its
//!    stdin (the registration gate), stdout and stderr as descriptors;
//! 2. the spawner makes `app-<id>-<n>/`, writes the foreground tier into it,
//!    lays it out ([`app_cgroup::lay_out`]: a `main/` leaf, one OOM group
//!    for a Tilo app) and starts the launcher in `main/` with
//!    `clone3(CLONE_INTO_CGROUP | CLONE_PARENT)`: born in the app's cgroup,
//!    so the launcher, its D-Bus proxies and the sandbox never run a single
//!    instruction anywhere else, and still the compositor's own child, which
//!    reaps it as it reaped a spawned launcher;
//! 3. it answers with the pid once the exec happened, or with the exec's
//!    error;
//! 4. it removes the cgroup when `cgroup.events` says nothing is left in it.
//!
//! Three more duties come with owning the tree:
//!
//! - **Jobs.** It listens on `$XDG_RUNTIME_DIR/tilo/app-jobs/<tree>.sock` for
//!   job requests (`runtime_portal::jobs`): from an app's launcher on the
//!   app's behalf, or from a session service starting a process for an app
//!   (`ush`, for a Terminal session). Each makes a `job-<j>/` leaf beside
//!   the app's `main/`, its own OOM group, and is answered with an open
//!   descriptor of its `cgroup.procs`.
//! - **Out of memory.** When the kernel ends a Tilo app's `main/` as one OOM
//!   group (`memory.events`), the spawner ends the rest of the app, its jobs,
//!   with `cgroup.kill`: the app ends completely. The compositor, reaping the
//!   app's launcher, asks why it ended (`ended`) and tells the person. A job
//!   that ended for memory is reported on the event socket (stdout), since
//!   its app keeps running.
//! - **Memory pressure.** A thread watches PSI and acts before the kernel
//!   has to ([`crate::memory_monitor`], [`crate::memory_pressure`]).
//!
//! The compositor keeps the plain spawn for every launch the spawner cannot
//! take (not up yet, no user manager, a refusal): the launcher then asks the
//! manager for a scope of its own, as before ([`crate::scope::join`]). The
//! spawner exits when the compositor's end of the socket closes; systemd
//! removes the tree once the last app in it has exited.

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use runtime_portal::jobs::{self, decode};

use crate::app_cgroup::{self, AppKind, Leaf};
use crate::memory_monitor::{JobInfo, Shared};
use crate::memory_pressure::Cause;
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
//
// A message is NUL-separated fields (`runtime_portal::jobs::encode`): nothing
// it carries (an argv, an environment, a path) can contain a NUL.

fn send(socket: &OwnedFd, fields: &[&[u8]], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    jobs::send(socket.as_fd(), fields, fds)
}

/// One message and the descriptors that came with it, close-on-exec.
/// `None` at end of file.
fn receive(socket: &OwnedFd, buffer: &mut [u8]) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
    jobs::receive(socket.as_fd(), buffer)
}

// --- the spawner -----------------------------------------------------------

/// The most job connections served at once.
const MAX_JOB_CLIENTS: usize = 64;

/// The most job leaves one app holds at once: each is a cgroup of the
/// person's, made on the app's request.
const MAX_JOBS_PER_APP: usize = 64;

/// How long an empty job leaf is kept for the process its slot was handed
/// out for, before it counts as abandoned.
const JOB_GRACE: Duration = Duration::from_secs(10);

/// Serve the compositor on `socket` until it closes, reporting the ends of
/// jobs on `events`. `tree` is a cgroupfs directory to use instead of asking
/// the user manager (a test's own subtree).
///
/// # Errors
///
/// When the socket fails; a tree that cannot be had is an answer, not an
/// error.
pub fn serve(socket: &OwnedFd, events: Option<OwnedFd>, tree: Option<PathBuf>) -> io::Result<()> {
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
    *server
        .shared
        .events
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = events;
    match JobsListener::bind(&server.dir) {
        Ok(listener) => server.jobs = Some(listener),
        Err(error) => eprintln!("tilo-spawner: apps start no jobs of their own: {error}"),
    }
    crate::memory_monitor::spawn(server.dir.clone(), Arc::clone(&server.shared));
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

/// The socket job requests arrive on, removed with the spawner.
struct JobsListener {
    socket: OwnedFd,
    path: PathBuf,
}

impl JobsListener {
    fn bind(tree: &Path) -> io::Result<Self> {
        use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, socket_with};
        use std::os::unix::fs::PermissionsExt as _;
        let runtime = jobs::runtime_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no XDG_RUNTIME_DIR"))?;
        let name = tree
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the tree has no name"))?;
        let path = jobs::socket_path(&runtime, name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        // A spawner of a tree with this name is gone: the name holds its pid.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )?;
        rustix::net::bind(&socket, &SocketAddrUnix::new(path.as_path())?)?;
        rustix::net::listen(&socket, 16)?;
        Ok(Self { socket, path })
    }
}

impl Drop for JobsListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What an inotify watch is on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Watch {
    /// An app cgroup's `cgroup.events`: whether anything is left in it.
    App(PathBuf),
    /// The `memory.events` of an app's main leaf (the app's cgroup here).
    Main(PathBuf),
    /// The `memory.events` of a job leaf.
    Job(PathBuf),
}

impl Watch {
    fn dir(&self) -> &Path {
        match self {
            Self::App(dir) | Self::Main(dir) | Self::Job(dir) => dir,
        }
    }
}

struct Server {
    dir: PathBuf,
    next: u64,
    next_job: u64,
    total_memory: Option<u64>,
    inotify: OwnedFd,
    /// What each inotify watch is on, by watch descriptor.
    watches: HashMap<i32, Watch>,
    /// The `oom_group_kill` count of each leaf already acted on.
    group_kills: HashMap<PathBuf, u64>,
    /// Apps whose end the compositor already asked about.
    answered: HashSet<PathBuf>,
    jobs: Option<JobsListener>,
    job_clients: Vec<OwnedFd>,
    shared: Arc<Shared>,
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
            next_job: 0,
            total_memory: crate::priority::mem_total_bytes(),
            // SAFETY: a descriptor this process just created and owns alone.
            inotify: unsafe { OwnedFd::from_raw_fd(inotify) },
            watches: HashMap::new(),
            group_kills: HashMap::new(),
            answered: HashSet::new(),
            jobs: None,
            job_clients: Vec::new(),
            shared: Arc::new(Shared::default()),
        })
    }

    fn run(&mut self, socket: &OwnedFd) -> io::Result<()> {
        let mut buffer = vec![0_u8; MAX_MESSAGE];
        loop {
            let pollfd = |fd: RawFd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let mut fds = vec![pollfd(socket.as_raw_fd()), pollfd(self.inotify.as_raw_fd())];
            fds.push(pollfd(
                self.jobs
                    .as_ref()
                    .map_or(-1, |jobs| jobs.socket.as_raw_fd()),
            ));
            fds.extend(
                self.job_clients
                    .iter()
                    .map(|client| pollfd(client.as_raw_fd())),
            );
            let count = libc::nfds_t::try_from(fds.len()).unwrap_or(0);
            // SAFETY: `fds` holds `count` valid pollfd entries; a negative fd
            // is ignored by poll.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let revents: Vec<libc::c_short> = fds.iter().map(|fd| fd.revents).collect();
            if revents.get(1).is_some_and(|events| *events != 0) {
                self.collect_changes();
            }
            if revents.get(2).is_some_and(|events| *events != 0) {
                self.accept_job_clients();
            }
            let clients: Vec<usize> = revents
                .iter()
                .skip(3)
                .enumerate()
                .filter(|(_, events)| **events != 0)
                .map(|(index, _)| index)
                .collect();
            for index in clients.into_iter().rev() {
                self.serve_job_client(index);
            }
            if revents.first().is_none_or(|events| *events == 0) {
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
        if let [b"ended", app] = fields {
            return self.ended(Path::new(OsStr::from_bytes(app)));
        }
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

    /// Why the app at `app` ended, if memory ended it: `memory` and the
    /// cause, or `none`.
    fn ended(&mut self, app: &Path) -> Vec<Vec<u8>> {
        if !app.starts_with(&self.dir) {
            return vec![b"none".to_vec()];
        }
        if app.exists() {
            self.answered.insert(app.to_path_buf());
        }
        match self.shared.take_end(app) {
            Some(cause) => vec![b"memory".to_vec(), cause.wire().as_bytes().to_vec()],
            None => vec![b"none".to_vec()],
        }
    }

    /// Make the app's cgroup and start the launcher in its main leaf.
    fn spawn(&mut self, launch: &Launch<'_>, io: &[OwnedFd; 3]) -> io::Result<Spawned> {
        let cgroup = self.create_cgroup(launch.app_id)?;
        let started = app_cgroup::lay_out(&cgroup, launch.kind)
            .and_then(|main| start_in(&main, &cgroup, launch, io));
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

    /// Learn of a change of `file` from inotify, as `watch`.
    fn add_watch(&mut self, file: &Path, watch: Watch) {
        let Ok(path) = CString::new(file.as_os_str().as_bytes()) else {
            return;
        };
        // SAFETY: a valid inotify descriptor and a NUL-terminated path.
        let descriptor = unsafe {
            libc::inotify_add_watch(self.inotify.as_raw_fd(), path.as_ptr(), libc::IN_MODIFY)
        };
        if descriptor < 0 {
            eprintln!(
                "tilo-spawner: {} goes unwatched: {}",
                file.display(),
                io::Error::last_os_error()
            );
            return;
        }
        self.watches.insert(descriptor, watch);
    }

    /// Learn of the end of `cgroup` from its `cgroup.events`, and of its
    /// main leaf running out of memory from that leaf's `memory.events`.
    fn watch(&mut self, cgroup: &Path) {
        self.add_watch(
            &cgroup.join(app_cgroup::MAIN_LEAF).join("memory.events"),
            Watch::Main(cgroup.to_path_buf()),
        );
        self.add_watch(
            &cgroup.join("cgroup.events"),
            Watch::App(cgroup.to_path_buf()),
        );
        // It may have ended before the watch existed.
        self.reap_if_empty(cgroup);
    }

    fn collect_changes(&mut self) {
        let mut buffer = [0_u8; 4096];
        let mut changed: Vec<(i32, u32)> = Vec::new();
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
                changed.push((i32::from_ne_bytes(field(0)), u32::from_ne_bytes(field(4))));
                let name_len = u32::from_ne_bytes(field(12)) as usize;
                at = at.saturating_add(16).saturating_add(name_len);
            }
        }
        let mut ignored = Vec::new();
        let mut seen = HashSet::new();
        for (descriptor, mask) in changed {
            if mask & libc::IN_IGNORED != 0 {
                ignored.push(descriptor);
                continue;
            }
            if !seen.insert(descriptor) {
                continue;
            }
            match self.watches.get(&descriptor).cloned() {
                Some(Watch::App(app)) => self.reap_if_empty(&app),
                Some(Watch::Main(app)) => self.main_memory_changed(&app),
                Some(Watch::Job(job)) => self.job_memory_changed(&job),
                None => {}
            }
        }
        for descriptor in ignored {
            self.watches.remove(&descriptor);
        }
    }

    /// The kernel ended a Tilo app's main leaf as one OOM group: end the
    /// rest of it, its jobs, so the app ends completely. Why it ended is read
    /// from the leaf's events when the compositor asks.
    fn main_memory_changed(&mut self, app: &Path) {
        let main = app.join(app_cgroup::MAIN_LEAF);
        let kills = app_cgroup::memory_events(&main).oom_group_kill;
        let seen = self.group_kills.insert(main, kills).unwrap_or(0);
        if kills > seen && app_cgroup::kind_of(app) == Some(AppKind::Tilo) {
            eprintln!(
                "tilo-spawner: {} ran out of memory; ending all of it",
                app.display()
            );
            if let Err(error) = app_cgroup::kill(app) {
                eprintln!("tilo-spawner: ending {}: {error}", app.display());
            }
        }
    }

    /// The kernel ended a job as one OOM group: tell the compositor.
    fn job_memory_changed(&mut self, job: &Path) {
        let kills = app_cgroup::memory_events(job).oom_group_kill;
        let seen = self
            .group_kills
            .insert(job.to_path_buf(), kills)
            .unwrap_or(0);
        if kills > seen {
            self.shared.report_job_end(job, Cause::Kernel);
        }
    }

    /// Remove the app cgroup `cgroup` once nothing runs in it any more,
    /// keeping why it ended for the compositor when memory ended it and the
    /// compositor has not asked yet.
    fn reap_if_empty(&mut self, cgroup: &Path) {
        if app_cgroup::populated(cgroup).unwrap_or(false) {
            return;
        }
        if !self.answered.remove(cgroup) {
            let events = app_cgroup::memory_events(&cgroup.join(app_cgroup::MAIN_LEAF));
            if events.oom_kill > 0 || events.oom_group_kill > 0 {
                self.shared.record_end(cgroup, Cause::Kernel);
            }
        }
        let gone: Vec<i32> = self
            .watches
            .iter()
            .filter(|(_, watch)| watch.dir().starts_with(cgroup))
            .map(|(descriptor, _)| *descriptor)
            .collect();
        for descriptor in gone {
            // SAFETY: a watch this process added to its own inotify descriptor.
            unsafe { libc::inotify_rm_watch(self.inotify.as_raw_fd(), descriptor) };
            self.watches.remove(&descriptor);
        }
        self.group_kills.retain(|leaf, _| !leaf.starts_with(cgroup));
        self.shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|job, _| !job.starts_with(cgroup));
        if let Err(error) = app_cgroup::remove(cgroup) {
            eprintln!("tilo-spawner: removing {}: {error}", cgroup.display());
        }
    }

    // --- jobs ------------------------------------------------------------

    fn accept_job_clients(&mut self) {
        let Some(jobs) = self.jobs.as_ref() else {
            return;
        };
        loop {
            match rustix::net::accept_with(&jobs.socket, rustix::net::SocketFlags::CLOEXEC) {
                Ok(client) if self.job_clients.len() < MAX_JOB_CLIENTS => {
                    self.job_clients.push(client);
                }
                // Over the bound: the connection closes unanswered.
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    /// Answer what the job client at `index` asked; drop it once it hung up.
    fn serve_job_client(&mut self, index: usize) {
        let Some(client) = self.job_clients.get(index) else {
            return;
        };
        let mut buffer = [0_u8; jobs::MAX_MESSAGE];
        let received = receive(client, &mut buffer);
        let Ok(Some((length, fds))) = received else {
            drop(self.job_clients.remove(index));
            return;
        };
        let fields = decode(buffer.get(..length).unwrap_or_default());
        let answer = self.grant_job(&fields, fds);
        let Some(client) = self.job_clients.get(index) else {
            return;
        };
        let sent = match answer {
            Ok((adj, procs)) => {
                let fields = jobs::granted_fields(adj);
                let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
                send(client, &fields, &[procs.as_fd()])
            }
            Err(reason) => {
                let fields = jobs::refused_fields(&reason);
                let fields: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
                send(client, &fields, &[])
            }
        };
        if sent.is_err() {
            drop(self.job_clients.remove(index));
        }
    }

    /// Make a job leaf in the app the request's pidfd belongs to: its
    /// `oom_score_adj` and its open `cgroup.procs`.
    fn grant_job(&mut self, fields: &[&[u8]], fds: Vec<OwnedFd>) -> Result<(i32, File), String> {
        let label = jobs::parse_request(fields).ok_or("malformed job request")?;
        let [member] = <[OwnedFd; 1]>::try_from(fds)
            .map_err(|_| "a job request names a process of its app".to_string())?;
        let pid = jobs::pid_of_pidfd(member.as_fd()).map_err(|error| error.to_string())?;
        let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .map_err(|error| error.to_string())?;
        // Read while the process lived: the pid named it, not a successor.
        if !jobs::alive(member.as_fd()) {
            return Err("the process has ended".into());
        }
        let app = app_cgroup::app_cgroup_of(&cgroup)
            .map(|path| app_cgroup::fs_path(&path))
            .filter(|app| app.parent() == Some(self.dir.as_path()))
            .ok_or("not an app of this session")?;
        let app_id = app
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(app_cgroup::app_id_of_leaf)
            .ok_or("not an app of this session")?;
        self.sweep_jobs(&app);
        if app_cgroup::jobs(&app).len() >= MAX_JOBS_PER_APP {
            return Err(format!("{app_id} already runs {MAX_JOBS_PER_APP} jobs"));
        }
        self.next_job = self.next_job.saturating_add(1);
        let job = app_cgroup::create_job(&app, self.next_job).map_err(|error| error.to_string())?;
        let procs = match std::fs::OpenOptions::new()
            .write(true)
            .open(job.join("cgroup.procs"))
        {
            Ok(procs) => procs,
            Err(error) => {
                let _ = std::fs::remove_dir(&job);
                return Err(error.to_string());
            }
        };
        let label = if label.is_empty() {
            app_id.clone()
        } else {
            label
        };
        self.shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                job.clone(),
                JobInfo {
                    app_id,
                    label,
                    created: Instant::now(),
                },
            );
        self.add_watch(&job.join("memory.events"), Watch::Job(job));
        let tier = app_cgroup::tier_of(&app).unwrap_or(crate::priority::Tier::Foreground);
        Ok((crate::priority::oom_score_adj(tier, Leaf::Job), procs))
    }

    /// Remove the app's empty job leaves whose process had time to join.
    fn sweep_jobs(&mut self, app: &Path) {
        let mut jobs = self
            .shared
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for job in app_cgroup::jobs(app) {
            let fresh = jobs
                .get(&job)
                .is_some_and(|info| info.created.elapsed() < JOB_GRACE);
            if fresh || app_cgroup::populated(&job).unwrap_or(true) {
                continue;
            }
            if std::fs::remove_dir(&job).is_ok() {
                jobs.remove(&job);
                self.group_kills.remove(&job);
            }
        }
        drop(jobs);
    }
}

/// A decoded launch request.
struct Launch<'a> {
    app_id: &'a str,
    kind: AppKind,
    program: CString,
    cwd: CString,
    argv: Vec<CString>,
    env: Vec<CString>,
}

impl<'a> Launch<'a> {
    /// `spawn, app id, kind, program, cwd, argc, argv…, env…`
    fn parse(fields: &[&'a [u8]]) -> Option<Self> {
        let [verb, app_id, kind, program, cwd, argc, rest @ ..] = fields else {
            return None;
        };
        if *verb != b"spawn" || !program.starts_with(b"/") {
            return None;
        }
        let argc: usize = std::str::from_utf8(argc).ok()?.parse().ok()?;
        let arguments = rest.get(..argc)?;
        let env = rest.get(argc..)?;
        let owned = |field: &[u8]| CString::new(field).ok();
        Some(Self {
            app_id: std::str::from_utf8(app_id).ok()?,
            kind: AppKind::from_wire(kind)?,
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

/// `clone3(CLONE_INTO_CGROUP | CLONE_PARENT)` into the leaf `leaf` of the
/// app cgroup `app`, then exec the launch in the child. Returns once the exec
/// happened or failed.
fn start_in(
    leaf: &Path,
    app: &Path,
    launch: &Launch<'_>,
    io: &[OwnedFd; 3],
) -> io::Result<Spawned> {
    let directory = File::open(leaf)?;
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
    // SAFETY: a valid clone_args of the size passed. The child holds only
    // this thread (the memory watch's is not copied) and makes nothing but
    // async-signal-safe system calls on memory prepared above before it
    // execs or exits: no lock another thread held can be waited on.
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
            cgroup: app.to_path_buf(),
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
    /// A Tilo app or a host app: decides its OOM grouping.
    pub kind: AppKind,
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
    pub fn from_command(command: &Command, app_id: &str, kind: AppKind) -> io::Result<Self> {
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
            kind,
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
            self.kind.wire().as_bytes().to_vec(),
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

/// Something the spawner tells the compositor on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnerEvent {
    /// An independent job of `app_id`, called `job`, ended for memory; the
    /// app keeps running.
    JobEnded {
        app_id: String,
        job: String,
        cause: Cause,
    },
}

impl SpawnerEvent {
    fn parse(fields: &[&[u8]]) -> Option<Self> {
        match fields {
            [b"job-ended", app_id, job, cause] => Some(Self::JobEnded {
                app_id: String::from_utf8_lossy(app_id).into_owned(),
                job: String::from_utf8_lossy(job).into_owned(),
                cause: Cause::from_wire(cause)?,
            }),
            _ => None,
        }
    }
}

/// The spawner's event socket, read by one thread of the compositor.
#[derive(Debug)]
pub struct SpawnerEvents(OwnedFd);

impl SpawnerEvents {
    /// The next event; `None` once the spawner is gone.
    #[must_use]
    pub fn recv(&self) -> Option<SpawnerEvent> {
        let mut buffer = [0_u8; jobs::MAX_MESSAGE];
        loop {
            let (length, _) = receive(&self.0, &mut buffer).ok()??;
            let fields = decode(buffer.get(..length).unwrap_or_default());
            if let Some(event) = SpawnerEvent::parse(&fields) {
                return Some(event);
            }
        }
    }
}

/// The compositor's connection to its spawner.
#[derive(Debug)]
pub struct Spawner {
    socket: Mutex<OwnedFd>,
    tree: PathBuf,
    process: Mutex<std::process::Child>,
    events: Mutex<Option<OwnedFd>>,
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
        let (events, their_events) = socketpair(
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
        // Its stdin is the request socket and its stdout the event socket: no
        // other descriptor needs passing, so no pre_exec hook, and `Command`
        // keeps using posix_spawn.
        let mut process = command
            .stdin(Stdio::from(theirs))
            .stdout(Stdio::from(their_events))
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
                events: Mutex::new(Some(events)),
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

    /// The event socket, for the one thread that reads it.
    #[must_use]
    pub fn take_events(&self) -> Option<SpawnerEvents> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .map(SpawnerEvents)
    }

    /// Why the app whose cgroup is `app` ended, if memory ended it. Ask once,
    /// after its launcher was reaped.
    #[must_use]
    pub fn memory_end(&self, app: &Path) -> Option<Cause> {
        let fields: [&[u8]; 2] = [b"ended", app.as_os_str().as_bytes()];
        let mut buffer = vec![0_u8; MAX_REPLY];
        let answer = {
            let socket = self.socket.lock().ok()?;
            let exchanged =
                send(&socket, &fields, &[]).and_then(|()| receive(&socket, &mut buffer));
            drop(socket);
            let Ok(Some((length, _))) = exchanged else {
                self.reap_exited();
                return None;
            };
            buffer.get(..length).unwrap_or_default().to_vec()
        };
        match decode(&answer).as_slice() {
            [b"memory", cause] => Cause::from_wire(cause),
            _ => None,
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
/// harness's). What the spawner reports on its own goes to `on_event`, on
/// that thread. Call once, from the compositor.
pub fn enable(
    launcher_bin: PathBuf,
    tree: Option<PathBuf>,
    on_event: impl Fn(SpawnerEvent) + Send + 'static,
) {
    let started = std::thread::Builder::new()
        .name("tilo-app-spawner".into())
        .spawn(
            move || match Spawner::start(&launcher_bin, tree.as_deref()) {
                Ok(spawner) => {
                    eprintln!(
                        "tilo-spawner: apps start in their own cgroup below {}",
                        spawner.tree().display()
                    );
                    let events = spawner.take_events();
                    let _ = SESSION.set(spawner);
                    // This thread has nothing else to do: it reads the
                    // spawner's events until the spawner is gone.
                    if let Some(events) = events {
                        while let Some(event) = events.recv() {
                            on_event(event);
                        }
                    }
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
    kind: AppKind,
) -> Option<io::Result<(DelegatedChild, RegistrationGate)>> {
    let spawner = SESSION.get()?;
    let (gate, wait_end) = match crate::registration_gate::install_external(command) {
        Ok(pair) => pair,
        Err(error) => return Some(Err(error)),
    };
    let request = match Request::from_command(command, app_id, kind) {
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

/// Why the app whose cgroup is `app` ended, if memory ended it, from this
/// compositor's spawner. Ask once, after its launcher was reaped.
#[must_use]
pub fn memory_end(app: &Path) -> Option<Cause> {
    SESSION.get()?.memory_end(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_portal::jobs::encode;

    #[test]
    fn a_launch_request_round_trips_and_refuses_what_it_cannot_carry() {
        let request = Request {
            app_id: "so.tilo.notes".into(),
            kind: AppKind::Tilo,
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
        assert_eq!(launch.kind, AppKind::Tilo);
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
        relative[3] = b"tilo-launcher".to_vec();
        let refs: Vec<&[u8]> = relative.iter().map(Vec::as_slice).collect();
        assert!(Launch::parse(&refs).is_none());
        // A launch says what it runs: a Tilo app or a host app, nothing else.
        let mut unknown = fields.clone();
        unknown[2] = b"other".to_vec();
        let refs: Vec<&[u8]> = unknown.iter().map(Vec::as_slice).collect();
        assert!(Launch::parse(&refs).is_none());
        assert!(Launch::parse(&[b"spawn", b"a", b"host", b"/x", b"/"]).is_none());
        assert!(
            Launch::parse(&[b"spawn", b"a", b"host", b"/x", b"/", b"3", b"only-one"]).is_none()
        );
        assert!(Launch::parse(&[b"kill", b"a", b"host", b"/x", b"/", b"0"]).is_none());
        assert_eq!(
            SpawnerEvent::parse(&[b"job-ended", b"so.tilo.code", b"rust-analyzer", b"runaway"]),
            Some(SpawnerEvent::JobEnded {
                app_id: "so.tilo.code".into(),
                job: "rust-analyzer".into(),
                cause: Cause::Runaway,
            })
        );
        assert_eq!(
            SpawnerEvent::parse(&[b"job-ended", b"a", b"b", b"other"]),
            None
        );
    }

    #[test]
    fn a_request_is_what_the_command_would_have_run() {
        let mut command = Command::new("/usr/bin/tilo-launcher");
        command
            .args(["run", "--app-id", "so.tilo.notes"])
            .env("TILO_SPAWNER_TEST_SET", "1")
            .env_remove("PATH")
            .current_dir("/tmp");
        let request = Request::from_command(&command, "so.tilo.notes", AppKind::Tilo).unwrap();
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
        assert!(Request::from_command(&Command::new("tilo-launcher"), "x", AppKind::Host).is_err());
    }
}
