//! A hosted workspace's shell on a terminal, asked of the session by the device's shell runtime
//! (`ush`) for a Terminal window.
//!
//! A workspace a server hosts runs its Terminal sessions' shells where the server runs them,
//! not on the device. When a Terminal window in such a workspace opens its session, `ush` asks
//! the session, on the session's MCP socket, for that shell: one [`Request`] line. The session
//! holds the device's key and its connection to the workspace's host, so it is the one that
//! asks the host to open the session's shell on a terminal (the session record's
//! `open-terminal` action) and dials the host's terminal streams ([`ALPN`]) for it.
//!
//! The session answers [`Reply::Local`] at once for a workspace no host other than this device
//! runs: `ush` opens the session on the device as before. For a hosted one it answers
//! [`Reply::Hosted`] at once, then, once the host answered (which waits for the person when
//! they are asked to allow it), [`Reply::Opened`] or [`Reply::Refused`]. From
//! [`Reply::Opened`] on, the connection is the host's terminal stream, byte for byte: `ush`
//! presents the stream's id there and speaks the stream's frames
//! (`ush::environment_terminals::wire`). The connection closing is the stream ending.

use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const OP: &str = "workspace-terminal";

/// The protocol a device dials a host's terminal streams on.
pub const ALPN: &[u8] = b"tilo-environment-terminal/1";

/// The Terminal session record's resource, and its action that opens its shell on a terminal
/// for the device that asks (`apps/terminal`'s `open-terminal`).
pub const RESOURCE: &str = "terminal.session";
pub const ACTION: &str = "open-terminal";

/// The widest and tallest window a terminal opens at, as the host takes it.
pub const MAX_SIDE: u16 = 1000;

const MAX_LINE: usize = 16 * 1024;

/// How long the session may take to say whether the workspace is hosted.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub op: String,
    /// The workspace the Terminal window is in.
    pub workspace: String,
    /// The Terminal session record the window shows, by its route (empty: the workspace's own).
    #[serde(default)]
    pub record: String,
    pub cols: u16,
    pub rows: u16,
    /// The host's session to come back to, by the id an earlier open answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<String>,
    /// That the runtime asking holds the device identity, for a session that cannot look at it
    /// ([`crate::holder_proof`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

impl Request {
    #[must_use]
    pub fn new(
        workspace: &str,
        record: &str,
        (cols, rows): (u16, u16),
        attach: Option<&str>,
    ) -> Self {
        Self {
            op: OP.to_owned(),
            workspace: workspace.to_owned(),
            record: record.to_owned(),
            cols: cols.clamp(1, MAX_SIDE),
            rows: rows.clamp(1, MAX_SIDE),
            attach: attach.map(str::to_owned),
            proof: None,
        }
    }

    /// This request with `proof` ([`crate::holder_proof`]).
    #[must_use]
    pub fn proved(mut self, proof: Option<String>) -> Self {
        self.proof = proof;
        self
    }

    /// The request `line` is, when it is a well-formed one.
    #[must_use]
    pub fn of(line: &[u8]) -> Option<Self> {
        let request: Self = serde_json::from_slice(line).ok()?;
        let plain =
            |text: &str, most: usize| text.len() <= most && !text.chars().any(char::is_control);
        (request.op == OP
            && !request.workspace.is_empty()
            && plain(&request.workspace, 128)
            && plain(&request.record, 512)
            && (1..=MAX_SIDE).contains(&request.cols)
            && (1..=MAX_SIDE).contains(&request.rows)
            && request.attach.as_deref().is_none_or(is_session_id)
            && request
                .proof
                .as_deref()
                .is_none_or(crate::holder_proof::well_formed))
        .then_some(request)
    }
}

/// Whether `id` is a host's session id: 32 hex characters.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Reply {
    /// No host other than this device runs the workspace: its sessions run here.
    Local,
    /// A host runs it, and is asked: [`Reply::Opened`] or [`Reply::Refused`] follows.
    Hosted,
    /// The host opened the session's shell on a terminal: present `stream` on this connection,
    /// which is the host's terminal stream from here on. `session` is the host's session, to
    /// come back to.
    Opened { stream: String, session: String },
    /// No shell, and why, in words.
    Refused { reason: String },
}

impl Reply {
    #[must_use]
    pub fn line(&self) -> Vec<u8> {
        let mut line = serde_json::to_vec(self).unwrap_or_default();
        line.push(b'\n');
        line
    }
}

/// What the session answered a [`Request`] with.
#[derive(Debug)]
pub enum Answer {
    Local,
    /// The host's terminal stream, and what to present on it.
    Opened {
        stream: String,
        session: String,
        connection: UnixStream,
    },
    Refused(String),
}

