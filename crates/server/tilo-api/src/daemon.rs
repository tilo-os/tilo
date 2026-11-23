//! Where this computer's one daemon (`tilo-runtime`) listens, the one way it is started, who it
//! admits, and what the daemon and the commands that reach it say to each other about it.
//!
//! The daemon starts one way on each computer ([`service::start`]): on Tilo OS the system unit
//! runs it from boot as its own account, and everywhere else it is the signed-in person's own
//! service. No command starts one of its own: every client asks the one that runs, at the
//! sockets [`this_computer`] names, and starts the service when none answers.
//!
//! The host keeps one session per device, so two processes entering as the same device cut each
//! other off. Every `tilo mcp` and every workspace command of a login therefore reaches the one
//! daemon over a socket ([`DaemonPlace::connect`]), and the daemon takes the device's lock
//! ([`DaemonPlace::lock`]) or waits for the one holding it to hand the device over.

pub mod admission;
pub mod enter;
pub mod folders;
pub mod service;
pub mod who;

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What `tilo status` asks this computer's daemon: who it is, which workspaces it shows where,
/// and whether they are in sync.
pub const PEER_STATUS: &str = "tilo/peer";

/// How long the service gives a daemon told to stop before it kills it. The daemon's own waits
/// add up to less, so a stop ends by itself rather than being killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the system unit keeps the daemon's sockets on Tilo OS.
const SYSTEM_SOCKETS: &str = tilo_paths::machine::DAEMON_SOCKETS;
/// The system unit Tilo OS ships: where it is installed, the daemon is the system's.
const SYSTEM_UNIT: &str = "/usr/lib/systemd/system/tilo-runtime.service";
/// The system unit's profile: the daemon's home and state on Tilo OS.
pub const SYSTEM_PROFILE: &str = "/var/lib/tilo/state";
/// The longest a socket's path may be: a socket's address holds 104 bytes on macOS (108 on
/// Linux), its closing NUL among them.
const SOCKET_PATH_MAX: usize = 103;

/// The native endpoint's socket: apps, `tilo-runtime --connect` and the system's own callers.
const NATIVE: &str = tilo_paths::machine::DAEMON_NATIVE;
/// Where agents' `tilo mcp` connections reach the daemon.
const AGENTS: &str = "agents.sock";
/// Where the daemon serves Tilo's API as the device.
const API: &str = "api.sock";

/// Whether this computer's daemon is the system's: Tilo OS runs it from boot as its own
/// account for every person of the computer, where other computers run it as the person.
#[must_use]
pub fn system_daemon() -> bool {
    cfg!(target_os = "linux") && Path::new(SYSTEM_UNIT).is_file()
}

/// Where this computer's daemon listens: the one place every client finds it, and the daemon
/// binds. Under Tilo OS's system unit its sockets are in its runtime directory; a person's own
/// service keeps them in `$XDG_RUNTIME_DIR/tilo` on Linux and in the account's temporary
/// directory (`/var/folders/…/T/tilo`) on macOS. Its lock is in the login's directory, as the
/// device's state is.
///
/// # Errors
/// The login's directory or the sockets' directory cannot be made, the sockets' directory is not
/// this account's own, or a socket's path in it is too long for a socket's address.
pub fn this_computer() -> Result<DaemonPlace, String> {
    let login = tilo_device_identity::login::Place::here(
        tilo_identity::provisioning::collaboration_identity_paths(),
    )
    .map_err(|error| format!("{error:#}"))?;
    let system = system_daemon();
    let sockets = if system {
        PathBuf::from(SYSTEM_SOCKETS)
    } else {
        own_sockets(login.dir())?
    };
    fits(&sockets)?;
    Ok(DaemonPlace {
        system,
        ..DaemonPlace::at(&sockets, login.dir())
    })
}

/// Where this computer's daemon serves Tilo's API as its device, when it serves one here.
///
/// # Errors
/// The login is damaged, or the daemon's sockets' directory is not this account's.
pub fn device_api() -> anyhow::Result<Option<PathBuf>> {
    if tilo_device_identity::login::served_device_key(&crate::client::device_keys()).is_none() {
        return Ok(None);
    }
    let place = this_computer().map_err(|why| anyhow::anyhow!(why))?;
    Ok(Some(place.api().to_path_buf()))
}

/// Whether every socket the daemon listens at in `sockets` has a path a socket's address holds:
/// one that does not is refused, naming it, never cut short.
fn fits(sockets: &Path) -> Result<(), String> {
    [NATIVE, AGENTS, API]
        .into_iter()
        .try_for_each(|name| socket_fits(&sockets.join(name)))
}

