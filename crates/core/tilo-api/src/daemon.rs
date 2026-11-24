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
//! daemon at its native endpoint ([`DaemonPlace::native`]), and the daemon takes the device's lock
//! ([`DaemonPlace::lock`]) or waits for the one holding it to hand the device over.

pub mod admission;
pub mod enter;
pub mod folders;
pub mod service;
pub mod who;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What `tilo status` asks this computer's daemon: who it is, which workspaces it shows where,
/// and whether they are in sync.
pub const PEER_STATUS: &str = "tilo/peer";

/// The first line of an agent's connection to the daemon's native endpoint (`tilo mcp`): the
/// rest is the agent's MCP session, served over the device's one session with its workspace.
pub const AGENT_LINE: &str = r#"{"native":"agent"}"#;

/// How long the service gives a daemon told to stop before it kills it. The daemon's own waits
/// add up to less, so a stop ends by itself rather than being killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the system unit keeps the daemon's sockets on Tilo OS.
const SYSTEM_SOCKETS: &str = tilo_paths::machine::DAEMON_SOCKETS;
/// The system unit's profile: the daemon's home and state on Tilo OS.
pub const SYSTEM_PROFILE: &str = "/var/lib/tilo/state";
/// The longest a socket's path may be: a socket's address holds 104 bytes on macOS (108 on
/// Linux), its closing NUL among them.
const SOCKET_PATH_MAX: usize = 103;

/// The native endpoint's socket: apps, `tilo-runtime --connect`, the system's own callers,
/// every command's action frames and agents' `tilo mcp`.
const NATIVE: &str = tilo_paths::machine::DAEMON_NATIVE;

pub use tilo_paths::machine::system_daemon;

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
    // A daemon serving a profile named apart (a VM host's member) listens in that profile, as it
    // is started over it: a command run beside it with the same profile asks it there.
    let named = std::env::var_os(tilo_paths::machine::DAEMON_PROFILE_VAR)
        .filter(|profile| !profile.is_empty());
    let sockets = if system {
        PathBuf::from(SYSTEM_SOCKETS)
    } else if let Some(profile) = named {
        PathBuf::from(profile)
    } else {
        own_sockets(login.dir())?
    };
    fits(&sockets)?;
    Ok(DaemonPlace {
        system,
        ..DaemonPlace::at(&sockets, login.dir())
    })
}

