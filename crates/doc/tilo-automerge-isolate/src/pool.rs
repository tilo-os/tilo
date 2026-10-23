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
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use automerge::{AutoCommit, SaveOptions};

use crate::engine::{self, Key};
use crate::wire::{self, Prepare, Reply, Request};
use crate::worker;
use crate::{Applied, Dependencies, Limits, Rejected};
use tilo_lock::MutexRecover as _;

/// Extra time for a full document sent to rebuild a worker's copy.
const REBUILD_PER_MIB: Duration = Duration::from_millis(250);
/// Document states whose worker the pool remembers; the least recently used
/// quarter is forgotten when full.
const ROUTES: usize = 4096;
/// How long a new worker that is still running may take to say it is ready.
/// Starting means executing this whole program again, which on a loaded
/// machine (a cold page cache, every core busy) takes seconds; a worker that
/// cannot serve exits at once and is reported as soon as it does, so only a
/// slow start ever waits this long.
const STARTUP: Duration = Duration::from_secs(30);

pub(crate) struct Pool {
    limits: Limits,
    slots: Vec<Mutex<Option<Worker>>>,
    next: AtomicUsize,
    /// Which worker last saw each document state, and when.
    routes: Mutex<Routes>,
    /// Times an apply rebuilt a worker copy from a full save.
    #[cfg(test)]
    full_rebuilds: AtomicUsize,
}

impl Pool {
    pub(crate) fn new(limits: Limits) -> Self {
        let count = limits.workers.max(1);
        Self {
            limits,
            slots: (0..count).map(|_| Mutex::new(None)).collect(),
            next: AtomicUsize::new(0),
            routes: Mutex::new(Routes::default()),
            #[cfg(test)]
            full_rebuilds: AtomicUsize::new(0),
        }
    }

    pub(crate) const fn limits(&self) -> &Limits {
        &self.limits
    }

    #[cfg(test)]
    pub(crate) fn full_rebuilds(&self) -> usize {
        self.full_rebuilds.load(Ordering::Relaxed)
    }

