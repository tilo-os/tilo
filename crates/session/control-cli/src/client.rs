//! The client half of the control protocol: one JSON request and one JSON
//! answer per line, over a desktop's control socket.
//!
//! `tilo-control` (the developer's and admin's tool) and `tilo` (what a
//! person types) both speak it through this module, so there is one client
//! and one protocol. What differs is only which socket a command reaches —
//! [`Door`] — and how the answer is shown.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// Connect to the control socket at `path`.
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// Send `request` (one JSON object, no newline) and read one answer line.
///
/// # Errors
/// The connection broke, or the answer was not JSON.
pub fn round_trip(stream: &mut UnixStream, request: &str) -> io::Result<serde_json::Value> {
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

/// The `actions_invoke` request, carrying `device` and `wait_ms` only when
/// they are given.
#[must_use]
pub fn invoke_wire(
    provider: &str,
    id: &str,
    args: &serde_json::Value,
    device: Option<&str>,
    wait_ms: Option<u64>,
) -> io::Result<serde_json::Value> {
    let args = args.as_object().cloned().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "action arguments must be an object",
        )
    })?;
    let request: crate::protocol::Command = crate::protocol::Command::ActionsInvoke {
        provider: provider.to_owned(),
        id: id.to_owned(),
        args,
        device: device.map(str::to_owned),
        wait_ms,
    };
    serde_json::to_value(request).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// `id` with every control character written as `\xHH`, so a terminal
/// shows each part and a person can copy it back into an invocation.
#[must_use]
pub fn printable_id(id: &str) -> std::borrow::Cow<'_, str> {
    use std::fmt::Write as _;
    if !id.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(id);
    }
    let mut out = String::with_capacity(id.len().saturating_add(8));
    for ch in id.chars() {
        // Every control character is below U+0100, so two digits name it.
        if ch.is_control() {
            let _ = write!(out, "\\x{:02x}", u32::from(ch));
        } else {
            out.push(ch);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// [`printable_id`] undone: `\xHH` of a control character back to the
/// character. Ids never hold such text themselves, so a raw id (from
/// `--json`) passes through unchanged.
#[must_use]
pub fn id_from_printed(printed: &str) -> String {
    let mut out = String::with_capacity(printed.len());
    let mut chars = printed.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let mut ahead = chars.clone();
            let escaped = (ahead.next() == Some('x'))
                .then(|| Some([ahead.next()?, ahead.next()?]))
                .flatten()
                .and_then(|[high, low]| {
                    high.to_digit(16)?
                        .checked_mul(16)?
                        .checked_add(low.to_digit(16)?)
                })
                .and_then(char::from_u32)
                .filter(|decoded| decoded.is_control());
            if let Some(decoded) = escaped {
                out.push(decoded);
                chars = ahead;
                continue;
            }
        }
        out.push(ch);
    }
    out
}

/// A socket that answers the action model's semantic requests
/// (`actions_enumerate`, `actions_invoke`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Door {
    /// Named by `$TILO_TEST_CONTROL` (a development session) or a caller.
    Named(PathBuf),
    /// The device admin's own socket: their login or ssh, never an app.
    Admin(PathBuf),
    /// The session's agent door: the socket a process inside an app the
    /// session launched — a terminal, and what runs in it — reaches. It acts
    /// with that app's workspace scope, never the admin's authority.
    Agent(PathBuf),
}

impl Door {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Named(path) | Self::Admin(path) | Self::Agent(path) => path,
        }
    }
}

/// The doors a semantic request may go through, in the order to try them:
/// the named development socket alone when `$TILO_TEST_CONTROL` is set;
/// otherwise the admin's socket, then the agent door. Empty without a
/// private runtime directory.
#[must_use]
pub fn semantic_doors(test_control: Option<OsString>, runtime_dir: Option<OsString>) -> Vec<Door> {
    if let Some(named) = test_control.filter(|named| !named.is_empty()) {
        return vec![Door::Named(PathBuf::from(named))];
    }
    let Some(session) = tilo_paths::login::session_dir(runtime_dir) else {
        return Vec::new();
    };
    vec![
        Door::Admin(session.join(tilo_paths::login::ADMIN_CONTROL_SOCKET_FILE)),
        Door::Agent(session.join(tilo_paths::login::AGENT_SOCKET_FILE)),
    ]
}

/// The doors of this process's own environment.
#[must_use]
pub fn doors_from_env() -> Vec<Door> {
    semantic_doors(
        std::env::var_os(crate::TEST_CONTROL_ENV),
        std::env::var_os("XDG_RUNTIME_DIR"),
    )
}

/// Whether a desktop session serves any of `doors` here: something exists
/// at one of their paths. What a person may do through it is the desktop's
/// to decide, at the first request.
#[must_use]
pub fn desktop_present(doors: &[Door]) -> bool {
    doors.iter().any(|door| door.path().exists())
}

/// Why no door answered.
#[derive(Debug)]
pub enum Unreachable {
    /// No desktop session serves a door here.
    NoDesktop,
    /// Every door that exists turned this process away.
    Refused { door: Door, message: String },
    /// A door broke off mid-conversation.
    Io { door: Door, error: io::Error },
}

