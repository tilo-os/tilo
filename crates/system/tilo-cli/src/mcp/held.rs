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

use super::{Connection, Enter, Shared, answer};

/// How long a connection waits for a holder to listen.
const REACHING: Duration = Duration::from_secs(10);
/// How long a holder starting waits for one that is leaving to let go.
const TAKING_OVER: Duration = Duration::from_secs(5);
/// How long the holder stays in the workspace after its last connection
/// ended, for the next one (an agent restarted, `tilo agent check` before
/// it) to find it there.
const LINGER: Duration = Duration::from_secs(10);
/// How often a connection looks for a holder that has not started listening.
const POLL: Duration = Duration::from_millis(50);

/// Where one device's holder listens, the lock that keeps it one, and its log.
#[derive(Clone)]
pub(crate) struct Place {
    socket: PathBuf,
    lock: PathBuf,
    log: PathBuf,
}

impl Place {
    /// The holder of the device whose public key is `key`, logged in under
    /// `dir`: a new login is a new device, with a holder of its own. Its
    /// socket is in this account's own short directory ([`socket_dir`]),
    /// since a socket's path is short (about a hundred bytes) and a home's
    /// state directory need not be.
    pub(crate) fn of(dir: &Path, key: &[u8; 32]) -> Result<Self, String> {
        Ok(Self::at(&socket_dir(dir)?, dir, key))
    }

    /// The holder of `key`'s device under `dir`, listening in `sockets`.
    fn at(sockets: &Path, dir: &Path, key: &[u8; 32]) -> Self {
        let name = format!("session-{}", hex::encode(&key[..8]));
        Self {
            socket: sockets.join(format!("{name}.sock")),
            lock: dir.join(format!("{name}.lock")),
            log: dir.join(format!("{name}.log")),
        }
    }
}

/// Where this account's holders listen: the session's runtime directory
/// when it has one, else one of this account's in the temporary directory,
/// made so that only this account enters it. One someone else made, or
/// others may enter, is refused: whoever reaches a holder acts as the
/// person.
fn socket_dir(device_dir: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let owner = std::fs::metadata(device_dir)
        .map_err(|error| format!("{} is not readable: {error}", device_dir.display()))?
        .uid();
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|runtime| runtime.is_absolute() && runtime.is_dir())
        .map_or_else(
            || std::env::temp_dir().join(format!("tilo-{owner}")),
            |runtime| runtime.join("tilo"),
        );
    if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&dir)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(format!("{} could not be made: {error}", dir.display()));
    }
    let made = std::fs::symlink_metadata(&dir)
        .map_err(|error| format!("{} is not readable: {error}", dir.display()))?;
    if !made.is_dir() || made.uid() != owner || made.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is not this account's own private directory",
            dir.display()
        ));
    }
    Ok(dir)
}

/// A connection to the device's holder, starting it when none listens.
pub(crate) fn reach(place: &Place) -> Result<UnixStream, String> {
    reach_starting(place, || start_holder(place))
}

/// [`reach`], with `start` starting a holder; what it started is waited for,
/// and started again if it ended without listening (one that was leaving
/// held the lock).
fn reach_starting(
    place: &Place,
    start: impl Fn() -> std::io::Result<std::process::Child>,
) -> Result<UnixStream, String> {
    let deadline = Instant::now() + REACHING;
    let mut started: Option<std::process::Child> = None;
    loop {
        match UnixStream::connect(&place.socket) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                return Err(format!(
                    "no process holds this computer's session at {} ({error}); its log is {}",
                    place.socket.display(),
                    place.log.display()
                ));
            }
            Err(_) => {}
        }
        let ended = started
            .as_mut()
            .is_none_or(|child| child.try_wait().ok().flatten().is_some());
        if ended {
            started = Some(start().map_err(|error| {
                format!("the process holding this computer's session did not start: {error}")
            })?);
        }
        std::thread::sleep(POLL);
    }
}

