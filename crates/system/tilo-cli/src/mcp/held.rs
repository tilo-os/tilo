//! The one process on this computer holding the device's session with the
//! workspace, and the `tilo mcp` connections to it.
//!
//! The host keeps one session per device, so two processes entering as the
//! same device cut each other off, over and over: each agent's calls then
//! fail as "not connected to the host" or go unanswered. Every `tilo mcp`
//! therefore reaches the holder over a socket in the device's directory
//! ([`reach`], starting `tilo mcp --hold` when none listens) and relays its
//! agent's stdio to it ([`relay`]). The holder ([`hold`]) serves each
//! connection as its own agent run over the one session, and leaves the
//! workspace a while after its last connection ended.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};

use tilo_api::client::device;

use super::{Connection, Enter, Shared, answer};

/// How long a connection waits for a holder to listen.
const REACHING: Duration = Duration::from_secs(10);
/// How long a connection waits for the user service it asked to start a holder that listens,
/// before it starts one itself.
const SERVICE_STARTING: Duration = Duration::from_secs(5);
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
/// How often a connection looks for a holder that has not started listening.
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
const STOPPING: Duration = Duration::from_secs(8);
/// How often a holder looks whether the login it serves is still the one under its device
/// directory: a login made again makes another device, and the holder it replaced steps down so
/// one computer runs one holder.
const LOGIN_LOOK: Duration = Duration::from_secs(3);

/// Where one device's holder listens, the lock that keeps it one, and its log.
#[derive(Clone)]
pub(crate) struct Place {
    socket: PathBuf,
    /// Where it serves Tilo's API as the device, over the one session: what
    /// `tilo do`, `ls`, `read` and the other workspace commands of this login
    /// reach ([`device::held_api`]).
    api: PathBuf,
    /// One per device directory, not per key: a computer has one holder, and a login made again
    /// under a new key waits for the holder of the old one to step down instead of running
    /// beside it. Two live endpoints under one device's keys take each other's place at the
    /// relay over and over, and neither serves.
    lock: PathBuf,
    log: PathBuf,
    /// The first bytes of the device key the listener serves, as its socket and log are named.
    key: [u8; 8],
}

impl Place {
    /// The holder of the device whose public key is `key`, logged in under
    /// `dir`: a new login is a new device, with a holder of its own. Its
    /// sockets are in this account's own short directory
    /// ([`device::holder_dir`]).
    pub(crate) fn of(dir: &Path, key: &[u8; 32]) -> Result<Self, String> {
        Ok(Self::at(&device::holder_dir(dir)?, dir, key))
    }

    /// The holder of `key`'s device under `dir`, listening in `sockets`.
    fn at(sockets: &Path, dir: &Path, key: &[u8; 32]) -> Self {
        let mut first = [0; 8];
        first.copy_from_slice(&key[..8]);
        let name = device::holder_name(key);
        Self {
            socket: sockets.join(format!("{name}.sock")),
            api: device::holder_api_socket(sockets, &name),
            lock: dir.join("holder.lock"),
            log: dir.join(format!("{name}.log")),
            key: first,
        }
    }

    /// Whether the login under this device's directory is still `key`'s: a login made again
    /// makes another device, and the holder of the one it replaced ends so that one computer
    /// keeps one holder and one live connection.
    fn serves_current_login(&self) -> bool {
        let Some(dir) = self.lock.parent() else {
            return true;
        };
        match crate::login::load(dir) {
            Ok(Some((key, _))) => key.verifying_key().to_bytes()[..8] == self.key,
            // A login mid-write reads as none; the next look answers for it.
            _ => true,
        }
    }
}

/// A connection to the device's holder when one listens, starting none.
pub(crate) fn connect(place: &Place) -> Option<UnixStream> {
    tilo_files::native::socket::connect(&place.socket).ok()
}

/// Start the device's holder, the peer, unless one listens already: what
/// `tilo login` and `tilo mount` leave running.
///
/// # Errors
/// It did not start, or did not listen in time.
pub(crate) fn ensure(place: &Place) -> Result<(), String> {
    reach(place).map(drop)
}

/// A connection to the device's holder, starting it when none listens.
pub(crate) fn reach(place: &Place) -> Result<UnixStream, String> {
    reach_starting(place, |via_service| start_holder(place, via_service))
}