/// A connection to the desktop's action model through the first door that
/// serves this process.
pub struct Desktop {
    door: Door,
    stream: UnixStream,
    /// Doors after `door`, tried when it turns this process away.
    rest: Vec<Door>,
    /// Whether `door` has answered a request; only a first answer can be a
    /// refusal of the process itself.
    answered: bool,
}

impl std::fmt::Debug for Desktop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Desktop")
            .field("door", &self.door)
            .field("rest", &self.rest)
            .finish_non_exhaustive()
    }
}

impl Desktop {
    /// Connect through the first of `doors` that accepts a connection.
    ///
    /// # Errors
    /// [`Unreachable::NoDesktop`] when none does.
    pub fn connect(doors: &[Door]) -> Result<Self, Unreachable> {
        let mut remaining = doors.to_vec();
        while !remaining.is_empty() {
            let door = remaining.remove(0);
            if let Ok(stream) = connect(door.path()) {
                return Ok(Self {
                    door,
                    stream,
                    rest: remaining,
                    answered: false,
                });
            }
        }
        Err(Unreachable::NoDesktop)
    }

    /// The door this connection goes through now.
    #[must_use]
    pub fn door(&self) -> &Door {
        &self.door
    }

    /// One request, one answer. A door that turns this process away on its
    /// first answer — the admin's socket refuses every app and what runs in
    /// one — hands the request on to the next door.
    ///
    /// # Errors
    /// Every door refused, or the connection broke.
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
                        door: self.door.clone(),
                        error,
                    };
                    if self.answered {
                        return Err(broke);
                    }
                    // Closed on us before any answer: the next door may serve.
                    self.advance(broke)?;
                    continue;
                }
            };
            if !self.answered
                && let Some(message) = refusal_of_process(&self.door, &answer)
            {
                self.advance(Unreachable::Refused {
                    door: self.door.clone(),
                    message,
                })?;
                continue;
            }
            self.answered = true;
            return Ok(answer);
        }
    }

    /// Move on to the next door that accepts a connection, or give back
    /// `why` the current one did not serve when none is left.
    fn advance(&mut self, why: Unreachable) -> Result<(), Unreachable> {
        while !self.rest.is_empty() {
            let door = self.rest.remove(0);
            if let Ok(stream) = connect(door.path()) {
                self.door = door;
                self.stream = stream;
                self.answered = false;
                return Ok(());
            }
        }
        Err(why)
    }
}

/// Whether `answer`, a door's first, turns away the process itself rather
/// than answering its request: the admin socket's `not_permitted` refusal
/// of a connection, or the agent door's JSON-RPC error. Returns the door's
/// own words, for logs; a person is shown a plain sentence instead.
#[must_use]
pub fn refusal_of_process(door: &Door, answer: &serde_json::Value) -> Option<String> {
    match door {
        Door::Admin(_) => {
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
        Door::Agent(_) => {
            // The control protocol answers `answer` or `err`; a JSON-RPC
            // error is the door's MCP framing refusing before any request.
            answer.get("jsonrpc")?;
            Some(
                answer
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        Door::Named(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doors_are_the_named_socket_alone_or_admin_then_agent() {
        assert_eq!(
            semantic_doors(
                Some("/run/dev/control.sock".into()),
                Some("/run/user/7".into())
            ),
            [Door::Named("/run/dev/control.sock".into())]
        );
        assert_eq!(
            semantic_doors(Some(OsString::new()), Some("/run/user/7".into())),
            [
                Door::Admin("/run/user/7/tilo-session/admin-control.sock".into()),
                Door::Agent("/run/user/7/tilo-session/agent.sock".into()),
            ]
        );
        assert!(semantic_doors(None, None).is_empty());
        assert!(semantic_doors(None, Some("relative".into())).is_empty());
    }

    #[test]
    fn only_a_doors_refusal_of_the_process_hands_on() {
        let admin = Door::Admin("/a".into());
        let agent = Door::Agent("/b".into());
        let refused = serde_json::json!({"err": {"code": "not_permitted", "message": "no"}});
        assert_eq!(refusal_of_process(&admin, &refused).as_deref(), Some("no"));
        let unknown = serde_json::json!({"err": {"code": "unknown_action", "message": "x"}});
        assert_eq!(refusal_of_process(&admin, &unknown), None);
        let rpc = serde_json::json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32001, "message": "m"}});
        assert_eq!(refusal_of_process(&agent, &rpc).as_deref(), Some("m"));
        assert_eq!(
            refusal_of_process(&agent, &serde_json::json!({"answer": {}})),
            None
        );
        assert_eq!(
            refusal_of_process(&Door::Named("/c".into()), &refused),
            None
        );
    }

    #[test]
    fn printed_ids_round_trip() {
        let id = "app\u{1f}notes\u{1f}rename";
        assert_eq!(printable_id(id), "app\\x1fnotes\\x1frename");
        assert_eq!(id_from_printed(&printable_id(id)), id);
        assert_eq!(id_from_printed("plain\\xzz"), "plain\\xzz");
    }
}
