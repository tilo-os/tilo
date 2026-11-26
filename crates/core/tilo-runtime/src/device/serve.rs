//! This computer's one daemon as it serves the device: the `tilo mcp` connections every agent
//! of the login makes to it, the workspace commands of the login, and the person's workspaces'
//! folders.
//!
//! Every `tilo mcp` reaches this daemon over a socket in the device's directory and relays its
//! agent's stdio to it; the daemon answers each connection from its own workspace, as a run of
//! the agent the login was made for ([`hold`]). One process keeps the device: the host keeps one
//! session per device, and two processes entering as it would cut each other off.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_lock::MutexRecover as _;
use tilo_socket::Stream;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use tilo_api::daemon::{DaemonPlace, PEER_STATUS};

use super::presence::Presence;
use super::services::{Daemon, RuntimeActionProvider};

/// How long the person's part waits once this computer logged in with a ticket before it stops:
/// the login's answer reaches the program that asked for it first.
const ANSWERED: Duration = Duration::from_secs(2);
/// How long a holder starting waits for one that is leaving to let go.
const TAKING_OVER: Duration = Duration::from_secs(20);
/// How long a holder that has the lock waits for the last one's stores to let go: the lock is
/// let go before the process ends, and the databases it still holds are not.
const OPENING: Duration = Duration::from_secs(30);
/// How often it looks whether those stores let go.
const OPENING_POLL: Duration = Duration::from_millis(100);
/// How long a holder leaving waits for its stores to close and the device's endpoint to say it
/// is going, before it lets the lock go: the next holder opens the store as soon as this
/// process lets it go rather than when this process ends.
const RELEASING: Duration = Duration::from_secs(3);
/// How often a daemon starting looks whether the one leaving let go of the lock.
const POLL: Duration = Duration::from_millis(50);
/// How long a holder's process waits for its runtime's threads once it ended. A step that walks
/// a whole folder tree runs on its thread to its end and cannot be interrupted; a stop leaves it
/// behind rather than hanging until it finishes.
const SHUTDOWN: Duration = Duration::from_secs(2);
/// How long a stop waits for one folder to unmount: one whose mount thread is inside a step on
/// the tree is left behind, and its mount cleared when the next holder shows it.
pub(crate) const UNMOUNTING: Duration = Duration::from_secs(3);
/// How long a stop waits for the folders to unmount and each workspace's tree to be kept on
/// disk, all of them together: a tree's changes made here are saved, and a step still running
/// is left behind rather than waited for.
const STOPPING: Duration = Duration::from_secs(16);

/// The most a holder's log keeps: a holder that takes the device empties a log grown past it.
const LOG_KEPT: u64 = 8 << 20;

/// Empty this holder's log once it grew past [`LOG_KEPT`]: it is added to by every holder
/// started ([`start_holder`]), and the one that holds the device is the one that writes most.
fn trim_log() {
    let Ok(log) = stderr_file() else {
        return;
    };
    if log
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() > LOG_KEPT)
    {
        let _ = log.set_len(0);
    }
}

/// What this process's error output is written to, as a file of its own.
fn stderr_file() -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    use std::os::fd::AsFd as _;
    #[cfg(windows)]
    use std::os::windows::io::AsHandle as _;
    let stderr = std::io::stderr();
    #[cfg(unix)]
    let owned = stderr.as_fd().try_clone_to_owned()?;
    #[cfg(windows)]
    let owned = stderr.as_handle().try_clone_to_owned()?;
    Ok(std::fs::File::from(owned))
}

/// The threads a bridge's runtime works on.
const BRIDGE_THREADS: usize = 2;

/// The runtime a bridge between agents and their workspace runs on: a worker per core would hold
/// a thread and its heap arena per core of the computer, hundreds of them on a large one, for
/// agents that each ask one thing at a time.
fn bridge_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(BRIDGE_THREADS)
        .enable_all()
        .build()
}

/// What the device's socket serves: the agents' endpoint, and the daemon's own requests.
pub(crate) struct Socket {
    presence: Arc<Presence>,
    agents: crate::daemon::Endpoint<RuntimeActionProvider>,
}

impl Socket {
    /// `daemon`'s socket.
    ///
    /// # Errors
    /// The daemon's API cannot be built.
    pub(crate) fn new(daemon: &Arc<Daemon>) -> std::io::Result<Arc<Self>> {
        let presence = Presence::new(daemon)?;
        Ok(Arc::new(Self {
            agents: crate::daemon::agents_endpoint(daemon, Arc::clone(&presence)),
            presence,
        }))
    }
}

/// What takes an agent's connection, with what was read of it already.
type Taker = Arc<dyn Fn(Stream, Vec<u8>) + Send + Sync>;

/// How long an agent's connection waits for the person's part of the daemon to take it.
const TAKEN_WITHIN: Duration = Duration::from_secs(30);

/// Where the daemon's native endpoint hands agents' connections
/// ([`tilo_api::daemon::AGENT_LINE`]): to the person's part of the daemon while it runs.
#[derive(Clone, Default)]
pub(crate) struct Agents(Arc<(std::sync::Mutex<Option<Taker>>, std::sync::Condvar)>);

impl Agents {
    /// Hand `stream`, of which `read` was read already, to the person's part, waiting for it to
    /// start; one that none takes in time is told why and closed.
    pub(crate) fn take(&self, stream: Stream, read: Vec<u8>) {
        let (slot, started) = &*self.0;
        let deadline = Instant::now()
            .checked_add(TAKEN_WITHIN)
            .unwrap_or_else(Instant::now);
        let mut taking = slot.lock_recover();
        let taker = loop {
            if let Some(taker) = taking.as_ref() {
                break Arc::clone(taker);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(taking);
                refuse(
                    &stream,
                    "this computer's Tilo has not started its person's part yet: run `tilo \
                     status` to see how it stands, then try again",
                );
                return;
            }
            taking = started
                .wait_timeout(taking, left)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(taking, _)| taking);
        };
        drop(taking);
        taker(stream, read);
    }

    /// Hand every agent's connection to `taker` from now on.
    fn serve(&self, taker: Taker) {
        *self.0.0.lock_recover() = Some(taker);
        self.0.1.notify_all();
    }

    /// Hand no more connections to the person's part that ends.
    fn withdraw(&self) {
        *self.0.0.lock_recover() = None;
    }

    /// An agent's connection, handed over as the native endpoint hands one: waits for the
    /// person's part to take it.
    #[cfg(test)]
    fn connect(&self) -> Stream {
        let (agent, served) = tilo_socket::pair().unwrap();
        self.take(served, Vec::new());
        agent
    }
}

