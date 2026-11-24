//! A hosted workspace's Terminal shell, carried to this device.
//!
//! A workspace a server hosts runs its Terminal sessions' shells where the server runs them, not
//! on the device. When a Terminal window in such a workspace opens its session, the device's shell
//! runtime (`ush`) asks the device's daemon for that shell ([`Request`], its `openTerminal`
//! device action). The daemon holds the device's endpoint, so it asks the host to open the
//! session's shell on a terminal (the session record's [`ACTION`]) and dials the host's terminal
//! streams ([`ALPN`]) for it.
//!
//! The daemon answers at once whether a host other than this device runs the workspace. For a
//! hosted one it hands the runtime one end of a socket pair beside its answer; once the host
//! answered (which waits for the person when they are asked to allow it), one [`Reply`] line
//! follows on it, and from [`Reply::Opened`] on it is the host's terminal stream, byte for byte:
//! the runtime presents the stream's id there and speaks the stream's frames
//! (`ush::environment_terminals::wire`). The socket closing is the stream ending.

use std::io::{self, Read};

use serde::{Deserialize, Serialize};

/// The protocol a device dials a host's terminal streams on.
pub const ALPN: &[u8] = b"tilo-environment-terminal/1";

/// The Terminal session record's resource, and its action that opens its shell on a terminal
/// for the device that asks (`apps/terminal`'s `open-terminal`).
pub const RESOURCE: &str = "terminal.session";
pub const ACTION: &str = "open-terminal";

/// The widest and tallest window a terminal opens at, as the host takes it.
pub const MAX_SIDE: u16 = 1000;

const MAX_LINE: usize = 16 * 1024;

/// What the runtime asks the daemon for: the arguments of its `openTerminal` device action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
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
            workspace: workspace.to_owned(),
            record: record.to_owned(),
            cols: cols.clamp(1, MAX_SIDE),
            rows: rows.clamp(1, MAX_SIDE),
            attach: attach.map(str::to_owned),
        }
    }

    /// The request `args` are, when they are a well-formed one.
    #[must_use]
    pub fn of(args: &serde_json::Value) -> Option<Self> {
        let request = Self::deserialize(args).ok()?;
        let plain =
            |text: &str, most: usize| text.len() <= most && !text.chars().any(char::is_control);
        (!request.workspace.is_empty()
            && plain(&request.workspace, 128)
            && plain(&request.record, 512)
            && (1..=MAX_SIDE).contains(&request.cols)
            && (1..=MAX_SIDE).contains(&request.rows)
            && request.attach.as_deref().is_none_or(is_session_id))
        .then_some(request)
    }
}

/// Whether `id` is a host's session id: 32 hex characters.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// What the host answered, said on the handed socket before its stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Reply {
    /// The host opened the session's shell on a terminal: present `stream` on this socket, which
    /// is the host's terminal stream from here on. `session` is the host's session, to come back
    /// to.
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

    /// One reply line on `stream`, read a byte at a time: what follows it is the host's stream.
    ///
    /// # Errors
    /// The socket closed first, or said something else.
    pub fn read(stream: &mut impl Read) -> io::Result<Self> {
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        loop {
            if stream.read(&mut byte)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the device's daemon closed the workspace's terminal without answering",
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
}

fn unexpected(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the device's daemon answered a workspace's terminal with {what}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_reads_back_and_only_names_what_a_host_takes() {
        let session = "0123456789abcdef0123456789abcdef";
        let request = Request::new("w1", "uuid:1", (120, 40), Some(session));
        assert_eq!(
            Request::of(&serde_json::to_value(&request).unwrap()),
            Some(request)
        );
        assert_eq!(Request::new("w1", "", (0, 5000), None).cols, 1);
        assert_eq!(Request::new("w1", "", (0, 5000), None).rows, MAX_SIDE);
        for refused in [
            Request::new("", "", (80, 24), None),
            Request::new("w\n1", "", (80, 24), None),
            Request::new("w1", "a\nb", (80, 24), None),
            Request::new("w1", "", (80, 24), Some("not-a-session")),
        ] {
            assert_eq!(
                Request::of(&serde_json::to_value(&refused).unwrap()),
                None,
                "{refused:?}"
            );
        }
        assert_eq!(
            Request::of(&serde_json::json!({"workspace": "w1", "cols": 1, "rows": 1, "app": "x"})),
            None,
            "an app is never the asker's to name"
        );
    }

    #[test]
    fn a_reply_is_one_line_and_what_follows_is_the_stream() {
        let mut said = Reply::Opened {
            stream: "s".into(),
            session: "0123456789abcdef0123456789abcdef".into(),
        }
        .line();
        said.extend_from_slice(b"frames");
        let mut socket = io::Cursor::new(said);
        assert!(
            matches!(Reply::read(&mut socket), Ok(Reply::Opened { stream, .. }) if stream == "s")
        );
        let mut rest = Vec::new();
        socket.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"frames");
        assert!(Reply::read(&mut io::Cursor::new(b"{\"result\":\"refused\"".to_vec())).is_err());
    }
}