    pub(crate) fn apply(
        &self,
        doc: &mut AutoCommit,
        incoming: &[u8],
        dependencies: Dependencies,
    ) -> Result<Applied, Rejected> {
        let key = Key::of(doc);
        let slot = self.route(&key);
        let mut worker = (self.slots.get(slot).ok_or(Rejected::Crashed)?).lock_recover();
        let mut next = if key.complete_heads().is_some() {
            Base::Cached
        } else {
            // Missing dependency hashes do not identify the queued changes.
            #[cfg(test)]
            self.full_rebuilds.fetch_add(1, Ordering::Relaxed);
            Base::Full(full_save(doc))
        };
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
            match self.call(&mut worker, &request, trusted, incoming.len())? {
                Reply::Uncached(cached) => {
                    next = match next {
                        Base::Cached => catch_up(doc, &cached).map_or_else(
                            || {
                                #[cfg(test)]
                                self.full_rebuilds.fetch_add(1, Ordering::Relaxed);
                                Base::Full(full_save(doc))
                            },
                            |(from, delta)| Base::After(from, delta),
                        ),
                        Base::After(..) => {
                            #[cfg(test)]
                            self.full_rebuilds.fetch_add(1, Ordering::Relaxed);
                            Base::Full(full_save(doc))
                        }
                        // A worker that cannot build the caller's own save
                        // is not working.
                        Base::Full(_) => return Err(Rejected::Crashed),
                    };
                }
                Reply::Invalid(message) => return Err(Rejected::Invalid(message)),
                // Only a load is answered with this.
                Reply::Loaded(_) => return Err(Rejected::Crashed),
                Reply::Applied(Applied::MissingDependencies) => {
                    return Ok(Applied::MissingDependencies);
                }
                Reply::Applied(Applied::Changed(_)) => {
                    drop(worker);
                    // The worker may keep the state it started from, for
                    // other documents at it.
                    self.remember(key, slot);
                    let applied = engine::apply_trusted(doc, incoming, dependencies)?;
                    self.remember(Key::of(doc), slot);
                    return Ok(applied);
                }
            }
        }
    }

    pub(crate) fn load(&self, bytes: &[u8]) -> Result<AutoCommit, Rejected> {
        let slot = self.any_slot();
        let mut worker = (self.slots.get(slot).ok_or(Rejected::Crashed)?).lock_recover();
        match self.call(
            &mut worker,
            &Request::Load { incoming: bytes },
            0,
            bytes.len(),
        )? {
            Reply::Loaded(hashes) => {
                drop(worker);
                // The worker rebuilt and hashed every change, and checked
                // the result against the heads `bytes` store; this load takes
                // its hashes rather than doing that a second time.
                let mut doc = engine::load_validated(bytes, &hashes)?;
                self.remember(Key::of(&mut doc), slot);
                Ok(doc)
            }
            Reply::Invalid(message) => Err(Rejected::Invalid(message)),
            Reply::Applied(_) | Reply::Uncached(_) => Err(Rejected::Crashed),
        }
    }

    fn route(&self, key: &Key) -> usize {
        let known = self.routes.lock_recover().get(key);
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
        if key.complete_heads().is_some() {
            self.routes.lock_recover().insert(key, slot);
        }
    }

    /// Send `request` to the worker in `slot`, starting one if there is
    /// none. A worker that had exited for being idle is replaced, and a
    /// request it died waiting for is sent again to its replacement.
    fn call(
        &self,
        slot: &mut Option<Worker>,
        request: &Request<'_>,
        trusted_bytes: usize,
        untrusted_bytes: usize,
    ) -> Result<Reply, Rejected> {
        let deadline = deadline(&self.limits, trusted_bytes, untrusted_bytes);
        let mut frame = Some(request.encode());
        let mut retried = false;
        loop {
            if slot.as_mut().is_some_and(Worker::exited) {
                *slot = None;
            }
            if slot.is_none() {
                *slot = Some(Worker::spawn(&self.limits)?);
            }
            let worker = slot.as_mut().ok_or(Rejected::Crashed)?;
            let frame = frame.take().unwrap_or_else(|| request.encode());
            match worker.call(frame, deadline) {
                Ok(reply) => return Ok(reply),
                Err(error) => {
                    let idle = slot.take().is_some_and(Worker::kill_after_idle);
                    if idle && !retried && error == Rejected::Crashed {
                        retried = true;
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }
}

/// How long a request may take: [`Limits::timeout`], plus time for a full
/// save the worker rebuilds from and for the untrusted input it decodes.
fn deadline(limits: &Limits, trusted_bytes: usize, untrusted_bytes: usize) -> Duration {
    limits
        .timeout
        .saturating_add(REBUILD_PER_MIB.mul_f64(mib(trusted_bytes)))
        .saturating_add(limits.timeout_per_mib.mul_f64(mib(untrusted_bytes)))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a deadline needs no more precision than f64 gives"
)]
fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Which worker last saw each document state, bounded by forgetting the
/// least recently used states rather than all of them at once.
#[derive(Default)]
struct Routes {
    slots: HashMap<Key, (usize, u64)>,
    tick: u64,
}

impl Routes {
    fn get(&mut self, key: &Key) -> Option<usize> {
        self.tick = self.tick.saturating_add(1);
        let tick = self.tick;
        self.slots.get_mut(key).map(|(slot, used)| {
            *used = tick;
            *slot
        })
    }

    fn insert(&mut self, key: Key, slot: usize) {
        self.tick = self.tick.saturating_add(1);
        if self.slots.len() >= ROUTES && !self.slots.contains_key(&key) {
            let mut used: Vec<u64> = self.slots.values().map(|(_, used)| *used).collect();
            let cut = used.len() / 4;
            let (_, oldest, _) = used.select_nth_unstable(cut);
            let oldest = *oldest;
            self.slots.retain(|_, (_, used)| *used > oldest);
        }
        self.slots.insert(key, (slot, self.tick));
    }
}

/// What the worker builds the caller's document from.
enum Base {
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
            // It started, but could not confine itself and said why.
            Ok(Ok(frame)) => {
                worker.kill();
                Err(Rejected::Unavailable(format!(
                    "automerge worker {}",
                    String::from_utf8_lossy(&frame)
                )))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                worker.kill();
                Err(Rejected::Unavailable(format!(
                    "automerge worker did not say it was ready within {STARTUP:?}"
                )))
            }
            // It ended without a word: this program does not serve workers.
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => {
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

    /// Whether the process has exited, as an idle worker does.
    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the worker if it still runs, and say whether it had instead
    /// ended itself for being idle.
    fn kill_after_idle(mut self) -> bool {
        let _ = self.child.kill();
        self.child
            .wait()
            .is_ok_and(|status| status.signal() == Some(libc::SIGALRM))
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
        .env(
            worker::ENV,
            worker::Config {
                documents: limits.max_documents,
                cache_bytes: limits.cache_bytes,
                idle_seconds: limits.idle.as_secs().max(1),
            }
            .encode(),
        )
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
        self.0
            .lock_recover()
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

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable as _;

    use super::*;
    use crate::wire::Escape;

    fn limits() -> Limits {
        Limits {
            workers: 1,
            timeout: tilo_test_env::hang_bound(),
            ..Limits::default()
        }
    }

    fn status_line(pid: u32, field: &str) -> String {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        status
            .lines()
            .find_map(|line| line.strip_prefix(field))
            .expect("the field is in /proc/<pid>/status")
            .trim()
            .to_owned()
    }

    #[test]
    fn a_ready_worker_is_already_confined_and_still_admits() {
        let worker = Worker::spawn(&limits()).unwrap();
        let pid = worker.child.id();
        // 2 = a seccomp filter is installed.
        assert_eq!(status_line(pid, "Seccomp:"), "2");
        assert_eq!(status_line(pid, "NoNewPrivs:"), "1");
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "k", "v").unwrap();
        let saved = doc.save();
        let reply = worker.call(Request::Load { incoming: &saved }.encode(), STARTUP);
        assert!(matches!(reply, Ok(Reply::Loaded(ref hashes)) if *hashes == doc.get_heads()));
    }

    #[test]
    fn incomplete_keys_never_authorize_a_cached_worker_queue() {
        let worker = Worker::spawn(&limits()).unwrap();
        let mut base = AutoCommit::new();
        base.put(automerge::ROOT, "seed", "committed").unwrap();
        base.commit();
        let dependency = base.get_heads();
        let committed = full_save(&mut base);
        let mut queued = Vec::new();
        for value in ["first original queue", "different original queue"] {
            let mut branch = base.fork();
            branch.put(automerge::ROOT, "queued", value).unwrap();
            branch.commit();
            let orphan = branch.save_after(&dependency);
            let mut doc = AutoCommit::new();
            // Locally generated valid bytes: this prepares the caller's state,
            // not a substitute for the actual confined-worker request below.
            assert_eq!(
                engine::apply_trusted(&mut doc, &orphan, Dependencies::RetainOrphans),
                Ok(Applied::Changed(false))
            );
            assert_eq!(doc.get_missing_deps(&[]), dependency);
            let saved = full_save(&mut doc);
            assert!(matches!(
                worker.call(Request::Load { incoming: &saved }.encode(), STARTUP),
                Ok(Reply::Loaded(_))
            ));
            queued.push((doc, saved));
        }
        assert_ne!(queued[0].1, queued[1].1, "different queued changes");
        let key = Key::of(&mut queued[0].0);
        assert_eq!(key, Key::of(&mut queued[1].0));
        assert!(key.complete_heads().is_none());
        // Before the bypass this raw response is Applied: the worker used the
        // second queue under the first queue's identical incomplete key. Do
        // not apply in the caller, where independent decoding masks that error.
        let reply = worker.call(
            Request::Apply {
                key: key.clone(),
                dependencies: Dependencies::RequireComplete,
                prepare: Prepare::Cached,
                incoming: &committed,
            }
            .encode(),
            STARTUP,
        );
        assert!(
            matches!(reply, Ok(Reply::Uncached(ref keys)) if keys.iter().all(|key| key.complete_heads().is_some())),
            "an incomplete state cannot authorize cached worker acceptance"
        );
        // Sending the actual retained queue resolves its dependency and may
        // cache the resulting COMPLETE state under its exact applied heads.
        assert!(matches!(
            worker.call(
                Request::Apply {
                    key,
                    dependencies: Dependencies::RequireComplete,
                    prepare: Prepare::Full(&queued[0].1),
                    incoming: &committed,
                }
                .encode(),
                STARTUP,
            ),
            Ok(Reply::Applied(Applied::Changed(true)))
        ));
        assert_eq!(
            engine::apply_trusted(&mut queued[0].0, &committed, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
        assert!(matches!(
            worker.call(
                Request::Apply {
                    key: Key::of(&mut queued[0].0),
                    dependencies: Dependencies::RequireComplete,
                    prepare: Prepare::Cached,
                    incoming: &[],
                }
                .encode(),
                STARTUP,
            ),
            Ok(Reply::Applied(Applied::Changed(false)))
        ));
    }

    #[test]
    fn a_path_or_socket_syscall_is_refused_or_kills_the_worker() {
        // The caller itself may do both, so only the sandbox can refuse them.
        assert!(std::fs::File::open("/proc/self/status").is_ok());
        assert!(std::os::unix::net::UnixStream::pair().is_ok());

        // `openat` is answered with EPERM and the worker lives on.
        let worker = Worker::spawn(&limits()).unwrap();
        let reply = worker.call(Request::Escape(Escape::Open).encode(), STARTUP);
        let message = match reply {
            Ok(Reply::Invalid(message)) => message,
            _ => String::new(),
        };
        assert!(
            message.contains("os error 1"),
            "open not refused: {message:?}"
        );
        let reply = worker.call(Request::Load { incoming: &[] }.encode(), STARTUP);
        assert!(matches!(reply, Ok(Reply::Loaded(_))));

        // Anything else off the list kills it.
        let mut worker = Worker::spawn(&limits()).unwrap();
        let reply = worker.call(Request::Escape(Escape::Socket).encode(), STARTUP);
        assert!(
            matches!(reply, Err(Rejected::Crashed)),
            "socket was allowed"
        );
        let status = worker.child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGSYS), "{status}");

        let pool = Pool::new(limits());
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "after", 1).unwrap();
        assert!(pool.load(&doc.save()).is_ok());
    }

    #[test]
    fn the_deadline_grows_with_the_input() {
        let limits = Limits::default();
        let mib = 1024 * 1024;
        assert_eq!(deadline(&limits, 0, 0), limits.timeout);
        assert_eq!(
            deadline(&limits, 0, 3 * mib),
            limits.timeout + limits.timeout_per_mib * 3
        );
        assert_eq!(
            deadline(&limits, 4 * mib, 0),
            limits.timeout + REBUILD_PER_MIB * 4
        );
    }

    #[test]
    fn routes_forget_the_least_recently_used_states_not_all() {
        let mut routes = Routes::default();
        let key = |n: usize| Key::from_bytes(n.to_le_bytes().to_vec());
        routes.insert(key(0), 1);
        for n in 1..ROUTES {
            routes.insert(key(n), 0);
        }
        // Touch the first state so it is the most recently used.
        assert_eq!(routes.get(&key(0)), Some(1));
        routes.insert(key(ROUTES), 0);
        assert_eq!(routes.get(&key(0)), Some(1), "a live route was dropped");
        assert_eq!(routes.get(&key(ROUTES)), Some(0));
        assert!(routes.get(&key(1)).is_none(), "the oldest route was kept");
        assert!(routes.slots.len() > ROUTES / 2);
    }

    #[test]
    fn an_idle_worker_exits_and_the_next_request_starts_another() {
        let pool = Pool::new(Limits {
            idle: Duration::from_secs(1),
            ..limits()
        });
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "k", 1).unwrap();
        doc.commit();
        let mut first = pool.load(&doc.save()).unwrap();
        let pid = (pool.slots[0]).lock_recover().as_ref().unwrap().child.id();
        tilo_test_env::wait_until(format!("idle worker {pid} to exit"), || {
            (pool.slots[0]).lock_recover().as_mut().unwrap().exited()
        });
        let mut remote = first.fork();
        remote.put(automerge::ROOT, "k", 2).unwrap();
        remote.commit();
        let change = remote.save_after(&doc.get_heads());
        assert_eq!(
            pool.apply(&mut doc, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
    }

    #[test]
    fn a_worker_that_died_idle_while_the_request_was_sent_is_replaced() {
        let pool = Pool::new(limits());
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "k", 1).unwrap();
        doc.commit();
        pool.load(&doc.save()).unwrap();
        let mut worker = (pool.slots[0]).lock_recover().take().unwrap();
        let pid = libc::pid_t::try_from(worker.child.id()).unwrap();
        // SAFETY: signalling our own child.
        unsafe { libc::kill(pid, libc::SIGALRM) };
        tilo_test_env::wait_until("the signalled worker to exit", || worker.exited());
        *(pool.slots[0]).lock_recover() = Some(worker);
        assert!(pool.load(&doc.save()).is_ok());
    }

    #[test]
    fn isolate_load_avoids_a_full_rebuild_on_the_next_change() {
        let pool = Pool::new(Limits {
            workers: 2,
            ..limits()
        });
        let mut seeded = AutoCommit::new();
        for i in 0..32 {
            seeded
                .put(automerge::ROOT, format!("seed{i}"), i64::from(i))
                .unwrap();
            seeded.commit();
        }
        let saved = seeded.save();

        let mut warm = pool.load(&saved).unwrap();
        let before_warm = pool.full_rebuilds();
        let mut remote = warm.fork();
        remote.put(automerge::ROOT, "warm", 1).unwrap();
        remote.commit();
        let change = remote.save_after(&warm.get_heads());
        assert_eq!(
            pool.apply(&mut warm, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
        assert_eq!(
            pool.full_rebuilds(),
            before_warm,
            "Pool::load must leave the document warm for the next change"
        );

        let mut cold = AutoCommit::load(&saved).unwrap();
        let before_cold = pool.full_rebuilds();
        let mut remote = cold.fork();
        remote.put(automerge::ROOT, "cold", 1).unwrap();
        remote.commit();
        let change = remote.save_after(&cold.get_heads());
        assert_eq!(
            pool.apply(&mut cold, &change, Dependencies::RequireComplete),
            Ok(Applied::Changed(true))
        );
        assert!(
            pool.full_rebuilds() > before_cold,
            "a document never seen by a worker must still rebuild once from a full save"
        );
    }

    /// Two collaborators' history with concurrent heads, merges, a text and
    /// an author, saved as one document chunk.
    fn collaborated() -> AutoCommit {
        let mut ada = AutoCommit::new()
            .with_actor(automerge::ActorId::from(vec![0xa1; 16]))
            .with_author(Some(automerge::Author::from(b"ada".to_vec())));
        let text = ada
            .put_object(automerge::ROOT, "body", automerge::ObjType::Text)
            .unwrap();
        ada.commit();
        let mut bo = ada
            .fork()
            .with_actor(automerge::ActorId::from(vec![0xb2; 16]));
        for i in 0..24_usize {
            let (writer, other) = if i % 3 == 0 {
                (&mut bo, &mut ada)
            } else {
                (&mut ada, &mut bo)
            };
            writer.splice_text(&text, 0, 0, &format!("w{i} ")).unwrap();
            writer
                .put(automerge::ROOT, "last", i64::try_from(i).unwrap())
                .unwrap();
            writer.commit();
            if i % 5 == 4 {
                tilo_automerge::ApplyIncoming::merge_incoming(other, writer).unwrap();
            }
        }
        bo.put(automerge::ROOT, "bo", true).unwrap();
        bo.commit();
        tilo_automerge::ApplyIncoming::merge_incoming(&mut ada, &mut bo).unwrap();
        ada
    }

    #[test]
    fn a_load_hands_the_caller_the_hashes_its_own_load_takes() {
        let mut doc = collaborated();
        assert_eq!(doc.get_heads().len(), 2, "concurrent heads");
        let saved = doc.save();
        let worker = Worker::spawn(&limits()).unwrap();
        let reply = worker.call(Request::Load { incoming: &saved }.encode(), STARTUP);
        let Ok(Reply::Loaded(hashes)) = reply else {
            panic!("a load is answered with its hashes");
        };
        // Every change's hash, in the order the document chunk lists them:
        // the order the known-hash load requires.
        let mut reference = engine::load(&saved).unwrap();
        assert_eq!(hashes, reference.document().change_hashes());
        assert_eq!(hashes.len(), doc.get_changes(&[]).len());

        let mut validated = engine::load_validated(&saved, &hashes).unwrap();
        assert_eq!(validated.get_heads(), reference.get_heads());
        assert_eq!(
            validated.document().change_hashes(),
            reference.document().change_hashes()
        );
        let changes = |doc: &mut AutoCommit| {
            doc.get_changes(&[])
                .iter()
                .map(|change| (change.hash(), change.max_op(), change.raw_bytes().to_vec()))
                .collect::<Vec<_>>()
        };
        assert_eq!(changes(&mut validated), changes(&mut reference));
        assert_eq!(validated.save(), reference.save());

        // And so does the pool, end to end.
        let pool = Pool::new(limits());
        let mut admitted = pool.load(&saved).unwrap();
        assert_eq!(changes(&mut admitted), changes(&mut reference));
        assert_eq!(admitted.save(), reference.save());
    }

    #[test]
    fn input_that_is_not_one_document_chunk_loads_without_hashes() {
        let mut doc = collaborated();
        let heads = doc.get_heads();
        let mut saved = doc.save();
        let mut remote = doc.fork();
        remote.put(automerge::ROOT, "after", 1).unwrap();
        remote.commit();
        // A document chunk followed by a change chunk: the worker's document
        // holds a change the document chunk does not.
        saved.extend(remote.save_after(&heads));
        let worker = Worker::spawn(&limits()).unwrap();
        let reply = worker.call(Request::Load { incoming: &saved }.encode(), STARTUP);
        assert!(
            matches!(reply, Ok(Reply::Loaded(ref hashes)) if hashes.is_empty()),
            "no hashes for a document chunk with more after it"
        );
        let pool = Pool::new(limits());
        let mut admitted = pool.load(&saved).unwrap();
        assert_eq!(admitted.get_heads(), remote.get_heads());
        assert_eq!(admitted.save(), engine::load(&saved).unwrap().save());
    }
}
