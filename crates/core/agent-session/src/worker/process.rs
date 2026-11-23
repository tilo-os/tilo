//! The supervisor's handle on app worker processes: one process
//! ([`Process`]), and the pool of processes documents, views and reads are held
//! in, each in a slot of its own ([`Lease`]).
//!
//! A worker answers its slots side by side, each on a thread of its own, and
//! the supervisor sends each request with its slot and routes each reply back
//! to the caller waiting for that slot. So a request that takes long holds up
//! only its own slot: every other document, view and read in the same worker is
//! answered meanwhile. A caller that waits past [`REQUEST_PATIENCE`] stops
//! waiting and lets that one slot go ([`Stopped::Slow`]); the worker and its
//! other slots go on.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::ENV;
use super::proto::{FRAMING, Mode, PROTOCOL_VERSION, Reply, Request, Slot};
use tilo_lock::MutexRecover as _;

/// How long a worker may take to start, load what it needs and confine
/// itself.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a caller waits for one request before it gives up on it: the
/// slot answering it is let go and what it held is given again, while the
/// worker and every other slot in it go on.
const REQUEST_PATIENCE: Duration = Duration::from_secs(60);

/// How long a pooled worker that holds no slot is kept for the next one
/// before it ends: the server's memory follows what is open, not what was.
#[cfg(not(test))]
const IDLE_GRACE: Duration = Duration::from_secs(30);
#[cfg(test)]
const IDLE_GRACE: Duration = Duration::from_millis(300);

/// The only variables a worker inherits: locale and where fonts are found.
/// Never the supervisor's own (model API keys, state paths).
const PASSED_ENV: &[&str] = &[
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "HOME",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "RUST_BACKTRACE",
    "RUST_LOG",
];

/// The callers waiting for a reply, by the slot they asked; `None` once the
/// worker stopped, which every caller still waiting learns at once.
type Waiting = Arc<Mutex<Option<HashMap<Slot, mpsc::Sender<Reply>>>>>;

/// Why a call to a worker answered nothing.
enum CallError {
    /// The worker is gone, or spoke garbage; it is no longer usable.
    Stopped(String),
    /// The worker is alive and still working on the request.
    Slow,
}

/// One running worker. Dropping it ends the process.
pub(crate) struct Process {
    child: Mutex<Child>,
    id: u32,
    /// Where requests are written, one whole frame at a time.
    writer: Mutex<UnixStream>,
    waiting: Waiting,
    mode: Mode,
}

