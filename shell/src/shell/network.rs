//! Shell-side bridge into the peer-sync [`Actor`]. Owns the
//! [`WorkspaceTreeReplica`], drives the `tokio` runtime that hosts the
//! actor, and exposes a small handle the shell calls into to drain
//! incoming `NetEvent`s and forward replica replies back out as
//! `NetCmd::Publish`s.
//!
//! v1 wires the actor into the shell's startup path behind an opt-in
//! flag (`--enable-peer-sync`). Tasks 3.2 + 3.3 add cursor-presence
//! pumping and palette commands on top.
//!
//! Plan reference: `docs/superpowers/plans/2026-05-02-peer-sync-end-to-end.md`,
//! Phase 3 Task 3.1.

use std::sync::{Arc, Mutex};

use libp2p::SwarmBuilder;
use tokio::sync::mpsc;

use crate::network::actor::{Actor, NetCmd, NetEvent};
use crate::network::behaviour::{NiriosBehaviour, load_or_create_keypair};
use crate::network::hub::TopicSubscriber;
use crate::network::invite::accept_invite_into;
use crate::network::protocol::NeighbourMsg;
use crate::network::replica::WorkspaceTreeReplica;
#[cfg(test)]
use crate::shell::state::WorkspaceTree;
use crate::shell::state::WorkspaceTreeHandle;

/// Default workspace topic id that v1 publishes/subscribes on. Per-
/// workspace routing is a Phase 3 follow-up — every shell currently
/// shares the same gossipsub topic so a v1 LAN demo "just works"
/// without per-workspace negotiation.
pub const DEFAULT_WS_TOPIC: u64 = 0;

/// Largest app-document snapshot this shell will put on the gossip
/// topic, in bytes.
///
/// # Where the number comes from
///
/// libp2p gossipsub caps one RPC at 64 KiB and
/// [`crate::network::behaviour::NiriosBehaviour::new`] does not raise
/// it, so a larger publish is rejected by the transport rather than
/// truncated.
///
/// The budget is far tighter than that ceiling suggests, because the
/// actor serialises a [`NeighbourMsg`] with `serde_json`
/// (`crate::network::actor`), and serde_json writes a `Vec<u8>` as an
/// array of decimal numbers — up to four characters (`"255,"`) per
/// byte. A snapshot therefore costs up to **4x its size** on the wire,
/// and the naive "64 KiB minus a little" reading of the limit is wrong
/// by a factor of four. `snapshot_at_the_cap_fits_a_gossipsub_rpc`
/// pins this against worst-case bytes rather than against the
/// arithmetic in this comment, which is how the earlier figure here was
/// caught.
///
/// So: 14 KiB of document expands to at most 56 KiB of JSON, leaving
/// ~9 KiB for the envelope — the variant tag, the room uuid, a record
/// id that may be a full path, and gossipsub's own protobuf framing
/// (topic, signature, author key, sequence number).
///
/// Moving the publish path to the `postcard` encoding that
/// `crate::network::protocol`'s own round-trip tests already use would
/// make the payload roughly 1:1 and raise this to ~60 KiB. That is a
/// wire-format change every peer has to agree on, so it is a decision
/// to take deliberately rather than a constant to edit.
///
/// # Why a cap rather than a bigger pipe
///
/// A document has no upper bound, so no ceiling makes this correct in
/// general — raising the gossipsub limit would move the failure, not
/// remove it, and would do so on a **broadcast** medium where every
/// peer in the room pays for every byte. The bounded case is worth
/// having anyway because it is the common one: a document that fits is
/// a document that converges, immediately, over transport that already
/// exists.
///
/// Past this size the right carrier is a direct peer-to-peer stream
/// rather than gossip — the substrate for one is already in
/// [`crate::network::surface_stream`], which video frames use for the
/// same reason (they are large, and they concern one peer rather than
/// the room). Wiring a snapshot onto it is the follow-up this constant
/// marks the boundary of.
pub const MAX_APP_SNAPSHOT_BYTES: usize = 14 * 1024;

/// Default on-disk path of the machine's device key, from which the
/// shell derives its libp2p identity. The FUSE mount reads the same
/// file, so one device has one identity rather than one per subsystem.
pub const DEFAULT_KEYPAIR_PATH: &str = crate::network::behaviour::DEVICE_KEY_PATH;

/// Where this session's device key lives.
///
/// [`DEFAULT_KEYPAIR_PATH`] unless `QOS_PEER_KEY_PATH` names somewhere
/// else. The override is what makes two sessions on one machine two
/// *peers*: the key is the shell's network identity and also the seed
/// for the id salt that keeps their minted ids disjoint, so two
/// processes reading one key file are one peer talking to itself.
pub fn peer_key_path() -> std::path::PathBuf {
    match std::env::var("QOS_PEER_KEY_PATH") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from(DEFAULT_KEYPAIR_PATH),
    }
}

/// Transport addresses the swarm binds on startup.
///
/// Loopback is listed first so a two-session demonstration on one
/// machine has a dialable address that does not depend on which
/// interface is up; the wildcard covers the LAN case.
const LISTEN_ADDRS: [&str; 2] = ["/ip4/127.0.0.1/tcp/0", "/ip4/0.0.0.0/tcp/0"];

/// How often a shell asks the rooms it is in for anything it is
/// missing. Room replication has no push side, so this interval is the
/// upper bound on how stale a guest's view of a room can be.
const SYNC_KICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// One outstanding invitation, and the name promised to whoever
/// redeems it.
///
/// Held per invitation rather than per room. A host invites people
/// faster than they arrive — two, three, before any of them knocks —
/// and every one of those invitations names the same room, so the room
/// cannot say which of them a given arrival redeemed. The invitation
/// can, and this is what it carries until it is spent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingInvite {
    /// The room this invitation is into. Checked at redemption: an id
    /// minted for one room does not redeem an invitation to another,
    /// even on a machine that governs both.
    room: crate::shell::state::RoomId,
    /// What the room will call whoever redeems it.
    name: String,
    /// The token's own expiry, so a promise nobody came for is dropped
    /// rather than kept for a machine that could no longer redeem it
    /// anyway.
    expires_unix_seconds: u64,
}

/// Wall-clock seconds since the unix epoch, or `0` if the system clock
/// is somehow before 1970. Matches what an invite token's expiry is
/// measured against.
fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Drop the promises nobody can come and collect any more.
///
/// Run on both ends of an invitation's life — when one is parked and
/// when one is redeemed — so an expired promise is never handed out,
/// and a host that invites people all afternoon does not keep every
/// name it has ever typed.
fn prune_expired(pending: &mut std::collections::HashMap<String, PendingInvite>) {
    let now = now_unix_seconds();
    pending.retain(|_, held| held.expires_unix_seconds > now);
}

/// Shell-side handle for the peer-sync subsystem.
///
/// Owns the tokio runtime that hosts the [`Actor`], the per-workspace
/// [`WorkspaceTreeReplica`], and the actor↔replica event channel. The
/// runtime is dropped (and the actor task awaited) when this struct is
/// dropped.
pub struct ShellNetwork {
    /// Send commands into the actor (e.g. publish a `NeighbourMsg`).
    pub cmd_tx: mpsc::Sender<NetCmd>,
    /// Shared replica that owns the tree-side reactions to
    /// `NetEvent::Msg`. Wrapped in `Arc<Mutex<_>>` so future tasks
    /// (cursor presence, palette commands) can hand a clone to the
    /// shell's per-frame update loop.
    pub replica: Arc<Mutex<WorkspaceTreeReplica>>,
    /// Sender side of the actor→replica event channel. Cloned and
    /// passed into the actor via `NetCmd::JoinWorkspace`.
    pub event_tx: mpsc::Sender<NetEvent>,
    /// Receiver side of the actor→replica event channel. Drained on
    /// each [`Self::tick`]. Wrapped in a `Mutex` so `tick` can take
    /// `&self` — cursor-presence pumping shares the handle behind
    /// `Arc<ShellNetwork>` between the broadcast sink (writes) and
    /// the frame-tick (reads).
    pub event_rx: Mutex<mpsc::Receiver<NetEvent>>,
    /// Tokio runtime that hosts the actor. `Option` so we can `take`
    /// it on drop and shut the runtime down cleanly without blocking
    /// the test thread.
    runtime: Option<tokio::runtime::Runtime>,
    /// Pre-computed hash of our local libp2p `PeerId`, used as the
    /// `peer` field on every outbound `NeighbourMsg::PresenceMoved`.
    /// Cached once at spawn so per-pointer-move publishes don't redo
    /// the hash work for every cursor delta.
    local_peer_hash: u64,
    /// This shell's libp2p identity. Held because an invite is a
    /// *signed* statement by the inviting owner — [`Self::issue_invite`]
    /// signs with it, and the receiver verifies against the public half
    /// carried in the token.
    keypair: libp2p::identity::Keypair,
    /// `PeerId` derived from [`Self::keypair`], appended to the listen
    /// address so an invite carries a fully-qualified `/p2p/…` dial
    /// target rather than a bare transport address.
    local_peer_id: libp2p::PeerId,
    /// Addresses the swarm reported binding, filled by the actor.
    listen_addrs: Arc<Mutex<Vec<libp2p::Multiaddr>>>,
    /// When [`Self::kick_periodic_sync`] last asked. `None` until the
    /// first tick, so the first frame after startup kicks a round.
    last_sync_kick: Mutex<Option<std::time::Instant>>,
    /// Names this machine has promised to whoever redeems each invite
    /// it has minted, keyed by that **invitation** — not by its room.
    ///
    /// An invite is minted before the person it is for has a `PeerId`
    /// this machine has ever seen, so the name cannot be written into
    /// the room document at mint time. It waits here until that peer
    /// asks this room for its document — the moment its libp2p identity
    /// is both known and authenticated — and
    /// [`Self::name_invitee_on_arrival`] writes it then.
    ///
    /// Keyed by [`crate::network::invite::PeerInviteToken::invite_id`]
    /// because a host invites people faster than they arrive. Keyed by
    /// room, a second invitation overwrote the first: invite Xaver,
    /// invite Claude, and whoever knocked first was called Claude while
    /// the other was left as `peer@<handle>` with nothing waiting for
    /// them. Invitations are independent, so the thing they are filed
    /// under has to be the invitation.
    ///
    /// The *name* is still not in the token: a token is a bearer
    /// credential that crosses machines, so a name carried inside it
    /// would be a name its *holder* presents, which is exactly the
    /// thing `apply_room_changes` refuses. Only the invitation's id
    /// travels, and it is a label the holder cannot profit from
    /// choosing — it selects which of this machine's own promises
    /// applies, and every one of them was written here.
    pending_invites: Mutex<std::collections::HashMap<String, PendingInvite>>,
    /// The invitation this machine redeemed to get into each room, by
    /// room.
    ///
    /// The other end of [`Self::pending_invites`]. A host can only
    /// match an arrival to an invitation if the arrival says which
    /// invitation it is, so every sync request this machine sends for a
    /// room it joined by invite quotes that invitation's id
    /// ([`Self::publish_outgoing`]). One entry per room because there
    /// is one way into a room: the invite that was accepted for it.
    redeemed_invites: Mutex<std::collections::HashMap<crate::shell::state::RoomId, String>>,
    /// Buffer of inbound presence updates pushed by [`Self::tick`] when
    /// a `NeighbourMsg::PresenceMoved` arrives. The shell's frame-tick
    /// hook drains this into `PresenceCtx::remote_cursors` once per
    /// wakeup so the overlay paints remote peers' cursors.
    inbound_presence: Arc<Mutex<Vec<InboundPresence>>>,
    /// Buffer of inbound app-CRDT increments pushed by [`Self::tick`]
    /// when a `NeighbourMsg::AppCrdtIncremental` arrives. The
    /// `collab_broker` drainer copies these into every locally-
    /// connected app via [`super::collab_broker::BrokerHandle::fanout_event`].
    /// See [`Self::inbound_app_crdt_handle`].
    inbound_app_crdt: Arc<Mutex<Vec<InboundAppCrdt>>>,
    /// Phase 3 buffer of inbound *signed* app-CRDT envelopes pushed
    /// by [`Self::tick`] when a
    /// `NeighbourMsg::AppCrdtIncrementalSigned` arrives. The
    /// `collab_broker` drainer pulls envelopes out of here, runs them
    /// through `validate_inbound_op`, and only then fans the payload
    /// to locally-connected apps. See
    /// [`Self::inbound_app_crdt_signed_handle`].
    inbound_app_crdt_signed: Arc<Mutex<Vec<InboundAppCrdtSigned>>>,
    /// Received app-presence frames awaiting fan-out to local apps.
    /// See [`Self::inbound_app_presence_handle`].
    inbound_app_presence: Arc<Mutex<Vec<InboundAppPresence>>>,
    /// Received app-CRDT *snapshots* awaiting fan-out to local apps.
    /// Separate from [`Self::inbound_app_crdt`] because the two are
    /// not interchangeable on delivery: an increment is applied to a
    /// document that already holds its history, a snapshot *is* that
    /// history, and the receiving app folds them in by different calls.
    /// See [`Self::inbound_app_crdt_snapshot_handle`].
    inbound_app_crdt_snapshot: Arc<Mutex<Vec<InboundAppCrdt>>>,
    /// Rooms with an outstanding request for somebody's whole
    /// document, keyed by cluster id.
    ///
    /// A room is entered here when a peer announces it has arrived, and
    /// removed when a snapshot is published for it. See
    /// [`Self::note_snapshot_request`] for why the pairing lives here
    /// rather than in the frame.
    snapshot_requests: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Carries document content between peers, and notices when a shared
    /// record references something that has not arrived, or is waiting on a
    /// change it was never sent.
    blob_relay: Arc<Mutex<crate::network::blob_relay::BlobRelay>>,
    /// Content that has arrived whole and verified, awaiting hand-off to the
    /// local apps that reference it. The shell holding a photograph is not the
    /// same as a photograph being on a page — the app that draws it is another
    /// process with its own store, and it keeps drawing a placeholder until
    /// the bytes are handed down. See [`Self::inbound_blob_handle`].
    inbound_blob: Arc<Mutex<Vec<InboundBlob>>>,
    /// Phase 6 buffer of inbound cooperative-scrollback *requests*.
    /// The broker's drainer reads these, consults its local op log,
    /// and (if it has any matching ops) publishes a corresponding
    /// `ScrollbackResponse` back over gossipsub. See
    /// [`Self::inbound_scrollback_request_handle`].
    inbound_scrollback_request: Arc<Mutex<Vec<InboundScrollbackRequest>>>,
    /// Phase 6 buffer of inbound cooperative-scrollback *responses*.
    /// The broker's drainer pulls answers out of here and forwards
    /// them to the local app that originally asked, as a
    /// `CollabEvent::ScrollbackBatch`. See
    /// [`Self::inbound_scrollback_response_handle`].
    inbound_scrollback_response: Arc<Mutex<Vec<InboundScrollbackResponse>>>,
}

/// Content that arrived whole and verified, waiting to be handed to the local
/// apps in `cluster_id` that reference it.
#[derive(Debug, Clone)]
pub struct InboundBlob {
    /// The room it belongs to; only apps in it are offered the bytes.
    pub cluster_id: String,
    /// blake3 hex of the content, which is also its name in every store.
    pub hash: String,
}

/// One received app-CRDT incremental update, queued by
/// [`ShellNetwork::tick`] for the `collab_broker` drainer to fan out
/// to every locally-connected app.
#[derive(Debug, Clone)]
pub struct InboundAppCrdt {
    /// Hashed libp2p peer id of the publisher. Useful for
    /// debugging / attribution; the broker's fan-out routes on the
    /// app + cluster pair below rather than on this.
    pub peer: u64,
    /// Originating app id (e.g. `"terminal"`), opaque to the bridge
    /// but the first half of the broker's delivery filter.
    pub app_id: String,
    /// Originating cluster id (uuid as string); second half of the
    /// broker's delivery filter.
    pub cluster_id: String,
    /// Originating record id — which document these changes belong
    /// to. Empty for a sender whose state is one document per
    /// `(app, cluster)`. Third component of the delivery filter.
    pub record_id: String,
    /// Opaque Automerge incremental change blob, passed through to
    /// the receiving app's `CollaborationManager` as a
    /// `CollabEvent::CRDTIncremental`.
    pub data: Vec<u8>,
}

