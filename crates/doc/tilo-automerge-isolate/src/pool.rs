//! The caller's side: a few long-lived worker processes, each serving one
//! request at a time.
//!
//! A worker keeps its own copies of recently used documents, so the caller
//! sends only the incoming bytes. When the worker does not hold the caller's
//! state it says so, and the caller sends the changes the worker is missing,
//! or a full save. A worker that crashes, runs out of memory or overruns its
//! deadline is killed, the input is rejected, and the next request starts a
//! fresh one.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, mpsc};
use std::time::Duration;

use automerge::{AutoCommit, SaveOptions};

use crate::engine::{self, Key};
use crate::wire::{self, Prepare, Reply, Request};
use crate::worker;
use crate::{Applied, Dependencies, Limits, Rejected};

/// Extra time for a full document sent to rebuild a worker's copy.
const REBUILD_PER_MIB: Duration = Duration::from_millis(250);
/// How long a new worker may take to say it is ready.
const STARTUP: Duration = Duration::from_secs(5);

pub(crate) struct Pool {
    limits: Limits,
    slots: Vec<Mutex<Option<Worker>>>,
    next: AtomicUsize,
    /// Which worker last saw each document state.
    routes: Mutex<HashMap<Key, usize>>,
}

impl Pool {
    pub(crate) fn new(limits: Limits) -> Self {
        let count = limits.workers.max(1);
        Self {
            limits,
            slots: (0..count).map(|_| Mutex::new(None)).collect(),
            next: AtomicUsize::new(0),
            routes: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) const fn limits(&self) -> &Limits {
        &self.limits
    }

    pub(crate) fn apply(
        &self,
        doc: &mut AutoCommit,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        let key = Key::of(doc);
        let slot = self.route(&key);
        let mut worker = lock(self.slots.get(slot).ok_or(Rejected::Crashed)?);
        let mut next = Base::Cached;
        loop {
            let (prepare, trusted) = match &next {
                Base::Cached => (Prepare::Cached, 0),
                Base::After(from, delta) => (
                    Prepare::After {
                        from: from.clone(),
                        delta,
                    },
                    delta.len(),
                ),
                Base::Full(saved) => (Prepare::Full(saved), saved.len()),
            };
            let request = Request::Apply {
                key: key.clone(),
                dependencies,
                prepare,
                incoming,
            };
            match self.call(&mut worker, &request, trusted)? {
                Reply::Uncached(cached) => {
                    next = match next {
                        Base::Cached => catch_up(doc, &cached).map_or_else(
                            || Base::Full(full_save(doc)),
                            |(from, delta)| Base::After(from, delta),
                        ),
                        Base::After(..) => Base::Full(full_save(doc)),
                        // A worker that cannot build the caller's own save
                        // is not working.
                        Base::Full(_) => return Err(Rejected::Crashed),
                    };
                }
                Reply::Invalid(message) => return Err(Rejected::Invalid(message)),
                Reply::Applied(Applied::MissingDependencies) => {
                    return Ok(Applied::MissingDependencies);
                }
                Reply::Applied(Applied::Changed(_)) => {
                    drop(worker);
                    let applied = engine::apply_trusted(doc, incoming, dependencies)?;
                    self.remember(Key::of(doc), slot);
                    return Ok(applied);
                }
            }
        }
    }

    pub(crate) fn load(&self, bytes: &[u8]) -> Result<AutoCommit, Rejected> {
        let slot = self.any_slot();
        let mut worker = lock(self.slots.get(slot).ok_or(Rejected::Crashed)?);
        match self.call(&mut worker, &Request::Load { incoming: bytes }, 0)? {
            Reply::Applied(_) => {
                drop(worker);
                let mut doc = engine::load(bytes)?;
                self.remember(Key::of(&mut doc), slot);
                Ok(doc)
            }
            Reply::Invalid(message) => Err(Rejected::Invalid(message)),
            Reply::Uncached(_) => Err(Rejected::Crashed),
        }
    }

    fn route(&self, key: &Key) -> usize {
        let known = lock(&self.routes).get(key).copied();
        known.unwrap_or_else(|| self.any_slot())
    }

    /// Round robin over the workers.
    fn any_slot(&self) -> usize {
        self.next
            .fetch_add(1, Ordering::Relaxed)
            .checked_rem(self.slots.len())
            .unwrap_or(0)
    }

    fn remember(&self, key: Key, slot: usize) {
        let mut routes = lock(&self.routes);
        // Each worker keeps at most `max_documents`; older routes only
        // point at copies it has since dropped.
        let bound = self
            .limits
            .max_documents
            .saturating_mul(self.slots.len())
            .saturating_mul(4);
        if routes.len() >= bound {
            routes.clear();
        }
        routes.insert(key, slot);
    }

    fn call(
        &self,
        slot: &mut Option<Worker>,
        request: &Request<'_>,
        trusted_bytes: usize,
    ) -> Result<Reply, Rejected> {
        if slot.is_none() {
            *slot = Some(Worker::spawn(&self.limits)?);
        }
        let worker = slot.as_mut().ok_or(Rejected::Crashed)?;
        let mib = u32::try_from(trusted_bytes / (1024 * 1024)).unwrap_or(u32::MAX);
        let deadline = self
            .limits
            .timeout
            .saturating_add(REBUILD_PER_MIB.saturating_mul(mib));
        match worker.call(request.encode(), deadline) {
            Ok(reply) => Ok(reply),
            Err(error) => {
                if let Some(worker) = slot.take() {
                    worker.kill();
                }
                Err(error)
            }
        }
    }
}

/// What the worker builds the caller's document from.
enum Base {
    /// Its own copy.
    Cached,
    /// Its copy at an earlier state, plus the caller's changes since.
    After(Key, Vec<u8>),
    /// The caller's full save.
    Full(Vec<u8>),
}

fn full_save(doc: &mut AutoCommit) -> Vec<u8> {
    doc.save_with_options(SaveOptions {
        deflate: false,
        retain_orphans: true,
    })
}

/// The changes the worker needs to bring one of its copies up to `doc`,
/// if it holds a state that `doc` descends from.
fn catch_up(doc: &mut AutoCommit, cached: &[Key]) -> Option<(Key, Vec<u8>)> {
    cached.iter().find_map(|key| {
        let heads = key.complete_heads()?;
        heads
            .iter()
            .all(|hash| doc.get_change_meta_by_hash(hash).is_some())
            .then(|| (key.clone(), doc.save_after(&heads)))
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Worker {
    child: Child,
    requests: mpsc::Sender<Vec<u8>>,
    replies: mpsc::Receiver<io::Result<Vec<u8>>>,
}

impl Worker {
    fn spawn(limits: &Limits) -> Result<Self, Rejected> {
        let command = command(limits);
        let (request_read, request_write) = pipe().map_err(unavailable)?;
        let (reply_read, reply_write) = pipe().map_err(unavailable)?;
        let child = spawn_with(command, request_read, reply_write, limits.memory_bytes)
            .map_err(unavailable)?;
        let (requests, outgoing) = mpsc::channel::<Vec<u8>>();
        let mut writer = File::from(request_write);
        std::thread::Builder::new()
            .name("automerge-isolate-writer".into())
            .spawn(move || {
                for frame in outgoing {
                    if wire::write_frame(&mut writer, &frame).is_err() {
                        break;
                    }
                }
            })
            .map_err(unavailable)?;
        let (incoming, replies) = mpsc::channel();
        let mut reader = BufReader::new(File::from(reply_read));
        std::thread::Builder::new()
            .name("automerge-isolate-reader".into())
            .spawn(move || {
                loop {
                    let frame = match wire::read_frame(&mut reader, wire::MAX_REPLY_BYTES) {
                        Ok(Some(frame)) => Ok(frame),
                        Ok(None) => Err(io::ErrorKind::UnexpectedEof.into()),
                        Err(error) => Err(error),
                    };
                    let done = frame.is_err();
                    if incoming.send(frame).is_err() || done {
                        break;
                    }
                }
            })
            .map_err(unavailable)?;
        let worker = Self {
            child,
            requests,
            replies,
        };
        match worker.replies.recv_timeout(STARTUP) {
            Ok(Ok(frame)) if frame == wire::READY => Ok(worker),
            _ => {
                worker.kill();
                Err(Rejected::Unavailable(not_serving()))
            }
        }
    }

    fn call(&self, frame: Vec<u8>, deadline: Duration) -> Result<Reply, Rejected> {
        self.requests.send(frame).map_err(|_| Rejected::Crashed)?;
        match self.replies.recv_timeout(deadline) {
            Ok(Ok(frame)) => Reply::decode(&frame).ok_or(Rejected::Crashed),
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Err(Rejected::Crashed),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Rejected::TimedOut),
        }
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the request pipe ends the worker's loop; a worker stuck
        // on an input is killed.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// How to start this executable as a worker. The same arguments select
/// worker mode in a program that calls [`crate::dispatch_early`] and, as a
/// test filter, the test `test_worker!` defines in a test binary.
fn command(limits: &Limits) -> Command {
    let mut command = Command::new("/proc/self/exe");
    command
        .args([worker::ARG, "--nocapture", "--test-threads=1", "--quiet"])
        .env(worker::ENV, limits.max_documents.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    command
}

fn not_serving() -> String {
    "this executable does not serve automerge workers: call \
     `tilo_automerge_isolate::dispatch_early()` at the top of `main`, or add \
     `tilo_automerge_isolate::test_worker!();` to a test crate"
        .to_owned()
}

#[expect(clippy::needless_pass_by_value, reason = "used as a map_err adapter")]
fn unavailable(error: io::Error) -> Rejected {
    Rejected::Unavailable(error.to_string())
}

/// A close-on-exec pipe whose ends are numbered above the worker's fixed
/// descriptors, so moving them into place never overwrites one another.
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded, so both are open descriptors we now own.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Ok((above_fixed(read)?, above_fixed(write)?))
}

fn above_fixed(fd: OwnedFd) -> io::Result<OwnedFd> {
    let minimum = worker::REPLY_FD.saturating_add(1);
    if fd.as_raw_fd() >= minimum {
        return Ok(fd);
    }
    // SAFETY: F_DUPFD_CLOEXEC on an open descriptor returns a new one.
    let moved = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, minimum) };
    if moved < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a fresh descriptor we own.
    Ok(unsafe { OwnedFd::from_raw_fd(moved) })
}

/// Spawn `command` with the request pipe on descriptor 3, the reply pipe on
/// descriptor 4, no core dumps, and its heap capped at `memory_bytes`.
///
/// Workers are spawned from one long-lived thread: the kernel delivers the
/// parent-death signal when the spawning *thread* exits, and callers' threads
/// come and go.
fn spawn_with(
    mut command: Command,
    request_read: OwnedFd,
    reply_write: OwnedFd,
    memory_bytes: u64,
) -> io::Result<Child> {
    let (request, reply) = (request_read.as_raw_fd(), reply_write.as_raw_fd());
    // SAFETY: the closure runs between fork and exec and only makes
    // async-signal-safe system calls on descriptors this process owns.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(request, worker::REQUEST_FD) < 0
                || libc::dup2(reply, worker::REPLY_FD) < 0
            {
                return Err(io::Error::last_os_error());
            }
            for (resource, value) in [
                (libc::RLIMIT_CORE, 0),
                // Heap and private anonymous mappings; the executable's own
                // mapped image does not count.
                (libc::RLIMIT_DATA, memory_bytes),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &raw const limit) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = spawner().spawn(command)?;
    // The worker holds its own copies now.
    drop((request_read, reply_write));
    Ok(child)
}

type SpawnJob = (Command, mpsc::Sender<io::Result<Child>>);

struct Spawner(Mutex<mpsc::Sender<SpawnJob>>);

impl Spawner {
    fn spawn(&self, command: Command) -> io::Result<Child> {
        let (reply, result) = mpsc::channel();
        lock(&self.0)
            .send((command, reply))
            .map_err(|_| io::Error::other("worker spawner stopped"))?;
        result
            .recv()
            .map_err(|_| io::Error::other("worker spawner stopped"))?
    }
}

fn spawner() -> &'static Spawner {
    static SPAWNER: std::sync::OnceLock<Spawner> = std::sync::OnceLock::new();
    SPAWNER.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<SpawnJob>();
        let _ = std::thread::Builder::new()
            .name("automerge-isolate-spawner".into())
            .spawn(move || {
                for (mut command, reply) in queue {
                    let _ = reply.send(command.spawn());
                }
            });
        Spawner(Mutex::new(jobs))
    })
}
