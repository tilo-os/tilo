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

/// The system unit's profile: the daemon's home and state on Tilo OS.
pub const SYSTEM_PROFILE: &str = tilo_machine::DAEMON_SYSTEM_PROFILE;
pub use tilo_device_identity::login::daemon::{DeviceRefusal, socket_fits};
pub use tilo_machine::system_daemon;

/// Where this computer's daemon listens: the one place every client finds it, and the daemon
/// binds. Under Tilo OS's system unit its sockets are in its runtime directory; a person's own
/// service keeps them in `$XDG_RUNTIME_DIR/tilo` on Linux and in the account's temporary
/// directory (`/var/folders/…/T/tilo`) on macOS. A daemon serving a profile named apart keeps
/// them in a directory of its own beside those ([`profile_sockets`]). Its lock is in the login's
/// directory, as the device's state is: the profile's own, for a profile named apart
/// ([`serving_profile`]).
///
/// # Errors
/// The login's directory or the sockets' directory cannot be made, the sockets' directory is not
/// this account's own, or a socket's path in it is too long for a socket's address.
pub fn this_computer() -> Result<DaemonPlace, String> {
    if let Some(profile) = tilo_device_identity::login::daemon::named_profile() {
        return serving_profile(&profile);
    }
    let login = login_here()?;
    let (sockets, system) = tilo_device_identity::login::daemon::sockets(login.dir())?;
    Ok(DaemonPlace {
        system,
        ..DaemonPlace::at(&sockets, login.dir())
    })
}

/// The daemon serving a `profile` named apart (a VM host's, a stack's, a test's), as every client
/// that names the profile finds it: its sockets beside this account's own, and its lock in the
/// profile's own state directory, where the daemon keeps it whatever state directory the client
/// has. It is started by the host that named the profile, never by this account's service
/// manager, which runs this computer's own daemon over no profile.
///
/// # Errors
/// The sockets' directory cannot be made or is not this account's own private directory, or a
/// socket's path in it is too long for a socket's address.
pub fn serving_profile(profile: &Path) -> Result<DaemonPlace, String> {
    let sockets = profile_sockets(profile)?;
    // The lock's directory is the profile's own, made here as the login's is
    // ([`tilo_device_identity::login::Place::here`]), by whoever names the place.
    let device = tilo_device_identity::login::daemon::profile_device_dir(profile);
    tilo_files::native::private::directory(&device).map_err(|error| {
        format!(
            "this profile's device directory {}: {error}",
            device.display()
        )
    })?;
    let place = DaemonPlace::at(&sockets, &device);
    socket_fits(&place.native)?;
    Ok(DaemonPlace {
        starts: false,
        ..place
    })
}

/// This account's login, whose directory keeps the device's state and lock.
fn login_here() -> Result<tilo_device_identity::login::Place, String> {
    tilo_device_identity::login::Place::here(
        tilo_identity::provisioning::collaboration_identity_paths(),
    )
    .map_err(|error| format!("{error:#}"))
}

/// The sockets of the daemon serving a `profile` named apart (a test's, a VM host's, a stack's)
/// ([`tilo_device_identity::login::daemon::profile_sockets`]).
///
/// # Errors
/// The sockets' directory cannot be made, or it is not this account's own private directory.
pub fn profile_sockets(profile: &Path) -> Result<PathBuf, String> {
    tilo_device_identity::login::daemon::profile_sockets(login_here()?.dir(), profile)
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
    /// Whether this account's service manager starts it ([`service::start`]): this computer's own
    /// daemon is its own only. A daemon serving a profile named apart is started by the host that
    /// named the profile, so a client of it waits for that daemon and starts no service, which
    /// would serve no profile at all.
    starts: bool,
}