impl Process {
    /// Start a worker in `mode` and wait until it has confined itself.
    ///
    /// # Errors
    ///
    /// Why it did not start, or that it could not confine itself; nothing of
    /// an app runs then.
    pub(crate) fn start(mode: Mode) -> Result<Self, String> {
        let (ours, theirs) = UnixStream::pair().map_err(|error| error.to_string())?;
        let mut command = Command::new(executable());
        command
            .args(super::worker_args())
            .env_clear()
            .env(ENV, mode.as_str())
            // The parent the worker watches for: the host may be its process
            // namespace's init (a container runs one program), which the
            // worker would otherwise read as a parent that had exited.
            .env(
                tilo_sandbox::confine::PARENT_ENV,
                std::process::id().to_string(),
            )
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        for key in PASSED_ENV {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // The catalog this host verified: the worker lays its views' text out
        // before it confines itself, and has no build directory above it.
        if let Some(catalog) = ui::font_catalog::catalog_root() {
            command.env(ui::font_catalog::OVERRIDE_ENV, catalog);
        }
        let child =
            spawn(command).map_err(|error| format!("the app worker did not start: {error}"))?;
        // Dropped, and so ended, when anything below fails.
        let process = Self {
            id: child.id(),
            child: Mutex::new(child),
            writer: Mutex::new(ours.try_clone().map_err(|error| error.to_string())?),
            waiting: Arc::new(Mutex::new(Some(HashMap::new()))),
            mode,
        };
        let mut reader = ours;
        reader
            .set_read_timeout(Some(START_TIMEOUT))
            .map_err(|error| error.to_string())?;
        let ready: Result<Option<(Slot, Reply)>, _> = FRAMING.read_value(&mut reader);
        match ready {
            Ok(Some((_, Reply::Ready { version }))) if version == PROTOCOL_VERSION => {}
            Ok(Some((_, Reply::Unconfined(reason)))) => {
                return Err(format!(
                    "the app worker could not confine itself, so no app runs: {reason}"
                ));
            }
            Ok(Some(_)) => return Err("the app worker speaks another protocol".into()),
            // It ended without a word: this executable is no app worker when asked to be one.
            Ok(None) => return Err(not_serving()),
            // It is still starting (a machine too busy to start it in time), or its socket broke.
            Err(error) => {
                return Err(format!(
                    "the app worker was not ready within {} s: {error}",
                    START_TIMEOUT.as_secs()
                ));
            }
        }
        reader
            .set_read_timeout(None)
            .map_err(|error| error.to_string())?;
        let waiting = Arc::clone(&process.waiting);
        std::thread::Builder::new()
            .name("app-worker-replies".into())
            .spawn(move || route_replies(reader, &waiting))
            .map_err(|error| format!("no thread to read the app worker's replies on: {error}"))?;
        Ok(process)
    }

    /// Send `request` to the worker's one slot and wait for its reply: a
    /// worker started for one thing.
    ///
    /// # Errors
    ///
    /// The worker died, hung or answered with garbage; it is no longer
    /// usable and the caller drops it.
    pub(crate) fn call(&self, request: &Request) -> Result<Reply, String> {
        self.call_slot(0, self.mode, request, REQUEST_PATIENCE)
            .map_err(|error| match error {
                CallError::Stopped(reason) => reason,
                CallError::Slow => "the app worker took too long to answer".into(),
            })
    }

    /// Send `request` to `slot`, a slot doing `mode`, and wait up to
    /// `patience` for its reply. Requests to other slots are sent and
    /// answered meanwhile.
    fn call_slot(
        &self,
        slot: Slot,
        mode: Mode,
        request: &Request,
        patience: Duration,
    ) -> Result<Reply, CallError> {
        let (sender, answer) = mpsc::channel();
        match self.waiting.lock_recover().as_mut() {
            Some(waiting) => {
                waiting.insert(slot, sender);
            }
            None => return Err(CallError::Stopped(self.ended())),
        }
        if let Err(reason) = self.send(slot, mode, request) {
            self.forget(slot);
            return Err(CallError::Stopped(reason));
        }
        match answer.recv_timeout(patience) {
            Ok(reply) => Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.forget(slot);
                Err(CallError::Slow)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(CallError::Stopped(self.ended())),
        }
    }

    /// Why the worker's replies ended, as its exit tells: a worker the
    /// kernel ended for a call its confinement refuses says so.
    fn ended(&self) -> String {
        let mut child = self.child.lock_recover();
        // The socket closes as the process exits, a moment before it can be
        // waited for.
        for _ in 0..50 {
            match child.try_wait() {
                Ok(Some(status)) => return format!("the app worker stopped ({status})"),
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
        "the app worker stopped".into()
    }

    /// Send `request` to `slot` without waiting for an answer. The views it renders are in the
    /// device's region ([`super::region`]).
    fn send(&self, slot: Slot, mode: Mode, request: &Request) -> Result<(), String> {
        let region = super::region::wire();
        FRAMING
            .write_value(
                &mut *self.writer.lock_recover(),
                &(slot, mode, region, request),
            )
            .map_err(|error| format!("the app worker stopped: {error}"))
    }

    /// No one waits for `slot`'s reply any longer: it is dropped when it comes.
    fn forget(&self, slot: Slot) {
        if let Some(waiting) = self.waiting.lock_recover().as_mut() {
            waiting.remove(&slot);
        }
    }

    /// Whether the worker has stopped: its replies ended.
    fn stopped(&self) -> bool {
        self.waiting.lock_recover().is_none()
    }

    /// The worker's process id.
    pub(crate) const fn id(&self) -> u32 {
        self.id
    }
}

/// Hand each reply the worker sends to the caller waiting for its slot, until
/// the worker stops; then every caller still waiting learns it.
fn route_replies(mut socket: UnixStream, waiting: &Waiting) {
    loop {
        let reply: Result<Option<(Slot, Reply)>, _> = FRAMING.read_value(&mut socket);
        let Ok(Some((slot, reply))) = reply else {
            break;
        };
        let sender = waiting
            .lock_recover()
            .as_mut()
            .and_then(|waiting| waiting.remove(&slot));
        if let Some(sender) = sender {
            let _ = sender.send(reply);
        }
    }
    *waiting.lock_recover() = None;
}

impl Drop for Process {
    fn drop(&mut self) {
        let mut child = self.child.lock_recover();
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The running image: on Linux even if the file on disk was replaced since.
/// An empty path where it cannot be named, which then fails to start.
fn executable() -> std::path::PathBuf {
    if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap_or_default()
    }
}

fn not_serving() -> String {
    "this executable does not serve app workers: call \
     `tilo_agent_session::worker::dispatch_early()` at the top of `main`, \
     or add `tilo_agent_session::app_worker!();` to a test crate"
        .to_owned()
}

/// Start workers from one thread that lives as long as the process.
///
/// A worker asks the kernel to end it when its parent goes away, and on
/// Linux "parent" is the *thread* that forked it: spawning from whichever
/// thread needed a worker would end the worker with that thread.
fn spawn(command: Command) -> std::io::Result<Child> {
    type Job = (Command, mpsc::Sender<std::io::Result<Child>>);
    static SPAWNER: std::sync::OnceLock<Mutex<mpsc::Sender<Job>>> = std::sync::OnceLock::new();
    let gone = || std::io::Error::other("the app worker spawner thread is gone");
    let sender = SPAWNER.get_or_init(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        let started = std::thread::Builder::new()
            .name("app-worker-spawner".into())
            .spawn(move || {
                for (mut command, reply) in queue {
                    let _ = reply.send(command.spawn());
                }
            });
        if let Err(error) = started {
            tracing::error!(%error, "app workers: the spawner thread did not start");
        }
        Mutex::new(jobs)
    });
    let (reply, answer) = mpsc::channel();
    sender
        .lock_recover()
        .send((command, reply))
        .map_err(|_| gone())?;
    answer.recv().map_err(|_| gone())?
}

/// How many workers the pool may hold: `TILO_APP_WORKERS`, or one for each
/// processor up to eight. A worker starts when a slot is first asked for in
/// it and ends once it has held none for a while ([`IDLE_GRACE`]), so the
/// pool's memory follows what is open, never how many workspaces it serves.
#[must_use]
pub fn pool_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("TILO_APP_WORKERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|size| *size > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism().map_or(2, |count| count.get().clamp(2, 8))
            })
    })
}

