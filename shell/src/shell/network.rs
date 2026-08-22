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
/// truncated. The cap here sits below that ceiling with room for what
/// rides along with the payload: the bincode `NeighbourMsg` envelope,
/// the room uuid, a record id that may be a full path, and gossipsub's
/// own protobuf wrapper (topic, signature, author key, sequence
/// number). 48 KiB leaves 16 KiB for all of it, which is far more than
/// any of them need and is deliberately not tuned to the edge.
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
pub const MAX_APP_SNAPSHOT_BYTES: usize = 48 * 1024;

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
            inbound_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt_signed: Arc::new(Mutex::new(Vec::new())),
            inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt_snapshot: Arc::new(Mutex::new(Vec::new())),
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
            return false;
        };
        if room.is_nil() {
            return false;
        }
        let Ok(replica) = self.replica.lock() else {
            return false;
        };
        let in_room = replica.tree.read().rooms().contains(&room);
        drop(replica);
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
        let addr = self
            .dial_addr()
            .ok_or_else(|| "peer-sync is not listening yet; try again in a moment".to_string())?;
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
        crate::network::invite::make_invite_token(&self.keypair, ws, &addr, expiry_seconds_from_now)
            .map_err(|e| e.to_string())
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
        if let Ok(parsed) = crate::network::invite::validate_invite_token(token)
            && let Ok(addr) = parsed.owner_addr.parse::<libp2p::Multiaddr>()
        {
            let _ = self.cmd_tx.try_send(NetCmd::Connect { addr });
        }
        replica.request_sync();
        let outgoing = replica.drain_outgoing();
        drop(replica);
        for msg in outgoing {
            let _ = self.cmd_tx.try_send(NetCmd::Publish {
                ws: DEFAULT_WS_TOPIC,
                msg,
            });
        }
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
        for msg in outgoing {
            let _ = self.cmd_tx.try_send(NetCmd::Publish {
                ws: DEFAULT_WS_TOPIC,
                msg,
            });
        }
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
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
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
                    if peer_hash != self.local_peer_hash && self.is_in_room(cluster_id) {
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
                let mut rep = self.replica.lock().unwrap();
                // The peer's own identity, not the liveness handle's
                // digest of it: room replication decides from this
                // whether a payload may rewrite a room's governance.
                rep.handle_from(&crate::shell::state::PeerOrigin::peer(&peer), &msg);
                let outgoing = rep.drain_outgoing();
                drop(rep);
                for out_msg in outgoing {
                    let _ = self.cmd_tx.try_send(NetCmd::Publish {
                        ws: DEFAULT_WS_TOPIC,
                        msg: out_msg,
                    });
                }
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
        inbound_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt_signed: Arc::new(Mutex::new(Vec::new())),
        inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt_snapshot: Arc::new(Mutex::new(Vec::new())),
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
    fn the_snapshot_cap_leaves_room_under_the_gossipsub_ceiling() {
        // The cap is only meaningful if a message built at it still
        // fits the transport once the envelope, the routing strings and
        // gossipsub's own framing are added. Encode a worst-ish case —
        // a full-length record path — and check the encoded frame is
        // comfortably under 64 KiB rather than trusting the arithmetic
        // in the constant's doc comment.
        let msg = NeighbourMsg::AppCrdtSnapshot {
            peer: u64::MAX,
            app_id: "dev.qos.word-processor".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:".to_string() + &"a".repeat(4096),
            data: vec![0u8; MAX_APP_SNAPSHOT_BYTES],
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