/// How a holder was started.
enum Started {
    /// As a process of this one's, waited for and started again if it ended without listening.
    Process(std::process::Child),
    /// As the user service, which keeps it from then on: asked at this instant.
    Service(Instant),
}

/// [`reach`], with `start` starting a holder (through the user service where it may); what it
/// started is waited for, and started again if it ended without listening (one that was leaving
/// held the lock). A service that started no holder listening within half the wait is not asked
/// again: this command starts one itself.
fn reach_starting(
    place: &Place,
    start: impl Fn(bool) -> std::io::Result<Started>,
) -> Result<UnixStream, String> {
    let deadline = Instant::now()
        .checked_add(REACHING)
        .unwrap_or_else(Instant::now);
    let mut started: Option<Started> = None;
    let mut via_service = true;
    loop {
        match tilo_files::native::socket::connect(&place.socket) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                // Where it listens and what it logged: for whoever keeps this
                // computer's logs, not for the agent's sentence.
                tracing::debug!(
                    socket = %place.socket.display(),
                    log = %place.log.display(),
                    %error,
                    "Tilo's background sync did not answer"
                );
                return Err(
                    "Tilo's background sync isn't answering on this computer. Run `tilo status` \
                     to see how it stands, then try again."
                        .to_owned(),
                );
            }
            Err(_) => {}
        }
        let ended = match started.as_mut() {
            None => true,
            Some(Started::Process(child)) => child.try_wait().ok().flatten().is_some(),
            Some(Started::Service(asked)) => {
                let given_up = asked.elapsed() >= SERVICE_STARTING;
                via_service &= !given_up;
                given_up
            }
        };
        if ended {
            started = Some(start(via_service).map_err(|error| {
                tracing::debug!(%error, "Tilo's background sync did not start");
                "Tilo's background sync didn't start on this computer. Run `tilo status` to see \
                 how it stands, then try again."
                    .to_owned()
            })?);
        }
        std::thread::sleep(POLL);
    }
}

/// Start the peer (`tilo mcp --hold`): as the user service where it is installed, so the one
/// holder of the device lives where the person's login keeps it, never inside the agent or
/// command that happened to need it first (and ends with it). Elsewhere on its own: in a process
/// group of its own, so the agent that ends this connection does not end it with the others',
/// and adding what it says to its log.
fn start_holder(place: &Place, via_service: bool) -> std::io::Result<Started> {
    use std::os::unix::process::CommandExt as _;
    if via_service && crate::service::start_installed() {
        return Ok(Started::Service(Instant::now()));
    }
    // Added to, never emptied: a holder started while another holds the device ends at once, and
    // emptying the log the one holding writes to would lose what it said.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&place.log)?;
    std::process::Command::new(std::env::current_exe()?)
        .args(["mcp", "--hold"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()
        .map(Started::Process)
}

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

/// Serve this connection's stdio alone: with no login to share, or when no
/// holder could be reached.
pub(crate) fn alone<E: Enter>(shared: &Arc<Shared<E>>) -> Result<ExitCode> {
    let runtime = bridge_runtime()?;
    runtime.block_on(async {
        // Enter the workspace while the agent starts, not at its first call.
        let entering = Arc::clone(shared);
        tokio::spawn(async move {
            let _ = entering.session(false, super::Ask::Sync).await;
        });
        // This process is the agent's own: what its harness calls the session, it says in the
        // initialize it passes on.
        let session_name = tilo_mcp::session_api::session_name();
        converse(
            Arc::clone(shared),
            tokio::io::stdin(),
            tokio::io::stdout(),
            session_name.as_deref(),
        )
        .await;
        // The connection ended (its input closed): the agent leaves the
        // workspace now, while its session can still say so.
        shared.leaving().await;
    });
    // As for the holder: a step on a folder's tree is left behind, not waited for.
    runtime.shutdown_timeout(SHUTDOWN);
    Ok(ExitCode::SUCCESS)
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
    let mut answering = tokio::task::JoinSet::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let line = naming(line, session_name);
        let (connection, write) = (Arc::clone(&connection), Arc::clone(&write));
        answering.spawn(async move {
            if let Some(answer) = answer(&connection, &line).await {
                let mut write = write.lock().await;
                let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
                let _ = write.flush().await;
                drop(write);
            }
        });
        while answering.try_join_next().is_some() {}
    }
    while answering.join_next().await.is_some() {}
}

/// This computer as the person's peer: the device's session held for every
/// connection at `place` for as long as this process runs, and the person's
/// workspaces' files shown below `home` over it ([`crate::peer`]). Another
/// holder already there keeps it: this one ends at once.
pub(crate) fn hold<E: Enter>(
    place: &Place,
    shared: Arc<Shared<E>>,
    home: PathBuf,
) -> Result<ExitCode> {
    let device_dir = place
        .lock
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    hold_for(
        place,
        shared,
        Holding::Peer {
            home,
            state: device_dir,
        },
        TAKING_OVER,
        Claim {
            program: Program::running(),
            service: crate::service::running_as_service(),
        },
    )
}

/// What a holder claims the device as: its program, and whether the user service started it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Claim {
    pub(crate) program: Program,
    /// Started by the user service, which keeps it running for as long as the person is logged
    /// in. A holder an agent or a command started lives in their process tree and ends with it.
    #[serde(default)]
    pub(crate) service: bool,
}