/// Ask the session whose MCP server is `socket` for `request`'s shell; `hosted` runs once the
/// session says a host runs the workspace, before the host answered.
///
/// # Errors
///
/// The session cannot be reached, or answers something else.
pub fn open(socket: &Path, request: &Request, hosted: impl FnOnce()) -> io::Result<Answer> {
    let mut stream = tilo_files::native::socket::connect(socket)?;
    stream.set_read_timeout(Some(ANSWER_WITHIN))?;
    stream.set_write_timeout(Some(ANSWER_WITHIN))?;
    let mut line = serde_json::to_vec(request).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    match read_reply(&mut stream)? {
        Reply::Local => return Ok(Answer::Local),
        Reply::Hosted => {}
        Reply::Refused { reason } => return Ok(Answer::Refused(reason)),
        Reply::Opened { .. } => return Err(unexpected("an opened stream before it was asked")),
    }
    hosted();
    // The host may wait for the person to allow it; the session bounds that wait.
    stream.set_read_timeout(None)?;
    match read_reply(&mut stream)? {
        Reply::Opened {
            stream: id,
            session,
        } => {
            stream.set_write_timeout(None)?;
            Ok(Answer::Opened {
                stream: id,
                session,
                connection: stream,
            })
        }
        Reply::Refused { reason } => Ok(Answer::Refused(reason)),
        Reply::Local | Reply::Hosted => Err(unexpected("the session answered twice")),
    }
}

/// One reply line, read a byte at a time: what follows it is the host's stream.
fn read_reply(stream: &mut UnixStream) -> io::Result<Reply> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the session closed without answering for the workspace's terminal",
            ));
        }
        if byte == *b"\n" {
            break;
        }
        if line.len() >= MAX_LINE {
            return Err(unexpected("a line too long to read"));
        }
        line.extend_from_slice(&byte);
    }
    serde_json::from_slice(&line).map_err(|_| {
        unexpected(&format!(
            "something else: {}",
            String::from_utf8_lossy(&line).trim()
        ))
    })
}

fn unexpected(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the session answered a workspace's terminal with {what}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead as _;

    tilo_test_env::scenario! {
        fn the_workspace_terminal;

        phase a_request_reads_back_and_only_names_what_a_host_takes;
        {
            let session = "0123456789abcdef0123456789abcdef";
            let request = Request::new("w1", "uuid:1", (120, 40), Some(session));
            let line = serde_json::to_vec(&request).unwrap();
            assert_eq!(Request::of(&line), Some(request));
            let proved = Request::new("w1", "", (80, 24), None).proved(Some("0f".repeat(32)));
            assert_eq!(Request::of(&serde_json::to_vec(&proved).unwrap()), Some(proved));
            assert_eq!(Request::new("w1", "", (0, 5000), None).cols, 1);
            assert_eq!(Request::new("w1", "", (0, 5000), None).rows, MAX_SIDE);
            for refused in [
                Request::new("", "", (80, 24), None),
                Request::new("w\n1", "", (80, 24), None),
                Request::new("w1", "a\nb", (80, 24), None),
                Request::new("w1", "", (80, 24), Some("not-a-session")),
                Request::new("w1", "", (80, 24), None).proved(Some("a proof".into())),
            ] {
                assert_eq!(Request::of(&serde_json::to_vec(&refused).unwrap()), None, "{refused:?}");
            }
            assert_eq!(Request::of(br#"{"op":"environment-socket","workspace":"w1","cols":1,"rows":1}"#), None);
        }

        phase a_hosted_workspace_is_said_first_and_its_stream_follows_on_the_connection;
        {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("agent.sock");
            let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            let session = std::thread::spawn(move || {
                let (stream, _) = listener.accept().unwrap();
                let mut line = Vec::new();
                io::BufReader::new(stream.try_clone().unwrap())
                    .read_until(b'\n', &mut line)
                    .unwrap();
                let asked = Request::of(line.strip_suffix(b"\n").unwrap()).unwrap();
                assert_eq!((asked.workspace.as_str(), asked.cols), ("w1", 80));
                (&stream).write_all(&Reply::Hosted.line()).unwrap();
                let mut opened = Reply::Opened {
                    stream: "s".into(),
                    session: "0123456789abcdef0123456789abcdef".into(),
                }
                .line();
                opened.extend_from_slice(b"frames");
                (&stream).write_all(&opened).unwrap();
            });
            let mut told = false;
            let answer = open(&socket, &Request::new("w1", "", (80, 24), None), || told = true).unwrap();
            let Answer::Opened { stream, session: id, mut connection } = answer else {
                panic!("{answer:?}");
            };
            assert!(told, "the window hears that a host is asked");
            assert_eq!((stream.as_str(), id.len()), ("s", 32));
            let mut rest = Vec::new();
            connection.read_to_end(&mut rest).unwrap();
            assert_eq!(rest, b"frames", "what follows the answer is the stream's");
            session.join().unwrap();
        }

        phase a_workspace_no_host_runs_is_local_and_a_refusal_is_its_reason;
        {
            for (reply, local) in [
                (Reply::Local, true),
                (Reply::Refused { reason: "no".into() }, false),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let socket = directory.path().join("agent.sock");
                let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                let session = std::thread::spawn(move || {
                    let (stream, _) = listener.accept().unwrap();
                    let mut line = Vec::new();
                    io::BufReader::new(stream.try_clone().unwrap())
                        .read_until(b'\n', &mut line)
                        .unwrap();
                    (&stream).write_all(&reply.line()).unwrap();
                });
                let answer = open(&socket, &Request::new("w1", "", (80, 24), None), || {
                    panic!("no host is asked")
                })
                .unwrap();
                match answer {
                    Answer::Local => assert!(local),
                    Answer::Refused(reason) => assert_eq!((local, reason.as_str()), (false, "no")),
                    Answer::Opened { .. } => panic!("nothing opened"),
                }
                session.join().unwrap();
            }
        }
    }
}
