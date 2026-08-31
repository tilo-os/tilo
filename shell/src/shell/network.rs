//! Shell-side bridge into the peer-sync [`Actor`]: owns the
//! [`WorkspaceTreeReplica`], drives the actor's `tokio` runtime, and drains
//! `NetEvent`s into replica replies re-published as `NetCmd::Publish`s.
//! Wired in behind `--enable-peer-sync`.

use std::sync::{Arc, Mutex};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use tokio::sync::mpsc;

use crate::network::actor::{Actor, HostIdentity, NetCmd, NetEvent};
use crate::network::behaviour::load_or_create_keypair;
use crate::network::identity::PeerIdentityStore;
use crate::network::invite::accept_invite_into;
use crate::network::protocol::NeighbourMsg;
use crate::network::replica::WorkspaceTreeReplica;
#[cfg(test)]
use crate::shell::state::WorkspaceTree;
use crate::shell::state::{RoomId, WorkspaceTreeHandle};

/// Compatibility/control topic used only when a message has no room identity.
pub const DEFAULT_WS_TOPIC: u64 = 0;

/// Stable transport/host shard for a durable room. Local workspace handles are
/// deliberately not used: they differ between devices.
pub fn room_topic_id(room: RoomId) -> u64 {
    if room.is_nil() {
        return DEFAULT_WS_TOPIC;
    }
    // `room.to_string()` is `Uuid::to_string()`, which `room_workspace_id`
    // can always re-parse, so the `Err` path is unreachable in practice.
    // Fall back to the "no room identity" topic rather than panic on this
    // hot path — `room_workspace_id` never returns 0 on success, so a real
    // topic id can't collide with the fallback.
    qos_relay::room_workspace_id(&room.to_string()).unwrap_or(DEFAULT_WS_TOPIC)
}

/// Largest app-document snapshot this shell will put on the gossip
/// topic, in bytes.
///
/// `serde_json` writes a `Vec<u8>` as decimal numbers, so the 14 KiB
/// document cap can expand to roughly 56 KiB before postcard framing.
/// The Iroh actor accepts much larger messages, but this application-level
/// bound limits memory amplification and keeps snapshots interactive.
/// Past this size the carrier is a direct peer-to-peer stream via
/// [`crate::network::surface_stream`].
pub const MAX_APP_SNAPSHOT_BYTES: usize = 14 * 1024;

/// Default on-disk path of the machine's device key, from which the
/// shell derives its Iroh identity. The FUSE mount reads the same
/// file, so one device has one identity rather than one per subsystem.
pub const DEFAULT_KEYPAIR_PATH: &str = crate::network::behaviour::DEVICE_KEY_PATH;

/// [`DEFAULT_KEYPAIR_PATH`] unless `QOS_PEER_KEY_PATH` names somewhere
/// else. The override is what makes two sessions on one machine two
/// *peers*: the key is the shell's network identity, so two processes
/// reading one key file are one peer talking to itself.
pub fn peer_key_path() -> std::path::PathBuf {
    match std::env::var("QOS_PEER_KEY_PATH") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from(DEFAULT_KEYPAIR_PATH),
    }
}

fn parse_host_addr(value: &str) -> Option<iroh::EndpointAddr> {
    serde_json::from_str(value).ok().or_else(|| {
        value
            .parse::<iroh::EndpointId>()
            .ok()
            .map(iroh::EndpointAddr::new)
    })
}

fn configured_host_addrs() -> Vec<iroh::EndpointAddr> {
    let mut hosts = Vec::new();
    if let Ok(value) = std::env::var("QOS_COLLAB_HOSTS") {
        match serde_json::from_str::<Vec<iroh::EndpointAddr>>(&value) {
            Ok(parsed) => hosts.extend(parsed),
            Err(error) => {
                tracing::warn!(%error, "QOS_COLLAB_HOSTS must be a JSON array of Iroh endpoint addresses");
            }
        }
    }
    if let Ok(value) = std::env::var("QOS_COLLAB_HOST") {
        if let Some(addr) = parse_host_addr(&value) {
            hosts.push(addr);
        } else {
            tracing::warn!("QOS_COLLAB_HOST is not a valid Iroh endpoint address or id");
        }
    }
    hosts.sort_by_key(|addr| addr.id.to_string());
    hosts.dedup_by_key(|addr| addr.id);
    hosts
}

/// How often a shell asks the rooms it is in for anything it is
/// missing. Room replication has no push side, so this interval is the
/// upper bound on how stale a guest's view of a room can be.
const SYNC_KICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Shell-side handle for the peer-sync subsystem. Owns the tokio
/// runtime hosting the [`Actor`], the per-workspace
/// [`WorkspaceTreeReplica`], and the actor↔replica event channel.
pub struct ShellNetwork {
    /// Send commands into the actor (e.g. publish a `NeighbourMsg`).
    pub cmd_tx: mpsc::Sender<NetCmd>,
    /// Shared replica owning the tree-side reactions to `NetEvent::Msg`;
    /// `Arc<Mutex<_>>` so other tasks can share a clone.
    pub replica: Arc<Mutex<WorkspaceTreeReplica>>,
    /// Sender half of the actor→replica event channel, cloned into
    /// `NetCmd::JoinWorkspace`.
    pub event_tx: mpsc::Sender<NetEvent>,
    /// Receiver half of the actor→replica event channel, drained on
    /// each [`Self::tick`]. `Mutex`-wrapped so `tick` can take `&self`
    /// while shared behind `Arc<ShellNetwork>`.
    pub event_rx: Mutex<mpsc::Receiver<NetEvent>>,
    /// Topics already subscribed in the actor. Avoids sending a Join command on
    /// every app delta while still allowing rooms accepted after startup.
    joined_topics: Mutex<std::collections::HashSet<u64>>,
    /// Tokio runtime hosting the actor. `Option` so `drop` can `take`
    /// it and shut down without blocking the test thread.
    runtime: Option<tokio::runtime::Runtime>,
    /// Cached hash of the local iroh endpoint id, stamped on every
    /// outbound `NeighbourMsg::PresenceMoved`. Computed once at spawn.
    local_peer_hash: u64,
    /// This device's iroh identity. An invite is a signed statement
    /// by the inviting owner — [`Self::issue_invite`] signs with it.
    keypair: iroh::SecretKey,
    /// Endpoint id derived from [`Self::keypair`].
    local_peer_id: iroh::EndpointId,
    /// Current direct/relay dialing addresses reported by iroh.
    listen_addrs: Arc<Mutex<Vec<iroh::EndpointAddr>>>,
    /// Always-on hosts included in newly issued invites.
    host_addrs: Mutex<Vec<iroh::EndpointAddr>>,
    /// When [`Self::kick_periodic_sync`] last asked. `None` until the
    /// first tick, so the first frame after startup kicks a round.
    last_sync_kick: Mutex<Option<std::time::Instant>>,
    /// Verified transport-device chains received on workspace topics.
    peer_identities: PeerIdentityStore,
    /// Production is fail-closed until a peer presents `IdentityHello`.
    /// The in-process test bridge disables this because it has no Iroh actor.
    require_peer_identity: bool,
    /// Names this machine has promised to whoever redeems the invite it
    /// last minted for a room, keyed by that room — held here rather
    /// than in the token because the token is a bearer credential that
    /// crosses machines, so a name inside it would be a name its
    /// *holder* presents, which `apply_room_changes` refuses.
    /// [`Self::name_invitee_on_arrival`] writes it once the invitee's
    /// `PeerId` is known.
    pending_invitees: Mutex<std::collections::HashMap<crate::shell::state::RoomId, String>>,
    /// Invite redemptions awaiting a certificate-bearing person principal.
    /// Unlike the display-name map, unnamed invites also enter this table.
    pending_identity_admissions:
        Mutex<std::collections::HashMap<crate::shell::state::RoomId, String>>,
    /// Inbound presence updates pushed by [`Self::tick`], drained by the
    /// shell's frame-tick into `PresenceCtx::remote_cursors`.
    inbound_presence: Arc<Mutex<Vec<InboundPresence>>>,
    /// Inbound app-CRDT increments pushed by [`Self::tick`], fanned by
    /// the `collab_broker` drainer to every locally-connected app.
    inbound_app_crdt: Arc<Mutex<Vec<InboundAppCrdt>>>,
    /// Inbound *signed* app-CRDT envelopes pushed by [`Self::tick`].
    /// The `collab_broker` drainer runs each through `validate_inbound_op`
    /// before fanning the payload out.
    inbound_app_crdt_signed: Arc<Mutex<Vec<InboundAppCrdtSigned>>>,
    /// Received app-presence frames awaiting fan-out to local apps.
    inbound_app_presence: Arc<Mutex<Vec<InboundAppPresence>>>,
    /// Received app-CRDT *snapshots* awaiting fan-out to local apps.
    /// Separate from [`Self::inbound_app_crdt`]: an increment is applied
    /// to a document that already holds its history, a snapshot *is*
    /// that history, and the receiving app folds them in differently.
    inbound_app_crdt_snapshot: Arc<Mutex<Vec<InboundAppCrdt>>>,
    /// Rooms with an outstanding request for somebody's whole document,
    /// keyed by cluster id. Entered when a peer announces arrival,
    /// removed when a snapshot is published for it.
    snapshot_requests: Arc<Mutex<std::collections::HashMap<String, SnapshotWindow>>>,
    /// Inbound cooperative-scrollback *requests*; the broker's drainer
    /// answers any it holds matching ops for.
    inbound_scrollback_request: Arc<Mutex<Vec<InboundScrollbackRequest>>>,
    /// Inbound cooperative-scrollback *responses*, forwarded by the
    /// broker's drainer to the local app that asked.
    inbound_scrollback_response: Arc<Mutex<Vec<InboundScrollbackResponse>>>,
}