/// One worker of the pool, started on first use and again after it stopped.
struct Member {
    state: Mutex<MemberState>,
    /// Told when the worker is given a slot while it held none.
    busy: Condvar,
}

#[derive(Default)]
struct MemberState {
    process: Option<Arc<Process>>,
    /// Bumped each time a process starts: a slot leased on an earlier one
    /// was lost with it.
    generation: u64,
    /// The slots open in the running process.
    slots: usize,
}

fn members() -> &'static [Member] {
    static MEMBERS: std::sync::OnceLock<Vec<Member>> = std::sync::OnceLock::new();
    MEMBERS.get_or_init(|| {
        (0..pool_size())
            .map(|_| Member {
                state: Mutex::new(MemberState::default()),
                busy: Condvar::new(),
            })
            .collect()
    })
}

/// The process ids of the pool's workers running now.
#[must_use]
pub fn running() -> Vec<u32> {
    members()
        .iter()
        .filter_map(|member| {
            member
                .state
                .lock_recover()
                .process
                .as_ref()
                .map(|process| process.id())
        })
        .collect()
}

thread_local! {
    /// The pool member this thread's leases go to, when it was given one.
    static AFFINITY: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Send every lease this thread makes from now on to pool member `index`
/// (modulo the pool's size): a thread serving a share of the workspaces
/// keeps their documents in one worker.
pub fn set_affinity(index: usize) {
    AFFINITY.with(|affinity| affinity.set(Some(index)));
}

/// The pool member this thread was given ([`set_affinity`]), for a thread it
/// starts to lease in the same worker.
#[must_use]
pub fn affinity() -> Option<usize> {
    AFFINITY.with(std::cell::Cell::get)
}

/// End `member`'s worker of `generation` if it is still given no slot after
/// [`IDLE_GRACE`].
fn retire_when_idle(member: &'static Member, generation: u64) {
    let started = std::thread::Builder::new()
        .name("app-worker-idle".into())
        .spawn(move || {
            let idle = |state: &mut MemberState| {
                state.slots == 0 && state.generation == generation && state.process.is_some()
            };
            let (mut state, _) = member
                .busy
                .wait_timeout_while(member.state.lock_recover(), IDLE_GRACE, idle)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ended = if idle(&mut state) {
                state.process.take()
            } else {
                None
            };
            drop(state);
            // The process ends outside the lock.
            drop(ended);
        });
    if let Err(error) = started {
        tracing::debug!(%error, "app workers: no thread to end an idle worker on");
    }
}

/// Why a leased slot answered nothing.
#[derive(Debug)]
pub(crate) enum Stopped {
    /// The worker the slot was in stopped since it was last asked: what the
    /// slot held went with it. Nothing of this request was sent.
    Lost,
    /// The worker died or answered garbage on this request, and was stopped.
    Failed(String),
    /// The worker is still working on this request after
    /// [`REQUEST_PATIENCE`]: the slot is let go, and the worker and every
    /// other slot in it go on.
    Slow,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost => formatter.write_str("the app worker holding this was replaced"),
            Self::Failed(reason) => formatter.write_str(reason),
            Self::Slow => {
                formatter.write_str("the app was still working on this after a minute; ask again")
            }
        }
    }
}

