//! Automatic attachment at the native GUI boundary. No process environment is
//! mutated: the host supplies connected descriptors when it execs the app again.
//!
//! A launch is asked of this computer's daemon over its native endpoint
//! ([`LaunchRequest`]): what runs is the package the daemon has installed,
//! never a program the caller names. An app the launcher started asks for
//! another instance of itself that way ([`launch_another_instance`]); a
//! program a person runs from a terminal attaches with [`ensure_attached`].
use std::io;

/// The value of [`LaunchRequest::native`] that asks this computer's daemon to start an app.
pub const LAUNCH: &str = "launch";

/// `{"native":"launch", ...}`: a program asking this computer's daemon to start an app.
///
/// It names only the app, the workspace and whether it is the app's default
/// presentation. What runs — the program, its sandbox, its grants and the
/// records it reaches — is the installed package this computer resolves the
/// name to: a program is never one the caller names.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub native: String,
    pub app_id: String,
    /// The workspace the app opens in: any this computer holds for its person.
    pub workspace: String,
    /// Whether this starts the app's default presentation, which one already shown answers.
    pub presentation: bool,
    /// What the app captures, when this launches it to capture rather than to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<Capture>,
}

/// The capture a launch runs: the app's own capture flags, as the app parses them
/// (`--headless-drive`, `--shots`, `--shots-dir`, `--scenario`). The device starts the app the way
/// a window launch starts it, with the connector it runs for the app's origin, and waits for the
/// capture to end.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    /// The app's own capture arguments, handed to it as given: the app's parser decides what they
    /// mean, and refuses those it does not have.
    pub args: Vec<String>,
}

impl Capture {
    /// The app's own arguments this capture runs with, checked to be an argv at all.
    ///
    /// # Errors
    /// No argument, too many, or one too long to be an argument.
    pub fn args(&self) -> io::Result<&[String]> {
        if self.args.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a capture names what it captures",
            ));
        }
        if self.args.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a capture names at most 64 arguments",
            ));
        }
        for argument in &self.args {
            if argument.len() > 16384 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a capture's argument is too long",
                ));
            }
        }
        Ok(&self.args)
    }
}

impl LaunchRequest {
    /// The launch of `app_id` in `workspace`: its default presentation when `presentation`, else an
    /// instance of its own.
    #[must_use]
    pub fn new(app_id: &str, workspace: &str, presentation: bool) -> Self {
        Self {
            native: LAUNCH.to_owned(),
            app_id: app_id.to_owned(),
            workspace: workspace.to_owned(),
            presentation,
            capture: None,
        }
    }

    /// This launch running `args`, the app's own capture arguments, rather than showing a window.
    #[must_use]
    pub fn capturing(mut self, args: &[String]) -> Self {
        self.capture = Some(Capture {
            args: args.to_vec(),
        });
        self
    }

    /// Whether `request`, a native endpoint's first line, asks this computer to start an app.
    #[must_use]
    pub fn is_launch(request: &[u8]) -> bool {
        serde_json::from_slice::<serde_json::Value>(request).is_ok_and(|request| {
            request.get("native").and_then(serde_json::Value::as_str) == Some(LAUNCH)
        })
    }

    /// This request as the one line the daemon's native endpoint reads.
    ///
    /// # Errors
    /// The request does not encode.
    pub fn line(&self) -> serde_json::Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        Ok(line)
    }
}

/// The value of [`ShellRecord::native`] that tells this computer's daemon of a shell started for
/// an app.
pub const SHELL: &str = "shell";

