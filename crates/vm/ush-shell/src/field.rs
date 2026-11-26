//! The line at the prompt as a text field the app showing the session edits
//! too: Terminal's native editing, its input method and dictation.
//!
//! While the line editor waits for a key it publishes the line, the cursor
//! and a lease on them ([`Status::Ready`]); the app answers with an edit of
//! exactly that state ([`Request::Replace`]), which the editor applies as if
//! typed and acknowledges, and can take back ([`Request::Undo`],
//! [`Request::Redo`]). An edit of a line that changed since is
//! [`Status::Stale`]; nothing is ever pressed (an edit is literal text, never
//! Enter). Away from the prompt the line is [`Status::Unavailable`].
//!
//! The values are those of `tilo_process::editor`, the wire format Terminal
//! speaks; the host carries them ([`Field`]). Nothing here touches a socket,
//! so the same editor serves the app wherever the shell runs.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub incarnation: [u8; 16],
    pub generation: u64,
    pub revision: u64,
    pub expires_ns: u64,
    pub anchor: u32,
    pub focus: u32,
}

/// Request from the app to the shell's line editor.
///
/// `Debug` omits replacement text so prompt contents cannot reach a log.
#[derive(Clone, PartialEq, Eq)]
pub enum Request {
    Inspect,
    /// Put `text` in place of the lease's selection.
    Replace {
        lease: Lease,
        transaction: u64,
        text: String,
    },
    Undo {
        lease: Lease,
        transaction: u64,
    },
    Redo {
        lease: Lease,
        transaction: u64,
    },
    Query {
        lease: Lease,
        transaction: u64,
    },
    Revoke,
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inspect => f.write_str("Inspect"),
            Self::Replace {
                lease,
                transaction,
                text: _,
            } => f
                .debug_struct("Replace")
                .field("lease", lease)
                .field("transaction", transaction)
                .finish_non_exhaustive(),
            Self::Undo { lease, transaction } => f
                .debug_struct("Undo")
                .field("lease", lease)
                .field("transaction", transaction)
                .finish(),
            Self::Redo { lease, transaction } => f
                .debug_struct("Redo")
                .field("lease", lease)
                .field("transaction", transaction)
                .finish(),
            Self::Query { lease, transaction } => f
                .debug_struct("Query")
                .field("lease", lease)
                .field("transaction", transaction)
                .finish(),
            Self::Revoke => f.write_str("Revoke"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unavailable,
    Ready,
    Applied,
    Undone,
    Redone,
    Stale,
    Invalid,
    Unknown,
    Revoked,
}

/// Field as it is now.
///
/// `Debug` omits the line text so prompt contents cannot reach a log.
#[derive(Clone, PartialEq, Eq)]
pub struct Reply {
    pub request: u64,
    pub status: Status,
    pub lease: Option<Lease>,
    pub transaction: u64,
    pub can_undo: bool,
    pub can_redo: bool,
    pub text: String,
}

impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply")
            .field("request", &self.request)
            .field("status", &self.status)
            .field("lease", &self.lease)
            .field("transaction", &self.transaction)
            .field("can_undo", &self.can_undo)
            .field("can_redo", &self.can_redo)
            .finish_non_exhaustive()
    }
}

pub const LEASE_NS: u64 = 120_000_000_000;

pub const TEXT_BYTES: usize = 4096;

pub trait Field: Send + Sync {
    fn incarnation(&self) -> [u8; 16];

    fn now_ns(&self) -> u64;

    /// The editor waits for a key (`on`) or stops waiting: while it waits, a
    /// request arriving makes its read of the terminal return
    /// [`std::io::ErrorKind::Interrupted`], so it is served at once.
    fn listen(&self, on: bool);

    fn take(&self) -> Option<(u64, Request)>;

    fn reply(&self, reply: &Reply);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_lease() -> Lease {
        Lease {
            incarnation: [1; 16],
            generation: 2,
            revision: 3,
            expires_ns: 4,
            anchor: 0,
            focus: 5,
        }
    }

    #[test]
    fn debug_never_prints_prompt_text() {
        let secret = "super-secret-prompt-text";
        let request = Request::Replace {
            lease: sample_lease(),
            transaction: 7,
            text: secret.into(),
        };
        let reply = Reply {
            request: 9,
            status: Status::Ready,
            lease: Some(sample_lease()),
            transaction: 7,
            can_undo: true,
            can_redo: false,
            text: secret.into(),
        };

        for formatted in [
            format!("{request:?}"),
            format!("{request:#?}"),
            format!("{reply:?}"),
            format!("{reply:#?}"),
        ] {
            assert!(
                !formatted.contains(secret),
                "Debug leaked prompt text: {formatted}"
            );
        }
        assert!(format!("{request:?}").contains("Replace"));
        assert!(format!("{reply:?}").contains("Ready"));
    }
}