impl Claim {
    /// Whether a holder claiming this takes the device over from one claiming `holding`: it is
    /// a newer program, or it is the user service's and `holding` is not, of a program no newer.
    /// The service's holder is the one that lasts: one an agent started first hands the device
    /// (its session and its folders) over to it, rather than the service ending beside it.
    pub(crate) fn takes_over(&self, holding: &Self) -> bool {
        self.program.replaces(&holding.program)
            || (self.service && !holding.service && !holding.program.replaces(&self.program))
    }
}

/// The program a holder is, as the file it was started from: Tilo replaced by a newer build is
/// another file, and a holder still running the one it replaced lets a holder of the new one take
/// over. Two holders of one program never do: two agents starting one at the same moment keep the
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Program {
    /// The file's inode: a new build put in place is a new file, whatever its name.
    file: u64,
    /// When it was written, in nanoseconds since the epoch.
    modified_ns: u128,
}

impl Program {
    /// The program this process was started from. A program file replaced since it started is still read as
    /// the one it started from (`/proc/self/exe` names it, deleted or not).
    pub(crate) fn running() -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata("/proc/self/exe")
            .or_else(|_| std::env::current_exe().and_then(std::fs::metadata));
        metadata.map_or(
            Self {
                file: 0,
                modified_ns: 0,
            },
            |metadata| Self {
                file: metadata.ino(),
                modified_ns: metadata
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_nanos()),
            },
        )
    }

    /// Whether a holder running this program takes over from one running `holding`: it is
    /// another file, written no earlier.
    pub(crate) fn replaces(self, holding: &Self) -> bool {
        self.file != holding.file && self.modified_ns >= holding.modified_ns
    }
}

