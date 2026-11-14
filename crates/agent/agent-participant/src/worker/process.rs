//! The supervisor's handle on app worker processes: one process
//! ([`Process`]), and the pool of long-lived processes records, views and
//! reads are held in, each in a slot of its own ([`Lease`]).

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use super::proto::{FRAMING, Mode, PROTOCOL_VERSION, Reply, Request, Slot};
use super::{ARG, ENV};
use tilo_lock::MutexRecover as _;

/// How long a worker may take to start, load what it needs and confine
/// itself.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one request may run before the worker is presumed hung and
/// killed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

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

/// One running worker. Dropping it ends the process.
pub(crate) struct Process {
    child: Child,
    socket: UnixStream,
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
            .args([ARG, "--nocapture", "--test-threads=1", "--quiet"])
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
        let mut process = Self {
            child,
            socket: ours,
            mode,
        };
        process
            .socket
            .set_read_timeout(Some(START_TIMEOUT))
            .map_err(|error| error.to_string())?;
        match process.receive() {
            Ok(Reply::Ready { version }) if version == PROTOCOL_VERSION => {}
            Ok(Reply::Unconfined(reason)) => {
                return Err(format!(
                    "the app worker could not confine itself, so no app runs: {reason}"
                ));
            }
            Ok(_) => return Err("the app worker speaks another protocol".into()),
            Err(_) => return Err(not_serving()),
        }
        process
            .socket
            .set_read_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|error| error.to_string())?;
        Ok(process)
    }

    /// Send `request` to the worker's one slot and wait for its reply: a
    /// worker started for one thing.
    ///
    /// # Errors
    ///
    /// The worker died, hung or answered with garbage; it is no longer
    /// usable and the caller drops it.
    pub(crate) fn call(&mut self, request: &Request) -> Result<Reply, String> {
        self.call_slot(0, self.mode, request)
    }

    /// Send `request` to `slot`, a slot doing `mode`, and wait for its reply.
    ///
    /// # Errors
    ///
    /// As [`Self::call`].
    fn call_slot(&mut self, slot: Slot, mode: Mode, request: &Request) -> Result<Reply, String> {
        FRAMING
            .write_value(&mut self.socket, &(slot, mode, request))
            .map_err(|error| format!("the app worker stopped: {error}"))?;
        self.receive()
    }

    fn receive(&mut self) -> Result<Reply, String> {
        match FRAMING.read_value(&mut self.socket) {
            Ok(Some(reply)) => Ok(reply),
            Ok(None) => Err("the app worker stopped".into()),
            Err(error) => Err(format!("the app worker stopped: {error}")),
        }
    }

    /// The worker's process id.
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
     `tilo_agent_participant::worker::dispatch_early()` at the top of `main`, \
     or add `tilo_agent_participant::app_worker!();` to a test crate"
        .to_owned()
}