/// `{"native":"shell", ...}`: the device's shell runtime telling this computer's daemon of a shell
/// it started for an app (Terminal above all) in a workspace, answered `{"workspace"}` once
/// recorded, `{"unseen": true}` by a daemon that cannot see the account's processes (one that runs
/// as an account of its own), or `{"error"}`. From then on the daemon judges that shell, and every
/// process started in it that connects to it directly, as the app: with the app's grants in that
/// workspace, never the authority of the account it runs as. A daemon that cannot see them judges
/// only the connections the shell's own endpoint asks for ([`ShellLine`]).
///
/// The daemon takes it from a process of the person's own that no app's launch is above, and for a
/// process below the one asking alone: a program can only ever narrow what its own children
/// reach. The start time is the one the runtime read as it started the shell, so a process that
/// took the pid since is never taken for it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellRecord {
    pub native: String,
    /// The shell's process.
    pub pid: u32,
    /// When the shell's process started, as the kernel counts it (`/proc/<pid>/stat`'s
    /// `starttime` on Linux).
    pub started: u64,
    /// The app the shell is for.
    pub app: String,
    /// The workspace the shell runs in: the runtime's primary one when empty.
    pub workspace: String,
}

impl ShellRecord {
    /// The record of `pid`, started at `started`, as `app`'s shell in `workspace`.
    #[must_use]
    pub fn new(pid: u32, started: u64, app: &str, workspace: &str) -> Self {
        Self {
            native: SHELL.to_owned(),
            pid,
            started,
            app: app.to_owned(),
            workspace: workspace.to_owned(),
        }
    }

    /// This record as the one line the daemon's native endpoint reads.
    ///
    /// # Errors
    /// The record does not encode.
    pub fn line(&self) -> serde_json::Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        Ok(line)
    }
}

/// The value of [`ShellLine::native`] that asks this computer's daemon for a connection of an
/// app's shell.
pub const SHELL_LINE: &str = "shell-line";

/// `{"native":"shell-line", ...}`: the device's shell runtime asking this computer's daemon for a
/// connection that acts as an app (Terminal above all) in a workspace, on behalf of a program in a
/// shell it started for that app. Answered `{"workspace"}` with the connection as the line's one
/// descriptor, or `{"error"}`.
///
/// The runtime serves each shell an endpoint of its own and asks for one of these for each
/// connection a program in the shell makes there, so the program reaches the daemon as the app
/// and never as the account it runs as, whether or not the daemon can see the program's process.
/// The daemon answers it to a program of the person's own that no app's launch is above: a
/// connection it hands only ever acts with less than the program asking for it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellLine {
    pub native: String,
    /// The app the connection acts as.
    pub app: String,
    /// The workspace it acts in: the runtime's primary one when empty.
    pub workspace: String,
    /// The ticket of the daemon's launch of the app the shell runs for, when the runtime knows
    /// it: the connection is made only while that launch of the app runs in the workspace.
    #[serde(default)]
    pub ticket: Option<u64>,
}

impl ShellLine {
    /// The ask for a connection of `app` in `workspace`.
    #[must_use]
    pub fn new(app: &str, workspace: &str) -> Self {
        Self {
            native: SHELL_LINE.to_owned(),
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            ticket: None,
        }
    }

    /// This ask as the one line the daemon's native endpoint reads.
    ///
    /// # Errors
    /// The ask does not encode.
    pub fn line(&self) -> serde_json::Result<Vec<u8>> {
        let mut line = serde_json::to_vec(self)?;
        line.push(b'\n');
        Ok(line)
    }
}

/// Ask this computer's daemon to start another instance of `app_id` in this app's workspace, over
/// the native service connection the launcher handed this app: the launch a person's `--run-app`
/// makes, which starts the installed package's own program and records the instance like any
/// other. A launched app that wants a second process with a connection of its own asks here; it
/// never makes a connection by re-running the daemon.
///
/// # Errors
/// This program is not an app the launcher started, or the daemon refused the launch, saying why.
pub fn launch_another_instance(app_id: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::launch_another_instance(app_id)
    }
    #[cfg(not(unix))]
    {
        let _ = app_id;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a second instance of an app is asked of the daemon that started it, which this \
             platform has none of",
        ))
    }
}

