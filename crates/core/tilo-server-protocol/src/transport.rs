//! Wire messages exchanged between a peer and the server after the
//! [`auth`](crate::auth) handshake.
//!
//! The framing that carries them lives with the stream I/O in
//! `tilo-server-client`'s `frame` module; this module only defines the values.
//!
//! The server uses **one long-lived bidirectional stream per
//! connection**. After the handshake, the stream is a sequence of
//! length-prefixed postcard-encoded [`Envelope`] messages:
//!
//! - The server is not a general-purpose RPC service — its message
//!   shape is a small closed enum.
//! - Length-prefixed framing lets the reader skip malformed messages
//!   without desynchronising the stream.
//! - Postcard keeps the encoding compact + deterministic (no field-
//!   reorder) so integration tests can byte-compare fixtures.

use serde::{Deserialize, Serialize};
pub use tilo_unlock_protocol::opened::{Opening, UnlockMethod};

/// ALPN of a device's link to another device of its workspace, which carries
/// the workspace's records, its governance and the content they refer to. A
/// page reaches the same link over WebTransport (`crate::web::LINK_RECORDS_PATH`).
/// `/4` is the first version: the link names its workspace and whether it
/// takes the workspace's records only as it opens them.
pub const DIRECT_ALPN: &[u8] = b"tilo-peer-state-direct/4";
/// ALPN of one device asking another of its workspace to list, call or watch
/// an action, on a link of the device's own.
pub const ACTIONS_ALPN: &[u8] = b"tilo-actions/1";

/// One thing a server's person, or an administrator of a workspace it serves,
/// asks the machine to do: each is one of the `server` device actions its
/// daemon answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerOperation {
    /// What it serves and how its copies stand, how it unlocks, whether it
    /// takes work and how its updates are, read live.
    Status,
    /// The last `lines` journal lines of `service`.
    Logs {
        /// The systemd unit whose journal to read.
        service: String,
        /// How many lines back to read, at most 500.
        lines: u32,
    },
    /// Restart `service`.
    Restart {
        /// The systemd unit to restart.
        service: String,
    },
    /// Reboot the machine.
    Reboot,
    /// Fetch and stage the newest update now, instead of waiting for the
    /// update timer.
    Update,
    /// Stop taking OS updates.
    PauseUpdates,
    /// Follow OS updates again.
    ResumeUpdates,
    /// Boot the previous system from now on.
    Rollback,
    /// Suspend its workspaces' Linux machines and hand their work off.
    Drain,
    /// Make Tilo the firmware's default start, in the retained OS's place
    /// after any network-boot entries; asked again, it restores the order it
    /// wrote.
    SetDefaultStart,
}

impl ServerOperation {
    /// The operation in one word: what the action that asks it is named, and
    /// what a server's entry keeps as asked.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Logs { .. } => "logs",
            Self::Restart { .. } => "restart",
            Self::Reboot => "reboot",
            Self::Update => "update",
            Self::PauseUpdates => "pause-updates",
            Self::ResumeUpdates => "resume-updates",
            Self::Rollback => "rollback",
            Self::Drain => "drain",
            Self::SetDefaultStart => "set-default-start",
        }
    }

    /// The unit a `restart` or `logs` names, or `""` for an operation that
    /// names none.
    #[must_use]
    pub fn service(&self) -> &str {
        match self {
            Self::Logs { service, .. } | Self::Restart { service } => service,
            _ => "",
        }
    }

    /// How many journal lines a `logs` reads, `0` for an operation that reads
    /// none.
    #[must_use]
    pub const fn lines(&self) -> u32 {
        match self {
            Self::Logs { lines, .. } => *lines,
            _ => 0,
        }
    }
}

/// What a server answers an operation with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerOperationResult {
    /// What was asked, in one word: `status`, `logs`, `restart`, `reboot`,
    /// `update`, `pause-updates`, `resume-updates`, `rollback`, `drain` or
    /// `set-default-start`.
    pub operation: String,
    /// Whether the server did it now. `false` when it asked a root helper
    /// to do something that ends or changes a boot, where an immediate answer
    /// would not survive the machine going down.
    pub done: bool,
    /// One sentence a person reads.
    pub summary: String,
    /// What it read, when the operation read something.
    pub answer: ServerAnswer,
}

impl ServerOperationResult {
    /// The answer of a request that was not an operation.
    #[must_use]
    pub fn nothing() -> Self {
        Self {
            operation: String::new(),
            done: true,
            summary: String::new(),
            answer: ServerAnswer::Nothing,
        }
    }
}

/// What an operation read, if anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerAnswer {
    /// Nothing: the operation changed something.
    Nothing,
    /// Text it read: a service's journal.
    Text(String),
    /// The server as it is now.
    Status(Box<ServerStatus>),
}

/// A server as it is now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    /// The machine's name on its network.
    pub hostname: String,
    /// How long it has been up, in seconds.
    pub uptime_seconds: u64,
    /// The image it booted.
    pub booted_image: String,
    /// The image an update staged, empty when none is staged.
    pub staged_image: String,
    /// The updates it follows (`stable` or `dev`), empty for the ones its
    /// image came with.
    pub updates: String,
    /// Whether its update timer is stopped.
    pub updates_paused: bool,
    /// The image it restarted into while it held its person's update lease,
    /// empty when none is pending.
    pub pending_update: String,
    /// How it unlocks after a boot: `owner-device`, `tpm` or `passphrase`.
    pub unlock: String,
    /// How its system volume actually opened this boot, and when, as early
    /// boot noted it; `None` when this boot left no note.
    pub unlocked: Option<Opening>,
    /// The workspaces it holds, and how each one's copies stand.
    pub holding: Vec<HeldWorkspace>,
    /// The machine it is.
    pub hardware: ServerHardware,
}

/// One workspace a server holds, and how its copies stand.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldWorkspace {
    /// The workspace's id.
    pub workspace: String,
    /// The workspace's name.
    pub name: String,
    /// Each holder of it, with its role.
    pub holders: Vec<HeldCopy>,
    /// What falls short of the copies its owners chose, in words.
    pub shortfall: Vec<String>,
}

/// One holder's copy of a workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldCopy {
    /// The holder's device key, in lowercase hex.
    pub device: String,
    /// Its role: `key-holder` or `store`.
    pub role: String,
    /// Whether its copy is current.
    pub current: bool,
    /// Whether it has said where its copy stands at all: one that has not is
    /// not known to be current.
    pub reported: bool,
}

/// The machine a server is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHardware {
    /// The product the firmware names.
    pub product: String,
    /// Its processor.
    pub cpu_model: String,
    /// How many processors it has.
    pub cpus: u32,
    /// Its memory, in megabytes.
    pub memory_mb: u64,
    /// Free storage under its state root, in gigabytes.
    pub storage_free_gb: u64,
    /// Total storage under its state root, in gigabytes.
    pub storage_total_gb: u64,
}

/// Bound, in characters, of the agent name a session declares its run of.
pub const MAX_AGENT_LABEL_CHARS: usize = 32;

/// Hard cap for a single framed message on the wire. Guards
/// against a peer that sends `u32::MAX` as a length prefix to
/// exhaust the reader's memory. 64 MiB is a very generous ceiling
/// for a CRDT delta.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// What a host answered of a record a device opened: everything it retains of
/// it went before this answer, or the record is another person's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentAnswer {
    /// Everything it retains of the record went before this answer, this many
    /// frames: none says the workspace holds none of it.
    Sent { frames: u32 },
    /// The record is another person's own: only that person's devices are
    /// sent it, and this device's person is someone else.
    NotYours,
}