/// One received app-CRDT incremental update, queued by
/// [`ShellNetwork::tick`] for the `collab_broker` drainer to fan out
/// to every locally-connected app.
#[derive(Debug, Clone)]
pub struct InboundAppCrdt {
    /// Hashed Iroh peer id of the publisher; the broker's fan-out
    /// routes on `app_id` + `cluster_id` rather than on this.
    pub peer: u64,
    /// Originating app id (e.g. `"terminal"`); first half of the
    /// broker's delivery filter.
    pub app_id: String,
    /// Originating cluster id (uuid as string); second half of the
    /// delivery filter.
    pub cluster_id: String,
    /// Originating record id — which document these changes belong to.
    /// Empty for a sender with one document per `(app, cluster)`.
    pub record_id: String,
    /// Opaque Automerge incremental change blob, passed through as a
    /// `CollabEvent::CRDTIncremental`.
    pub data: Vec<u8>,
}

/// One received app-presence frame, fanned to local apps by the
/// `collab_broker` drainer. Carries no record id: presence is fanned
/// at cluster scope.
#[derive(Debug, Clone)]
pub struct InboundAppPresence {
    /// Hashed Iroh peer id of the publisher. Unlike the CRDT queues
    /// this one is load-bearing: it becomes the `user_id` the
    /// receiving app attributes the caret and its colour to.
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub presence: collab_types::PeerPresence,
}

/// One received *signed* app-CRDT envelope. Distinct from
/// [`InboundAppCrdt`] so a malformed envelope can never poison the
/// unsigned fan-out path, and vice versa.
#[derive(Debug, Clone)]
pub struct InboundAppCrdtSigned {
    pub peer: u64,
    /// Empty (a peer too old to stamp one) addresses nobody.
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    /// Boxed to mirror the wire shape and bound the queue entry size —
    /// `SignedOp` is large (32-byte pubkey + 64-byte sig + payload + UUIDs).
    pub envelope: Box<collab_types::SignedOp>,
}

/// One received cooperative-scrollback request. The broker's drainer
/// may answer with a `ScrollbackResponse` if its op log holds the range.
#[derive(Debug, Clone)]
pub struct InboundScrollbackRequest {
    /// Hashed peer id of the asker; used to address the reply.
    pub requester: u64,
    pub artifact: collab_types::ArtifactId,
    /// Exclusive upper bound on `seq`.
    pub before_seq: u64,
    pub max: u16,
}

/// One received cooperative-scrollback response, translated by the
/// broker's drainer into a `CollabEvent::ScrollbackBatch`.
#[derive(Debug, Clone)]
pub struct InboundScrollbackResponse {
    pub responder: u64,
    pub artifact: collab_types::ArtifactId,
    pub ops: Vec<collab_types::SignedOp>,
}

/// One received cursor-presence update, merged by the shell's
/// frame drainer into `PresenceCtx`.
#[derive(Debug, Clone)]
pub struct InboundPresence {
    pub peer: u64,
    /// Who the publishing session says is sitting at it. Empty when it
    /// did not say; the drainer falls back to the session handle.
    pub user: String,
    /// Presence channel key (workspace, tab, widget path, …), JSON so
    /// the wire format stays opaque to the bridge.
    pub key: String,
    /// Pointer position in the publisher's logical-pixel scene-space.
    pub point: (f64, f64),
    /// Wall-clock millisecond timestamp from the publisher.
    pub ts: u64,
}

/// Short window opened by one peer arrival. Every distinct document in the
/// room may publish one baseline; repeat snapshots from the same document are
/// suppressed so ordinary commits cannot turn into whole-state broadcasts.
struct SnapshotWindow {
    opened: std::time::Instant,
    released: std::collections::HashSet<(String, String)>,
}

/// Start mDNS LAN peer discovery when `QOS_COLLAB_LAN_DISCOVERY` requests
/// it, registering it with `endpoint`'s address lookup. Returns `None`
/// when discovery is disabled, disallowed by the env var, or the mDNS
/// service or address-lookup registry is unavailable — the caller then
/// simply skips [`spawn_lan_discovery_forwarding`].
fn init_lan_discovery(
    endpoint: &iroh::Endpoint,
) -> Option<iroh_mdns_address_lookup::MdnsAddressLookup> {
    let lan_discovery = std::env::var_os("QOS_COLLAB_LAN_DISCOVERY")
        .is_some_and(|value| value != "0" && !value.is_empty());
    if !lan_discovery {
        return None;
    }
    let mdns = match iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .service_name("qos-collab-v1")
        .build(endpoint.id())
    {
        Ok(mdns) => mdns,
        Err(error) => {
            tracing::warn!(%error, "LAN discovery requested but could not start");
            return None;
        }
    };
    let Ok(address_lookup) = endpoint.address_lookup() else {
        tracing::warn!("LAN discovery requested but endpoint has no address lookup registry");
        return None;
    };
    address_lookup.add(mdns.clone());
    Some(mdns)
}

/// Forward every mDNS-discovered peer into `cmd_tx` as a
/// `NetCmd::Connect`, on a task spawned onto `runtime`.
fn spawn_lan_discovery_forwarding(
    runtime: &tokio::runtime::Runtime,
    mdns: iroh_mdns_address_lookup::MdnsAddressLookup,
    cmd_tx: &mpsc::Sender<NetCmd>,
) {
    let discovery_tx = cmd_tx.clone();
    runtime.spawn(async move {
        use futures_lite::StreamExt;
        let mut events = mdns.subscribe().await;
        while let Some(event) = events.next().await {
            if let iroh_mdns_address_lookup::DiscoveryEvent::Discovered { endpoint_info, .. } =
                event
            {
                let _ = discovery_tx
                    .send(NetCmd::Connect {
                        addr: endpoint_info.into_endpoint_addr(),
                    })
                    .await;
            }
        }
    });
    tracing::info!(
        "peer-sync: privacy-sensitive LAN discovery enabled; advertising on _qos-collab-v1._udp.local"
    );
}

