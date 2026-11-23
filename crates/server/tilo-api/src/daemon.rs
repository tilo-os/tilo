//! Where this computer's one daemon (`tilo-runtime`) listens, the lock that keeps it one,
//! and what the daemon and the commands that reach it say to each other about it: the claim a
//! daemon starting makes on the device, and how it asks the one there to step down.
//!
//! The host keeps one session per device, so two processes entering as the same device cut each
//! other off. Every `tilo mcp` and every workspace command of a login therefore reaches the one
//! daemon over a socket ([`DaemonPlace::connect`]), and a daemon starting takes the device's lock
//! ([`DaemonPlace::lock`]) or waits for the one holding it to hand the device over.

pub mod enter;
pub mod folders;

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What `tilo status` asks this computer's daemon: who it is, which workspaces it shows where,
/// and whether they are in sync.
pub const PEER_STATUS: &str = "tilo/peer";

/// What a daemon starting asks the one keeping the device: to let it keep the device instead,
/// as what it claims the device as ([`Claim`]): a newer program, or the user service.
pub const STEP_DOWN: &str = "tilo/stepDown";

/// Set in the user service's environment: the daemon it starts knows itself as the service's,
/// the one that lasts for as long as the person is logged in.
pub const SERVICE_ENV: &str = "TILO_PEER_SERVICE";

/// How long the user service gives a daemon told to stop before it kills it. The daemon's own
/// waits add up to less, so a stop ends by itself rather than being killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether this process is the daemon the user service started.
#[must_use]
pub fn running_as_service() -> bool {
    std::env::var_os(SERVICE_ENV).is_some_and(|value| value == "1")
}

/// Where one device's daemon listens, the lock that keeps it one, and its log.
#[derive(Clone, Debug)]
pub struct DaemonPlace {
    socket: PathBuf,
    /// Where it serves Tilo's API as the device, over the one session: what
    /// `tilo do`, `ls`, `read` and the other workspace commands of this login
    /// reach ([`tilo_device_identity::login::held_api`]).
    api: PathBuf,
    /// One per device directory, not per key: a computer has one daemon, and a login made again
    /// under a new key waits for the daemon of the old one to step down instead of running
    /// beside it. Two live endpoints under one device's keys take each other's place at the
    /// relay over and over, and neither serves.
    lock: PathBuf,
    log: PathBuf,
    /// The first bytes of the device key the listener serves, as its socket and log are named.
    key: [u8; 8],
}

impl DaemonPlace {
    /// The daemon of the device whose public key is `key`, logged in under `dir`: a new login is
    /// a new device. Its sockets are in this account's own short directory
    /// ([`tilo_device_identity::login::holder_dir`]).
    ///
    /// # Errors
    /// The sockets' directory is not this account's own.
    pub fn of(dir: &Path, key: &[u8; 32]) -> Result<Self, String> {
        Ok(Self::at(
            &tilo_device_identity::login::holder_dir(dir)?,
            dir,
            key,
        ))
    }

    /// The daemon of `key`'s device under `dir`, listening in `sockets`.
    #[must_use]
    pub fn at(sockets: &Path, dir: &Path, key: &[u8; 32]) -> Self {
        let mut first = [0; 8];
        first.copy_from_slice(&key[..8]);
        let name = tilo_device_identity::login::holder_name(key);
        Self {
            socket: sockets.join(format!("{name}.sock")),
            api: tilo_device_identity::login::holder_api_socket(sockets, &name),
            lock: dir.join("holder.lock"),
            log: dir.join(format!("{name}.log")),
            key: first,
        }
    }

    /// Where it takes `tilo mcp` connections.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Where it serves Tilo's API as the device.
    #[must_use]
    pub fn api(&self) -> &Path {
        &self.api
    }

    /// The lock a live daemon holds.
    #[must_use]
    pub fn lock(&self) -> &Path {
        &self.lock
    }

    /// The log every daemon started for this device adds to.
    #[must_use]
    pub fn log(&self) -> &Path {
        &self.log
    }

