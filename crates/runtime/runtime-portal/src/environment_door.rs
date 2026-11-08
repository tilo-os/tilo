//! An environment's own agent door, asked of the session by the runtime that
//! confines the environment (`ush`).
//!
//! The session's agent door (`$XDG_RUNTIME_DIR/tilo-session/agent.sock`)
//! serves only programs in the session's own namespaces, so nothing inside a
//! confined environment reaches it. When `ush` opens an environment it asks
//! the session, on that door, for a door of the environment's own: one
//! [`Request`] line. The session binds a fresh socket for the environment and
//! answers with its path ([`Reply::Opened`]); `ush` binds that socket into the
//! environment alone, at [`tilo_paths::login::GUEST_AGENT_SOCKET`].
//!
//! The connection the request was made on is the door's lease: the door lives
//! while it is open, and the session removes it when the connection closes,
//! which it does when the environment ends or `ush` does. A program the
//! session serves on the door is the environment's, working in the
//! environment's workspace; only a descendant of the process holding the lease
//! is served there.

use std::io::{self, BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The request's `op`.
pub const OP: &str = "environment-door";

/// The longest line either side writes.
const MAX_LINE: u64 = 16 * 1024;

/// How long the session may take to open the door: it answers on its loop's
/// next pass.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// What `ush` asks the session for: a door for the environment `environment`
/// (its id on this device, `<workspace>/<record>`), called `name`, working in
/// `workspace` (a workspace id), or in the workspace in front of the person
/// when it is `None`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub op: String,
    pub environment: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl Request {
    /// The request for `environment`, called `name`, in `workspace`.
    #[must_use]
    pub fn new(environment: &str, name: &str, workspace: Option<&str>) -> Self {
        Self {
            op: OP.to_owned(),
            environment: environment.to_owned(),
            name: name.to_owned(),
            workspace: workspace.map(str::to_owned),
        }
    }

    /// `line`, when it is this request: the `op` it names, and an
    /// environment named as an environment's id may be (a workspace id and a
    /// record key, around a `/`).
    #[must_use]
    pub fn of(line: &[u8]) -> Option<Self> {
        let request: Self = serde_json::from_slice(line).ok()?;
        let named = |text: &str, most: usize| {
            !text.is_empty() && text.len() <= most && !text.chars().any(char::is_control)
        };
        (request.op == OP
            && named(&request.environment, 160)
            && request
                .environment
                .split('/')
                .all(|part| !matches!(part, "" | "." | ".."))
            && request
                .environment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
            && named(&request.name, 128)
            && request
                .workspace
                .as_deref()
                .is_none_or(|workspace| named(workspace, 128)))
        .then_some(request)
    }
}

/// What the session answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Reply {
    /// The door is at `path`, for programs working in `workspace`.
    Opened { path: PathBuf, workspace: String },
    /// No door, and why.
    Refused { reason: String },
}

impl Reply {
    /// This reply as the line the session writes.
    #[must_use]
    pub fn line(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }
}

/// An environment's door, held open: dropping it closes the lease, and the
/// session removes the door.
#[derive(Debug)]
pub struct Lease {
    _stream: UnixStream,
    path: PathBuf,
    workspace: String,
}

impl Lease {
    /// Where the session listens for the environment's programs.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The workspace the environment's programs work in.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

/// Ask the session whose agent door is `door` for `request`'s door.
///
/// # Errors
/// The door does not answer, refuses, or answers something else.
pub fn open(door: &Path, request: &Request) -> io::Result<Lease> {
    let mut stream = tilo_files::native::socket::connect(door)?;
    stream.set_read_timeout(Some(ANSWER_WITHIN))?;
    stream.set_write_timeout(Some(ANSWER_WITHIN))?;
    let mut line = serde_json::to_vec(request).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut answer = Vec::new();
    io::BufReader::new(io::Read::take(stream.try_clone()?, MAX_LINE))
        .read_until(b'\n', &mut answer)?;
    let reply: Reply = serde_json::from_slice(&answer).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the session answered no environment door: {}",
                String::from_utf8_lossy(&answer).trim()
            ),
        )
    })?;
    match reply {
        Reply::Opened { path, workspace } => {
            stream.set_read_timeout(None)?;
            Ok(Lease {
                _stream: stream,
                path,
                workspace,
            })
        }
        Reply::Refused { reason } => Err(io::Error::new(io::ErrorKind::PermissionDenied, reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_reads_back_and_only_names_an_environment_a_record_key_may() {
        let request = Request::new("personal/dev", "Development", Some("workspace-1"));
        let line = serde_json::to_vec(&request).unwrap();
        assert_eq!(Request::of(&line), Some(request));
        let anywhere = Request::new("dev", "Development", None);
        assert_eq!(
            Request::of(&serde_json::to_vec(&anywhere).unwrap()),
            Some(anywhere)
        );
        for refused in [
            Request::new("../dev", "Development", None),
            Request::new("w/../dev", "Development", None),
            Request::new("w//dev", "Development", None),
            Request::new("w/dev box", "Development", None),
            Request::new("", "Development", None),
            Request::new("dev", "", None),
            Request::new("dev", "a\nb", None),
        ] {
            assert_eq!(
                Request::of(&serde_json::to_vec(&refused).unwrap()),
                None,
                "{refused:?}"
            );
        }
        assert_eq!(
            Request::of(br#"{"op":"input","environment":"dev","name":"d"}"#),
            None
        );
        assert_eq!(Request::of(b"{\"jsonrpc\":\"2.0\"}"), None);
    }

    #[test]
    fn a_lease_holds_the_door_the_session_answered_until_it_is_dropped() {
        use std::io::Read as _;
        let directory = tempfile::tempdir().unwrap();
        let door = directory.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&door).unwrap();
        let session = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = Vec::new();
            io::BufReader::new(stream.try_clone().unwrap())
                .read_until(b'\n', &mut line)
                .unwrap();
            let asked = Request::of(line.strip_suffix(b"\n").unwrap()).unwrap();
            assert_eq!(asked.environment, "dev");
            (&stream)
                .write_all(
                    &Reply::Opened {
                        path: "/run/door.sock".into(),
                        workspace: "w1".into(),
                    }
                    .line(),
                )
                .unwrap();
            // The lease is open until the environment's side drops it.
            let mut rest = Vec::new();
            (&stream).read_to_end(&mut rest).unwrap();
            rest
        });
        let lease = open(&door, &Request::new("dev", "Development", None)).unwrap();
        assert_eq!(lease.path(), Path::new("/run/door.sock"));
        assert_eq!(lease.workspace(), "w1");
        drop(lease);
        assert!(session.join().unwrap().is_empty(), "the lease only closes");
    }

    #[test]
    fn a_refusal_is_the_reason_the_session_gave() {
        let directory = tempfile::tempdir().unwrap();
        let door = directory.path().join("agent.sock");
        let listener = std::os::unix::net::UnixListener::bind(&door).unwrap();
        let session = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = Vec::new();
            io::BufReader::new(stream.try_clone().unwrap())
                .read_until(b'\n', &mut line)
                .unwrap();
            (&stream)
                .write_all(
                    &Reply::Refused {
                        reason: "no workspace".into(),
                    }
                    .line(),
                )
                .unwrap();
        });
        let error = open(&door, &Request::new("dev", "Development", None)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "no workspace");
        session.join().unwrap();
    }
}
