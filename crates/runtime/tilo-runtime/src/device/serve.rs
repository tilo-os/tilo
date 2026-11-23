//! This computer's one daemon as it serves the device: the `tilo mcp` connections every agent
//! of the login makes to it, the workspace commands of the login, and the person's workspaces'
//! folders, all over the one session it holds with the workspace.
//!
//! The host keeps one session per device, so two processes entering as the same device cut each
//! other off, over and over: each agent's calls then fail as "not connected to the host" or go
//! unanswered. Every `tilo mcp` therefore reaches this daemon over a socket in the device's
//! directory and relays its agent's stdio to it; the daemon serves each connection as its own
//! agent run over the one session ([`hold`]).

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};

use tilo_api::daemon::{Claim, DaemonPlace, Program, naming};

use super::{Connection, Enter, Shared, answer};

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
/// How often the service's holder, waiting for a newer program's holder to end, looks for its lock.
const STANDBY_POLL: Duration = Duration::from_secs(1);
/// How often the service's holder, waiting so, asks the holder there again to step down.
const STANDBY_ASK: Duration = Duration::from_secs(30);
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
/// How often a holder looks whether the login it serves is still the one under its device
/// directory: a login made again makes another device, and the holder it replaced steps down so
/// one computer runs one holder.
const LOGIN_LOOK: Duration = Duration::from_secs(3);

/// The most a holder's log keeps: a holder that takes the device empties a log grown past it.
const LOG_KEPT: u64 = 8 << 20;

/// Empty this holder's log once it grew past [`LOG_KEPT`]: it is added to by every holder
/// started ([`start_holder`]), and the one that holds the device is the one that writes most.
fn trim_log() {
    let stderr = std::io::stderr();
    let Ok(metadata) = rustix::fs::fstat(&stderr) else {
        return;
    };
    let regular =
        rustix::fs::FileType::from_raw_mode(metadata.st_mode) == rustix::fs::FileType::RegularFile;
    if regular && u64::try_from(metadata.st_size).is_ok_and(|size| size > LOG_KEPT) {
        let _ = rustix::fs::ftruncate(&stderr, 0);
    }
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

/// One agent's connection, served as its own run until its input ends: each
/// request answered as it comes, so a long call holds up no other.
///
/// `session_name` is what the agent's harness calls this session, when the process serving it
/// knows: the initialize the client sends then says it ([`tilo_mcp::session_api::SESSION_NAME`]),
/// and the workspace shows the agent by that name rather than by its product's name.
async fn converse<E: Enter>(
    shared: Arc<Shared<E>>,
    read: impl AsyncRead + Unpin,
    write: impl AsyncWrite + Unpin + Send + 'static,
    session_name: Option<&str>,
) {
    let connection = Arc::new(Connection::new(shared));
    let write = Arc::new(tokio::sync::Mutex::new(write));
    let mut lines = BufReader::new(read).lines();
    let mut replying = tokio::task::JoinSet::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let line = naming(line, session_name);
        let (connection, write) = (Arc::clone(&connection), Arc::clone(&write));
        replying.spawn(async move {
            if let Some(answer) = answer(&connection, &line).await {
                let mut write = write.lock().await;
                let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
                let _ = write.flush().await;
                drop(write);
            }
        });
        while replying.try_join_next().is_some() {}
    }
    while replying.join_next().await.is_some() {}
}

/// The claim this process makes on the device: its program, and whether the user service
/// started it.
fn claim() -> Claim {
    Claim {
        program: Program::running(),
        service: tilo_api::daemon::running_as_service(),
    }
}

/// Take the lock that makes this process the device's one daemon, as [`take_lock`] does for
/// this process's claim.
///
/// # Errors
/// Another daemon keeps the device, or the lock cannot be opened.
pub(crate) fn lock_device(place: &DaemonPlace) -> std::io::Result<advisory_lock::FileLock> {
    take_lock(place, TAKING_OVER, claim())
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

/// This computer as the person's peer: the device's session held for every connection at
/// `place` for as long as this process runs, while it holds the device's lock
/// ([`lock_device`]), and the person's workspaces' files shown below `home` over it
/// ([`super::peer`]).
pub(crate) fn hold<E: Enter>(
    place: &DaemonPlace,
    shared: Arc<Shared<E>>,
    home: PathBuf,
) -> Result<ExitCode> {
    let device_dir = place.device_dir();
    held(
        place,
        shared,
        Holding::Peer {
            home,
            state: device_dir,
        },
        claim(),
    )
}

/// How long a holder holds, and what else it does.
enum Holding {
    /// Until no connection came for this long.
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "a holder that lingers out is a test's")
    )]
    Linger(Duration),
    /// For as long as it runs, as the peer that shows the workspaces' files
    /// below `home`, keeping their state in `state`.
    Peer { home: PathBuf, state: PathBuf },
}

/// [`hold`], holding as `holding` says, as a holder claiming `claim`, and waiting up to
/// `taking_over` for a holder that is leaving, or that `claim` asked to step down.
#[cfg(test)]
fn hold_for<E: Enter>(
    place: &DaemonPlace,
    shared: Arc<Shared<E>>,
    holding: Holding,
    taking_over: Duration,
    claim: Claim,
) -> Result<ExitCode> {
    let Some(_lock) = take_lock(place, taking_over, claim)? else {
        tracing::error!("this holder ends: another holder of this device's session keeps it");
        return Ok(ExitCode::SUCCESS);
    };
    held(place, shared, holding, claim)
}