    /// The device's directory, which the daemon keeps the device's state in.
    #[must_use]
    pub fn device_dir(&self) -> PathBuf {
        self.lock
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    /// Whether the login under this device's directory is still this daemon's: a login made
    /// again makes another device, and the daemon of the one it replaced ends so that one
    /// computer keeps one daemon and one live connection.
    #[must_use]
    pub fn serves_current_login(&self) -> bool {
        let Some(dir) = self.lock.parent() else {
            return true;
        };
        match tilo_device_identity::login::load(&tilo_device_identity::login::Place::at(
            dir.to_owned(),
            tilo_identity::provisioning::collaboration_identity_paths(),
        )) {
            Ok(Some((key, _))) => key.verifying_key().to_bytes()[..8] == self.key,
            // A login mid-write reads as none; the next look answers for it.
            _ => true,
        }
    }

    /// Whether this computer is logged in, as the login under this device's directory says.
    #[must_use]
    pub fn logged_in(&self) -> bool {
        self.lock.parent().is_some_and(|dir| {
            tilo_device_identity::login::Place::at(
                dir.to_owned(),
                tilo_identity::provisioning::collaboration_identity_paths(),
            )
            .logged_in()
        })
    }

    /// A connection to the device's daemon when one listens, starting none.
    #[must_use]
    pub fn connect(&self) -> Option<UnixStream> {
        tilo_files::native::socket::connect(&self.socket).ok()
    }

    /// Whether this login's daemon listens: its socket is there while it serves, and removed as
    /// it ends. Unlike [`Self::connect`], this waits on nothing, so a command reads it of a
    /// daemon that is busy with a step over a workspace's folder.
    #[must_use]
    pub fn listening(&self) -> bool {
        self.socket.exists()
    }

    /// Whether a live process holds this device's lock. The kernel drops the lock with its
    /// process, so a daemon that ended never reads as one that is there, and one inside a step
    /// that walks a whole folder tree does. A command reads this instead of asking, so a daemon
    /// that answers slowly is never mistaken for one that is not there.
    #[must_use]
    pub fn alive(&self) -> bool {
        use advisory_lock::{FileLock, TryLockError};
        // The look never makes the lock: the daemon makes it, private to this user, and refuses
        // one made otherwise. No lock is no daemon.
        let Ok(file) = std::fs::File::open(&self.lock) else {
            return false;
        };
        match FileLock::try_exclusive(file) {
            // Free: taken here for the look, and let go at once.
            Ok(_free) => false,
            Err(TryLockError::WouldBlock) => true,
            // A lock that cannot even be opened is no daemon to wait for: starting one says more.
            Err(TryLockError::Error(_)) => false,
        }
    }

    /// Start this device's daemon (`tilo-runtime`) as a process of its own, in a process
    /// group of its own so the command that started it ending does not end it, adding what it
    /// says to its log.
    ///
    /// # Errors
    /// The program is not there, or it did not start.
    pub fn start(&self) -> std::io::Result<std::process::Child> {
        use std::os::unix::process::CommandExt as _;
        // Added to, never emptied: a daemon started while another holds the device ends at once,
        // and emptying the log the one holding writes to would lose what it said.
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)?;
        std::process::Command::new(runtime_program()?)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(log)
            .process_group(0)
            .spawn()
    }

    /// Wait until this device's daemon serves Tilo's API at [`Self::api`], starting one with
    /// `start` when none answers: what a client of this daemon needs before its first call.
    ///
    /// # Errors
    /// No daemon answered in `within`, or starting one failed.
    pub fn wait_for_api(
        &self,
        within: Duration,
        start: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<(), String> {
        let deadline = Instant::now()
            .checked_add(within)
            .unwrap_or_else(Instant::now);
        let mut start = Some(start);
        loop {
            if tilo_files::native::socket::connect(&self.api).is_ok() {
                return Ok(());
            }
            if let Some(start) = start.take() {
                start().map_err(|error| error.to_string())?;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "this computer's daemon did not answer at {} in time: run `tilo status` to \
                     see how it stands, then try again (its log is {})",
                    self.api.display(),
                    self.log.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Ask the daemon listening here to let a daemon claiming `claim` keep the device: whether
    /// it steps down, as it does when the claim takes over from its own ([`Claim::takes_over`]);
    /// `None` when none answered.
    #[must_use]
    pub fn ask_to_step_down(&self, claim: Claim) -> Option<bool> {
        use std::io::{BufRead as _, Write as _};
        let mut stream = self.connect()?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let asked = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": STEP_DOWN,
            "params": {"program": claim.program, "service": claim.service},
        });
        writeln!(stream, "{asked}").ok()?;
        let mut line = String::new();
        std::io::BufReader::new(stream).read_line(&mut line).ok()?;
        serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|answer| answer.get("result")?.get("stepping_down")?.as_bool())
    }
}

/// What a daemon claims the device as: its program, and whether the user service started it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub program: Program,
    /// Started by the user service, which keeps it running for as long as the person is logged
    /// in. A daemon an agent or a command started lives in their process tree and ends with it.
    #[serde(default)]
    pub service: bool,
}

