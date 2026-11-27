//! The client half of the control protocol: one JSON request and one JSON
//! answer per line, over a desktop's control socket.
//!
//! `tilo-control` (the developer's and admin's tool) and `tilo` (what a
//! person types) both speak it through this module, so there is one client
//! and one protocol. What differs is only which socket a command reaches —
//! [`ControlSocket`] — and how the answer is shown. The device's daemon
//! answers the action model's frames at its native endpoint whether or not a
//! desktop session runs: the session serves its windows and seat there as
//! providers of the daemon's one dispatch.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tilo_files::native::socket::Stream;

/// Connect to the control socket at `path`.
pub fn connect(path: &Path) -> io::Result<Stream> {
    tilo_files::native::socket::connect(path)
}

/// Send `request` (one JSON object, no newline) and read one answer line.
///
/// # Errors
/// The connection broke, or the answer was not JSON.
pub fn round_trip(stream: &mut Stream, request: &str) -> io::Result<serde_json::Value> {
    let mut line = request.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut answer = String::new();
    if reader.read_line(&mut answer)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the desktop closed the connection without answering",
        ));
    }
    serde_json::from_str(answer.trim()).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A socket that answers the action model's frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlSocket {
    /// Named by `$TILO_TEST_CONTROL` (a development session) or a caller.
    Named(PathBuf),
    /// The device admin's own socket: their login or ssh, never an app.
    Admin(PathBuf),
    /// The session's MCP server: the socket a process inside an app the
    /// session launched — a terminal, and what runs in it — reaches. It acts
    /// with that app's workspace scope, never the admin's authority.
    Agent(PathBuf),
    /// The device daemon's native endpoint, which answers the frames as
    /// whoever the kernel says is on the other end, in each workspace a
    /// frame names, desktop session or not.
    Daemon(PathBuf),
}

impl ControlSocket {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Named(path) | Self::Admin(path) | Self::Agent(path) | Self::Daemon(path) => path,
        }
    }
}

/// The sockets a semantic request may go through, in the order to try them:
/// the named development socket alone when `$TILO_TEST_CONTROL` is set;
/// where a desktop session serves its admin socket in the private runtime
/// directory, that socket, then the session's MCP server, which serves a
/// process inside an app it launched with that app's workspace scope;
/// otherwise the device's daemon at `daemon`. A process a session turns away
/// is never handed on to the daemon, which would answer it as the person.
#[must_use]
pub fn semantic_sockets(
    test_control: Option<OsString>,
    runtime_dir: Option<OsString>,
    daemon: Option<PathBuf>,
) -> Vec<ControlSocket> {
    if let Some(named) = test_control.filter(|named| !named.is_empty()) {
        return vec![ControlSocket::Named(PathBuf::from(named))];
    }
    let session = tilo_paths::login::session_dir(runtime_dir).filter(|session| {
        session
            .join(tilo_paths::login::ADMIN_CONTROL_SOCKET_FILE)
            .exists()
    });
    match session {
        Some(session) => vec![
            ControlSocket::Admin(session.join(tilo_paths::login::ADMIN_CONTROL_SOCKET_FILE)),
            ControlSocket::Agent(session.join(tilo_paths::login::AGENT_SOCKET_FILE)),
        ],
        None => daemon.map(ControlSocket::Daemon).into_iter().collect(),
    }
}

