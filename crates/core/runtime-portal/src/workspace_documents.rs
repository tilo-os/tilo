//! Terminal's documents in a workspace, read and written by the device's shell runtime (`ush`)
//! through the session.
//!
//! An environment is one of Terminal's records, kept in its workspace's one store like every
//! other record. The shell runtime has no connection to the workspace of its own: it asks the
//! session, on the session's MCP socket, which holds this device's workspace service. One
//! connection carries one store command: a [`Request`] line, then the command's line (a
//! `tilo_resource_store::wire::Command`, one of the record commands, as JSON). The session carries
//! it out as Terminal's, in the workspace the request names, and answers with an [`Answer`] line,
//! followed for [`Answer::Answered`] by the store's reply on a line of its own.
//!
//! The app is never the asker's to name: the session reads and writes Terminal's documents
//! alone. Like the environment's socket, the request carries the proof that the asker holds the
//! device identity ([`crate::holder_proof`]).

use std::io::{self, BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const OP: &str = "workspace-documents";

/// The longest command or reply line: a page or run of a record, base64, with room for what
/// frames it.
pub const MAX_PAYLOAD: u64 = 8 << 20;

const MAX_LINE: u64 = 16 * 1024;

const ANSWER_WITHIN: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub op: String,
    /// The workspace, by id; none for the workspace in front of the person, which is where the
    /// runtime's primary workspace keeps its records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// That the runtime asking holds the device identity, for a session that cannot look at it
    /// ([`crate::holder_proof`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

impl Request {
    #[must_use]
    pub fn new(workspace: Option<&str>) -> Self {
        Self {
            op: OP.to_owned(),
            workspace: workspace.map(str::to_owned),
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
        (request.op == OP
            && request.workspace.as_deref().is_none_or(|workspace| {
                !workspace.is_empty()
                    && workspace.len() <= 128
                    && workspace
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
            && request
                .proof
                .as_deref()
                .is_none_or(crate::holder_proof::well_formed))
        .then_some(request)
    }
}

/// What the session answered a command with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Answer {
    /// The store's reply follows on the next line.
    Answered,
    /// The command was not carried out, and why, in words.
    Refused { reason: String },
}

/// Ask the session whose MCP server is `socket` to carry out `command` (the store command's
/// JSON) for `request`: the store's reply, as JSON.
///
/// # Errors
/// The session cannot be reached ([`io::ErrorKind::NotFound`],
/// [`io::ErrorKind::ConnectionRefused`]), refused the command
/// ([`io::ErrorKind::PermissionDenied`], its reason the error's text), or answered something else.
pub fn ask(socket: &Path, request: &Request, command: &[u8]) -> io::Result<Vec<u8>> {
    let mut stream = tilo_files::native::socket::connect(socket)?;
    stream.set_read_timeout(Some(ANSWER_WITHIN))?;
    stream.set_write_timeout(Some(ANSWER_WITHIN))?;
    let mut sent = serde_json::to_vec(request).map_err(io::Error::other)?;
    sent.push(b'\n');
    sent.extend_from_slice(command);
    sent.push(b'\n');
    stream.write_all(&sent)?;
    let mut reader = io::BufReader::new(stream);
    let answer = read_line(&mut reader, MAX_LINE)?;
    match serde_json::from_slice(&answer) {
        Ok(Answer::Answered) => read_line(&mut reader, MAX_PAYLOAD),
        Ok(Answer::Refused { reason }) => {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, reason))
        }
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the session answered no store command: {}",
                String::from_utf8_lossy(&answer).trim()
            ),
        )),
    }
}

/// Serve one command on `stream`, whose [`Request`] line was read: read the command's line, and
/// answer what `carry_out` makes of it, the store's reply or why it was not carried out.
///
/// # Errors
/// The asker sent no command, or left before the answer.
pub fn serve(
    stream: UnixStream,
    carry_out: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(ANSWER_WITHIN))?;
    let mut writer = stream.try_clone()?;
    let command = read_line(&mut io::BufReader::new(stream), MAX_PAYLOAD)?;
    let mut line = match carry_out(&command) {
        Ok(reply) => {
            let mut line = serde_json::to_vec(&Answer::Answered).map_err(io::Error::other)?;
            line.push(b'\n');
            line.extend_from_slice(&reply);
            line
        }
        Err(reason) => serde_json::to_vec(&Answer::Refused { reason }).map_err(io::Error::other)?,
    };
    line.push(b'\n');
    writer.write_all(&line)
}

/// The [`Request`] line a connection opens with, read a byte at a time so the command's line
/// stays the stream's: `None` for a line that is no request.
///
/// # Errors
/// The asker left before the line's end.
pub fn read_request(stream: &UnixStream) -> io::Result<Option<Request>> {
    let line = read_line(&mut io::BufReader::with_capacity(1, stream), MAX_LINE)?;
    Ok(Request::of(&line))
}

fn read_line(reader: &mut impl io::BufRead, most: u64) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    io::Read::take(reader, most.saturating_add(1)).read_until(b'\n', &mut line)?;
    if line.pop() != Some(b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the line ended before its end, or was longer than a line may be",
        ));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_is_answered_or_refused_over_one_connection() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("session.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let served = std::thread::spawn(move || {
            for refuse in [false, true] {
                let (stream, _) = listener.accept().unwrap();
                assert_eq!(
                    read_request(&stream).unwrap(),
                    Some(Request::new(Some("w-1")).proved(Some("ab".repeat(32))))
                );
                serve(stream, |command| {
                    if refuse {
                        Err("not this one".to_owned())
                    } else {
                        Ok([b"echo ", command].concat())
                    }
                })
                .unwrap();
            }
        });
        let request = Request::new(Some("w-1")).proved(Some("ab".repeat(32)));
        assert_eq!(
            ask(&socket, &request, b"{\"x\":1}").unwrap(),
            b"echo {\"x\":1}"
        );
        let refused = ask(&socket, &request, b"{}").unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(refused.to_string(), "not this one");
        served.join().unwrap();
        for malformed in [
            Request::new(Some("")),
            Request::new(Some("../w")),
            Request::new(Some("w\n")),
            Request::new(None).proved(Some("zz".into())),
        ] {
            assert_eq!(
                Request::of(&serde_json::to_vec(&malformed).unwrap()),
                None,
                "{malformed:?}"
            );
        }
    }
}