/// Tell the agent at `stream` why it is not served, as a JSON-RPC error that names no request.
pub(crate) fn refuse(stream: &Stream, why: &str) {
    use std::io::Write as _;
    let refusal = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32000, "message": why}});
    let mut writing = stream;
    let _ = writing.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = writeln!(writing, "{refusal}");
}

/// Take the lock that makes this process the device's one daemon, as [`take_lock`] does.
///
/// # Errors
/// Another daemon keeps the device, or the lock cannot be opened.
pub(crate) fn lock_device(place: &DaemonPlace) -> std::io::Result<advisory_lock::FileLock> {
    take_lock(place, TAKING_OVER)
        .map_err(std::io::Error::other)?
        .ok_or_else(|| {
            let owner = advisory_lock::owner_pid(place.lock())
                .map(|pid| format!(" by process {pid}"))
                .unwrap_or_default();
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("this computer's Tilo device is in use{owner}: another daemon keeps it"),
            )
        })
}

/// This computer as the person's peer: `daemon`'s device served to every connection at its
/// place until `stop` is told or this computer logs in with a ticket, while it has the device's
/// lock ([`lock_device`]), and the person's workspaces' files shown below `home`
/// ([`super::peer`]).
pub(crate) fn hold(daemon: &Arc<Daemon>, home: PathBuf, stop: &Stop) -> Result<super::Ended> {
    say_panics();
    trim_log();
    let place = daemon.place();
    let runtime = bridge_runtime()?;
    // A holder that stops answering for seconds says what it was doing, with every thread's
    // stack, long before [`WEDGED`] ends it: a person's `ls` that waits has a reason in the log.
    tilo_rt::stall::watch(WEDGED.min(STALL));
    tilo_rt::stall::heartbeat(runtime.handle());
    runtime.spawn(beating());
    let watching = watch_workers();
    let served = runtime.block_on(serve_device(place, daemon, home, stop));
    // A step that walks a whole folder tree — a merge of a tree record, a save of one — runs on
    // its thread to its end and cannot be interrupted. The process ends now instead of waiting
    // for it: the socket is gone, the folders were unmounted (or given up on), and the trees
    // were kept, above.
    ending_step(None, "the person's part's runtime", SHUTDOWN, move || {
        runtime.shutdown_timeout(SHUTDOWN);
    });
    drop(watching);
    // The next holder opens the stores as soon as this process lets go of the lock, which is
    // before this process ends: let go of them first, or it finds them still open. The next
    // part in this process opens them too, and finds them held while this peer is.
    let peer = PEER.lock_recover().take();
    if let Some(peer) = peer {
        ending_step(None, "the workspaces' files", RELEASING, move || {
            let Ok(releasing) = bridge_runtime() else {
                tracing::error!(
                    "the workspaces' files cannot be closed: no runtime to close them on"
                );
                return;
            };
            // The timer is made inside the runtime: one made outside it has no clock to tick on.
            if releasing
                .block_on(async { tokio::time::timeout(RELEASING, peer.shutdown(RELEASING)).await })
                .is_err()
            {
                tracing::warn!("the workspaces' files did not close in time");
            }
            releasing.shutdown_timeout(SHUTDOWN);
        });
    }
    *NOT_IN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    PART_ENDED.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Err(error) = &served {
        tracing::error!(%error, "this holder ends: it could not serve");
    }
    served
}

/// Run `step`, a step of the person's part ending, on a thread of its own, and wait for it at
/// most `within` and a moment: one that takes longer is left behind and named in the log, so
/// the part that starts next is never held by it, and a step that hangs says which it is.
pub(crate) fn ending_step(
    until: Option<&Stop>,
    what: &'static str,
    within: Duration,
    step: impl FnOnce() + Send + 'static,
) {
    let started = Instant::now();
    let (done, finished) = std::sync::mpsc::sync_channel::<()>(1);
    let spawned = std::thread::Builder::new()
        .name("tilo-part-ends".to_owned())
        .spawn(move || {
            let _watched = tilo_rt::stall::doing(format!("the person's part ending: {what}"));
            step();
            let _ = done.send(());
        });
    if let Err(error) = spawned {
        tracing::error!(%error, step = what, "a step of the person's part ending could not run");
        return;
    }
    let bound = within.saturating_add(ENDING_SLACK);
    let deadline = Instant::now()
        .checked_add(bound)
        .unwrap_or_else(Instant::now);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            tracing::error!(
                step = what,
                waited_ms = bound.as_millis(),
                "a step of the person's part ending did not finish in time; it is left behind"
            );
            return;
        }
        match finished.recv_timeout(left.min(ENDING_POLL)) {
            Ok(()) => {
                tracing::debug!(
                    step = what,
                    took_ms = started.elapsed().as_millis(),
                    "a step of the person's part ending finished"
                );
                return;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // A stop told while this closes is not waited on when the caller says so: the daemon
        // ends, and this step is left behind as it would be past its bound.
        if until.is_some_and(Stop::told) {
            tracing::info!(
                step = what,
                "a step of the person's part ending stops with the daemon"
            );
            return;
        }
    }
}

/// How long a part that ends waits for the calls it serves to be answered before it ends: a
/// call that named the server it starts again through is a person's, and its answer is theirs.
const ANSWERING: Duration = Duration::from_secs(10);

/// How often a step of the person's part ending looks whether the daemon was told to stop while
/// it waits for the step.
const ENDING_POLL: Duration = Duration::from_millis(100);

/// What a step of the person's part ending is given past its own bound before it is left behind.
const ENDING_SLACK: Duration = Duration::from_secs(1);

/// How long a daemon between its person's parts takes to end when it is told to stop: there is
/// nothing to unmount or keep there, and the start that follows may wait on something that
/// never answers.
const BETWEEN_PARTS: Duration = Duration::from_secs(2);

/// What tells a daemon running in this process to stop ([`crate::daemon::Running::stop`]): heard
/// by its holder's loop, and by its wait for a person to claim the device.
#[derive(Clone, Default)]
pub(crate) struct Stop(Arc<StopState>);