/// The sockets of this process's own environment.
#[must_use]
pub fn sockets_from_env() -> Vec<ControlSocket> {
    semantic_sockets(
        std::env::var_os(crate::TEST_CONTROL_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
        daemon_socket(std::env::var_os(tilo_machine::DAEMON_PROFILE_VAR)),
    )
}

/// The native endpoint of the daemon this process reaches: the one serving
/// the profile `named` names apart (a test's, a stack's, a VM host's), which
/// links its endpoint into that profile where every client of the profile
/// finds it; else this computer's own.
#[must_use]
pub fn daemon_socket(named: Option<OsString>) -> Option<PathBuf> {
    // A program in a shell reaches the daemon at the endpoint its shell was given, as the app the
    // shell is for.
    if let Some(shells) = tilo_machine::named_daemon_native() {
        return Some(shells);
    }
    match named.filter(|named| !named.is_empty()).map(PathBuf::from) {
        Some(profile)
            if !tilo_machine::serves_the_computer(
                tilo_machine::system_daemon(),
                Some(&profile),
            ) =>
        {
            Some(profile.join(tilo_machine::DAEMON_NATIVE))
        }
        _ => tilo_machine::daemon_native().ok(),
    }
}

/// Whether a desktop session serves any of `sockets` here: something exists
/// at the path of one of the session's own. What a person may do through it
/// is the desktop's to decide, at the first request.
#[must_use]
pub fn desktop_present(sockets: &[ControlSocket]) -> bool {
    sockets
        .iter()
        .filter(|socket| !matches!(socket, ControlSocket::Daemon(_)))
        .any(|socket| socket.path().exists())
}

/// Why no socket answered.
#[derive(Debug)]
pub enum Unreachable {
    /// No desktop session serves a socket here.
    NoDesktop,
    /// Every socket that exists turned this process away.
    Refused {
        socket: ControlSocket,
        message: String,
    },
    /// A socket broke off mid-conversation.
    Io {
        socket: ControlSocket,
        error: io::Error,
    },
}

/// A connection to the desktop's action model through the first socket that
/// serves this process.
pub struct Desktop {
    socket: ControlSocket,
    stream: Stream,
    /// Control sockets after `socket`, tried when it turns this process away.
    rest: Vec<ControlSocket>,
    /// Whether `socket` has answered a request; only a first answer can be a
    /// refusal of the process itself.
    answered: bool,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("socket", &self.socket)
            .field("rest", &self.rest)
            .finish_non_exhaustive()
    }
}

impl Desktop {
    /// Connect through the first of `sockets` that accepts a connection.
    ///
    /// # Errors
    /// [`Unreachable::NoDesktop`] when none does.
    pub fn connect(sockets: &[ControlSocket]) -> Result<Self, Unreachable> {
        let mut remaining = sockets.to_vec();
        while !remaining.is_empty() {
            let socket = remaining.remove(0);
            if let Ok(stream) = connect(socket.path()) {
                return Ok(Self {
                    socket,
                    stream,
                    rest: remaining,
                    answered: false,
                });
            }
        }
        Err(Unreachable::NoDesktop)
    }

    /// The socket this connection goes through now.
    #[must_use]
    pub fn socket(&self) -> &ControlSocket {
        &self.socket
    }