/// The automatic host records which process received its descriptor transfer.
/// A normal later `Command` child retains environment strings, not that transfer.
pub(crate) fn connection_owner() -> io::Result<Option<u32>> {
    std::env::var_os("TILO_RUNTIME_CONNECTION_OWNER_PID")
        .map(|value| {
            value
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|pid| *pid != 0)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "inherited runtime connection owner must be a nonzero process ID",
                    )
                })
        })
        .transpose()
}

pub(crate) fn connection_metadata_is_current() -> bool {
    connection_owner().is_ok_and(|owner| owner.is_none_or(|pid| pid == std::process::id()))
}

/// This computer's runtime host, for the program at `program`: `TILO_RUNTIME_BINARY`, else beside
/// the program, else on `PATH` (`/usr/bin` on a Tilo or RPM install), else a checkout's build.
/// What an app attaches to, and what the host's search providers start.
///
/// # Errors
///
/// No host in any of those places, or a `TILO_RUNTIME_BINARY` that names no executable.
#[cfg(unix)]
pub fn locate_host(program: &std::path::Path) -> io::Result<std::path::PathBuf> {
    unix::locate_host(program)
}

/// Join the shared native runtime, or retain an inherited launcher connection.
///
/// # Errors
/// Invalid inherited descriptors, unavailable hosts and failed execs are
/// reported; they never silently select a different authority.
pub fn ensure_attached(app_id: &str) -> io::Result<()> {
    // Android has no runtime host to re-exec under: its apps run as they are.
    #[cfg(target_os = "android")]
    {
        let _ = app_id;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "android")))]
    {
        unix::ensure_attached(app_id)
    }
    #[cfg(not(unix))]
    {
        let _ = app_id;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "automatic native runtime attachment requires Unix",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use std::{
        env,
        ffi::OsStr,
        io::{self, BufRead as _, Write as _},
        os::{
            fd::FromRawFd as _,
            unix::{fs::PermissionsExt, net::UnixStream},
        },
        path::{Path, PathBuf},
        time::Duration,
    };
    // Android has no runtime host to re-exec under: its apps run as they are, so nothing
    // attaches and the code that would is not built there.
    #[cfg(not(target_os = "android"))]
    use std::{os::unix::process::CommandExt as _, process::Command};

    use super::LaunchRequest;

    fn invalid(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, message)
    }

    #[cfg(not(target_os = "android"))]
    fn inherited() -> io::Result<bool> {
        let owner = super::connection_owner()?;
        if owner.is_some_and(|pid| pid != std::process::id()) {
            return Ok(false);
        }
        let collab = env::var_os("TILO_SVC_COLLAB_FD");
        let identity = env::var_os("TILO_SVC_IDENTITY_FD");
        if collab.is_none() && identity.is_none() && owner.is_none() {
            return Ok(false);
        }
        // The automatic host hands over both services. The launcher hands
        // over the ones the launch has: an app that declares no
        // collaboration service still gets its identity broker, and a session
        // without an identity service still hands over collaboration. Every
        // descriptor that is named must be valid either way.
        let named: Vec<&Option<std::ffi::OsString>> = if owner.is_some() {
            vec![&collab, &identity]
        } else {
            [&collab, &identity]
                .into_iter()
                .filter(|value| value.is_some())
                .collect()
        };
        let mut descriptors = Vec::new();
        for value in named {
            let fd = value
                .as_deref()
                .and_then(OsStr::to_str)
                .and_then(|raw| raw.parse::<i32>().ok())
                .filter(|fd| *fd >= 3)
                .ok_or_else(|| {
                    invalid("inherited runtime connections are incomplete or malformed")
                })?;
            // An ordinary child keeps the environment strings, not the transfer: the process
            // that took those connections closed them before it started this one. A name whose
            // descriptor is gone is a handoff this process did not receive, so it attaches
            // itself rather than reading a stale name as a malformed handoff.
            // SAFETY: fcntl only inspects a descriptor number here; it borrows nothing.
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0
                && io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
            {
                return Ok(false);
            }
            // SAFETY: getpeername only inspects an existing descriptor; it does
            // not borrow ownership or close descriptors belonging to SDK code.
            let connected = unsafe {
                let mut address: libc::sockaddr_storage = std::mem::zeroed();
                let mut length = libc::socklen_t::try_from(std::mem::size_of_val(&address))
                    .map_err(io::Error::other)?;
                let mut socket_type: libc::c_int = 0;
                let mut type_length =
                    libc::socklen_t::try_from(std::mem::size_of_val(&socket_type))
                        .map_err(io::Error::other)?;
                libc::getpeername(fd, (&raw mut address).cast(), &raw mut length) == 0
                    && i32::from(address.ss_family) == libc::AF_UNIX
                    && libc::getsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        libc::SO_TYPE,
                        (&raw mut socket_type).cast(),
                        &raw mut type_length,
                    ) == 0
                    && socket_type == libc::SOCK_STREAM
            };
            if !connected {
                return Err(invalid(
                    "inherited runtime descriptor is not a connected Unix socket",
                ));
            }
            descriptors.push(fd);
        }
        if descriptors.len() > 1 && descriptors.first() == descriptors.get(1) {
            return Err(invalid(
                "inherited runtime services must have distinct descriptors",
            ));
        }
        Ok(true)
    }

    #[cfg(not(target_os = "android"))]
    pub(super) fn ensure_attached(app_id: &str) -> io::Result<()> {
        if inherited()? {
            if let Some(raw) = env::var_os("TILO_RUNTIME_LAUNCH_GUARD_FD") {
                let fd = raw
                    .to_str()
                    .and_then(|raw| raw.parse::<i32>().ok())
                    .filter(|fd| *fd >= 3)
                    .ok_or_else(|| invalid("invalid native launch guard"))?;
                // SAFETY: this changes only descriptor flags on the inherited
                // lifetime guard. Keep its lock, but never pass it to an exec child.
                let changed = unsafe {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    flags >= 0 && libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) >= 0
                };
                if !changed {
                    return Err(io::Error::last_os_error());
                }
            }
            return Ok(());
        }
        if app_id.trim().is_empty() || app_id.len() > 4096 {
            return Err(invalid("invalid application identity"));
        }
        let executable = env::current_exe()?;
        let host = locate_host(&executable)?;
        // The daemon this computer runs starts the app it has installed: this process names the
        // app and nothing else, and the host's asking ends it.
        Err(Command::new(host).arg("--run-app").arg(app_id).exec())
    }

    /// How long the exchange with the daemon may take: the daemon starts the instance through the
    /// launcher before it answers, and a slow start is still a start.
    const LAUNCH_ANSWER_BOUND: Duration = Duration::from_secs(60);

    /// Why a program with no inherited connection cannot be given an instance: what a person
    /// can do about it, as [`ensure_attached`]'s own refusal says.
    fn not_launched(app_id: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "this program is not an app this computer launched, so it has no instance of its \
                 own to ask for: no native service connection (`TILO_SVC_NATIVE_FD`) or workspace \
                 was handed to it; run it with `tilo-runtime --run-app {app_id}` and name \
                 `native` in its [sandbox.services]"
            ),
        )
    }

    pub(super) fn launch_another_instance(app_id: &str) -> io::Result<()> {
        let workspace = env::var("TILO_WORKSPACE_ID")
            .ok()
            .filter(|workspace| !workspace.trim().is_empty())
            .ok_or_else(|| not_launched(app_id))?;
        let fd = crate::service_fd("native").ok_or_else(|| not_launched(app_id))?;
        // The exchange runs on a duplicate: the app's own native connection stays open, and this
        // process keeps it for whatever else it reaches there.
        // SAFETY: `fcntl` duplicates an open descriptor into one this process owns.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `duplicate` is a descriptor this process owns and nothing else uses.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicate) };
        let mut stream = UnixStream::from(owned);
        stream.set_read_timeout(Some(LAUNCH_ANSWER_BOUND))?;
        stream.set_write_timeout(Some(LAUNCH_ANSWER_BOUND))?;
        stream.write_all(&LaunchRequest::new(app_id, &workspace, false).line()?)?;
        stream.flush()?;
        let mut answer = String::new();
        std::io::BufReader::new(&stream).read_line(&mut answer)?;
        let answer: serde_json::Value = serde_json::from_str(answer.trim()).map_err(|error| {
            io::Error::other(format!("the launch's answer, {answer:?}: {error}"))
        })?;
        match answer.get("error").and_then(serde_json::Value::as_str) {
            Some(error) => Err(io::Error::other(error.to_owned())),
            None => Ok(()),
        }
    }

    fn executable(path: &Path) -> bool {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }

    /// The runtime host an app attaches to: this computer's one daemon, the same program on a
    /// Tilo desktop and anywhere else, so an app never reaches a second authority beside it.
    const HOST: &str = "tilo-runtime";

    pub(super) fn locate_host(app: &Path) -> io::Result<PathBuf> {
        if let Some(path) = env::var_os("TILO_RUNTIME_BINARY") {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !executable(&path) {
                return Err(invalid(
                    "TILO_RUNTIME_BINARY must name an absolute executable file",
                ));
            }
            return Ok(path);
        }
        if let Some(parent) = app.parent() {
            let sibling = parent.join(HOST);
            if executable(&sibling) {
                return Ok(sibling);
            }
        }
        if let Some(paths) = env::var_os("PATH") {
            for directory in env::split_paths(&paths) {
                let candidate = directory.join(HOST);
                if executable(&candidate) {
                    return candidate.canonicalize();
                }
            }
        }
        if let Some(target) = checkout_target(app) {
            for profile in ["debug", "thin-app", "release"] {
                let candidate = target.join(profile).join(HOST);
                if executable(&candidate) {
                    return candidate.canonicalize();
                }
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no runtime host found; in this checkout run `cargo build --locked -p tilo-daemon --bin {HOST}` once, then rerun the app"
                ),
            ));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no runtime host found; install {HOST} beside the app or on PATH"),
        ))
    }

    /// A source app may use the host built by Cargo's default profile even
    /// when the app itself uses thin-app. Installed apps never search another
    /// profile or another checkout, and desktop host selection stays intact.
    fn checkout_target(app: &Path) -> Option<PathBuf> {
        let target = app.parent()?.parent()?;
        let root = target.parent()?;
        (target.file_name() == Some(OsStr::new("target"))
            && root.join("Cargo.toml").is_file()
            && root.join("crates/core/tilo-runtime/Cargo.toml").is_file())
        .then(|| target.to_path_buf())
    }
}