#[derive(Default)]
struct StopState {
    /// Whether the daemon was told to stop, and by which signal when one told it.
    told: std::sync::Mutex<(bool, Option<&'static str>)>,
    waiting: std::sync::Condvar,
    heard: tokio::sync::Notify,
    /// Whether a person's part serves in this process now, or the daemon is between parts.
    serving: std::sync::atomic::AtomicBool,
    /// What the daemon is doing now, for a stop that is told to say what it waited on.
    step: std::sync::Mutex<Option<String>>,
}

impl Stop {
    /// Tell the daemon to stop.
    pub(crate) fn tell(&self) {
        self.0.told.lock_recover().0 = true;
        self.0.waiting.notify_all();
        self.0.heard.notify_waiters();
    }

    /// Tell the daemon to stop, as the signal `signal` told it.
    fn told_by(&self, signal: &'static str) {
        self.0.told.lock_recover().1 = Some(signal);
        self.tell();
    }

    /// Wait up to `within` for the daemon to be told to stop: whether it was.
    pub(crate) fn wait(&self, within: Duration) -> bool {
        let told = self.0.told.lock_recover();
        self.0
            .waiting
            .wait_timeout_while(told, within, |told| !told.0)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .0
    }

    /// Whether the daemon was told to stop.
    pub(crate) fn told(&self) -> bool {
        self.0.told.lock_recover().0
    }

    /// The signal that told the daemon to stop, while one did.
    fn signal(&self) -> Option<&'static str> {
        self.0.told.lock_recover().1
    }

    /// Say whether a person's part serves now: one that does may have folders to unmount and
    /// trees to keep, where between parts there is nothing to let go of.
    pub(crate) fn serving(&self, serving: bool) {
        self.0
            .serving
            .store(serving, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a person's part serves now.
    fn part_serves(&self) -> bool {
        self.0.serving.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Say what the daemon is doing now, for a stop that is told to say what it waited on.
    pub(crate) fn waiting_on(&self, what: impl Into<String>) {
        *self.0.step.lock_recover() = Some(what.into());
    }

    /// What the daemon was doing when a stop was told.
    fn waited_on(&self) -> String {
        self.0
            .step
            .lock_recover()
            .clone()
            .unwrap_or_else(|| "its next step".to_owned())
    }

    /// Return once the daemon is told to stop.
    async fn heard(&self) {
        loop {
            let heard = self.0.heard.notified();
            if self.told() {
                return;
            }
            heard.await;
        }
    }
}

/// Say every panic in the holder's log at error level before it unwinds or aborts: a holder
/// that ends never does so without its log saying why, and one task's panic (which ends that
/// task alone) is said too.
fn say_panics() {
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let thread = std::thread::current();
        tracing::error!(
            thread = thread.name().unwrap_or("unnamed"),
            %panic,
            "a step of this holder panicked; if it was the holder's own, this holder ends"
        );
        earlier(panic);
    }));
}

/// The lock that makes this process the device's one daemon, once one that is leaving (the
/// service restarting it) let go of it within `taking_over`; `None` when another keeps it still.
/// Only the service starts this computer's daemon, so a second one is never asked to make way.
fn take_lock(
    place: &DaemonPlace,
    taking_over: Duration,
) -> Result<Option<advisory_lock::FileLock>> {
    use advisory_lock::{FileLock, TryLockError};
    let path = place.lock();
    let deadline = Instant::now()
        .checked_add(taking_over)
        .unwrap_or_else(Instant::now);
    loop {
        // A lock that is not this user's own private file (a link to another file, one others
        // may write) is refused rather than taken: whoever holds it serves the person.
        // Its error names the file and why it is refused.
        let file = crate::host::private_file(path)?;
        match FileLock::try_exclusive(file) {
            Ok(lock) => {
                // Who holds it, for a daemon starting beside this one to name.
                let _ = lock.resource_owner();
                return Ok(Some(lock));
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("open {}", path.display()));
            }
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                lock = %path.display(),
                "another daemon of this device keeps it; this one ends"
            );
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// Listen at `place` and serve each connection as `daemon`, while the lock is held, until `stop`
/// is told, the daemon is told to stop by a signal, or this computer logs in with a ticket.
async fn serve_device(
    place: &DaemonPlace,
    daemon: &Arc<Daemon>,
    home: PathBuf,
    stop: &Stop,
) -> Result<super::Ended> {
    let socket = Socket::new(daemon)?;
    // Agents' connections reach the native endpoint, which hands them here from now on.
    daemon.agents().serve(taking(Arc::clone(&socket)));
    super::serving(Some(Arc::clone(&socket.presence)));
    // A server keeps the workspaces placed on it and shows none of them at a folder: nobody
    // works in its home, and its service may not mount anything.
    if super::keep::config().is_none() {
        tokio::spawn(keep_peer(Arc::clone(daemon), home, place.device_dir()));
    }
    // A computer that logs in with a ticket while its daemon runs is another device now, perhaps
    // of another person: the person's part ends, and starts again as that device.
    let mut logged_in = daemon.logged_in();
    // A server the person's own workspace names after this part started (one they set up, or
    // the first the copy took in) is entered through at once: the part starts again through it.
    let home_source = daemon.home_source();
    // Why this daemon ends, said in its log at error level whatever it is: one that ended
    // without its log saying why left the person's folders unmounted with nothing to read.
    let (why, held) = tokio::select! {
        Ok(device) = async { logged_in.wait_for(Option::is_some).await.map(|device| device.clone()) } => {
            let device = device.map_or(super::Ended::Stopped, super::Ended::LoggedIn);
            // The login's answer is the asking program's before this part stops.
            tokio::time::sleep(ANSWERED).await;
            ("this computer logged in with a ticket; it starts again as that device".to_owned(), device)
        },
        device = home_source.moved() => (
            "its person's own workspace names a server it does not enter through yet; it starts \
             again through it".to_owned(),
            super::Ended::LoggedIn(Box::new(device)),
        ),
        () = stop.heard() => (
            stop.signal().map_or_else(
                || "it was told to stop".to_owned(),
                |signal| format!("it was told to stop ({signal})"),
            ),
            super::Ended::Stopped,
        ),
    };
    tracing::error!("this daemon's person's part ends: {why}");
    // The calls this part serves are answered before it ends: the one that named the server it
    // starts again through, or placed a workspace on one, hears its answer rather than being
    // cut by the part ending under it. A stop is not waited on: the daemon ends, and its calls
    // with it.
    tokio::select! {
        answered = daemon.connections().answered(ANSWERING) => {
            if !answered {
                tracing::warn!(
                    waited_ms = ANSWERING.as_millis(),
                    "a call this part served did not answer before this part ends"
                );
            }
        }
        () = stop.heard() => {}
    }
    // No agent's connection is handed here from now on: one that comes waits for the person's
    // part that starts next, or is told why it is not served.
    daemon.agents().withdraw();
    super::serving(None);
    if let Some(peer) = peer() {
        // Unmounting and keeping the trees wait for the mounts' and trees' own steps, which
        // cannot be interrupted; both are bounded, so a stop ends inside the unit's timeout.
        peer.stop(STOPPING).await;
    }
    socket.presence.close().await;
    Ok(held)
}

/// What takes each agent's connection the native endpoint hands over: its first request is read
/// on a thread of its own, so a person's `tilo status` is answered there whether or not the
/// runtime's workers are free.
fn taking(socket: Arc<Socket>) -> Taker {
    let runtime = tokio::runtime::Handle::current();
    Arc::new(move |stream, read| {
        let (socket, runtime) = (Arc::clone(&socket), runtime.clone());
        let started = std::thread::Builder::new()
            .name("tilo-connection".to_owned())
            .spawn(move || first_request(stream, read, &socket, &runtime));
        if let Err(error) = started {
            tracing::warn!(%error, "a connection to this daemon could not be served");
        }
    })
}

/// Serve `stream`, a connection [`taking`] took with `read` read of it already: its first request is read here, and one
/// asking how the peer stands ([`PEER_STATUS`]) is answered here; any other is an agent's, served by `socket`'s agents'
/// endpoint from there on.
fn first_request(stream: Stream, read: Vec<u8>, socket: &Socket, runtime: &tokio::runtime::Handle) {
    use std::io::{BufRead as _, Read as _, Write as _};
    let Ok(reading) = stream.try_clone() else {
        return;
    };
    let mut reader = std::io::BufReader::new(std::io::Cursor::new(read).chain(reading));
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return;
    }
    let request = serde_json::from_str::<Value>(&first).ok();
    let method = request
        .as_ref()
        .and_then(|request| request.get("method")?.as_str());
    let result = match method {
        Some(PEER_STATUS) => Some(status_answer(&socket.presence, runtime)),
        _ => None,
    };
    if let Some(result) = result {
        let id = request
            .as_ref()
            .and_then(|request| request.get("id").cloned())
            .unwrap_or(Value::Null);
        let mut writing = &stream;
        let _ = writeln!(
            writing,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        );
        let _ = writing.flush();
        return;
    }
    // What the thread read past the first line is the connection's too.
    let mut read = first.into_bytes();
    read.extend_from_slice(reader.buffer());
    drop(reader);
    let _handed = socket.agents.serve(stream, read, None, false);
}