/// [`hold`], holding as `holding` says, as a holder claiming `claim`, while the device's lock is
/// held.
fn held<E: Enter>(
    place: &DaemonPlace,
    shared: Arc<Shared<E>>,
    holding: Holding,
    claim: Claim,
) -> Result<ExitCode> {
    say_panics();
    trim_log();
    let _ = shared.held_as.set(claim);
    let runtime = bridge_runtime()?;
    // A holder that stops answering for seconds says what it was doing, with every thread's
    // stack, long before [`WEDGED`] ends it: a person's `ls` that waits has a reason in the log.
    tilo_rt::stall::watch(WEDGED.min(STALL));
    tilo_rt::stall::heartbeat(runtime.handle());
    runtime.spawn(beating());
    let watching = watch_workers();
    let served = runtime.block_on(serve_held(place, shared, holding));
    // A step that walks a whole folder tree — a merge of a tree record, a save of one — runs on
    // its thread to its end and cannot be interrupted. The process ends now instead of waiting
    // for it: the socket is gone, the folders were unmounted (or given up on), and the trees
    // were kept, above.
    runtime.shutdown_timeout(SHUTDOWN);
    drop(watching);
    // The next holder opens the stores as soon as this process lets go of the lock, which is
    // before this process ends: let go of them first, or it finds them still open.
    if let Some(peer) = PEER.get() {
        let releasing = bridge_runtime()?;
        // The timer is made inside the runtime: one made outside it has no clock to tick on.
        if releasing
            .block_on(async { tokio::time::timeout(RELEASING, peer.shutdown(RELEASING)).await })
            .is_err()
        {
            tracing::warn!("the workspaces' files did not close in time");
        }
        releasing.shutdown_timeout(SHUTDOWN);
    }
    if let Err(error) = &served {
        tracing::error!(%error, "this holder ends: it could not serve");
    }
    served?;
    Ok(ExitCode::SUCCESS)
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

/// The lock that makes this process the device's one holder, once one that is leaving let go of
/// it; `None` when another holds it still. A holder this one's `claim` takes over from (an older
/// program's, or one an agent started where this is the user service's) is asked to step down,
/// and waited for: a CLI command that found no holder running starts one from the program it
/// is, and Tilo updated since must not leave that one serving the old program's answers while
/// the new one's holder gives up.
///
/// The user service's holder never gives up: while a newer program's holder keeps the device it
/// waits for the lock, and holds the device the moment that one ends. A service that ended
/// beside a holder some agent started left the person's folders to that agent's lifetime.
fn take_lock(
    place: &DaemonPlace,
    taking_over: Duration,
    claim: Claim,
) -> Result<Option<advisory_lock::FileLock>> {
    use advisory_lock::{FileLock, TryLockError};
    let path = place.lock();
    let mut deadline = Instant::now()
        .checked_add(taking_over)
        .unwrap_or_else(Instant::now);
    // When the holder there last answered: it is asked until it does, once (again and again, by
    // the service's holder waiting for it).
    let mut answered_at: Option<Instant> = None;
    let mut waiting = false;
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
        // The service's holder asks again now and then: the holder that refused may have been
        // replaced by one that would step down.
        if claim.service && answered_at.is_some_and(|at: Instant| at.elapsed() >= STANDBY_ASK) {
            answered_at = None;
        }
        let stepping_down = if answered_at.is_some() {
            None
        } else {
            place.ask_to_step_down(claim)
        };
        if stepping_down.is_some() {
            answered_at = Some(Instant::now());
        }
        if stepping_down == Some(true) {
            tracing::info!("the holder of this device steps down for this one");
            // Stepping down unmounts the folders and closes the stores: it is waited for.
            deadline = deadline.max(
                Instant::now()
                    .checked_add(TAKING_OVER)
                    .unwrap_or_else(Instant::now),
            );
        }
        if Instant::now() >= deadline {
            if claim.service {
                if !waiting {
                    waiting = true;
                    tracing::info!(
                        lock = %path.display(),
                        "a holder of a newer Tilo keeps this device; the service holds it once \
                         that one ends"
                    );
                }
                std::thread::sleep(STANDBY_POLL);
                continue;
            }
            tracing::warn!(
                lock = %path.display(),
                "another holder of this device's session keeps it; this one ends"
            );
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// Listen at `place` and serve each connection, while the lock is held.
async fn serve_held<E: Enter>(
    place: &DaemonPlace,
    shared: Arc<Shared<E>>,
    holding: Holding,
) -> Result<()> {
    // Replaces a socket left by a holder that ended without removing it.
    let listener = tilo_files::native::private::bind_socket(place.socket())
        .with_context(|| format!("listen at {}", place.socket().display()))?;
    serve_api(place, &shared)?;
    let linger = match holding {
        Holding::Linger(linger) => {
            // Enter the workspace while the first agent starts, not at its first call.
            let entering = Arc::clone(&shared);
            tokio::spawn(async move {
                let _ = entering.session(false, super::Ask::Sync).await;
            });
            Some(linger)
        }
        Holding::Peer { home, state } => {
            tokio::spawn(keep_peer(Arc::clone(&shared), home, state));
            None
        }
    };
    let (count, mut live) = tokio::sync::watch::channel(0_usize);
    let count = Arc::new(count);
    let mut accepting = accepting(listener, Arc::clone(&shared), Arc::clone(&count))?;
    let mut ended = ending(STOPPED_WITHIN)?;
    let mut login = tokio::time::interval(LOGIN_LOOK);
    // A computer that logs in, or out, while its daemon runs has another device to be: the
    // daemon ends, and the next one serves the login as it is now.
    let logged_in = place.logged_in();
    // Whether the login this holder serves was replaced: a holder on its way out then unmounts
    // nothing, since the holder of the new login shows the folders again.
    let mut replaced = false;
    // Why this holder ends, said in its log at error level whatever it is: a holder that ended
    // without its log saying why left the person's folders unmounted with nothing to read.
    let why = loop {
        tokio::select! {
            () = idle(&mut live, linger) => break "no connection came for as long as it lingers".to_owned(),
            _ = login.tick() => if !place.serves_current_login() || place.logged_in() != logged_in {
                replaced = true;
                break "this computer logged in again; the holder of the new login takes over"
                    .to_owned();
            },
            Some(signal) = ended.recv() => break format!("it was told to stop ({signal})"),
            () = shared.stepping_down.notified() => {
                break "a holder of a newer Tilo, or the user service's, takes this device over \
                       now that its folders are quiet"
                    .to_owned();
            },
        }
    };
    tracing::error!("this holder ends: {why}");
    // No connection from here on: the socket is shut down rather than left queuing clients that
    // would be told a holder is there by the kernel and wait in vain for it.
    accepting.stop();
    if !replaced && let Some(peer) = PEER.get() {
        // Unmounting and keeping the trees wait for the mounts' and trees' own steps, which
        // cannot be interrupted; both are bounded, so a stop ends inside the unit's timeout.
        peer.stop(STOPPING).await;
    }
    let _ = std::fs::remove_file(place.socket());
    let _ = std::fs::remove_file(place.api());
    shared.leaving().await;
    Ok(())
}

/// The thread that takes connections: on one of its own, so a client asking (a person's `tilo
/// status`, an agent's `tilo mcp`) is taken the moment it does, however busy the runtime that
/// answers it is with another client's work. Each connection is served on that runtime, whose
/// workers a bulk write's steps never occupy: those go to the blocking pool.
struct Accepting {
    /// A duplicate of the listening socket: shutting it down wakes the thread's accept.
    socket: std::os::unix::net::UnixListener,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Accepting {
    fn stop(&mut self) {
        let _ = rustix::net::shutdown(&self.socket, rustix::net::Shutdown::Both);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Accepting {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Take the connections at `listener` on a thread of its own, serving each on this runtime.
fn accepting<E: Enter>(
    listener: std::os::unix::net::UnixListener,
    shared: Arc<Shared<E>>,
    count: Arc<tokio::sync::watch::Sender<usize>>,
) -> std::io::Result<Accepting> {
    let socket = listener.try_clone()?;
    let runtime = tokio::runtime::Handle::current();
    let thread = std::thread::Builder::new()
        .name("tilo-accept".to_owned())
        .spawn(move || {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        count.send_modify(|live| *live = live.saturating_add(1));
                        let (shared, count) = (Arc::clone(&shared), Arc::clone(&count));
                        let runtime = runtime.clone();
                        // Its first request is read on a thread of its own: `tilo status` is
                        // answered there whether or not the runtime's workers are free, and
                        // anything else goes on to the runtime as before.
                        let started = std::thread::Builder::new()
                            .name("tilo-connection".to_owned())
                            .spawn(move || first_request(stream, shared, &runtime, count));
                        if let Err(error) = started {
                            tracing::warn!(%error, "a connection to this holder could not be served");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    // The holder stopped listening: the socket was shut down.
                    Err(_) => break,
                }
            }
        })?;
    Ok(Accepting {
        socket,
        thread: Some(thread),
    })
}

/// Serve `stream`, a connection [`accepting`] took: its first request is read here, and one
/// asking how the peer stands ([`super::PEER_STATUS`]) is answered here too, from
/// [`status_answer`]; the connection is served on `runtime` from there on.
fn first_request<E: Enter>(
    stream: UnixStream,
    shared: Arc<Shared<E>>,
    runtime: &tokio::runtime::Handle,
    count: Arc<tokio::sync::watch::Sender<usize>>,
) {
    use std::io::{BufRead as _, Write as _};
    let ended = || count.send_modify(|live| *live = live.saturating_sub(1));
    let Ok(reading) = stream.try_clone() else {
        ended();
        return;
    };
    let mut reader = std::io::BufReader::new(reading);
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        ended();
        return;
    }
    let request = serde_json::from_str::<Value>(&first).ok();
    if let Some(request) = &request
        && request.get("method").and_then(Value::as_str) == Some(super::PEER_STATUS)
    {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let answer = json!({"jsonrpc": "2.0", "id": id, "result": status_answer(&shared, runtime)});
        let mut writing = &stream;
        let _ = writeln!(writing, "{answer}");
        let _ = writing.flush();
        ended();
        return;
    }
    // What the thread read past the first line is the connection's too.
    let mut read = first.into_bytes();
    read.extend_from_slice(reader.buffer());
    drop(reader);
    let Ok(()) = stream.set_nonblocking(true) else {
        ended();
        return;
    };
    let guard = runtime.enter();
    let stream = tokio::net::UnixStream::from_std(stream);
    drop(guard);
    let Ok(stream) = stream else {
        ended();
        return;
    };
    runtime.spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let (rest, write) = stream.into_split();
        // This process is no agent's: the name comes in each connection's own initialize,
        // which the connection that reached it wrote.
        converse(shared, std::io::Cursor::new(read).chain(rest), write, None).await;
        count.send_modify(|live| *live = live.saturating_sub(1));
    });
}

/// How long `tilo status` waits for the runtime to say how the peer stands before it is answered
/// with what the peer last said.
const STATUS_FRESH: Duration = Duration::from_secs(2);

/// The last answer to `tilo status` and when it was made.
static LAST_STATUS: std::sync::Mutex<Option<(Instant, Value)>> = std::sync::Mutex::new(None);

/// How the peer stands: asked of the runtime and waited for at most [`STATUS_FRESH`]. When the
/// runtime's workers do not answer in that time, what the peer last said, with how long ago
/// (`answered_ago_s`), so `tilo status` answers whatever the peer is busy with and says it is.
fn status_answer<E: Enter>(shared: &Arc<Shared<E>>, runtime: &tokio::runtime::Handle) -> Value {
    let (told, answered) = std::sync::mpsc::sync_channel(1);
    let asked = Arc::clone(shared);
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

/// Serve Tilo's API at `place` as this computer's device, over the one session it holds: every
/// workspace command of this login (`tilo do`, `ls`, `read`, `workspace`, `keys`) is a call here,
/// one round trip over a session already in the workspace, rather than a session of its own that
/// would cut this one off at the server. While the device is not in the workspace, each call
/// answers why.
fn serve_api<E: Enter>(place: &DaemonPlace, shared: &Arc<Shared<E>>) -> Result<()> {
    let listener = tilo_files::native::private::bind_socket(place.api())
        .and_then(|listener| {
            listener.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(listener)
        })
        .with_context(|| format!("listen at {}", place.api().display()))?;
    let shared = Arc::clone(shared);
    tokio::spawn(tilo_api::direct::serve_current(listener, move || {
        let shared = Arc::clone(&shared);
        async move {
            let fallback = shared.fallback_api();
            match shared.session(false, super::Ask::Agent).await {
                Ok(session) => session.api.clone().or(fallback).ok_or_else(|| {
                    "this computer's login does not reach Tilo's API here; log in again".to_owned()
                }),
                Err(why) => fallback.ok_or(why),
            }
        }
    }));
    Ok(())
}

/// How long a holder told to stop takes at most to end: every wait of its stop together (the
/// folders and trees, the session closing, the runtime's threads, the stores closing). Less than
/// the user service gives a stop, so the service never has to kill it.
const STOPPED_WITHIN: Duration = STOPPING
    .saturating_add(super::CLOSING)
    .saturating_add(SHUTDOWN)
    .saturating_add(RELEASING)
    .saturating_add(SHUTDOWN);

/// What ends a holder from outside: the user service stopping it, or a
/// person's Ctrl-C.
///
/// The signals are waited for on a thread of their own, with a runtime of its own: the holder's
/// two workers may both be inside a step that walks a whole folder tree, and a stop that waited
/// for a free worker was heard only once that step ended. The stop itself runs on those workers
/// (the folders unmount, the trees are kept, the session closes), so a holder whose workers do
/// not take it up ends from this thread once `within` went by: a signal always ends
/// the holder, and its folders' changes are on disk, as a kill would leave them.
#[expect(
    clippy::exit,
    reason = "a holder told to stop whose workers do not take the stop up cannot end through them"
)]
fn ending(within: Duration) -> Result<tokio::sync::mpsc::UnboundedReceiver<&'static str>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (said, ended) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("tilo-signals".to_owned())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                tracing::error!("this holder cannot hear a stop by a signal");
                return;
            };
            runtime.block_on(async move {
                let (signalled, mut heard) = tokio::sync::mpsc::channel(1);
                for (kind, name) in [
                    (SignalKind::terminate(), "SIGTERM"),
                    (SignalKind::interrupt(), "SIGINT"),
                    (SignalKind::hangup(), "SIGHUP"),
                ] {
                    let Ok(mut signal) = signal(kind) else {
                        tracing::error!(
                            signal = name,
                            "this holder cannot hear a stop by this signal"
                        );
                        continue;
                    };
                    let signalled = signalled.clone();
                    tokio::spawn(async move {
                        if signal.recv().await.is_some() {
                            let _ = signalled.send(name).await;
                        }
                    });
                }
                drop(signalled);
                if let Some(name) = heard.recv().await {
                    let _ = said.send(name);
                    tokio::time::sleep(within).await;
                    tracing::error!(
                        signal = name,
                        "this holder ends: it was told to stop ({name}) {}s ago and its stop did \
                         not finish, so its workers never took it up",
                        within.as_secs()
                    );
                    std::process::exit(1);
                }
            });
        })?;
    Ok(ended)
}