impl ShellNetwork {
    /// Build a tokio runtime, spawn the iroh [`Actor`], wrap the
    /// shared [`WorkspaceTree`] in a [`WorkspaceTreeReplica`], and
    /// return a handle the shell can use to drive sync. The keypair is
    /// loaded if it already exists, otherwise a fresh ed25519 key is
    /// generated and persisted.
    ///
    /// # Errors
    ///
    /// Returns an error if the tokio runtime fails to build, the device
    /// keypair can't be loaded or generated at `keypair_path`, or the
    /// Iroh endpoint fails to bind.
    pub fn spawn(
        tree: WorkspaceTreeHandle,
        keypair_path: &std::path::Path,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let _enter = runtime.enter();

        let keypair = load_or_create_keypair(keypair_path)?;
        let local_peer_id = keypair.public();
        // Must use the same hash function `tick`'s receive path uses,
        // so a peer's own publishes hash to the same `peer` u64 on
        // both sides of the wire.
        let local_peer_hash = crate::network::behaviour::peer_liveness_handle(&keypair.public());
        let lookup = MemoryLookup::new();
        let endpoint = runtime
            .block_on(
                iroh::Endpoint::builder(presets::N0)
                    .secret_key(keypair.clone())
                    .address_lookup(lookup.clone())
                    .bind(),
            )
            .map_err(|e| std::io::Error::other(format!("iroh bind: {e}")))?;
        let mdns = init_lan_discovery(&endpoint);
        tracing::info!(
            local_peer_id = %local_peer_id,
            addr = ?endpoint.addr(),
            "peer-sync: Iroh endpoint started"
        );
        let listen_addrs: Arc<Mutex<Vec<iroh::EndpointAddr>>> = Arc::new(Mutex::new(Vec::new()));
        let host_identity = host_identity_for_key(&keypair, keypair_path);
        let (cmd_tx, _join) = Actor::spawn_reporting_addrs(
            endpoint,
            lookup,
            host_identity,
            Arc::clone(&listen_addrs),
        );
        if let Some(mdns) = mdns {
            spawn_lan_discovery_forwarding(&runtime, mdns, &cmd_tx);
        }

        let host_addrs = configured_host_addrs();
        for addr in host_addrs.iter().cloned() {
            let _ = cmd_tx.try_send(NetCmd::ConnectHost { addr });
        }

        // Peer id `1` is the replica-side identity for
        // `WorkspaceTreeReplica::handle`'s `_sender` arg; the actor's
        // Iroh `EndpointId` is transport-level and hashed separately.
        let replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree)));

        // Capacity matches `Actor::spawn`'s command-channel default.
        let (event_tx, event_rx) = mpsc::channel(64);

        Ok(Self {
            cmd_tx,
            replica,
            event_tx,
            event_rx: Mutex::new(event_rx),
            joined_topics: Mutex::new(std::collections::HashSet::new()),
            runtime: Some(runtime),
            local_peer_hash,
            keypair,
            local_peer_id,
            listen_addrs,
            host_addrs: Mutex::new(host_addrs),
            last_sync_kick: Mutex::new(None),
            peer_identities: PeerIdentityStore::new(),
            require_peer_identity: true,
            pending_invitees: Mutex::new(std::collections::HashMap::new()),
            pending_identity_admissions: Mutex::new(std::collections::HashMap::new()),
            inbound_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt_signed: Arc::new(Mutex::new(Vec::new())),
            inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
            inbound_app_crdt_snapshot: Arc::new(Mutex::new(Vec::new())),
            snapshot_requests: Arc::new(Mutex::new(std::collections::HashMap::new())),
            inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
            inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Pre-computed hash of the local iroh endpoint id. Stamped on every
    /// outbound presence move so peers can attribute the cursor to its
    /// publisher.
    pub fn local_peer_id_hash(&self) -> u64 {
        self.local_peer_hash
    }

    /// What the peers in `cluster_id` are called, keyed by the peer
    /// handle presence frames carry. `cluster_id` is a room id, so this
    /// is the room document's answer and nobody else's — names are
    /// written there by the room's authority and refused from anyone
    /// else. Empty for a cluster that is not a room this machine is in,
    /// or a room where nobody has been named; both leave every peer
    /// labelled with its own handle.
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

        replica.tree.read().room_peer_labels(room)
    }

    /// Workspace topic for session traffic associated with the active room.
    pub fn active_ws_for_publish(&self) -> u64 {
        let Ok(replica) = self.replica.lock() else {
            return DEFAULT_WS_TOPIC;
        };
        replica
            .tree
            .read()
            .active_workspace()
            .map_or(DEFAULT_WS_TOPIC, |workspace| room_topic_id(workspace.room))
    }

    fn explicit_topic_for_message(msg: &NeighbourMsg) -> Option<u64> {
        let room = match msg {
            NeighbourMsg::SyncReq { room, .. } | NeighbourMsg::SyncResp { room, .. } => {
                Some(room.as_str())
            }
            NeighbourMsg::AppCrdtIncremental { cluster_id, .. }
            | NeighbourMsg::AppCrdtIncrementalSigned { cluster_id, .. }
            | NeighbourMsg::AppPresenceV2 { cluster_id, .. }
            | NeighbourMsg::AppCrdtSnapshot { cluster_id, .. } => Some(cluster_id.as_str()),
            _ => None,
        };
        room.and_then(RoomId::parse)
            .filter(|room| !room.is_nil())
            .map(room_topic_id)
    }

    fn topic_for_message(&self, msg: &NeighbourMsg) -> u64 {
        Self::explicit_topic_for_message(msg).unwrap_or_else(|| self.active_ws_for_publish())
    }

    fn join_topic(&self, ws: u64) -> bool {
        let Ok(mut joined) = self.joined_topics.lock() else {
            return false;
        };
        if !joined.insert(ws) {
            return true;
        }
        if self
            .cmd_tx
            .try_send(NetCmd::JoinWorkspace {
                ws,
                on_event: self.event_tx.clone(),
            })
            .is_err()
        {
            joined.remove(&ws);
            return false;
        }
        true
    }

    fn publish_message(&self, msg: NeighbourMsg) -> bool {
        let ws = self.topic_for_message(&msg);
        self.join_topic(ws) && self.cmd_tx.try_send(NetCmd::Publish { ws, msg }).is_ok()
    }

    fn publish_shell_checkpoint(&self, msg: NeighbourMsg) -> bool {
        let ws = self.topic_for_message(&msg);
        self.join_topic(ws)
            && self
                .cmd_tx
                .try_send(NetCmd::PublishCheckpoint { ws, msg })
                .is_ok()
    }

    /// Shared handle to the inbound-presence buffer. The shell's
    /// per-frame drainer swaps the `Vec` out under the lock and writes
    /// each entry into `PresenceCtx::remote_cursors`.
    pub fn inbound_presence_handle(&self) -> Arc<Mutex<Vec<InboundPresence>>> {
        Arc::clone(&self.inbound_presence)
    }

    /// Forward a local-cursor presence move to the active workspace's
    /// peers. Throttling lives upstream (`pointer_broadcast_allowed`).
    /// `try_send` failures are swallowed — presence is best-effort and
    /// a momentary backlog should never block the render loop.
    pub fn publish_presence(&self, key: String, point: (f64, f64)) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let msg = NeighbourMsg::PresenceMoved {
            peer: self.local_peer_id_hash(),
            user: crate::shell::state::local_user_id(),
            key,
            point,
            ts,
        };
        let _ = self.publish_message(msg);
    }

    /// Queue a received presence move; public so tests can inject one
    /// without going through gossip.
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

    /// Shared handle to the inbound-app-CRDT buffer, fanned to every
    /// locally-connected app by the `collab_broker` drainer.
    pub fn inbound_app_crdt_handle(&self) -> Arc<Mutex<Vec<InboundAppCrdt>>> {
        Arc::clone(&self.inbound_app_crdt)
    }

    /// Publish a `NeighbourMsg::AppCrdtIncremental` to the default
    /// workspace topic. `try_send` failures are swallowed — CRDT
    /// updates are idempotent and the next `SyncIncremental` republishes.
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
        let _ = self.publish_message(msg);
    }

    /// Is `cluster_id` a room this machine is in? An app's cluster is
    /// the room's uuid, so this is the same question `apply_room_changes`
    /// asks before storing anything. A `cluster_id` that fails to parse
    /// as a uuid is not a room, and refusing it is fail-closed.
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
            known = ?known.iter().map(std::string::ToString::to_string).collect::<Vec<_>>(),
            "PROBE is_in_room",
        );
        in_room
    }

    /// Queue a received `AppCrdtIncremental` for the broker's drainer.
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

    /// Shared handle to the inbound-signed-app-CRDT buffer. The broker
    /// drainer validates each envelope and fans it out as
    /// `CollabEvent::SignedCRDTIncremental`.
    pub fn inbound_app_crdt_signed_handle(&self) -> Arc<Mutex<Vec<InboundAppCrdtSigned>>> {
        Arc::clone(&self.inbound_app_crdt_signed)
    }

    /// Publish a `NeighbourMsg::AppCrdtIncrementalSigned` to the
    /// default workspace topic. Called by the broker for every
    /// validated `CollabRequest::SignedSyncIncremental`.
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
        let _ = self.publish_message(msg);
    }

    /// Queue a received `AppCrdtIncrementalSigned` for the broker's drainer.
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

    /// Publish an app's presence frame to the peers in its room —
    /// without this a peer's caret never reaches another machine, since
    /// the broker's local fan-out only reaches apps on *this* host.
    pub fn publish_app_presence(
        &self,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        let msg = NeighbourMsg::AppPresenceV2 {
            peer: self.local_peer_id_hash(),
            app_id,
            cluster_id,
            presence,
        };
        let _ = self.publish_message(msg);
    }

    /// Shared handle to the inbound app-presence buffer, drained by the
    /// broker and fanned to local apps as `RemotePresenceUpdate`.
    pub fn inbound_app_presence_handle(&self) -> Arc<Mutex<Vec<InboundAppPresence>>> {
        Arc::clone(&self.inbound_app_presence)
    }

    pub fn handle_typed_app_presence_message(
        &self,
        peer: u64,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        if let Ok(mut buf) = self.inbound_app_presence.lock() {
            buf.push(InboundAppPresence {
                peer,
                app_id,
                cluster_id,
                presence,
            });
        }
    }

    /// Publish one document's whole state so a peer that has just joined
    /// can adopt a document it holds no history for. Refuses above
    /// [`MAX_APP_SNAPSHOT_BYTES`] to bound serialization and fan-out memory —
    /// snapshots reach the wire through here, so the cap is enforced once.
    /// Refusal logs at `warn`: the consequence
    /// (this document does not converge) has no other symptom.
    pub fn publish_app_crdt_snapshot(
        &self,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
        if !self.take_snapshot_request(&cluster_id, &app_id, &record_id) {
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
                "shell_network: refusing to publish an app snapshot over the application cap; this document will not converge for peers that lack its history",
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
        let _ = self.publish_message(msg);
    }

    /// Record that somebody in `cluster_id` needs a whole document —
    /// called on arrival, the only thing that makes a snapshot worth its
    /// size on a broadcast topic. `CollabRequest::SyncCRDT` is not
    /// reliably a join reply (same-host fan-out and undo send it too),
    /// so arming on `Arrived` and firing on the next whole document
    /// keeps the rate bounded whichever source supplies it.
    pub fn note_snapshot_request(&self, cluster_id: &str) {
        if cluster_id.is_empty() {
            return;
        }
        if let Ok(mut pending) = self.snapshot_requests.lock() {
            pending.insert(
                cluster_id.to_string(),
                SnapshotWindow {
                    opened: std::time::Instant::now(),
                    released: std::collections::HashSet::new(),
                },
            );
        }
    }

    /// Take the outstanding snapshot request for `cluster_id`, if any.
    fn take_snapshot_request(&self, cluster_id: &str, app_id: &str, record_id: &str) -> bool {
        let Ok(mut pending) = self.snapshot_requests.lock() else {
            return false;
        };
        let Some(window) = pending.get_mut(cluster_id) else {
            return false;
        };
        if window.opened.elapsed() > std::time::Duration::from_secs(5) {
            pending.remove(cluster_id);
            return false;
        }
        window
            .released
            .insert((app_id.to_string(), record_id.to_string()))
    }

    /// Shared handle to the inbound app-snapshot buffer, drained by the
    /// broker and fanned to local apps as `CollabEvent::CRDTUpdate`.
    pub fn inbound_app_crdt_snapshot_handle(&self) -> Arc<Mutex<Vec<InboundAppCrdt>>> {
        Arc::clone(&self.inbound_app_crdt_snapshot)
    }

    /// Queue a received `AppCrdtSnapshot` for the broker's drainer.
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

    /// Shared handle to the cooperative-scrollback request buffer,
    /// answered by the broker's drainer on each tick.
    pub fn inbound_scrollback_request_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackRequest>>> {
        Arc::clone(&self.inbound_scrollback_request)
    }

    /// Shared handle to the cooperative-scrollback response buffer.
    pub fn inbound_scrollback_response_handle(&self) -> Arc<Mutex<Vec<InboundScrollbackResponse>>> {
        Arc::clone(&self.inbound_scrollback_response)
    }

    /// Publish a `NeighbourMsg::ScrollbackRequest`. Called when a local
    /// app's `FetchScrollback` returned `exhausted=true` with an empty
    /// batch — the broker has nothing more in-memory, but the app still
    /// wants the range.
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
        let _ = self.publish_message(msg);
    }

    /// Publish a `NeighbourMsg::ScrollbackResponse` for an artifact
    /// whose log has matching ops.
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
        let _ = self.publish_message(msg);
    }

    /// Queue a received `ScrollbackRequest` for the broker's drainer.
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

    /// Queue a received `ScrollbackResponse` for the broker's drainer.
    pub fn handle_scrollback_response_message(
        &self,
        responder: u64,
        artifact: collab_types::ArtifactId,
        ops: Vec<collab_types::SignedOp>,
    ) {
        if let Ok(mut buf) = self.inbound_scrollback_response.lock() {
            buf.push(InboundScrollbackResponse {
                responder,
                artifact,
                ops,
            });
        }
    }

    /// Subscribe the actor to every durable room currently in the tree, plus
    /// the compatibility/control topic. Rooms accepted later are joined by
    /// [`Self::publish_message`].
    pub fn join_default_workspace(&self) -> bool {
        let mut topics = vec![DEFAULT_WS_TOPIC];
        if let Ok(replica) = self.replica.lock() {
            topics.extend(replica.tree.read().rooms().into_iter().map(room_topic_id));
        }
        topics.into_iter().all(|topic| self.join_topic(topic))
    }

    /// Attach an always-on home or hosted workspace server. The actor keeps
    /// its direct Iroh gossip path and additionally uses the authenticated
    /// host session for offline store-and-forward.
    pub fn connect_host(&self, addr: iroh::EndpointAddr) -> bool {
        if let Ok(mut hosts) = self.host_addrs.lock()
            && !hosts.iter().any(|known| known.id == addr.id)
        {
            hosts.push(addr.clone());
        }
        self.cmd_tx.try_send(NetCmd::ConnectHost { addr }).is_ok()
    }

    /// The current iroh address published in an invite. It contains the
    /// stable endpoint id plus fresh direct/relay dialing hints.
    pub fn dial_addr(&self) -> Option<iroh::EndpointAddr> {
        let addrs = self.listen_addrs.lock().ok()?;
        addrs.first().cloned()
    }

    /// Mint a signed invite to the workspace `local`, valid for
    /// `expiry_seconds_from_now`. Signs with this shell's own Iroh
    /// key and embeds the address the endpoint actually bound, so the
    /// token a user copies is one a peer can act on.
    ///
    /// # Errors
    ///
    /// Returns an error (as a message) if peer-sync has no dial address
    /// yet, `local` names no known workspace, or the replica lock is
    /// poisoned.
    pub fn issue_invite(
        &self,
        local: crate::shell::state::WorkspaceId,
        expiry_seconds_from_now: u64,
    ) -> Result<String, String> {
        self.issue_invite_for(local, expiry_seconds_from_now, None)
    }

    /// [`Self::issue_invite`], for a person this machine will name.
    /// `invitee` does not go into the token (see
    /// [`Self::pending_invitees`]) and is held until that peer turns up.
    /// A second invite for a room replaces the name waiting on it.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::issue_invite`].
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
        let (token, room) = {
            let ws = {
                let replica = self
                    .replica
                    .lock()
                    .map_err(|_| "peer-sync replica lock poisoned".to_string())?;
                let tree = replica.tree.read();
                tree.workspaces
                    .iter()
                    .find(|w| w.id == local)
                    .ok_or_else(|| format!("no workspace {} to invite into", local.raw()))?
                    .clone()
            };
            // Clone `ws` out and drop both guards above before the
            // (unrelated) hosts lock and the invite-signing work below —
            // neither needs the replica/tree lock held.
            let hosts = self
                .host_addrs
                .lock()
                .map(|hosts| hosts.clone())
                .unwrap_or_default();
            let device_cert = selected_device_cert(ws.room, &self.keypair)?;
            let token = crate::network::invite::make_invite_token_with_identity(
                &self.keypair,
                &ws,
                &addr,
                &hosts,
                Some(device_cert),
                expiry_seconds_from_now,
            )
            .map_err(|e| e.to_string())?;
            (token, ws.room)
        };

        // Nobody invites the inviter, so without this the host would be
        // the one peer left unnamed. Self-declared, but through the same
        // authority-checked channel every other name travels on, so it
        // can't be forged. Best-effort: a non-administering machine has
        // the write refused and stays an id rather than failing the invite.
        if let Ok(replica) = self.replica.lock() {
            let me = crate::shell::state::local_user_id_for_room(&room);
            let peer = self.local_peer_id.to_string();
            replica.tree.write().name_peer(room, &me, &peer, &me);
        }
        self.join_topic(room_topic_id(room));

        // And park the invitee's name against the room, for
        // `name_invitee_on_arrival` to write when they turn up.
        let invitee = invitee.map(str::trim).filter(|name| !name.is_empty());
        if let Some(invitee) = invitee
            && let Ok(mut pending) = self.pending_invitees.lock()
        {
            pending.insert(room, invitee.to_string());
        }
        if let Ok(mut pending) = self.pending_identity_admissions.lock() {
            pending.insert(room, invitee.unwrap_or("Member").to_string());
        }
        Ok(token)
    }

    /// Name the peer that just redeemed this room's invite. `peer` is
    /// the *authenticated* author — gossip runs
    /// `MessageAuthenticity::Signed` with `ValidationMode::Strict`, so
    /// it signed what arrived rather than merely forwarding it.
    ///
    /// Three guards close off naming the wrong person: never this
    /// machine, never a peer the room has already named, and only if
    /// `name_peer` accepts the write. The pending name is consumed by
    /// the first peer to redeem it, so a second arrival keeps its own id.
    fn name_invitee_on_arrival(&self, peer: &iroh::EndpointId, room: &str) {
        if *peer == self.local_peer_id {
            return;
        }
        let Some(room) = crate::shell::state::RoomId::parse(room) else {
            return;
        };
        let Ok(mut pending) = self.pending_invitees.lock() else {
            return;
        };
        let Some(name) = pending.get(&room).cloned() else {
            return;
        };
        let endpoint_id = peer.to_string();
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        if let Some(existing) = tree
            .room_peer_names
            .get(&room)
            .and_then(|n| n.get(&endpoint_id))
        {
            // Already named — but if it's the name that was waiting,
            // this is the peer the invite was for. Spend the pending
            // copy rather than leave it lying around.
            if existing == &name {
                pending.remove(&room);
            }
            return;
        }
        let me = crate::shell::state::local_user_id();
        if tree.name_peer(room, &me, &endpoint_id, &name) {
            tracing::info!(
                peer = %endpoint_id,
                %name,
                %room,
                "peer-sync: named the invitee that arrived"
            );
            pending.remove(&room);
        }
    }

    /// Accept an invite token into `local`, then immediately ask the
    /// room it named to catch us up — replication is per room, so
    /// without that request a freshly accepted invite sits idle until
    /// something else kicks a round. Takes the token *string*, not a
    /// parsed token, so validation cannot be skipped on the way in.
    ///
    /// # Errors
    ///
    /// Returns an error (as a message) if the replica lock is poisoned
    /// or `token` fails to parse/validate.
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
        if !self.join_topic(room_topic_id(room)) {
            return Err("peer-sync command queue is full; try accepting the invite again".into());
        }
        // Dial the authenticated address carried by the invite.
        // `validate_invite_token` already ran inside `accept_invite_into`,
        // so re-reading the token here cannot let an unverified address through.
        if let Ok(parsed) = crate::network::invite::validate_invite_token(token) {
            let _ = self.cmd_tx.try_send(NetCmd::Connect {
                addr: parsed.owner_addr,
            });
            for host in parsed.host_addrs {
                let _ = self.connect_host(host);
            }
        }
        replica.request_sync();
        let outgoing = replica.drain_outgoing();
        drop(replica);
        for msg in outgoing {
            let _ = self.publish_message(msg);
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
            let _ = self.publish_message(msg);
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

    /// Drain pending [`NetEvent`]s from the actor, forward `Msg`
    /// variants into the replica's `handle`, and publish each resulting
    /// outbound message via `cmd_tx`. Non-blocking — never stalls the
    /// render loop even on a starved channel.
    pub fn tick(&self) {
        // Room replication is pull-only — a host answers `SyncReq` and
        // never announces — so nothing else brings a stale guest's view
        // of a capability change up to date.
        self.kick_periodic_sync();
        // Hold the lock for the whole drain so a concurrent frame-tick
        // never sees half-merged events. A poisoned mutex (a prior
        // holder panicked mid-drain) just skips this tick rather than
        // panicking this one too.
        let Ok(mut event_rx) = self.event_rx.lock() else {
            return;
        };
        while let Ok(event) = event_rx.try_recv() {
            self.dispatch_net_event(event);
        }
    }

    /// Route one drained [`NetEvent`] to its handler. Split out of
    /// [`Self::tick`], which owns only the drain loop and the
    /// `event_rx` lock.
    fn dispatch_net_event(&self, event: NetEvent) {
        match event {
            NetEvent::HostReady { ws, peer } => self.handle_host_ready(ws, peer),
            NetEvent::PeerJoined { .. } => self.publish_sync_requests(),
            // Without this the roster only ever grows: nothing else
            // tells it a session went quiet.
            NetEvent::PeerLeft { peer, .. } => {
                let handle = crate::network::behaviour::peer_liveness_handle(&peer);
                self.peer_identities
                    .remove(&crate::shell::state::PeerOrigin::peer(&peer));
                if let Ok(replica) = self.replica.lock() {
                    replica.tree.write().retire_session(handle);
                }
            }
            NetEvent::Msg { ws, peer, msg } => self.dispatch_neighbour_msg(ws, peer, msg),
        }
    }

    /// Seed a durable host's checkpoint once it finishes subscribing to
    /// `ws` — see [`NetEvent::HostReady`]. Unlike `SyncReq`, this does
    /// not need another device online to answer it.
    fn handle_host_ready(&self, ws: u64, peer: iroh::EndpointId) {
        let room = self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .rooms()
                .into_iter()
                .find(|room| room_topic_id(*room) == ws)
        });
        let Some(room) = room else {
            return;
        };
        // Seed a complete shell-room checkpoint.
        let changes = self
            .replica
            .lock()
            .ok()
            .and_then(|replica| replica.tree.write().export_room_changes_since(room, &[]))
            .unwrap_or_default();
        if !changes.is_empty() {
            let _ = self.publish_shell_checkpoint(NeighbourMsg::SyncResp {
                changes,
                room: room.to_string(),
            });
        }

        // Apps answer an Arrived presence event with their full
        // Automerge document. Synthesize that local event for the
        // durable host so its retained checkpoints include app
        // state that existed before the host connected.
        let host_peer = crate::network::behaviour::peer_liveness_handle(&peer);
        for payload in [
            ui::collaboration::PresencePayload::Arrived,
            ui::collaboration::PresencePayload::Left,
        ] {
            let key = ui::presence::PresenceKey::Workspace {
                id: room.to_string(),
            };
            if let Some(presence) = crate::shell::collab_presence::to_peer(&key, &payload) {
                self.handle_typed_app_presence_message(
                    host_peer,
                    String::new(),
                    room.to_string(),
                    presence,
                );
            }
        }
    }

    /// Route one gossip [`NeighbourMsg`] that arrived on topic `ws`
    /// from `peer` to its per-variant handler, then flush anything the
    /// replica queued in response. Split out of [`Self::tick`] via
    /// [`Self::dispatch_net_event`].
    fn dispatch_neighbour_msg(&self, ws: u64, peer: iroh::EndpointId, msg: NeighbourMsg) {
        if let Some(expected) = Self::explicit_topic_for_message(&msg)
            && expected != ws
        {
            tracing::warn!(
                ws,
                expected,
                "dropping collaboration message on the wrong room topic"
            );
            return;
        }
        let origin = crate::shell::state::PeerOrigin::peer(&peer);
        if let NeighbourMsg::IdentityHello { cert, revocations } = &msg {
            self.accept_identity_hello(ws, &origin, &peer, cert, revocations);
            return;
        }
        if self.require_peer_identity && self.peer_identities.get(&origin).is_none() {
            tracing::warn!(peer = %peer, "dropping collaboration message from an unauthenticated device");
            return;
        }
        match msg {
            // Ephemeral overlay state, not CRDT state — skips the replica entirely.
            NeighbourMsg::PresenceMoved {
                peer: peer_hash,
                user,
                key,
                point,
                ts,
            } => {
                self.handle_presence_message(peer_hash, user, key, point, ts);
            }
            NeighbourMsg::AppCrdtIncremental {
                peer: peer_hash,
                app_id,
                cluster_id,
                record_id,
                data,
            } => {
                self.dispatch_app_crdt_incremental(
                    &origin, peer_hash, app_id, cluster_id, record_id, data,
                );
            }
            NeighbourMsg::AppCrdtSnapshot {
                peer: peer_hash,
                app_id,
                cluster_id,
                record_id,
                data,
            } => {
                self.dispatch_app_crdt_snapshot(
                    &origin, peer_hash, app_id, cluster_id, record_id, data,
                );
            }
            // Signed path, same routing as the plain increment.
            NeighbourMsg::AppCrdtIncrementalSigned {
                peer: peer_hash,
                app_id,
                cluster_id,
                record_id,
                envelope,
            } => {
                if peer_hash != self.local_peer_hash && self.is_in_room(&cluster_id) {
                    self.handle_app_crdt_signed_message(
                        peer_hash, app_id, cluster_id, record_id, *envelope,
                    );
                }
            }
            NeighbourMsg::AppPresenceV2 {
                peer: peer_hash,
                app_id,
                cluster_id,
                presence,
            } => self.dispatch_app_presence_v2(peer_hash, app_id, cluster_id, presence),
            // Cooperative scrollback: skip self-echoes.
            NeighbourMsg::ScrollbackRequest {
                artifact,
                before_seq,
                max,
                requester,
            } => self.dispatch_scrollback_request(requester, artifact, before_seq, max),
            NeighbourMsg::ScrollbackResponse {
                artifact,
                ops,
                responder,
            } => self.dispatch_scrollback_response(
                responder,
                artifact,
                // Unbox at the wire boundary: nothing downstream needs
                // per-op indirection once it's off the wire.
                ops.into_iter().map(|op| *op).collect(),
            ),
            other => self.dispatch_to_replica(peer, &other),
        }
    }

    /// `AppPresenceV2`: app-level presence crossing the host boundary.
    /// Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_app_presence_v2(
        &self,
        peer_hash: u64,
        app_id: String,
        cluster_id: String,
        presence: collab_types::PeerPresence,
    ) {
        if peer_hash != self.local_peer_hash && self.is_in_room(&cluster_id) {
            self.handle_typed_app_presence_message(peer_hash, app_id, cluster_id, presence);
        }
    }

    /// `ScrollbackRequest`: skip self-echoes, otherwise queue for the
    /// broker's drainer. Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_scrollback_request(
        &self,
        requester: u64,
        artifact: collab_types::ArtifactId,
        before_seq: u64,
        max: u16,
    ) {
        if requester != self.local_peer_hash {
            self.handle_scrollback_request_message(requester, artifact, before_seq, max);
        }
    }

    /// `ScrollbackResponse`: skip self-echoes, otherwise queue for the
    /// broker's drainer. Split out of [`Self::dispatch_neighbour_msg`].
    fn dispatch_scrollback_response(
        &self,
        responder: u64,
        artifact: collab_types::ArtifactId,
        ops: Vec<collab_types::SignedOp>,
    ) {
        if responder != self.local_peer_hash {
            self.handle_scrollback_response_message(responder, artifact, ops);
        }
    }

    /// `AppCrdtIncremental`: opaque app-level Automerge blobs, not
    /// shell-tree changes; self-publishes are filtered so a local edit
    /// doesn't loop back as a "remote" event. Split out of
    /// [`Self::dispatch_neighbour_msg`].
    fn dispatch_app_crdt_incremental(
        &self,
        origin: &crate::shell::state::PeerOrigin,
        peer_hash: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
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
        if peer_hash != self.local_peer_hash
            && self.is_in_room(&cluster_id)
            && (!self.require_peer_identity
                || self.peer_may_write_app(origin, &cluster_id, &app_id))
        {
            self.handle_app_crdt_message(peer_hash, app_id, cluster_id, record_id, data);
        }
    }

    /// `AppCrdtSnapshot`: a whole document for a peer with no history to
    /// apply deltas against. Same two gates as the increment, and they
    /// matter more here: a whole document merged into the wrong one
    /// cannot be taken back out. Split out of
    /// [`Self::dispatch_neighbour_msg`].
    fn dispatch_app_crdt_snapshot(
        &self,
        origin: &crate::shell::state::PeerOrigin,
        peer_hash: u64,
        app_id: String,
        cluster_id: String,
        record_id: String,
        data: Vec<u8>,
    ) {
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
        if peer_hash != self.local_peer_hash
            && self.is_in_room(&cluster_id)
            && (!self.require_peer_identity
                || self.peer_may_write_app(origin, &cluster_id, &app_id))
        {
            self.handle_app_crdt_snapshot_message(peer_hash, app_id, cluster_id, record_id, data);
        }
    }

    /// Every [`NeighbourMsg`] variant not special-cased in
    /// [`Self::dispatch_neighbour_msg`] (`SyncReq`, `SyncResp`, ...)
    /// replicates through the shell-tree replica. `SyncReq`
    /// additionally names the peer redeeming an invite before the
    /// replica answers, so roster and reply land together.
    fn dispatch_to_replica(&self, peer: iroh::EndpointId, msg: &NeighbourMsg) {
        if let NeighbourMsg::SyncReq { room, .. } = msg {
            self.name_invitee_on_arrival(&peer, room);
        }
        // A poisoned replica lock here just drops this one message
        // rather than panicking the whole compositor.
        let Ok(mut rep) = self.replica.lock() else {
            return;
        };
        // The peer's own identity, not the liveness-handle
        // digest: this decides whether the payload may rewrite
        // a room's governance.
        rep.handle_from(&crate::shell::state::PeerOrigin::peer(&peer), msg);
        let outgoing = rep.drain_outgoing();
        drop(rep);
        for out_msg in outgoing {
            let _ = self.publish_message(out_msg);
        }
    }

    fn accept_identity_hello(
        &self,
        workspace_topic: u64,
        origin: &crate::shell::state::PeerOrigin,
        peer: &iroh::EndpointId,
        cert: &qos_user_identity::DeviceCert,
        revocations: &[qos_user_identity::RevocationEntry],
    ) {
        if cert.device_public_key != *peer.as_bytes() || revocations.len() > 4096 {
            tracing::warn!(peer = %peer, "rejecting invalid peer identity handshake");
            self.peer_identities.remove(origin);
            return;
        }
        let mut list = qos_user_identity::RevocationList::new();
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key || list.insert(entry.clone()).is_err()
            {
                tracing::warn!(peer = %peer, "rejecting peer identity with invalid revocation set");
                self.peer_identities.remove(origin);
                return;
            }
        }
        self.peer_identities
            .set_revocations(cert.user_public_key, list);
        let was_known = self.peer_identities.get(origin).is_some();
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .unwrap_or(u64::MAX);
        if self
            .peer_identities
            .insert(origin.clone(), cert.clone(), now_ns)
            .is_err()
            || self.peer_identities.is_revoked(
                &cert.user_public_key,
                &cert.device_public_key,
                now_ns,
            )
        {
            tracing::warn!(peer = %peer, "rejecting expired, invalid, or revoked peer device");
            self.peer_identities.remove(origin);
            return;
        }
        self.admit_invited_identity(workspace_topic, peer, cert);
        // The initial SyncReq can race ahead of the periodic identity hello
        // and is intentionally dropped. Re-kick anti-entropy after trust is
        // established so joining never waits for another topology event.
        if !was_known {
            self.publish_sync_requests();
        }
    }

    fn admit_invited_identity(
        &self,
        workspace_topic: u64,
        peer: &iroh::EndpointId,
        cert: &qos_user_identity::DeviceCert,
    ) {
        let room = self.replica.lock().ok().and_then(|replica| {
            replica
                .tree
                .read()
                .rooms()
                .into_iter()
                .find(|room| room_topic_id(*room) == workspace_topic)
        });
        let Some(room) = room else { return };
        let name = self
            .pending_identity_admissions
            .lock()
            .ok()
            .and_then(|pending| pending.get(&room).cloned());
        let Some(name) = name else { return };
        let person = match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
        ) {
            collab_types::caps::Principal::Person(person) => person,
            _ => return,
        };
        let Ok(replica) = self.replica.lock() else {
            return;
        };
        let mut tree = replica.tree.write();
        let local = crate::shell::state::local_user_id_for_room(&room);
        let granted = {
            let Some(workspace) = tree
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
            else {
                return;
            };
            let room_scope = workspace.room.to_string();
            workspace
                .grant_as(
                    &local,
                    crate::shell::state::CapOverride::membership(
                        crate::shell::state::Subject::person(person.clone()),
                        room_scope,
                        crate::shell::state::Verb::Write,
                    ),
                )
                .is_some()
        };
        if !granted {
            return;
        }
        let session = crate::network::behaviour::peer_liveness_handle(peer);
        tree.admit_collaborator_in_room(
            room,
            crate::shell::state::Collaborator::new(person, name.clone()).with_session(session),
        );
        let _ = tree.name_peer(room, &local, &peer.to_string(), &name);
        tree.flush_to_disk();
        drop(tree);
        if let Ok(mut pending) = self.pending_identity_admissions.lock() {
            pending.remove(&room);
        }
    }

    fn peer_may_write_app(
        &self,
        origin: &crate::shell::state::PeerOrigin,
        cluster_id: &str,
        app_id: &str,
    ) -> bool {
        let Some(identity) = self.peer_identities.get(origin) else {
            return false;
        };
        let person = match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(identity.user_pubkey),
        ) {
            collab_types::caps::Principal::Person(person) => person,
            _ => return false,
        };
        let Some(room) = RoomId::parse(cluster_id) else {
            return false;
        };
        let Ok(replica) = self.replica.lock() else {
            return false;
        };
        let tree = replica.tree.read();
        let Some(workspace) = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
        else {
            return false;
        };
        let actor = workspace.actor_for_person(&person);
        workspace.allows(
            &actor,
            &crate::shell::state::ResourceRef::App(app_id.to_string()),
            crate::shell::state::Verb::Write,
        )
    }
}