/// How long `tilo status` waits for the runtime to say how the peer stands before it is answered
/// with what the peer last said.
const STATUS_FRESH: Duration = Duration::from_secs(2);

/// The last answer to `tilo status` and when it was made.
static LAST_STATUS: std::sync::Mutex<Option<(Instant, Value)>> = std::sync::Mutex::new(None);

/// How the peer stands: asked of the runtime and waited for at most [`STATUS_FRESH`]. When the
/// runtime's workers do not answer in that time, what the peer last said, with how long ago
/// (`answered_ago_s`), so `tilo status` answers whatever the peer is busy with and says it is.
fn status_answer(presence: &Arc<Presence>, runtime: &tokio::runtime::Handle) -> Value {
    let (told, answered) = std::sync::mpsc::sync_channel(1);
    let asked = Arc::clone(presence);
    runtime.spawn(async move {
        let _ = told.send(super::peer_status(&asked).await);
    });
    if let Ok(status) = answered.recv_timeout(STATUS_FRESH) {
        *LAST_STATUS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((Instant::now(), status.clone()));
        return status;
    }
    let last = LAST_STATUS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let (ago, mut status) = match last {
        Some((at, status)) => (at.elapsed(), status),
        None => (
            WORKERS.since_beat(),
            json!({"pid": std::process::id(), "session": "starting", "mounted": [], "workspaces": [], "failed": []}),
        ),
    };
    if let Some(status) = status.as_object_mut() {
        status.insert("answered_ago_s".to_owned(), json!(ago.as_secs()));
        status.insert("restarts_after_s".to_owned(), json!(WEDGED.as_secs()));
    }
    status
}

/// How long the runtime's workers may go without a step of their own before the holder ends so
/// that a fresh one takes its place: everything that walks a whole tree is done off them, so a
/// worker that gives no sign for this long is stuck, and nothing in the process frees it.
const WEDGED: Duration = Duration::from_secs(120);

/// How long one piece of the holder's work may take, or its workers stay quiet, before it says
/// what it was doing with every thread's stack: a wait a person would notice, reported while it
/// lasts rather than after [`WEDGED`] ends it.
const STALL: Duration = Duration::from_secs(5);

/// When the runtime's workers last took a step: [`beating`] notes it every second.
struct Beat {
    started: std::sync::OnceLock<Instant>,
    at_ms: std::sync::atomic::AtomicU64,
}

static WORKERS: Beat = Beat {
    started: std::sync::OnceLock::new(),
    at_ms: std::sync::atomic::AtomicU64::new(0),
};

impl Beat {
    fn note(&self) {
        let started = self.started.get_or_init(Instant::now);
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.at_ms.store(ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// How long since the workers last took a step.
    fn since_beat(&self) -> Duration {
        let Some(started) = self.started.get() else {
            return Duration::ZERO;
        };
        let at = Duration::from_millis(self.at_ms.load(std::sync::atomic::Ordering::Relaxed));
        started.elapsed().saturating_sub(at)
    }
}

/// Note every second that the runtime's workers take steps, for as long as the runtime lasts.
async fn beating() {
    loop {
        WORKERS.note();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// End this holder once the runtime's workers took no step for [`WEDGED`], saying so in its log:
/// its service starts a fresh one, and a command that needs one starts it. Its folders' changes
/// are on disk, as a kill would leave them, and the next holder sends them. Watched until the
/// value this returns is dropped, as the holder ends.
#[expect(
    clippy::exit,
    reason = "a holder whose workers are stuck cannot end through them; its folders' changes are \
              on disk, as a kill would leave them"
)]
fn watch_workers() -> Watching {
    WORKERS.note();
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = Arc::clone(&stopped);
    let watching = std::thread::Builder::new()
        .name("tilo-watch".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let quiet = WORKERS.since_beat();
                if quiet >= WEDGED {
                    tracing::error!(
                        quiet_s = quiet.as_secs(),
                        "this holder ends: its workers took no step for {}s, so nothing it does \
                         can finish; a fresh holder takes its place",
                        quiet.as_secs()
                    );
                    std::process::exit(1);
                }
            }
        });
    if let Err(error) = watching {
        tracing::warn!(%error, "this holder's workers are not watched");
    }
    Watching(stopped)
}