/// Start `tilo mcp --hold` on its own: in a process group of its own, so the
/// agent that ends this connection does not end it with the others', and
/// writing what it says to its log.
fn start_holder(place: &Place) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt as _;
    let log = std::fs::File::create(&place.log)?;
    std::process::Command::new(std::env::current_exe()?)
        .args(["mcp", "--hold"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()
}

/// Serve this connection's stdio alone: with no login to share, or when no
/// holder could be reached.
pub(crate) fn alone<E: Enter>(shared: Arc<Shared<E>>) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Enter the workspace while the agent starts, not at its first call.
        let entering = Arc::clone(&shared);
        tokio::spawn(async move {
            let _ = entering.session(false).await;
        });
        converse(Arc::clone(&shared), tokio::io::stdin(), tokio::io::stdout()).await;
        // The connection ended (its input closed): the agent leaves the
        // workspace now, while its session can still say so.
        shared.leaving().await;
    });
    Ok(ExitCode::SUCCESS)
}

/// One agent's connection, served as its own run until its input ends: each
/// request answered as it comes, so a long call holds up no other.
async fn converse<E: Enter>(
    shared: Arc<Shared<E>>,
    read: impl AsyncRead + Unpin,
    write: impl AsyncWrite + Unpin + Send + 'static,
) {
    let connection = Arc::new(Connection::new(shared));
    let write = Arc::new(tokio::sync::Mutex::new(write));
    let mut lines = BufReader::new(read).lines();
    let mut answering = tokio::task::JoinSet::new();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let (connection, write) = (Arc::clone(&connection), Arc::clone(&write));
        answering.spawn(async move {
            if let Some(answer) = answer(&connection, &line).await {
                let mut write = write.lock().await;
                let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
                let _ = write.flush().await;
            }
        });
        while answering.try_join_next().is_some() {}
    }
    while answering.join_next().await.is_some() {}
}

/// Hold the device's session for every connection at `place`, until none
/// came for [`LINGER`]. Another holder already there keeps it: this one
/// ends at once.
pub(crate) fn hold<E: Enter>(place: &Place, shared: Arc<Shared<E>>) -> Result<ExitCode> {
    hold_for(place, shared, LINGER, TAKING_OVER)
}

/// [`hold`], lingering `linger` after the last connection, and waiting up to
/// `taking_over` for a holder that is leaving.
fn hold_for<E: Enter>(
    place: &Place,
    shared: Arc<Shared<E>>,
    linger: Duration,
    taking_over: Duration,
) -> Result<ExitCode> {
    let Some(_held) = take_lock(&place.lock, taking_over)? else {
        return Ok(ExitCode::SUCCESS);
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve_held(place, shared, linger))?;
    Ok(ExitCode::SUCCESS)
}

/// The lock that makes this process the device's one holder, once one that
/// is leaving let go of it; `None` when another holds it still.
fn take_lock(path: &Path, taking_over: Duration) -> Result<Option<std::fs::File>> {
    use rustix::fs::{FlockOperation, flock};
    let lock = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let deadline = Instant::now() + taking_over;
    loop {
        if flock(&lock, FlockOperation::NonBlockingLockExclusive).is_ok() {
            return Ok(Some(lock));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

/// Listen at `place` and serve each connection, while the lock is held.
async fn serve_held<E: Enter>(
    place: &Place,
    shared: Arc<Shared<E>>,
    linger: Duration,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    // A socket left by a holder that ended without removing it.
    let _ = std::fs::remove_file(&place.socket);
    let listener = tokio::net::UnixListener::bind(&place.socket)
        .with_context(|| format!("listen at {}", place.socket.display()))?;
    std::fs::set_permissions(&place.socket, std::fs::Permissions::from_mode(0o600))?;
    // Enter the workspace while the first agent starts, not at its first call.
    let entering = Arc::clone(&shared);
    tokio::spawn(async move {
        let _ = entering.session(false).await;
    });
    let (count, mut live) = tokio::sync::watch::channel(0_usize);
    let count = Arc::new(count);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                count.send_modify(|live| *live += 1);
                let (shared, count) = (Arc::clone(&shared), Arc::clone(&count));
                tokio::spawn(async move {
                    let (read, write) = stream.into_split();
                    converse(shared, read, write).await;
                    count.send_modify(|live| *live -= 1);
                });
            }
            () = idle(&mut live, linger) => break,
        }
    }
    // No connection is left to cut off: the next one starts a holder anew.
    let _ = std::fs::remove_file(&place.socket);
    drop(listener);
    shared.leaving().await;
    Ok(())
}