#[cfg(test)]
mod launch_request_tests {
    use super::{LAUNCH, LaunchRequest};

    #[test]
    fn a_launch_names_only_the_app_the_workspace_and_the_presentation() {
        let request = LaunchRequest::new("dev.tilo.app", "workspace-one", false);
        let line = request.line().expect("a request line");
        assert!(line.ends_with(b"\n"), "one line: {line:?}");
        assert!(
            LaunchRequest::is_launch(&line),
            "the daemon reads it as a launch"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&line).expect("json"),
            serde_json::json!({
                "native": LAUNCH,
                "app_id": "dev.tilo.app",
                "workspace": "workspace-one",
                "presentation": false,
            })
        );
        assert_eq!(
            serde_json::from_slice::<LaunchRequest>(&line).expect("the daemon's own reading"),
            request
        );
        // A program is never named: what runs is the package the daemon has installed.
        let named = br#"{"native":"launch","app_id":"a","workspace":"w","presentation":false,"executable":"/bin/sh"}"#;
        assert!(
            serde_json::from_slice::<LaunchRequest>(named).is_err(),
            "a launch naming a program is refused"
        );
        // Anything else on the endpoint is not a launch.
        assert!(!LaunchRequest::is_launch(b"{\"native\":\"status\"}\n"));
        assert!(!LaunchRequest::is_launch(b"not json\n"));
    }
}
