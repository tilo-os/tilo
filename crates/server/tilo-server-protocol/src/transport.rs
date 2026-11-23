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

use collab_types::WorkspaceId;
use serde::{Deserialize, Serialize};
pub use tilo_unlock_protocol::opened::{Opening, UnlockMethod};

/// ALPN of the server stream. It never changes: the peer protocol version
/// is the handshake's first field, so a device at another version is told so
/// there, and there is one framing per version
/// (docs/architecture/compatibility.md).
pub const ALPN: &[u8] = b"tilo-server/1";
/// ALPN of the one connection between two of a person's devices, which
/// carries their device-to-device session: each side first proves its
/// person's certificates (the endpoint id proves the device key), then the
/// two exchange what is for the person's own devices alone. The device with
/// the lower endpoint id dials it, the other only when it is not dialled;
/// of two dialled at once, both keep the lower id's.
pub const SIBLING_ALPN: &[u8] = b"tilo-server-sibling/1";
/// ALPN of a device's link to another device of its workspace, which carries
/// the workspace's records, its governance and the content they refer to. A
/// page reaches the same link over WebTransport (`crate::web::LINK_RECORDS_PATH`).
/// `/4` is the first version: the link names its workspace and whether it
/// takes the workspace's records only as it opens them.
pub const DIRECT_ALPN: &[u8] = b"tilo-peer-state-direct/4";
/// ALPN of one device asking another of its workspace to list, call or watch
/// an action, on a link of the device's own.
pub const ACTIONS_ALPN: &[u8] = b"tilo-actions/1";

/// One thing the person who owns a server asks it to do from another of their
/// devices. Each is an action on their Servers workspace, which the server the
/// entry names carries out and answers in that entry.
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

    /// The operations a person asks of a server, in the order their actions
    /// are listed.
    pub const NAMES: [&'static str; 10] = [
        "status",
        "logs",
        "restart",
        "reboot",
        "update",
        "pause-updates",
        "resume-updates",
        "rollback",
        "drain",
        "set-default-start",
    ];

    /// The operation `name` is, with the unit a `restart` or `logs` names and
    /// the lines a `logs` reads: `None` for a name no operation goes by.
    #[must_use]
    pub fn named(name: &str, service: &str, lines: u32) -> Option<Self> {
        Some(match name {
            "status" => Self::Status,
            "logs" => Self::Logs {
                service: service.to_owned(),
                lines,
            },
            "restart" => Self::Restart {
                service: service.to_owned(),
            },
            "reboot" => Self::Reboot,
            "update" => Self::Update,
            "pause-updates" => Self::PauseUpdates,
            "resume-updates" => Self::ResumeUpdates,
            "rollback" => Self::Rollback,
            "drain" => Self::Drain,
            "set-default-start" => Self::SetDefaultStart,
            _ => return None,
        })
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
    /// What was asked of it recently, newest first.
    pub history: Vec<OperationEntry>,
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

/// One operation a server was asked to do, as it keeps them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationEntry {
    /// When it was asked, in nanoseconds since the epoch.
    pub at_ns: u64,
    /// What was asked, in one word.
    pub operation: String,
    /// What the server answered, in one sentence.
    pub summary: String,
    /// Whether it was carried out.
    pub done: bool,
}