fn selected_device_cert(
    room: RoomId,
    keypair: &iroh::SecretKey,
) -> Result<qos_user_identity::DeviceCert, String> {
    let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "no identity is selected for this workspace".to_string())?;
    let paths = crate::firstboot::collaboration_identity_paths();
    let cert = qos_user_identity::persist::read_device_cert(
        &paths.device_cert_for_user(&profile.user_public_key),
    )
    .map_err(|error| error.to_string())?;
    qos_user_identity::verify_device_cert(&cert).map_err(|error| error.to_string())?;
    if cert.user_public_key != profile.user_public_key
        || cert.device_public_key != *keypair.public().as_bytes()
    {
        return Err("workspace identity certificate does not match this device".into());
    }
    if crate::device_fleet::DeviceFleetStore::from_env()
        .and_then(|store| store.load(&profile.id))
        .is_ok_and(|fleet| fleet.is_revoked(&cert.device_public_key))
    {
        return Err("this device has been revoked for the workspace identity".into());
    }
    Ok(cert)
}

fn host_identity_for_key(
    keypair: &iroh::SecretKey,
    keypair_path: &std::path::Path,
) -> Option<HostIdentity> {
    let root = keypair_path.parent()?.parent()?;
    let paths = qos_user_identity::IdentityPaths::under(root);
    let user_public_key = qos_user_identity::persist::read_public_key(&paths.user_pubkey).ok()?;
    let device_cert = qos_user_identity::persist::read_device_cert(&paths.device_cert).ok()?;
    if qos_user_identity::verify_device_cert(&device_cert).is_err()
        || device_cert.user_public_key != user_public_key
        || device_cert.device_public_key != *keypair.public().as_bytes()
    {
        tracing::warn!("ignoring invalid collaboration device certificate");
        return None;
    }
    let mut workspace_device_certs = std::collections::HashMap::new();
    let mut workspace_revocations = std::collections::HashMap::new();
    if let Ok(store) = crate::identity_registry::IdentityRegistryStore::from_env()
        && let Ok(registry) = store.migrate_legacy_primary()
    {
        for (room, identity_id) in &registry.workspace_identities {
            let Some(profile) = registry.identities.get(identity_id) else {
                continue;
            };
            let Some(room) = RoomId::parse(room) else {
                continue;
            };
            let cert_path = paths.device_cert_for_user(&profile.user_public_key);
            let Ok(cert) = qos_user_identity::persist::read_device_cert(&cert_path) else {
                tracing::warn!(
                    room = %room,
                    identity = %identity_id,
                    "workspace identity has no device certificate"
                );
                continue;
            };
            if qos_user_identity::verify_device_cert(&cert).is_ok()
                && cert.user_public_key == profile.user_public_key
                && cert.device_public_key == *keypair.public().as_bytes()
                && !crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(identity_id))
                    .is_ok_and(|fleet| fleet.is_revoked(&cert.device_public_key))
            {
                workspace_device_certs.insert(room_topic_id(room), cert);
                if let Ok(fleet) = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(identity_id))
                {
                    workspace_revocations.insert(room_topic_id(room), fleet.revocations);
                }
            }
        }
    }
    Some(HostIdentity {
        device_cert,
        device_signing_key: ed25519_dalek::SigningKey::from_bytes(&keypair.to_bytes()),
        workspace_device_certs,
        workspace_revocations,
        identity_paths: Some(paths),
    })
}