impl Claim {
    /// Whether a daemon claiming this takes the device over from one claiming `holding`: it is
    /// a newer program, or it is the user service's and `holding` is not, of a program no newer.
    /// The service's daemon is the one that lasts: one an agent started first hands the device
    /// (its session and its folders) over to it, rather than the service ending beside it.
    #[must_use]
    pub fn takes_over(&self, holding: &Self) -> bool {
        self.program.replaces(&holding.program)
            || (self.service && !holding.service && !holding.program.replaces(&self.program))
    }
}

/// The program a daemon is, as the file it was started from: Tilo replaced by a newer build is
/// another file, and a daemon still running the one it replaced lets a daemon of the new one take
/// over. Two daemons of one program never do: two agents starting one at the same moment keep the
/// first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// The file's inode: a new build put in place is a new file, whatever its name.
    pub file: u64,
    /// When it was written, in nanoseconds since the epoch.
    pub modified_ns: u128,
}

impl Program {
    /// The program this process was started from. A program file replaced since it started is
    /// still read as the one it started from (`/proc/self/exe` names it, deleted or not).
    #[must_use]
    pub fn running() -> Self {
        let metadata = std::fs::metadata("/proc/self/exe")
            .or_else(|_| std::env::current_exe().and_then(std::fs::metadata));
        metadata.map_or(Self::NONE, |metadata| Self::from(&metadata))
    }

    /// The program in the file at `path`: what a command asks for when it starts a daemon of
    /// that file.
    #[must_use]
    pub fn of(path: &Path) -> Self {
        std::fs::metadata(path).map_or(Self::NONE, |metadata| Self::from(&metadata))
    }

    /// A program whose file cannot be read.
    const NONE: Self = Self {
        file: 0,
        modified_ns: 0,
    };

    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            file: metadata.ino(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos()),
        }
    }

    /// Whether a daemon running this program takes over from one running `holding`: it is
    /// another file, written no earlier.
    #[must_use]
    pub fn replaces(self, holding: &Self) -> bool {
        self.file != holding.file && self.modified_ns >= holding.modified_ns
    }
}

/// `line` with the session this connection is written in, when it is the client's `initialize`
/// and the agent's harness named the session: what the workspace shows the agent by, in place of
/// the product's name. Any other line, and a session with no name, go as they are.
#[must_use]
pub fn naming(line: String, session_name: Option<&str>) -> String {
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

/// Where the `tilo-runtime` program beside `tilo` is: the daemon `tilo` starts. Beside the
/// running program (`~/.local/bin`, `/usr/bin`, a build's target directory), else in
/// Tilo.app on macOS.
///
/// # Errors
/// Neither is there.
pub fn runtime_program() -> std::io::Result<PathBuf> {
    const NAME: &str = "tilo-runtime";
    const IN_APP: &str = "/Applications/Tilo.app/Contents/MacOS/tilo-runtime";
    let running = std::env::current_exe()?;
    let beside = running
        .parent()
        .map(|dir| dir.join(NAME))
        .filter(|path| path.is_file());
    if let Some(beside) = beside {
        return Ok(beside);
    }
    // A test binary lives in `deps/`, one below the programs of its build.
    let built = running
        .parent()
        .and_then(Path::parent)
        .map(|dir| dir.join(NAME))
        .filter(|path| path.is_file());
    if let Some(built) = built {
        return Ok(built);
    }
    if cfg!(target_os = "macos") && Path::new(IN_APP).is_file() {
        return Ok(PathBuf::from(IN_APP));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "{NAME} is not installed beside {}; install Tilo again with `curl -fsSL https://tilo.sh | sh`",
            running.display()
        ),
    ))
}

/// The MCP protocol version an agent's connection speaks while the workspace cannot be reached;
/// the workspace's projection answers with its own once it can.
const PROTOCOL: &str = "2025-06-18";

/// What an agent's connection answers `request` with while the workspace cannot be reached: who
/// it is, no tools, and `problem` for any call. `None` for a notification.
#[must_use]
pub fn unreachable_answer(request: &serde_json::Value, problem: &str) -> Option<serde_json::Value> {
    use serde_json::{Value, json};
    let id = request.get("id").cloned()?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tilo", "version": env!("CARGO_PKG_VERSION")},
            "instructions": problem,
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": []}),
        "tools/call" => json!({"content": [{"type": "text", "text": problem}], "isError": true}),
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": format!("no method {method}")}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}
