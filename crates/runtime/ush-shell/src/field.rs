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

/// The field's state an edit is made against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lease {
    /// The editor's own, for the session.
    pub incarnation: [u8; 16],
    /// Which prompt: a new one for each line read.
    pub generation: u64,
    /// Which state of the line: any change makes a new one.
    pub revision: u64,
    /// When the lease ends, on the host's monotonic clock ([`Field::now_ns`]).
    pub expires_ns: u64,
    /// The selection, in bytes of the line; the cursor when they are equal.
    pub anchor: u32,
    pub focus: u32,
}

/// What the app asks of the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Publish the field as it is now.
    Inspect,
    /// Put `text` in place of the lease's selection.
    Replace {
        lease: Lease,
        transaction: u64,
        text: String,
    },
    /// Take the replacement `transaction` back.
    Undo { lease: Lease, transaction: u64 },
    /// Make it again.
    Redo { lease: Lease, transaction: u64 },
    /// What became of `transaction`: its receipt, never a second edit.
    Query { lease: Lease, transaction: u64 },
    /// The app gives up its lease.
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

/// The field's answer to request `request`, or, with `request` zero, the
/// field as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub request: u64,
    pub status: Status,
    /// With [`Status::Ready`] only.
    pub lease: Option<Lease>,
    pub transaction: u64,
    pub can_undo: bool,
    pub can_redo: bool,
    /// The line, with [`Status::Ready`] only.
    pub text: String,
}

/// How long a published lease holds.
pub const LEASE_NS: u64 = 120_000_000_000;

/// The most the line may hold to be published.
pub const TEXT_BYTES: usize = 4096;

/// The app's channel to the field, as the host carries it.
pub trait Field: Send + Sync {
    /// The editor's identity for the session: random, never all zero.
    fn incarnation(&self) -> [u8; 16];

    /// Nanoseconds on the monotonic clock the app reads leases by.
    fn now_ns(&self) -> u64;

    /// The editor waits for a key (`on`) or stops waiting: while it waits, a
    /// request arriving makes its read of the terminal return
    /// [`std::io::ErrorKind::Interrupted`], so it is served at once.
    fn listen(&self, on: bool);

    /// The next request the app sent, with its request ID, if one waits.
    fn take(&self) -> Option<(u64, Request)>;

    /// Send `reply` to the app.
    fn reply(&self, reply: &Reply);
}