/// Hard cap for a single framed envelope on the wire. Guards
/// against a peer that sends `u32::MAX` as a length prefix to
/// exhaust the reader's memory. 64 MiB is a very generous ceiling
/// for a CRDT delta.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// One message on the post-handshake stream.
///
/// Encoded via postcard's default enum representation: a compact
/// varint discriminant + the variant payload. Externally-tagged
/// (JSON-style) representations are not supported by postcard, so
/// we deliberately do not use `#[serde(tag = ...)]` — the wire form
/// is fully binary anyway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Envelope {
    /// Peer asks to receive updates for `workspace_id`. The server
    /// registers the peer as a subscriber and immediately drains
    /// any pending deltas queued for it (store-and-forward).
    Subscribe {
        /// Workspace id.
        workspace_id: WorkspaceId,
    },

    /// Peer stops receiving updates for `workspace_id`. The
    /// subscription is removed; any deltas received while
    /// unsubscribed are queued for the next Subscribe.
    Unsubscribe {
        /// Workspace id.
        workspace_id: WorkspaceId,
    },

    /// A workspace-directory CRDT delta. The server:
    /// 1. Applies it to its own pinned copy (if pinned).
    /// 2. Fans it out to every other peer currently subscribed.
    /// 3. Queues it for every other member device that is offline.
    Delta {
        workspace_id: WorkspaceId,
        /// Author device pubkey (redundant with the peer's cert on
        /// this connection but included so a fan-out recipient can
        /// dedup / attribute without maintaining connection state).
        author_device_public_key: [u8; 32],
        /// Authored-at nanoseconds since epoch.
        authored_at_ns: u64,
        /// Automerge incremental delta bytes.
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
        /// Author's device signature over the delta.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
    },

    /// A presence event, broadcast as an opaque payload. The host fans it out
    /// to the workspace's other subscribers, and the server never inspects the
    /// bytes; a forwarded presence reaches a device batched, as the delivery
    /// protocol's `EnvelopeV2::PresenceBatch` writes it.
    Presence {
        workspace_id: WorkspaceId,
        /// Peer-defined presence payload. The server never inspects
        /// the bytes.
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },

    /// The server's response to a `Subscribe`: how many pending
    /// deltas the server is about to flush from the peer's
    /// store-and-forward queue.
    Ready {
        workspace_id: WorkspaceId,
        /// Number of pending deltas queued for this peer that the
        /// server is about to flush.
        pending_deltas: u32,
        /// The workspace sends this peer its records only when it opens
        /// them ([`Self::OpenDocument`]), as a public workspace does: the
        /// replay carried its governance and shared desktop, and a record
        /// the peer holds from before, or does not hold at all, is asked
        /// for when it is opened, never taken as complete or as new.
        documents_when_opened: bool,
    },

    /// The server's response to a subscribe that failed — e.g.
    /// workspace not bound here, peer not a member.
    NotAuthorised {
        workspace_id: WorkspaceId,
        /// Why.
        refusal: crate::refusal::Refusal,
    },

    /// Ping/keep-alive. Not required but useful for the peer to
    /// detect a dead connection quickly.
    Ping,
    /// Response to Ping.
    Pong,

    /// Sent by a host immediately after auth — advertises the set of
    /// workspaces it holds. The receiver computes the intersection
    /// with its own hosted set and records the sender as a host
    /// attached to each workspace in it.
    ///
    /// Sent by BOTH sides of an attach session: the dialer sends
    /// first, the accepter replies with its own hosted set.
    PeerServerHello {
        /// Workspace ids this host currently hosts.
        hosted_workspaces: Vec<WorkspaceId>,
    },

    /// Anti-entropy request. The sender advertises the Automerge heads
    /// it currently holds for `workspace_id`; the host replies with a
    /// [`Envelope::SyncResponse`] carrying every change the sender is
    /// missing, computed against the host's *materialized* pinned doc.
    ///
    /// This is the recovery tier that sits underneath the per-peer
    /// store-and-forward queue: a delta lost in a genuine disconnect
    /// race (fanned out to a live subscriber whose wire died before the
    /// write landed, so it was neither delivered nor re-queued) leaves
    /// that peer permanently behind. Reconciling against the host's
    /// authoritative doc converges the peer regardless of what the
    /// queue delivered — run on subscribe, periodically, or on demand.
    SyncRequest {
        workspace_id: WorkspaceId,
        /// Automerge change hashes the requester already has (its
        /// `get_heads()`), each a 32-byte hash. An empty vec means "I
        /// hold nothing — send me the whole document."
        have_heads: Vec<[u8; 32]>,
    },

    /// The host's reply to a [`Envelope::SyncRequest`]: the changes the
    /// requester was missing, as a concatenated series of Automerge
    /// changes loadable with `load_incremental`. Empty when the
    /// requester is already up to date, or the workspace is not
    /// materialized on this host (unpinned).
    SyncResponse {
        workspace_id: WorkspaceId,
        /// Concatenated Automerge changes the requester was missing.
        #[serde(with = "serde_bytes")]
        delta_since: Vec<u8>,
    },

    /// A remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) addressed to one device of the workspace.
    ///
    /// The server routes it to the addressed device's live connection and
    /// never stores, replays or executes it: an action that ran late would
    /// be worse than one that did not run. Only when the addressed device is
    /// the server itself *and* an app action server was supplied does the
    /// server answer it, through that protocol. Anything else comes back as
    /// [`Envelope::RemoteActionUndeliverable`].
    RemoteAction {
        /// Workspace id both devices are subscribed to.
        workspace_id: WorkspaceId,
        /// Version of this envelope kind, see [`REMOTE_ACTION_ENVELOPE_VERSION`].
        version: u16,
        /// The addressed device.
        to_device_public_key: [u8; 32],
        /// The sending device, stamped by the server from the authenticated
        /// connection. Whatever a sender puts here is overwritten.
        from_device_public_key: [u8; 32],
        /// The user root the sending device chains to, stamped the same way.
        from_user_public_key: [u8; 32],
        /// Opaque remote-action frame.
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// The server could not deliver a [`Envelope::RemoteAction`]: the
    /// addressed device is not connected to this workspace, the envelope
    /// version is unknown, or nothing here serves apps.
    RemoteActionUndeliverable {
        workspace_id: WorkspaceId,
        /// The device it was addressed to.
        to_device_public_key: [u8; 32],
        /// Why.
        refusal: crate::refusal::Refusal,
    },

    /// A [`Envelope::Delta`] whose author wants to know when the host has
    /// made it durable. The host admits it exactly like a `Delta`, fans it
    /// out to other subscribers as a plain `Delta`, and answers the author
    /// with [`Envelope::Committed`] once it is persisted, or
    /// [`Envelope::CommitRefused`]. Only the author sees either answer, so
    /// peers that predate these variants never receive them.
    Commit {
        workspace_id: WorkspaceId,
        /// Author-chosen id echoed in the answer. Resending the same commit
        /// after a reconnect reuses it.
        op: [u8; 16],
        /// Must equal the authenticated device.
        author_device_public_key: [u8; 32],
        /// Authored-at nanoseconds since epoch (metadata only).
        authored_at_ns: u64,
        /// The same body an `Envelope::Delta` carries.
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
    },

    /// The host persisted commit `op` of this connection. `seq` is the
    /// host's order for the workspace: later commits get larger values.
    Committed {
        workspace_id: WorkspaceId,
        /// The commit's id.
        op: [u8; 16],
        /// Host order of the commit within the workspace.
        seq: u64,
    },

    /// The host did not admit commit `op`. Nothing was persisted or fanned
    /// out.
    CommitRefused {
        workspace_id: WorkspaceId,
        /// The commit's id.
        op: [u8; 16],
        /// Why.
        refusal: crate::refusal::Refusal,
    },

    /// Join the workspace's live session: the host answers with the members
    /// already present and tells them this device joined.
    LiveJoin {
        /// Workspace id.
        workspace_id: WorkspaceId,
    },

    /// A device joined (`active`) or left the workspace's live session, sent
    /// by the host to the live session's other members.
    LiveMember {
        workspace_id: WorkspaceId,
        device_public_key: [u8; 32],
        /// Whether it is now a live member.
        active: bool,
    },

    /// A `Subscribe` that resumes: the host replays only what this device
    /// has not received since `position` (a [`Envelope::Position`] it
    /// issued to this device), or everything when it cannot honour the
    /// position, and then follows the subscriber's durable deliveries with
    /// `Position`s. Sent only to a host that advertised resumable sessions.
    SubscribeFrom {
        workspace_id: WorkspaceId,
        /// The last position received for this workspace, if any.
        position: Option<Vec<u8>>,
    },

    /// Everything the host delivered to this subscriber so far. Opaque and
    /// host-authenticated; present it to [`Envelope::SubscribeFrom`] after
    /// a reconnect. Only subscribers that used `SubscribeFrom` receive it.
    Position {
        workspace_id: WorkspaceId,
        position: Vec<u8>,
    },

    /// This device answers [`Envelope::CompactRequest`]s for
    /// `workspace_id` while its subscription lasts. Sent only to a host
    /// that advertised compaction.
    CompactVolunteer {
        /// Workspace id.
        workspace_id: WorkspaceId,
    },

    /// The host holds a long sealed tail for one route and asks this member
    /// for a snapshot of it and a
    /// [`crate::compaction::CoverageStatement`]. Sent only to volunteers.
    CompactRequest {
        workspace_id: WorkspaceId,
        /// Echoed in the statement.
        request: [u8; 16],
        /// The route's application.
        app_id: String,
        /// The route's workspace.
        cluster_id: String,
        /// The route's record.
        record: String,
    },

    /// A member's answer to [`Envelope::CompactRequest`], sent after the
    /// snapshot's commits.
    Compacted {
        workspace_id: WorkspaceId,
        /// Encoded [`crate::compaction::CoverageStatement`].
        #[serde(with = "serde_bytes")]
        statement: Vec<u8>,
        /// The member's device signature over
        /// [`crate::compaction::signing_bytes`] of `statement`.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
    },

    /// One piece of a record's content for exactly one device of the
    /// workspace, which asked the workspace for it. Routed like
    /// [`Envelope::RemoteAction`] — to the addressed device's live
    /// connection, never stored, never fanned out — but never answered:
    /// content that does not arrive is asked for again. A shared window's
    /// control message (a viewer's ask, its input, a keyframe request) takes
    /// the same route to a device no direct connection reaches, such as a
    /// hosted environment's.
    DocumentContent {
        /// Workspace id both devices are subscribed to.
        workspace_id: WorkspaceId,
        /// The addressed device.
        to_device_public_key: [u8; 32],
        /// The sending device, stamped by the server from the authenticated
        /// connection. Whatever a sender puts here is overwritten.
        from_device_public_key: [u8; 32],
        /// Opaque piece, at most [`MAX_DOCUMENT_CONTENT_FRAME_BYTES`].
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// Open an on-demand record of a subscribed workspace: a record a joining
    /// device is not replayed (`neighbour_wire::document_sync`). The server
    /// sends what it retains of the record as [`Envelope::Delta`]s, under
    /// the same admission as a replay, then [`Envelope::DocumentOpened`];
    /// from then on the record's live changes reach the device too.
    OpenDocument {
        /// Workspace id, subscribed.
        workspace_id: WorkspaceId,
        /// The record's retained channel
        /// (`neighbour_wire::app_document_group`).
        channel: [u8; 32],
    },

    /// The server's answer to an opened record: everything it retains of it
    /// was sent, or it is not this device's to open.
    DocumentOpened {
        workspace_id: WorkspaceId,
        /// The record's retained channel.
        channel: [u8; 32],
        answer: DocumentAnswer,
    },

    /// One of the host's own operations (`tilo.host`: creating, listing,
    /// entering and hosting workspaces), asked of the host itself by a device of
    /// the host's own person, or the host's answer to it: an opaque
    /// remote-action frame. It names no workspace: the authenticated
    /// connection says who asks, and the host answers only its own person's
    /// devices.
    HostAction {
        /// Opaque remote-action frame.
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// What a member device builds its replica of the workspace from, sent to a
    /// subscriber before the workspace's replay: the workspace's
    /// `tilo_workspace_join::JoinBootstrap` for this device, postcard
    /// encoded, as a joining native device receives it. Its governance
    /// messages and records stay out: the replay that follows carries the
    /// workspace's retained governance and records.
    WorkspaceBootstrap {
        workspace_id: WorkspaceId,
        #[serde(with = "serde_bytes")]
        bootstrap: Vec<u8>,
    },

    /// A member asks the workspace for content one of its records
    /// references. The host fans it out to the workspace's other members and
    /// answers with what it keeps itself; each member answers to the asking
    /// device alone ([`Self::DocumentContent`]). `frame` is the
    /// `NeighbourMsg::DocumentContentWant` that says what is wanted, and
    /// `from_device_public_key` is the host's stamp of who asked.
    DocumentContentWant {
        workspace_id: WorkspaceId,
        from_device_public_key: [u8; 32],
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// The host's fetch proxy grant for this connection: the bearer token a
    /// page sends with every request it makes through its host
    /// ([`FETCH_PATH`], as `tilo_net::proxy` names it). Sent once, as the
    /// session starts, and dead when the connection ends; a host that serves
    /// no proxy sends none.
    ///
    /// [`FETCH_PATH`]: crate::web::FETCH_PATH
    FetchGrant {
        /// What names this connection to the host.
        token: [u8; 32],
    },

    /// A device says this session is one run of the agent `label` names (as
    /// History names its runs), answered through [`Self::AgentSessionAnswer`]
    /// with the asker's `request_id`. Said before anything else: every
    /// request the session makes from then on is the agent's, for the person,
    /// within what bounds an agent, and the person's MCP server lists the run
    /// while the session lasts.
    AgentSession { request_id: u64, label: String },

    /// The host's answer to [`Self::AgentSession`] with the same `request_id`:
    /// `None` once the session is that agent's run, else why not.
    AgentSessionAnswer {
        request_id: u64,
        refusal: Option<String>,
    },

    /// The host's word that the person stopped the agent run this session is
    /// ([`Self::AgentSession`]), said just before the host ends it: the device
    /// does not connect again, so the agent and its presence end here.
    AgentSessionStopped,

    /// The host's answer to the entry the session's hello asked for
    /// ([`crate::entering`]): the first envelope of that session, before
    /// anything else it is sent.
    Entered(crate::entering::Entered),

    /// The host's own word for a run it executes
    /// ([`crate::host_voice::HostSaid`]): sent by the host's device to the
    /// server that is the host, which fans it out to the workspace's other
    /// subscribers only when its own device said it. A device takes it only
    /// when `signature` is its host's over `said`. It never rides the live
    /// live session, whose keys a host that is no member of it does not hold.
    HostSaid {
        workspace_id: WorkspaceId,
        /// The canonical [`crate::host_voice::HostSaid`] encoding.
        #[serde(with = "serde_bytes")]
        said: Vec<u8>,
        /// The host device's signature over `said`.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
    },

    /// This session's device only follows the channel groups it is in: it
    /// never admits another device to one, as a page does. The host then
    /// sends it no device's request to be admitted, which only a device that
    /// admits others acts on; a newcomer to a workspace of hundreds would
    /// download one per member. Said once, before the session subscribes,
    /// and it stays for the session.
    KeyFollower,

    /// The person's workspace list, as their host keeps it now: sent to every
    /// session of one of their devices whenever the set of workspaces they
    /// have changes (one made, one they joined, one renamed, one they left or
    /// was deleted), so a device reads it as it changes rather than asking
    /// again. `list` is the `PersonalList` (`tilo_server_session::workspaces`)
    /// as its host action answers it, in JSON.
    PersonalList {
        #[serde(with = "serde_bytes")]
        list: Vec<u8>,
    },

    /// The server kept none of a [`Self::Delta`]'s document changes: the
    /// document they write, the changes themselves, and why. Only the device
    /// that sent them sees it, so peers that predate it never receive it.
    ChangeRefused {
        /// The workspace the changes were sent to.
        workspace_id: WorkspaceId,
        /// The app whose document the changes write.
        app_id: String,
        /// The workspace the document belongs to, as the change named it: one
        /// a change names but is not sent to is refused all the same.
        cluster_id: String,
        /// The document.
        document_id: String,
        /// The changes the refused change carried, as their hashes: what a
        /// reader withdraws and a person's history names as missing.
        changes: Vec<[u8; 32]>,
        /// Why.
        refusal: crate::refusal::Refusal,
    },
}

/// What the server answered of a record a device opened ([`Envelope::OpenDocument`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentAnswer {
    /// Everything it retains of the record went before this answer, this many
    /// frames: none says the workspace holds none of it.
    Sent { frames: u32 },
    /// The record is another person's own: only that person's devices are
    /// sent it, and this device's person is someone else.
    NotYours,
}

impl Envelope {
    /// The variant's name, for logs: what a connection was doing, without
    /// printing what it carried.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Subscribe { .. } => "Subscribe",
            Self::Unsubscribe { .. } => "Unsubscribe",
            Self::Delta { .. } => "Delta",
            Self::Presence { .. } => "Presence",
            Self::Ready { .. } => "Ready",
            Self::NotAuthorised { .. } => "NotAuthorised",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
            Self::PeerServerHello { .. } => "PeerServerHello",
            Self::SyncRequest { .. } => "SyncRequest",
            Self::SyncResponse { .. } => "SyncResponse",
            Self::RemoteAction { .. } => "RemoteAction",
            Self::RemoteActionUndeliverable { .. } => "RemoteActionUndeliverable",
            Self::Commit { .. } => "Commit",
            Self::Committed { .. } => "Committed",
            Self::CommitRefused { .. } => "CommitRefused",
            Self::LiveJoin { .. } => "LiveJoin",
            Self::LiveMember { .. } => "LiveMember",
            Self::SubscribeFrom { .. } => "SubscribeFrom",
            Self::Position { .. } => "Position",
            Self::CompactVolunteer { .. } => "CompactVolunteer",
            Self::CompactRequest { .. } => "CompactRequest",
            Self::Compacted { .. } => "Compacted",
            Self::DocumentContent { .. } => "DocumentContent",
            Self::OpenDocument { .. } => "OpenDocument",
            Self::DocumentOpened { .. } => "DocumentOpened",
            Self::HostAction { .. } => "HostAction",
            // The kind names a key in the web envelope JSON, so it keeps the
            // spelling it was first written under.
            Self::WorkspaceBootstrap { .. } => "WorkspaceBootstrap",
            Self::DocumentContentWant { .. } => "DocumentContentWant",
            Self::FetchGrant { .. } => "FetchGrant",
            Self::AgentSession { .. } => "AgentSession",
            Self::AgentSessionAnswer { .. } => "AgentSessionAnswer",
            Self::AgentSessionStopped => "AgentSessionStopped",
            Self::Entered(_) => "Entered",
            Self::HostSaid { .. } => "HostSaid",
            Self::KeyFollower => "KeyFollower",
            Self::PersonalList { .. } => "PersonalList",
            Self::ChangeRefused { .. } => "ChangeRefused",
        }
    }

    /// The workspace (server workspace) this envelope belongs to, or `None` for
    /// the connection-level frames (`Ping`, `Pong`, `PeerServerHello`).
    ///
    /// The match is exhaustive on purpose: a new variant has to say which
    /// workspace it is charged to.
    #[must_use]
    pub const fn workspace_id(&self) -> Option<WorkspaceId> {
        match self {
            Self::Subscribe { workspace_id }
            | Self::Unsubscribe { workspace_id }
            | Self::Delta { workspace_id, .. }
            | Self::Presence { workspace_id, .. }
            | Self::Ready { workspace_id, .. }
            | Self::NotAuthorised { workspace_id, .. }
            | Self::SyncRequest { workspace_id, .. }
            | Self::SyncResponse { workspace_id, .. }
            | Self::RemoteAction { workspace_id, .. }
            | Self::RemoteActionUndeliverable { workspace_id, .. }
            | Self::DocumentContent { workspace_id, .. }
            | Self::Commit { workspace_id, .. }
            | Self::Committed { workspace_id, .. }
            | Self::CommitRefused { workspace_id, .. }
            | Self::ChangeRefused { workspace_id, .. }
            | Self::LiveJoin { workspace_id }
            | Self::LiveMember { workspace_id, .. }
            | Self::SubscribeFrom { workspace_id, .. }
            | Self::Position { workspace_id, .. }
            | Self::CompactVolunteer { workspace_id }
            | Self::CompactRequest { workspace_id, .. }
            | Self::Compacted { workspace_id, .. }
            | Self::OpenDocument { workspace_id, .. }
            | Self::DocumentOpened { workspace_id, .. }
            | Self::WorkspaceBootstrap { workspace_id, .. }
            | Self::DocumentContentWant { workspace_id, .. }
            | Self::HostSaid { workspace_id, .. } => Some(*workspace_id),
            Self::Ping
            | Self::Pong
            | Self::PeerServerHello { .. }
            | Self::HostAction { .. }
            | Self::AgentSession { .. }
            | Self::AgentSessionAnswer { .. }
            | Self::AgentSessionStopped
            | Self::Entered(_)
            | Self::KeyFollower
            | Self::FetchGrant { .. }
            | Self::PersonalList { .. } => None,
        }
    }
}