/// Ask the holder listening at `place` to let a holder claiming `claim` hold the device: whether
/// it steps down, as it does when the claim takes over from its own ([`Claim::takes_over`]);
/// `None` when none answered.
fn ask_to_step_down(place: &Place, claim: Claim) -> Option<bool> {
    use std::io::{BufRead as _, Write as _};
    let mut stream = connect(place)?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let asked = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": super::STEP_DOWN,
        "params": {"program": claim.program, "service": claim.service},
    });
    writeln!(stream, "{asked}").ok()?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str::<serde_json::Value>(&line)
        .ok()
        .and_then(|answer| answer.get("result")?.get("stepping_down")?.as_bool())
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
fn hold_for<E: Enter>(
    place: &Place,
    shared: Arc<Shared<E>>,
    holding: Holding,
    taking_over: Duration,
    claim: Claim,
) -> Result<ExitCode> {
    let Some(_held) = take_lock(place, taking_over, claim)? else {
        return Ok(ExitCode::SUCCESS);
    };
    trim_log();
    let _ = shared.held_as.set(claim);
    let runtime = bridge_runtime()?;
    let served = runtime.block_on(serve_held(place, shared, holding));
    // A step that walks a whole folder tree — a merge of a tree record, a save of one — runs on
    // its thread to its end and cannot be interrupted. The process ends now instead of waiting
    // for it: the socket is gone, the folders were unmounted (or given up on), and the trees
    // were kept, above.
    runtime.shutdown_timeout(SHUTDOWN);
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
    served?;
    Ok(ExitCode::SUCCESS)
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
    place: &Place,
    taking_over: Duration,
    claim: Claim,
) -> Result<Option<advisory_lock::FileLock>> {
    use advisory_lock::{FileLock, LockMode, TryLockError};
    let path = &place.lock;
    let mut deadline = Instant::now()
        .checked_add(taking_over)
        .unwrap_or_else(Instant::now);
    // When the holder there last answered: it is asked until it does, once (again and again, by
    // the service's holder waiting for it).
    let mut answered_at: Option<Instant> = None;
    let mut waiting = false;
    loop {
        match FileLock::try_open(path, LockMode::Exclusive) {
            Ok(lock) => return Ok(Some(lock)),
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
            ask_to_step_down(place, claim)
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
    place: &Place,
    shared: Arc<Shared<E>>,
    holding: Holding,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    // A socket left by a holder that ended without removing it.
    let _ = std::fs::remove_file(&place.socket);
    let listener = tilo_files::native::socket::bind(&place.socket)
        .with_context(|| format!("listen at {}", place.socket.display()))?;
    std::fs::set_permissions(&place.socket, std::fs::Permissions::from_mode(0o600))?;
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
    let mut ended = ending()?;
    let mut login = tokio::time::interval(LOGIN_LOOK);
    // Whether the login this holder serves was replaced: a holder on its way out then unmounts
    // nothing, since the holder of the new login shows the folders again.
    let mut replaced = false;
    loop {
        tokio::select! {
            () = idle(&mut live, linger) => break,
            _ = login.tick() => if !place.serves_current_login() {
                tracing::info!(
                    "this computer logged in again; the holder of the new login takes over"
                );
                replaced = true;
                break;
            },
            _ = ended.recv() => break,
            () = shared.stepping_down.notified() => {
                tracing::info!(
                    "a holder of a newer Tilo, or the user service's, takes over this device; \
                     this one steps down"
                );
                break;
            },
        }
    }
    // No connection from here on: the socket is shut down rather than left queuing clients that
    // would be told a holder is there by the kernel and wait in vain for it.
    accepting.stop();
    if !replaced && let Some(peer) = PEER.get() {
        // Unmounting and keeping the trees wait for the mounts' and trees' own steps, which
        // cannot be interrupted; both are bounded, so a stop ends inside the unit's timeout.
        peer.stop(STOPPING).await;
    }
    let _ = std::fs::remove_file(&place.socket);
    let _ = std::fs::remove_file(&place.api);
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
                        let Ok(()) = stream.set_nonblocking(true) else {
                            continue;
                        };
                        count.send_modify(|live| *live = live.saturating_add(1));
                        let (shared, count) = (Arc::clone(&shared), Arc::clone(&count));
                        runtime.spawn(async move {
                            let Ok(stream) = tokio::net::UnixStream::from_std(stream) else {
                                count.send_modify(|live| *live = live.saturating_sub(1));
                                return;
                            };
                            let (read, write) = stream.into_split();
                            // This process is no agent's: the name comes in each connection's
                            // own initialize, which the connection that reached it wrote.
                            converse(shared, read, write, None).await;
                            count.send_modify(|live| *live = live.saturating_sub(1));
                        });
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

/// Serve Tilo's API at `place` as this computer's device, over the one session it holds: every
/// workspace command of this login (`tilo do`, `ls`, `read`, `workspace`, `keys`) is a call here,
/// one round trip over a session already in the workspace, rather than a session of its own that
/// would cut this one off at the server. While the device is not in the workspace, each call
/// answers why.
fn serve_api<E: Enter>(place: &Place, shared: &Arc<Shared<E>>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::remove_file(&place.api);
    let listener = tilo_files::native::socket::bind(&place.api)
        .and_then(|listener| {
            listener.set_nonblocking(true)?;
            tokio::net::UnixListener::from_std(listener)
        })
        .with_context(|| format!("listen at {}", place.api.display()))?;
    std::fs::set_permissions(&place.api, std::fs::Permissions::from_mode(0o600))?;
    let shared = Arc::clone(shared);
    tokio::spawn(tilo_api::direct::serve_current(listener, move || {
        let shared = Arc::clone(&shared);
        async move {
            let session = shared.session(false, super::Ask::Agent).await?;
            session.api.clone().ok_or_else(|| {
                "this computer's login does not reach Tilo's API here; log in again".to_owned()
            })
        }
    }));
    Ok(())
}

/// What ends a holder from outside: the user service stopping it, or a
/// person's Ctrl-C.
///
/// The signals are waited for on a thread of their own, with a runtime of its own: the holder's
/// two workers may both be inside a step that walks a whole folder tree, and a stop that waited
/// for a free worker was heard only once that step ended.
fn ending() -> Result<tokio::sync::mpsc::UnboundedReceiver<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let (said, ended) = tokio::sync::mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("tilo-signals".to_owned())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let (signalled, mut heard) = tokio::sync::mpsc::channel(1);
                for kind in [
                    SignalKind::terminate(),
                    SignalKind::interrupt(),
                    SignalKind::hangup(),
                ] {
                    let Ok(mut signal) = signal(kind) else {
                        continue;
                    };
                    let signalled = signalled.clone();
                    tokio::spawn(async move {
                        if signal.recv().await.is_some() {
                            let _ = signalled.send(()).await;
                        }
                    });
                }
                drop(signalled);
                if heard.recv().await.is_some() {
                    let _ = said.send(());
                }
            });
        })?;
    Ok(ended)
}