/// The watch [`watch_workers`] keeps, until this is dropped.
struct Watching(Arc<std::sync::atomic::AtomicBool>);

impl Drop for Watching {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// How long a daemon told to stop takes at most to end: every wait of its stop together (the
/// folders and trees, the session closing, the runtime's threads, the stores closing). Less than
/// the user service gives a stop, so the service never has to kill it.
pub(crate) const STOPPED_WITHIN: Duration = STOPPING
    .saturating_add(super::presence::CLOSING)
    .saturating_add(SHUTDOWN)
    .saturating_add(RELEASING)
    .saturating_add(SHUTDOWN);

/// What ends this daemon from outside: the user service stopping it, or a person's Ctrl-C,
/// heard from its first moment and through its whole life.
///
/// The signals are waited for on a thread of their own, with a runtime of its own: the daemon's
/// two workers may both be inside a step that walks a whole folder tree, and a stop that waited
/// for a free worker was heard only once that step ended. The stop itself runs on those workers
/// (the folders unmount, the trees are kept, the session closes), so a daemon whose workers do
/// not take it up ends from this thread once `within` went by: a signal always ends the daemon,
/// and its folders' changes are on disk, as a kill would leave them.
///
/// It is installed before the daemon binds anything, so a computer started before its person
/// exists hears a stop too: until then it only waited for one, and a signal a caller had ignored
/// is inherited across exec, which left that daemon running with nothing of its own to hear it.
///
/// A part that serves gets the whole of a stop to unmount its folders and keep its trees. A
/// daemon between parts has nothing to let go of, and the start that follows the part that ended
/// may wait on something that never answers: a stop heard there ends the daemon itself, taking
/// `native` away with it, rather than leave a process nobody can stop but with a kill.
#[expect(
    clippy::exit,
    reason = "a daemon told to stop whose workers do not take the stop up cannot end through them"
)]
pub(crate) fn hear_stops(stop: Stop, within: Duration, native: PathBuf) -> Result<()> {
    std::thread::Builder::new()
        .name("tilo-signals".to_owned())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                tracing::error!("this daemon cannot hear a stop by a signal");
                return;
            };
            runtime.block_on(async move {
                let (signalled, mut heard) = tokio::sync::mpsc::channel(1);
                listen_for_stops(signalled);
                if let Some(name) = heard.recv().await {
                    stop.told_by(name);
                    if !stop.part_serves() {
                        tokio::time::sleep(BETWEEN_PARTS).await;
                    }
                    if stop.part_serves() {
                        tokio::time::sleep(within).await;
                    }
                    let serving = stop.part_serves();
                    if serving {
                        tracing::error!(
                            signal = name,
                            waited_on = %stop.waited_on(),
                            "this daemon ends: it was told to stop ({name}) {}s ago and its \
                             stop did not finish, so its workers never took it up",
                            within.as_secs()
                        );
                    } else {
                        tracing::error!(
                            signal = name,
                            waited_on = %stop.waited_on(),
                            "this daemon ends: it was told to stop ({name}) between its person's \
                             parts and no part started to take it up; its endpoint goes with it"
                        );
                        // The endpoint is the daemon's own: one that ends without closing it
                        // leaves the next start a socket nobody listens on, so it is taken away
                        // here as its own close would.
                        let _ = std::fs::remove_file(&native);
                    }
                    std::process::exit(i32::from(serving));
                }
            });
        })?;
    Ok(())
}

/// Send `name` on `signalled` once `signal` answers, on a task of its own that owns the signal:
/// one listener per way a holder can be told to stop.
macro_rules! hear_stop {
    ($signal:ident, $name:expr, $signalled:ident) => {{
        let signalled = $signalled.clone();
        let mut signal = $signal;
        tokio::spawn(async move {
            if signal.recv().await.is_some() {
                let _ = signalled.send($name).await;
            }
        });
    }};
}

/// Hear each way this platform stops a holder and send its name on `signalled`, keeping
/// `signalled` open until the listeners end: a name a person's stop or their service's stop is
/// known by.
#[cfg(unix)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "the function owns the sender, so the channel closes when its listeners end"
)]
fn listen_for_stops(signalled: tokio::sync::mpsc::Sender<&'static str>) {
    use tokio::signal::unix::{SignalKind, signal};
    for (kind, name) in [
        (SignalKind::terminate(), "SIGTERM"),
        (SignalKind::interrupt(), "SIGINT"),
        (SignalKind::hangup(), "SIGHUP"),
    ] {
        match signal(kind) {
            Ok(signal) => hear_stop!(signal, name, signalled),
            Err(error) => tracing::error!(
                %error,
                signal = name,
                "this holder cannot hear a stop by this signal"
            ),
        }
    }
}

/// Hear each control event Windows ends a process with: Ctrl-C and Ctrl-Break from the console,
/// and Ctrl-Close and Ctrl-Shutdown from the system, which a service stop or a logout raises.
#[cfg(windows)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "the function owns the sender, so the channel closes when its listeners end"
)]
fn listen_for_stops(signalled: tokio::sync::mpsc::Sender<&'static str>) {
    use tokio::signal::windows;
    macro_rules! hear_one {
        ($listen:path, $name:literal) => {
            match $listen() {
                Ok(signal) => hear_stop!(signal, $name, signalled),
                Err(error) => tracing::error!(
                    %error,
                    signal = $name,
                    "this holder cannot hear a stop by this signal"
                ),
            }
        };
    }
    hear_one!(windows::ctrl_c, "Ctrl-C");
    hear_one!(windows::ctrl_break, "Ctrl-Break");
    hear_one!(windows::ctrl_close, "Ctrl-Close");
    hear_one!(windows::ctrl_shutdown, "Ctrl-Shutdown");
}

/// The peer this holder runs, once it does, until it ends.
static PEER: std::sync::Mutex<Option<Arc<super::peer::Peer>>> = std::sync::Mutex::new(None);

