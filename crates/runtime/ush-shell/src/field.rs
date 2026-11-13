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
//! The values are those of `runtime_portal::editor`, the wire format Terminal
//! speaks; the host carries them ([`Field`]). Nothing here touches a socket,
//! so the same editor serves the app wherever the shell runs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    pub incarnation: [u8; 16],
    pub generation: u64,
    pub revision: u64,
    pub expires_ns: u64,
    pub anchor: u32,
    pub focus: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

/// field as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub request: u64,
    pub status: Status,
    pub lease: Option<Lease>,
    pub transaction: u64,
    pub can_undo: bool,
    pub can_redo: bool,
    pub text: String,
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