/// The peer this holder runs, once it does.
pub(crate) static PEER: std::sync::OnceLock<Arc<super::peer::Peer>> = std::sync::OnceLock::new();
/// Why this holder's device is not in the workspace, while it is not.
pub(crate) static NOT_IN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// How often the peer looks whether its session is still the device's.
const PEER_LOOK: Duration = Duration::from_secs(5);

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
        match super::peer::Peer::open(state, home, true).await {
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

/// Run the peer: enter the workspace, and show the person's workspaces' folders
/// over the session, mounted again whenever the session is a new one, and as
/// `tilo folders` changes them.
async fn keep_peer<E: Enter>(shared: Arc<Shared<E>>, home: PathBuf, state: PathBuf) {
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
    let _ = PEER.set(Arc::clone(&peer));
    let mut shown: Option<Arc<super::Session>> = None;
    // What the host sends this session: the person's workspaces as they are
    // now, and a wake when one arrives.
    let mut personal: Option<tilo_session_client::PersonalWorkspaces> = None;
    let woken = Arc::new(tokio::sync::Notify::new());
    loop {
        match shared.session(false, super::Ask::Sync).await {
            Ok(session) => {
                *NOT_IN
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                follow_session(&peer, &session, &mut shown, &mut personal, &woken).await;
            }
            Err(why) => {
                *NOT_IN
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(why);
            }
        }
        tokio::select! {
            // The host sent a list: the folders follow now, rather than at the
            // next look, and the session is looked at again as it changes.
            () = woken.notified() => {}
            () = tokio::time::sleep(PEER_LOOK) => {}
        }
    }
}

/// Follow one session of the person's device: the folders as `tilo folders`
/// set them, and the person's workspaces as the host sends them. A session
/// this holder did not hold before asks for the list once, so a workspace made
/// while the computer was away is not missed; after that every list the host
/// sends is kept.
async fn follow_session(
    peer: &super::peer::Peer,
    session: &Arc<super::Session>,
    shown: &mut Option<Arc<super::Session>>,
    personal: &mut Option<tilo_session_client::PersonalWorkspaces>,
    woken: &Arc<tokio::sync::Notify>,
) {
    if shown
        .as_ref()
        .is_none_or(|shown| !Arc::ptr_eq(shown, session))
    {
        peer.close().await;
        shown.replace(Arc::clone(session));
        peer.forget_personal().await;
        let followed = tilo_session_client::PersonalWorkspaces::follow(session.device.session());
        let woken = Arc::clone(woken);
        session
            .device
            .session()
            .on_event(followed.sent(), Arc::new(move || woken.notify_one()));
        *personal = Some(followed);
    }
    if let Some(followed) = personal {
        peer.take_personal(&session.device, followed).await;
    }
    // The device's own service holds what the session takes part in, so the keys its key service
    // settles over the member copies are there for the folders, and again once the folders
    // entered more of the person's workspaces.
    if let Some(own) = &session.own {
        own.hold(&session.device).await;
    }
    peer.reconcile(&session.device, session.own.as_deref())
        .await;
    if let Some(own) = &session.own {
        own.hold(&session.device).await;
    }
}

/// Once no connection has been open for `linger`; never without one.
async fn idle(live: &mut tokio::sync::watch::Receiver<usize>, linger: Option<Duration>) {
    let Some(linger) = linger else {
        return std::future::pending().await;
    };
    loop {
        if *live.borrow_and_update() == 0 {
            if tokio::time::timeout(linger, live.changed()).await.is_err() {
                return;
            }
        } else if live.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A holder an agent's or a command's `tilo` started, of `program`.
    const fn agents(program: Program) -> Claim {
        Claim {
            program,
            service: false,
        }
    }

    /// The user service's holder, of `program`.
    const fn services(program: Program) -> Claim {
        Claim {
            program,
            service: true,
        }
    }

    /// The user service's holder starting while one an agent started holds the device (the
    /// service was restarted, or the agent came first): the agent's steps down and the service's
    /// holds the device, so the session and the folders live as long as the login, not as long
    /// as some agent. A newer program's holder an agent started is waited for instead, and the
    /// service holds the device once it ends; the service never ends beside it.
    #[test]
    fn the_services_holder_takes_over_from_one_an_agent_started() {
        let program = Program {
            file: 1,
            modified_ns: 1_000,
        };
        let newer = Program {
            file: 2,
            modified_ns: 2_000,
        };
        assert!(services(program).takes_over(&agents(program)));
        assert!(!agents(program).takes_over(&services(program)));
        assert!(!agents(program).takes_over(&agents(program)));
        assert!(!services(program).takes_over(&services(program)));
        assert!(!services(program).takes_over(&agents(newer)));
        assert!(agents(newer).takes_over(&services(program)));

        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path(), &[7; 32]);
        let hold_waiting = |name: &'static str, claim: Claim, taking_over: Duration| {
            let place = place.clone();
            std::thread::spawn(move || {
                let shared = Arc::new(Shared::new(Named(name), Err("unused".to_owned())));
                hold_for(
                    &place,
                    shared,
                    Holding::Linger(Duration::from_secs(60)),
                    taking_over,
                    claim,
                )
                .unwrap()
            })
        };
        let hold = |name: &'static str, claim: Claim| hold_waiting(name, claim, TAKING_OVER);
        let program_of = |file: u64| Program {
            file,
            modified_ns: u128::from(file).saturating_mul(1_000),
        };
        let held_by = |name: &str| {
            let started = Instant::now();
            loop {
                if let Ok(mut stream) = UnixStream::connect(place.socket())
                    && initialize(&mut stream).contains(&format!("held by the {name} Tilo"))
                {
                    return;
                }
                assert!(
                    started.elapsed() < TAKING_OVER,
                    "the {name} holder did not hold the device"
                );
                std::thread::sleep(POLL);
            }
        };

        let agent = hold("agent's", agents(program));
        held_by("agent's");
        let service = hold("service's", services(program));
        agent.join().unwrap();
        held_by("service's");
        assert_eq!(
            place.ask_to_step_down(agents(program)),
            Some(false),
            "an agent's holder never takes the device from the service's"
        );

        // A newer program's holder takes over from the service's...
        let newer_agent = hold("newer agent's", agents(newer));
        service.join().unwrap();
        held_by("newer agent's");
        // ...and the service's holder started again waits for it rather than ending, even past
        // the wait a holder of an agent's gives up after.
        let service = hold_waiting("service's", services(program), Duration::ZERO);
        std::thread::sleep(Duration::from_secs(2));
        assert!(
            !service.is_finished(),
            "the service's holder waits for the device"
        );
        assert_eq!(place.ask_to_step_down(agents(program_of(3))), Some(true));
        newer_agent.join().unwrap();
        held_by("service's");
        assert_eq!(place.ask_to_step_down(agents(program_of(4))), Some(true));
        service.join().unwrap();
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
    /// every other run: a holder told to stop whose workers never read that it was.
    #[test]
    fn a_child_is_stopped_with_busy_workers() {
        if std::env::var_os(CHILD_STOPPED).is_none() {
            return;
        }
        let _ended = ending(CHILD_STOPPED_WITHIN).unwrap();
        // The signal thread's handlers are in place well before this: a SIGTERM before them
        // would end the child by the signal, which the parent tells apart.
        std::thread::sleep(Duration::from_millis(500));
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TERM)
            .unwrap();
        // Workers that never take the stop up: the receiver is never read.
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

    /// A computer's login made again makes another device, with a holder of its own place at the
    /// server; one computer still runs one holder, so the two never hold a second endpoint under
    /// a device key at the same time.
    #[test]
    fn a_device_directory_has_one_holder_however_its_login_changed() {
        let dir = Path::new("/state/tilo/device");
        let first = DaemonPlace::at(dir, dir, &[1; 32]);
        let second = DaemonPlace::at(dir, dir, &[2; 32]);
        assert_eq!(
            first.lock(),
            second.lock(),
            "one holder per device directory"
        );
        assert_ne!(
            first.socket(),
            second.socket(),
            "each login's connections reach its own holder"
        );
        assert_ne!(first.log(), second.log());
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
        let place = DaemonPlace::at(dir.path(), dir.path(), &[3; 32]);
        let lock = place.lock().to_path_buf();
        let held = take_lock(&place, Duration::ZERO, agents(Program::running())).unwrap();
        assert!(held.is_some());
        assert!(
            take_lock(&place, Duration::ZERO, agents(Program::running()))
                .unwrap()
                .is_none(),
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
            take_lock(&place, Duration::ZERO, agents(Program::running()))
                .unwrap()
                .is_some(),
            "the lock of a process that ended wedges the next holder"
        );
    }

    /// A holder that is there is seen from its lock alone, without asking it anything: that is
    /// the fact a command starts, waits or hands over on, and a busy holder still gives it.
    #[test]
    fn a_holders_liveness_is_seen_from_its_lock() {
        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path(), &[5; 32]);
        assert!(!place.alive(), "nothing holds the device");
        let held = take_lock(&place, Duration::ZERO, agents(Program::running()))
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
            .block_on(super::super::peer::Peer::open(
                dir.path(),
                dir.path(),
                false,
            ))
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

    /// A device that cannot enter, saying how many times it was asked: the
    /// holder asks it again for each connection, so the count says which
    /// holder answered.
    struct Counting(AtomicUsize);

    impl Enter for Arc<Counting> {
        async fn enter(&self) -> Result<Arc<super::super::Session>, String> {
            let asked = self.0.fetch_add(1, Ordering::SeqCst).saturating_add(1);
            Err(format!("asked {asked}"))
        }
    }

    fn initialize(stream: &mut UnixStream) -> String {
        stream
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .unwrap();
        let mut answer = String::new();
        std::io::BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut answer)
            .unwrap();
        let answer: Value = serde_json::from_str(&answer).unwrap();
        answer["result"]["instructions"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Two agents' `tilo mcp` on one computer are two connections to the one
    /// holder of the device's session, not two sessions that cut each other
    /// off; a second holder started meanwhile leaves the first in place.
    #[test]
    fn every_connection_of_a_device_reaches_its_one_holder() {
        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path(), &[7; 32]);
        let counting = Arc::new(Counting(AtomicUsize::new(0)));
        let shared = Arc::new(Shared::new(Arc::clone(&counting), Err("unused".to_owned())));
        let linger = Duration::from_millis(300);
        let holding = {
            let (place, shared) = (place.clone(), Arc::clone(&shared));
            std::thread::spawn(move || {
                hold_for(
                    &place,
                    shared,
                    Holding::Linger(linger),
                    Duration::ZERO,
                    agents(Program::running()),
                )
                .unwrap()
            })
        };
        let mut first = loop {
            if let Ok(stream) = UnixStream::connect(place.socket()) {
                break stream;
            }
            std::thread::sleep(POLL);
        };
        let mut second = UnixStream::connect(place.socket()).unwrap();
        // The holder enters as it starts, before anyone asks.
        while counting.0.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(POLL);
        }
        let said = [initialize(&mut first), initialize(&mut second)];
        // Both connections are the same holder's, and it asks the device to enter once: a call
        // never waits on an entry retry, so both hear why at once.
        assert_eq!(said[0], said[1]);
        assert!(
            said[0].contains("Tilo keeps trying in the background"),
            "{}",
            said[0]
        );
        assert_eq!(
            counting.0.load(Ordering::SeqCst),
            1,
            "the device is asked to enter once, not once per connection"
        );
        // A second holder (another agent's `tilo mcp` starting one at the same moment) finds
        // the device held and ends, leaving the first listening.
        let second_holder = hold_for(
            &place,
            Arc::clone(&shared),
            Holding::Linger(linger),
            Duration::ZERO,
            agents(Program::running()),
        );
        assert!(second_holder.is_ok());
        let mut third = UnixStream::connect(place.socket()).unwrap();
        assert_eq!(initialize(&mut third), said[0]);
        assert_eq!(counting.0.load(Ordering::SeqCst), 1);
        drop((first, second, third));
        holding.join().unwrap();
        assert!(
            !place.socket().exists(),
            "a holder whose connections all ended stops listening"
        );
    }

    /// An entry that answers which holder asked it: what tells two holders' answers apart.
    /// What keeps both of the runtime's workers in a step that does not let go of them for
    /// [`Self::FOR`], each time the device is entered: a step stuck where it should not be.
    struct Wedging;

    impl Wedging {
        const FOR: Duration = Duration::from_secs(12);
    }

    impl Enter for Wedging {
        async fn enter(&self) -> Result<Arc<super::super::Session>, String> {
            // Spawned from outside the runtime, so the other worker takes it at once: one spawned
            // from this worker would wait in its own queue for this step to end.
            let runtime = tokio::runtime::Handle::current();
            std::thread::spawn(move || {
                runtime.spawn(async { std::thread::sleep(Wedging::FOR) });
            })
            .join()
            .unwrap();
            std::thread::sleep(Wedging::FOR);
            Err("wedged".to_owned())
        }
    }

    /// `tilo status` is answered while every worker of the holder's runtime is stuck in a step:
    /// within seconds, with what the peer last said and how long ago, rather than not at all,
    /// which the command reads as a holder busy with a large change and waits on.
    #[test]
    fn a_status_is_answered_while_the_workers_are_stuck() {
        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path(), &[7; 32]);
        let holding = {
            let place = place.clone();
            std::thread::spawn(move || {
                let shared = Arc::new(Shared::new(Wedging, Err("unused".to_owned())));
                hold_for(
                    &place,
                    shared,
                    Holding::Linger(Duration::from_millis(100)),
                    TAKING_OVER,
                    agents(Program::running()),
                )
                .unwrap()
            })
        };
        let started = Instant::now();
        let mut agent = loop {
            if let Ok(stream) = UnixStream::connect(place.socket()) {
                break stream;
            }
            assert!(started.elapsed() < TAKING_OVER, "no holder listened");
            std::thread::sleep(POLL);
        };
        // An agent's first call enters the workspace, which takes both workers.
        agent
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));

        let asked = Instant::now();
        let mut status = UnixStream::connect(place.socket()).unwrap();
        status
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            status,
            "{}",
            json!({"jsonrpc": "2.0", "id": 1, "method": tilo_api::daemon::PEER_STATUS})
        )
        .unwrap();
        let mut answer = String::new();
        let read = std::io::BufReader::new(status).read_line(&mut answer);
        let took = asked.elapsed();
        assert!(
            read.is_ok_and(|read| read > 0),
            "no status answered in {took:?} while the workers were stuck"
        );
        assert!(took < Duration::from_secs(4), "the status took {took:?}");
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer["result"]["pid"], json!(std::process::id()));
        assert!(
            answer["result"]["answered_ago_s"].is_u64(),
            "it says the answer is not a fresh one: {answer}"
        );

        drop(agent);
        holding.join().unwrap();
    }

    struct Named(&'static str);

    impl Enter for Named {
        async fn enter(&self) -> Result<Arc<super::super::Session>, String> {
            Err(format!("held by the {} Tilo", self.0))
        }
    }

    /// Tilo updated while a holder of the old program still holds the device (one a CLI command
    /// started while the service was stopped): the holder the new program starts asks it to step
    /// down, and holds the device from then on, so the next command is the new program's to
    /// answer. A holder of the same program never takes over: two agents starting one at once keep
    /// the first.
    #[test]
    fn a_holder_of_a_newer_tilo_takes_over_from_one_of_an_older() {
        let dir = tempfile::tempdir().unwrap();
        let place = DaemonPlace::at(dir.path(), dir.path(), &[7; 32]);
        let older = Program {
            file: 1,
            modified_ns: 1_000,
        };
        let newer = Program {
            file: 2,
            modified_ns: 2_000,
        };
        assert!(newer.replaces(&older));
        assert!(!older.replaces(&newer), "an older program never takes over");
        assert!(!older.replaces(&older), "nor does the same one");

        let hold = |name: &'static str, program: Program| {
            let place = place.clone();
            std::thread::spawn(move || {
                let shared = Arc::new(Shared::new(Named(name), Err("unused".to_owned())));
                hold_for(
                    &place,
                    shared,
                    Holding::Linger(Duration::from_secs(60)),
                    TAKING_OVER,
                    agents(program),
                )
                .unwrap()
            })
        };
        let first = hold("older", older);
        let mut asking = loop {
            if let Ok(stream) = UnixStream::connect(place.socket()) {
                break stream;
            }
            std::thread::sleep(POLL);
        };
        assert!(initialize(&mut asking).contains("held by the older Tilo"));
        drop(asking);

        // A holder of the same program finds the device held, and ends at once.
        let same = std::thread::spawn({
            let place = place.clone();
            move || {
                let shared = Arc::new(Shared::new(Named("same"), Err("unused".to_owned())));
                hold_for(
                    &place,
                    shared,
                    Holding::Linger(Duration::from_secs(60)),
                    Duration::ZERO,
                    agents(older),
                )
                .unwrap()
            }
        });
        same.join().unwrap();

        let started = Instant::now();
        let second = hold("newer", newer);
        first.join().unwrap();
        let mut asking = loop {
            if let Ok(mut stream) = UnixStream::connect(place.socket()) {
                let said = initialize(&mut stream);
                if said.contains("held by the newer Tilo") {
                    break stream;
                }
            }
            assert!(
                started.elapsed() < TAKING_OVER,
                "the newer Tilo's holder did not take over"
            );
            std::thread::sleep(POLL);
        };
        assert!(initialize(&mut asking).contains("held by the newer Tilo"));
        drop(asking);
        // Its connections gone, it lingers no longer once asked by an even newer one.
        let newest = Program {
            file: 3,
            modified_ns: 3_000,
        };
        assert_eq!(place.ask_to_step_down(agents(newest)), Some(true));
        second.join().unwrap();
    }

    /// A certificate `root` gives `key`, as a login's is.
    fn certified(
        root: &ed25519_dalek::SigningKey,
        key: &ed25519_dalek::SigningKey,
    ) -> tilo_user_identity::DeviceCert {
        let now = tilo_clock::unix_now_ns();
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// On a logged-in computer, a workspace command (`tilo do`, `ls`, `read`, `workspace`) is a
    /// call to the one process with the device's session: every call reaches it, and none enters
    /// the workspace as the device on its own, which would cut that session off at the server. A
    /// key someone handed over (`--key-file`) still acts as that key, in process, and never
    /// reaches that process.
    #[test]
    fn a_logged_in_computers_workspace_commands_are_calls_to_its_one_session() {
        let state = tempfile::tempdir().unwrap();
        let runtime_dir = tempfile::tempdir().unwrap();
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let mut env = tilo_test_env::ScopedEnv::new();
        let key = log_in(&mut env, state.path(), &root);
        env.set("XDG_RUNTIME_DIR", runtime_dir.path())
            .remove(tilo_api::client::KEY_ENV)
            .remove(tilo_api::client::DESKTOP_ENV);
        let place = DaemonPlace::of(
            tilo_device_identity::login::Place::here(tilo_api::client::device_keys())
                .unwrap()
                .dir(),
            &key.verifying_key().to_bytes(),
        )
        .unwrap();
        assert_eq!(
            tilo_device_identity::login::held_api(tilo_api::client::device_keys()).unwrap(),
            Some(place.api().to_path_buf()),
            "the login's commands reach where its holder serves"
        );
        let counting = Arc::new(Counting(AtomicUsize::new(0)));
        let shared = Arc::new(Shared::new(Arc::clone(&counting), Err("unused".to_owned())));
        let holding = {
            let (place, shared) = (place.clone(), Arc::clone(&shared));
            std::thread::spawn(move || {
                hold_for(
                    &place,
                    shared,
                    Holding::Linger(Duration::from_millis(300)),
                    Duration::ZERO,
                    agents(Program::running()),
                )
                .unwrap()
            })
        };
        // An agent's connection keeps the holder up meanwhile.
        let agent = loop {
            if let Ok(stream) = UnixStream::connect(place.socket()) {
                break stream;
            }
            std::thread::sleep(POLL);
        };
        while counting.0.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(POLL);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let as_login = || {
            tilo_api::client::Client::open(tilo_api::client::Reach {
                api: None,
                key_file: None,
                json: false,
            })
            .unwrap()
        };
        for _ in 0..3 {
            let refused = runtime
                .block_on(as_login().workspaces())
                .expect_err("this device is not in the workspace");
            let refusal = tilo_api::client::Refusal::of(&refused).expect("the holder's answer");
            assert_eq!(refusal.code(), "not_in_workspace", "{refused}");
            assert!(refused.to_string().contains("asked 1"), "{refused}");
        }
        // A key file naming the device's own key is the device: it goes to the holder too.
        let own = state.path().join("own.json");
        std::fs::write(
            &own,
            serde_json::to_vec(&tilo_api::client::login_key(None).unwrap().unwrap()).unwrap(),
        )
        .unwrap();
        let refused = runtime
            .block_on(
                tilo_api::client::Client::open(tilo_api::client::Reach {
                    api: None,
                    key_file: Some(&own),
                    json: false,
                })
                .unwrap()
                .workspaces(),
            )
            .expect_err("this device is not in the workspace");
        assert_eq!(
            tilo_api::client::Refusal::of(&refused).map(tilo_api::client::Refusal::code),
            Some("not_in_workspace"),
            "{refused}"
        );
        assert_eq!(
            counting.0.load(Ordering::SeqCst),
            1,
            "the device entered once, for the holder; no command entered on its own"
        );

        assert!(
            a_key_file_asks_its_own_server(state.path(), &root, &runtime),
            "a key handed over acts as itself, against its own server, in process"
        );
        assert_eq!(counting.0.load(Ordering::SeqCst), 1, "the holder was not");

        drop(agent);
        holding.join().unwrap();
        assert!(!place.api().exists(), "a holder that ended serves no API");
        drop(env);
    }

    /// Whether a call with a key file `root` certified (`--key-file`) dials the host it names.
    fn a_key_file_asks_its_own_server(
        dir: &Path,
        root: &ed25519_dalek::SigningKey,
        runtime: &tokio::runtime::Runtime,
    ) -> bool {
        let script = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        runtime.block_on(async {
            let server = test_server::direct_endpoint().await;
            let host = ed25519_dalek::SigningKey::from_bytes(&server.secret_key().to_bytes());
            let origin = "https://eu1.tilo.test";
            let key_file = dir.join("script.json");
            std::fs::write(
                &key_file,
                serde_json::to_vec(
                    &tilo_api::direct::KeyFile::new(
                        origin,
                        None,
                        "Nightly build",
                        &script,
                        &certified(root, &script),
                    )
                    .reaching_at(tilo_server_protocol::web::ServerInfo {
                        server_cert: certified(root, &host),
                        address: Some(tilo_server_protocol::web::ServerAddress {
                            endpoint_addr: server.addr(),
                            relay_url: None,
                            web: None,
                        }),
                        origin: Some(origin.to_owned()),
                    }),
                )
                .unwrap(),
            )
            .unwrap();
            let as_key = tilo_api::client::Client::open(tilo_api::client::Reach {
                api: None,
                key_file: Some(&key_file),
                json: false,
            })
            .unwrap();
            // The server takes no session, so the call keeps asking: what counts is that it
            // dialled there.
            tilo_test_env::within("the key's call dials its server or ends", async {
                tokio::select! {
                    dialled = server.accept() => dialled.is_some(),
                    _ = as_key.workspaces() => false,
                }
            })
            .await
        })
    }

    /// This computer logged in under `dir`, as `env` names it, as a device `root` certified: its
    /// key.
    fn log_in(
        env: &mut tilo_test_env::ScopedEnv,
        dir: &Path,
        root: &ed25519_dalek::SigningKey,
    ) -> ed25519_dalek::SigningKey {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let cert = certified(root, &key);
        let login = tilo_device_identity::login::Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".into(),
            workspace_name: Some("Studio".into()),
            agent: "Claude Code".into(),
            server_cert: cert.clone(),
            cert,
            server_address: None,
        };
        env.set("XDG_STATE_HOME", dir.join("state"))
            .set("TILO_IDENTITY_ROOT", dir.join("identity"));
        let place =
            tilo_device_identity::login::Place::here(tilo_api::client::device_keys()).unwrap();
        tilo_device_identity::persist::adopt_device_key(place.keys(), &key.to_bytes()).unwrap();
        tilo_device_identity::login::store(&place, &login).unwrap();
        key
    }

    tilo_test_env::scenario! {
        fn a_devices_holder_listens_at_a_place_of_its_own;

        phase each_device_has_its_own_holder;
        // A device logged in anew is another device: its connections reach a
        // holder of their own.
        {
            let dir = Path::new("/state/tilo/device");
            assert_ne!(
                DaemonPlace::at(dir, dir, &[1; 32]).socket(),
                DaemonPlace::at(dir, dir, &[2; 32]).socket()
            );
        }

        phase a_deep_home_still_listens_in_a_private_directory;
        // A home however deep still has a socket short enough to listen at:
        // its holder listens in the account's own directory, which nobody else
        // may enter.
        {
            use std::os::unix::fs::PermissionsExt as _;
            let home = tempfile::tempdir().unwrap();
            let deep = home.path().join("a".repeat(120)).join("state/tilo/device");
            std::fs::create_dir_all(&deep).unwrap();
            let place = DaemonPlace::of(&deep, &[3; 32]).unwrap();
            assert!(
                place.socket().as_os_str().len() < 100,
                "{}",
                place.socket().display()
            );
            let sockets = place.socket().parent().unwrap();
            let mode = std::fs::metadata(sockets).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{} is {mode:o}", sockets.display());
            assert!(place.lock().starts_with(&deep));
        }
    }
}
