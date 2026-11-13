//! Wire messages exchanged between a peer and the relay after the
//! [`auth`](crate::auth) handshake.
//!
//! The framing that carries them lives with the stream I/O in
//! `tilo-relay-client`'s `frame` module; this module only defines the values.
//!
//! The relay uses **one long-lived bidirectional stream per
//! connection**. After the handshake, the stream is a sequence of
//! length-prefixed postcard-encoded [`Envelope`] messages:
//!
//! - The relay is not a general-purpose RPC service — its message
//!   shape is a small closed enum.
//! - Length-prefixed framing lets the reader skip malformed messages
//!   without desynchronising the stream.
//! - Postcard keeps the encoding compact + deterministic (no field-
//!   reorder) so integration tests can byte-compare fixtures.

use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};

/// ALPN of the relay stream. It never changes: the peer protocol version
/// is the handshake's first field, so a device at another version is told so
/// there, and there is one framing per version
/// (docs/architecture/compatibility.md).
pub const ALPN: &[u8] = b"tilo-relay/1";
/// ALPN of the one connection between two of a person's devices, which
/// carries their device-to-device session: each side first proves its
/// person's certificates (the endpoint id proves the device key), then the
/// two exchange what is for the person's own devices alone. The device with
/// the lower endpoint id dials it, the other only when it is not dialled;
/// of two dialled at once, both keep the lower id's.
pub const SIBLING_ALPN: &[u8] = b"tilo-relay-sibling/1";
/// Authenticated one-shot discovery of direct filesystem endpoints.
pub const DISCOVERY_ALPN: &[u8] = b"tilo-relay-discovery/2";

/// Authenticated owner-only health and lifecycle management protocol.
pub const MANAGEMENT_ALPN: &[u8] = b"tilo-home-peer-management/1";

/// Small, stable discovery reply consumed directly by filesystem workers.
/// Keeping this separate from the relay's richer placement profile avoids a
/// relay-only address ever being mistaken for a content endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEndpointAdvertisement {
    /// Explicit schema version for persisted/client registration envelopes.
    pub schema_version: u16,
    /// Relay workspace whose signed filesystem authority authorised this lookup.
    pub workspace_id: u64,
    /// Direct authenticated filesystem/blob endpoints currently ready for it.
    pub filesystem_endpoints: Vec<EndpointAddr>,
    /// Authenticated Files workspace binding used to join the record plane.
    pub filesystem_ticket: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Complete authenticated registration handed to the local Files daemon.
pub struct ContentPeerRegistration {
    /// Direct Iroh endpoint serving filesystem records and blobs.
    pub endpoint: EndpointAddr,
    /// Encoded Files ticket binding a fresh daemon to the shared namespace.
    pub ticket: String,
    /// Relay service that authenticated this registration.  This binds a
    /// persisted Files endpoint to the enrolled home peer which nominated it,
    /// so removing that peer can revoke its hydration route too.
    pub relay_service_id: [u8; 32],
}

/// Bounded owner operation for an enrolled home peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomePeerManagementRequest {
    /// Read current service and full-replica health.
    Status,
    /// Change the operator-facing server name.
    Rename {
        /// New operator-facing label, bounded to 80 bytes.
        label: String,
    },
    /// Revoke this host and stop all hosted services.
    Decommission {
        /// Root-signed permanent revocation targeting this host certificate.
        revocation: tilo_user_identity::RevocationEntry,
    },
}

/// Authenticated result returned by a home peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePeerManagementResponse {
    /// Certified host device key.
    pub device_public_key: [u8; 32],
    /// Current network-addressable relay service endpoint. The client has
    /// authenticated the certified host before accepting this advertisement.
    pub relay_endpoint: iroh_base::EndpointAddr,
    /// Current operator-facing label.
    pub label: String,
    /// Whether the full filesystem replica currently proves readiness.
    pub files_ready: bool,
    /// Active service advertisement after capability clamping.
    pub services: crate::mesh::ServiceCapabilities,
    /// Whether this response completed permanent decommissioning.
    pub decommissioned: bool,
}