/// Whether a socket's address holds `path`: one it does not is refused, naming it, never cut
/// short.
///
/// # Errors
/// The path is longer than a socket's address holds.
pub fn socket_fits(path: &Path) -> Result<(), String> {
    let len = path.as_os_str().len();
    if len > SOCKET_PATH_MAX {
        return Err(format!(
            "{} is too long for a socket's path ({len} bytes, at most {SOCKET_PATH_MAX})",
            path.display()
        ));
    }
    Ok(())
}

/// This account's temporary directory on macOS (`/var/folders/…/T/`), as the system names it:
/// private to the account, and short whatever the home folder is.
#[cfg(target_os = "macos")]
fn darwin_temp_dir() -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt as _;
    let mut buf = vec![0_u8; 1024];
    // SAFETY: `buf` is writable for `buf.len()` bytes; confstr writes at most that many, its
    // NUL included.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if len == 0 || len > buf.len() {
        return Err(format!(
            "this account has no temporary directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    buf.truncate(len.saturating_sub(1));
    Ok(PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// The directory a person's own service keeps its sockets in, made private to this account.
fn own_sockets(login: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    #[cfg(target_os = "macos")]
    let _ = login;
    #[cfg(target_os = "macos")]
    let dir = darwin_temp_dir()?.join("tilo");
    // A command run where the login's runtime directory is not said (over ssh, from cron) finds
    // the one the service has, which systemd names by the account.
    #[cfg(not(target_os = "macos"))]
    let dir = {
        let uid = std::fs::metadata(login)
            .map_err(|error| format!("{} is not readable: {error}", login.display()))?
            .uid();
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|runtime| runtime.is_absolute() && runtime.is_dir())
            .or_else(|| Some(PathBuf::from(format!("/run/user/{uid}"))).filter(|run| run.is_dir()))
            .map_or_else(
                || std::env::temp_dir().join(format!("tilo-{uid}")),
                |runtime| runtime.join("tilo"),
            )
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|error| format!("{} could not be made: {error}", dir.display()))?;
    let made = std::fs::symlink_metadata(&dir)
        .map_err(|error| format!("{} is not readable: {error}", dir.display()))?;
    let parent = dir
        .parent()
        .and_then(|parent| std::fs::metadata(parent).ok());
    if !made.is_dir()
        || made.mode() & 0o077 != 0
        || parent.is_some_and(|parent| parent.uid() != made.uid())
    {
        return Err(format!(
            "{} is not this account's own private directory",
            dir.display()
        ));
    }
    Ok(dir)
}

/// Where one device's daemon listens, and the lock that keeps it one.
#[derive(Clone, Debug)]
pub struct DaemonPlace {
    /// The directory its sockets are in: these, its native endpoint and its services'.
    sockets: PathBuf,
    /// Where it takes `tilo mcp` connections.
    socket: PathBuf,
    /// Where it serves Tilo's API as the device, over the one session: what `tilo do`, `ls`,
    /// `read` and the other workspace commands of this login reach.
    api: PathBuf,
    /// One per device directory: a computer has one daemon, and a login made again under a new
    /// key waits for the daemon of the old one to end instead of running beside it.
    lock: PathBuf,
    /// Whether it is the system unit's: its sockets are open to every account, which it admits
    /// by who they are ([`admission`]).
    system: bool,
}

impl DaemonPlace {
    /// A daemon listening in `sockets`, keeping its lock in `dir`.
    #[must_use]
    pub fn at(sockets: &Path, dir: &Path) -> Self {
        Self {
            sockets: sockets.to_owned(),
            socket: sockets.join(AGENTS),
            api: sockets.join(API),
            lock: dir.join("holder.lock"),
            system: false,
        }
    }

    /// Whether this is the system unit's daemon.
    #[must_use]
    pub const fn system(&self) -> bool {
        self.system
    }

    /// The mode its sockets are made with: open to every account under the system unit, which
    /// admits each connection by its account ([`admission`]), and the owner's alone otherwise.
    #[must_use]
    pub const fn socket_mode(&self) -> u32 {
        if self.system { 0o666 } else { 0o600 }
    }

    /// The directory its sockets are in.
    #[must_use]
    pub fn sockets(&self) -> &Path {
        &self.sockets
    }

    /// Its native endpoint: what every program of this computer asks about the device
    /// ([`who`]).
    #[must_use]
    pub fn native(&self) -> PathBuf {
        self.sockets.join(NATIVE)
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

    /// The device's directory, which the daemon keeps the device's state in.
    #[must_use]
    pub fn device_dir(&self) -> PathBuf {
        self.lock
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
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
    /// process, so a daemon that ended never reads as one that is there.
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
            Err(TryLockError::Error(_)) => false,
        }
    }

    /// Wait until this device's daemon serves Tilo's API at [`Self::api`], starting the service
    /// ([`service::start`]) when none answers: what a client of this daemon needs before its
    /// first call.
    ///
    /// # Errors
    /// The service did not start, or no daemon answered in `within`.
    pub fn wait_for_api(&self, within: Duration) -> Result<(), String> {
        self.wait_for(&self.api, within, !self.system)
    }

    /// Wait until the daemon answers at its native endpoint, starting the service when none
    /// runs: a computer no person's certificate names answers there alone. It founds no person:
    /// this is how a computer about to be logged in with a ticket reaches its daemon, and the
    /// ticket's person is the one it becomes.
    ///
    /// # Errors
    /// The service did not start, or no daemon answered in `within`.
    pub fn wait_for_native(&self, within: Duration) -> Result<(), String> {
        self.wait_for(&self.native(), within, false)
    }

    /// A connection to the daemon's `tilo mcp` socket, starting the service when none answers
    /// and waiting up to `within` for it.
    ///
    /// # Errors
    /// The service did not start, or no daemon answered in `within`.
    pub fn reach(&self, within: Duration) -> Result<UnixStream, String> {
        self.wait_for(&self.socket, within, !self.system)?;
        tilo_files::native::socket::connect(&self.socket).map_err(|error| error.to_string())
    }

    /// Wait until the daemon answers at `socket`, starting the service once when no daemon
    /// keeps the device: one that runs (still starting, or taking a login up) is waited for. With
    /// `founding`, a person's own computer that no person's certificate names yet founds its
    /// person here, once its daemon answers, as its first command always has.
    fn wait_for(&self, socket: &Path, within: Duration, mut founding: bool) -> Result<(), String> {
        let deadline = Instant::now()
            .checked_add(within)
            .unwrap_or_else(Instant::now);
        let mut started = false;
        loop {
            if tilo_files::native::socket::connect(socket).is_ok() {
                return Ok(());
            }
            if !started && !self.alive() {
                started = true;
                service::start()?;
            }
            if founding && tilo_files::native::socket::connect(&self.native()).is_ok() {
                founding = false;
                self.found()?;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Tilo isn't answering on this computer yet ({}): {}",
                    socket.display(),
                    service::LOOK
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl DaemonPlace {
    /// Ask the daemon to found this computer's person ([`tilo_device_identity::login::FOUND`]),
    /// as a person's own computer's first command does. A device a person's certificate names
    /// already refuses it, which is no failure here: that person is served.
    ///
    /// # Errors
    /// The daemon does not answer, or could not found the person.
    pub fn found(&self) -> Result<(), String> {
        use std::io::{BufRead as _, Write as _};
        let mut native = tilo_files::native::socket::connect(&self.native())
            .map_err(|error| error.to_string())?;
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "tilo_do", "arguments": {
                "path": format!(
                    "{}#{}",
                    tilo_device_identity::login::RESOURCE,
                    tilo_device_identity::login::FOUND
                ),
                "args": {},
            }},
        });
        writeln!(native, "{line}").map_err(|error| error.to_string())?;
        let mut answer = String::new();
        std::io::BufReader::new(native)
            .read_line(&mut answer)
            .map_err(|error| error.to_string())?;
        let refused = answer.contains(r#""error""#) || answer.contains(r#""isError":true"#);
        if refused && !answer.contains(tilo_device_identity::login::ALREADY_A_PERSONS) {
            return Err(format!(
                "Tilo could not set up this computer's person: {}",
                answer.trim()
            ));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A sockets' directory whose longest socket path still fits a socket's address is taken; one
    /// a byte longer is refused, naming the path it would have made, never cut short.
    #[test]
    fn a_socket_path_too_long_for_its_address_is_refused_by_name() {
        let longest = NATIVE.len().max(AGENTS.len()).max(API.len());
        // `/`, the directory's name, `/`, the socket's name.
        let room = SOCKET_PATH_MAX - longest - 2;
        let fitting = PathBuf::from(format!("/{}", "d".repeat(room)));
        assert_eq!(fits(&fitting), Ok(()));
        let over = PathBuf::from(format!("/{}", "d".repeat(room + 1)));
        let refused = fits(&over).expect_err("a path past the address is refused");
        assert!(refused.contains(&over.display().to_string()), "{refused}");
        assert!(refused.contains("too long"), "{refused}");
    }

    /// On macOS a person's own service keeps its sockets in the account's temporary directory,
    /// as the system names it, private to the account, and short enough for every socket.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_persons_sockets_on_macos_are_in_their_temporary_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let login = tempfile::tempdir().unwrap();
        let sockets = own_sockets(login.path()).unwrap();
        assert_eq!(sockets, darwin_temp_dir().unwrap().join("tilo"));
        assert!(sockets.starts_with("/var/folders"), "{}", sockets.display());
        let mode = std::fs::metadata(&sockets).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        assert_eq!(fits(&sockets), Ok(()));
    }
}