impl DaemonPlace {
    /// A daemon listening in `sockets`, keeping its lock in `dir`.
    #[must_use]
    pub fn at(sockets: &Path, dir: &Path) -> Self {
        Self {
            sockets: sockets.to_owned(),
            native: tilo_device_identity::login::daemon::native_in(sockets),
            lock: dir.join("holder.lock"),
            system: false,
            starts: true,
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

    /// The daemon's answer to a status call ([`PEER_STATUS`]), asked of the daemon that answers
    /// now: `None` when none does in `within`. Nothing is started for asking, so a command that
    /// only reads waits on the daemon a host runs and is told at once when there is none. A
    /// socket a daemon left behind is no answer either: the daemon itself must say who it is.
    #[must_use]
    pub fn status(&self, within: Duration) -> Option<serde_json::Value> {
        use std::io::{BufRead as _, Write as _};
        let mut stream = self.connect()?;
        stream.set_read_timeout(Some(within)).ok()?;
        writeln!(
            stream,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": PEER_STATUS})
        )
        .ok()?;
        let mut line = String::new();
        std::io::BufReader::new(stream).read_line(&mut line).ok()?;
        let answer: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
        answer.get("result").cloned()
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
    /// calling actions as the device makes before its first call. A daemon serving a profile
    /// named apart is started by the host that named it, and is only waited for.
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
    /// ticket's person is the one it becomes. A daemon serving a profile named apart is waited
    /// for alone.
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
    /// for a sign-in. A daemon serving a profile named apart is the host's to start, so it is
    /// waited for alone, up to `within`.
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
            if self.starts && !started && !self.alive() {
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
        self.actions().act(action, args)
    }

    /// [`Self::act`], with the refusal's code beside its words, for a caller that answers one
    /// refusal in its own way.
    ///
    /// # Errors
    /// The daemon does not answer, or refused.
    pub fn ask(
        &self,
        action: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, DeviceRefusal> {
        self.actions().ask(action, args)
    }

    /// [`Self::act`] for an action that may wait (for the person's answer at a device of theirs
    /// that keeps their root, say): its call is followed by its invocation until it settles.
    ///
    /// # Errors
    /// The daemon does not answer, or the call failed or was refused.
    pub fn act_followed(
        &self,
        action: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.actions()
            .ask_followed(action, args)
            .map_err(|refused| refused.message)
    }

    /// The device actions of this daemon, at its native endpoint.
    #[must_use]
    pub fn actions(&self) -> tilo_device_identity::login::daemon::DeviceActions {
        tilo_device_identity::login::daemon::DeviceActions::at(&self.native)
    }

    /// The record `record` of `app`, in this login's workspace, read through the one record API
    /// (`tilo_read` of `record:<app>:<record>`): the reading, its records each with the value the
    /// app keeps, or the refusal's words.
    ///
    /// # Errors
    /// The daemon does not answer, or refused the read.
    pub fn read_record(&self, app: &str, record: &str) -> Result<serde_json::Value, String> {
        let (typed, text, failed) = self
            .actions()
            .tool(
                "tilo_read",
                &serde_json::json!({ "path": format!("record:{app}:{record}") }),
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
    use std::time::{Duration, Instant};

    use super::*;

    /// A daemon that answers its status call: the answer is read from the daemon, not from a file
    /// that names it, and it is the daemon's own status ([`PEER_STATUS`]) that is asked.
    #[cfg(unix)]
    #[test]
    fn a_daemon_that_answers_is_read_its_status() {
        use std::io::{BufRead as _, Write as _};
        let dir = tempfile::tempdir().unwrap();
        let sockets = dir.path().join("sockets");
        std::fs::create_dir_all(&sockets).unwrap();
        let native = tilo_device_identity::login::daemon::native_in(&sockets);
        let listener = std::os::unix::net::UnixListener::bind(&native).unwrap();
        let answering = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut lines = std::io::BufReader::new(stream.try_clone().unwrap()).lines();
            assert_eq!(lines.next().unwrap().unwrap(), AGENT_LINE);
            let asked: serde_json::Value =
                serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            assert_eq!(
                asked["method"], PEER_STATUS,
                "the daemon is asked its own status"
            );
            writeln!(
                stream,
                "{}",
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"who": "this computer"}})
            )
            .unwrap();
        });
        let place = DaemonPlace::at(&sockets, dir.path());
        let status = place
            .status(Duration::from_secs(5))
            .expect("the daemon answers");
        assert_eq!(status["who"], "this computer");
        answering.join().unwrap();
    }

    /// A socket a daemon left behind is no daemon: the file is there and nothing answers it, so
    /// the status call says so. A call that waited for a daemon instead of reading one would
    /// sleep its bound out here.
    #[cfg(unix)]
    #[test]
    fn a_socket_a_daemon_left_behind_answers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let sockets = dir.path().join("sockets");
        std::fs::create_dir_all(&sockets).unwrap();
        let native = tilo_device_identity::login::daemon::native_in(&sockets);
        let listener = std::os::unix::net::UnixListener::bind(&native).unwrap();
        drop(listener);
        assert!(
            native.exists(),
            "the ended daemon's socket file is left behind"
        );
        let place = DaemonPlace::at(&sockets, dir.path());
        let asked = Instant::now();
        assert!(
            place.status(Duration::from_secs(2)).is_none(),
            "a socket nothing answers is no daemon"
        );
        assert!(
            asked.elapsed() < Duration::from_secs(1),
            "nothing is waited for: {:?}",
            asked.elapsed()
        );
    }

    /// A status call starts nothing: the service manager this account's own daemon would be
    /// started through is not asked, and a place whose service is this account's to start is
    /// still only read.
    #[cfg(unix)]
    #[test]
    fn a_status_call_starts_no_service() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let programs = home.root().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        let asked = home.root().join("service-manager-was-asked");
        let systemctl = programs.join("systemctl");
        std::fs::write(
            &systemctl,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n", asked.display()),
        )
        .unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();
        home.env()
            .set("PATH", &programs)
            .remove("TILO_RUNTIME_PROFILE")
            .remove("TILO_IDENTITY_ROOT");
        let place = this_computer().unwrap();
        assert!(
            place.starts,
            "this place's own daemon is one this account's service starts"
        );
        assert!(
            place.status(Duration::from_millis(200)).is_none(),
            "no daemon answers here"
        );
        assert!(
            !asked.exists(),
            "a status call asked no service manager: {}",
            std::fs::read_to_string(&asked).unwrap_or_default()
        );
    }

    /// A client of a daemon serving a profile named apart waits for the daemon the host that
    /// named the profile started, and starts no service of its own: this account's service
    /// manager runs this computer's own daemon, over no profile, so a start here would answer a
    /// place the client never asked about. The wait ends at its bound, saying no daemon answered.
    #[test]
    fn a_named_profiles_client_waits_for_its_daemon_and_starts_none() {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let profile = home.state().join("hosted");
        std::fs::create_dir_all(&profile).unwrap();
        // A service manager this account has none of: a place that started one would say that,
        // instead of waiting for the daemon the profile's host starts.
        let no_programs = home.root().join("no-programs");
        std::fs::create_dir_all(&no_programs).unwrap();
        home.env()
            .set(tilo_machine::DAEMON_PROFILE_VAR, &profile)
            .set("PATH", &no_programs)
            .remove("TILO_IDENTITY_ROOT");
        let place = this_computer().unwrap();
        assert!(!place.starts, "a profile named apart starts no service");
        let asked = Instant::now();
        let why = place
            .wait_for_native(Duration::from_millis(300))
            .expect_err("no daemon serves the profile");
        assert!(
            why.contains("isn't answering on this computer"),
            "the wait says no daemon answered: {why}"
        );
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "the wait ends at its own bound: {:?}",
            asked.elapsed()
        );
    }
}