/// Whether every socket the daemon listens at in `sockets` has a path a socket's address holds:
/// one that does not is refused, naming it, never cut short.
fn fits(sockets: &Path) -> Result<(), String> {
    socket_fits(&sockets.join(NATIVE))
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

/// The directory a person's own service keeps its sockets in, made private to this account.
#[cfg(unix)]
fn own_sockets(login: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    let uid = std::fs::metadata(login)
        .map_err(|error| format!("{} is not readable: {error}", login.display()))?
        .uid();
    let dir = tilo_paths::machine::daemon_sockets(uid)?;
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

/// The directory a person's own service names its pipes by, made private to this account: what
/// Windows keeps of a pipe is its name, which this account's profile holds alone.
#[cfg(windows)]
fn own_sockets(login: &Path) -> Result<PathBuf, String> {
    let _ = login;
    let dir = tilo_paths::machine::daemon_sockets()?;
    tilo_files::native::private::directory(&dir)
        .map_err(|error| format!("{} could not be made private: {error}", dir.display()))?;
    Ok(dir)
}

/// Where one device's daemon listens, and the lock that keeps it one.
#[derive(Clone, Debug)]
pub struct DaemonPlace {
    /// The directory its sockets are in: these, its native endpoint and its services'.
    sockets: PathBuf,
    /// Its native endpoint, in `sockets`.
    native: PathBuf,
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
            native: sockets.join(NATIVE),
            lock: dir.join("holder.lock"),
            system: false,
        }
    }

    /// Whether this is the system unit's daemon.
    #[must_use]
    pub const fn system(&self) -> bool {
        self.system
    }

    /// The directory its sockets are in.
    #[must_use]
    pub fn sockets(&self) -> &Path {
        &self.sockets
    }

    /// Its native endpoint: where `tilo do`, `ls`, `read` and the other workspace commands of
    /// this login call actions as the device, over the one session, in action frames, and where
    /// agents' `tilo mcp` connections reach it ([`AGENT_LINE`]).
    #[must_use]
    pub fn native(&self) -> PathBuf {
        self.native.clone()
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

    /// An agent's connection to the device's daemon when one listens, starting none: its first
    /// line ([`AGENT_LINE`]) said.
    #[must_use]
    pub fn connect(&self) -> Option<tilo_files::native::socket::Stream> {
        use std::io::Write as _;
        let mut stream = tilo_files::native::socket::connect(&self.native).ok()?;
        writeln!(stream, "{AGENT_LINE}").ok()?;
        Some(stream)
    }

    /// Whether this login's daemon listens: its native endpoint is there while it serves, and
    /// removed as it ends. Unlike [`Self::connect`], this waits on nothing, so a command reads
    /// it of a daemon that is busy with a step over a workspace's folder.
    #[must_use]
    pub fn listening(&self) -> bool {
        tilo_files::native::socket::is_bound(&self.native)
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

    /// A connection to the daemon's native endpoint ([`Self::native`]), starting the service
    /// ([`service::start`]) when none answers and waiting up to `within` for it: what a command
    /// calling actions as the device makes before its first call.
    ///
    /// # Errors
    /// The service did not start, or no daemon answered in `within`.
    pub fn reach_native(
        &self,
        within: Duration,
    ) -> Result<tilo_files::native::socket::Stream, String> {
        self.wait_for(within, None)?;
        tilo_files::native::socket::connect(&self.native).map_err(|error| error.to_string())
    }

    /// [`Self::reach_native`], founding this account's person with `founding` when this computer
    /// has none yet: the connection, and the founding answer (their first tickets among it) when
    /// a person was founded now.
    ///
    /// # Errors
    /// The service did not start, no daemon answered in `within`, or the person could not be
    /// founded.
    pub fn reach_native_founding(
        &self,
        within: Duration,
        founding: &Founding,
    ) -> Result<
        (
            tilo_files::native::socket::Stream,
            Option<serde_json::Value>,
        ),
        String,
    > {
        let answer = self.wait_for(within, Some(founding))?;
        let stream =
            tilo_files::native::socket::connect(&self.native).map_err(|error| error.to_string())?;
        Ok((stream, answer))
    }

    /// Wait until the daemon answers at its native endpoint, starting the service when none
    /// runs: a computer no person's certificate names answers there alone. It founds no person:
    /// this is how a computer about to be logged in with a ticket reaches its daemon, and the
    /// ticket's person is the one it becomes.
    ///
    /// # Errors
    /// The service did not start, or no daemon answered in `within`.
    pub fn wait_for_native(&self, within: Duration) -> Result<(), String> {
        self.wait_for(within, None).map(drop)
    }

    /// An agent's connection to the daemon ([`Self::connect`]), starting the service when none
    /// answers and waiting up to `within` for it.
    ///
    /// # Errors
    /// The service did not start, or no daemon answered in `within`.
    pub fn reach(&self, within: Duration) -> Result<tilo_files::native::socket::Stream, String> {
        self.wait_for(within, None)?;
        self.connect()
            .ok_or_else(|| format!("{} stopped answering", self.native.display()))
    }

    /// Wait until the daemon answers at its native endpoint, starting the service once when no
    /// daemon keeps the device: one that runs (still starting, or taking a login up) is waited
    /// for. A person's own computer that no person's certificate names yet founds its person
    /// here, once its daemon answers, as its first command always has; Tilo OS's device waits
    /// for a sign-in.
    fn wait_for(
        &self,
        within: Duration,
        founding: Option<&Founding>,
    ) -> Result<Option<serde_json::Value>, String> {
        let socket = &self.native;
        let deadline = Instant::now()
            .checked_add(within)
            .unwrap_or_else(Instant::now);
        let mut started = false;
        loop {
            if tilo_files::native::socket::connect(socket).is_ok() {
                // A device no person's certificate names serves nobody yet: its own account
                // founds its person here, as its first command always has.
                if !self.system && who::who().is_ok_and(|who| !who.claimed) {
                    return founding.map_or_else(
                        || self.found(&Founding::default()),
                        |founding| self.found(founding),
                    );
                }
                return Ok(None);
            }
            if !started && !self.alive() {
                started = true;
                service::start()?;
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

/// What founding this account's person asks for beside the person: the name they go by, and the
/// tickets their first device issues as part of founding them.
#[derive(Clone, Debug, Default)]
pub struct Founding {
    /// The name the person goes by, when they gave one.
    pub name: Option<String>,
    /// The tickets the founding call is answered, in this order.
    pub tickets: Vec<TicketKind>,
}

/// What a ticket a founding person asks for lets in, as `tilo login --new-identity` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketKind {
    /// A computer at hand, which becomes one of their devices, as `tilo device add` adds one.
    Device,
    /// A server, which keeps their workspaces.
    Server,
}

impl TicketKind {
    /// What the daemon's founding call names this kind by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Server => "server",
        }
    }

    /// The kind `name` says, when it says one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "device" => Some(Self::Device),
            "server" => Some(Self::Server),
            _ => None,
        }
    }
}

impl Founding {
    /// What this founding is asked with: the name and the tickets, as the daemon reads them.
    #[must_use]
    pub fn arguments(&self) -> serde_json::Value {
        let mut args = serde_json::Map::new();
        if let Some(name) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            args.insert("name".to_owned(), name.into());
        }
        if !self.tickets.is_empty() {
            let kinds: Vec<&str> = self.tickets.iter().map(|kind| kind.as_str()).collect();
            args.insert("tickets".to_owned(), kinds.into());
        }
        serde_json::Value::Object(args)
    }
}