impl Drop for ShellNetwork {
    fn drop(&mut self) {
        // `take` + explicit shutdown avoids the noisy "runtime dropped
        // while still running tasks" warning from the implicit path.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Test-only constructor that wires `ShellNetwork`'s command sender to
/// a caller-owned receiver, bypassing the iroh actor. `pub(crate)` so
/// the broker's own tests can drive the inbound side of the peer bridge too.
#[cfg(test)]
pub(crate) fn shell_network_for_test(
    tree: WorkspaceTreeHandle,
    local_peer_hash: u64,
) -> (ShellNetwork, mpsc::Receiver<NetCmd>) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (event_tx, event_rx) = mpsc::channel(64);
    let keypair = iroh::SecretKey::generate();
    let local_peer_id = keypair.public();
    let mut joined_topics = std::collections::HashSet::from([DEFAULT_WS_TOPIC]);
    joined_topics.extend(tree.read().rooms().into_iter().map(room_topic_id));
    let net = ShellNetwork {
        cmd_tx,
        replica: Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree))),
        event_tx,
        event_rx: Mutex::new(event_rx),
        joined_topics: Mutex::new(joined_topics),
        runtime: None,
        local_peer_hash,
        keypair,
        local_peer_id,
        listen_addrs: Arc::new(Mutex::new(Vec::new())),
        host_addrs: Mutex::new(Vec::new()),
        last_sync_kick: Mutex::new(None),
        peer_identities: PeerIdentityStore::new(),
        require_peer_identity: false,
        pending_invitees: Mutex::new(std::collections::HashMap::new()),
        pending_identity_admissions: Mutex::new(std::collections::HashMap::new()),
        inbound_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt_signed: Arc::new(Mutex::new(Vec::new())),
        inbound_app_presence: Arc::new(Mutex::new(Vec::new())),
        inbound_app_crdt_snapshot: Arc::new(Mutex::new(Vec::new())),
        snapshot_requests: Arc::new(Mutex::new(std::collections::HashMap::new())),
        inbound_scrollback_request: Arc::new(Mutex::new(Vec::new())),
        inbound_scrollback_response: Arc::new(Mutex::new(Vec::new())),
    };
    (net, cmd_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(identity_env)]
    fn identity_hello_redeems_invite_as_editor_and_revocation_removes_trust() {
        use qos_user_identity::{
            DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
            sign_revocation_entry,
        };

        let root = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let device = iroh::SecretKey::from_bytes(&[62; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Guest laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("certificate");

        let room = RoomId::new();
        let mut workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(17),
            "Shared",
        );
        workspace.room = room;
        workspace.set_owner(crate::shell::state::local_user_id_for_room(&room));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (net, _commands) = shell_network_for_test(tree.shared(), 99);
        net.pending_identity_admissions
            .lock()
            .expect("pending")
            .insert(room, "Ada".into());

        let origin = crate::shell::state::PeerOrigin::peer(&device.public());
        net.accept_identity_hello(room_topic_id(room), &origin, &device.public(), &cert, &[]);
        let person = match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(cert.user_public_key),
        ) {
            collab_types::caps::Principal::Person(person) => person,
            _ => unreachable!(),
        };
        let replica = net.replica.lock().expect("replica");
        let tree = replica.tree.read();
        let joined = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .expect("workspace");
        assert_eq!(
            joined.role_of(&person),
            Some(crate::shell::state::CapRole::Editor)
        );
        drop(tree);
        drop(replica);
        assert!(net.peer_may_write_app(&origin, &room.to_string(), "dev.qos.notes"));

        let mut revoked = RevocationEntry {
            user_public_key: cert.user_public_key,
            revoked_device_public_key: cert.device_public_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revoked, &root).expect("revocation");
        net.accept_identity_hello(
            room_topic_id(room),
            &origin,
            &device.public(),
            &cert,
            &[revoked],
        );
        assert!(net.peer_identities.get(&origin).is_none());
        assert!(!net.peer_may_write_app(&origin, &room.to_string(), "dev.qos.notes"));
    }

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
            // `bootstrap_demo` leaves the owner `None`; `may_administer`
            // reads it, so an unset owner would refuse every name below
            // for the wrong reason.
            let me = crate::shell::state::local_user_id();
            for ws in &mut t.workspaces {
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

    fn name_in(
        tree: &crate::shell::state::WorkspaceTreeHandle,
        room: crate::shell::state::RoomId,
        peer: &iroh::EndpointId,
    ) -> Option<String> {
        tree.read()
            .room_peer_names
            .get(&room)
            .and_then(|names| names.get(&peer.to_string()))
            .cloned()
    }

    /// The name the inviter typed reaches the room document only once
    /// the person it was for turns up, when there's an identity to
    /// attach it to.
    #[test]
    fn the_invitee_is_named_when_they_ask_this_room_for_its_document() {
        let (_tmp, tree, net, rooms) = net_with_rooms();
        let room = rooms[0];
        net.pending_invitees
            .lock()
            .unwrap()
            .insert(room, "Xaver".to_string());

        let guest = iroh::SecretKey::generate().public();
        assert_eq!(name_in(&tree, room, &guest), None, "nobody is named yet");
        net.name_invitee_on_arrival(&guest, &room.to_string());
        assert_eq!(
            name_in(&tree, room, &guest).as_deref(),
            Some("Xaver"),
            "the room's own authority recorded the name the inviter gave"
        );

        let stranger = iroh::SecretKey::generate().public();
        net.name_invitee_on_arrival(&stranger, &room.to_string());
        assert_eq!(
            name_in(&tree, room, &stranger),
            None,
            "a name belongs to one person, and it was already given"
        );
    }

    /// A name waiting on one room is not a name for another.
    #[test]
    fn a_pending_name_does_not_leak_into_another_room() {
        let (_tmp, tree, net, rooms) = net_with_rooms();
        net.pending_invitees
            .lock()
            .unwrap()
            .insert(rooms[0], "Xaver".to_string());

        let guest = iroh::SecretKey::generate().public();
        net.name_invitee_on_arrival(&guest, &rooms[1].to_string());
        assert_eq!(name_in(&tree, rooms[1], &guest), None);
        assert_eq!(
            name_in(&tree, rooms[0], &guest),
            None,
            "asking about another room does not redeem this one's invite"
        );
        assert!(
            net.pending_invitees.lock().unwrap().contains_key(&rooms[0]),
            "and the name is still waiting for whoever actually redeems it"
        );
    }

    /// Nobody invites themselves, and nothing unparseable is a room.
    #[test]
    fn arrival_naming_refuses_this_machine_and_nonsense_rooms() {
        let (_tmp, tree, net, rooms) = net_with_rooms();
        let room = rooms[0];
        net.pending_invitees
            .lock()
            .unwrap()
            .insert(room, "Xaver".to_string());

        let me = net.local_peer_id;
        net.name_invitee_on_arrival(&me, &room.to_string());
        assert_eq!(
            name_in(&tree, room, &me),
            None,
            "this machine's own sync request is not somebody arriving"
        );

        net.name_invitee_on_arrival(&iroh::SecretKey::generate().public(), "not-a-room");
        assert!(
            net.pending_invitees.lock().unwrap().contains_key(&room),
            "a room id that does not parse names nothing and spends nothing"
        );
    }

    #[test]
    fn tick_with_no_events_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let key_path = tmp.path().join("peer-key");
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let net = ShellNetwork::spawn(tree, &key_path).expect("spawn");
        net.tick();
    }

    #[test]
    fn host_ready_seeds_shell_and_app_checkpoints() {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let room_id = tree.read().rooms()[0];
        let topic = room_topic_id(room_id);
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFE);
        // Consume the normal first-tick anti-entropy request so the assertion
        // below is specifically about the host baseline edge.
        net.tick();
        while cmd_rx.try_recv().is_ok() {}

        net.event_tx
            .try_send(NetEvent::HostReady {
                ws: topic,
                peer: iroh::SecretKey::from_bytes(&[23; 32]).public(),
            })
            .unwrap();
        net.tick();

        assert!(matches!(
            cmd_rx.try_recv(),
            Ok(NetCmd::PublishCheckpoint {
                ws,
                msg: NeighbourMsg::SyncResp { ref room, .. },
            }) if ws == topic && room == &room_id.to_string()
        ));
        let app_events = net.inbound_app_presence.lock().unwrap().clone();
        assert_eq!(
            app_events.len(),
            2,
            "Arrived + Left prompt a bounded app snapshot refresh"
        );
        let payloads: Vec<_> = app_events.iter().map(|event| &event.presence).collect();
        assert!(matches!(
            payloads[0],
            collab_types::PeerPresence {
                payload: collab_types::PeerPresencePayload::Arrived,
                ..
            }
        ));
        assert!(matches!(
            payloads[1],
            collab_types::PeerPresence {
                payload: collab_types::PeerPresencePayload::Left,
                ..
            }
        ));
    }

    /// Build the inbound event a LAN peer's app-CRDT publish arrives as.
    fn app_crdt_event(cluster_id: &str, from_peer_hash: u64, peer: iroh::PublicKey) -> NetEvent {
        NetEvent::Msg {
            ws: RoomId::parse(cluster_id)
                .filter(|room| !room.is_nil())
                .map_or(DEFAULT_WS_TOPIC, room_topic_id),
            peer,
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
    /// machine is in. Driven through the real receive path (`tick`),
    /// not the helper directly, with a positive control so the two
    /// refusals prove something rather than just "nothing arrives".
    #[test]
    fn app_crdt_from_a_room_we_are_not_in_never_reaches_the_broker() {
        let mut tree = WorkspaceTree::default();
        let local = tree.append_empty_workspace("Mine");
        let our_room = tree.room_of(local).expect("a minted room");
        tree.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == local)
            .expect("workspace")
            .set_owner(crate::shell::state::local_user_id_for_room(&our_room));
        let handle = tree.shared();
        let (net, _cmd_rx) = shell_network_for_test(handle, 0xAAAAu64);

        // Plain legacy app deltas are accepted only after the sender has
        // proven an identity->device chain and received membership. Keep the
        // routing assertion's positive control authenticated like production.
        let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let device = iroh::SecretKey::from_bytes(&[72; 32]);
        let mut cert = qos_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: *device.public().as_bytes(),
            device_label: "Peer laptop".into(),
            capabilities: qos_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        qos_user_identity::sign_device_cert(&mut cert, &root).expect("device certificate");
        net.pending_identity_admissions
            .lock()
            .expect("pending admission")
            .insert(our_room, "Peer".into());
        net.event_tx
            .try_send(NetEvent::Msg {
                ws: room_topic_id(our_room),
                peer: device.public(),
                msg: NeighbourMsg::IdentityHello {
                    cert: Box::new(cert),
                    revocations: Vec::new(),
                },
            })
            .expect("queue identity proof");
        net.tick();

        let stranger_room = crate::shell::state::RoomId::new();
        assert_ne!(stranger_room, our_room);

        net.event_tx
            .try_send(app_crdt_event(
                &stranger_room.to_string(),
                0xBBBBu64,
                device.public(),
            ))
            .expect("queue the stranger's publish");
        net.tick();
        assert!(
            net.inbound_app_crdt_handle().lock().unwrap().is_empty(),
            "a delta for a room this machine is not in must not reach the broker queue",
        );

        net.event_tx
            .try_send(app_crdt_event("not-a-room", 0xBBBBu64, device.public()))
            .expect("queue the malformed publish");
        net.tick();
        assert!(
            net.inbound_app_crdt_handle().lock().unwrap().is_empty(),
            "a cluster id that is not a uuid names no room, so it must be refused",
        );

        net.event_tx
            .try_send(app_crdt_event(
                &our_room.to_string(),
                0xBBBBu64,
                device.public(),
            ))
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

        let host_ws = Workspace::test_default(
            crate::shell::state::WorkspaceId::new(),
            "Design review".to_string(),
        );
        let host_room = host_ws.room;
        let owner_key = iroh::SecretKey::generate();
        let owner_addr = iroh::EndpointAddr::new(owner_key.public())
            .with_ip_addr("127.0.0.1:4001".parse().unwrap());
        let token = make_invite_token(&owner_key, &host_ws, &owner_addr, 3600)
            .expect("the host issues an invite");

        let mut guest = WorkspaceTree::default();
        let local = guest.append_empty_workspace("Mine");
        let before = guest.room_of(local).expect("the guest's own room");
        assert_ne!(before, host_room);
        let tree = guest.shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree.clone(), 0x1234u64);

        let joined = net
            .accept_invite(local, &token)
            .expect("a well-formed invite is accepted");
        assert_eq!(
            joined, host_room,
            "the guest joins the room it was invited to"
        );
        assert_eq!(
            tree.read().room_of(local),
            Some(host_room),
            "the guest's workspace now addresses the inviter's room",
        );

        // Three commands, in order: join the stable room shard, dial the
        // address the token named, then ask that room for a catch-up.
        let subscribe = cmd_rx
            .try_recv()
            .expect("accepting an invite must join its room topic");
        match subscribe {
            NetCmd::JoinWorkspace { ws, .. } => assert_eq!(ws, room_topic_id(host_room)),
            other => panic!("expected NetCmd::JoinWorkspace, got {other:?}"),
        }
        let dial = cmd_rx
            .try_recv()
            .expect("accepting an invite must dial the address it named");
        match dial {
            NetCmd::Connect { addr } => assert_eq!(addr, owner_addr),
            other => panic!("expected NetCmd::Connect, got {other:?}"),
        }
        let cmd = cmd_rx
            .try_recv()
            .expect("accepting an invite must ask the room for a catch-up");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, room_topic_id(host_room));
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

    /// A token that does not verify joins nothing and asks nothing.
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
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xABCDu64);
        let expected_topic = net.active_ws_for_publish();
        net.publish_presence("local-cursor".into(), (100.0, 200.0));
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_presence must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, expected_topic);
                match msg {
                    NeighbourMsg::PresenceMoved {
                        peer,
                        user,
                        key,
                        point,
                        ts: _,
                    } => {
                        assert_eq!(peer, 0xABCDu64);
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
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFEu64);
        let expected_topic = net.active_ws_for_publish();

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
                assert_eq!(ws, expected_topic);
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
    fn an_oversized_snapshot_is_refused_rather_than_handed_to_gossip() {
        // Refusing here turns "this document silently never converges"
        // into a log line naming the size, rather than a transport-level failure.
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
    fn one_arrival_releases_each_document_once() {
        // One arrival may need baselines for several records/apps. Repeated
        // snapshots for one record still stay local until another arrival.
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
        assert!(
            cmd_rx.try_recv().is_ok(),
            "the arrival releases one document"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "the commits after it must not each broadcast the document again",
        );

        net.publish_app_crdt_snapshot(
            "dev.qos.word-processor".into(),
            room.into(),
            "a-second-record".into(),
            vec![0xBB],
        );
        assert!(
            cmd_rx.try_recv().is_ok(),
            "the same arrival releases a baseline for each distinct document",
        );

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
    fn snapshot_at_the_cap_fits_the_iroh_frame_budget() {
        let msg = NeighbourMsg::AppCrdtSnapshot {
            peer: u64::MAX,
            app_id: "dev.qos.word-processor".into(),
            cluster_id: "00000000-0000-0000-0000-000000000000".into(),
            record_id: "file:".to_string() + &"a".repeat(4096),
            data: vec![0xFFu8; MAX_APP_SNAPSHOT_BYTES],
        };
        let encoded = postcard::to_allocvec(&msg).expect("encode");
        assert!(
            encoded.len() < 64 * 1024 * 1024,
            "a snapshot at the cap must fit the Iroh frame budget; got {} bytes",
            encoded.len(),
        );
    }

    #[test]
    fn handle_app_crdt_snapshot_message_buffers_for_drain() {
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
        let buf = buf_handle.lock().unwrap().clone();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].app_id, "dev.qos.word-processor");
        assert_eq!(buf[0].cluster_id, "00000000-0000-0000-0000-000000000000");
        assert_eq!(buf[0].record_id, "file:/work/report.odt");
        assert_eq!(buf[0].data, vec![0x01, 0x02]);
    }

    #[test]
    fn handle_presence_message_buffers_for_drain() {
        // Folded into `PresenceCtx` by the shell's frame drainer; every
        // field must round-trip so the overlay attributes the cursor
        // correctly.
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
        let buf = buf_handle.lock().unwrap().clone();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].user, "bob");
        assert_eq!(buf[0].key, "ws/0");
        assert_eq!(buf[0].point, (10.0, 20.0));
        assert_eq!(buf[0].ts, 1_700_000_000);
    }

    #[test]
    fn a_session_does_not_take_its_own_presence_off_the_network() {
        // Gossip hands a session its own frames back; painting them
        // would draw a second cursor chasing the user's own hand.
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
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xCAFEu64);
        let expected_topic = net.active_ws_for_publish();
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
                assert_eq!(ws, expected_topic);
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
        let buf = buf_handle.lock().unwrap().clone();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 7u64);
        assert_eq!(buf[0].app_id, "terminal");
        assert_eq!(buf[0].cluster_id, "cluster-uuid");
        assert_eq!(buf[0].record_id, "file:/work/notes.md");
        assert_eq!(buf[0].data, vec![0x01, 0x02]);
    }

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
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xFEED_u64);
        let expected_topic = net.active_ws_for_publish();
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
                assert_eq!(ws, expected_topic);
                match msg {
                    NeighbourMsg::AppCrdtIncrementalSigned {
                        peer,
                        app_id,
                        cluster_id,
                        record_id,
                        envelope: got,
                    } => {
                        assert_eq!(peer, 0xFEED_u64);
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
        let buf = buf_handle.lock().unwrap().clone();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].peer, 99u64);
        assert_eq!(buf[0].app_id, "notes");
        assert_eq!(buf[0].cluster_id, "cluster-7");
        assert_eq!(buf[0].record_id, "uuid:9f");
        assert_eq!(*buf[0].envelope, envelope);
    }

    #[test]
    fn active_ws_for_publish_returns_the_active_rooms_stable_topic() {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let expected = room_topic_id(tree.read().active_workspace().unwrap().room);
        let (net, _cmd_rx) = shell_network_for_test(tree, 0u64);
        assert_eq!(net.active_ws_for_publish(), expected);
    }

    #[test]
    fn publish_scrollback_request_emits_a_publish_command() {
        use collab_types::ArtifactId;
        let tree = WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = shell_network_for_test(tree, 0xDEAD_u64);
        let expected_topic = net.active_ws_for_publish();
        let artifact = ArtifactId::new();
        net.publish_scrollback_request(artifact, 100, 25);
        let cmd = cmd_rx
            .try_recv()
            .expect("publish_scrollback_request must enqueue a NetCmd");
        match cmd {
            NetCmd::Publish { ws, msg } => {
                assert_eq!(ws, expected_topic);
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
        let buf = handle.lock().unwrap().clone();
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
        let env = make_envelope(11);
        net.handle_scrollback_response_message(99u64, artifact, vec![env.clone()]);
        let buf = handle.lock().unwrap().clone();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].responder, 99u64);
        assert_eq!(buf[0].artifact, artifact);
        assert_eq!(buf[0].ops.len(), 1);
        assert_eq!(buf[0].ops[0], env);
    }
}