/// A client must name the relay workspace it is resolving. The host never exposes
/// its filesystem endpoints as a global, unauthorised discovery answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEndpointRequest {
    /// Relay workspace whose filesystem endpoint the caller wants to resolve.
    pub workspace_id: u64,
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
    /// Peer asks to receive updates for `workspace_id`. The relay
    /// registers the peer as a subscriber and immediately drains
    /// any pending deltas queued for it (store-and-forward).
    Subscribe {
        /// Workspace id.
        workspace_id: u64,
    },

    /// Peer stops receiving updates for `workspace_id`. The
    /// subscription is removed; any deltas received while
    /// unsubscribed are queued for the next Subscribe.
    Unsubscribe {
        /// Workspace id.
        workspace_id: u64,
    },

    /// A workspace-directory CRDT delta. The relay:
    /// 1. Applies it to its own pinned copy (if pinned).
    /// 2. Fans it out to every other peer currently subscribed.
    /// 3. Queues it for every other member device that is offline.
    Delta {
        workspace_id: u64,
        /// Author device pubkey (redundant with the peer's cert on
        /// this connection but included so a fan-out recipient can
        /// dedup / attribute without maintaining connection state).
        author_device_public_key: [u8; 32],
        /// Authored-at nanoseconds since epoch.
        authored_at_ns: u64,
        /// Automerge incremental delta bytes.
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
        /// Author's signature over the delta — the same
        /// `DirectoryUpdate.signature` shape the identity crate
        /// produces.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
    },

    /// Retired: a presence event, broadcast as an opaque payload. Nothing
    /// sends it — presence rides the workspace's live plane, on its presence
    /// track — and it stays only so every later variant keeps its postcard
    /// position.
    RetiredPresence {
        workspace_id: u64,
        /// Peer-defined presence payload. The relay never inspects
        /// the bytes.
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    },

    /// The relay's response to a `Subscribe`: how many pending
    /// deltas the relay is about to flush from the peer's
    /// store-and-forward queue.
    Ready {
        workspace_id: u64,
        /// Number of pending deltas queued for this peer that the
        /// relay is about to flush.
        pending_deltas: u32,
        /// The workspace sends this peer its records only when it opens
        /// them ([`Self::OpenRecord`]), as a public workspace does: the
        /// replay carried its governance and shared desktop, and a record
        /// the peer holds from before, or does not hold at all, is asked
        /// for when it is opened, never taken as complete or as new.
        records_when_opened: bool,
    },

    /// The relay's response to a subscribe that failed — e.g.
    /// workspace not bound here, peer not a member.
    NotAuthorised {
        workspace_id: u64,
        /// Human-readable reason (for tracing / logging).
        reason: String,
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
    /// first, the accepter replies with its own hosted set. Only
    /// meaningful when the sender's cert has
    /// `capabilities.can_act_as_host = true` — otherwise the receiver
    /// ignores it.
    PeerRelayHello {
        /// Workspace ids this host currently hosts.
        hosted_workspaces: Vec<u64>,
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
        workspace_id: u64,
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
        workspace_id: u64,
        /// Concatenated Automerge changes the requester was missing.
        #[serde(with = "serde_bytes")]
        delta_since: Vec<u8>,
    },

    /// A remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) addressed to one device of the workspace.
    ///
    /// The relay routes it to the addressed device's live connection and
    /// never stores, replays or executes it: an action that ran late would
    /// be worse than one that did not run. Only when the addressed device is
    /// the relay itself *and* an app-serving plane was supplied does the
    /// relay answer it, through that plane. Anything else comes back as
    /// [`Envelope::RemoteActionUndeliverable`].
    RemoteAction {
        /// Workspace id both devices are subscribed to.
        workspace_id: u64,
        /// Version of this envelope kind, see [`REMOTE_ACTION_ENVELOPE_VERSION`].
        version: u16,
        /// The addressed device.
        to_device_public_key: [u8; 32],
        /// The sending device, stamped by the relay from the authenticated
        /// connection. Whatever a sender puts here is overwritten.
        from_device_public_key: [u8; 32],
        /// The user root the sending device chains to, stamped the same way.
        from_user_public_key: [u8; 32],
        /// Opaque remote-action frame.
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// The relay could not deliver a [`Envelope::RemoteAction`]: the
    /// addressed device is not connected to this workspace, the envelope
    /// version is unknown, or nothing here serves apps.
    RemoteActionUndeliverable {
        workspace_id: u64,
        /// The device it was addressed to.
        to_device_public_key: [u8; 32],
        /// Human-readable reason (for tracing / logging).
        reason: String,
    },

    /// A [`Envelope::Delta`] whose author wants to know when the host has
    /// made it durable. The host admits it exactly like a `Delta`, fans it
    /// out to other subscribers as a plain `Delta`, and answers the author
    /// with [`Envelope::Committed`] once it is persisted, or
    /// [`Envelope::CommitRefused`]. Only the author sees either answer, so
    /// peers that predate these variants never receive them.
    Commit {
        workspace_id: u64,
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
        workspace_id: u64,
        /// The commit's id.
        op: [u8; 16],
        /// Host order of the commit within the workspace.
        seq: u64,
    },

    /// The host did not admit commit `op`. Nothing was persisted or fanned
    /// out.
    CommitRefused {
        workspace_id: u64,
        /// The commit's id.
        op: [u8; 16],
        /// Human-readable reason (for tracing / logging).
        reason: String,
    },

    /// Join the workspace's live plane: the host answers with the members
    /// already present and tells them this device joined. The name keeps the
    /// variant's place in the wire enum.
    RetiredLiveJoin {
        /// Workspace id.
        workspace_id: u64,
    },

    /// Leave the workspace's live plane. Nothing sends it — a device that
    /// goes is dropped with its connection — and it stays only so every
    /// later variant keeps its postcard position.
    LiveLeave {
        /// Workspace id.
        workspace_id: u64,
    },

    /// Retired: one ephemeral live frame ([`crate::live::LiveFrame`] bytes)
    /// relayed by the host. Nothing sends it — frames travel on the plane —
    /// and it stays only so every later variant keeps its postcard position.
    RetiredLive {
        workspace_id: u64,
        /// Publishing device, stamped by the host. Whatever a sender puts
        /// here is overwritten.
        from_device_public_key: [u8; 32],
        /// Canonical [`crate::live::LiveFrame`] encoding.
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// A device joined (`active`) or left the workspace's live plane, sent
    /// by the host to the plane's other members. The name keeps the
    /// variant's place in the wire enum.
    RetiredLiveMember {
        workspace_id: u64,
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
        workspace_id: u64,
        /// The last position received for this workspace, if any.
        position: Option<Vec<u8>>,
    },

    /// Everything the host delivered to this subscriber so far. Opaque and
    /// host-authenticated; present it to [`Envelope::SubscribeFrom`] after
    /// a reconnect. Only subscribers that used `SubscribeFrom` receive it.
    Position {
        workspace_id: u64,
        position: Vec<u8>,
    },

    /// This device answers [`Envelope::CompactRequest`]s for
    /// `workspace_id` while its subscription lasts. Sent only to a host
    /// that advertised compaction.
    CompactVolunteer {
        /// Workspace id.
        workspace_id: u64,
    },

    /// The host holds a long sealed tail for one route and asks this member
    /// for a snapshot of it and a
    /// [`crate::compaction::CoverageStatement`]. Sent only to volunteers.
    CompactRequest {
        workspace_id: u64,
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
        workspace_id: u64,
        /// Encoded [`crate::compaction::CoverageStatement`].
        #[serde(with = "serde_bytes")]
        statement: Vec<u8>,
        /// The member's device signature over
        /// [`crate::compaction::signing_bytes`] of `statement`.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
    },

    /// Open, start, end or moderate a call of a workspace the host governs (see
    /// [`crate::voice`]). Requires an accepted `Subscribe`; opening a
    /// channel's call takes a member, everything else the workspace's owner or an
    /// administrator. Answered with [`Envelope::VoiceCallResult`].
    /// Sent only to a host that advertised voice.
    VoiceCall {
        workspace_id: u64,
        /// Caller-chosen id echoed in the answer.
        request: [u8; 16],
        /// The change.
        operation: crate::voice::CallOperation,
    },

    /// The outcome of [`Envelope::VoiceCall`] `request`.
    VoiceCallResult {
        workspace_id: u64,
        /// The request's id.
        request: [u8; 16],
        /// The call as governance now records it, or why the change was
        /// refused.
        result: Result<crate::voice::CallState, String>,
    },

    /// Join an active call, or with `participation`, repeat an earlier join
    /// as its heartbeat. Requires an accepted `Subscribe`. Answered with
    /// [`Envelope::VoiceGranted`] or [`Envelope::VoiceRefused`].
    VoiceJoin {
        workspace_id: u64,
        call: [u8; 32],
        /// The participation to continue; `None` joins afresh.
        participation: Option<[u8; 32]>,
    },

    /// A participation's current grant: the answer to a join, and sent
    /// unasked when the call's roster or this participation's directions
    /// change.
    VoiceGranted {
        workspace_id: u64,
        grant: Box<crate::voice::VoiceGrant>,
    },

    /// The device may not be in the call (any more).
    VoiceRefused {
        workspace_id: u64,
        call: [u8; 32],
        /// Human-readable reason.
        reason: String,
    },

    /// Leave a call.
    VoiceLeave {
        workspace_id: u64,
        call: [u8; 32],
        /// The participation that ends.
        participation: [u8; 32],
    },

    /// Ask for the calls of a workspace the host governs. Requires an accepted
    /// `Subscribe`. Answered with [`Envelope::VoiceCallList`].
    VoiceCalls {
        /// Workspace id.
        workspace_id: u64,
    },

    /// A workspace's calls as its governance records them, with who is in each.
    VoiceCallList {
        workspace_id: u64,
        /// The calls, or why the workspace has none to list.
        result: Result<Vec<crate::voice::CallSummary>, String>,
    },

    /// One piece of a record's content for exactly one device of the
    /// workspace, which asked the workspace for it. Routed like
    /// [`Envelope::RemoteAction`] — to the addressed device's live
    /// connection, never stored, never fanned out — but never answered:
    /// content that does not arrive is asked for again. A shared window's
    /// control message (a viewer's ask, its input, a keyframe request) takes
    /// the same route to a device no direct connection reaches, such as a
    /// hosted environment's.
    RecordContent {
        /// Workspace id both devices are subscribed to.
        workspace_id: u64,
        /// The addressed device.
        to_device_public_key: [u8; 32],
        /// The sending device, stamped by the relay from the authenticated
        /// connection. Whatever a sender puts here is overwritten.
        from_device_public_key: [u8; 32],
        /// Opaque piece, at most [`MAX_RECORD_CONTENT_FRAME_BYTES`].
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// Open an on-demand record of a subscribed workspace: a record a joining
    /// device is not replayed (`neighbour_wire::record_sync`). The relay
    /// sends what it retains of the record as [`Envelope::Delta`]s, under
    /// the same admission as a replay, then [`Envelope::RecordOpened`];
    /// from then on the record's live changes reach the device too.
    OpenRecord {
        /// Workspace id, subscribed.
        workspace_id: u64,
        /// The record's retained channel
        /// (`neighbour_wire::app_document_channel`).
        channel: [u8; 32],
    },

    /// The relay sent everything it retains of an opened record.
    RecordOpened {
        workspace_id: u64,
        /// The record's retained channel.
        channel: [u8; 32],
        /// How many frames it sent.
        frames: u32,
    },

    /// What `participation` shows besides its voice from now on (its
    /// camera, a shared screen). The relay records it on the participation
    /// and tells the call's participants and the workspace's call list when it
    /// changed. Not answered.
    VoiceShow {
        workspace_id: u64,
        call: [u8; 32],
        participation: [u8; 32],
        /// What it shows.
        showing: crate::voice::Showing,
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
        workspace_id: u64,
        #[serde(with = "serde_bytes")]
        bootstrap: Vec<u8>,
    },

    /// A member asks the workspace for content one of its records
    /// references. The host fans it out to the workspace's other members and
    /// answers with what it keeps itself; each member answers to the asking
    /// device alone ([`Self::RecordContent`]). `frame` is the
    /// `NeighbourMsg::RecordContentWant` that says what is wanted, and
    /// `from_device_public_key` is the host's stamp of who asked.
    RecordContentWant {
        workspace_id: u64,
        from_device_public_key: [u8; 32],
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// A device asks the host it is joined to what it may do there and for its
    /// workspaces: the postcard-encoded [`WorkspacesRequest`], answered
    /// through [`Self::WorkspacesReply`] with the asker's `request_id`. The
    /// connection says who asks; the host checks that device's authority over
    /// each request as every door does.
    ///
    /// [`WorkspacesRequest`]: crate::workspaces::WorkspacesRequest
    WorkspacesRequest {
        request_id: u64,
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// The host's answer to [`Self::WorkspacesRequest`] with the same
    /// `request_id`: the postcard-encoded
    /// [`WorkspacesReply`](crate::workspaces::WorkspacesReply).
    WorkspacesReply {
        request_id: u64,
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// Retired: a device asked the host it is joined to who is here, and the
    /// host answered ([`Self::RetiredHereReply`]). Who is here is the live
    /// plane alone now; nothing sends it, and it stays only so every later
    /// variant keeps its postcard position.
    RetiredHereRequest {
        request_id: u64,
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },

    /// Retired: the host's answer to [`Self::RetiredHereRequest`]. Nothing
    /// sends it; it keeps its postcard position.
    RetiredHereReply {
        request_id: u64,
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
    /// History names its runs), answered through [`Self::AgentRunAnswer`]
    /// with the asker's `request_id`. Said before anything else: every
    /// request the session makes from then on is the agent's, for the person,
    /// within what bounds an agent, and the person's agent door lists the run
    /// while the session lasts.
    AgentRun { request_id: u64, label: String },

    /// The host's answer to [`Self::AgentRun`] with the same `request_id`:
    /// `None` once the session is that agent's run, else why not.
    AgentRunAnswer {
        request_id: u64,
        refusal: Option<String>,
    },

    /// The host's word that the person stopped the agent run this session is
    /// ([`Self::AgentRun`]), said just before the host ends it: the device
    /// does not connect again, so the agent and its presence end here.
    AgentRunStopped,

    /// The host's answer to the entry the session's hello asked for
    /// ([`crate::entering`]): the first envelope of that session, before
    /// anything else it is sent.
    Entered(crate::entering::Entered),

    /// The host's own word for a run it executes
    /// ([`crate::host_voice::HostSaid`]): sent by the host's device to the
    /// relay that is the host, which fans it out to the workspace's other
    /// subscribers only when its own device said it. A device takes it only
    /// when `signature` is its host's over `said`. It never rides the live
    /// plane, whose keys a host that is no member of it does not hold.
    HostSaid {
        workspace_id: u64,
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
    ChannelReader,
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
            Self::RetiredPresence { .. } => "Presence",
            Self::Ready { .. } => "Ready",
            Self::NotAuthorised { .. } => "NotAuthorised",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
            Self::PeerRelayHello { .. } => "PeerRelayHello",
            Self::SyncRequest { .. } => "SyncRequest",
            Self::SyncResponse { .. } => "SyncResponse",
            Self::RemoteAction { .. } => "RemoteAction",
            Self::RemoteActionUndeliverable { .. } => "RemoteActionUndeliverable",
            Self::Commit { .. } => "Commit",
            Self::Committed { .. } => "Committed",
            Self::CommitRefused { .. } => "CommitRefused",
            Self::RetiredLiveJoin { .. } => "LiveJoin",
            Self::LiveLeave { .. } => "LiveLeave",
            Self::RetiredLive { .. } => "Live",
            Self::RetiredLiveMember { .. } => "LiveMember",
            Self::SubscribeFrom { .. } => "SubscribeFrom",
            Self::Position { .. } => "Position",
            Self::CompactVolunteer { .. } => "CompactVolunteer",
            Self::CompactRequest { .. } => "CompactRequest",
            Self::Compacted { .. } => "Compacted",
            Self::VoiceCall { .. } => "VoiceCall",
            Self::VoiceCallResult { .. } => "VoiceCallResult",
            Self::VoiceJoin { .. } => "VoiceJoin",
            Self::VoiceGranted { .. } => "VoiceGranted",
            Self::VoiceRefused { .. } => "VoiceRefused",
            Self::VoiceLeave { .. } => "VoiceLeave",
            Self::VoiceCalls { .. } => "VoiceCalls",
            Self::VoiceCallList { .. } => "VoiceCallList",
            Self::RecordContent { .. } => "RecordContent",
            Self::OpenRecord { .. } => "OpenRecord",
            Self::RecordOpened { .. } => "RecordOpened",
            Self::VoiceShow { .. } => "VoiceShow",
            Self::HostAction { .. } => "HostAction",
            // The kind names a key in the web envelope JSON, so it keeps the
            // spelling it was first written under.
            Self::WorkspaceBootstrap { .. } => "RoomBootstrap",
            Self::RecordContentWant { .. } => "RecordContentWant",
            Self::WorkspacesRequest { .. } => "WorkspacesRequest",
            Self::WorkspacesReply { .. } => "WorkspacesReply",
            Self::RetiredHereRequest { .. } => "HereRequest",
            Self::RetiredHereReply { .. } => "HereReply",
            Self::FetchGrant { .. } => "FetchGrant",
            Self::AgentRun { .. } => "AgentRun",
            Self::AgentRunAnswer { .. } => "AgentRunAnswer",
            Self::AgentRunStopped => "AgentRunStopped",
            Self::Entered(_) => "Entered",
            Self::HostSaid { .. } => "HostSaid",
            Self::ChannelReader => "ChannelReader",
        }
    }

    /// The workspace (relay workspace) this envelope belongs to, or `None` for
    /// the connection-level frames (`Ping`, `Pong`, `PeerRelayHello`).
    ///
    /// The match is exhaustive on purpose: a new variant has to say which
    /// workspace it is charged to.
    #[must_use]
    pub const fn workspace_id(&self) -> Option<u64> {
        match self {
            Self::Subscribe { workspace_id }
            | Self::Unsubscribe { workspace_id }
            | Self::Delta { workspace_id, .. }
            | Self::RetiredPresence { workspace_id, .. }
            | Self::Ready { workspace_id, .. }
            | Self::NotAuthorised { workspace_id, .. }
            | Self::SyncRequest { workspace_id, .. }
            | Self::SyncResponse { workspace_id, .. }
            | Self::RemoteAction { workspace_id, .. }
            | Self::RemoteActionUndeliverable { workspace_id, .. }
            | Self::RecordContent { workspace_id, .. }
            | Self::Commit { workspace_id, .. }
            | Self::Committed { workspace_id, .. }
            | Self::CommitRefused { workspace_id, .. }
            | Self::RetiredLiveJoin { workspace_id }
            | Self::LiveLeave { workspace_id }
            | Self::RetiredLive { workspace_id, .. }
            | Self::RetiredLiveMember { workspace_id, .. }
            | Self::SubscribeFrom { workspace_id, .. }
            | Self::Position { workspace_id, .. }
            | Self::CompactVolunteer { workspace_id }
            | Self::CompactRequest { workspace_id, .. }
            | Self::Compacted { workspace_id, .. }
            | Self::VoiceCall { workspace_id, .. }
            | Self::VoiceCallResult { workspace_id, .. }
            | Self::VoiceJoin { workspace_id, .. }
            | Self::VoiceGranted { workspace_id, .. }
            | Self::VoiceRefused { workspace_id, .. }
            | Self::VoiceLeave { workspace_id, .. }
            | Self::VoiceCalls { workspace_id }
            | Self::VoiceCallList { workspace_id, .. }
            | Self::VoiceShow { workspace_id, .. }
            | Self::OpenRecord { workspace_id, .. }
            | Self::RecordOpened { workspace_id, .. }
            | Self::WorkspaceBootstrap { workspace_id, .. }
            | Self::RecordContentWant { workspace_id, .. }
            | Self::HostSaid { workspace_id, .. } => Some(*workspace_id),
            Self::Ping
            | Self::Pong
            | Self::PeerRelayHello { .. }
            | Self::HostAction { .. }
            | Self::WorkspacesRequest { .. }
            | Self::WorkspacesReply { .. }
            | Self::RetiredHereRequest { .. }
            | Self::RetiredHereReply { .. }
            | Self::AgentRun { .. }
            | Self::AgentRunAnswer { .. }
            | Self::AgentRunStopped
            | Self::Entered(_)
            | Self::ChannelReader
            | Self::FetchGrant { .. } => None,
        }
    }
}

/// Bound, in characters, of the agent name an [`Envelope::AgentRun`] carries.
pub const MAX_AGENT_LABEL_CHARS: usize = 32;

/// Largest [`Envelope::RecordContent`] frame a relay routes.
pub const MAX_RECORD_CONTENT_FRAME_BYTES: usize = 256 * 1024;

/// Version of the [`Envelope::RemoteAction`] kind this build routes. The
/// relay refuses other versions as undeliverable instead of forwarding
/// frames whose routing fields it may misread.
pub const REMOTE_ACTION_ENVELOPE_VERSION: u16 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_v1_envelope_bytes() {
        // Literal ABI fixtures: changing an existing discriminant or field
        // ordering would strand deployed readers and retained state.
        let cases = [
            (Envelope::Subscribe { workspace_id: 7 }, vec![0, 7]),
            (Envelope::Unsubscribe { workspace_id: 7 }, vec![1, 7]),
            (
                Envelope::RetiredPresence {
                    workspace_id: 7,
                    payload: vec![9, 8],
                },
                vec![3, 7, 2, 9, 8],
            ),
            (
                Envelope::Ready {
                    workspace_id: 7,
                    pending_deltas: 2,
                    records_when_opened: true,
                },
                vec![4, 7, 2, 1],
            ),
            (
                Envelope::NotAuthorised {
                    workspace_id: 7,
                    reason: "no".into(),
                },
                vec![5, 7, 2, 110, 111],
            ),
            (Envelope::Ping, vec![6]),
            (Envelope::Pong, vec![7]),
            (
                Envelope::PeerRelayHello {
                    hosted_workspaces: vec![7, 8],
                },
                vec![8, 2, 7, 8],
            ),
            (
                Envelope::SyncRequest {
                    workspace_id: 7,
                    have_heads: vec![[9; 32]],
                },
                [vec![9, 7, 1], vec![9; 32]].concat(),
            ),
            (
                Envelope::SyncResponse {
                    workspace_id: 7,
                    delta_since: vec![9, 8],
                },
                vec![10, 7, 2, 9, 8],
            ),
            (
                Envelope::Delta {
                    workspace_id: 7,
                    author_device_public_key: [1; 32],
                    authored_at_ns: 8,
                    delta: vec![9],
                    signature: [2; 64],
                },
                [vec![2, 7], vec![1; 32], vec![8, 1, 9, 64], vec![2; 64]].concat(),
            ),
        ];
        // The frame codec (length prefix) lives in `tilo-relay-client`; the
        // payload bytes it carries are these postcard encodings.
        for (envelope, payload) in cases {
            assert_eq!(postcard::to_stdvec(&envelope).unwrap(), payload);
            assert_eq!(
                postcard::from_bytes::<Envelope>(&payload).unwrap(),
                envelope
            );
        }
    }

    #[test]
    fn frozen_voice_envelope_bytes() {
        let cases = [
            (
                Envelope::VoiceJoin {
                    workspace_id: 7,
                    call: [1; 32],
                    participation: None,
                },
                [vec![27, 7], vec![1; 32], vec![0]].concat(),
            ),
            (
                Envelope::VoiceJoin {
                    workspace_id: 7,
                    call: [1; 32],
                    participation: Some([2; 32]),
                },
                [vec![27, 7], vec![1; 32], vec![1], vec![2; 32]].concat(),
            ),
            (
                Envelope::VoiceLeave {
                    workspace_id: 7,
                    call: [1; 32],
                    participation: [2; 32],
                },
                [vec![30, 7], vec![1; 32], vec![2; 32]].concat(),
            ),
        ];
        for (envelope, payload) in cases {
            assert_eq!(postcard::to_stdvec(&envelope).unwrap(), payload);
            assert_eq!(
                postcard::from_bytes::<Envelope>(&payload).unwrap(),
                envelope
            );
        }
    }

    #[test]
    fn room_frames_name_their_workspace_and_connection_frames_none() {
        let delta = Envelope::Delta {
            workspace_id: 9,
            author_device_public_key: [1; 32],
            authored_at_ns: 2,
            delta: vec![3],
            signature: [0; 64],
        };
        assert_eq!(delta.workspace_id(), Some(9));
        assert_eq!(
            Envelope::RetiredPresence {
                workspace_id: 4,
                payload: Vec::new(),
            }
            .workspace_id(),
            Some(4)
        );
        assert_eq!(Envelope::Ping.workspace_id(), None);
        assert_eq!(
            Envelope::PeerRelayHello {
                hosted_workspaces: vec![1]
            }
            .workspace_id(),
            None
        );
    }

    #[test]
    fn alpn_is_stable() {
        assert_eq!(ALPN, b"tilo-relay/1");
        assert_eq!(DISCOVERY_ALPN, b"tilo-relay-discovery/2");
        assert_eq!(MANAGEMENT_ALPN, b"tilo-home-peer-management/1");
    }
}