impl DaemonPlace {
    /// Ask the daemon to found this computer's person ([`tilo_device_identity::login::FOUND`]),
    /// named as `founding` says and answered the tickets it asks for, as a person's own
    /// computer's first command and `tilo login --new-identity` do: the answer (the person and
    /// their first tickets), or none when a device a person's certificate names already serves
    /// them, which is no failure here.
    ///
    /// # Errors
    /// The daemon does not answer, or could not found the person.
    pub fn found(&self, founding: &Founding) -> Result<Option<serde_json::Value>, String> {
        // Founding a person starts their services before it can issue the tickets a founder
        // asked for: the call is answered in parts, and followed until it settles.
        match self.act_followed(tilo_device_identity::login::FOUND, founding.arguments()) {
            Ok(answer) => Ok(Some(answer)),
            Err(why) if why.contains(tilo_device_identity::login::ALREADY_A_PERSONS) => Ok(None),
            Err(why) => Err(format!(
                "Tilo could not set up this computer's person: {why}"
            )),
        }
    }

    /// Ask the daemon one of the device's actions (`devices#<action>`) with `args`, on its native
    /// endpoint, as this account: its answer, or the refusal it gave, in its own words.
    ///
    /// # Errors
    /// The daemon does not answer, or refused.
    pub fn act(&self, action: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
        self.ask(action, args).map_err(|refused| refused.message)
    }