/// One received app-presence frame, queued by [`ShellNetwork::tick`]
/// for the `collab_broker` drainer to fan out to local apps.
///
/// Carries no record id: presence is fanned at cluster scope, so the
/// room and the app are the whole delivery filter. See
/// [`crate::network::protocol::NeighbourMsg::AppPresence`].
#[derive(Debug, Clone)]
pub struct InboundAppPresence {
    /// Hashed libp2p peer id of the publisher. Unlike the CRDT queues
    /// this one is load-bearing rather than advisory: it becomes the
    /// `user_id` the receiving app attributes the caret to, and so the
    /// colour it is drawn in.
    pub peer: u64,
    /// Originating app id; first half of the delivery filter.
    pub app_id: String,
    /// Originating cluster id (the room); second half.
    pub cluster_id: String,
    /// The serialised `CollabRequest::Presence`, opaque here.
    pub frame: Vec<u8>,
}

/// One received *signed* app-CRDT envelope, queued by
/// [`ShellNetwork::tick`] for the `collab_broker` drainer to
/// validate-and-fan. Distinct from [`InboundAppCrdt`] so the broker
/// can keep the two streams separate — a malformed envelope should
/// never poison the legacy fan-out path, and vice versa.
#[derive(Debug, Clone)]
pub struct InboundAppCrdtSigned {
    /// Hashed libp2p peer id of the publisher.
    pub peer: u64,
    /// Originating app id, mirroring [`InboundAppCrdt::app_id`]. The
    /// broker delivers the validated payload only to local apps that
    /// announced the same `(app_id, cluster_id)` pair; empty (a peer
    /// too old to stamp one) addresses nobody.
    pub app_id: String,
    /// Originating cluster id; second half of the delivery filter.
    pub cluster_id: String,
    /// Originating record id; third component of the delivery filter.
    /// Empty for a sender that announced no record.
    pub record_id: String,
    /// The signed envelope. Wrapped in `Box` to mirror the wire shape
    /// and keep the queue entry size bounded — `SignedOp` is large
    /// (32-byte pubkey + 64-byte sig + variable payload + UUIDs).
    pub envelope: Box<collab_types::SignedOp>,
}

/// One received cooperative-scrollback request, queued by
/// [`ShellNetwork::tick`]. The broker's drainer reads these and may
/// publish a `ScrollbackResponse` if its local op log holds the
/// requested range.
#[derive(Debug, Clone)]
pub struct InboundScrollbackRequest {
    /// Hashed libp2p peer id of the asker; the broker's response
    /// uses this to mark its own response as a reply to that peer
    /// (i.e. fills `ScrollbackResponse::responder` with `self`, not
    /// `requester`).
    pub requester: u64,
    /// The artifact whose scrollback is requested.
    pub artifact: collab_types::ArtifactId,
    /// Exclusive upper bound on `seq`.
    pub before_seq: u64,
    /// Page cap.
    pub max: u16,
}

/// One received cooperative-scrollback response, queued by
/// [`ShellNetwork::tick`]. The broker's drainer translates these
/// into `CollabEvent::ScrollbackBatch` events and forwards them to
/// the local app that initiated the request.
#[derive(Debug, Clone)]
pub struct InboundScrollbackResponse {
    /// Hashed libp2p peer id of the answering broker.
    pub responder: u64,
    /// Artifact the response covers.
    pub artifact: collab_types::ArtifactId,
    /// The page of ops the answering peer is sharing.
    pub ops: Vec<Box<collab_types::SignedOp>>,
}

/// One received cursor-presence update, queued by [`ShellNetwork::tick`]
/// for the shell-side frame drainer to merge into `PresenceCtx`.
#[derive(Debug, Clone)]
pub struct InboundPresence {
    /// Hashed libp2p peer id of the publisher, mirrored from the
    /// originating `NeighbourMsg::PresenceMoved::peer` field. Mapped
    /// to a stable string id by the drainer before being stored on
    /// the `PresenceCtx::remote_cursors` row.
    pub peer: u64,
    /// Who the publishing session says is sitting at it. Empty when it
    /// did not say; the drainer then falls back to the session handle
    /// rather than inventing a name.
    pub user: String,
    /// Presence channel key (workspace, tab, widget path, …) — encoded
    /// JSON so the wire format stays opaque to the bridge. The shell's
    /// drainer decodes it back into a `PresenceKey` so a pointer lands
    /// on the room it was published under.
    pub key: String,
    /// Pointer position in the publisher's logical-pixel scene-space.
    pub point: (f64, f64),
    /// Wall-clock millisecond timestamp from the publisher; preserved
    /// so later eviction logic can age out stale rows.
    pub ts: u64,
}