/// Start workers from one thread that lives as long as the process.
///
/// A worker asks the kernel to end it when its parent goes away, and on
/// Linux "parent" is the *thread* that forked it: spawning from whichever
/// thread needed a worker would end the worker with that thread.
fn spawn(command: Command) -> std::io::Result<Child> {
    type Job = (Command, std::sync::mpsc::Sender<std::io::Result<Child>>);
    static SPAWNER: std::sync::OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> =
        std::sync::OnceLock::new();
    let gone = || std::io::Error::other("the app worker spawner thread is gone");
    let sender = SPAWNER.get_or_init(|| {
        let (jobs, queue) = std::sync::mpsc::channel::<Job>();
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
    let (reply, answer) = std::sync::mpsc::channel();
    sender
        .lock_recover()
        .send((command, reply))
        .map_err(|_| gone())?;
    answer.recv().map_err(|_| gone())?
}

/// How many long-lived workers the pool keeps: `TILO_APP_WORKERS`, or one
/// for each processor up to eight. Each costs its process once, whatever it
/// holds, so the pool's memory follows this number and what the records in
/// it hold, never how many workspaces they belong to.
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

/// One long-lived worker of the pool, started on first use and again after
/// it stopped.
struct Member {
    state: Mutex<MemberState>,
}

#[derive(Default)]
struct MemberState {
    process: Option<Process>,
    /// Bumped each time a process starts: a slot leased on an earlier one
    /// was lost with it.
    generation: u64,
}

fn members() -> &'static [Member] {
    static MEMBERS: std::sync::OnceLock<Vec<Member>> = std::sync::OnceLock::new();
    MEMBERS.get_or_init(|| {
        (0..pool_size())
            .map(|_| Member {
                state: Mutex::new(MemberState::default()),
            })
            .collect()
    })
}

thread_local! {
    /// The pool member this thread's leases go to, when it was given one.
    static AFFINITY: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Send every lease this thread makes from now on to pool member `index`
/// (modulo the pool's size): a thread serving a share of the workspaces
/// keeps their records in a worker no other such thread waits for.
pub fn set_affinity(index: usize) {
    AFFINITY.with(|affinity| affinity.set(Some(index)));
}

/// Start every worker of the pool now, each on a thread of its own, so no
/// request waits for one to start. Returns at once.
pub fn prestart() {
    for (index, member) in members().iter().enumerate() {
        let started = std::thread::Builder::new()
            .name(format!("app-worker-start-{index}"))
            .spawn(move || {
                let mut state = member.state.lock_recover();
                if state.process.is_some() {
                    return;
                }
                match Process::start(Mode::Replica) {
                    Ok(process) => {
                        state.process = Some(process);
                        state.generation = state.generation.wrapping_add(1);
                        drop(state);
                    }
                    Err(reason) => {
                        drop(state);
                        tracing::warn!(%reason, "app workers: a pooled worker did not start");
                    }
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "app workers: no thread to start a pooled worker on");
        }
    }
}

/// Why a leased slot answered nothing.
#[derive(Debug)]
pub(crate) enum Stopped {
    /// The worker the slot was in stopped since it was last asked, through
    /// another slot's request: what the slot held went with it. Nothing of
    /// this request was sent.
    Lost,
    /// The worker died, hung or answered garbage on this request, and was
    /// stopped.
    Failed(String),
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lost => formatter.write_str("the app worker holding this was replaced"),
            Self::Failed(reason) => formatter.write_str(reason),
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
}

impl Lease {
    /// A new slot doing `mode`, in the member this thread was given
    /// ([`set_affinity`]), or the one `key` (the workspace) falls to.
    pub(crate) fn new(mode: Mode, key: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let members = members();
        let index = AFFINITY
            .with(std::cell::Cell::get)
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
        }
    }

    /// Send `request` to the slot and wait for its reply, starting the
    /// member's worker if none runs.
    ///
    /// # Errors
    ///
    /// [`Stopped::Lost`] when what the slot held is gone with an earlier
    /// worker; [`Stopped::Failed`] when this request stopped the worker.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the worker is held for the whole request: another slot's request waits for it"
    )]
    pub(crate) fn call(&mut self, request: &Request) -> Result<Reply, Stopped> {
        let mut state = self.member.state.lock_recover();
        if state.process.is_none() {
            if self.generation.is_some() {
                return Err(Stopped::Lost);
            }
            let process = Process::start(Mode::Replica).map_err(Stopped::Failed)?;
            state.process = Some(process);
            state.generation = state.generation.wrapping_add(1);
        }
        match self.generation {
            None => self.generation = Some(state.generation),
            Some(generation) if generation != state.generation => return Err(Stopped::Lost),
            Some(_) => {}
        }
        let generation = state.generation;
        let Some(process) = state.process.as_mut() else {
            return Err(Stopped::Lost);
        };
        match process.call_slot(self.slot, self.mode, request) {
            Ok(reply) => Ok(reply),
            Err(reason) => {
                tracing::warn!(slot = self.slot, generation, %reason,
                    "app workers: a pooled worker stopped; what its slots held is given again");
                state.process = None;
                Err(Stopped::Failed(reason))
            }
        }
    }

    /// Stop the worker the slot is in: it answered in a way no worker
    /// should, and every slot in it is given again from what the supervisor
    /// holds.
    pub(crate) fn stop(&mut self) {
        let mut state = self.member.state.lock_recover();
        let stopped = (self.generation == Some(state.generation))
            .then(|| state.process.take())
            .flatten();
        drop(state);
        // The process ends outside the lock.
        drop(stopped);
        self.generation = None;
    }

    /// The process id of the worker the slot is in, while it runs.
    pub(crate) fn worker_id(&self) -> Option<u32> {
        let state = self.member.state.lock_recover();
        state
            .process
            .as_ref()
            .filter(|_| self.generation == Some(state.generation))
            .map(Process::id)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.member.state.lock_recover();
        if self.generation != Some(state.generation) {
            return;
        }
        if let Some(process) = state.process.as_mut()
            && process
                .call_slot(self.slot, self.mode, &Request::Close)
                .is_err()
        {
            state.process = None;
        }
    }
}