    /// One request, one answer. A socket that turns this process away on its
    /// first answer — the admin's socket refuses a sandboxed app and what runs in
    /// one — hands the request on to the next socket.
    ///
    /// # Errors
    /// Every socket refused, or the connection broke.
    pub fn request(
        &mut self,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, Unreachable> {
        let line = request.to_string();
        loop {
            let answer = match round_trip(&mut self.stream, &line) {
                Ok(answer) => answer,
                Err(error) => {
                    let broke = Unreachable::Io {
                        socket: self.socket.clone(),
                        error,
                    };
                    if self.answered {
                        return Err(broke);
                    }
                    // Closed on us before any answer: the next socket may serve.
                    self.advance(broke)?;
                    continue;
                }
            };
            if !self.answered
                && let Some(message) = refusal_of_process(&self.socket, &answer)
            {
                self.advance(Unreachable::Refused {
                    socket: self.socket.clone(),
                    message,
                })?;
                continue;
            }
            self.answered = true;
            return Ok(answer);
        }
    }

    /// Move on to the next socket that accepts a connection, or give back
    /// `why` the current one did not serve when none is left.
    fn advance(&mut self, why: Unreachable) -> Result<(), Unreachable> {
        while !self.rest.is_empty() {
            let socket = self.rest.remove(0);
            if let Ok(stream) = connect(socket.path()) {
                self.socket = socket;
                self.stream = stream;
                self.answered = false;
                return Ok(());
            }
        }
        Err(why)
    }
}

/// Whether `answer`, a socket's first, turns away the process itself rather
/// than answering its request: the admin socket's `not_permitted` refusal
/// of a connection. Returns the socket's own words, for logs; a person is
/// shown a plain sentence instead. The daemon is the last socket tried, and
/// what it answers is its answer.
#[must_use]
pub fn refusal_of_process(socket: &ControlSocket, answer: &serde_json::Value) -> Option<String> {
    match socket {
        ControlSocket::Admin(_) => {
            let err = answer.get("err")?;
            (err.get("code").and_then(serde_json::Value::as_str) == Some("not_permitted")).then(
                || {
                    err.get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                },
            )
        }
        ControlSocket::Agent(_) => {
            // The control protocol answers `answer` or `err`; a JSON-RPC
            // error is the socket's MCP framing refusing before any request.
            answer.get("jsonrpc")?;
            Some(
                answer
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        ControlSocket::Named(_) | ControlSocket::Daemon(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_are_the_named_one_alone_the_sessions_or_the_daemon() {
        let daemon = || Some(PathBuf::from("/run/user/7/tilo/native.sock"));
        let run = tempfile::tempdir().unwrap();
        assert_eq!(
            semantic_sockets(
                Some("/run/dev/control.sock".into()),
                Some(run.path().into()),
                daemon(),
            ),
            [ControlSocket::Named("/run/dev/control.sock".into())]
        );
        // No desktop session here: the daemon alone answers.
        assert_eq!(
            semantic_sockets(Some(OsString::new()), Some(run.path().into()), daemon()),
            [ControlSocket::Daemon("/run/user/7/tilo/native.sock".into())]
        );
        assert!(semantic_sockets(None, None, None).is_empty());
        // A desktop session here: its admin socket, then its MCP server, and never the daemon,
        // which would answer a process the session turned away as the person.
        let session = run.path().join("tilo-session");
        std::fs::create_dir(&session).unwrap();
        std::fs::write(session.join("admin-control.sock"), "").unwrap();
        assert_eq!(
            semantic_sockets(None, Some(run.path().into()), daemon()),
            [
                ControlSocket::Admin(session.join("admin-control.sock")),
                ControlSocket::Agent(session.join("agent.sock")),
            ]
        );
    }

    #[test]
    fn a_profile_named_apart_is_reached_where_its_daemon_links_its_endpoint() {
        assert_eq!(
            daemon_socket(Some("/home/me/stack/profile".into())),
            Some(PathBuf::from("/home/me/stack/profile/native.sock"))
        );
    }

    #[test]
    fn only_the_sessions_refusals_of_the_process_hand_on() {
        let admin = ControlSocket::Admin("/a".into());
        let refused = serde_json::json!({"err": {"code": "not_permitted", "message": "no"}});
        let mcp = serde_json::json!({"jsonrpc": "2.0", "error": {"message": "not admitted"}});
        assert_eq!(
            refusal_of_process(&ControlSocket::Agent("/b".into()), &mcp).as_deref(),
            Some("not admitted")
        );
        assert_eq!(refusal_of_process(&admin, &refused).as_deref(), Some("no"));
        let unknown = serde_json::json!({"err": {"code": "unknown_action", "message": "x"}});
        assert_eq!(refusal_of_process(&admin, &unknown), None);
        for answering in [
            ControlSocket::Named("/c".into()),
            ControlSocket::Daemon("/d".into()),
        ] {
            assert_eq!(refusal_of_process(&answering, &refused), None);
        }
    }

    #[test]
    fn the_daemon_is_no_desktop_session() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join("native.sock");
        std::fs::write(&native, "").unwrap();
        assert!(!desktop_present(&[
            ControlSocket::Admin(dir.path().join("admin-control.sock")),
            ControlSocket::Daemon(native),
        ]));
    }
}