impl ShellNetwork {
    /// Build a tokio runtime, spawn the libp2p [`Actor`], wrap the
    /// shared [`WorkspaceTree`] in a [`WorkspaceTreeReplica`], and
    /// return a handle the shell can use to drive sync.
    ///
    /// `keypair_path` defaults to [`DEFAULT_KEYPAIR_PATH`]; tests pass
    /// a tempdir path. The keypair is loaded if it already exists,
    /// otherwise a fresh ed25519 key is generated and persisted —
    /// matching [`load_or_create_keypair`]'s contract.
    pub fn spawn(
        tree: WorkspaceTreeHandle,
        keypair_path: &std::path::Path,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let _enter = runtime.enter();

        // Build the libp2p swarm. Mirrors the actor's existing
        // `build_swarm` test helper but uses the persistent keypair so
        // the shell's `PeerId` is stable across restarts.
        let keypair = load_or_create_keypair(keypair_path)?;
        let local_peer_id = keypair.public().to_peer_id();
        // Cache the hashed peer id once so per-pointer-move
        // `publish_presence` doesn't redo the hash every frame.
        // Mirrors the same hash function `tick`'s receive path uses,
        // so a peer's own publishes hash to the same `peer` u64 on
        // both sides of the wire.
        let local_peer_hash =
            crate::network::behaviour::peer_liveness_handle(&keypair.public().to_peer_id());
        let mut swarm = SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
            .with_tcp(
                Default::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .map_err(|e| std::io::Error::other(format!("tcp: {e:?}")))?
            .with_behaviour(|kp| NiriosBehaviour::new(kp).expect("behaviour construction"))
            .map_err(|e| std::io::Error::other(format!("behaviour wrap: {e:?}")))?
            .build();

        // Without this the swarm has no transport address: mdns
        // advertises nothing, an invite can carry no dialable address,
        // and no peer can ever reach this shell. Everything downstream
        // of it — the invite, the sync round, the capability that has to
        // reach the guest — depends on the socket actually being bound.
        for addr in LISTEN_ADDRS {
            let addr: libp2p::Multiaddr = addr.parse().map_err(std::io::Error::other)?;
            if let Err(e) = swarm.listen_on(addr.clone()) {
                tracing::warn!(%addr, "peer-sync: listen failed ({e})");
            }
        }

        let listen_addrs: Arc<Mutex<Vec<libp2p::Multiaddr>>> = Arc::new(Mutex::new(Vec::new()));
        let (cmd_tx, _join) = Actor::spawn_reporting_addrs(swarm, Arc::clone(&listen_addrs));

        // Replica owns the tree-side reactions to `NetEvent::Msg`.
        // Peer id `1` here is the replica-side identity used by
        // `WorkspaceTreeReplica::handle`'s `_sender` arg; the actor's
        // libp2p `PeerId` is not the same thing — it's transport-level
        // and gets hashed to `u64` per-event by `tick`.
        let replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree)));

        // Event channel: actor → replica delivery. Capacity matches
        // `Actor::spawn`'s own command-channel default so a burst of
        // `NetEvent`s during peer discovery doesn't immediately back-
        // pressure the actor's send loop.
        let (event_tx, event_rx) = mpsc::channel(64);

        Ok(Self {
            cmd_tx,
            replica,
            event_tx,
            event_rx: Mutex::new(event_rx),
            runtime: Some(runtime),
            local_peer_hash,
            keypair,
            local_peer_id,
            listen_addrs,
            last_sync_kick: Mutex::new(None),
            pending_invites: Mutex::new(std::collections::HashMap::new()),
            redeemed_invites: Mutex::new(std::collections::HashMap::new()),
            inbound_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt_signed: Arc::new(Mutex::new(Vec::new())),
            inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt_snapshot: Arc::new(Mutex::new(Vec::new())),
            snapshot_requests: Arc::new(Mutex::new(std::collections::HashSet::new())),
            blob_relay: Arc::new(Mutex::new(crate::network::blob_relay::BlobRelay::new())),
            inbound_blob: Arc::new(Mutex::new(Vec::new())),
            inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
            inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Pre-computed hash of the local libp2p `PeerId`. Stamped on every
    /// outbound presence move so peers can attribute the cursor to its
    /// publisher.
    pub fn local_peer_id_hash(&self) -> u64 {
        self.local_peer_hash
    }

    /// Workspace topic the next [`Self::publish_presence`] should
    /// publish to. v1 hard-codes [`DEFAULT_WS_TOPIC`] because the actor
    /// only joins the default workspace at startup; per-workspace
    /// routing arrives alongside the workspace-id resolver in a Phase
    /// 3 follow-up.
    /// What the peers in `cluster_id` are called, keyed by the peer
    /// handle presence frames carry.
    ///
    /// `cluster_id` is a room id, so this is the room document's answer
    /// and nobody else's: names are written there by the room's
    /// authority and refused from anyone else
    /// ([`crate::shell::state::WorkspaceTree::apply_room_changes`]).
    /// Empty for a cluster that is not a room this machine is in, and
    /// for a room in which nobody has been named -- both of which leave
    /// every peer labelled with its own handle.
    pub fn peer_labels(&self, cluster_id: &str) -> std::collections::BTreeMap<u64, String> {
        let empty = std::collections::BTreeMap::new();
        let Some(room) = crate::shell::state::RoomId::parse(cluster_id) else {
            return empty;
        };
        if room.is_nil() {
            return empty;
        }
        let Ok(replica) = self.replica.lock() else {
            return empty;
        };
        let labels = replica.tree.read().room_peer_labels(room);
        labels
    }

    pub fn active_ws_for_publish(&self) -> u64 {
        DEFAULT_WS_TOPIC
    }

    /// Shared handle to the inbound-presence buffer. The shell's
    /// per-frame drainer takes a clone, then on each frame swaps the
    /// `Vec` out under the lock and writes each entry into
    /// `PresenceCtx::remote_cursors`. Cloning the `Arc` is cheap; the
    /// underlying `Mutex` only contends with the `tick` path that
    /// pushes received messages.
    pub fn inbound_presence_handle(&self) -> Arc<Mutex<Vec<InboundPresence>>> {
        Arc::clone(&self.inbound_presence)
    }

    /// Forward a local-cursor presence move to the active workspace's
    /// peers. Throttling lives upstream in the toolkit
    /// (`pointer_broadcast_allowed`); this method just packages the
    /// event into a `NetCmd::Publish`. `try_send` failures (full
    /// command channel, actor dropped) are intentionally swallowed —
    /// presence is best-effort and a momentary backlog of cursor
    /// deltas should never block the render loop.
    pub fn publish_presence(&self, key: String, point: (f64, f64)) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let msg = NeighbourMsg::PresenceMoved {
            peer: self.local_peer_id_hash(),
            user: crate::shell::state::local_user_id(),
            key,
            point,
            ts,
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Adapter: queue a received presence move into the inbound buffer
    /// the shell's frame-tick drains into `PresenceCtx::remote_cursors`.
    /// Public so integration tests (and a future test-only handle in
    /// the loopback harness) can inject presence updates without going
    /// through gossipsub.
    pub fn handle_presence_message(
        &self,
        peer: u64,
        user: String,
        key: String,
        point: (f64, f64),
        ts: u64,
    ) {
        // A session does not paint its own cursor from the network: the
        // pointer it would draw is the one already under the user's hand.
        if peer == self.local_peer_hash {
            return;
        }
        if let Ok(mut buf) = self.inbound_presence.lock() {
            buf.push(InboundPresence {
                peer,
                user,
                key,
                point,
                ts,
            });
        }
    }

    /// Shared handle to the inbound-app-CRDT buffer. The
    /// `collab_broker` per-frame drainer takes a clone, swaps the
    /// `Vec` out under the lock once per wakeup, and fans each entry
    /// to every locally-connected app. Mirrors the
    /// [`Self::inbound_presence_handle`] pattern.
    pub fn inbound_app_crdt_handle(&self) -> Arc<Mutex<Vec<InboundAppCrdt>>> {
        Arc::clone(&self.inbound_app_crdt)
    }

    /// Publish a `NeighbourMsg::AppCrdtIncremental` to the default
    /// workspace topic. The broker calls this for every local
    /// `SyncIncremental` it relays so two laptops sharing the topic
    /// converge their app-level CRDTs. `try_send` failures are
    /// swallowed — CRDT updates are idempotent and the next
    /// `SyncIncremental` will republish, so a brief actor backlog
    /// is recoverable.
    pub fn publish_app_crdt_incremental(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        self.sync_content(&cluster_id, &record_id, &data);
        let msg = NeighbourMsg::AppCrdtIncremental {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            record_id,
            data,
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Offer the room any content this frame newly refers to, and **ask** the
    /// room for anything it refers to that this machine does not hold.
    ///
    /// # Why both directions
    ///
    /// This used to push only. The side that had just stored a blob offered it
    /// once per room per hash, and a side holding only the reference did
    /// nothing but log. That is correct exactly when the one broadcast lands,
    /// and permanently wrong every other time — a peer that joined afterwards,
    /// a mesh still forming when it went out, a dropped chunk. All three end
    /// the same way: a document naming a picture that will never arrive, and
    /// a placeholder that never resolves.
    ///
    /// It was also wrong in a way pushing alone can never fix. The bytes are
    /// held by the *app*, in its own sandbox, and the shell learns of them
    /// only when the app offers them ([`Self::offer_local_blob`]). Until then
    /// the shell holds the reference and not the content — indistinguishable,
    /// from here, from a peer that is missing it. So the sending shell was
    /// classifying its own document's photograph as "missing" and sending
    /// nothing at all.
    ///
    /// Asking is safe because the answer is content-addressed: a peer can only
    /// ever return the bytes the asker already named, and a wrong answer fails
    /// to verify and is discarded.
    fn sync_content(&self, cluster_id: &str, record_id: &str, frame: &[u8]) {
        if cluster_id.is_empty() {
            return;
        }
        let Ok(mut relay) = self.blob_relay.lock() else {
            return;
        };
        let obs = relay.observe_frame(cluster_id, record_id, frame);

        if obs.missing_ancestors > 0 {
            // The document has a hole: Automerge is queueing changes it cannot
            // apply, and the record reads as the page *before* the gap however
            // many later changes arrived. Saying so is the whole point — this
            // state used to be reachable only by reading the log afterwards.
            tracing::warn!(
                room = %cluster_id,
                record = %record_id,
                missing = obs.missing_ancestors,
                "shell_network: this document is waiting on {} change(s) it was never sent; \
                 it will read as the text before the gap until they arrive",
                obs.missing_ancestors,
            );
        }
        let ws = self.active_ws_for_publish();
        for hash in &obs.missing {
            if !relay.should_ask(cluster_id, hash) {
                continue;
            }
            tracing::info!(
                room = %cluster_id, record = %record_id, hash = %hash,
                "shell_network: this document references content this machine does not hold; \
                 asking the room for it. It renders as a placeholder until it arrives",
            );
            let msg = NeighbourMsg::BlobRequest {
                peer: self.local_peer_id_hash(),
                cluster_id: cluster_id.to_string(),
                hash: hash.clone(),
            };
            if self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_err() {
                tracing::warn!(
                    room = %cluster_id, hash = %hash,
                    "shell_network: could not send the request for this content; \
                     the image stays a placeholder until the next attempt",
                );
            }
        }
        for hash in &obs.have {
            if relay.already_announced(cluster_id, hash) {
                continue;
            }
            relay.mark_announced(cluster_id, hash);
            self.publish_blob(&mut relay, cluster_id, hash, "offered to the room");
        }
        self.report_unanswered(&mut relay);
    }

    /// Put every chunk of `hash` on the room's topic.
    ///
    /// Shared by the unsolicited offer and the answer to a request, so the two
    /// cannot drift in how they split, address or count the pieces.
    fn publish_blob(
        &self,
        relay: &mut crate::network::blob_relay::BlobRelay,
        cluster_id: &str,
        hash: &str,
        why: &str,
    ) {
        let Some(bytes) = relay.store().get(hash) else {
            // Only reachable if the store lost content between the check and
            // here. Worth a line: from a peer's side this looks like an offer
            // that never came.
            tracing::warn!(
                room = %cluster_id, hash = %hash,
                "shell_network: content vanished from the store before it could be sent",
            );
            return;
        };
        let Some(chunks) = crate::network::blob_relay::BlobRelay::split(&bytes) else {
            tracing::error!(
                room = %cluster_id, hash = %hash,
                bytes = bytes.len(),
                limit = crate::network::blob_relay::MAX_BLOB_BYTES,
                "shell_network: this content is too large to send; peers will show a \
                 placeholder for it and no retry will change that",
            );
            return;
        };
        let count = chunks.len() as u32;
        let ws = self.active_ws_for_publish();
        let mut sent = 0u32;
        for (index, data) in chunks.into_iter().enumerate() {
            let msg = NeighbourMsg::BlobChunk {
                peer: self.local_peer_id_hash(),
                cluster_id: cluster_id.to_string(),
                hash: hash.to_string(),
                index: index as u32,
                count,
                data,
            };
            if self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_ok() {
                sent += 1;
            }
        }
        if sent < count {
            // A partial send is a transfer that cannot complete. The receiver
            // will ask again, but this end has to say what it did.
            tracing::warn!(
                room = %cluster_id, hash = %hash, sent, count,
                "shell_network: only {sent} of {count} pieces of this content could be \
                 queued; peers will have to ask again",
            );
            return;
        }
        tracing::info!(
            room = %cluster_id, hash = %hash, bytes = bytes.len(), chunks = count,
            "shell_network: {why}",
        );
    }

    /// Report requests that have gone unanswered past every retry.
    ///
    /// The defect this whole area came from was a failure that said nothing.
    /// A picture that will not arrive is a fact the log has to carry before a
    /// person notices a grey box.
    fn report_unanswered(&self, relay: &mut crate::network::blob_relay::BlobRelay) {
        for (room, hash, waited) in relay.unanswered() {
            tracing::error!(
                room = %room, hash = %hash, waited_s = waited,
                "shell_network: asked the room for this content repeatedly over {waited}s and \
                 nobody answered; the image referencing it will stay a placeholder",
            );
        }
    }

    /// Answer a peer's request for content, when this machine holds it.
    ///
    /// Deliberately ignores the once-per-room `announced` mark: the whole
    /// reason a peer is asking is that it did not get the announcement, so
    /// treating "already offered" as "already delivered" is the bug.
    pub fn handle_blob_request_message(&self, peer: u64, cluster_id: String, hash: String) {
        let Ok(mut relay) = self.blob_relay.lock() else {
            return;
        };
        if !relay.holds(&hash) {
            tracing::debug!(
                from = peer, room = %cluster_id, hash = %hash,
                "shell_network: a peer asked for content this machine does not hold either",
            );
            return;
        }
        self.publish_blob(&mut relay, &cluster_id, &hash, "sent on request from a peer");
    }

    /// Take content an app on this machine has just stored, and offer it.
    ///
    /// This is the seam the transfer was missing. An app holds its blobs in
    /// its own sandbox — the launcher gives every app a private `HOME` and
    /// `XDG_CACHE_HOME`, so there is no directory the shell and the app can
    /// both read, and there must not be. The bytes therefore reach the shell
    /// the same way everything else an app shares does: over the collaboration
    /// socket. Without this the shell only ever saw the *reference* its app
    /// published, classified its own photograph as content it was missing, and
    /// sent nothing.
    ///
    /// Returns whether the content was stored, so the broker can say so.
    pub fn offer_local_blob(&self, cluster_id: &str, hash: &str, bytes: &[u8]) -> bool {
        let Ok(mut relay) = self.blob_relay.lock() else {
            return false;
        };
        if !relay.store_arrived(hash, bytes) {
            tracing::warn!(
                room = %cluster_id, hash = %hash, bytes = bytes.len(),
                "shell_network: refused content from a local app — it does not hash to the \
                 name it was offered under, so it is not that content",
            );
            return false;
        }
        if cluster_id.is_empty() {
            // Held, but not shared: an app with no room has no peers to offer
            // it to. Storing it still matters — it is what lets this shell
            // answer if the app later joins one.
            return true;
        }
        relay.mark_announced(cluster_id, hash);
        self.publish_blob(&mut relay, cluster_id, hash, "offered a local app's content to the room");
        true
    }

    /// Whether this machine holds `hash`, so the broker can answer an app
    /// without a round trip through the room.
    pub fn holds_blob(&self, hash: &str) -> Option<Vec<u8>> {
        let relay = self.blob_relay.lock().ok()?;
        relay.store().get(hash)
    }

    /// Queue arrived content for hand-off to local apps.
    pub fn note_blob_arrived(&self, cluster_id: &str, hash: &str) {
        if let Ok(mut buf) = self.inbound_blob.lock() {
            buf.push(InboundBlob {
                cluster_id: cluster_id.to_string(),
                hash: hash.to_string(),
            });
        }
    }

    /// Shared handle to the arrived-content buffer, drained by the broker and
    /// pushed to the apps that are waiting on a picture.
    pub fn inbound_blob_handle(&self) -> Arc<Mutex<Vec<InboundBlob>>> {
        Arc::clone(&self.inbound_blob)
    }

    /// Ask the room for content a local app needs and this machine lacks.
    pub fn want_blob(&self, cluster_id: &str, hash: &str) {
        if cluster_id.is_empty() {
            return;
        }
        let Ok(mut relay) = self.blob_relay.lock() else {
            return;
        };
        if !relay.should_ask(cluster_id, hash) {
            return;
        }
        let msg = NeighbourMsg::BlobRequest {
            peer: self.local_peer_id_hash(),
            cluster_id: cluster_id.to_string(),
            hash: hash.to_string(),
        };
        let ws = self.active_ws_for_publish();
        if self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_err() {
            tracing::warn!(
                room = %cluster_id, hash = %hash,
                "shell_network: could not ask the room for content an app is waiting on",
            );
        }
    }

    /// Take one received piece of content, storing the whole once it verifies.
    ///
    /// Public so an integration test can drive it without standing up
    /// gossipsub, mirroring the other `handle_*_message` adapters.
    ///
    /// Returns the hash when a whole, verified blob landed, so the caller can
    /// tell the local apps that were waiting on it.
    pub fn handle_blob_chunk_message(
        &self,
        peer: u64,
        hash: String,
        index: u32,
        count: u32,
        data: Vec<u8>,
    ) -> Option<String> {
        let Ok(mut relay) = self.blob_relay.lock() else {
            return None;
        };
        let whole = relay.accept_chunk(peer, &hash, index, count, data)?;
        let bytes = whole.len();
        if relay.store_arrived(&hash, &whole) {
            tracing::info!(
                from = peer, hash = %hash, bytes,
                "shell_network: content arrived and verified; the image it backs can render",
            );
            Some(hash)
        } else {
            tracing::warn!(
                from = peer, hash = %hash, bytes,
                "shell_network: content arrived but could not be stored; the image it backs \
                 stays a placeholder",
            );
            None
        }
    }

    /// Adapter: queue a received `AppCrdtIncremental` into the
    /// inbound buffer. Public so integration tests can inject
    /// messages without going through gossipsub.
    /// Is `cluster_id` a room this machine is in?
    ///
    /// An app addresses its traffic by `(app_id, cluster_id,
    /// record_id)` and the cluster is the room's uuid, so this is the
    /// same question `apply_room_changes` asks of a tree payload
    /// before it stores anything — and it gets the same answer here.
    /// Without it the only filter on an inbound app document was the
    /// self-echo test, which is a `DefaultHasher` fold: fine for
    /// suppressing your own publishes, and not something to hang an
    /// admission decision on (`plan/collaboration.md` §4).
    ///
    /// A `cluster_id` that is not a uuid is not a room, so it is not
    /// one of ours: refusing on a parse failure is the fail-closed
    /// direction.
    fn is_in_room(&self, cluster_id: &str) -> bool {
        let Some(room) = crate::shell::state::RoomId::parse(cluster_id) else {
            tracing::info!(
                target: "qos_collab_probe",
                cluster = %cluster_id,
                "PROBE is_in_room=false (not a uuid)",
            );
            return false;
        };
        if room.is_nil() {
            tracing::info!(
                target: "qos_collab_probe",
                cluster = %cluster_id,
                "PROBE is_in_room=false (nil)",
            );
            return false;
        }
        let Ok(replica) = self.replica.lock() else {
            return false;
        };
        let known = replica.tree.read().rooms();
        let in_room = known.contains(&room);
        drop(replica);
        tracing::info!(
            target: "qos_collab_probe",
            cluster = %cluster_id,
            in_room,
            known = ?known.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
            "PROBE is_in_room",
        );
        in_room
    }

    pub fn handle_app_crdt_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        if let Ok(mut buf) = self.inbound_app_crdt.lock() {
            buf.push(InboundAppCrdt {
                peer,
                app_id,
                cluster_id,
                record_id,
                data,
            });
        }
    }

    /// Shared handle to the inbound-signed-app-CRDT buffer.
    /// Mirror of [`Self::inbound_app_crdt_handle`] for the Phase 3
    /// signed-envelope path. The broker drainer pulls envelopes off
    /// this buffer, validates them, and fans the payload out as
    /// `CollabEvent::SignedCRDTIncremental`.
    pub fn inbound_app_crdt_signed_handle(&self) -> Arc<Mutex<Vec<InboundAppCrdtSigned>>> {
        Arc::clone(&self.inbound_app_crdt_signed)
    }

    /// Publish a `NeighbourMsg::AppCrdtIncrementalSigned` to the
    /// default workspace topic. Called by the broker for every
    /// validated `CollabRequest::SignedSyncIncremental` so two
    /// laptops sharing the topic exchange signed CRDT envelopes.
    /// `try_send` failures are intentionally swallowed; the next
    /// successful send will republish.
    pub fn publish_app_crdt_signed(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: collab_types::SignedOp,
    ) {
        let msg = NeighbourMsg::AppCrdtIncrementalSigned {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            record_id,
            envelope: Box::new(envelope),
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Adapter: queue a received `AppCrdtIncrementalSigned` into the
    /// inbound buffer. Public so integration tests can inject
    /// messages without going through gossipsub. Mirrors
    /// [`Self::handle_app_crdt_message`] for the signed path.
    pub fn handle_app_crdt_signed_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        envelope: collab_types::SignedOp,
    ) {
        if let Ok(mut buf) = self.inbound_app_crdt_signed.lock() {
            buf.push(InboundAppCrdtSigned {
                peer,
                app_id,
                cluster_id,
                record_id,
                envelope: Box::new(envelope),
            });
        }
    }

    /// Publish an app's presence frame to the peers in its room.
    ///
    /// The counterpart to [`Self::publish_app_crdt_incremental`], and
    /// the reason a peer's caret is visible on another machine at all:
    /// the broker's local fan-out reaches apps on *this* host, and
    /// without this the frame stopped there.
    ///
    /// `frame` is the serialised `CollabRequest::Presence`, passed
    /// through untouched. Presence is best-effort — a dropped caret
    /// update is corrected by the next one — so a full command channel
    /// is swallowed rather than surfaced, exactly as the pointer path
    /// upstream does.
    pub fn publish_app_presence(&self, app_id: String, cluster_id: String, frame: Vec<u8>) {
        let msg = NeighbourMsg::AppPresence {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            frame,
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Shared handle to the inbound app-presence buffer, drained by the
    /// broker and fanned to local apps as `RemotePresenceUpdate`.
    pub fn inbound_app_presence_handle(&self) -> Arc<Mutex<Vec<InboundAppPresence>>> {
        Arc::clone(&self.inbound_app_presence)
    }

    /// Adapter: queue a received `AppPresence`. Public so integration
    /// tests can inject one without standing up gossipsub. Mirrors
    /// [`Self::handle_app_crdt_message`].
    pub fn handle_app_presence_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        frame: Vec<u8>,
    ) {
        if let Ok(mut buf) = self.inbound_app_presence.lock() {
            buf.push(InboundAppPresence {
                peer,
                app_id,
                cluster_id,
                frame,
            });
        }
    }

    /// Publish a `NeighbourMsg::AppCrdtSnapshot` — one document's whole
    /// state — to the workspace topic, so a peer that has just joined
    /// can adopt a document it holds no history for.
    ///
    /// Refuses above [`MAX_APP_SNAPSHOT_BYTES`] instead of handing
    /// gossipsub a message it would reject. Both transports (the JSON
    /// reader loop and the capnp bridge) reach the wire through this
    /// one function, so the cap is enforced once rather than at each
    /// caller.
    ///
    /// A refusal is logged at `warn` with the size and the room,
    /// because the user-visible consequence — this document does not
    /// converge — has no other symptom. Silence here would look
    /// exactly like a document that simply had nothing to send.
    pub fn publish_app_crdt_snapshot(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        if !self.take_snapshot_request(&cluster_id) {
            tracing::trace!(
                app = %app_id,
                room = %cluster_id,
                "shell_network: nobody is waiting for a whole document; the delta carries this edit",
            );
            return;
        }
        if data.len() > MAX_APP_SNAPSHOT_BYTES {
            tracing::warn!(
                bytes = data.len(),
                cap = MAX_APP_SNAPSHOT_BYTES,
                app = %app_id,
                room = %cluster_id,
                record = %record_id,
                "shell_network: refusing to publish an app snapshot over the gossip cap; this document will not converge for peers that lack its history",
            );
            return;
        }
        // Same reason as the receive side: a whole document is the frame that
        // establishes the record, and the relay's mirror needs it or every
        // later increment lands on a hole and the document's content becomes
        // invisible to this code.
        self.sync_content(&cluster_id, &record_id, &data);
        let msg = NeighbourMsg::AppCrdtSnapshot {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            record_id,
            data,
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Record that somebody in `cluster_id` needs a whole document.
    ///
    /// Called when a peer announces its arrival. Arrival is the only
    /// thing that makes a snapshot worth its size on a broadcast topic:
    /// a peer that already shares history is served by the deltas, and
    /// a peer that does not cannot use them at all.
    ///
    /// # Why the pairing is reconstructed here
    ///
    /// `CollabRequest::SyncCRDT` is not reliably a join reply. It is one
    /// when the toolkit answers an `Arrived`, and it is also what an app
    /// sends to feed same-host fan-out and the undo snapshot — whether a
    /// commit produces one is a property of the toolkit build, not
    /// something visible in the frame. Republishing whatever arrives
    /// therefore risks a whole document on the room topic per commit:
    /// unbounded, growing with the session, and redundant with the delta
    /// that already carries the same edit to anyone who shares the
    /// history.
    ///
    /// The broker cannot tell those apart by looking, but it does see
    /// the *request* — a peer's `Arrived` passes through its own drain on
    /// the way to local apps. So the arrival arms the room and the next
    /// whole document for it goes out. Which one that turns out to be
    /// does not matter: any full state serves a peer that has none, so
    /// consuming the arming on a commit's snapshot answers the join just
    /// as well as replying to it directly.
    ///
    /// Gating here rather than trusting the sender is what keeps the
    /// rate bounded no matter which way the toolkit sends.
    pub fn note_snapshot_request(&self, cluster_id: &str) {
        if cluster_id.is_empty() {
            return;
        }
        if let Ok(mut pending) = self.snapshot_requests.lock() {
            pending.insert(cluster_id.to_string());
        }
    }

    /// Take the outstanding snapshot request for `cluster_id`, if any.
    fn take_snapshot_request(&self, cluster_id: &str) -> bool {
        match self.snapshot_requests.lock() {
            Ok(mut pending) => pending.remove(cluster_id),
            Err(_) => false,
        }
    }

    /// Shared handle to the inbound app-snapshot buffer, drained by the
    /// broker and fanned to local apps as `CollabEvent::CRDTUpdate`.
    pub fn inbound_app_crdt_snapshot_handle(&self) -> Arc<Mutex<Vec<InboundAppCrdt>>> {
        Arc::clone(&self.inbound_app_crdt_snapshot)
    }

    /// Adapter: queue a received `AppCrdtSnapshot`. Public so tests can
    /// inject one without standing up gossipsub. Mirrors
    /// [`Self::handle_app_crdt_message`].
    pub fn handle_app_crdt_snapshot_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        if let Ok(mut buf) = self.inbound_app_crdt_snapshot.lock() {
            buf.push(InboundAppCrdt {
                peer,
                app_id,
                cluster_id,
                record_id,
                data,
            });
        }
    }

    /// Shared handle to the cooperative-scrollback request buffer.
    /// The broker's drainer reads it on each tick and answers any
    /// request for an artifact whose log it holds.
    pub fn inbound_scrollback_request_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackRequest>>> {
        Arc::clone(&self.inbound_scrollback_request)
    }

    /// Shared handle to the cooperative-scrollback response buffer.
    pub fn inbound_scrollback_response_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackResponse>>> {
        Arc::clone(&self.inbound_scrollback_response)
    }

    /// Phase 6: publish a `NeighbourMsg::ScrollbackRequest` to the
    /// default workspace topic. The broker calls this when a local
    /// app's `FetchScrollback` returned `exhausted=true` with an
    /// empty batch (the broker has nothing more in-memory, but the
    /// app still wants the range). `try_send` failures are
    /// swallowed; the app can re-issue the request after a back-off.
    pub fn publish_scrollback_request(
        &self,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        let msg = NeighbourMsg::ScrollbackRequest {
            artifact,
            before_seq,
            max,
            requester: self.local_peer_id_hash(),
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Phase 6: publish a `NeighbourMsg::ScrollbackResponse`. The
    /// broker calls this when it sees a `ScrollbackRequest` for an
    /// artifact whose log it has matching ops for.
    pub fn publish_scrollback_response(
        &self,
        artifact: collab_types::ArtifactId,
        ops: Vec<Box<collab_types::SignedOp>>,
    ) {
        let msg = NeighbourMsg::ScrollbackResponse {
            artifact,
            ops,
            responder: self.local_peer_id_hash(),
        };
        let _ = self.cmd_tx.try_send(NetCmd::Publish {
            ws: self.active_ws_for_publish(),
            msg,
        });
    }

    /// Adapter: queue a received `ScrollbackRequest` for the broker
    /// drainer. Public for tests; in production the `tick` path
    /// calls this when it sees the inbound `NeighbourMsg`.
    pub fn handle_scrollback_request_message(
        &self,
        requester: u64,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        if let Ok(mut buf) = self.inbound_scrollback_request.lock() {
            buf.push(InboundScrollbackRequest {
                requester,
                artifact,
                before_seq,
                max,
            });
        }
    }

    /// Adapter: queue a received `ScrollbackResponse` for the
    /// broker drainer.
    pub fn handle_scrollback_response_message(
        &self,
        responder: u64,
        artifact: collab_types::ArtifactId,
        ops: Vec<Box<collab_types::SignedOp>>,
    ) {
        if let Ok(mut buf) = self.inbound_scrollback_response.lock() {
            buf.push(InboundScrollbackResponse {
                responder,
                artifact,
                ops,
            });
        }
    }

    /// Subscribe the actor to the default workspace topic, registering
    /// our `event_tx` as the per-workspace event sink. v1 uses a
    /// single topic ([`DEFAULT_WS_TOPIC`]); per-workspace routing
    /// lands in a Phase 3 follow-up. Returns whether the command was
    /// successfully queued.
    pub fn join_default_workspace(&self) -> bool {
        self.cmd_tx
            .try_send(NetCmd::JoinWorkspace {
                ws: DEFAULT_WS_TOPIC,
                on_event: self.event_tx.clone(),
            })
            .is_ok()
    }

    /// The dial address this shell publishes in an invite.
    ///
    /// The first address the swarm bound, with this peer's id appended
    /// so the receiver dials a specific peer rather than whatever
    /// answers on that port. `None` until the swarm has reported a
    /// listen address — an invite issued before then would name nothing.
    pub fn dial_addr(&self) -> Option<String> {
        let addrs = self.listen_addrs.lock().ok()?;
        let base = addrs.first()?.clone();
        Some(format!("{base}/p2p/{}", self.local_peer_id))
    }

    /// Mint a signed invite to the workspace `local`, valid for
    /// `expiry_seconds_from_now`.
    ///
    /// The half of the invite flow that had no implementation: the
    /// palette's `/invite` row named this function in a doc comment and
    /// nothing defined it, so there was no way to produce a token
    /// outside a unit test. Signs with this shell's own libp2p keypair
    /// and embeds the address the swarm actually bound, so the token the
    /// user copies is one a peer can act on.
    pub fn issue_invite(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
    ) -> Result<String, String> {
        self.issue_invite_for(local, expiry_seconds_from_now, None)
    }

    /// [`Self::issue_invite`], for a person this machine will name.
    ///
    /// `invitee` is what the room calls whoever redeems this token. It
    /// does not go into the token — see [`Self::pending_invitees`] for
    /// why — it is held until that peer turns up and then written into
    /// the room document by the room's own authority.
    ///
    /// Minting a second invite leaves the first one alone. Each token
    /// carries its own invitation id, the name is parked against that,
    /// and the redeemer quotes it back — so two people invited before
    /// either has arrived are two independent promises, and they can be
    /// redeemed in either order.
    pub fn issue_invite_named(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: &str,
    ) -> Result<String, String> {
        self.issue_invite_for(local, expiry_seconds_from_now, Some(invitee))
    }

    fn issue_invite_for(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
        invitee: Option<&str>,
    ) -> Result<String, String> {
        let addr = self
            .dial_addr()
            .ok_or_else(|| "peer-sync is not listening yet; try again in a moment".to_string())?;
        let (token, invite_id, room) = {
            let replica = self
                .replica
                .lock()
                .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
            let tree = replica.tree.read();
            let ws = tree
                .workspaces
                .iter()
                .find(|w| w.id == local)
                .ok_or_else(|| format!("no workspace {} to invite into", local.raw()))?;
            let (token, invite_id) = crate::network::invite::make_invite(
                &self.keypair,
                ws,
                &addr,
                expiry_seconds_from_now,
            )
            .map_err(|e| e.to_string())?;
            (token, invite_id, ws.room)
        };

        // Nobody invites the inviter.
        //
        // Names reach a guest's screen from the room document, written
        // by the room's authority and refused from anyone else. Every
        // peer in the room is named by whoever invited them — except
        // the person who did the inviting, who therefore showed up on
        // their guests' screens as an id while everybody else had a
        // name.
        //
        // This closes that without inventing anything. The name is the
        // identity this machine is configured with, and it is written
        // by the room's own authority onto the same replicated,
        // authority-checked channel every other name travels on — so a
        // guest checks it exactly the way it checks the others, and
        // cannot forge it. Self-declared, but declared by the one
        // party a guest already has to trust about this room.
        //
        // Best effort: a machine that does not administer the room has
        // its write refused, and the host stays an id rather than the
        // invite failing over a label.
        if let Ok(replica) = self.replica.lock() {
            let me = crate::shell::state::local_user_id();
            let peer = self.local_peer_id.to_base58();
            replica.tree.write().name_peer(room, &me, &peer, &me);
        }

        // And park the invitee's name against *this invitation*, for
        // `name_invitee_on_arrival` to write when they turn up. Against
        // the invitation rather than the room, so inviting a second
        // person does not overwrite the first person's name.
        if let Some(invitee) = invitee.map(str::trim).filter(|n| !n.is_empty())
            && let Ok(mut pending) = self.pending_invites.lock()
        {
            prune_expired(&mut pending);
            pending.insert(
                invite_id,
                PendingInvite {
                    room,
                    name: invitee.to_string(),
                    expires_unix_seconds: now_unix_seconds()
                        .saturating_add(expiry_seconds_from_now),
                },
            );
        }
        Ok(token)
    }

    /// Name the peer that just redeemed this room's invite.
    ///
    /// Called with the *authenticated* author of a message asking about
    /// `room` — gossipsub runs `MessageAuthenticity::Signed` with
    /// `ValidationMode::Strict`, so `peer` is the identity that signed
    /// what arrived, not whoever forwarded it. Asking this room for its
    /// document is what redeeming an invite looks like from the host's
    /// side: it is the same signal the replica records a participant
    /// on.
    ///
    /// `invite` is the invitation the arriving peer says it is
    /// redeeming — [`crate::network::invite::PeerInviteToken::invite_id`],
    /// quoted back on the request. It is what picks out *which* of this
    /// host's outstanding promises applies, and without it a host with
    /// two invitations out could only guess: the room they share cannot
    /// tell them apart, which is how the second person to arrive ended
    /// up unnamed.
    ///
    /// Four guards, each closing a way this could name the wrong
    /// person: never this machine, only an invitation this machine
    /// actually minted *for the room being asked about*, never a peer
    /// the room has already named, and only if `name_peer` accepts the
    /// write — which it does only for a room this machine administers.
    /// The pending name is consumed by the peer that redeems that
    /// invitation, and every other invitation is left standing for
    /// whoever it was for.
    fn name_invitee_on_arrival(&self, peer: &libp2p::PeerId, room: &str, invite: Option<&str>) {
        if *peer == self.local_peer_id {
            return;
        }
        let Some(room) = crate::shell::state::RoomId::parse(room) else {
            return;
        };
        // No invitation quoted is not "any invitation will do". A peer
        // that found this room some other way has redeemed nothing, and
        // handing it a name meant for somebody who is still on their
        // way is how that person loses theirs.
        let Some(invite) = invite else {
            return;
        };
        let Ok(mut pending) = self.pending_invites.lock() else {
            return;
        };
        prune_expired(&mut pending);
        // The invitation has to be one of this room's. Quoting an id
        // minted for a different room is not a redemption of this one.
        let Some(name) = pending
            .get(invite)
            .filter(|held| held.room == room)
            .map(|held| held.name.clone())
        else {
            return;
        };
        let base58 = peer.to_base58();
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        if let Some(existing) = tree.room_peer_names.get(&room).and_then(|n| n.get(&base58)) {
            // Already named, so nothing to do — but if what they are
            // already called is the name that was waiting, this is the
            // peer the invite was for, named at mint time because that
            // route knew their id (`peer.invite.named` does). Spend the
            // pending copy rather than leave it lying around.
            if existing == &name {
                pending.remove(invite);
            }
            return;
        }
        let me = crate::shell::state::local_user_id();
        if tree.name_peer(room, &me, &base58, &name) {
            tracing::info!(
                peer = %base58,
                %name,
                %room,
                invite = %invite,
                "peer-sync: named the invitee that arrived"
            );
            pending.remove(invite);
        }
    }

    /// Publish messages the replica produced, stamping every sync
    /// request with the invitation that got this machine into the room
    /// it names.
    ///
    /// The replica builds the requests and knows nothing about invites;
    /// this is the layer that holds both, so this is where the two are
    /// put together. Every round carries it, not just the first: a
    /// single announcement at join time would be one gossip publish
    /// against a mesh that may not have formed yet, and losing it would
    /// leave the guest unnamed for as long as they stayed.
    fn publish_outgoing(&self, outgoing: Vec<NeighbourMsg>) {
        let redeemed = self
            .redeemed_invites
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        for mut msg in outgoing {
            if let NeighbourMsg::SyncReq {
                ref room,
                ref mut invite,
                ..
            } = msg
                && invite.is_none()
                && let Some(room) = crate::shell::state::RoomId::parse(room)
                && let Some(id) = redeemed.get(&room)
            {
                *invite = Some(id.clone());
            }
            let _ = self.cmd_tx.try_send(NetCmd::Publish {
                ws: DEFAULT_WS_TOPIC,
                msg,
            });
        }
    }

    /// Accept an invite token into `local`, then immediately ask the
    /// room it named to catch us up.
    ///
    /// The two halves belong together. [`accept_invite_into`] is what
    /// makes the invitee a member — it points their workspace at the
    /// inviter's room, mirrors that to the document and flushes it to
    /// disk. But membership is also what decides which rooms this machine
    /// asks about, and replication is per room: without the request that
    /// follows, a freshly accepted invite would sit idle until something
    /// else happened to kick a round, and the guest would be in a room
    /// they could not see.
    ///
    /// Takes the token *string* rather than a parsed token, so there is
    /// no way to reach this having skipped validation. Returns the room
    /// joined, or the reason the token was refused.
    pub fn accept_invite(
        &self,
        local: crate::shell::state::WorkspaceId,
        token: &str,
    ) -> Result<crate::shell::state::RoomId, String> {
        let replica = self
            .replica
            .lock()
            .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
        let room = {
            let mut tree = replica.tree.write();
            accept_invite_into(&mut tree, local, token)?
        };
        // Dial what the token named. mdns finds peers on a LAN, but an
        // invite that carries an address and never uses it leaves the
        // guest waiting on discovery for a host it was handed directions
        // to. `validate_invite_token` has already run inside
        // `accept_invite_into`, so re-reading the token here cannot let
        // an unverified address through.
        if let Ok(parsed) = crate::network::invite::validate_invite_token(token) {
            if let Ok(addr) = parsed.owner_addr.parse::<libp2p::Multiaddr>() {
                let _ = self.cmd_tx.try_send(NetCmd::Connect { addr });
            }
            // Which invitation this was, recorded before the first
            // request goes out — so the very first thing the host hears
            // from this machine already says which of its outstanding
            // promises just came due. The host has two people invited
            // and one room; without this it cannot tell which arrived.
            if let Ok(mut redeemed) = self.redeemed_invites.lock() {
                redeemed.insert(room, parsed.invite_id);
            }
        }
        replica.request_sync();
        let outgoing = replica.drain_outgoing();
        drop(replica);
        self.publish_outgoing(outgoing);
        Ok(room)
    }

    /// Ask every room this machine is in for whatever it is missing,
    /// and publish the requests.
    pub fn publish_sync_requests(&self) {
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        replica.request_sync();
        let outgoing = replica.drain_outgoing();
        drop(replica);
        self.publish_outgoing(outgoing);
    }

    /// [`Self::publish_sync_requests`], rate-limited to
    /// [`SYNC_KICK_INTERVAL`]. Called from every frame tick, which runs
    /// far more often than a room changes.
    fn kick_periodic_sync(&self) {
        let Ok(mut last) = self.last_sync_kick.lock() else {
            return;
        };
        let now = std::time::Instant::now();
        if let Some(prev) = *last
            && now.duration_since(prev) < SYNC_KICK_INTERVAL
        {
            return;
        }
        *last = Some(now);
        drop(last);
        self.publish_sync_requests();
    }

    /// Drive a single tick: drain pending [`NetEvent`]s from the actor
    /// and forward `Msg` variants into the replica's `handle`. Each
    /// resulting outbound message is published via `cmd_tx` so peers
    /// observe the reply.
    ///
    /// Called from the shell's per-frame update path (see
    /// `compositor::hosted::run` when `--enable-peer-sync` is set), or
    /// directly from tests. Non-blocking — uses `try_recv` /
    /// `try_send` so a starved channel never stalls the render loop.
    pub fn tick(&self) {
        // Room replication is pull-only: a host answers `SyncReq` and
        // never announces. So without something asking on a cadence, a
        // guest sees the room exactly as it was the moment they joined —
        // a capability the owner grants or revokes afterwards never
        // arrives, which is the difference between a snapshot and
        // collaboration. Ask periodically; a round with nothing to send
        // produces no reply at all.
        self.kick_periodic_sync();
        // Hold the receiver lock for the whole drain so a concurrent
        // frame-tick (e.g. one queued from a redraw inside the very
        // same wakeup) doesn't see half-merged events. The lock has
        // no contention in practice — only the frame-tick path holds
        // it — but keeping the scope tight is cheap and safe.
        let mut event_rx = self
            .event_rx
            .lock()
            .expect("event_rx mutex must not be poisoned");
        while let Ok(event) = event_rx.try_recv() {
            // A peer arriving is the one moment we know for certain
            // there is someone new to be caught up by. Documented in
            // `plan/collaboration.md` §5 and previously not done —
            // `PeerJoined` fell through the `if let` below unhandled.
            if let NetEvent::PeerJoined { .. } = event {
                self.publish_sync_requests();
                continue;
            }
            // A session going quiet is the one departure signal the
            // transport produces on its own, and the roster has to act
            // on it. Without this the avatar stack only ever grows: it
            // would keep announcing everyone who had ever been in the
            // room for the rest of the session, which is the same class
            // of untruth as seeding it with people who never arrived.
            if let NetEvent::PeerLeft { peer, .. } = event {
                let handle = crate::network::behaviour::peer_liveness_handle(&peer);
                if let Ok(replica) = self.replica.lock() {
                    replica.tree.write().retire_session(handle);
                }
                continue;
            }
            if let NetEvent::Msg { ws: _, peer, msg } = event {
                // Presence updates skip the replica entirely — they're
                // ephemeral overlay state, not CRDT state — and land
                // in the inbound buffer the shell's frame-tick drains.
                if let NeighbourMsg::PresenceMoved {
                    peer: peer_hash,
                    ref user,
                    ref key,
                    point,
                    ts,
                } = msg
                {
                    self.handle_presence_message(
                        peer_hash,
                        user.clone(),
                        key.clone(),
                        point,
                        ts,
                    );
                    continue;
                }
                // App-CRDT updates skip the shell-tree replica too —
                // they're opaque app-level Automerge blobs, not
                // shell-tree changes — and land in the broker's
                // inbound queue for local fan-out. Self-publishes are
                // filtered out so a local edit doesn't loop back to
                // its own broker as a "remote" event.
                if let NeighbourMsg::AppCrdtIncremental {
                    peer: peer_hash,
                    ref app_id,
                    ref cluster_id,
                    ref record_id,
                    ref data,
                } = msg
                {
                    tracing::info!(
                        target: "qos_collab_probe",
                        from = peer_hash,
                        me = self.local_peer_hash,
                        app = %app_id,
                        cluster = %cluster_id,
                        record = %record_id,
                        bytes = data.len(),
                        "PROBE gossip recv AppCrdtIncremental",
                    );
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
                        // Fold it into this shell's view of the record before
                        // handing it on: that is what tells us whether the
                        // document now references content we lack, and whether
                        // its history has a hole.
                        self.sync_content(cluster_id, record_id, data);
                        self.handle_app_crdt_message(
                            peer_hash,
                            app_id.clone(),
                            cluster_id.clone(),
                            record_id.clone(),
                            data.clone(),
                        );
                    }
                    continue;
                }
                // A piece of a document's content, travelling separately from
                // the document precisely so neither has to fit inside the
                // other. Verified against its hash before it is stored.
                if let NeighbourMsg::BlobChunk {
                    peer: peer_hash,
                    ref cluster_id,
                    ref hash,
                    index,
                    count,
                    ref data,
                } = msg
                {
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
                        if let Some(landed) = self.handle_blob_chunk_message(
                            peer_hash,
                            hash.clone(),
                            index,
                            count,
                            data.clone(),
                        ) {
                            // Tell the apps waiting on it. A shell holding the
                            // bytes is not a picture on a page: the app that
                            // renders it is a different process with its own
                            // store, and until it is handed the content it
                            // goes on drawing a placeholder.
                            self.note_blob_arrived(cluster_id, &landed);
                        }
                    }
                    continue;
                }
                // A peer asking for content it holds a reference to. Answered
                // only if this machine has the bytes and shares the room.
                if let NeighbourMsg::BlobRequest {
                    peer: peer_hash,
                    ref cluster_id,
                    ref hash,
                } = msg
                {
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
                        self.handle_blob_request_message(
                            peer_hash,
                            cluster_id.clone(),
                            hash.clone(),
                        );
                    }
                    continue;
                }
                // A whole document, sent because a peer joined and
                // has no history to apply deltas against. Same two
                // gates the increment gets, and they matter more here
                // rather than less: an increment landing in the wrong
                // document is noise the widgets ignore, a whole
                // document merged into the wrong one cannot be taken
                // back out.
                if let NeighbourMsg::AppCrdtSnapshot {
                    peer: peer_hash,
                    ref app_id,
                    ref cluster_id,
                    ref record_id,
                    ref data,
                } = msg
                {
                    tracing::info!(
                        target: "qos_collab_probe",
                        from = peer_hash,
                        me = self.local_peer_hash,
                        app = %app_id,
                        cluster = %cluster_id,
                        record = %record_id,
                        bytes = data.len(),
                        "PROBE gossip recv AppCrdtSnapshot",
                    );
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
                        // A whole document has to be folded into the mirror as
                        // much as an increment does — more so. It is the frame
                        // that *creates* the record for a peer with no history,
                        // so a mirror that never sees it is missing the change
                        // every later one is built on: `qdoc::model::open`
                        // fails on it, and the relay reports no content at all
                        // for a document that plainly references some. That is
                        // why neither end of a real session ever logged a word
                        // about the photograph it was failing to move.
                        self.sync_content(cluster_id, record_id, data);
                        self.handle_app_crdt_snapshot_message(
                            peer_hash,
                            app_id.clone(),
                            cluster_id.clone(),
                            record_id.clone(),
                            data.clone(),
                        );
                    }
                    continue;
                }
                // Phase 3 signed path. Mirrors the legacy
                // `AppCrdtIncremental` routing: skip self-publishes,
                // then push onto the signed-inbound queue for the
                // broker drainer to validate + fan.
                if let NeighbourMsg::AppCrdtIncrementalSigned {
                    peer: peer_hash,
                    ref app_id,
                    ref cluster_id,
                    ref record_id,
                    ref envelope,
                } = msg
                {
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
                        self.handle_app_crdt_signed_message(
                            peer_hash,
                            app_id.clone(),
                            cluster_id.clone(),
                            record_id.clone(),
                            (**envelope).clone(),
                        );
                    }
                    continue;
                }
                // App presence — a peer's caret, selection or authorship
                // inside a shared document. Same two gates the CRDT
                // variants get, and for the same reasons: our own
                // gossipsub echo is not a peer, and a room we are not in
                // does not get to put a cursor on our screen.
                if let NeighbourMsg::AppPresence {
                    peer: peer_hash,
                    ref app_id,
                    ref cluster_id,
                    ref frame,
                } = msg
                {
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
                        self.handle_app_presence_message(
                            peer_hash,
                            app_id.clone(),
                            cluster_id.clone(),
                            frame.clone(),
                        );
                    }
                    continue;
                }
                // Phase 6 cooperative scrollback. Skip self-echoes
                // (we already have the answer) and route requests +
                // responses to their dedicated buffers.
                if let NeighbourMsg::ScrollbackRequest {
                    artifact,
                    before_seq,
                    max,
                    requester,
                } = msg
                {
                    if requester != self.local_peer_hash {
                        self.handle_scrollback_request_message(
                            requester, artifact, before_seq, max,
                        );
                    }
                    continue;
                }
                if let NeighbourMsg::ScrollbackResponse {
                    artifact,
                    ref ops,
                    responder,
                } = msg
                {
                    if responder != self.local_peer_hash {
                        self.handle_scrollback_response_message(responder, artifact, ops.clone());
                    }
                    continue;
                }
                // A peer asking this machine for a room's document is
                // a peer redeeming an invite to it. If one was minted
                // for somebody by name, this is the moment that name
                // can be attached to an identity — before the replica
                // answers, so the roster and the answer land together.
                if let NeighbourMsg::SyncReq {
                    ref room,
                    ref invite,
                    ..
                } = msg
                {
                    self.name_invitee_on_arrival(&peer, room, invite.as_deref());
                }
                let mut rep = self.replica.lock().unwrap();
                // The peer's own identity, not the liveness handle's
                // digest of it: room replication decides from this
                // whether a payload may rewrite a room's governance.
                rep.handle_from(&crate::shell::state::PeerOrigin::peer(&peer), &msg);
                let outgoing = rep.drain_outgoing();
                drop(rep);
                self.publish_outgoing(outgoing);
            }
        }
    }
}

impl Drop for ShellNetwork {
    fn drop(&mut self) {
        // Drop the runtime explicitly so the actor's tokio task gets
        // a chance to wind down before the surrounding test (or the
        // process) exits. Using `take` + drop avoids the implicit
        // "drop on panic" path that would otherwise log a noisy
        // "runtime dropped while still running tasks" warning.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}



/// Test-only constructor that wires `ShellNetwork`'s command sender to
/// a caller-owned receiver, bypassing the libp2p actor entirely. Tests
/// use this to assert that `publish_presence` (and future Phase 3
/// publishers) emit the expected `NetCmd::Publish` shapes without
/// having to spin up a real swarm. Not exposed outside `cfg(test)`.
///
/// `pub(crate)` so the broker's own tests can drive the inbound side of the
/// peer bridge — `handle_app_crdt_message` into the drain thread — which is
/// where a frame's route is rebuilt from what the sending host stamped on it.
#[cfg(test)]
pub(crate) fn shell_network_for_test(
    tree: WorkspaceTreeHandle,
    local_peer_hash: u64,
) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
    shell_network_for_test_with_store(tree, local_peer_hash, qdoc::blobstore::store())
}

/// [`shell_network_for_test`] with a store of its own.
///
/// Two peers are two stores. A test that lets both ends read the process-wide
/// singleton can assert a transfer succeeded while no transfer happened at
/// all, which is exactly the shape of failure this module exists to prevent.
#[cfg(test)]
pub(crate) fn shell_network_for_test_with_store(
    tree: WorkspaceTreeHandle,
    local_peer_hash: u64,
    store: Arc<dyn qdoc::blobstore::BlobStore>,
) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (event_tx, event_rx) = mpsc::channel(64);
    let net = ShellNetwork {
        cmd_tx,
        replica: Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree))),
        event_tx,
        event_rx: Mutex::new(event_rx),
        runtime: None,
        local_peer_hash,
        keypair: libp2p::identity::Keypair::generate_ed25519(),
        local_peer_id: libp2p::PeerId::random(),
        listen_addrs: Arc::new(Mutex::new(Vec::new())),
        last_sync_kick: Mutex::new(None),
        pending_invites: Mutex::new(std::collections::HashMap::new()),
        redeemed_invites: Mutex::new(std::collections::HashMap::new()),
        inbound_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt_signed: Arc::new(Mutex::new(Vec::new())),
        inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt_snapshot: Arc::new(Mutex::new(Vec::new())),
        snapshot_requests: Arc::new(Mutex::new(std::collections::HashSet::new())),
        blob_relay: Arc::new(Mutex::new(
            crate::network::blob_relay::BlobRelay::with_store(store),
        )),
        inbound_blob: Arc::new(Mutex::new(Vec::new())),
        inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
        inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
    };
    (net, cmd_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_network_spawns_with_default_keypair() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        // `cmd_tx` capacity is the actor's bounded-channel size; just
        // assert it's nonzero so we know the channel was constructed.
        assert!(net.cmd_tx.capacity() > 0);
        // Persistence side-effect: the keypair must have been written.
        assert!(key_path.exists(), "keypair file must exist after spawn");
    }

    /// Two rooms this machine governs, and the network that holds
    /// them. Enough to ask who a name belongs to.
    fn net_with_rooms() -> (
        tempfile::TempDir,
        crate::shell::state::WorkspaceTreeHandle,
        ShellNetwork,
        Vec<crate::shell::state::RoomId>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let rooms: Vec<_> = {
            let mut t = tree.write();
            t.adopt_roomless_workspaces();
            // A real session's workspaces have an owner — first boot
            // installs one, and it is what `may_administer` reads.
            // `bootstrap_demo` leaves it `None`, and a fixture that
            // kept it would have every name below refused for the
            // wrong reason: the negative assertions would pass without
            // the naming path ever having been able to run.
            let me = crate::shell::state::local_user_id();
            for ws in t.workspaces.iter_mut() {
                ws.set_owner(me.clone());
            }
            t.workspaces.iter().map(|w| w.room).collect()
        };
        assert!(
            rooms.len() >= 2 && rooms.iter().all(|r| !r.is_nil()),
            "the fixture governs at least two rooms: {rooms:?}"
        );
        let net = ShellNetwork::spawn(tree.clone(), &tmp.path().join("peer-key")).expect("spawn");
        (tmp, tree, net, rooms)
    }

    /// Park a name against a fresh invitation into `room`, and hand
    /// back the invitation's id — what a redeemer would quote.
    fn park_invite(net: &ShellNetwork, room: crate::shell::state::RoomId, name: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        net.pending_invites.lock().unwrap().insert(
            id.clone(),
            PendingInvite {
                room,
                name: name.to_string(),
                expires_unix_seconds: now_unix_seconds() + 3600,
            },
        );
        id
    }

    fn name_in(
        tree: &crate::shell::state::WorkspaceTreeHandle,
        room: crate::shell::state::RoomId,
        peer: &libp2p::PeerId,
    ) -> Option<String> {
        tree.read()
            .room_peer_names
            .get(&room)
            .and_then(|names| names.get(&peer.to_base58()))
            .cloned()
    }

    /// The name the inviter typed reaches the room document when the
    /// person it was for turns up — not before, because until then
    /// there is no identity to attach it to.
    ///
    /// This is the whole of what makes a host's roster read a person
    /// rather than `peer@12191268537764683811`.
    #[test]
    fn the_invitee_is_named_when_they_ask_this_room_for_its_document() {
        let (_tmp, tree, net, rooms) = net_with_rooms();
        let room = rooms[0];
        let invite = park_invite(&net, room, "Xaver");

        let guest = libp2p::PeerId::random();
        assert_eq!(name_in(&tree, room, &guest), None, "nobody is named yet");
        net.name_invitee_on_arrival(&guest, &room.to_string(), Some(&invite));
        assert_eq!(
            name_in(&tree, room, &guest).as_deref(),
            Some("Xaver"),
            "the room's own authority recorded the name the inviter gave"
        );

        // And it is spent. A second machine that turns up quoting the
        // same invitation keeps its own id rather than inheriting a
        // name that was never for it.
        let stranger = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&stranger, &room.to_string(), Some(&invite));
        assert_eq!(
            name_in(&tree, room, &stranger),
            None,
            "a name belongs to one person, and it was already given"
        );
    }

    /// A name waiting on one room is not a name for another. Rooms are
    /// the unit an invite is issued against, so a peer asking about a
    /// different room is not the peer that redeemed this one.
    #[test]
    fn a_pending_name_does_not_leak_into_another_room() {
        let (_tmp, tree, net, rooms) = net_with_rooms();
        let invite = park_invite(&net, rooms[0], "Xaver");

        let guest = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&guest, &rooms[1].to_string(), Some(&invite));
        assert_eq!(name_in(&tree, rooms[1], &guest), None);
        assert_eq!(
            name_in(&tree, rooms[0], &guest),
            None,
            "asking about another room does not redeem this one's invite"
        );
        assert!(
            net.pending_invites.lock().unwrap().contains_key(&invite),
            "and the name is still waiting for whoever actually redeems it"
        );
    }

    /// Nobody invites themselves, and nothing unparseable is a room.
    /// Both are ways the wrong write reaches an authority-checked
    /// channel, and both are refused before it is asked.
    #[test]
    fn arrival_naming_refuses_this_machine_and_nonsense_rooms() {
        let (_tmp, tree, net, rooms) = net_with_rooms();
        let room = rooms[0];
        let invite = park_invite(&net, room, "Xaver");

        let me = net.local_peer_id;
        net.name_invitee_on_arrival(&me, &room.to_string(), Some(&invite));
        assert_eq!(
            name_in(&tree, room, &me),
            None,
            "this machine's own sync request is not somebody arriving"
        );

        net.name_invitee_on_arrival(&libp2p::PeerId::random(), "not-a-room", Some(&invite));
        assert!(
            net.pending_invites.lock().unwrap().contains_key(&invite),
            "a room id that does not parse names nothing and spends nothing"
        );
    }

    /// A host that governs a room and can mint real invites into it,
    /// with the wire it publishes on left in the caller's hand.
    ///
    /// No swarm: the point of these is what the invitation state does,
    /// and a live actor would put mdns discovery of whatever else is
    /// running on the machine into the middle of it.
    fn inviting_host() -> (
        crate::shell::state::WorkspaceTreeHandle,
        ShellNetwork,
        mpsc::Receiver<NetCmd>,
        crate::shell::state::RoomId,
        crate::shell::state::WorkspaceId,
    ) {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (room, ws) = {
            let mut t = tree.write();
            t.adopt_roomless_workspaces();
            // A room with no owner is a room this machine may not name
            // anybody in, so every assertion below would pass or fail
            // for the wrong reason.
            let me = crate::shell::state::local_user_id();
            for w in t.workspaces.iter_mut() {
                w.set_owner(me.clone());
            }
            let w = t.workspaces.first().expect("bootstrap leaves a workspace");
            (w.room, w.id)
        };
        assert!(!room.is_nil(), "the fixture must govern a real room");
        let (net, cmds) = shell_network_for_test(tree.clone(), 1);
        // An invite carries the address a guest dials, and there is no
        // swarm here to report one.
        net.listen_addrs
            .lock()
            .unwrap()
            .push("/ip4/127.0.0.1/tcp/4001".parse().expect("a multiaddr"));
        (tree, net, cmds, room, ws)
    }

    /// What a guest reads off the token it was handed. Quoting it back
    /// is what a redemption does, so a test that quotes it is quoting
    /// exactly what travels.
    fn invitation_in(token: &str) -> String {
        crate::network::invite::validate_invite_token(token)
            .expect("a token this machine just minted must verify")
            .invite_id
    }

    /// Two invitations out at once, redeemed in the order they were
    /// minted, and each person is called what they were invited as.
    #[test]
    fn invitations_out_together_are_redeemed_in_the_order_they_were_minted() {
        let (tree, net, _cmds, room, ws) = inviting_host();
        let for_xaver = net.issue_invite_named(ws, 3600, "Xaver").expect("mint");
        let for_claude = net.issue_invite_named(ws, 3600, "Claude").expect("mint");

        let xaver = libp2p::PeerId::random();
        let claude = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&xaver, &room.to_string(), Some(&invitation_in(&for_xaver)));
        net.name_invitee_on_arrival(&claude, &room.to_string(), Some(&invitation_in(&for_claude)));

        assert_eq!(name_in(&tree, room, &xaver).as_deref(), Some("Xaver"));
        assert_eq!(
            name_in(&tree, room, &claude).as_deref(),
            Some("Claude"),
            "minting a second invite must not have spent the first one's name"
        );
    }

    /// The same two invitations, redeemed the other way round.
    ///
    /// This is the case that was broken: invite one person, invite a
    /// second before the first has redeemed anything, and the second
    /// one knocks first. Both names were parked against the room, so
    /// the second mint overwrote the first, whoever arrived first took
    /// the surviving name, and the person who came after had nothing
    /// waiting for them.
    #[test]
    fn invitations_out_together_are_redeemed_in_reverse_order() {
        let (tree, net, _cmds, room, ws) = inviting_host();
        let for_xaver = net.issue_invite_named(ws, 3600, "Xaver").expect("mint");
        let for_claude = net.issue_invite_named(ws, 3600, "Claude").expect("mint");

        let xaver = libp2p::PeerId::random();
        let claude = libp2p::PeerId::random();
        // Claude was invited second and arrives first.
        net.name_invitee_on_arrival(&claude, &room.to_string(), Some(&invitation_in(&for_claude)));
        net.name_invitee_on_arrival(&xaver, &room.to_string(), Some(&invitation_in(&for_xaver)));

        assert_eq!(name_in(&tree, room, &claude).as_deref(), Some("Claude"));
        assert_eq!(
            name_in(&tree, room, &xaver).as_deref(),
            Some("Xaver"),
            "arriving second is not a reason to lose the name you were invited as"
        );
    }

    /// A person and an agent invited back to back, agent first through
    /// the door.
    ///
    /// The agent is admitted by capability *and* redeems a token, which
    /// is what the two of them do in a real session — so if the two
    /// routes shared one slot of pending-name state this is where they
    /// would take each other's.
    #[test]
    fn a_person_and_an_agent_invited_together_keep_their_own_names() {
        use crate::shell::state::{CapOverride, Subject, Verb};
        let (tree, net, _cmds, room, ws) = inviting_host();
        let me = crate::shell::state::local_user_id();

        let for_xaver = net.issue_invite_named(ws, 3600, "Xaver").expect("mint");
        // Admitting the agent is an ordinary grant on the room, the way
        // `agent.invite` makes it.
        let admitted = tree.write().grant_in_workspace(
            ws,
            &me,
            CapOverride::membership(
                Subject::agent("claude".to_string()),
                ws.raw().to_string(),
                Verb::Write,
            )
            .with_id("agent-member:claude".to_string()),
        );
        assert!(admitted.is_some(), "the fixture's owner administers this room");
        let for_claude = net.issue_invite_named(ws, 3600, "Claude").expect("mint");

        let xaver = libp2p::PeerId::random();
        let claude = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&claude, &room.to_string(), Some(&invitation_in(&for_claude)));
        net.name_invitee_on_arrival(&xaver, &room.to_string(), Some(&invitation_in(&for_xaver)));

        assert_eq!(name_in(&tree, room, &claude).as_deref(), Some("Claude"));
        assert_eq!(name_in(&tree, room, &xaver).as_deref(), Some("Xaver"));
    }

    /// Nobody who was invited by name is left showing the fallback.
    ///
    /// `peer@<handle>` is what the roster falls back to for somebody
    /// the room never named, and it is the whole of what the user saw.
    /// Asserted on the pair the frame loop actually builds a roster
    /// from — `room_peer_labels` for what the room calls people, and
    /// `room_roster` for the rows — rather than on the naming map the
    /// tests above read, because the fallback lives in the second of
    /// those and not the first.
    #[test]
    fn nobody_invited_by_name_is_left_as_a_bare_handle() {
        let (tree, net, _cmds, room, ws) = inviting_host();
        let for_xaver = net.issue_invite_named(ws, 3600, "Xaver").expect("mint");
        let for_claude = net.issue_invite_named(ws, 3600, "Claude").expect("mint");

        let xaver = libp2p::PeerId::random();
        let claude = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&claude, &room.to_string(), Some(&invitation_in(&for_claude)));
        net.name_invitee_on_arrival(&xaver, &room.to_string(), Some(&invitation_in(&for_xaver)));

        let t = tree.read();
        let workspace = t
            .workspaces
            .iter()
            .find(|w| w.id == ws)
            .expect("the workspace the invites were minted from");
        let labels = t.room_peer_labels(room);
        let arrived: std::collections::BTreeSet<u64> = [xaver, claude]
            .iter()
            .map(crate::network::behaviour::peer_liveness_handle)
            .collect();
        // The host's own row is the one the roster leaves out, and
        // `spawn` derives that handle from the same peer id the room
        // document names this machine under. The test fixture mints its
        // keypair and its peer id separately, so it has to be spelled
        // here rather than read off `local_peer_id_hash`.
        let rows = crate::shell::peer_presence::room_roster(
            workspace,
            &labels,
            &arrived,
            crate::network::behaviour::peer_liveness_handle(&net.local_peer_id),
        );

        assert_eq!(rows.len(), 2, "both people are in the room: {rows:?}");
        for row in &rows {
            assert!(
                !row.name.starts_with("peer@"),
                "somebody the host typed a name for is on screen as a number: {row:?}"
            );
        }
        let mut names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Claude", "Xaver"]);
    }

    /// An invitation nobody redeems, and one that has run out, leave
    /// the invitations beside them exactly as they were.
    #[test]
    fn an_unredeemed_or_expired_invitation_leaves_the_others_alone() {
        let (tree, net, _cmds, room, ws) = inviting_host();
        let expired = net.issue_invite_named(ws, 3600, "Ghost").expect("mint");
        let never_redeemed = net.issue_invite_named(ws, 3600, "Mallory").expect("mint");
        let for_xaver = net.issue_invite_named(ws, 3600, "Xaver").expect("mint");

        // Minted the way the other two were, then moved into the past.
        // Running out is the passage of time and nothing else, and a
        // test cannot wait an hour for it — nor mint something already
        // spent, because reading the id back off a token is itself a
        // validation that refuses an expired one.
        let ghost_invitation = invitation_in(&expired);
        net.pending_invites
            .lock()
            .unwrap()
            .get_mut(&ghost_invitation)
            .expect("the invitation was parked when it was minted")
            .expires_unix_seconds = now_unix_seconds() - 1;

        // The one that ran out redeems nothing, even quoted correctly.
        let ghost = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&ghost, &room.to_string(), Some(&ghost_invitation));
        assert_eq!(
            name_in(&tree, room, &ghost),
            None,
            "an invitation that has run out is not a name anybody may still collect"
        );

        // And the ones beside it are untouched.
        let xaver = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&xaver, &room.to_string(), Some(&invitation_in(&for_xaver)));
        assert_eq!(name_in(&tree, room, &xaver).as_deref(), Some("Xaver"));
        assert_eq!(
            net.pending_invites
                .lock()
                .unwrap()
                .get(&invitation_in(&never_redeemed))
                .map(|held| held.name.clone())
                .as_deref(),
            Some("Mallory"),
            "an invitation nobody has redeemed is still waiting for the person it names"
        );
    }

    /// The same name, twice, is two invitations.
    ///
    /// Two people can be called the same thing, and inviting a second
    /// one must not hand the first one's promise away or leave the
    /// second waiting on a slot that has already been spent.
    #[test]
    fn the_same_name_invited_twice_is_two_invitations() {
        let (tree, net, _cmds, room, ws) = inviting_host();
        let first = net.issue_invite_named(ws, 3600, "Claude").expect("mint");
        let second = net.issue_invite_named(ws, 3600, "Claude").expect("mint");
        assert_ne!(
            invitation_in(&first),
            invitation_in(&second),
            "two invitations are two invitations even when they name the same person"
        );

        let one = libp2p::PeerId::random();
        let two = libp2p::PeerId::random();
        net.name_invitee_on_arrival(&two, &room.to_string(), Some(&invitation_in(&second)));
        net.name_invitee_on_arrival(&one, &room.to_string(), Some(&invitation_in(&first)));

        assert_eq!(name_in(&tree, room, &one).as_deref(), Some("Claude"));
        assert_eq!(name_in(&tree, room, &two).as_deref(), Some("Claude"));
    }

    /// The round trip, through the code a running session runs.
    ///
    /// A host mints two invitations; a guest accepts the second one for
    /// real, and the request it puts on the wire is inspected as it
    /// goes — the invitation has to be *in* it, because the host has no
    /// other way to tell which of its two promises just came due. That
    /// request is then handed to the host through `tick`, which is the
    /// function the compositor calls every frame, and the host names
    /// the guest from it.
    #[test]
    fn a_guest_quotes_the_invitation_it_redeemed_and_the_host_names_it() {
        let (host_tree, host_net, _host_cmds, room, ws) = inviting_host();
        let _for_xaver = host_net.issue_invite_named(ws, 3600, "Xaver").expect("mint");
        let for_claude = host_net.issue_invite_named(ws, 3600, "Claude").expect("mint");

        // A second machine, which knows nothing except the token.
        let guest_tree = WorkspaceTree::bootstrap_demo().shared();
        let guest_ws = {
            let mut t = guest_tree.write();
            t.adopt_roomless_workspaces();
            t.workspaces.first().expect("a workspace").id
        };
        let (guest_net, mut guest_cmds) = shell_network_for_test(guest_tree.clone(), 2);
        let joined = guest_net
            .accept_invite(guest_ws, &for_claude)
            .expect("the token the host minted must be redeemable");
        assert_eq!(joined, room, "the guest joined the room the token named");

        // What actually went out on the wire.
        let mut quoted: Option<String> = None;
        while let Ok(cmd) = guest_cmds.try_recv() {
            if let NetCmd::Publish {
                msg:
                    NeighbourMsg::SyncReq {
                        room: ref asked,
                        ref invite,
                        ..
                    },
                ..
            } = cmd
                && asked == &room.to_string()
            {
                quoted = invite.clone();
            }
        }
        assert_eq!(
            quoted.as_deref(),
            Some(invitation_in(&for_claude).as_str()),
            "a guest that redeemed an invitation has to say which one, or a \
             host with two out cannot tell them apart"
        );

        // And the host, driven the way a frame drives it.
        let guest_peer = libp2p::PeerId::random();
        host_net
            .event_tx
            .try_send(crate::network::actor::NetEvent::Msg {
                ws: DEFAULT_WS_TOPIC,
                peer: guest_peer,
                msg: NeighbourMsg::SyncReq {
                    since: serde_json::to_vec(&Vec::<automerge::ChangeHash>::new())
                        .expect("empty heads serialise"),
                    room: room.to_string(),
                    invite: quoted,
                },
            })
            .expect("the test channel has room");
        host_net.tick();

        assert_eq!(
            name_in(&host_tree, room, &guest_peer).as_deref(),
            Some("Claude"),
            "the host named the arrival from the invitation it quoted"
        );
        assert!(
            !host_net.pending_invites.lock().unwrap().is_empty(),
            "and Xaver's invitation is still standing, waiting for Xaver"
        );
    }

    #[test]
    fn tick_with_no_events_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        // Should not panic on empty channel; should not block.
        net.tick();
    }

    /// Accepting an invite joins the room *and* asks it to catch us up,
    /// in one call. Both halves are checked on the real objects: the
    /// tree's room after the call, and the command the network actor was
    /// handed.
    /// Build the inbound event a LAN peer's app-CRDT publish arrives as.
    fn app_crdt_event(cluster_id: &str, from_peer_hash: u64) -> NetEvent {
        NetEvent::Msg {
            // The shared gossipsub topic every machine subscribes to,
            // which is exactly how such a frame reaches us today.
            ws: DEFAULT_WS_TOPIC,
            peer: libp2p::PeerId::random(),
            msg: NeighbourMsg::AppCrdtIncremental {
                peer: from_peer_hash,
                app_id: "dev.qos.notes".to_string(),
                cluster_id: cluster_id.to_string(),
                record_id: "note-1".to_string(),
                data: vec![1, 2, 3, 4],
            },
        }
    }

    /// A peer's app document reaches local apps only for a room this
    /// machine is in.
    ///
    /// Driven through the real receive path — an event pushed onto the
    /// actor→shell channel and drained by `tick`, which is what the
    /// live actor does — because the defect was that this path asked no
    /// membership question at all. Calling the helper directly would
    /// prove only that the helper exists.
    ///
    /// The accepted case is not decoration: without it an assertion
    /// that the buffer is empty passes just as well when nothing is
    /// ever delivered, and would keep passing if the filter were
    /// tightened into a wall.
    #[test]
    fn app_crdt_from_a_room_we_are_not_in_never_reaches_the_broker() {
        let mut tree = WorkspaceTree::default();
        let local = tree.append_empty_workspace("Mine");
        let our_room = tree.room_of(local).expect("a minted room");
        let handle = tree.shared();
        let (net, _cmd_rx) = shell_network_for_test(handle, 0xAAAAu64);

        // A room this machine has never joined.
        let stranger_room = crate::shell::state::RoomId::new();
        assert_ne!(stranger_room, our_room);

        net.event_tx
            .try_send(app_crdt_event(&stranger_room.to_string(), 0xBBBBu64))
            .expect("queue the stranger's publish");
        net.tick();
        assert!(
            net.inbound_app_crdt_handle().lock().unwrap().is_empty(),
            "a delta for a room this machine is not in must not reach the broker queue",
        );

        // Not a uuid at all — fail closed rather than fall through.
        net.event_tx
            .try_send(app_crdt_event("not-a-room", 0xBBBBu64))
            .expect("queue the malformed publish");
        net.tick();
        assert!(
            net.inbound_app_crdt_handle().lock().unwrap().is_empty(),
            "a cluster id that is not a uuid names no room, so it must be refused",
        );

        // The positive control: the same peer, the same shape, a room
        // we are in — this one must arrive, or the two refusals above
        // prove nothing.
        net.event_tx
            .try_send(app_crdt_event(&our_room.to_string(), 0xBBBBu64))
            .expect("queue the member's publish");
        net.tick();
        let delivered = net.inbound_app_crdt_handle().lock().unwrap().len();
        assert_eq!(
            delivered, 1,
            "a delta for a room this machine is in must still reach the broker queue",
        );
    }

    #[test]
    fn accepting_an_invite_joins_the_room_and_asks_it_for_a_catch_up() {
        use crate::network::invite::make_invite_token;
        use crate::shell::state::Workspace;

        // The inviter's workspace, and the room it is inviting into.
        let host_ws = Workspace::test_default(
            crate::shell::state::WorkspaceId::new(),
            "Design review".to_string(),
        );
        let host_room = host_ws.room;
        let owner_key = libp2p::identity::Keypair::generate_ed25519();
        let token = make_invite_token(&owner_key, &host_ws, "/ip4/127.0.0.1/tcp/4001", 3600)
            .expect("the host issues an invite");

        // The guest: its own workspace, its own room, until it accepts.
        let mut guest = WorkspaceTree::default();
        let local = guest.append_empty_workspace("Mine");
        let before = guest.room_of(local).expect("the guest's own room");
        assert_ne!(before, host_room);
        let tree = guest.shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree.clone(), 0x1234u64);

        let joined = net
            .accept_invite(local, &token)
            .expect("a well-formed invite is accepted");
        assert_eq!(joined, host_room, "the guest joins the room it was invited to");
        assert_eq!(
            tree.read().room_of(local),
            Some(host_room),
            "the guest's workspace now addresses the inviter's room",
        );

        // Two commands, in this order: dial the address the token named,
        // then ask that room for a catch-up. The dial is what makes the
        // address in an invite mean anything — mdns finds peers on a
        // LAN, but a guest handed directions should not have to wait for
        // discovery to find the host it was pointed at.
        let dial = cmd_rx
            .try_recv()
            .expect("accepting an invite must dial the address it named");
        match dial {
            NetCmd::Connect { addr } => assert_eq!(
                addr.to_string(),
                "/ip4/127.0.0.1/tcp/4001",
                "the dial must go to the address inside the token",
            ),
            other => panic!("expected NetCmd::Connect, got {other:?}"),
        }
        let cmd = cmd_rx
            .try_recv()
            .expect("accepting an invite must ask the room for a catch-up");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::SyncReq { room, .. } => assert_eq!(
                        room,
                        host_room.to_string(),
                        "the request must name the room just joined",
                    ),
                    other => panic!("expected a SyncReq, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
        assert!(
            cmd_rx.try_recv().is_err(),
            "one room joined is one request — the guest must not ask about \
             anything else",
        );
    }

    /// A token that does not verify joins nothing and asks nothing. The
    /// refusal comes from `accept_invite_into`'s own validation, which is
    /// why this path takes the token string rather than a parsed token.
    #[test]
    fn a_refused_invite_changes_nothing_and_asks_nothing() {
        let mut guest = WorkspaceTree::default();
        let local = guest.append_empty_workspace("Mine");
        let before = guest.room_of(local).expect("the guest's own room");
        let tree = guest.shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree.clone(), 0x1234u64);

        let err = net
            .accept_invite(local, "not-a-token")
            .expect_err("garbage must be refused");
        assert!(!err.is_empty());
        assert_eq!(
            tree.read().room_of(local),
            Some(before),
            "a refused invite must leave the guest in its own room",
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "a refused invite must not put anything on the wire",
        );
    }

    #[test]
    fn publish_presence_emits_a_publish_command() {
        // Verify that `publish_presence` packages the (key, point) pair
        // into a `NetCmd::Publish { msg: PresenceMoved }` on the actor
        // command channel. Uses the test-only constructor so we own the
        // receiver and can drain commands without spinning up libp2p.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xABCDu64);
        net.publish_presence("local-cursor".into(), (100.0, 200.0));
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_presence must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::PresenceMoved {
                        peer,
                        user,
                        key,
                        point,
                        ts: _,
                    } => {
                        assert_eq!(peer, 0xABCDu64);
                        // The session names who is at it, so a peer's
                        // cursor can be labelled with a person rather
                        // than a hashed session handle.
                        assert_eq!(user, crate::shell::state::local_user_id());
                        assert!(!user.is_empty());
                        assert_eq!(key, "local-cursor");
                        assert_eq!(point, (100.0, 200.0));
                    }
                    other => panic!("expected PresenceMoved, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    #[test]
    fn publish_app_crdt_snapshot_wraps_the_whole_document() {
        // The frame that makes a peer's freshly-created document adopt
        // somebody else's. The record has to survive the wrap for the
        // same reason it does on an increment, and more so: a whole
        // document merged into the wrong record cannot be un-merged.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFEu64);

        // Nobody has arrived, so this document's state is carried by the
        // deltas and has no business on a broadcast topic.
        net.publish_app_crdt_snapshot(
            "dev.qos.word-processor".into(),
            "00000000-0000-0000-0000-000000000000".into(),
            "file:/work/report.odt".into(),
            vec![0xAA, 0xBB, 0xCC],
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "a snapshot nobody asked for must not be published",
        );

        net.note_snapshot_request("00000000-0000-0000-0000-000000000000");
        net.publish_app_crdt_snapshot(
            "dev.qos.word-processor".into(),
            "00000000-0000-0000-0000-000000000000".into(),
            "file:/work/report.odt".into(),
            vec![0xAA, 0xBB, 0xCC],
        );
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_app_crdt_snapshot must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::AppCrdtSnapshot {
                        peer,
                        app_id,
                        cluster_id,
                        record_id,
                        data,
                    } => {
                        assert_eq!(peer, 0xCAFEu64);
                        assert_eq!(app_id, "dev.qos.word-processor");
                        assert_eq!(cluster_id, "00000000-0000-0000-0000-000000000000");
                        assert_eq!(record_id, "file:/work/report.odt");
                        assert_eq!(data, vec![0xAA, 0xBB, 0xCC]);
                    }
                    other => panic!("expected AppCrdtSnapshot, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_snapshot_is_refused_rather_than_handed_to_gossipsub() {
        // gossipsub caps an RPC at 64 KiB and rejects anything larger,
        // so publishing a document past the cap would fail inside the
        // transport with nothing at this layer knowing it had. Refusing
        // here is what turns "this document silently never converges"
        // into a log line naming the size.
        //
        // Asserted on the queue rather than on the log: the observable
        // consequence is that nothing goes out.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFEu64);
        net.note_snapshot_request("00000000-0000-0000-0000-000000000000");
        net.publish_app_crdt_snapshot(
            "dev.qos.word-processor".into(),
            "00000000-0000-0000-0000-000000000000".into(),
            String::new(),
            vec![0u8; MAX_APP_SNAPSHOT_BYTES + 1],
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "a snapshot past the cap must not be queued for publish",
        );

        // Exactly at the cap still goes out — the boundary belongs to
        // the documents that fit, not to the ones that don't.
        net.note_snapshot_request("00000000-0000-0000-0000-000000000000");
        net.publish_app_crdt_snapshot(
            "dev.qos.word-processor".into(),
            "00000000-0000-0000-0000-000000000000".into(),
            String::new(),
            vec![0u8; MAX_APP_SNAPSHOT_BYTES],
        );
        assert!(
            cmd_rx.try_recv().is_ok(),
            "a snapshot exactly at the cap must still publish",
        );
    }

    #[test]
    fn one_arrival_releases_one_document_and_no_more() {
        // The toolkit sends a whole document on every commit that
        // changes it, and the delta alongside already carries the edit
        // to anyone who shares the history. Republishing each one would
        // put an unbounded payload on the room topic per commit, so the
        // arming is one-shot: a peer arrives, one document goes out, and
        // the commits after it stay local until somebody else arrives.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFEu64);
        let room = "00000000-0000-0000-0000-000000000000";

        net.note_snapshot_request(room);
        for _ in 0..4 {
            net.publish_app_crdt_snapshot(
                "dev.qos.word-processor".into(),
                room.into(),
                String::new(),
                vec![0xAA],
            );
        }
        assert!(cmd_rx.try_recv().is_ok(), "the arrival releases one document");
        assert!(
            cmd_rx.try_recv().is_err(),
            "the commits after it must not each broadcast the document again",
        );

        // A different room's arrival does not release this one's.
        net.note_snapshot_request("00000000-0000-0000-0000-000000000001");
        net.publish_app_crdt_snapshot(
            "dev.qos.word-processor".into(),
            room.into(),
            String::new(),
            vec![0xAA],
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "an arrival in another room must not release this room's document",
        );
    }

    #[test]
    fn snapshot_at_the_cap_fits_a_gossipsub_rpc() {
        // The cap is only worth anything if a message built at it
        // actually fits the transport, and the arithmetic is not the
        // obvious one: the actor publishes with `serde_json`, which
        // writes a `Vec<u8>` as decimal numbers, so every byte costs up
        // to four characters. An earlier version of this constant was
        // set as though the cost were 1:1 and would have been rejected
        // by gossipsub for any document near it.
        //
        // Worst case on both axes: bytes that each need three digits,
        // and a record id the length of a real path.
        let msg = NeighbourMsg::AppCrdtSnapshot {
            peer: u64::MAX,
            app_id: "dev.qos.word-processor".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:".to_string() + &"a".repeat(4096),
            data: vec![0xFFu8; MAX_APP_SNAPSHOT_BYTES],
        };
        let encoded = serde_json::to_vec(&msg).expect("encode");
        assert!(
            encoded.len() < 65536,
            "a snapshot at the cap must still fit a gossipsub RPC; got {} bytes",
            encoded.len(),
        );
    }

    #[test]
    fn handle_app_crdt_snapshot_message_buffers_for_drain() {
        // The receive-side adapter the broker's drainer reads. Every
        // component of the routing triple has to round-trip, because
        // the drainer rebuilds the route from exactly these fields.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let buf_handle = net.inbound_app_crdt_snapshot_handle();
        net.handle_app_crdt_snapshot_message(
            7u64,
            "dev.qos.word-processor".into(),
            "00000000-0000-0000-0000-000000000000".into(),
            "file:/work/report.odt".into(),
            vec![0x01, 0x02],
        );
        let buf = buf_handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].app_id, "dev.qos.word-processor");
        assert_eq!(buf[0].cluster_id, "00000000-0000-0000-0000-000000000000");
        assert_eq!(buf[0].record_id, "file:/work/report.odt");
        assert_eq!(buf[0].data, vec![0x01, 0x02]);
    }

    #[test]
    fn handle_presence_message_buffers_for_drain() {
        // The receive-side adapter pushes to the inbound buffer that
        // the shell's frame drainer folds into `PresenceCtx`. Verify
        // that the entry round-trips with every field preserved so the
        // overlay can attribute the cursor to the right person, in the
        // right scope, at the right time.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let buf_handle = net.inbound_presence_handle();
        net.handle_presence_message(
            7u64,
            "bob".into(),
            "ws/0".into(),
            (10.0, 20.0),
            1_700_000_000,
        );
        let buf = buf_handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].user, "bob");
        assert_eq!(buf[0].key, "ws/0");
        assert_eq!(buf[0].point, (10.0, 20.0));
        assert_eq!(buf[0].ts, 1_700_000_000);
    }

    #[test]
    fn a_session_does_not_take_its_own_presence_off_the_network() {
        // A session republishes the pointers of every app under it, so
        // gossip can hand its own frames back. Painting them would put
        // a second cursor on screen chasing the one under the user's
        // hand — and label it as a peer.
        let tree = WorkspaceTree::bootstrap_default().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 99u64);
        let buf_handle = net.inbound_presence_handle();
        net.handle_presence_message(
            99u64,
            "me".into(),
            "ws/0".into(),
            (10.0, 20.0),
            1_700_000_000,
        );
        assert!(
            buf_handle.lock().unwrap().is_empty(),
            "a session's own presence must not be drained back into its overlay"
        );
        net.handle_presence_message(
            7u64,
            "bob".into(),
            "ws/0".into(),
            (30.0, 40.0),
            1_700_000_001,
        );
        assert_eq!(
            buf_handle.lock().unwrap().len(),
            1,
            "another session's presence must still arrive"
        );
    }

    #[test]
    fn publish_app_crdt_incremental_emits_a_publish_command() {
        // Phase 2 bridge: the broker should be able to forward a
        // local `SyncIncremental` as a `NeighbourMsg::AppCrdtIncremental`.
        // Verify the wrapping is correct (peer id stamped, payload
        // round-trips verbatim, default workspace topic) without
        // spinning up libp2p.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFEu64);
        net.publish_app_crdt_incremental(
            "terminal".into(),
            "00000000-0000-0000-0000-000000000000".into(),
            "file:/work/notes.md".into(),
            vec![0xAA, 0xBB, 0xCC],
        );
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_app_crdt_incremental must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::AppCrdtIncremental {
                        peer,
                        app_id,
                        cluster_id,
                        record_id,
                        data,
                    } => {
                        assert_eq!(peer, 0xCAFEu64);
                        assert_eq!(app_id, "terminal");
                        assert_eq!(cluster_id, "00000000-0000-0000-0000-000000000000");
                        // The record has to survive the wrap: it is what
                        // tells the receiving broker which document these
                        // changes belong to.
                        assert_eq!(record_id, "file:/work/notes.md");
                        assert_eq!(data, vec![0xAA, 0xBB, 0xCC]);
                    }
                    other => panic!("expected AppCrdtIncremental, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    /// The whole product claim, on the production path: a photograph goes into
    /// a shared document, everything the room has to send fits the transport,
    /// the far side reassembles the picture byte-for-byte, and the typing that
    /// happened *after* the image is on the far document.
    ///
    /// This drives the real `publish_app_crdt_incremental` — so the real
    /// `sync_content`, the real chunking, and the real reassembly-and-verify —
    /// and inspects the actual `NeighbourMsg` stream it puts on the wire.
    /// That the stream then crosses two machines is proven separately by
    /// `network::actor::tests::two_actors_exchange_publish_via_loopback`, which
    /// runs the same messages over real libp2p on loopback.
    ///
    /// Against the previous code this could not pass: the photograph's frame
    /// alone measured ~146 KB against a 64 KiB ceiling, so it was refused by
    /// the transport and dropped, and every keystroke after it stayed queued on
    /// the far replica forever.
    #[test]
    fn a_photograph_crosses_and_typing_after_it_keeps_arriving() {
        use automerge::{AutoCommit, ROOT};
        use qdoc::model::{self, DocMut, IdGen};

        const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000c";
        const RECORD: &str = "file:/work/shared/notes.qdoc";
        // libp2p-gossipsub's default max RPC; `NiriosBehaviour` does not raise it.
        const GOSSIP_MAX: usize = 65_536;

        // A photograph: incompressible bytes behind a real PNG header, so its
        // size on the wire is its size on disk.
        let photo: Vec<u8> = {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            v.extend_from_slice(&13u32.to_be_bytes());
            v.extend_from_slice(b"IHDR");
            v.extend_from_slice(&1200u32.to_be_bytes());
            v.extend_from_slice(&800u32.to_be_bytes());
            v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
            let mut x = 0x2545_F491_4F6C_DD1Du64;
            while v.len() < 146_492 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                v.push((x >> 33) as u8);
            }
            v
        };
        let photo_hash = qdoc::blobstore::hash_bytes(&photo);

        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xA11CEu64);

        // --- the authoring session -------------------------------------
        let mut doc = AutoCommit::new();
        let c = model::init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        let mut publish = |doc: &mut AutoCommit| {
            net.publish_app_crdt_incremental(
                "dev.qos.word-processor".into(),
                ROOM.into(),
                RECORD.into(),
                doc.save_incremental(),
            );
        };

        let first = {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "Before the photograph.");
            p
        };
        publish(&mut doc);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let h = dm.put_blob(&photo);
            let img = dm.insert_block(Some(&first), "image");
            dm.set_attr(&img, "blob", h.as_str());
        }
        publish(&mut doc);
        for i in 0..10 {
            {
                let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
                let p = dm.insert_block(None, "paragraph");
                dm.splice_text(&p, 0, 0, &format!("Line {i} after the photograph."));
            }
            publish(&mut doc);
        }

        // --- what actually goes on the wire ----------------------------
        let mut far_doc = AutoCommit::new();
        let mut far_relay = crate::network::blob_relay::BlobRelay::new();
        let mut arrived: Option<Vec<u8>> = None;
        let mut frames = 0usize;
        let mut chunks = 0usize;

        while let Ok(cmd) = cmd_rx.try_recv() {
            let NetCmd::Publish { msg, .. } = cmd else { continue };
            let encoded = serde_json::to_vec(&msg).expect("the actor encodes with serde_json");
            assert!(
                encoded.len() <= GOSSIP_MAX,
                "a {} of {} bytes cannot be published: the transport refuses it and the \
                 publish path drops it",
                msg.kind(),
                encoded.len(),
            );
            match msg {
                NeighbourMsg::AppCrdtIncremental { data, .. } => {
                    frames += 1;
                    far_doc.load_incremental(&data).expect("a delivered frame applies");
                }
                NeighbourMsg::BlobChunk { hash, index, count, data, .. } => {
                    chunks += 1;
                    if let Some(whole) = far_relay.accept_chunk(1, &hash, index, count, data) {
                        arrived = Some(whole);
                    }
                }
                _ => {}
            }
        }

        assert_eq!(frames, 12, "every edit must produce a publishable frame");
        assert!(chunks > 1, "the photograph must have been split, got {chunks} chunk(s)");

        // The picture crossed, intact.
        assert_eq!(
            arrived.as_deref(),
            Some(photo.as_slice()),
            "the photograph must arrive byte-for-byte and verify against its hash",
        );

        // The document is whole, and still receiving.
        let missing = far_doc.get_missing_deps(&[]);
        assert!(
            missing.is_empty(),
            "the far document must not be waiting on a change it was never sent, got {} \
             missing ancestor(s)",
            missing.len(),
        );
        let far_c = model::open(&far_doc, &ROOT).expect("the far side has the document");
        let text = model::materialize(&far_doc, &far_c)
            .blocks
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Before the photograph."), "got: {text:?}");
        assert!(
            text.contains("Line 0 after the photograph."),
            "typing after the photograph must keep arriving — this is the line that used \
             to be lost forever. Got: {text:?}",
        );
        assert!(text.contains("Line 9 after the photograph."), "got: {text:?}");

        // And the far side can name the picture it now holds.
        let img = model::materialize(&far_doc, &far_c)
            .blocks
            .into_iter()
            .find(|b| b.kind == "image")
            .expect("the image block crossed");
        assert_eq!(
            img.attrs.get("blob"),
            Some(&qdoc::snapshot::AttrVal::Str(photo_hash)),
            "the reference must name the content that arrived",
        );
    }

    /// A photograph must reach a peer that missed the moment it was offered.
    ///
    /// This is the defect as a person met it: the run sheet, the prose and
    /// every keystroke converged on the second machine, and where the picture
    /// belonged there was a grey box reading "image". The reference crossed;
    /// the bytes did not.
    ///
    /// Two things had to be true at once for that, and this test holds both:
    ///
    /// - **Two peers are two stores.** Each side gets its own, so "the guest
    ///   has the photograph" cannot be satisfied by the sender's copy. The
    ///   process-wide singleton made that mistake easy to write and impossible
    ///   to see.
    /// - **The one offer is missed.** The chunks the host emits unprompted are
    ///   dropped here rather than delivered — which is what a peer that joined
    ///   afterwards, a mesh still forming, or a lost piece all look like from
    ///   the guest's side. Every one of those was permanent, because content
    ///   was offered once per room per hash and never asked for.
    ///
    /// Against the previous code this could not pass: the guest observed a
    /// reference it could not resolve, wrote one line about it, and stopped.
    /// Nothing on the wire asked for the content, so the placeholder was the
    /// end state rather than a state on the way to the picture.
    #[test]
    fn a_photograph_reaches_a_peer_that_missed_the_offer() {
        use automerge::{AutoCommit, ROOT};
        use qdoc::blobstore::{BlobStore, MemoryBlobStore};
        use qdoc::model::{self, DocMut, IdGen};

        const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000d";
        const RECORD: &str = "file:/work/shared/harbour.qdoc";

        let photo: Vec<u8> = {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            v.extend_from_slice(&13u32.to_be_bytes());
            v.extend_from_slice(b"IHDR");
            v.extend_from_slice(&512u32.to_be_bytes());
            v.extend_from_slice(&320u32.to_be_bytes());
            v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
            let mut x = 0x9E37_79B9_7F4A_7C15u64;
            while v.len() < 98_304 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                v.push((x >> 33) as u8);
            }
            v
        };
        let photo_hash = qdoc::blobstore::hash_bytes(&photo);

        // Two machines, two stores. The host holds the picture because its own
        // app handed it over; the guest holds nothing at all.
        let host_store: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
        let guest_store: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
        assert!(host_store.put(&photo_hash, &photo));

        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (host, mut host_rx) =
            shell_network_for_test_with_store(tree.clone(), 0x400D_5EED, Arc::clone(&host_store));
        let (guest, mut guest_rx) =
            shell_network_for_test_with_store(tree, 0x64E57, Arc::clone(&guest_store));

        // --- the host's document, with a photograph in it ----------------
        let mut doc = AutoCommit::new();
        let c = model::init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            let p = dm.insert_block(None, "paragraph");
            dm.splice_text(&p, 0, 0, "Run sheet.");
            let h = dm.put_blob(&photo);
            assert_eq!(h, photo_hash, "the fixture must be the content the store holds");
            let img = dm.insert_block(Some(&p), "image");
            dm.set_attr(&img, "blob", h.as_str());
        }
        host.publish_app_crdt_incremental(
            "dev.qos.word-processor".into(),
            ROOM.into(),
            RECORD.into(),
            doc.save_incremental(),
        );

        // --- the guest misses the offer ----------------------------------
        // Keep the document frames; drop every chunk. This is a peer that
        // joined a moment late, or a mesh that had not finished forming.
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut offered = 0usize;
        while let Ok(cmd) = host_rx.try_recv() {
            let NetCmd::Publish { msg, .. } = cmd else { continue };
            match msg {
                NeighbourMsg::AppCrdtIncremental { data, .. } => frames.push(data),
                NeighbourMsg::BlobChunk { .. } => offered += 1,
                _ => {}
            }
        }
        assert!(!frames.is_empty(), "the document itself must have gone out");
        assert!(offered > 0, "the host must have offered the content at least once");

        // The guest folds in what it did receive, exactly as `tick` does.
        let mut guest_doc = AutoCommit::new();
        for f in &frames {
            guest.sync_content(ROOM, RECORD, f);
            guest_doc.load_incremental(f).expect("a delivered frame applies");
        }

        // The reference is here and the content is not. That is the state the
        // frame showed: a placeholder, correctly, on a converged document.
        let guest_c = model::open(&guest_doc, &ROOT).expect("the guest has the document");
        assert_eq!(
            model::blob_hashes(&guest_doc, &guest_c),
            vec![photo_hash.clone()],
            "the guest's document must name the photograph",
        );
        assert!(
            guest_store.get(&photo_hash).is_none(),
            "the premise of this test is that the guest does not have the bytes yet",
        );

        // --- the guest asks, the host answers ----------------------------
        let mut asked = 0usize;
        while let Ok(cmd) = guest_rx.try_recv() {
            let NetCmd::Publish { msg, .. } = cmd else { continue };
            if let NeighbourMsg::BlobRequest { cluster_id, hash, .. } = msg {
                asked += 1;
                host.handle_blob_request_message(0x64E57, cluster_id, hash);
            }
        }
        assert!(
            asked > 0,
            "a peer holding a reference it cannot resolve must ask the room for the \
             content. Without this the picture never arrives by any route, and the \
             placeholder is the end state rather than a state on the way to a photograph",
        );

        while let Ok(cmd) = host_rx.try_recv() {
            let NetCmd::Publish { msg, .. } = cmd else { continue };
            if let NeighbourMsg::BlobChunk { hash, index, count, data, .. } = msg {
                guest.handle_blob_chunk_message(0x400D_5EED, hash, index, count, data);
            }
        }

        // --- the photograph is on the second machine ---------------------
        assert_eq!(
            guest_store.get(&photo_hash).as_deref(),
            Some(photo.as_slice()),
            "the photograph must be on the guest's machine byte-for-byte, not a \
             placeholder standing in for it",
        );
        // The descriptor the renderer sizes its placeholder from describes the
        // picture that just arrived, so the page does not reflow when the grey
        // box becomes a photograph.
        let meta = model::blob_meta(&guest_doc, &guest_c, &photo_hash)
            .expect("the guest's document must describe the content it references");
        assert_eq!((meta.mime.as_str(), meta.w, meta.h), ("image/png", 512, 320));
        assert_eq!(meta.len, photo.len() as u64);
    }

    /// Answering a request must not be gated on "already offered".
    ///
    /// The offer bookkeeping exists so a document edited a hundred times does
    /// not resend its photograph a hundred times. Letting it also suppress a
    /// *reply* would make every recoverable miss unrecoverable, because the
    /// peers that need the content most are exactly the ones an offer already
    /// went out without.
    #[test]
    fn a_peer_that_already_offered_content_still_answers_for_it() {
        use qdoc::blobstore::{BlobStore, MemoryBlobStore};

        const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000e";
        let content: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let hash = qdoc::blobstore::hash_bytes(&content);

        let store: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::default());
        assert!(store.put(&hash, &content));
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut rx) = shell_network_for_test_with_store(tree, 0xA11CE, Arc::clone(&store));

        // Offer it once, and consume everything that produced.
        assert!(net.offer_local_blob(ROOM, &hash, &content));
        let mut first = 0usize;
        while let Ok(cmd) = rx.try_recv() {
            if let NetCmd::Publish { msg: NeighbourMsg::BlobChunk { .. }, .. } = cmd {
                first += 1;
            }
        }
        assert!(first > 0, "the offer must have gone out");

        // Now a peer says it missed it.
        net.handle_blob_request_message(0xBEEF, ROOM.to_string(), hash.clone());
        let mut again = 0usize;
        while let Ok(cmd) = rx.try_recv() {
            if let NetCmd::Publish { msg: NeighbourMsg::BlobChunk { .. }, .. } = cmd {
                again += 1;
            }
        }
        assert_eq!(
            again, first,
            "a request must be answered in full even though the content was already \
             offered once — the asker is asking precisely because it did not get it",
        );
    }

    #[test]
    fn handle_app_crdt_message_buffers_for_drain() {
        // The inbound receive-side adapter pushes onto the buffer
        // the broker's drainer reads from. Verify all four fields
        // round-trip so the local fan-out can attribute payloads.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let buf_handle = net.inbound_app_crdt_handle();
        net.handle_app_crdt_message(
            7u64,
            "terminal".into(),
            "cluster-uuid".into(),
            "file:/work/notes.md".into(),
            vec![0x01, 0x02],
        );
        let buf = buf_handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].app_id, "terminal");
        assert_eq!(buf[0].cluster_id, "cluster-uuid");
        assert_eq!(buf[0].record_id, "file:/work/notes.md");
        assert_eq!(buf[0].data, vec![0x01, 0x02]);
    }

    /// Document why a real network test (two `ShellNetwork`s + a real
    /// libp2p mesh) isn't covered here:
    ///
    /// - `ShellNetwork::spawn` builds a tokio runtime and binds a
    ///   real TCP listener via `libp2p::SwarmBuilder`. Two parallel
    ///   spawns in the same test process compete for the same
    ///   ports unless we plumb `0.0.0.0:0` end-to-end (which the
    ///   current builder doesn't expose). The existing peer-sync
    ///   integration test (`tests/peer_sync_test.rs`) uses the
    ///   in-process `Hub` for the same reason.
    /// - The actor-level encoder/decoder loop already round-trips
    ///   every `NeighbourMsg` variant via `protocol::tests::*` (see
    ///   the `app_crdt_incremental_round_trips` test below the
    ///   enum) so the wire format itself is covered.
    /// - The replica/match-arm side of `AppCrdtIncremental` is a
    ///   no-op (the bridge code is in `ShellNetwork::tick` above
    ///   and routes into the inbound buffer), so the unit tests
    ///   above are sufficient to assert routing.
    ///
    /// A real two-host test belongs in the headless-integration
    /// harness (`tests/peer_sync_test.rs` follow-up) once that
    /// harness supports app-level CRDT injection. The two unit
    /// tests above are the contract for now.
    fn make_envelope(seq: u64) -> collab_types::SignedOp {
        use collab_types::{ArtifactId, AttestationId, Ed25519PublicKey, OpClass, SignedOp};
        use std::time::SystemTime;
        SignedOp::new_unsigned(
            vec![0xAA, 0xBB],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes([1u8; 32]),
            AttestationId::new(),
            OpClass::Splice,
            SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    #[test]
    fn publish_app_crdt_signed_emits_a_publish_command() {
        // Phase 3 outbound bridge: the broker forwards a validated
        // `SignedSyncIncremental` as a
        // `NeighbourMsg::AppCrdtIncrementalSigned`. Verify the wrapping
        // is correct (peer id stamped, envelope round-trips verbatim,
        // default workspace topic) without spinning up libp2p.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xFEED_u64);
        let envelope = make_envelope(7);
        net.publish_app_crdt_signed(
            "notes".to_string(),
            "cluster-7".to_string(),
            "uuid:9f".to_string(),
            envelope.clone(),
        );
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_app_crdt_signed must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::AppCrdtIncrementalSigned {
                        peer,
                        app_id,
                        cluster_id,
                        record_id,
                        envelope: got,
                    } => {
                        assert_eq!(peer, 0xFEED_u64);
                        // The route the receiving broker filters on
                        // has to survive the wrap verbatim, or the
                        // envelope arrives unaddressed and is dropped.
                        assert_eq!(app_id, "notes");
                        assert_eq!(cluster_id, "cluster-7");
                        assert_eq!(record_id, "uuid:9f");
                        assert_eq!(*got, envelope);
                    }
                    other => panic!("expected AppCrdtIncrementalSigned, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    #[test]
    fn handle_app_crdt_signed_message_buffers_for_drain() {
        // The inbound adapter pushes onto the signed buffer the broker
        // drainer reads from. Verify the entry preserves the peer hash,
        // the route the drainer delivers against, and the envelope
        // verbatim.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let buf_handle = net.inbound_app_crdt_signed_handle();
        let envelope = make_envelope(13);
        net.handle_app_crdt_signed_message(
            99u64,
            "notes".to_string(),
            "cluster-7".to_string(),
            "uuid:9f".to_string(),
            envelope.clone(),
        );
        let buf = buf_handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 99u64);
        assert_eq!(buf[0].app_id, "notes");
        assert_eq!(buf[0].cluster_id, "cluster-7");
        assert_eq!(buf[0].record_id, "uuid:9f");
        assert_eq!(*buf[0].envelope, envelope);
    }

    #[test]
    fn active_ws_for_publish_returns_default_topic() {
        // v1: publishes go to the single default workspace topic. This
        // contract is asserted explicitly so per-workspace routing
        // (Phase 3 follow-up) gets a tripping test rather than a
        // silent behaviour change.
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        assert_eq!(net.active_ws_for_publish(), DEFAULT_WS_TOPIC);
    }

    #[test]
    fn publish_scrollback_request_emits_a_publish_command() {
        use collab_types::ArtifactId;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xDEAD_u64);
        let artifact = ArtifactId::new();
        net.publish_scrollback_request(artifact, 100, 25);
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_scrollback_request must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, DEFAULT_WS_TOPIC);
                match msg {
                    NeighbourMsg::ScrollbackRequest {
                        artifact: got_art,
                        before_seq,
                        max,
                        requester,
                    } => {
                        assert_eq!(got_art, artifact);
                        assert_eq!(before_seq, 100);
                        assert_eq!(max, 25);
                        assert_eq!(requester, 0xDEAD_u64);
                    }
                    other => panic!("expected ScrollbackRequest, got {other:?}"),
                }
            }
            other => panic!("expected NetCmd::Publish, got {other:?}"),
        }
    }

    #[test]
    fn handle_scrollback_request_message_buffers_for_drain() {
        use collab_types::ArtifactId;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let handle = net.inbound_scrollback_request_handle();
        let artifact = ArtifactId::new();
        net.handle_scrollback_request_message(7u64, artifact, 50, 10);
        let buf = handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].requester, 7u64);
        assert_eq!(buf[0].artifact, artifact);
        assert_eq!(buf[0].before_seq, 50);
        assert_eq!(buf[0].max, 10);
    }

    #[test]
    fn handle_scrollback_response_message_buffers_for_drain() {
        use collab_types::ArtifactId;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        let handle = net.inbound_scrollback_response_handle();
        let artifact = ArtifactId::new();
        let env = Box::new(make_envelope(11));
        net.handle_scrollback_response_message(99u64, artifact, vec![env.clone()]);
        let buf = handle.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].responder, 99u64);
        assert_eq!(buf[0].artifact, artifact);
        assert_eq!(buf[0].ops.len(), 1);
        assert_eq!(*buf[0].ops[0], *env);
    }
}