/// Bound, in characters, of the agent name an [`Envelope::AgentSession`] carries.
pub const MAX_AGENT_LABEL_CHARS: usize = 32;

/// Largest [`Envelope::DocumentContent`] frame a server routes.
pub const MAX_DOCUMENT_CONTENT_FRAME_BYTES: usize = 256 * 1024;

/// Version of the [`Envelope::RemoteAction`] kind this build routes. The
/// server refuses other versions as undeliverable instead of forwarding
/// frames whose routing fields it may misread.
pub const REMOTE_ACTION_ENVELOPE_VERSION: u16 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace id's postcard bytes: its length, then the UUID.
    fn id(last: u8) -> Vec<u8> {
        let mut bytes = vec![16];
        bytes.extend([0; 15]);
        bytes.push(last);
        assert_eq!(bytes.len(), 17, "an id is a length byte and 16 UUID bytes");
        bytes
    }

    tilo_test_env::scenario! {
        fn the_transport_wire_is_frozen_and_frames_name_their_workspace;

        phase frozen_v1_envelope_bytes;
        {
            // Literal ABI fixtures: a change to a discriminant or a field's
            // order moves what every frame after it means on the wire.
            let cases = [
                (
                    Envelope::Subscribe {
                        workspace_id: WorkspaceId::from_u128(7),
                    },
                    [vec![0], id(7)].concat(),
                ),
                (
                    Envelope::Unsubscribe {
                        workspace_id: WorkspaceId::from_u128(7),
                    },
                    [vec![1], id(7)].concat(),
                ),
                (
                    Envelope::Presence {
                        workspace_id: WorkspaceId::from_u128(7),
                        payload: vec![9, 8],
                    },
                    [vec![3], id(7), vec![2, 9, 8]].concat(),
                ),
                (
                    Envelope::Ready {
                        workspace_id: WorkspaceId::from_u128(7),
                        pending_deltas: 2,
                        documents_when_opened: true,
                    },
                    [vec![4], id(7), vec![2, 1]].concat(),
                ),
                (
                    Envelope::NotAuthorised {
                        workspace_id: WorkspaceId::from_u128(7),
                        refusal: crate::refusal::Refusal::NotAMember("no".into()),
                    },
                    [vec![5], id(7), vec![4, 2, 110, 111]].concat(),
                ),
                (Envelope::Ping, vec![6]),
                (Envelope::Pong, vec![7]),
                (
                    Envelope::PeerServerHello {
                        hosted_workspaces: vec![WorkspaceId::from_u128(7), WorkspaceId::from_u128(8)],
                    },
                    [vec![8, 2], id(7), id(8)].concat(),
                ),
                (
                    Envelope::SyncRequest {
                        workspace_id: WorkspaceId::from_u128(7),
                        have_heads: vec![[9; 32]],
                    },
                    [vec![9], id(7), vec![1], vec![9; 32]].concat(),
                ),
                (
                    Envelope::SyncResponse {
                        workspace_id: WorkspaceId::from_u128(7),
                        delta_since: vec![9, 8],
                    },
                    [vec![10], id(7), vec![2, 9, 8]].concat(),
                ),
                (
                    Envelope::Delta {
                        workspace_id: WorkspaceId::from_u128(7),
                        author_device_public_key: [1; 32],
                        authored_at_ns: 8,
                        delta: vec![9],
                        signature: [2; 64],
                    },
                    [vec![2], id(7), vec![1; 32], vec![8, 1, 9, 64], vec![2; 64]].concat(),
                ),
            ];
            // The frame codec (length prefix) lives in `tilo-server-client`; the
            // payload bytes it carries are these postcard encodings.
            for (envelope, payload) in cases {
                assert_eq!(postcard::to_stdvec(&envelope).unwrap(), payload);
                assert_eq!(
                    postcard::from_bytes::<Envelope>(&payload).unwrap(),
                    envelope
                );
            }
        }

        phase frames_name_their_workspace_and_connection_frames_none;
        {
            let delta = Envelope::Delta {
                workspace_id: WorkspaceId::from_u128(9),
                author_device_public_key: [1; 32],
                authored_at_ns: 2,
                delta: vec![3],
                signature: [0; 64],
            };
            assert_eq!(delta.workspace_id(), Some(WorkspaceId::from_u128(9)));
            assert_eq!(
                Envelope::Presence {
                    workspace_id: WorkspaceId::from_u128(4),
                    payload: Vec::new(),
                }
                .workspace_id(),
                Some(WorkspaceId::from_u128(4))
            );
            assert_eq!(Envelope::Ping.workspace_id(), None);
            assert_eq!(
                Envelope::PeerServerHello {
                    hosted_workspaces: vec![WorkspaceId::from_u128(1)]
                }
                .workspace_id(),
                None
            );
        }

        phase alpn_is_stable;
        {
            assert_eq!(ALPN, b"tilo-server/1");
        }
    }
}