/// The peer this holder runs, once it does.
pub(crate) static PEER: std::sync::OnceLock<Arc<crate::peer::Peer>> = std::sync::OnceLock::new();
/// Why this holder's device is not in the workspace, while it is not.
pub(crate) static NOT_IN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// How often the peer looks whether its session is still the device's.
const PEER_LOOK: Duration = Duration::from_secs(5);

/// Open the peer's stores at `state`, waiting while another holder still has them open: the one
/// that held the lock before this one let go of it before its process ended, and the databases it
/// held are locked until then. redb's lock is the kernel's, dropped with the process, so a holder
/// that died is never waited out.
async fn open_peer(state: &Path, home: &Path) -> anyhow::Result<crate::peer::Peer> {
    let deadline = Instant::now()
        .checked_add(OPENING)
        .unwrap_or_else(Instant::now);
    let mut waiting = false;
    loop {
        match crate::peer::Peer::open(state, home, true).await {
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
    let mut personal: Option<tilo_session_client::Events> = None;
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
    peer: &crate::peer::Peer,
    session: &Arc<super::Session>,
    shown: &mut Option<Arc<super::Session>>,
    personal: &mut Option<tilo_session_client::Events>,
    woken: &Arc<tokio::sync::Notify>,
) {
    if shown
        .as_ref()
        .is_none_or(|shown| !Arc::ptr_eq(shown, session))
    {
        peer.close().await;
        shown.replace(Arc::clone(session));
        peer.forget_personal().await;
        let events = session
            .device
            .session()
            .events_where(|event| matches!(event, tilo_session_client::Event::Personal(_)));
        let woken = Arc::clone(woken);
        session
            .device
            .session()
            .on_event(&events, Arc::new(move || woken.notify_one()));
        *personal = Some(events);
    }
    if let Some(events) = personal {
        for event in events.drain() {
            if let tilo_session_client::Event::Personal(list) = event {
                peer.keep_personal(&session.device, &list).await;
            }
        }
    }
    peer.reconcile(&session.device).await;
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

/// Relay this agent's stdio to the holder over `stream`. A holder that ends
/// first is reached again (a new one started): the agent's opening is said
/// to it again, and a request it left unanswered is answered as one to make
/// again, so the agent keeps its tools.
pub(crate) fn relay(stream: UnixStream, place: &Place) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(relaying(stream, place))
}

async fn relaying(first: UnixStream, place: &Place) -> Result<ExitCode> {
    let (lines_in, mut input) = tokio::sync::mpsc::channel::<String>(64);
    // What the agent's harness calls this session: this process is the agent's own child, so it
    // is the one that knows, and each holder reached says it in the initialize it passes on.
    let session_name = tilo_mcp::session_api::session_name();
    tokio::spawn(async move {
        let mut stdin = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = stdin.next_line().await {
            if lines_in.send(line).await.is_err() {
                return;
            }
        }
    });
    let mut stdout = tokio::io::stdout();
    let mut opening = Opening::default();
    let mut next = Some(first);
    let mut input_open = true;
    // Requests a holder ended before answering: a read is asked again of the
    // holder reached in its place, once; anything else is the agent's to
    // repeat, since it may have run.
    let mut carried: Vec<String> = Vec::new();
    let mut retried: std::collections::HashSet<String> = std::collections::HashSet::new();
    loop {
        let stream = if let Some(stream) = next.take() {
            stream
        } else {
            let place = place.clone();
            let reached = tokio::task::spawn_blocking(move || reach(&place)).await?;
            match reached {
                Ok(stream) => stream,
                Err(why) => anyhow::bail!("{why}"),
            }
        };
        stream.set_nonblocking(true)?;
        let (read, mut write) = tokio::net::UnixStream::from_std(stream)?.into_split();
        let mut answers = BufReader::new(read).lines();
        // Said again to a holder reached anew; its answer to the
        // initialize the agent already had is not the agent's to hear.
        let mut swallow = None;
        if let Some(said) = opening.again() {
            for line in &said.lines {
                write.write_all(format!("{line}\n").as_bytes()).await?;
            }
            swallow = said.answered;
        }
        let mut unanswered: HashMap<String, Unanswered> = HashMap::new();
        for line in std::mem::take(&mut carried) {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = message.get("id").cloned() else {
                continue;
            };
            retried.insert(id.to_string());
            write.write_all(format!("{line}\n").as_bytes()).await?;
            unanswered.insert(id.to_string(), Unanswered { id, line });
        }
        let ended = loop {
            tokio::select! {
                line = input.recv(), if input_open => if let Some(line) = line {
                    let line = naming(line, session_name.as_deref());
                    if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                        break true;
                    }
                    if let Some(id) = opening.note(&line) {
                        unanswered.insert(id.to_string(), Unanswered { id, line });
                    }
                } else {
                    input_open = false;
                    let _ = write.shutdown().await;
                },
                answer = answers.next_line() => match answer {
                    Ok(Some(line)) => {
                        let id = serde_json::from_str::<Value>(&line)
                            .ok()
                            .and_then(|answer| answer.get("id").cloned())
                            .map(|id| id.to_string());
                        if id.is_some() && id == swallow {
                            swallow = None;
                            continue;
                        }
                        if let Some(id) = &id {
                            unanswered.remove(id);
                        }
                        stdout.write_all(format!("{line}\n").as_bytes()).await?;
                        stdout.flush().await?;
                    }
                    _ => break input_open,
                },
            }
        };
        if !ended {
            // The agent's input closed and the holder answered everything.
            return Ok(ExitCode::SUCCESS);
        }
        for (key, request) in unanswered {
            if a_read(&request.line) && retried.insert(key) {
                carried.push(request.line);
                continue;
            }
            let again = json!({"jsonrpc": "2.0", "id": request.id, "error": {"code": -32000,
                "message": "Tilo lost its connection to the workspace before it answered. Run `tilo status` to see how it stands, then try again."}});
            stdout.write_all(format!("{again}\n").as_bytes()).await?;
        }
        stdout.flush().await?;
    }
}

/// A request the holder ended before answering.
struct Unanswered {
    id: Value,
    line: String,
}

/// `line` with the session this connection is written in, when it is the client's `initialize`
/// and the agent's harness named the session: what the workspace shows the agent by, in place of
/// the product's name. Any other line, and a session with no name, go as they are.
fn naming(line: String, session_name: Option<&str>) -> String {
    let Some(name) = session_name else {
        return line;
    };
    let asking = tilo_mcp::session_api::Asking {
        session_name: Some(name.to_owned()),
        ..Default::default()
    };
    asking
        .written_into(line.as_bytes())
        .and_then(|rewritten| String::from_utf8(rewritten).ok())
        .unwrap_or(line)
}

/// Whether a request only reads: asking it again runs nothing twice.
fn a_read(line: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    message.get("method").and_then(Value::as_str) == Some("tools/call")
        && matches!(
            message.pointer("/params/name").and_then(Value::as_str),
            Some("tilo_ls" | "tilo_read" | "tilo_find" | "tilo_status")
        )
}

/// What an agent said to open its connection, said again to a holder
/// reached anew.
#[derive(Default)]
struct Opening {
    initialize: Option<(String, Value)>,
    initialized: Option<String>,
}

/// The opening to say again, and the id of the answer to drop.
struct SaidAgain {
    lines: Vec<String>,
    answered: Option<String>,
}

impl Opening {
    /// Note `line` the agent sends: its opening is kept, and a request's id
    /// is answered (an answer awaited).
    fn note(&mut self, line: &str) -> Option<Value> {
        let message: Value = serde_json::from_str(line).ok()?;
        let method = message.get("method").and_then(Value::as_str)?;
        let id = message.get("id").cloned();
        match method {
            "initialize" => {
                self.initialize = id.clone().map(|id| (line.to_owned(), id));
            }
            "notifications/initialized" => self.initialized = Some(line.to_owned()),
            _ => {}
        }
        id
    }

    fn again(&self) -> Option<SaidAgain> {
        let (initialize, id) = self.initialize.as_ref()?;
        Some(SaidAgain {
            lines: std::iter::once(initialize.clone())
                .chain(self.initialized.clone())
                .collect(),
            answered: Some(id.to_string()),
        })
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
        let place = Place::at(dir.path(), dir.path(), &[7; 32]);
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
                if let Ok(mut stream) = UnixStream::connect(&place.socket)
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
            ask_to_step_down(&place, agents(program)),
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
        assert_eq!(ask_to_step_down(&place, agents(program_of(3))), Some(true));
        newer_agent.join().unwrap();
        held_by("service's");
        assert_eq!(ask_to_step_down(&place, agents(program_of(4))), Some(true));
        service.join().unwrap();
    }

    /// A stop ends by itself inside the user service's timeout: every wait it makes, added
    /// up, is shorter than what the unit gives it before the process is killed, so a holder
    /// stopped while a bulk write is under way exits rather than being killed.
    #[test]
    fn a_stop_ends_inside_the_services_timeout() {
        let waits = STOPPING + crate::mcp::CLOSING + SHUTDOWN + RELEASING + SHUTDOWN;
        assert!(
            waits < crate::service::STOP_TIMEOUT,
            "a stop's waits add up to {waits:?}, past the unit's {:?}",
            crate::service::STOP_TIMEOUT
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
        let first = Place::at(dir, dir, &[1; 32]);
        let second = Place::at(dir, dir, &[2; 32]);
        assert_eq!(first.lock, second.lock, "one holder per device directory");
        assert_ne!(
            first.socket, second.socket,
            "each login's connections reach its own holder"
        );
        assert_ne!(first.log, second.log);
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
        let place = Place::at(dir.path(), dir.path(), &[3; 32]);
        let lock = place.lock.clone();
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

    /// A holder starting while the last one still has the workspaces' files open waits for them
    /// and opens them then: it does not start without a peer.
    #[test]
    fn a_holder_waits_for_the_last_ones_files_to_close() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = bridge_runtime().unwrap();
        let first = runtime
            .block_on(crate::peer::Peer::open(dir.path(), dir.path(), false))
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

    #[test]
    fn only_reads_are_asked_again_by_themselves() {
        assert!(a_read(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_read","arguments":{}}}"#
        ));
        assert!(a_read(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tilo_status"}}"#
        ));
        assert!(!a_read(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"tilo_do","arguments":{}}}"#
        ));
        assert!(!a_read(
            r#"{"jsonrpc":"2.0","id":5,"method":"initialize","params":{}}"#
        ));
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
        let place = Place::at(dir.path(), dir.path(), &[7; 32]);
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
        let never = |_| -> std::io::Result<Started> {
            Err(std::io::Error::other("the holder runs already"))
        };
        let mut first = loop {
            if let Ok(stream) = UnixStream::connect(&place.socket) {
                break stream;
            }
            std::thread::sleep(POLL);
        };
        let mut second = reach_starting(&place, never).unwrap();
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
        let mut third = UnixStream::connect(&place.socket).unwrap();
        assert_eq!(initialize(&mut third), said[0]);
        assert_eq!(counting.0.load(Ordering::SeqCst), 1);
        drop((first, second, third));
        holding.join().unwrap();
        assert!(
            !place.socket.exists(),
            "a holder whose connections all ended stops listening"
        );
    }

    /// An entry that answers which holder asked it: what tells two holders' answers apart.
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
        let place = Place::at(dir.path(), dir.path(), &[7; 32]);
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
            if let Ok(stream) = UnixStream::connect(&place.socket) {
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
            if let Ok(mut stream) = UnixStream::connect(&place.socket) {
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
        assert_eq!(ask_to_step_down(&place, agents(newest)), Some(true));
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
    /// call to the one holder of the device's session: every call reaches it, and none enters the
    /// workspace as the device on its own, which would cut that session off at the server. A key
    /// someone handed over (`--key-file`) still acts as that key, in process, and never reaches
    /// the holder.
    #[test]
    fn a_logged_in_computers_workspace_commands_are_calls_to_its_one_holder() {
        let state = tempfile::tempdir().unwrap();
        let runtime_dir = tempfile::tempdir().unwrap();
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let key = log_in(state.path(), &root);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_DEVICE_DIR", state.path())
            .set("XDG_RUNTIME_DIR", runtime_dir.path())
            .remove(tilo_api::client::KEY_ENV)
            .remove(tilo_api::client::DESKTOP_ENV);
        let place = Place::of(state.path(), &key.verifying_key().to_bytes()).unwrap();
        assert_eq!(
            device::held_api().unwrap(),
            Some(place.api.clone()),
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
            if let Ok(stream) = UnixStream::connect(&place.socket) {
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
            crate::api::Client::open(crate::api::Reach {
                api: None,
                key_file: None,
                json: false,
            })
            .unwrap()
        };
        for _ in 0..3 {
            let refused = runtime
                .block_on(as_login().get("/v1/workspaces"))
                .expect_err("this device is not in the workspace");
            let refusal = crate::api::Refusal::of(&refused).expect("the holder's answer");
            assert_eq!(refusal.code(), "not_in_workspace", "{refused}");
            assert!(refused.to_string().contains("asked 1"), "{refused}");
        }
        // A key file naming the device's own key is the device: it goes to the holder too.
        let own = state.path().join("own.json");
        std::fs::write(
            &own,
            serde_json::to_vec(&crate::api::login_key(None).unwrap().unwrap()).unwrap(),
        )
        .unwrap();
        let refused = runtime
            .block_on(
                crate::api::Client::open(crate::api::Reach {
                    api: None,
                    key_file: Some(&own),
                    json: false,
                })
                .unwrap()
                .get("/v1/workspaces"),
            )
            .expect_err("this device is not in the workspace");
        assert_eq!(
            crate::api::Refusal::of(&refused).map(crate::api::Refusal::code),
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
        assert!(!place.api.exists(), "a holder that ended serves no API");
        drop(env);
    }

    /// Whether a call with a key file `root` certified (`--key-file`) asked the key's own server.
    fn a_key_file_asks_its_own_server(
        dir: &Path,
        root: &ed25519_dalek::SigningKey,
        runtime: &tokio::runtime::Runtime,
    ) -> bool {
        let server = test_http::Responder::handle(|_| Some(test_http::Reply::status(404)));
        let key_file = dir.join("script.json");
        let script = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        std::fs::write(
            &key_file,
            serde_json::to_vec(&tilo_api::direct::KeyFile::new(
                &server.url(),
                None,
                "Nightly build",
                &script,
                &certified(root, &script),
            ))
            .unwrap(),
        )
        .unwrap();
        let as_key = crate::api::Client::open(crate::api::Reach {
            api: None,
            key_file: Some(&key_file),
            json: false,
        })
        .unwrap();
        // Its server answers nothing useful, so the call keeps asking: it is not waited out.
        let _ = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), as_key.get("/v1/workspaces")).await
        });
        !server.drain_requests().is_empty()
    }

    /// This computer logged in under `dir` as a device `root` certified: its key.
    fn log_in(dir: &Path, root: &ed25519_dalek::SigningKey) -> ed25519_dalek::SigningKey {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let cert = certified(root, &key);
        let login = crate::login::Device {
            site: "https://tilo.test/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".into(),
            workspace_name: Some("Studio".into()),
            user: hex::encode(root.verifying_key().to_bytes()),
            agent: "Claude Code".into(),
            label: "Claude Code on build-box".into(),
            cert: hex::encode(postcard::to_allocvec(&cert).unwrap()),
            server_cert: "00".into(),
            server_address: None,
        };
        crate::login::store(dir, &key, &login).unwrap();
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
                Place::at(dir, dir, &[1; 32]).socket,
                Place::at(dir, dir, &[2; 32]).socket
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
            let place = Place::of(&deep, &[3; 32]).unwrap();
            assert!(
                place.socket.as_os_str().len() < 100,
                "{}",
                place.socket.display()
            );
            let sockets = place.socket.parent().unwrap();
            let mode = std::fs::metadata(sockets).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{} is {mode:o}", sockets.display());
            assert!(place.lock.starts_with(&deep));
        }
    }
}