/// The peer this process's holder runs, while it runs one.
pub(crate) fn peer() -> Option<Arc<super::peer::Peer>> {
    PEER.lock_recover().clone()
}
/// Why this holder's device is not in the workspace, while it is not.
pub(crate) static NOT_IN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Whether a person's part ended in this process: what it held (the workspaces' port among it)
/// may still be let go of as the next one starts.
pub(crate) static PART_ENDED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Open the peer's stores at `state`, waiting while another holder still has them open: the one
/// that held the lock before this one let go of it before its process ended, and the databases it
/// held are locked until then. redb's lock is the kernel's, dropped with the process, so a holder
/// that died is never waited out.
async fn open_peer(state: &Path, home: &Path) -> anyhow::Result<super::peer::Peer> {
    let deadline = Instant::now()
        .checked_add(OPENING)
        .unwrap_or_else(Instant::now);
    let mut waiting = false;
    loop {
        match super::peer::Peer::open(state, home).await {
            Ok(peer) => return Ok(peer),
            Err(error) if tilo_fs::ContentStore::held_elsewhere(&error) => {
                if Instant::now() >= deadline {
                    return Err(error.context(format!(
                        "another Tilo still holds the workspaces' files open after {} seconds",
                        OPENING.as_secs()
                    )));
                }
                if !waiting {
                    waiting = true;
                    tracing::info!(
                        %error,
                        "another Tilo still has the workspaces' files open; this one waits"
                    );
                }
                tokio::time::sleep(OPENING_POLL).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Run the peer: the person's workspaces' folders, as their server lists them to `daemon`'s
/// account and its own workspace service keeps them ([`super::peer::Peer::keep`]). A computer
/// with no login shows none here, and says why.
async fn keep_peer(daemon: Arc<Daemon>, home: PathBuf, state: PathBuf) {
    let peer = match open_peer(&state, &home).await {
        Ok(peer) => Arc::new(peer),
        Err(error) => {
            tracing::error!(%error, "the workspaces' files cannot be kept here");
            *NOT_IN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(
                "Tilo's background sync isn't keeping this computer's workspace files yet. Run \
                 `tilo status` again in a minute."
                    .to_owned(),
            );
            return;
        }
    };
    *PEER.lock_recover() = Some(Arc::clone(&peer));
    let Some(account) = daemon.account().await else {
        *NOT_IN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(tilo_api::daemon::enter::NOT_LOGGED_IN.to_owned());
        return;
    };
    let own = Arc::clone(daemon.serving());
    let login = daemon.home_source();
    let key = daemon.key().clone();
    drop(daemon);
    Box::pin(peer.keep(own, account, login, key)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeServices as _;
    use std::io::{BufRead as _, Write as _};
    use tilo_test_env::SpawnUnderEnvLock as _;

    const AGENT_TEST: &str =
        "device::serve::tests::an_agent_on_the_device_socket_acts_in_the_daemons_workspace";

    /// One agent connection to the device's socket, as `tilo mcp` relays it.
    struct Agent {
        writing: Stream,
        reading: std::io::BufReader<Stream>,
        asked: u64,
        /// The run the daemon answers this connection as.
        run: Value,
    }

    impl Agent {
        fn connect(agents: &Agents) -> Self {
            let writing = agents.connect();
            writing
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let reading = std::io::BufReader::new(writing.try_clone().unwrap());
            let mut agent = Self {
                writing,
                reading,
                asked: 0,
                run: Value::Null,
            };
            let init = agent.ask(
                "initialize",
                &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "Claude Code", "version": "1"}}),
            );
            assert_eq!(init["serverInfo"]["name"], "tilo-runtime", "{init}");
            agent.run = init["capabilities"]["experimental"]["so.tilo.mcp"]["session"].clone();
            assert!(agent.run.is_string(), "{init}");
            agent
        }

        /// The result of `method` with `params`.
        fn ask(&mut self, method: &str, params: &Value) -> Value {
            self.asked = self.asked.saturating_add(1);
            let id = self.asked;
            writeln!(
                self.writing,
                "{}",
                json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
            )
            .unwrap();
            loop {
                let mut line = String::new();
                assert!(
                    self.reading.read_line(&mut line).unwrap() > 0,
                    "the daemon hung up"
                );
                let answer: Value = serde_json::from_str(&line).unwrap();
                if answer["id"] == json!(id) {
                    return answer["result"].clone();
                }
            }
        }

        /// The text `tool` answers with `arguments`, and whether it failed.
        fn tool(&mut self, tool: &str, arguments: &Value) -> (String, bool) {
            let called = self.ask("tools/call", &json!({"name": tool, "arguments": arguments}));
            let text = called["content"][0]["text"].as_str().unwrap_or_default();
            (text.to_owned(), called["isError"] == json!(true))
        }
    }

    /// An agent's `tilo mcp` reaches the device's socket and acts in the daemon's workspace, as
    /// the apps on this computer do: it lists what the workspace holds and the person's
    /// workspaces through their account. A second connection is a run of its own beside it.
    #[test]
    fn an_agent_on_the_device_socket_acts_in_the_daemons_workspace() {
        let Some(root) = super::super::services::tests::isolated_root(AGENT_TEST) else {
            return;
        };
        let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
        let runtime = bridge_runtime().unwrap();
        let socket = runtime.block_on(async {
            let socket = Socket::new(&daemon).unwrap();
            daemon.agents().serve(taking(Arc::clone(&socket)));
            socket
        });

        let mut first = Agent::connect(daemon.agents());
        let tools = first.ask("tools/list", &json!({}));
        assert!(
            tools["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "tilo_do")),
            "{tools}"
        );
        let (listed, failed) = first.tool("tilo_ls", &json!({"path": ""}));
        assert!(!failed && listed.contains("devices"), "{listed}");
        let (actions, failed) = first.tool("tilo_ls", &json!({"path": "devices"}));
        assert!(!failed && actions.contains("#issue"), "{actions}");
        let (workspaces, failed) =
            first.tool("tilo_do", &json!({"path": "account/workspaces#list"}));
        assert!(!failed, "{workspaces}");
        assert!(
            workspaces.contains(daemon.workspace().expect("the workspace").as_str()),
            "{workspaces}"
        );

        let mut second = Agent::connect(daemon.agents());
        assert_ne!(first.run, second.run, "each connection is a run of its own");
        let (listed, failed) = second.tool("tilo_ls", &json!({"path": ""}));
        assert!(!failed && listed.contains("devices"), "{listed}");

        drop((first, second));
        daemon.agents().withdraw();
        runtime.block_on(socket.presence.close());
    }

    const REVOKED_TEST: &str = "device::serve::tests::an_agent_whose_key_its_person_revokes_is_ended_where_it_is_connected";

    /// An agent on the device's socket acts under a key the person's root certified it. Once the
    /// person revokes that key, the open connection's next call is refused with why and the
    /// connection ends; the workspace's host refuses whatever the key signs, whichever device
    /// sends it; and an agent that connects again is told why it is not served.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one key, from its certification to its revocation and the connection's end, read top to bottom"
    )]
    fn an_agent_whose_key_its_person_revokes_is_ended_where_it_is_connected() {
        use tilo_app_host::agents::agent_keys::{REVOKED, this_device};
        let Some(root) = super::super::services::tests::isolated_root(REVOKED_TEST) else {
            return;
        };
        let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).unwrap());
        let runtime = bridge_runtime().unwrap();
        let socket = runtime.block_on(async {
            let socket = Socket::new(&daemon).unwrap();
            daemon.agents().serve(taking(Arc::clone(&socket)));
            socket
        });

        let mut agent = Agent::connect(daemon.agents());
        let (listed, failed) = agent.tool("tilo_ls", &json!({"path": ""}));
        assert!(!failed && listed.contains("devices"), "{listed}");

        // The one key the person's root certified the agent here, and what signs with it, as a
        // copy of it on another device would.
        let record = crate::device::services::device_root_record().unwrap();
        let root = tilo_identity::store::load_from_path(record).unwrap();
        let copies = daemon.serving().copies().clone();
        let person = copies.device_cert().user_public_key;
        let devices = runtime.block_on(async {
            tilo_workspace_service::workspaces::person_workspace::devices::of(&copies, &person)
                .await
        });
        let agents: Vec<_> = devices
            .listed(&person)
            .into_iter()
            .map(|listed| listed.cert)
            .filter(|cert| cert.capabilities.agent.is_some())
            .collect();
        let [cert] = agents.as_slice() else {
            panic!("the person's root certified the agent one key: {agents:?}");
        };
        // Its grant reaches this workspace alone, for this agent alone.
        let scope = cert.capabilities.agent.as_ref().unwrap();
        let workspace =
            tilo_server_protocol::WorkspaceId::parse(&daemon.workspace().expect("the workspace"))
                .unwrap();
        assert_eq!(scope.workspace(), Some(*workspace.as_uuid().as_bytes()));
        assert!(
            !scope.is_agent(&tilo_actions::remote::device_agent_id(
                &cert.user_public_key,
                "another agent"
            )),
            "another agent of the person's cannot sign under it"
        );
        let (_, copy) = this_device()
            .unwrap()
            .made(&cert.device_public_key)
            .unwrap();

        // The person revokes it while the agent's connection is open: their root signs the entry
        // and their devices take it in, the record marking it with them.
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: cert.user_public_key,
            revoked_device_public_key: cert.device_public_key,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        runtime
            .block_on(daemon.serving().copies().announce_revocations(vec![entry]))
            .unwrap();

        // The same connection's next call is refused with why, and the connection ends.
        let (said, failed) = agent.tool("tilo_ls", &json!({"path": ""}));
        assert!(failed, "{said}");
        assert_eq!(said, REVOKED);
        let mut line = String::new();
        assert_eq!(
            agent.reading.read_line(&mut line).unwrap(),
            0,
            "the connection ends after its refusal: {line}"
        );

        // The workspace's host was told: what the key signs is refused there, sent by anyone.
        let host = daemon.serving().copies().clone();
        let deadline = Instant::now() + tilo_test_env::hang_bound();
        while !host.revocations().refuses(cert) {
            assert!(
                Instant::now() < deadline,
                "the host was never told the revocation"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut asked = tilo_actions::remote::InvokeRequest {
            target: tilo_actions::remote::RemoteActionTarget {
                provider: "so.tilo.notes".into(),
                address: tilo_actions::api::ActionAddress::new("notes:1", "rename"),
            },
            argument: tilo_actions::remote::InvokeArgument::Text("Plans".into()),
            invocation: tilo_actions::InvocationId::new(),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        asked.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation_now(
                &daemon.workspace().expect("the workspace"),
                &asked,
                cert,
                &copy,
                tilo_clock::unix_now_ns(),
            )
            .unwrap(),
        ));
        assert_eq!(
            tilo_server_protocol::invocation::verify_invocation(
                &daemon.workspace().expect("the workspace"),
                &asked,
                &host.revocations(),
                tilo_clock::unix_now_ns(),
            ),
            Err(tilo_server_protocol::invocation::InvocationError::Revoked)
        );

        // An agent that connects again is told why, and served nothing.
        let mut again = daemon.agents().connect();
        again
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        writeln!(
            again,
            "{}",
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "Claude Code", "version": "1"}}})
        )
        .unwrap();
        let mut told = String::new();
        std::io::BufReader::new(&again)
            .read_line(&mut told)
            .unwrap();
        let told: Value = serde_json::from_str(&told).unwrap();
        assert_eq!(told["error"]["message"], REVOKED, "{told}");

        drop((agent, again));
        daemon.agents().withdraw();
        runtime.block_on(socket.presence.close());
    }

    /// A stop ends by itself inside the user service's timeout: every wait it makes, added
    /// up, is shorter than what the unit gives it before the process is killed, so a holder
    /// stopped while a bulk write is under way exits rather than being killed.
    #[test]
    fn a_stop_ends_inside_the_services_timeout() {
        assert!(
            STOPPED_WITHIN < tilo_api::daemon::STOP_TIMEOUT,
            "a stop's waits add up to {STOPPED_WITHIN:?}, past the unit's {:?}",
            tilo_api::daemon::STOP_TIMEOUT
        );
    }

    /// Set in the child of [`a_stop_ends_a_holder_whose_workers_never_take_it_up`].
    const CHILD_STOPPED: &str = "TILO_TEST_HOLDER_STOPPED";
    /// How long that child's stop may take.
    const CHILD_STOPPED_WITHIN: Duration = Duration::from_secs(1);

    /// The body of [`a_stop_ends_a_holder_whose_workers_never_take_it_up`]'s child, and a no-op in
    /// every other run: a daemon told to stop whose workers never read that it was.
    #[test]
    fn a_child_is_stopped_with_busy_workers() {
        if std::env::var_os(CHILD_STOPPED).is_none() {
            return;
        }
        // A part serves, as the bound this child is held to is a serving part's: the process
        // ends here once that bound went by, with the status a stop that was never taken up has.
        let stop = Stop::default();
        stop.serving(true);
        hear_stops(
            stop,
            CHILD_STOPPED_WITHIN,
            std::env::temp_dir().join("tilo-test-never-bound.sock"),
        )
        .unwrap();
        // The signal thread's handlers are in place well before this: a SIGTERM before them
        // would end the child by the signal, which the parent tells apart.
        std::thread::sleep(Duration::from_millis(500));
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TERM)
            .unwrap();
        // Workers that never take the stop up: the stop is told, and nothing reads it.
        std::thread::sleep(Duration::from_secs(60));
    }

    /// A holder told to stop ends however busy its workers are: the stop runs on them, and one
    /// they never take up still ends the process once a stop's waits went by, as a holder busy
    /// re-reading its folder's tree once ignored SIGTERM until it was killed.
    #[test]
    fn a_stop_ends_a_holder_whose_workers_never_take_it_up() {
        let module = module_path!();
        let name = format!(
            "{}::a_child_is_stopped_with_busy_workers",
            module.split_once("::").map_or(module, |(_, within)| within)
        );
        let started = Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture", "--test-threads", "1"])
            .env(CHILD_STOPPED, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status_locked()
            .unwrap();
        let took = started.elapsed();
        assert_eq!(
            status.code(),
            Some(1),
            "the stopped child ended as {status}"
        );
        assert!(
            took < Duration::from_secs(30),
            "the stopped child ended only after {} s",
            took.as_secs()
        );
    }

    /// A holder whose workers are both inside a step that walks a whole folder tree still ends
    /// when it is stopped: the steps are left behind rather than waited for, which is what a
    /// stop (the user service's, or `tilo folders stop`) hangs on otherwise.
    #[test]
    fn a_stop_does_not_wait_for_a_worker_inside_a_step() {
        let runtime = bridge_runtime().unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for _ in 0..BRIDGE_THREADS {
            let done = std::sync::Arc::clone(&done);
            runtime.spawn(async move {
                // A step on a folder's tree: it does not yield until it is done.
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            });
        }
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        runtime.shutdown_timeout(SHUTDOWN);
        let waited = started.elapsed();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            waited < SHUTDOWN.saturating_mul(2),
            "the runtime waited {} ms for a step to end",
            waited.as_millis()
        );
    }

    /// Names the lock file a child process takes and then ends still holding.
    const CHILD_LOCK: &str = "TILO_TEST_HOLDER_LOCK";

    /// The body of [`a_holder_that_ended_holds_no_lock`]'s child, and a no-op in every other run:
    /// it takes the holder's lock and ends without unlocking it, as an ended process does.
    #[test]
    #[allow(
        clippy::exit,
        reason = "the child ends still holding its lock: a return would run the destructor that unlocks it"
    )]
    fn a_child_takes_the_holders_lock() {
        let Some(path) = std::env::var_os(CHILD_LOCK) else {
            return;
        };
        let path = Path::new(&path);
        let _held = advisory_lock::FileLock::try_open(path, advisory_lock::LockMode::Exclusive)
            .expect("the child takes the lock");
        std::fs::write(path.with_extension("held"), b"").expect("the child records the lock");
        // End without running destructors: the kernel drops the lock with the process.
        std::process::exit(0);
    }

    /// A holder that ended, however it ended, holds no lock a new one waits for: the lock is the
    /// kernel's to drop with the process, and a process that is gone is not waited out.
    #[test]
    fn a_holder_that_ended_holds_no_lock() {
        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path());
        let lock = place.lock().to_path_buf();
        let held = take_lock(&place, Duration::ZERO).unwrap();
        assert!(held.is_some());
        assert!(
            take_lock(&place, Duration::ZERO).unwrap().is_none(),
            "a lock its owner holds keeps the next holder out"
        );
        drop(held);
        let module = module_path!();
        let name = format!(
            "{}::a_child_takes_the_holders_lock",
            module.split_once("::").map_or(module, |(_, within)| within)
        );
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture", "--test-threads", "1"])
            .env(CHILD_LOCK, &lock)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status_locked()
            .unwrap();
        assert!(status.success(), "the child that took the lock failed");
        assert!(
            lock.with_extension("held").exists(),
            "the child never took the lock"
        );
        assert!(
            take_lock(&place, Duration::ZERO).unwrap().is_some(),
            "the lock of a process that ended wedges the next holder"
        );
    }

    /// A holder that is there is seen from its lock alone, without asking it anything: that is
    /// the fact a command starts, waits or hands over on, and a busy holder still gives it.
    #[test]
    fn a_holders_liveness_is_seen_from_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path());
        assert!(!place.alive(), "nothing holds the device");
        let held = take_lock(&place, Duration::ZERO)
            .unwrap()
            .expect("the lock is free");
        assert!(place.alive(), "the holder's lock says it is there");
        drop(held);
        assert!(!place.alive(), "a holder that ended is not waited for");
    }

    /// A holder starting while the last one still has the workspaces' files open waits for them
    /// and opens them then: it does not start without a peer.
    #[test]
    fn a_holder_waits_for_the_last_ones_files_to_close() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = bridge_runtime().unwrap();
        let first = runtime
            .block_on(super::super::peer::Peer::open(dir.path(), dir.path()))
            .expect("the first holder opens the files");
        let (started, starting) = std::sync::mpsc::channel();
        let (opened, opening) = std::sync::mpsc::channel();
        let at = dir.path().to_path_buf();
        let waiter = std::thread::spawn(move || {
            let runtime = bridge_runtime().unwrap();
            let result = runtime.block_on(async move {
                started.send(()).unwrap();
                open_peer(&at, &at)
                    .await
                    .map(drop)
                    .map_err(|error| format!("{error:#}"))
            });
            opened.send(result).unwrap();
        });
        starting.recv().unwrap();
        assert!(
            opening.recv_timeout(Duration::from_millis(300)).is_err(),
            "a second holder opened the files this one still holds"
        );
        runtime.block_on(first.shutdown(RELEASING));
        opening
            .recv_timeout(Duration::from_secs(30))
            .expect("the second holder opens the files once the first lets go")
            .unwrap();
        waiter.join().unwrap();
    }
}