/// Once no connection has been open for `linger`.
async fn idle(live: &mut tokio::sync::watch::Receiver<usize>, linger: Duration) {
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
    loop {
        let stream = match next.take() {
            Some(stream) => stream,
            None => {
                let place = place.clone();
                let reached = tokio::task::spawn_blocking(move || reach(&place)).await?;
                match reached {
                    Ok(stream) => stream,
                    Err(why) => anyhow::bail!("{why}"),
                }
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
        let mut unanswered: HashMap<String, Value> = HashMap::new();
        let ended = loop {
            tokio::select! {
                line = input.recv(), if input_open => match line {
                    Some(line) => {
                        if let Some(id) = opening.note(&line) {
                            unanswered.insert(id.to_string(), id);
                        }
                        if write.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                            break true;
                        }
                    }
                    None => {
                        input_open = false;
                        let _ = write.shutdown().await;
                    }
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
        for id in unanswered.into_values() {
            let again = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000,
                "message": "Tilo's connection to the workspace started again before this was answered; make the call again."}});
            stdout.write_all(format!("{again}\n").as_bytes()).await?;
        }
        stdout.flush().await?;
    }
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

    /// A device that cannot enter, saying how many times it was asked: the
    /// holder asks it again for each connection, so the count says which
    /// holder answered.
    struct Counting(AtomicUsize);

    impl Enter for Arc<Counting> {
        async fn enter(&self) -> Result<Arc<super::super::Session>, String> {
            let asked = self.0.fetch_add(1, Ordering::SeqCst) + 1;
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
            std::thread::spawn(move || hold_for(&place, shared, linger, Duration::ZERO).unwrap())
        };
        let never = || -> std::io::Result<std::process::Child> {
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
        let entered_at_start = counting.0.load(Ordering::SeqCst);
        let said = [initialize(&mut first), initialize(&mut second)];
        // Each answer is the same holder's: its count went on from one to the next.
        assert_eq!(
            said,
            [
                format!("asked {}", entered_at_start + 1),
                format!("asked {}", entered_at_start + 2)
            ]
        );
        // A second holder (another agent's `tilo mcp` starting one at the same moment) finds
        // the device held and ends, leaving the first listening.
        let second_holder = hold_for(&place, Arc::clone(&shared), linger, Duration::ZERO);
        assert!(second_holder.is_ok());
        let mut third = UnixStream::connect(&place.socket).unwrap();
        assert_eq!(
            initialize(&mut third),
            format!("asked {}", entered_at_start + 3)
        );
        drop((first, second, third));
        holding.join().unwrap();
        assert!(
            !place.socket.exists(),
            "a holder whose connections all ended stops listening"
        );
    }

    /// A device logged in anew is another device: its connections reach a
    /// holder of their own.
    #[test]
    fn each_device_has_its_own_holder() {
        let dir = Path::new("/state/tilo/device");
        assert_ne!(
            Place::at(dir, dir, &[1; 32]).socket,
            Place::at(dir, dir, &[2; 32]).socket
        );
    }

    /// A home however deep still has a socket short enough to listen at:
    /// its holder listens in the account's own directory, which nobody else
    /// may enter.
    #[test]
    fn a_deep_home_still_listens_in_a_private_directory() {
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