    /// [`Self::act`], with the refusal's code beside its words, for a caller that answers one
    /// refusal in its own way.
    ///
    /// # Errors
    /// The daemon does not answer, or refused.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller builds the answer's arguments for this one call"
    )]
    pub fn ask(
        &self,
        action: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, DeviceRefusal> {
        let (typed, text, failed) = self.call(action, &args, None)?;
        Self::answered(&typed, &text, failed)
    }

    /// [`Self::act`] for an action that may wait (for the person's answer at a device of theirs
    /// that keeps their root, say): its call is followed by its invocation until it settles.
    ///
    /// # Errors
    /// The daemon does not answer, or the call failed or was refused.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller builds the answer's arguments for this one call"
    )]
    pub fn act_followed(
        &self,
        action: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut following = None;
        loop {
            let (typed, text, failed) = self
                .call(action, &args, following.as_deref())
                .map_err(|refused| refused.message)?;
            match typed.get("status").and_then(serde_json::Value::as_str) {
                Some("asked" | "pending") => {
                    following = Some(
                        typed
                            .get("invocation_id")
                            .and_then(serde_json::Value::as_str)
                            .ok_or("Tilo named no call to follow")?
                            .to_owned(),
                    );
                }
                _ => {
                    return Self::answered(&typed, &text, failed)
                        .map_err(|refused| refused.message);
                }
            }
        }
    }

    /// The record `record` of `app`, in this login's workspace, read through the one record API
    /// (`tilo_read` of `record:<app>:<record>`): the reading, its records each with the value the
    /// app keeps, or the refusal's words.
    ///
    /// # Errors
    /// The daemon does not answer, or refused the read.
    pub fn read_record(&self, app: &str, record: &str) -> Result<serde_json::Value, String> {
        let (typed, text, failed) = self
            .tool(
                "tilo_read",
                serde_json::json!({ "path": format!("record:{app}:{record}") }),
            )
            .map_err(|refused| refused.message)?;
        if typed.get("records").is_some() {
            return Ok(typed);
        }
        if failed {
            return Err(text);
        }
        serde_json::from_str(&text).map_err(|_| format!("Tilo read no record of {app}: {text}"))
    }

    /// The daemon's answer to `devices#<action>` with `args`, or to following the call
    /// `following` names: its typed answer, its words, and whether it says it failed.
    fn call(
        &self,
        action: &str,
        args: &serde_json::Value,
        following: Option<&str>,
    ) -> Result<(serde_json::Value, String, bool), DeviceRefusal> {
        let mut arguments = serde_json::json!({
            "path": format!("{}#{action}", tilo_device_identity::login::RESOURCE),
            "args": args,
        });
        if let (Some(invocation), Some(fields)) = (following, arguments.as_object_mut()) {
            fields.insert("invocation_id".into(), invocation.into());
            fields.insert("wait_ms".into(), FOLLOW_WAIT_MS.into());
        }
        self.tool("tilo_do", arguments)
    }

    /// The daemon's answer to the tool `tool` with `arguments`: its typed answer, its words, and
    /// whether it says it failed.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the call builds the request's arguments for this one call"
    )]
    fn tool(
        &self,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<(serde_json::Value, String, bool), DeviceRefusal> {
        use std::io::{BufRead as _, Write as _};
        let mut native = tilo_files::native::socket::connect(&self.native()).map_err(|error| {
            DeviceRefusal::said(format!("Tilo isn't answering on this computer: {error}"))
        })?;
        // A daemon that took this connection but never answers leaves the command hanging for as
        // long as it is broken: the person is told it did not answer, and where to see why.
        native
            .set_read_timeout(Some(ANSWERING_WITHIN))
            .map_err(|error| DeviceRefusal::said(error.to_string()))?;
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments,
                "_meta": {"so.tilo.mcp": {"structured": true}}},
        });
        writeln!(native, "{line}").map_err(|error| DeviceRefusal::said(error.to_string()))?;
        let mut answer = String::new();
        std::io::BufReader::new(native)
            .read_line(&mut answer)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                    DeviceRefusal::said(format!(
                        "Tilo's daemon took this request and did not answer it within {} seconds: \
                         run `tilo status` to see how it stands, then try again",
                        ANSWERING_WITHIN.as_secs()
                    ))
                }
                _ => DeviceRefusal::said(error.to_string()),
            })?;
        let answer: serde_json::Value = serde_json::from_str(answer.trim()).map_err(|_| {
            DeviceRefusal::said(format!(
                "Tilo answered something unreadable: {}",
                answer.trim()
            ))
        })?;
        if let Some(error) = answer.get("error") {
            return Err(DeviceRefusal::said(
                error
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| error.to_string(), str::to_owned),
            ));
        }
        let text = answer
            .pointer("/result/content/0/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let typed = answer
            .pointer("/result/structuredContent")
            .cloned()
            .unwrap_or_default();
        let failed = answer.pointer("/result/isError") == Some(&serde_json::Value::Bool(true));
        Ok((typed, text, failed))
    }

    /// What the daemon's answer `typed`, in words `text`, gives: its result, or its refusal.
    fn answered(
        typed: &serde_json::Value,
        text: &str,
        failed: bool,
    ) -> Result<serde_json::Value, DeviceRefusal> {
        match typed.get("status").and_then(serde_json::Value::as_str) {
            Some("ok") => Ok(typed.get("result").cloned().unwrap_or_default()),
            // An answer in words alone: what it said, read as the value it names.
            None if !failed => Ok(serde_json::from_str(text)
                .unwrap_or_else(|_| serde_json::Value::String(text.to_owned()))),
            _ => Err(DeviceRefusal {
                code: typed
                    .pointer("/error/code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(REFUSED)
                    .to_owned(),
                message: typed
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .filter(|message| !message.is_empty())
                    .unwrap_or(text)
                    .to_owned(),
            }),
        }
    }
}

/// How long one read of a followed call waits for it to move before it is read again.
const FOLLOW_WAIT_MS: u64 = 60_000;

/// How long a device action's answer is waited for before the command says the daemon took it
/// and did not answer. A daemon whose person's part is still starting, or whose workspace cannot
/// be read, answers nothing at all: what a command does meanwhile is say so.
const ANSWERING_WITHIN: Duration = Duration::from_secs(300);

/// The code of a device action's refusal that named none.
const REFUSED: &str = "refused";

/// Why the daemon did not do a device action: its stable code, and its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRefusal {
    pub code: String,
    pub message: String,
}

impl DeviceRefusal {
    /// A refusal said in `message` alone.
    #[must_use]
    pub fn said(message: String) -> Self {
        Self {
            code: REFUSED.to_owned(),
            message,
        }
    }
}

impl std::fmt::Display for DeviceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
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
        let longest = NATIVE.len();
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
        assert!(sockets.starts_with("/var/folders"), "{}", sockets.display());
        let mode = std::fs::metadata(&sockets).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        assert_eq!(fits(&sockets), Ok(()));
    }
}