/// One slot in a pooled worker: a record, a view or a read held there.
/// Dropping it lets go of what the slot holds.
pub(crate) struct Lease {
    member: &'static Member,
    slot: Slot,
    mode: Mode,
    /// The process the slot was opened in, once it was.
    generation: Option<u64>,
    /// How long a request is waited for.
    patience: Duration,
    /// The last request was given up on while the worker still worked on it.
    slow: bool,
}

impl Lease {
    /// A new slot doing `mode`, in the member this thread was given
    /// ([`set_affinity`]), or the one `key` (the workspace) falls to.
    pub(crate) fn new(mode: Mode, key: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let members = members();
        let index = affinity()
            .unwrap_or_else(|| {
                use std::hash::{Hash as _, Hasher as _};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                key.hash(&mut hasher);
                usize::try_from(hasher.finish()).unwrap_or_default()
            })
            .checked_rem(members.len())
            .unwrap_or_default();
        #[expect(
            clippy::indexing_slicing,
            reason = "the index is taken modulo the pool's size, which is at least one"
        )]
        let member = &members[index];
        Self {
            member,
            slot: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            mode,
            generation: None,
            patience: REQUEST_PATIENCE,
            slow: false,
        }
    }

    /// The live worker the slot is in: started when there is none and the
    /// slot was not opened yet.
    fn process(&mut self) -> Result<(Arc<Process>, u64), Stopped> {
        let mut state = self.member.state.lock_recover();
        if state
            .process
            .as_ref()
            .is_some_and(|process| process.stopped())
        {
            state.process = None;
        }
        if state.process.is_none() {
            if self.generation.is_some() {
                return Err(Stopped::Lost);
            }
            let process = Process::start(Mode::Replica).map_err(Stopped::Failed)?;
            state.process = Some(Arc::new(process));
            state.generation = state.generation.wrapping_add(1);
            state.slots = 0;
        }
        match self.generation {
            None => {
                self.generation = Some(state.generation);
                state.slots = state.slots.saturating_add(1);
                self.member.busy.notify_all();
            }
            Some(generation) if generation != state.generation => return Err(Stopped::Lost),
            Some(_) => {}
        }
        let process = state.process.clone().ok_or(Stopped::Lost)?;
        Ok((process, state.generation))
    }

    /// Send `request` to the slot and wait for its reply, starting the
    /// member's worker if there is none. Requests of other slots in the same
    /// worker are answered meanwhile.
    ///
    /// # Errors
    ///
    /// [`Stopped::Lost`] when what the slot held is gone with an earlier
    /// worker; [`Stopped::Failed`] when this request stopped the worker;
    /// [`Stopped::Slow`] when it was given up on while the worker went on.
    pub(crate) fn call(&mut self, request: &Request) -> Result<Reply, Stopped> {
        self.slow = false;
        // A slot that held nothing yet is asked again of a new worker when
        // the one it asked stopped meanwhile: what stopped it was another
        // slot's, and nothing of this one was lost.
        let mut fresh = self.generation.is_none();
        loop {
            let (process, generation) = self.process()?;
            match process.call_slot(self.slot, self.mode, request, self.patience) {
                Ok(reply) => return Ok(reply),
                Err(CallError::Slow) => {
                    tracing::warn!(slot = self.slot, kind = request.kind(), patience = ?self.patience,
                        "app workers: a request is still running; its slot is let go and the worker goes on");
                    self.slow = true;
                    return Err(Stopped::Slow);
                }
                Err(CallError::Stopped(reason)) => {
                    tracing::warn!(slot = self.slot, generation, %reason,
                        "app workers: a pooled worker stopped; what its slots held is given again");
                    let mut state = self.member.state.lock_recover();
                    let ended = if state.generation == generation {
                        state.process.take()
                    } else {
                        None
                    };
                    drop(state);
                    // The process ends outside the lock.
                    drop(ended);
                    if !fresh {
                        return Err(Stopped::Failed(reason));
                    }
                    fresh = false;
                    self.generation = None;
                }
            }
        }
    }

    /// Let go of the slot. After a request that was given up on, only the
    /// slot, which the worker ends once that request is done; otherwise the
    /// worker it is in, which answered in a way no worker should: every slot
    /// in it is given again from what the supervisor holds.
    pub(crate) fn stop(&mut self) {
        if self.slow {
            self.close();
            return;
        }
        let mut state = self.member.state.lock_recover();
        let stopped = if self.generation == Some(state.generation) {
            state.process.take()
        } else {
            None
        };
        drop(state);
        // The process ends outside the lock.
        drop(stopped);
        self.generation = None;
    }

    /// The process id of the worker the slot is in, while it runs.
    /// Wait `patience` for each request from now on, in place of [`REQUEST_PATIENCE`].
    #[cfg(test)]
    pub(crate) const fn wait_for(&mut self, patience: Duration) {
        self.patience = patience;
    }

    pub(crate) fn worker_id(&self) -> Option<u32> {
        let state = self.member.state.lock_recover();
        state
            .process
            .as_ref()
            .filter(|_| self.generation == Some(state.generation))
            .map(|process| process.id())
    }

    /// Tell the worker to let go of what the slot holds, and end the worker
    /// once it has held no slot for a while.
    fn close(&mut self) {
        let mut state = self.member.state.lock_recover();
        if self.generation.take() != Some(state.generation) {
            return;
        }
        if let Some(process) = state.process.as_ref()
            && process.send(self.slot, self.mode, &Request::Close).is_err()
        {
            state.process = None;
        }
        state.slots = state.slots.saturating_sub(1);
        if state.slots == 0 && state.process.is_some() {
            retire_when_idle(self.member, state.generation);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A pool member of the test's own: the tests running beside it in this
    /// process lease from the shared pool, never from this one.
    fn member() -> &'static Member {
        Box::leak(Box::new(Member {
            state: Mutex::new(MemberState::default()),
            busy: Condvar::new(),
        }))
    }

    fn lease(member: &'static Member) -> Lease {
        let mut lease = Lease::new(Mode::Views, "workspace");
        lease.member = member;
        lease
    }

    fn worker(member: &Member) -> Option<u32> {
        member
            .state
            .lock_recover()
            .process
            .as_ref()
            .map(|process| process.id())
    }

    /// A question any slot answers at once: a title of a document that is
    /// not one.
    fn quick() -> Request {
        Request::Title {
            app_id: "so.tilo.scratchpad".into(),
            doc: vec![1, 2, 3],
        }
    }

    #[test]
    fn a_worker_starts_when_first_asked_and_ends_once_nothing_is_held() {
        let member = member();
        let mut first = lease(member);
        assert_eq!(worker(member), None, "a slot alone starts no worker");
        assert!(matches!(first.call(&quick()), Ok(Reply::Failed(_))));
        let running = worker(member).expect("the first request starts the worker");
        let mut second = lease(member);
        assert!(matches!(second.call(&quick()), Ok(Reply::Failed(_))));
        assert_eq!(worker(member), Some(running), "a second slot shares it");
        drop(first);
        std::thread::sleep(IDLE_GRACE * 3);
        assert_eq!(
            worker(member),
            Some(running),
            "a worker holding a slot stays"
        );
        drop(second);
        let until = Instant::now() + IDLE_GRACE * 20;
        while worker(member).is_some() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(worker(member), None, "a worker holding nothing ends");
        // Asked again, a worker starts again.
        let mut again = lease(member);
        assert!(matches!(again.call(&quick()), Ok(Reply::Failed(_))));
        assert!(worker(member).is_some_and(|started| started != running));
    }

    #[test]
    fn a_slow_request_is_let_go_while_its_worker_answers_every_other_slot() {
        let member = member();
        let mut slow = lease(member);
        assert!(matches!(slow.call(&quick()), Ok(Reply::Failed(_))));
        let running = worker(member).unwrap();
        slow.patience = Duration::from_millis(500);
        let stalled = std::thread::spawn(move || {
            let answer = slow.call(&Request::Stall { millis: 5_000 });
            (slow, answer)
        });
        // While the stalled request goes on, another slot of the same worker is
        // answered at once, again and again.
        std::thread::sleep(Duration::from_millis(100));
        let mut other = lease(member);
        for _ in 0..5 {
            let asked = Instant::now();
            assert!(matches!(other.call(&quick()), Ok(Reply::Failed(_))));
            assert!(
                asked.elapsed() < Duration::from_millis(400),
                "another slot waited {:?} behind a slow one",
                asked.elapsed()
            );
        }
        let (mut slow, answer) = stalled.join().unwrap();
        assert!(matches!(answer, Err(Stopped::Slow)), "{answer:?}");
        // Let go as a document's replica does when its request fails: only
        // the slot goes.
        slow.stop();
        assert!(matches!(other.call(&quick()), Ok(Reply::Failed(_))));
        assert_eq!(worker(member), Some(running), "the worker was not stopped");
        // What it held is held again in a new place in the same worker.
        let mut again = lease(member);
        assert!(matches!(again.call(&quick()), Ok(Reply::Failed(_))));
        assert_eq!(again.worker_id(), Some(running));
    }

    #[test]
    fn a_worker_that_died_is_noticed_before_a_slot_is_asked_again() {
        let member = member();
        let mut held = lease(member);
        assert!(matches!(held.call(&quick()), Ok(Reply::Failed(_))));
        let running = worker(member).unwrap();
        let killed = Command::new("kill")
            .args(["-9", &running.to_string()])
            .status_locked()
            .unwrap();
        assert!(killed.success());
        let until = Instant::now() + Duration::from_secs(10);
        while member
            .state
            .lock_recover()
            .process
            .as_ref()
            .is_some_and(|process| !process.stopped())
            && Instant::now() < until
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        // The slot's worker is gone: what it held is to be given again,
        // without a request sent to the dead one.
        assert!(matches!(held.call(&quick()), Err(Stopped::Lost)));
        let mut fresh = lease(member);
        assert!(matches!(fresh.call(&quick()), Ok(Reply::Failed(_))));
        assert!(worker(member).is_some_and(|started| started != running));
    }
}
