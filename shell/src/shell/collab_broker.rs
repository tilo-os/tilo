//! Local CRDT fan-out broker for the `QUICK_COLLAB_SOCK` Unix socket.
//!
//! Every quick-ui app connects a [`ui::collaboration::CollaborationManager`]
//! to `QUICK_COLLAB_SOCK` and exchanges length-prefixed JSON frames (`u32`
//! little-endian length + JSON body). The broker accepts any number of
//! peers and, per peer, decodes each `CollabRequest` into the matching
//! `CollabEvent` — `SyncIncremental`/`SyncCRDT` → `CRDTIncremental`/
//! `CRDTUpdate`, `Presence` → `RemotePresenceUpdate`, `Hello` swallowed
//! and just records the connection's [`CrdtRoute`] — then fans it out to
//! the peers it's addressed to; see [`CrdtRoute`] for the routing rules.
//!
//! Routing **fails closed**: a connection that has not said `Hello`
//! receives nothing, and a frame whose sender has not said `Hello` is
//! neither fanned out locally nor republished over Iroh. There is no
//! broadcast primitive left in the broker.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use collab_types::{
    ArtifactId, ChannelState, OpClass, RedactedMarker, ValidationError, validate_inbound_op,
};
use ui::collaboration::{CollabEvent, CollabRequest};

use crate::shell::collab_broker_capnp::CapnpBrokerState;
pub use crate::shell::collab_core::{
    AuditEntry, BrokerChannels, RedactionRecord, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::{PeerPublisher, Transport};
use crate::shell::network::ShellNetwork;

/// Per-peer entry in the broker's fan-out table.
struct Peer {
    /// Stable id assigned at accept time, identifying the *connection*
    /// rather than a participant — see [`LocalIdentity`] for who that is.
    id: u64,
    /// Self-reported display name from `Hello`. Informational only —
    /// outranked by the room's name, see [`LocalIdentity`].
    handle: Mutex<Option<String>>,
    /// Write side; the read side belongs to the per-peer reader thread.
    writer: Mutex<UnixStream>,
    /// Route announced on `CollabRequest::Hello`. `None` until Hello —
    /// a peer with `None` neither receives nor originates routed traffic.
    route: Mutex<Option<CrdtRoute>>,
}

impl Peer {
    /// Snapshot the announced route, if any.
    fn route(&self) -> Option<CrdtRoute> {
        self.route.lock().ok().and_then(|g| g.clone())
    }

    /// What this connection said it wanted to be called — the weakest
    /// input to [`LocalIdentity::resolve`].
    fn announced_handle(&self) -> Option<String> {
        self.handle.lock().ok().and_then(|g| g.clone())
    }
}

/// What a room calls the peers in it, keyed by the handle presence
/// frames identify their sender by. Same shape as
/// [`crate::shell::peer_presence::PeerLabels`].
pub type PeerLabels = std::collections::BTreeMap<u64, String>;

/// How long a room's answer is cached before re-asking. Reading it
/// takes the replica's lock and parses every peer id in the room
/// document, and the paths that need it run at pointer rate; matches
/// the peer-sync drain's cadence for the same lookup.
const LABEL_TTL: Duration = Duration::from_secs(2);

/// Who the traffic from a locally connected app belongs to, stamped on
/// everything an app sends before another app sees it. A local
/// connection is this machine's peer, named through the same lookup
/// remote peers use, so a participant reads the same everywhere.
///
/// Resolution order:
/// 1. What the room says — only its authority can write this
///    (`apply_room_changes` refuses anyone else), since self-naming next
///    to a capability-gated caret would be unenforceable.
/// 2. What the connection calls itself. No authority; never overrules 1.
/// 3. This machine's own user id.
pub struct LocalIdentity {
    /// What the room calls a peer, keyed by liveness handle. A
    /// parameter (not a direct lookup) so this is testable without a
    /// live network.
    labels: Box<dyn Fn(&str) -> PeerLabels + Send + Sync>,
    /// This machine's own liveness handle. Presence frames key by that
    /// digest while the room document keys by Iroh `EndpointId`;
    /// `room_peer_labels` bridges the two.
    local_handle: u64,
    /// The last answer per cluster, with the instant it was given.
    cache: Mutex<HashMap<String, (Instant, Option<String>)>>,
}

impl LocalIdentity {
    /// Production wiring: ask the room document over the peer-sync
    /// network. Without one, every local participant falls back to
    /// `local_user_id`.
    pub fn from_network(net: Option<Arc<ShellNetwork>>) -> Arc<Self> {
        let local_handle = net.as_ref().map_or(0, |n| n.local_peer_id_hash());
        Arc::new(Self {
            labels: Box::new(move |cluster| {
                net.as_ref()
                    .map(|n| n.peer_labels(cluster))
                    .unwrap_or_default()
            }),
            local_handle,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Wire an explicit lookup in place of the room document.
    pub fn with_lookup(
        local_handle: u64,
        labels: impl Fn(&str) -> PeerLabels + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            labels: Box::new(labels),
            local_handle,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// The room's name for this machine's peer in `cluster_id`.
    fn room_name(&self, cluster_id: &str) -> Option<String> {
        if cluster_id.is_empty() {
            return None;
        }
        let now = Instant::now();
        if let Ok(cache) = self.cache.lock()
            && let Some((at, answer)) = cache.get(cluster_id)
            && now.duration_since(*at) < LABEL_TTL
        {
            return answer.clone();
        }
        let answer = (self.labels)(cluster_id).get(&self.local_handle).cloned();
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(cluster_id.to_string(), (now, answer.clone()));
        }
        answer
    }

    /// The identity to stamp on traffic from a local connection that
    /// announced `route` and calls itself `announced`.
    pub fn resolve(&self, route: Option<&CrdtRoute>, announced: Option<&str>) -> String {
        route
            .and_then(|r| self.room_name(&r.cluster_id))
            .or_else(|| announced.map(str::to_owned))
            .unwrap_or_else(crate::shell::state::local_user_id)
    }
}

/// Sentinel cluster id: `crate::shell::state::RoomId::nil` rendered as
/// a string. A workspace announces this when its durable room identity
/// has not been minted yet; never delivered to or from — see
/// [`CrdtRoute::is_addressable`].
const UNMINTED_CLUSTER_ID: &str = "00000000-0000-0000-0000-000000000000";

/// The address a CRDT (or chrome) event is delivered to, built from the
/// `(app_id, cluster_id)` pair a connection announces on `Hello`. App
/// scope (both fields set) carries CRDT payloads, so one app's deltas
/// never reach another; cluster scope (`app_id` empty) carries chrome
/// traffic, where every app in the cluster is a legitimate audience, and
/// the Cap'n Proto transport, whose `joinChannel` surface has a channel
/// id but no app identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrdtRoute {
    /// App identity from the sender's `Hello`. Empty means "this
    /// transport does not announce an app identity" — matched at
    /// cluster granularity.
    pub app_id: String,
    /// Collaboration cluster id (a uuid rendered as a string on the
    /// legacy dialect, the `channelId` on the capnp one).
    pub cluster_id: String,
    /// The record the sender announced, or empty for an app whose state
    /// is still one document per `(app, cluster)`. Finest granularity —
    /// two people editing different files in one room share app and
    /// cluster, and only the record tells them apart. Routing key only.
    pub record_id: String,
}

impl CrdtRoute {
    /// App-scoped route — the addressing a CRDT payload from an app
    /// that has not adopted records uses.
    pub fn app(app_id: impl Into<String>, cluster_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id: cluster_id.into(),
            record_id: String::new(),
        }
    }

    /// Cluster-scoped route — chrome traffic and the capnp transport.
    pub fn cluster(cluster_id: impl Into<String>) -> Self {
        Self {
            app_id: String::new(),
            cluster_id: cluster_id.into(),
            record_id: String::new(),
        }
    }

    /// Record-scoped route — one document's change stream.
    pub fn record(
        app_id: impl Into<String>,
        cluster_id: impl Into<String>,
        record_id: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id: cluster_id.into(),
            record_id: record_id.into(),
        }
    }

    /// Narrow this route to a record; an empty `record_id` leaves it as
    /// it was (pre-record addressing).
    pub fn on_record(mut self, record_id: impl Into<String>) -> Self {
        self.record_id = record_id.into();
        self
    }

    /// Widen an app-scoped route to its cluster — presence rides this.
    /// "Who else is here" is a fact about the room, so a peer editing a
    /// different document is still a legitimate audience for it.
    pub(crate) fn to_cluster_scope(&self) -> Self {
        Self::cluster(self.cluster_id.clone())
    }

    /// The channel this route names, as one string — the capnp
    /// transport subscribes by channel id. Encoding lives in
    /// [`ui::collaboration::channel_key`] (shared with the SDK) so both
    /// ends can't drift.
    pub fn channel_key(&self) -> String {
        ui::collaboration::channel_key::format(&self.app_id, &self.cluster_id, &self.record_id)
    }

    /// Split a capnp `channelId` back into its route — the inverse of
    /// [`Self::channel_key`].
    pub fn from_channel_key(key: &str) -> Self {
        let (app_id, cluster_id, record_id) = ui::collaboration::channel_key::parse(key);
        Self {
            app_id,
            cluster_id,
            record_id,
        }
    }

    /// Whether a cluster id names a real room: not the empty string
    /// (never said `Hello`) and not the nil uuid (a workspace whose room
    /// identity has not been minted yet). The nil case must be checked
    /// separately from empty — otherwise every workspace still carrying
    /// the sentinel would share one enormous room on every machine.
    fn is_addressable(cluster_id: &str) -> bool {
        !cluster_id.is_empty() && cluster_id != UNMINTED_CLUSTER_ID
    }

    /// Whether this route may be stamped onto a frame leaving the
    /// machine. Same test as [`Self::delivers_to`], because the
    /// receiving broker rebuilds the route from the wire fields and runs
    /// the identical check — publishing an unaddressable route would put
    /// a frame in the room every receiver silently drops.
    pub fn is_publishable(&self) -> bool {
        Self::is_addressable(&self.cluster_id)
    }

    /// Whether an event addressed to `self` reaches a subscriber that
    /// announced `subscriber`. Fails closed if either side lacks an
    /// addressable cluster. App granularity applies only when both ends
    /// announced an app id; an end with none stays cluster-scoped rather
    /// than silently receiving nothing.
    ///
    /// Record granularity is innermost and fails closed both ways — "no
    /// record" is a value like any other, so a converted app's deltas
    /// never reach an unconverted connection of the same app.
    pub fn delivers_to(&self, subscriber: &CrdtRoute) -> bool {
        if !Self::is_addressable(&self.cluster_id) || !Self::is_addressable(&subscriber.cluster_id)
        {
            return false;
        }
        if self.cluster_id != subscriber.cluster_id {
            return false;
        }
        if self.app_id.is_empty() || subscriber.app_id.is_empty() {
            return true; // Cluster-scoped on at least one end.
        }
        if self.app_id != subscriber.app_id {
            return false;
        }
        self.record_id == subscriber.record_id
    }
}

/// Shared registry of connected peers: JSON connections, the
/// process-wide peer-id counter, and an optional [`CapnpBrokerState`]
/// that layers the same fanout onto capnp's per-subscription queues so
/// every event reaches both transports.
pub struct PeerTable {
    peers: Mutex<Vec<Arc<Peer>>>,
    /// Monotonic id source, shared with the capnp listener so ids never
    /// collide when the `except` filter runs on fanout.
    peer_id_counter: AtomicU64,
    /// Set once during `spawn_*` when the capnp listener is bound.
    capnp: Mutex<Option<Arc<CapnpBrokerState>>>,
    /// In-process subscribers. See [`RoomSubscription`].
    local_subs: Mutex<Vec<Arc<RoomSubscription>>>,
}

/// A subscriber inside the shell process, addressed by room — how the
/// compositor reads presence its apps publish. Delivery goes through the
/// same [`CrdtRoute::delivers_to`] check as a socket peer, against
/// `CrdtRoute::cluster(room)`; since presence widens to cluster scope on
/// the way out, two apps in one room reach one subscription, so a cursor
/// crossing between them is one peer moving, not a second appearing.
/// Rooms are mutable — workspaces are created, and join a peer's room
/// when an invite is accepted.
pub struct RoomSubscription {
    rooms: Mutex<Vec<String>>,
    sink: Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>,
}

impl RoomSubscription {
    /// Replace the set of rooms this subscription covers.
    pub fn set_rooms(&self, rooms: Vec<String>) {
        if let Ok(mut g) = self.rooms.lock() {
            *g = rooms;
        }
    }

    /// Whether `route` addresses any room this subscription holds. Uses
    /// the broker's own delivery predicate, so it never receives
    /// anything a socket peer in the same room wouldn't.
    fn wants(&self, route: &CrdtRoute) -> bool {
        let Ok(rooms) = self.rooms.lock() else {
            return false;
        };
        rooms
            .iter()
            .any(|room| route.delivers_to(&CrdtRoute::cluster(room.clone())))
    }
}

impl PeerTable {
    fn new() -> Self {
        Self {
            peers: Mutex::new(Vec::new()),
            peer_id_counter: AtomicU64::new(0),
            capnp: Mutex::new(None),
            local_subs: Mutex::new(Vec::new()),
        }
    }

    /// Register an in-process subscriber, returning a handle the caller
    /// can use to keep its room set current.
    pub(crate) fn subscribe_rooms(
        &self,
        rooms: Vec<String>,
        sink: Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>,
    ) -> Arc<RoomSubscription> {
        let sub = Arc::new(RoomSubscription {
            rooms: Mutex::new(rooms),
            sink,
        });
        if let Ok(mut g) = self.local_subs.lock() {
            g.push(Arc::clone(&sub));
        }
        sub
    }

    /// Snapshot the in-process subscribers so the sinks run without
    /// the registry lock held.
    fn local_subs(&self) -> Vec<Arc<RoomSubscription>> {
        self.local_subs
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Mint the next peer id. Legacy accept + capnp accept both go
    /// through this so their ids never collide.
    pub(crate) fn next_peer_id(&self) -> u64 {
        self.peer_id_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// How many connections this broker has ever accepted, across both
    /// transports. Read as a generation, not a count: it only rises, so
    /// a value newer than the one recorded beside an announcement means
    /// someone connected who wasn't there to hear it — see
    /// [`remote_participant_id`], which uses that to re-say a name.
    pub(crate) fn connections_seen(&self) -> u64 {
        self.peer_id_counter.load(Ordering::Relaxed)
    }

    /// Attach the capnp fanout sink. Called at most once, at
    /// broker-spawn time; repeat calls are silently ignored.
    pub(crate) fn attach_capnp(&self, state: Arc<CapnpBrokerState>) {
        if let Ok(mut g) = self.capnp.lock()
            && g.is_none()
        {
            *g = Some(state);
        }
    }

    /// Read the capnp fanout sink if one has been attached.
    fn capnp_state(&self) -> Option<Arc<CapnpBrokerState>> {
        self.capnp.lock().ok().and_then(|g| g.clone())
    }

    /// Add a peer to the table. Called once per `accept`.
    fn insert(&self, peer: Arc<Peer>) {
        if let Ok(mut guard) = self.peers.lock() {
            guard.push(peer);
        }
    }

    /// Remove a peer by id. Called when its reader thread observes EOF
    /// or when a write fails (peer is presumed dead).
    fn remove(&self, id: u64) {
        if let Ok(mut guard) = self.peers.lock() {
            guard.retain(|p| p.id != id);
        }
    }

    /// Snapshot the current peer set so the caller can write without
    /// holding the registry lock during slow IO.
    fn snapshot(&self) -> Vec<Arc<Peer>> {
        self.peers.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Serialize `event` and write it to every peer `route` addresses,
    /// skipping `except` (the originator; [`u64::MAX`] skips none).
    /// Delivery is decided by [`CrdtRoute::delivers_to`] — no broadcast
    /// fallback, so a silent peer gets nothing. Drops peers that error
    /// on write, and pushes to the capnp subscription queues so both
    /// transports observe the same routing.
    pub(crate) fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if route.cluster_id.is_empty() {
            tracing::debug!(
                "collab_broker: dropping event with an unaddressed route \
                 (sender never announced a cluster)"
            );
            return;
        }
        let bytes = match serde_json::to_vec(event) {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut dead: Vec<u64> = Vec::new();
        for peer in self.snapshot() {
            if peer.id == except {
                continue;
            }
            let Some(peer_route) = peer.route() else {
                continue; // Connected but silent: no route to address yet.
            };
            if !route.delivers_to(&peer_route) {
                continue;
            }
            if write_frame(&peer.writer, &bytes).is_err() {
                dead.push(peer.id);
            }
        }
        for id in dead {
            self.remove(id);
        }
        for sub in self.local_subs() {
            if sub.wants(route) {
                (sub.sink)(event, route);
            }
        }
        if let Some(capnp) = self.capnp_state() {
            capnp.fanout_to_route(event, except, route);
        }
    }

    /// Send `event` to exactly one peer by id — used by
    /// `FetchScrollback`, whose reply belongs only to the requester.
    /// Drops the peer on write failure; falls through to the capnp sink
    /// so a capnp-side requester's reply still lands.
    fn send_to_peer(&self, peer_id: u64, event: &CollabEvent) -> bool {
        let bytes = match serde_json::to_vec(event) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let target = self.snapshot().into_iter().find(|p| p.id == peer_id);
        if let Some(peer) = target {
            if write_frame(&peer.writer, &bytes).is_err() {
                self.remove(peer.id);
                return false;
            }
            return true;
        }
        if let Some(capnp) = self.capnp_state() {
            return capnp.send_to_peer(peer_id, event);
        }
        false
    }
}

/// Outcome of [`translate`]: fan out, reply to one peer, or do nothing.
/// A typed enum rather than two `Option`s + a bool keeps
/// `reader_loop`'s post-translate dispatch clean.
enum TranslatedAction {
    /// `Hello` swallowed, invalid op dropped after auditing, sender
    /// never announced a route, etc.
    Nothing,
    /// Forward `event` to the peers `route` addresses, skipping the
    /// originator.
    Fanout {
        event: CollabEvent,
        route: CrdtRoute,
    },
    /// Reply only to the originator — `FetchScrollback`'s paged history
    /// is of no interest to peers who didn't ask.
    ReplyToSource(CollabEvent),
    /// A `SignedSyncIncremental` that validated and was a `Compaction`
    /// checkpoint: local channel state is already mutated, but the
    /// envelope must still fan out so downstream peers evict their logs too.
    CompactionApplied {
        event: CollabEvent,
        route: CrdtRoute,
    },
}

/// Read one length-prefixed frame. Returns `None` on EOF / broken pipe
/// — the caller treats that as "peer disconnected" and unwinds.
fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    s.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Write one length-prefixed frame. Errors propagate so the caller can
/// evict the peer from the fan-out table.
fn write_frame(writer: &Mutex<UnixStream>, bytes: &[u8]) -> std::io::Result<()> {
    let mut guard = writer
        .lock()
        .map_err(|_| std::io::Error::other("writer mutex poisoned"))?;
    let len = bytes.len() as u32;
    guard.write_all(&len.to_le_bytes())?;
    guard.write_all(bytes)?;
    Ok(())
}

/// Log-and-drop for a frame whose sender never announced a route.
/// Factored out so every fail-closed branch in [`translate`] reads the
/// same and leaves the same trace behind.
fn unrouted(src_id: u64, kind: &str) -> TranslatedAction {
    tracing::debug!(
        "collab_broker: dropping {kind} from peer-{src_id}: no Hello, \
         so the frame has no route",
    );
    TranslatedAction::Nothing
}

/// Translate one `CollabRequest` (already deserialized off the wire) to
/// the action peers should observe. `Hello` is swallowed (see module
/// docs) → `Nothing`.
///
/// `src_route` is `None` when the sender never said `Hello`: every
/// fan-out case then resolves to `Nothing`, since the broker has no
/// route to address the frame with. Only [`CollabRequest::FetchScrollback`]
/// still answers, addressed to the requester by peer id rather than route.
fn translate(
    src_id: u64,
    src_user_id: &str,
    src_route: Option<&CrdtRoute>,
    req: CollabRequest,
    channels: &Arc<Mutex<BrokerChannels>>,
) -> TranslatedAction {
    match req {
        CollabRequest::Hello { .. } => TranslatedAction::Nothing,
        CollabRequest::SyncIncremental { data } => {
            let Some(route) = src_route.cloned() else {
                return unrouted(src_id, "SyncIncremental");
            };
            TranslatedAction::Fanout {
                event: CollabEvent::CRDTIncremental { data },
                route,
            }
        }
        CollabRequest::SyncCRDT { data } => {
            let Some(route) = src_route.cloned() else {
                return unrouted(src_id, "SyncCRDT");
            };
            TranslatedAction::Fanout {
                event: CollabEvent::CRDTUpdate { data },
                route,
            }
        }
        CollabRequest::Presence { key, payload } => {
            // Chrome traffic: cluster scope, since every app in the
            // cluster is a legitimate audience for "who else is here".
            let Some(route) = src_route.map(CrdtRoute::to_cluster_scope) else {
                return unrouted(src_id, "Presence");
            };
            TranslatedAction::Fanout {
                event: CollabEvent::RemotePresenceUpdate {
                    user_id: src_user_id.to_string(),
                    key,
                    payload,
                },
                route,
            }
        }
        CollabRequest::SignedSyncIncremental { envelope } => {
            let Some(route) = src_route.cloned() else {
                return unrouted(src_id, "SignedSyncIncremental");
            };
            // Validate against per-artifact `ChannelState` before fanning
            // out; default state has empty `authorised_signers`, the
            // secure default until a role table is registered.
            let artifact = envelope.artifact;
            let validation = {
                let Ok(mut ch) = channels.lock() else {
                    return TranslatedAction::Nothing;
                };
                let state = ch.state_for(artifact);
                validate_inbound_op(&envelope, state)
            };
            match validation {
                Ok(()) => {
                    // Record before applying a Compaction checkpoint, or
                    // the checkpoint's own envelope is evicted by its
                    // own application — its seq is strictly greater than
                    // `up_to_seq`, so recording first is safe.
                    let mut compaction_evicted: Option<usize> = None;
                    if let Ok(mut ch) = channels.lock() {
                        let state = ch.state_for(artifact);
                        state.record_validated_op((*envelope).clone());
                        if envelope.op_class == OpClass::Compaction {
                            match collab_types::parse_compaction_payload(&envelope) {
                                Ok(cp) => match state.apply_compaction(&cp) {
                                    Ok(n) => {
                                        compaction_evicted = Some(n);
                                        tracing::info!(
                                            artifact = ?artifact,
                                            up_to_seq = cp.up_to_seq,
                                            evicted = n,
                                            "collab_broker: applied compaction checkpoint",
                                        );
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            artifact = ?artifact,
                                            up_to_seq = cp.up_to_seq,
                                            "collab_broker: compaction rejected: {err}",
                                        );
                                    }
                                },
                                Err(err) => {
                                    // Passed signature + role checks but
                                    // malformed payload: skip compaction,
                                    // still fan the envelope out.
                                    tracing::warn!(
                                        artifact = ?artifact,
                                        "collab_broker: malformed compaction payload: {err}",
                                    );
                                }
                            }
                        }
                    }
                    if envelope.op_class == OpClass::Redact {
                        apply_redaction_from_envelope(&envelope, artifact, channels);
                    }
                    let event = CollabEvent::SignedCRDTIncremental {
                        envelope,
                        // Same identity this connection's presence is
                        // stamped with, or a delta and the pointer beside
                        // it show one person twice.
                        from_user_id: src_user_id.to_string(),
                    };
                    if compaction_evicted.is_some() {
                        TranslatedAction::CompactionApplied { event, route }
                    } else {
                        TranslatedAction::Fanout { event, route }
                    }
                }
                Err(ValidationError::OpAlreadyRedacted { target }) => {
                    // A late re-broadcast of an already-redacted op: don't
                    // fan out, but do push a marker entry so the seq stream
                    // stays gapless (a late-joiner's scrollback would
                    // otherwise hunt for an op we deleted by policy).
                    tracing::warn!(
                        artifact = ?artifact,
                        author = ?envelope.author,
                        seq = envelope.seq,
                        target = ?target,
                        "collab_broker: late-arriving op was already redacted; \
                         storing marker entry, not fanning out",
                    );
                    if let Ok(mut ch) = channels.lock() {
                        let state = ch.state_for(artifact);
                        let mut marker_envelope = (*envelope).clone();
                        if let Ok(marker_bytes) = RedactedMarker::v1().canonical() {
                            marker_envelope.payload = marker_bytes;
                        }
                        state.record_validated_op(marker_envelope);
                        ch.audit.record(
                            artifact,
                            AuditEntry {
                                envelope,
                                rejection: Some(ValidationError::OpAlreadyRedacted { target }),
                                at: std::time::SystemTime::now(),
                            },
                        );
                    }
                    TranslatedAction::Nothing
                }
                Err(reason) => {
                    tracing::warn!(
                        artifact = ?artifact,
                        author = ?envelope.author,
                        seq = envelope.seq,
                        "collab_broker: dropping invalid signed op: {reason}",
                    );
                    if let Ok(mut ch) = channels.lock() {
                        ch.audit.record(
                            artifact,
                            AuditEntry {
                                envelope,
                                rejection: Some(reason),
                                at: std::time::SystemTime::now(),
                            },
                        );
                    }
                    TranslatedAction::Nothing
                }
            }
        }
        CollabRequest::FetchScrollback {
            artifact,
            before_seq,
            max,
        } => {
            // Up to `max` ops in seq order; the batch goes only to the
            // requester.
            let (ops, exhausted) = {
                let Ok(mut ch) = channels.lock() else {
                    return TranslatedAction::Nothing;
                };
                let state = ch.state_for(artifact);
                state.scrollback_for(artifact, before_seq, max)
            };
            TranslatedAction::ReplyToSource(CollabEvent::ScrollbackBatch {
                artifact,
                ops: ops.into_iter().map(Box::new).collect(),
                exhausted,
            })
        }
    }
}

/// Handle the read side of one connected peer until EOF / broken pipe,
/// then evict it so concurrent writers stop targeting a dead stream.
///
/// Reaching other machines is [`PeerPublisher`]'s job — the same
/// instance the capnp transport holds, so a sandboxed app and a
/// JSON-dialect app take the identical route out of this host.
fn reader_loop(
    peer: Arc<Peer>,
    mut stream: UnixStream,
    table: Arc<PeerTable>,
    _peer_sync: Option<Arc<ShellNetwork>>,
    identity: Arc<LocalIdentity>,
    publisher: Arc<PeerPublisher>,
    channels: Arc<Mutex<BrokerChannels>>,
    pending_scrollback: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>>,
) {
    while let Some(frame) = read_frame(&mut stream) {
        let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
            tracing::debug!(
                "collab_broker: dropping malformed frame from peer-{} ({} bytes)",
                peer.id,
                frame.len()
            );
            continue;
        };
        // Capture the Hello route before translating — `translate`
        // consumes the request, and both fan-out and the Iroh bridge
        // need the announced address.
        if let CollabRequest::Hello {
            ref app_id,
            cluster_id,
            ref record,
            ref handle,
        } = req
        {
            if let Ok(mut guard) = peer.route.lock() {
                *guard = Some(
                    CrdtRoute::app(app_id.clone(), cluster_id.to_string())
                        .on_record(record.clone().unwrap_or_default()),
                );
            }
            if let Ok(mut guard) = peer.handle.lock() {
                *guard = handle.clone().filter(|h| !h.trim().is_empty());
            }
        }
        let src_route = peer.route();
        // Outbound bridge: publish before fanning locally (cloned since
        // `translate` still needs the original). No route, no publish —
        // same fail-closed rule as local fan-out.
        match (&src_route, &req) {
            (Some(route), CollabRequest::SyncIncremental { data }) => {
                publisher.document_incremental(route, Transport::Json, data.clone());
            }
            (Some(route), CollabRequest::SignedSyncIncremental { envelope }) => {
                // Validation happens receiver-side against their own
                // channel state — publishing is not vouching for it.
                publisher.signed_document(route, Transport::Json, (**envelope).clone());
            }
            (
                None,
                CollabRequest::SyncIncremental { .. } | CollabRequest::SignedSyncIncremental { .. },
            ) => {
                tracing::warn!(
                    "collab_broker: peer-{} sent a CRDT payload before saying \
                     Hello — it has no route, so nothing it types reaches \
                     another machine",
                    peer.id,
                );
            }
            (Some(route), CollabRequest::SyncCRDT { data }) => {
                // Merges two separately-created documents into one; sent
                // only in reply to `Arrived`, never on commit —
                // `ShellNetwork` holds that gate plus the gossip size cap.
                publisher.document_snapshot(route, Transport::Json, data.clone());
            }
            (Some(route), CollabRequest::Presence { key, payload }) => {
                // Legacy local input is normalized before it reaches peers;
                // new peer writes never embed a JSON request frame.
                if let Some(presence) = crate::shell::collab_presence::to_peer(key, payload) {
                    publisher.presence(route, Transport::Json, presence);
                }
            }
            (None, CollabRequest::SyncCRDT { .. } | CollabRequest::Presence { .. }) => {
                tracing::debug!(
                    "collab_broker: not republishing a {} from peer-{}: no \
                     Hello, so the frame has no route",
                    if matches!(&req, CollabRequest::SyncCRDT { .. }) {
                        "document snapshot"
                    } else {
                        "presence frame"
                    },
                    peer.id,
                );
            }
            _ => {}
        }
        // A `FetchScrollback` always replies first from the local op log
        // (via `translate`); if that reply is empty and peer-sync is
        // wired, also publish a cooperative request so remote peers can
        // answer via the drain thread as a follow-up `ScrollbackBatch`.
        let cooperative_fetch = if let CollabRequest::FetchScrollback {
            artifact,
            before_seq,
            max,
        } = &req
        {
            Some((*artifact, *before_seq, *max))
        } else {
            None
        };
        // Resolved per frame, not once at Hello — the room can name a
        // peer at any later point; `LocalIdentity` caches the lookup.
        let src_user_id = identity.resolve(src_route.as_ref(), peer.announced_handle().as_deref());
        match translate(peer.id, &src_user_id, src_route.as_ref(), req, &channels) {
            TranslatedAction::Nothing => {}
            TranslatedAction::Fanout { event, route } => {
                table.fanout_to_route(&event, peer.id, &route);
            }
            TranslatedAction::ReplyToSource(event) => {
                // Targeted reply; failure is silently dropped, the peer
                // can re-ask.
                let _ = table.send_to_peer(peer.id, &event);
                if let Some((art, before_seq, max)) = cooperative_fetch {
                    let is_empty = matches!(
                        &event,
                        CollabEvent::ScrollbackBatch { ops, .. } if ops.is_empty()
                    );
                    if is_empty {
                        // Remember who is waiting — a gossip response
                        // carries only an artifact id.
                        if let Some(route) = &src_route {
                            if let Ok(mut pending) = pending_scrollback.lock() {
                                let waiting = pending.entry(art).or_default();
                                if !waiting.contains(route) {
                                    waiting.push(route.clone());
                                }
                            }
                            publisher.scrollback_request(
                                route,
                                Transport::Json,
                                art,
                                before_seq,
                                max,
                            );
                        } else {
                            tracing::warn!(
                                "collab_broker: not publishing a cooperative \
                                 scrollback request for peer-{}: no Hello, so \
                                 the answer would have nowhere to go",
                                peer.id,
                            );
                        }
                    }
                }
            }
            TranslatedAction::CompactionApplied { event, route } => {
                // Compaction already applied locally in `translate`; fan
                // the envelope out so peers replicate the same eviction.
                table.fanout_to_route(&event, peer.id, &route);
            }
        }
    }
    tracing::debug!("collab_broker: peer-{} disconnected", peer.id);
    table.remove(peer.id);
}

/// Public handle returned by [`spawn`]. Owns the listener thread's join
/// handle so the broker doesn't get torn down mid-session; drop this
/// to start the cleanup chain. The socket file is best-effort-removed
/// on `Drop` too, so a crashed shell can restart without a stale-inode
/// collision.
pub struct BrokerHandle {
    socket_path: Option<PathBuf>,
    capnp_socket_path: Option<PathBuf>,
    /// `Option` so `Drop` can `take()` and `join()` it.
    listener_thread: Option<JoinHandle<()>>,
    /// Spawned only when there's a `ShellNetwork` to pump.
    drain_thread: Option<JoinHandle<()>>,
    drain_quit: Arc<AtomicBool>,
    /// `None` when the capnp interface is not bound; runs a tokio
    /// `LocalSet` driving capnp-rpc futures.
    capnp_thread: Option<JoinHandle<()>>,
    capnp_quit: Arc<AtomicBool>,
    /// Shared with reader threads so external callers (the Iroh
    /// bridge) can fan a remote event to every locally-connected peer.
    pub(crate) peers: Arc<PeerTable>,
    channels: Arc<Mutex<BrokerChannels>>,
}

impl BrokerHandle {
    /// Snapshot of currently-connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.peers.lock().map_or(0, |g| g.len())
    }

    /// Connected peers that have said `Hello`. Strictly ≤
    /// [`Self::peer_count`] — the difference is peers not yet addressable.
    pub fn announced_peer_count(&self) -> usize {
        self.peers
            .peers
            .lock()
            .map_or(0, |g| g.iter().filter(|p| p.route().is_some()).count())
    }

    /// Fan a `CollabEvent` to the peers `route` addresses. `except`
    /// skips one peer id, for the Iroh bridge. No "reach everyone"
    /// form exists — see [`PeerTable::fanout_to_route`].
    pub fn fanout_event(&self, event: &CollabEvent, except: Option<u64>, route: &CrdtRoute) {
        self.peers
            .fanout_to_route(event, except.unwrap_or(u64::MAX), route);
    }

    /// Subscribe this process to the rooms it hosts. The sink runs on
    /// the reader thread that decoded the frame: keep it short.
    pub fn subscribe_rooms(
        &self,
        rooms: Vec<String>,
        sink: Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>,
    ) -> Arc<RoomSubscription> {
        self.peers.subscribe_rooms(rooms, sink)
    }

    /// The bound Cap'n Proto socket. `None` after requesting one means
    /// the bind failed — `spawn_full` returning `Ok` only speaks for
    /// the JSON socket.
    pub fn capnp_socket(&self) -> Option<&std::path::Path> {
        self.capnp_socket_path.as_deref()
    }

    /// Shared handle to per-channel state + audit log. Tests use this
    /// to install a [`ChannelState`] before sending signed envelopes.
    pub fn channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.channels)
    }

    /// Install a populated [`ChannelState`] for one artifact id.
    pub fn register_channel_state(&self, artifact: ArtifactId, state: ChannelState) {
        if let Ok(mut ch) = self.channels.lock() {
            ch.register_channel_state(artifact, state);
        }
    }

    /// Audit-log entries for one artifact.
    pub fn audit_for(&self, artifact: ArtifactId) -> Vec<AuditEntry> {
        self.channels
            .lock()
            .map(|g| g.audit.for_artifact(artifact))
            .unwrap_or_default()
    }

    /// Structured redaction records for one artifact, independent of
    /// `audit_for` so the compliance surface skips filtering drops.
    pub fn redactions_for(&self, artifact: ArtifactId) -> Vec<RedactionRecord> {
        self.channels
            .lock()
            .map(|g| g.redactions.for_artifact(artifact))
            .unwrap_or_default()
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.drain_quit.store(true, Ordering::Relaxed);
        self.capnp_quit.store(true, Ordering::Relaxed);
        if let Some(path) = &self.socket_path {
            let _ = std::fs::remove_file(path); // best-effort
        }
        if let Some(p) = &self.capnp_socket_path {
            let _ = std::fs::remove_file(p);
        }
        // Can't reliably join the listener thread — blocked on
        // `accept()`, which socket removal alone doesn't unblock.
        // Detach; process exit reaps it.
        let _ = self.listener_thread.take();
        let _ = self.capnp_thread.take();
        if let Some(t) = self.drain_thread.take() {
            let _ = t.join();
        }
    }
}

/// Bind `socket_path` and detach the listener thread. Keep the returned
/// [`BrokerHandle`] alive for the session, or its socket gets `unlink`ed
/// out from under live apps. On bind failure returns the `io::Error` so
/// the caller can warn and continue without collab.
pub fn spawn(socket_path: PathBuf) -> std::io::Result<BrokerHandle> {
    spawn_full(socket_path, None, None)
}

/// Production app-facing broker: bind only the typed Cap'n Proto service.
/// The legacy JSON listener is not opened and no compatibility socket inode
/// is created. Peer sync and in-process subscribers use the same core state.
pub fn spawn_native(
    capnp_socket_path: PathBuf,
    peer_sync: Option<Arc<ShellNetwork>>,
) -> std::io::Result<BrokerHandle> {
    let identity = LocalIdentity::from_network(peer_sync.clone());
    let publisher = PeerPublisher::from_network(peer_sync.clone());
    let handle = spawn_inner(
        None,
        Some(capnp_socket_path),
        peer_sync,
        identity,
        publisher,
    )?;
    if handle.capnp_socket().is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "native collaboration service did not bind",
        ));
    }
    Ok(handle)
}

/// Legacy shim kept for existing hosted-mode callers; new callers that
/// want the capnp interface should call `spawn_full`.
pub fn spawn_with_peer_sync(
    socket_path: PathBuf,
    peer_sync: Option<Arc<ShellNetwork>>,
) -> std::io::Result<BrokerHandle> {
    spawn_full(socket_path, None, peer_sync)
}

/// Full-featured broker spawn.
///
/// - `socket_path`: legacy JSON socket (`QUICK_COLLAB_SOCK`), used by
///   compatibility tests and hosted development tools only.
/// - `capnp_socket_path`: optional native Cap'n Proto socket
///   (`QOS_SVC_COLLAB`). When `Some`, both transports share fanout
///   through the same [`PeerTable`], so either side reaches every
///   subscriber on either side.
/// - `peer_sync`: optional Iroh bridge.
pub fn spawn_full(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    peer_sync: Option<Arc<ShellNetwork>>,
) -> std::io::Result<BrokerHandle> {
    let identity = LocalIdentity::from_network(peer_sync.clone());
    // Built once and shared by both transports — two instances would
    // give "does a local edit leave this machine" two different answers.
    let publisher = PeerPublisher::from_network(peer_sync.clone());
    spawn_inner(
        Some(socket_path),
        capnp_socket_path,
        peer_sync,
        identity,
        publisher,
    )
}

/// [`spawn_full`] with the identity lookup supplied rather than built
/// from the network, so attribution can be tested against a room whose
/// answers are known without standing up Iroh.
pub fn spawn_full_with_identity(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    peer_sync: Option<Arc<ShellNetwork>>,
    identity: Arc<LocalIdentity>,
) -> std::io::Result<BrokerHandle> {
    let publisher = PeerPublisher::from_network(peer_sync.clone());
    spawn_inner(
        Some(socket_path),
        capnp_socket_path,
        peer_sync,
        identity,
        publisher,
    )
}

/// Same broker, with the outbound half supplied by the caller — lets a
/// test watch what actually reaches the wire without standing up
/// Iroh. `peer_sync` stays `None`; the inbound drain has its own coverage.
pub fn spawn_with_publisher(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    publisher: Arc<PeerPublisher>,
) -> std::io::Result<BrokerHandle> {
    let identity = LocalIdentity::from_network(None);
    spawn_inner(
        Some(socket_path),
        capnp_socket_path,
        None,
        identity,
        publisher,
    )
}

fn spawn_inner(
    socket_path: Option<PathBuf>,
    capnp_socket_path: Option<PathBuf>,
    peer_sync: Option<Arc<ShellNetwork>>,
    identity: Arc<LocalIdentity>,
    publisher: Arc<PeerPublisher>,
) -> std::io::Result<BrokerHandle> {
    let table = Arc::new(PeerTable::new());
    let channels: Arc<Mutex<BrokerChannels>> = Arc::new(Mutex::new(BrokerChannels::default()));
    // Who is waiting on a cooperative scrollback answer, keyed by
    // artifact — a `ScrollbackResponse` off gossip carries an
    // artifact id and nothing else, so without this the drain thread
    // would have to broadcast the batch to every local app.
    let pending_scrollback: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let listener_thread =
        if let Some(path) = &socket_path {
            // Best-effort unlink of a stale inode from a previous instance; if
            // bind still fails (e.g. wrong owner), that surfaces below.
            let _ = std::fs::remove_file(path);
            let listener = UnixListener::bind(path)?;
            let listener_table = Arc::clone(&table);
            let listener_peer_sync = peer_sync.clone();
            let listener_identity = Arc::clone(&identity);
            let listener_publisher = Arc::clone(&publisher);
            let listener_channels = Arc::clone(&channels);
            let listener_pending = Arc::clone(&pending_scrollback);
            Some(
            std::thread::Builder::new()
                .name("qos-collab-broker-listener".into())
                .spawn(move || {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let id = listener_table.next_peer_id();
                        let writer = match stream.try_clone() {
                            Ok(w) => w,
                            Err(e) => {
                                tracing::warn!(
                                    "collab_broker: try_clone failed for peer-{id}: {e}"
                                );
                                continue;
                            }
                        };
                        let peer = Arc::new(Peer {
                            id,
                            handle: Mutex::new(None),
                            writer: Mutex::new(writer),
                            route: Mutex::new(None),
                        });
                        listener_table.insert(Arc::clone(&peer));
                        tracing::info!(
                            "collab_broker: peer-{id} connected ({} total)",
                            listener_table.snapshot().len()
                        );
                        let reader_table = Arc::clone(&listener_table);
                        let reader_peer = Arc::clone(&peer);
                        let reader_peer_sync = listener_peer_sync.clone();
                        let reader_identity = Arc::clone(&listener_identity);
                        let reader_publisher = Arc::clone(&listener_publisher);
                        let reader_channels = Arc::clone(&listener_channels);
                        let reader_pending = Arc::clone(&listener_pending);
                        if let Err(e) = std::thread::Builder::new()
                            .name(format!("qos-collab-broker-peer-{id}"))
                            .spawn(move || {
                                reader_loop(
                                    reader_peer,
                                    stream,
                                    reader_table,
                                    reader_peer_sync,
                                    reader_identity,
                                    reader_publisher,
                                    reader_channels,
                                    reader_pending,
                                );
                            })
                        {
                            tracing::warn!(
                                "collab_broker: failed to spawn reader for peer-{id}: {e}"
                            );
                            listener_table.remove(id);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("collab_broker: accept failed: {e}; listener exiting");
                        break;
                    }
                }
            }
                })?,
        )
        } else {
            None
        };

    // Inbound-drain: when peer-sync is on, drain `ShellNetwork`'s queues
    // every 100ms and fan to local apps — unsigned/signed CRDT deltas,
    // snapshots, presence, and cooperative scrollback requests/responses.
    // Every remote entry is routed like a local one, by the `(app_id,
    // cluster_id)` the publishing host stamped on the wire.
    let drain_quit = Arc::new(AtomicBool::new(false));
    let drain_thread = peer_sync
        .as_ref()
        .map(|net| {
            let inbound = net.inbound_app_crdt_handle();
            let presence_inbound = net.inbound_app_presence_handle();
            let snapshot_inbound = net.inbound_app_crdt_snapshot_handle();
            let signed_inbound = net.inbound_app_crdt_signed_handle();
            let sb_req_inbound = net.inbound_scrollback_request_handle();
            let sb_resp_inbound = net.inbound_scrollback_response_handle();
            let drain_table = Arc::clone(&table);
            let drain_channels = Arc::clone(&channels);
            let drain_net = Arc::clone(net);
            let drain_pending = Arc::clone(&pending_scrollback);
            let quit = Arc::clone(&drain_quit);
            std::thread::Builder::new()
                .name("qos-collab-broker-drain".into())
                .spawn(move || {
                    // Name already announced per (peer, cluster) — said
                    // again only when it changes, not on every
                    // pointer-rate presence frame.
                    let mut announced: std::collections::HashMap<(String, u64), (String, u64)> =
                        std::collections::HashMap::new();
                    // What the room last said, on a re-ask cadence
                    // rather than every frame — the lookup takes the
                    // replica's lock and parses every peer id it holds.
                    let mut labels: std::collections::HashMap<String, (Instant, PeerLabels)> =
                        std::collections::HashMap::new();
                    while !quit.load(Ordering::Relaxed) {
                        // Drain inbound unsigned CRDT increments.
                        let drained: Vec<_> = {
                            let mut buf = match inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained {
                            tracing::info!(
                                target: "qos_collab_probe",
                                app = %entry.app_id,
                                cluster = %entry.cluster_id,
                                record = %entry.record_id,
                                bytes = entry.data.len(),
                                "PROBE drain incremental -> local fanout",
                            );
                            let event = CollabEvent::CRDTIncremental { data: entry.data };
                            // Deliver only to apps that announced the same
                            // app+cluster; no local peer skipped by id since
                            // the update originated outside this host.
                            let route =
                                CrdtRoute::record(entry.app_id, entry.cluster_id, entry.record_id);
                            drain_table.fanout_to_route(&event, u64::MAX, &route);
                        }

                        // Drain inbound whole-document snapshots, sent
                        // because this host's app announced `Arrived`.
                        // Delivered as `CRDTUpdate`, which the toolkit
                        // merges rather than assigns — a snapshot
                        // arriving after the local user typed must not
                        // discard it. Routed by the full record triple:
                        // a document merged into the wrong record is
                        // unrecoverable, not just noisy.
                        let drained_snapshots: Vec<_> = {
                            let mut buf = match snapshot_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_snapshots {
                            tracing::info!(
                                target: "qos_collab_probe",
                                app = %entry.app_id,
                                cluster = %entry.cluster_id,
                                record = %entry.record_id,
                                bytes = entry.data.len(),
                                "PROBE drain snapshot -> local fanout",
                            );
                            let event = CollabEvent::CRDTUpdate { data: entry.data };
                            let route =
                                CrdtRoute::record(entry.app_id, entry.cluster_id, entry.record_id);
                            drain_table.fanout_to_route(&event, u64::MAX, &route);
                        }

                        // Drain inbound presence, attributed to its
                        // sender. Cluster scope, matching a local
                        // presence frame (`CrdtRoute::to_cluster_scope`)
                        // — narrowing to a record would make a peer
                        // visible locally but invisible remotely.
                        let drained_presence: Vec<_> = {
                            let mut buf = match presence_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_presence {
                            let decoded = match entry.payload {
                                crate::shell::network::InboundAppPresencePayload::Typed(
                                    presence,
                                ) => crate::shell::collab_presence::from_peer(presence),
                                crate::shell::network::InboundAppPresencePayload::LegacyJson(
                                    frame,
                                ) => {
                                    // Read-only compatibility for peers predating typed
                                    // presence. No current sender writes this dialect.
                                    match serde_json::from_slice::<CollabRequest>(&frame) {
                                        Ok(CollabRequest::Presence { key, payload }) => {
                                            Some((key, payload))
                                        }
                                        _ => None,
                                    }
                                }
                            };
                            let Some((key, payload)) = decoded else {
                                continue;
                            };
                            // `Arrived` means the peer has no document
                            // yet; note it so the next snapshot this
                            // host's apps produce gets published.
                            if matches!(&payload, ui::collaboration::PresencePayload::Arrived) {
                                drain_net.note_snapshot_request(&entry.cluster_id);
                            }
                            let user_id = remote_participant_id(
                                &entry.cluster_id,
                                entry.peer,
                                &|c| drain_net.peer_labels(c),
                                &drain_table,
                                &mut labels,
                                &mut announced,
                            );
                            let route = CrdtRoute::cluster(entry.cluster_id.clone());
                            let event = CollabEvent::RemotePresenceUpdate {
                                user_id,
                                key,
                                payload,
                            };
                            drain_table.fanout_to_route(&event, u64::MAX, &route);
                        }

                        // Validate against local channel state, record on success,
                        // fan to local apps.
                        let drained_signed: Vec<_> = {
                            let mut buf = match signed_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_signed {
                            let envelope = *entry.envelope;
                            let artifact = envelope.artifact;
                            let validation = {
                                let Ok(mut ch) = drain_channels.lock() else {
                                    continue;
                                };
                                let state = ch.state_for(artifact);
                                validate_inbound_op(&envelope, state)
                            };
                            match validation {
                                Ok(()) => {
                                    if let Ok(mut ch) = drain_channels.lock() {
                                        let state = ch.state_for(artifact);
                                        state.record_validated_op(envelope.clone());
                                        if envelope.op_class == OpClass::Compaction
                                            && let Ok(cp) =
                                                collab_types::parse_compaction_payload(&envelope)
                                        {
                                            let _ = state.apply_compaction(&cp);
                                        }
                                    }
                                    if envelope.op_class == OpClass::Redact {
                                        apply_redaction_from_envelope(
                                            &envelope,
                                            artifact,
                                            &drain_channels,
                                        );
                                    }
                                    // Same id and room lookup the peer's
                                    // presence arrives under, so one
                                    // person reads as one participant.
                                    let from_user_id = remote_participant_id(
                                        &entry.cluster_id,
                                        entry.peer,
                                        &|c| drain_net.peer_labels(c),
                                        &drain_table,
                                        &mut labels,
                                        &mut announced,
                                    );
                                    let event = CollabEvent::SignedCRDTIncremental {
                                        envelope: Box::new(envelope),
                                        from_user_id,
                                    };
                                    // Same app + cluster stamp as unsigned
                                    // envelopes; an unaddressed one (too
                                    // old to stamp) is dropped, not shown
                                    // to every app.
                                    let route = CrdtRoute::record(
                                        entry.app_id.clone(),
                                        entry.cluster_id.clone(),
                                        entry.record_id.clone(),
                                    );
                                    drain_table.fanout_to_route(&event, u64::MAX, &route);
                                }
                                Err(ValidationError::OpAlreadyRedacted { target }) => {
                                    // Late-arriving op via Iroh, already
                                    // redacted: don't fan, push a marker
                                    // entry so the seq stream stays gapless.
                                    tracing::warn!(
                                        artifact = ?artifact,
                                        target = ?target,
                                        "collab_broker (net): late-arriving op already redacted",
                                    );
                                    if let Ok(mut ch) = drain_channels.lock() {
                                        let state = ch.state_for(artifact);
                                        let mut marker_envelope = envelope.clone();
                                        if let Ok(marker_bytes) = RedactedMarker::v1().canonical() {
                                            marker_envelope.payload = marker_bytes;
                                        }
                                        state.record_validated_op(marker_envelope);
                                        ch.audit.record(
                                            artifact,
                                            AuditEntry {
                                                envelope: Box::new(envelope),
                                                rejection: Some(
                                                    ValidationError::OpAlreadyRedacted { target },
                                                ),
                                                at: std::time::SystemTime::now(),
                                            },
                                        );
                                    }
                                }
                                Err(reason) => {
                                    tracing::warn!(
                                        "collab_broker (net): dropping invalid signed op: {reason}",
                                    );
                                    if let Ok(mut ch) = drain_channels.lock() {
                                        ch.audit.record(
                                            artifact,
                                            AuditEntry {
                                                envelope: Box::new(envelope),
                                                rejection: Some(reason),
                                                at: std::time::SystemTime::now(),
                                            },
                                        );
                                    }
                                }
                            }
                        }

                        // Drain inbound scrollback requests; publish a
                        // response only if we have matching ops, so the
                        // requester isn't spammed by every peer's "I
                        // don't have it either".
                        let drained_req: Vec<_> = {
                            let mut buf = match sb_req_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_req {
                            let (ops, _exhausted) = {
                                let Ok(mut ch) = drain_channels.lock() else {
                                    continue;
                                };
                                let state = ch.state_for(entry.artifact);
                                state.scrollback_for(entry.artifact, entry.before_seq, entry.max)
                            };
                            if !ops.is_empty() {
                                let boxed: Vec<Box<collab_types::SignedOp>> =
                                    ops.into_iter().map(Box::new).collect();
                                drain_net.publish_scrollback_response(entry.artifact, boxed);
                            }
                        }

                        // Drain inbound scrollback responses, deliver to
                        // the route(s) recorded in `pending_scrollback`;
                        // an unrequested response is dropped, not
                        // broadcast.
                        let drained_resp: Vec<_> = {
                            let mut buf = match sb_resp_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_resp {
                            let waiting = drain_pending
                                .lock()
                                .ok()
                                .and_then(|mut p| p.remove(&entry.artifact))
                                .unwrap_or_default();
                            if waiting.is_empty() {
                                tracing::debug!(
                                    artifact = ?entry.artifact,
                                    "collab_broker: dropping cooperative scrollback \
                                     response nobody asked for",
                                );
                                continue;
                            }
                            // Closing half of a cooperative round-trip;
                            // exhausted=true so the app doesn't re-ask.
                            let event = CollabEvent::ScrollbackBatch {
                                artifact: entry.artifact,
                                ops: entry.ops,
                                exhausted: true,
                            };
                            for route in waiting {
                                drain_table.fanout_to_route(&event, u64::MAX, &route);
                            }
                        }

                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
                .ok()
        })
        .flatten();

    // Optional capnp interface, attached to the shared `PeerTable` so
    // every fanout call also reaches capnp subscribers. A capnp bind
    // failure does NOT tear down the legacy broker.
    let capnp_quit = Arc::new(AtomicBool::new(false));
    let (capnp_thread, capnp_socket_ok) = match &capnp_socket_path {
        Some(path) => match crate::shell::collab_broker_capnp::spawn(
            path.clone(),
            Arc::clone(&table),
            Arc::clone(&channels),
            Arc::clone(&publisher),
            Arc::clone(&identity),
            Arc::clone(&capnp_quit),
        ) {
            Ok(handle) => (Some(handle), true),
            Err(e) => {
                tracing::warn!(
                    "collab_broker (capnp): bind {} failed ({e}); \
                     capnp transport disabled",
                    path.display()
                );
                (None, false)
            }
        },
        None => (None, false),
    };
    let effective_capnp_path = if capnp_socket_ok {
        capnp_socket_path
    } else {
        None
    };

    Ok(BrokerHandle {
        socket_path,
        capnp_socket_path: effective_capnp_path,
        listener_thread,
        drain_thread,
        drain_quit,
        capnp_thread,
        capnp_quit,
        peers: table,
        channels,
    })
}

/// The id a remote peer's traffic is attributed under, announcing the
/// room's name for it first if nobody has said it yet. Every path
/// showing a remote participant goes through here so they agree. The id
/// stays the peer handle even once named — a roster entry names a
/// participant, it does not recolour one. `labels` caches the room's
/// answer on [`LABEL_TTL`]; `announced` remembers what has been said, so
/// the roster publishes on change rather than every frame.
fn remote_participant_id(
    cluster_id: &str,
    peer: u64,
    labels_of: &dyn Fn(&str) -> PeerLabels,
    table: &PeerTable,
    labels: &mut HashMap<String, (Instant, PeerLabels)>,
    announced: &mut HashMap<(String, u64), (String, u64)>,
) -> String {
    // Attributed to the publishing host, not a local peer slot: this is
    // what colours the caret, so two peers must never collapse to one id.
    let user_id = format!("peer@{peer}");
    let now = Instant::now();
    let fresh = labels
        .get(cluster_id)
        .is_some_and(|(at, _)| now.duration_since(*at) < LABEL_TTL);
    if !fresh {
        labels.insert(cluster_id.to_string(), (now, labels_of(cluster_id)));
    }
    // What the room (i.e. whoever invited them) calls this peer. An
    // unnamed peer has no entry and stays labelled with the id above.
    let slot = (cluster_id.to_string(), peer);
    let known = labels
        .get(cluster_id)
        .and_then(|(_, m)| m.get(&peer))
        .map(String::as_str);
    let prior = announced.get(&slot);
    let generation = table.connections_seen();
    let say = roster_announcement(prior.map(|(n, _)| n.as_str()), known, &user_id)
        // "Changed" alone misses an app that connected after the name
        // was said — it was never in the audience, and nothing would
        // say it again. A connection this answer hasn't been said in
        // front of is itself a reason to say it again.
        .or_else(|| {
            let unheard = prior.is_some_and(|(_, at)| *at != generation);
            unheard.then(|| known.unwrap_or(&user_id).to_string())
        });
    if let Some(name) = say {
        announced.insert(slot, (name.clone(), generation));
        let roster = CollabEvent::Context {
            collaborators: vec![ui::collaboration::Collaborator {
                id: user_id.clone(),
                name,
                color: [0.0, 0.0, 0.0],
                // Always a remote network peer here — an agent joins
                // over the local socket and is marked there instead.
                is_agent: false,
                is_self: false,
            }],
            permissions: Vec::new(),
        };
        // Cluster scope: a roster is chrome traffic, every app in the
        // room is a legitimate audience.
        table.fanout_to_route(
            &roster,
            u64::MAX,
            &CrdtRoute::cluster(cluster_id.to_string()),
        );
    }
    user_id
}

/// What, if anything, to tell local apps this peer is called. `said` is
/// the name last announced, `known` what the room says now, `id` the
/// `peer@<handle>` presence frames carry.
///
/// - Room names the peer, differs from last said: say it.
/// - Room stops naming a peer that was named: say the id — an app holds
///   a name until told otherwise, so a revoked label must be taken back.
/// - Room has never named the peer: say nothing, the id is already shown.
fn roster_announcement(said: Option<&str>, known: Option<&str>, id: &str) -> Option<String> {
    let now = known.unwrap_or(id);
    match said {
        Some(before) if before == now => None,
        Some(_) => Some(now.to_string()),
        None if now == id => None,
        None => Some(now.to_string()),
    }
}

#[cfg(test)]
mod tests {
    /// A digest-shaped peer id, as presence frames carry it.
    const PEER: &str = "peer@5537458103208628978";

    #[test]
    fn a_named_peer_is_announced_and_then_left_alone() {
        assert_eq!(
            super::roster_announcement(None, Some("Xaver"), PEER),
            Some("Xaver".to_string()),
        );
        assert_eq!(
            super::roster_announcement(Some("Xaver"), Some("Xaver"), PEER),
            None
        );
    }

    #[test]
    fn an_unnamed_peer_is_not_announced() {
        assert_eq!(super::roster_announcement(None, None, PEER), None);
    }

    #[test]
    fn a_revoked_name_reverts_the_peer_to_its_id() {
        assert_eq!(
            super::roster_announcement(Some("Xaver"), None, PEER),
            Some(PEER.to_string()),
        );
        assert_eq!(super::roster_announcement(Some(PEER), None, PEER), None);
    }

    #[test]
    fn a_remote_peer_is_one_id_across_presence_and_deltas() {
        let table = Arc::new(super::PeerTable::new());
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let _sub = table.subscribe_rooms(
            vec!["room-1".to_string()],
            Box::new(move |event, _route| {
                if let CollabEvent::Context { collaborators, .. } = event {
                    for c in collaborators {
                        if let Ok(mut g) = sink_seen.lock() {
                            g.push((c.id.clone(), c.name.clone()));
                        }
                    }
                }
            }),
        );

        const HANDLE: u64 = 721_275_257_761_426_039;
        let room = |_c: &str| {
            let mut m = PeerLabels::new();
            m.insert(HANDLE, "Xaver".to_string());
            m
        };
        let mut labels = HashMap::new();
        let mut announced = HashMap::new();

        // Pointer first, then the delta the caret is drawn from.
        let from_presence = super::remote_participant_id(
            "room-1",
            HANDLE,
            &room,
            &table,
            &mut labels,
            &mut announced,
        );
        let from_delta = super::remote_participant_id(
            "room-1",
            HANDLE,
            &room,
            &table,
            &mut labels,
            &mut announced,
        );

        assert_eq!(
            from_presence, from_delta,
            "one peer, one id — a delta attributed differently from the \
             pointer beside it is the same person drawn twice",
        );
        assert_eq!(
            from_presence,
            format!("peer@{HANDLE}"),
            "and the id stays the peer handle once a name is known: a \
             roster entry names a participant, it does not recolour one",
        );

        let announcements = seen.lock().unwrap().clone();
        assert_eq!(
            announcements,
            vec![(format!("peer@{HANDLE}"), "Xaver".to_string())],
            "the room's name is announced exactly once, against the id \
             both frames arrived under",
        );
    }

    /// An app that starts after the name was said still learns it —
    /// "changes" is measured against what was said, not against who heard it.
    #[test]
    fn a_name_already_announced_is_said_again_for_an_app_that_missed_it() {
        let table = Arc::new(super::PeerTable::new());
        let heard: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let _sub = table.subscribe_rooms(
            vec!["room-1".to_string()],
            Box::new(move |event, _route| {
                if let CollabEvent::Context { collaborators, .. } = event {
                    for c in collaborators {
                        if let Ok(mut g) = sink.lock() {
                            g.push(c.name.clone());
                        }
                    }
                }
            }),
        );

        const HANDLE: u64 = 4_242_424_242;
        let room = |_c: &str| {
            let mut m = PeerLabels::new();
            m.insert(HANDLE, "Xaver".to_string());
            m
        };
        let mut labels = HashMap::new();
        let mut announced = HashMap::new();
        let mut say = || {
            super::remote_participant_id(
                "room-1",
                HANDLE,
                &room,
                &table,
                &mut labels,
                &mut announced,
            );
        };

        say();
        say();
        assert_eq!(
            heard.lock().unwrap().clone(),
            vec!["Xaver".to_string()],
            "a name that has not changed is not re-broadcast per frame",
        );

        let _ = table.next_peer_id();
        say();
        assert_eq!(
            heard.lock().unwrap().clone(),
            vec!["Xaver".to_string(), "Xaver".to_string()],
            "the app that just connected was not in the audience for the \
             first announcement, so the name has to be said again",
        );

        say();
        assert_eq!(
            heard.lock().unwrap().len(),
            2,
            "once everyone connected has heard it, it stops being said",
        );
    }

    /// Same claim as above, on the production drain rather than the
    /// helper it calls.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_remote_delta_reaches_an_app_under_the_name_the_room_wrote() {
        use collab_types::{ArtifactId, ArtifactKind, ChannelId, ChannelState, OpClass, Role};

        // A room this machine governs; the inviter has named the peer
        // that is about to send something.
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Design review");
        let room = tree.room_of(ws).expect("a fresh workspace mints a room");
        let owner = "quinten".to_string();
        tree.adopt_ownerless_workspaces(&owner);
        let xaver = iroh::SecretKey::generate().public();
        assert!(
            tree.name_peer(room, &owner, &xaver.to_string(), "Xaver"),
            "the room's authority may name a peer in it",
        );
        let xaver_handle = crate::network::behaviour::peer_liveness_handle(&xaver);
        let cluster = room.to_string();

        let socket_path = temp_sock("remote-delta-name");
        let (net, _cmd_rx) =
            crate::shell::network::shell_network_for_test(tree.shared(), 0xB0BAu64);
        let net = Arc::new(net);
        let handle = spawn_full(socket_path.clone(), None, Some(Arc::clone(&net)))
            .expect("broker should bind");

        let app = "dev.qos.word-processor";
        let mut watcher = connect_manager_as(&socket_path, app, &cluster);
        wait_for_announced(&handle, 1, Duration::from_millis(500));
        let _ = watcher.poll();

        let (sk, pk) = collab_types::generate_keypair();
        let artifact = ArtifactId::new();
        let ra = collab_fixtures::role_attestation(&sk, pk, pk, ChannelId::new(), Role::Owner);
        let att = ra.attestation.id;
        let mut state = ChannelState::default();
        state.authorised_signers.push(pk);
        state.artifacts.insert(artifact, ArtifactKind::Note);
        state.register_attestation(ra);
        handle.register_channel_state(artifact, state);

        net.handle_app_crdt_signed_message(
            xaver_handle,
            app.to_string(),
            cluster,
            String::new(),
            collab_fixtures::signed_op(
                &sk,
                pk,
                artifact,
                att,
                OpClass::Splice,
                b"typed on another machine".to_vec(),
                1,
            ),
        );

        let events = poll_until(&mut watcher, 2, Duration::from_secs(2));
        let attributions: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                CollabEvent::SignedCRDTIncremental { from_user_id, .. } => {
                    Some(from_user_id.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            attributions,
            vec![format!("peer@{xaver_handle}")],
            "the delta has to arrive under the id the same peer's \
             presence arrives under, got {events:?}",
        );

        let named: Vec<(String, String)> = events
            .iter()
            .filter_map(|e| match e {
                CollabEvent::Context { collaborators, .. } => collaborators
                    .first()
                    .map(|c| (c.id.clone(), c.name.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            named,
            vec![(format!("peer@{xaver_handle}"), "Xaver".to_string())],
            "and the room's name for it has to reach the app, against \
             that same id, got {events:?}",
        );

        drop(watcher);
        drop(handle);
    }

    /// A rename reaches the people watching, rather than the first name
    /// standing for the session.
    #[test]
    fn a_renamed_peer_is_announced_again() {
        assert_eq!(
            super::roster_announcement(Some("Xaver"), Some("Xaver Peeters"), PEER),
            Some("Xaver Peeters".to_string()),
        );
    }

    use super::*;
    use std::path::Path;
    use std::time::Duration;
    use ui::collaboration::{CollaborationManager, PresencePayload};
    use ui::presence::PresenceKey;

    /// Unique-per-test socket path so concurrent test runs don't collide.
    fn temp_sock(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qos-collab-broker-{}-{}-{:?}.sock",
            tag,
            std::process::id(),
            std::thread::current().id(),
        ))
    }

    /// Drain a manager until `want` events show up or `timeout` elapses.
    fn poll_until(
        mgr: &mut CollaborationManager,
        want: usize,
        timeout: Duration,
    ) -> Vec<CollabEvent> {
        let deadline = std::time::Instant::now() + timeout;
        let mut out: Vec<CollabEvent> = Vec::new();
        while std::time::Instant::now() < deadline && out.len() < want {
            out.extend(mgr.poll());
            if out.len() >= want {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        out
    }

    /// Poll until the broker has accepted `n` connections.
    fn wait_for_peers(handle: &BrokerHandle, n: usize, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if handle.peer_count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Block until `n` peers have announced a route.
    fn wait_for_announced(handle: &BrokerHandle, n: usize, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if handle.announced_peer_count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Connect a `CollaborationManager` to `path` announcing `app_id` in
    /// `cluster_id` via the env vars `auto_hello_env` reads.
    fn connect_manager_as(path: &PathBuf, app_id: &str, cluster_id: &str) -> CollaborationManager {
        // SAFETY: tests set these env vars serially before each call;
        // no other thread reads them between sets. Every test in this
        // module is `#[serial_test::serial(collab_broker)]`.
        unsafe {
            std::env::set_var("QUICK_COLLAB_SOCK", path);
            std::env::set_var("QUICK_APP_ID", app_id);
            std::env::set_var("QUICK_CLUSTER_ID", cluster_id);
        }
        CollaborationManager::new()
    }

    /// Like [`connect_manager_as`], announcing a **record** too.
    fn connect_manager_on_record(
        path: &PathBuf,
        app_id: &str,
        cluster_id: &str,
        record: &ui::record::RecordId,
    ) -> CollaborationManager {
        // SAFETY: as `connect_manager_as` — every test here is
        // `#[serial_test::serial(collab_broker)]`.
        unsafe {
            std::env::set_var("QUICK_COLLAB_SOCK", path);
            std::env::set_var("QUICK_APP_ID", app_id);
            std::env::set_var("QUICK_CLUSTER_ID", cluster_id);
        }
        CollaborationManager::for_record(record)
    }

    const TEST_CLUSTER: &str = "3f2b1c00-0000-4000-8000-000000000001";

    /// Connect a manager as a peer of the same app in the shared test cluster.
    fn connect_manager(path: &PathBuf) -> CollaborationManager {
        connect_manager_as(path, "test-app", TEST_CLUSTER)
    }

    /// Apply every CRDT payload that reached `mgr` into `doc`, the way an
    /// app's own `ui::App` does. Returns how many payloads landed.
    fn absorb(mgr: &mut CollaborationManager, doc: &mut automerge::AutoCommit) -> usize {
        absorb_within(mgr, doc, Duration::from_millis(400))
    }

    /// [`absorb`] with an explicit window — a wire-crossed frame needs
    /// longer than one fanned out inline (drain runs on a 100ms cadence).
    fn absorb_within(
        mgr: &mut CollaborationManager,
        doc: &mut automerge::AutoCommit,
        window: Duration,
    ) -> usize {
        let mut applied = 0;
        for event in poll_until(mgr, 1, window) {
            if let CollabEvent::CRDTIncremental { data } = event {
                doc.load_incremental(&data).expect("peer changes apply");
                applied += 1;
            }
        }
        applied
    }

    /// Read a root-level string out of a document.
    fn field(doc: &automerge::AutoCommit, key: &str) -> Option<String> {
        use automerge::ReadDoc;
        match doc.get(&automerge::ROOT, key) {
            Ok(Some((automerge::Value::Scalar(v), _))) => match v.as_ref() {
                automerge::ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// A document with one field set, plus the delta that produced it.
    fn edited(key: &str, value: &str) -> (automerge::AutoCommit, Vec<u8>) {
        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::new();
        doc.put(&automerge::ROOT, key, value).expect("put");
        let delta = doc.save_incremental();
        (doc, delta)
    }

    /// Two people editing different documents in one room must not merge.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn two_documents_in_one_room_do_not_merge() {
        let socket_path = temp_sock("record-isolation");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let report = ui::record::RecordId::for_file("/work/report.odt");
        let minutes = ui::record::RecordId::for_file("/work/minutes.odt");
        let mut alice = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &report,
        );
        let mut bob = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &minutes,
        );
        wait_for_peers(&handle, 2, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));
        let _ = alice.poll();
        let _ = bob.poll();

        let (_alice_doc, alice_delta) = edited("body", "the quarterly report");
        alice.send(CollabRequest::SyncIncremental { data: alice_delta });

        let (mut bob_doc, _) = edited("body", "the minutes");
        let applied = absorb(&mut bob, &mut bob_doc);
        assert_eq!(
            applied, 0,
            "no payload from another document should have reached bob"
        );
        assert_eq!(
            field(&bob_doc, "body").as_deref(),
            Some("the minutes"),
            "bob's document is still the minutes, not the report"
        );

        drop(alice);
        drop(bob);
        drop(handle);
    }

    /// Fold inbound full-state frames into `doc` by merging (not
    /// assigning), so a late snapshot doesn't discard local typing.
    fn absorb_snapshots_within(
        mgr: &mut CollaborationManager,
        doc: &mut automerge::AutoCommit,
        window: Duration,
    ) -> usize {
        let mut applied = 0;
        for event in poll_until(mgr, 1, window) {
            if let CollabEvent::CRDTUpdate { data } = event {
                let mut incoming = automerge::AutoCommit::load(&data).expect("snapshot loads");
                doc.merge(&mut incoming).expect("snapshot merges");
                applied += 1;
            }
        }
        applied
    }

    /// Two people who each *start* the same app (rather than opening a
    /// shared document) have two automerge documents with no common
    /// history; deltas alone would read as nothing. The whole-document
    /// snapshot sent on `Arrived` is what resolves it — driven here via
    /// the real inbound path: queued like `ShellNetwork::tick` queues one
    /// off gossip, drained, delivered as `CRDTUpdate`.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_document_created_here_adopts_the_state_of_the_one_it_meets() {
        let socket_path = temp_sock("snapshot-adoption");
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = crate::shell::network::shell_network_for_test(tree, 0xB0BAu64);
        let net = std::sync::Arc::new(net);
        let handle = spawn_full(socket_path.clone(), None, Some(Arc::clone(&net)))
            .expect("broker should bind");

        let record = ui::record::RecordId::for_file("/work/report.odt");
        let other = ui::record::RecordId::for_file("/work/elsewhere.odt");
        let mut on_record = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &record,
        );
        let mut on_other =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &other);
        wait_for_peers(&handle, 2, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));
        let _ = on_record.poll();
        let _ = on_other.poll();

        let (peer_doc, _) = edited("body", "written on another machine");
        let snapshot = {
            let mut d = peer_doc;
            d.save()
        };

        net.handle_app_crdt_snapshot_message(
            0xFACEu64,
            "dev.qos.word-processor".to_string(),
            TEST_CLUSTER.to_string(),
            record.as_str().to_string(),
            snapshot,
        );

        let mut here = automerge::AutoCommit::new();
        assert_eq!(
            absorb_snapshots_within(&mut on_record, &mut here, Duration::from_secs(2)),
            1,
            "the app on that record receives the snapshot"
        );
        assert_eq!(
            field(&here, "body").as_deref(),
            Some("written on another machine"),
            "the document created here adopted the peer's state"
        );

        // The record boundary holds for a whole document too — more so,
        // since a document merged into the wrong record can't be undone.
        let mut elsewhere = automerge::AutoCommit::new();
        assert_eq!(
            absorb_snapshots_within(&mut on_other, &mut elsewhere, Duration::from_secs(1)),
            0,
            "the app on a different record receives nothing"
        );
        assert_eq!(field(&elsewhere, "body"), None);

        drop(on_record);
        drop(on_other);
        drop(handle);
    }

    /// The record boundary holds across hosts too. Iroh gossip is a
    /// broadcast — which of a receiving host's apps see a frame is
    /// decided entirely by the route, so the record has to be on the
    /// wire (`NeighbourMsg::AppCrdt*`) and rebuilt on arrival.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_json_edit_reaches_the_same_publisher_the_capnp_transport_uses() {
        use crate::shell::collab_outbound::recording::{RecordingSink, Sent};
        use crate::shell::collab_outbound::{PeerPublishSink, PeerPublisher};

        let socket_path = temp_sock("json-shared-publisher");
        let sink = std::sync::Arc::new(RecordingSink::default());
        let publisher = PeerPublisher::new(sink.clone() as Arc<dyn PeerPublishSink>);
        let handle =
            spawn_with_publisher(socket_path.clone(), None, publisher).expect("broker should bind");

        let record = ui::record::RecordId::for_file("/work/shared.odt");
        let mut app = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &record,
        );
        wait_for_peers(&handle, 1, Duration::from_millis(500));
        wait_for_announced(&handle, 1, Duration::from_millis(500));
        let _ = app.poll();

        let (_, delta) = edited("body", "typed on this machine");
        app.send(CollabRequest::SyncIncremental {
            data: delta.clone(),
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let seen = loop {
            let seen = sink.snapshot();
            if !seen.is_empty() || std::time::Instant::now() > deadline {
                break seen;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            seen,
            vec![Sent::Incremental {
                app_id: "dev.qos.word-processor".to_string(),
                cluster_id: TEST_CLUSTER.to_string(),
                record_id: record.as_str().to_string(),
                data: delta,
            }],
            "the JSON transport must publish through the shared outbound \
             path, carrying the same route it fanned out on",
        );
        drop(app);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_remote_frame_reaches_only_the_record_it_was_stamped_with() {
        let socket_path = temp_sock("record-over-the-wire");
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let (net, _cmd_rx) = crate::shell::network::shell_network_for_test(tree, 0xB0BAu64);
        let net = std::sync::Arc::new(net);
        let handle = spawn_full(socket_path.clone(), None, Some(Arc::clone(&net)))
            .expect("broker should bind");

        let theirs = ui::record::RecordId::for_file("/work/theirs.odt");
        let mine = ui::record::RecordId::for_file("/work/mine.odt");
        let mut on_theirs = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &theirs,
        );
        let mut on_mine =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &mine);
        wait_for_peers(&handle, 2, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));
        let _ = on_theirs.poll();
        let _ = on_mine.poll();

        // A frame as it arrives off the wire, stamped with its record —
        // queued like `ShellNetwork::tick` queues an `AppCrdtIncremental`.
        let (_, delta) = edited("body", "written on another machine");
        net.handle_app_crdt_message(
            0xFACEu64,
            "dev.qos.word-processor".to_string(),
            TEST_CLUSTER.to_string(),
            theirs.as_str().to_string(),
            delta,
        );

        let mut theirs_doc = automerge::AutoCommit::new();
        assert_eq!(
            absorb_within(&mut on_theirs, &mut theirs_doc, Duration::from_secs(2)),
            1,
            "the app on that record receives it"
        );
        assert_eq!(
            field(&theirs_doc, "body").as_deref(),
            Some("written on another machine")
        );

        let mut mine_doc = automerge::AutoCommit::new();
        assert_eq!(
            absorb_within(&mut on_mine, &mut mine_doc, Duration::from_secs(1)),
            0,
            "the app on a different record receives nothing"
        );
        assert_eq!(field(&mine_doc, "body"), None);

        drop(on_theirs);
        drop(on_mine);
        drop(handle);
    }

    /// Sharing one record shares **only** that record — the sender holds
    /// two documents open, the receiver was given one of them.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_peer_given_one_record_receives_only_that_record() {
        let socket_path = temp_sock("record-share-one");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let shared = ui::record::RecordId::for_file("/work/shared.odt");
        let private = ui::record::RecordId::for_file("/work/private.odt");

        let mut sender_shared = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &shared,
        );
        let mut sender_private = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &private,
        );
        let mut receiver = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &shared,
        );

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        wait_for_announced(&handle, 3, Duration::from_millis(500));
        let _ = sender_shared.poll();
        let _ = sender_private.poll();
        let _ = receiver.poll();

        let (_, shared_delta) = edited("shared_body", "everyone may read this");
        let (_, private_delta) = edited("private_body", "nobody else may read this");
        sender_shared.send(CollabRequest::SyncIncremental { data: shared_delta });
        sender_private.send(CollabRequest::SyncIncremental {
            data: private_delta,
        });

        let mut received = automerge::AutoCommit::new();
        let applied = absorb(&mut receiver, &mut received);
        assert_eq!(
            applied, 1,
            "exactly the shared record's delta should have arrived"
        );
        assert_eq!(
            field(&received, "shared_body").as_deref(),
            Some("everyone may read this"),
            "the receiver converged on the record it was given"
        );
        assert_eq!(
            field(&received, "private_body"),
            None,
            "and its document holds nothing from the record it was not given"
        );

        drop(sender_shared);
        drop(sender_private);
        drop(receiver);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_room_subscription_hears_every_app_in_the_room_as_one_stream() {
        // The property the compositor's peer cursors rest on: presence
        // widens to cluster scope, so two apps in one room reach one
        // subscriber (one cursor moving, not a second appearing), while
        // a third app in a different room must not be heard at all.
        let socket_path = temp_sock("room-sub");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let other_room = "3f2b1c00-0000-4000-8000-0000000000ff";
        let heard: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_heard = Arc::clone(&heard);
        let _sub = handle.subscribe_rooms(
            vec![TEST_CLUSTER.to_string()],
            Box::new(move |event, route| {
                if let CollabEvent::RemotePresenceUpdate { payload, .. } = event
                    && let ui::collaboration::PresencePayload::Pointer { x, .. } = payload
                {
                    sink_heard
                        .lock()
                        .unwrap()
                        .push((route.cluster_id.clone(), format!("{x}")));
                }
            }),
        );

        let mut files = connect_manager_as(&socket_path, "dev.qos.file-explorer", TEST_CLUSTER);
        let mut words = connect_manager_as(&socket_path, "dev.qos.word-processor", TEST_CLUSTER);
        let mut elsewhere = connect_manager_as(&socket_path, "dev.qos.word-processor", other_room);
        wait_for_peers(&handle, 3, Duration::from_millis(500));
        wait_for_announced(&handle, 3, Duration::from_millis(500));

        let key = PresenceKey::Workspace {
            id: TEST_CLUSTER.into(),
        };
        files.send(CollabRequest::Presence {
            key: key.clone(),
            payload: ui::collaboration::PresencePayload::Pointer { x: 1.0, y: 1.0 },
        });
        words.send(CollabRequest::Presence {
            key: key,
            payload: ui::collaboration::PresencePayload::Pointer { x: 2.0, y: 2.0 },
        });
        elsewhere.send(CollabRequest::Presence {
            key: PresenceKey::Workspace {
                id: other_room.into(),
            },
            payload: ui::collaboration::PresencePayload::Pointer { x: 3.0, y: 3.0 },
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline && heard.lock().unwrap().len() < 2 {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Give the third frame the same chance as the two that should
        // arrive, or "it did not leak" just means "it was slower".
        std::thread::sleep(Duration::from_millis(150));

        let got = heard.lock().unwrap().clone();
        let xs: Vec<&str> = got.iter().map(|(_, x)| x.as_str()).collect();
        assert!(
            xs.contains(&"1") && xs.contains(&"2"),
            "both apps in the room should reach one subscription, got {got:?}"
        );
        assert!(
            !xs.contains(&"3"),
            "an app in another room must not reach it, got {got:?}"
        );
        assert!(
            got.iter().all(|(room, _)| room == TEST_CLUSTER),
            "every frame should be delivered on the room it was published in, got {got:?}"
        );

        drop(files);
        drop(words);
        drop(elsewhere);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn broker_fans_out_to_three_peers() {
        // Prove N=3 fan-out, not just N=2.
        let socket_path = temp_sock("fanout3");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let mut peer_b = connect_manager(&socket_path);
        let mut peer_c = connect_manager(&socket_path);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        assert_eq!(handle.peer_count(), 3, "broker should see all three peers");
        wait_for_announced(&handle, 3, Duration::from_millis(500));

        let _ = peer_a.poll();
        let _ = peer_b.poll();
        let _ = peer_c.poll();

        peer_a.send(CollabRequest::SyncIncremental {
            data: b"hello-world".to_vec(),
        });

        let events_b = poll_until(&mut peer_b, 1, Duration::from_millis(500));
        let events_c = poll_until(&mut peer_c, 1, Duration::from_millis(500));

        let crdt_b: Vec<&Vec<u8>> = events_b
            .iter()
            .filter_map(|e| match e {
                CollabEvent::CRDTIncremental { data } => Some(data),
                _ => None,
            })
            .collect();
        let crdt_c: Vec<&Vec<u8>> = events_c
            .iter()
            .filter_map(|e| match e {
                CollabEvent::CRDTIncremental { data } => Some(data),
                _ => None,
            })
            .collect();

        assert_eq!(
            crdt_b.len(),
            1,
            "peer B should receive exactly one CRDTIncremental, got {events_b:?}"
        );
        assert_eq!(
            crdt_c.len(),
            1,
            "peer C should receive exactly one CRDTIncremental, got {events_c:?}"
        );
        assert_eq!(crdt_b[0].as_slice(), b"hello-world");
        assert_eq!(crdt_c[0].as_slice(), b"hello-world");

        let events_a = peer_a.poll();
        assert!(
            !events_a.iter().any(|e| matches!(
                e,
                CollabEvent::CRDTIncremental { data } if data.as_slice() == b"hello-world"
            )),
            "peer A must not see its own SyncIncremental echoed back, got {events_a:?}"
        );

        // Presence attributed to a participant, same on every screen. No
        // room here to name anyone, so B is called this machine's own id.
        peer_b.send(CollabRequest::Presence {
            key: PresenceKey::Workspace { id: "42".into() },
            payload: PresencePayload::Arrived,
        });
        let events_a = poll_until(&mut peer_a, 1, Duration::from_millis(500));
        let events_c = poll_until(&mut peer_c, 1, Duration::from_millis(500));

        let arrived = |events: &[CollabEvent]| -> Option<String> {
            events.iter().find_map(|e| match e {
                CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key: PresenceKey::Workspace { id },
                    payload: PresencePayload::Arrived,
                } if id == "42" => Some(user_id.clone()),
                _ => None,
            })
        };
        let saw_a = arrived(&events_a)
            .unwrap_or_else(|| panic!("peer A saw no Workspace 42 Arrived, got {events_a:?}"));
        let saw_c = arrived(&events_c)
            .unwrap_or_else(|| panic!("peer C saw no Workspace 42 Arrived, got {events_c:?}"));

        assert_eq!(
            saw_a, saw_c,
            "one participant must read the same on every screen",
        );
        assert!(
            !saw_a.starts_with("peer-"),
            "presence is attributed to a connection slot ({saw_a}) rather \
             than to whoever is on the other end of it",
        );
        assert_eq!(
            saw_a,
            crate::shell::state::local_user_id(),
            "with no room to ask, a local app's traffic belongs to the \
             person at this machine",
        );

        drop(peer_a);
        drop(peer_b);
        drop(peer_c);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn broker_drops_dead_peer_from_fanout() {
        // If a peer disconnects, subsequent fan-outs must not block on
        // the dead writer — they should reach the surviving peer.
        let socket_path = temp_sock("dropdead");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let peer_b = connect_manager(&socket_path);
        let mut peer_c = connect_manager(&socket_path);
        wait_for_peers(&handle, 3, Duration::from_millis(500));
        wait_for_announced(&handle, 3, Duration::from_millis(500));

        drop(peer_b);
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline && handle.peer_count() > 2 {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(handle.peer_count(), 2, "broker should evict dead peer B");

        let _ = peer_a.poll();
        let _ = peer_c.poll();
        peer_a.send(CollabRequest::SyncIncremental {
            data: b"after-drop".to_vec(),
        });
        let events_c = poll_until(&mut peer_c, 1, Duration::from_millis(500));
        let saw = events_c.iter().any(|e| {
            matches!(
                e,
                CollabEvent::CRDTIncremental { data } if data.as_slice() == b"after-drop"
            )
        });
        assert!(
            saw,
            "peer C should still receive after B died, got {events_c:?}"
        );

        drop(peer_a);
        drop(peer_c);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn broker_translates_sync_crdt_to_crdt_update() {
        // `SyncCRDT` is a full-state replacement; the broker must map
        // it to `CRDTUpdate` (not `CRDTIncremental`).
        let socket_path = temp_sock("full-state");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let mut peer_b = connect_manager(&socket_path);
        wait_for_peers(&handle, 2, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));

        let _ = peer_a.poll();
        let _ = peer_b.poll();

        peer_a.send(CollabRequest::SyncCRDT {
            data: vec![0xDE, 0xAD],
        });
        let events_b = poll_until(&mut peer_b, 1, Duration::from_millis(500));
        let saw = events_b.iter().any(|e| {
            matches!(
                e,
                CollabEvent::CRDTUpdate { data } if data.as_slice() == [0xDE, 0xAD]
            )
        });
        assert!(saw, "peer B should see CRDTUpdate, got {events_b:?}");

        drop(peer_a);
        drop(peer_b);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn crdt_deltas_do_not_cross_app_boundaries() {
        // Two instances of app-alpha and one of app-beta in one cluster.
        // Alpha-1's delta must reach alpha-2 (or this would just pass by
        // breaking delivery) and never beta.
        let socket_path = temp_sock("app-isolation");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut alpha_1 = connect_manager_as(&socket_path, "app-alpha", TEST_CLUSTER);
        let mut alpha_2 = connect_manager_as(&socket_path, "app-alpha", TEST_CLUSTER);
        let mut beta = connect_manager_as(&socket_path, "app-beta", TEST_CLUSTER);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        assert_eq!(handle.peer_count(), 3, "broker should see all three peers");
        wait_for_announced(&handle, 3, Duration::from_millis(500));

        let _ = alpha_1.poll();
        let _ = alpha_2.poll();
        let _ = beta.poll();

        alpha_1.send(CollabRequest::SyncIncremental {
            data: b"alpha-private".to_vec(),
        });

        let events_alpha_2 = poll_until(&mut alpha_2, 1, Duration::from_millis(500));
        let alpha_2_crdt: Vec<&Vec<u8>> = events_alpha_2
            .iter()
            .filter_map(|e| match e {
                CollabEvent::CRDTIncremental { data } => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(
            alpha_2_crdt.len(),
            1,
            "the second app-alpha instance must receive exactly one \
             CRDTIncremental, got {events_alpha_2:?}"
        );
        assert_eq!(alpha_2_crdt[0].as_slice(), b"alpha-private");

        let events_beta = poll_until(&mut beta, 1, Duration::from_millis(500));
        assert!(
            !events_beta
                .iter()
                .any(|e| matches!(e, CollabEvent::CRDTIncremental { .. })),
            "app-beta must receive no CRDT payload from app-alpha, got {events_beta:?}"
        );

        drop(alpha_1);
        drop(alpha_2);
        drop(beta);
        drop(handle);
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn crdt_deltas_do_not_cross_cluster_boundaries() {
        let socket_path = temp_sock("cluster-isolation");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let other_cluster = "3f2b1c00-0000-4000-8000-000000000002";
        let mut same_cluster_sender = connect_manager_as(&socket_path, "app-alpha", TEST_CLUSTER);
        let mut same_cluster_peer = connect_manager_as(&socket_path, "app-alpha", TEST_CLUSTER);
        let mut other_cluster_peer = connect_manager_as(&socket_path, "app-alpha", other_cluster);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        wait_for_announced(&handle, 3, Duration::from_millis(500));

        let _ = same_cluster_sender.poll();
        let _ = same_cluster_peer.poll();
        let _ = other_cluster_peer.poll();

        same_cluster_sender.send(CollabRequest::SyncIncremental {
            data: b"cluster-one-only".to_vec(),
        });

        let events_same = poll_until(&mut same_cluster_peer, 1, Duration::from_millis(500));
        assert!(
            events_same.iter().any(|e| matches!(
                e,
                CollabEvent::CRDTIncremental { data }
                    if data.as_slice() == b"cluster-one-only"
            )),
            "the peer in the sender's cluster must receive the delta, got {events_same:?}"
        );

        let events_other = poll_until(&mut other_cluster_peer, 1, Duration::from_millis(500));
        assert!(
            !events_other
                .iter()
                .any(|e| matches!(e, CollabEvent::CRDTIncremental { .. })),
            "a peer in a different cluster must receive no CRDT payload, got {events_other:?}"
        );

        drop(same_cluster_sender);
        drop(same_cluster_peer);
        drop(other_cluster_peer);
        drop(handle);
    }

    /// The nil room addresses nobody, in either direction — not the
    /// empty string, so every workspace still carrying the unminted
    /// sentinel doesn't collapse into one shared room.
    #[test]
    fn the_unminted_room_addresses_nobody() {
        let unminted = CrdtRoute::app(
            "dev.qos.notes",
            crate::shell::state::RoomId::nil().to_string(),
        );
        let real = CrdtRoute::app(
            "dev.qos.notes",
            crate::shell::state::RoomId::new().to_string(),
        );

        assert!(
            !unminted.delivers_to(&unminted),
            "two workspaces that have not yet been given an identity are not \
             the same room"
        );
        assert!(
            !unminted.delivers_to(&real),
            "a sender with no room identity must reach nobody"
        );
        assert!(
            !real.delivers_to(&unminted),
            "a subscriber with no room identity must receive nothing"
        );
        assert!(
            real.delivers_to(&real.clone()),
            "and a real room must still deliver to itself, or this rule has \
             broken delivery outright"
        );
    }

    #[test]
    #[serial_test::serial(collab_broker)]
    fn silent_client_receives_nothing() {
        // Fail-closed: no Hello means no route, must not fall back to
        // receiving everything. Driven over a raw socket since
        // CollaborationManager always auto-Hellos on construct.
        let socket_path = temp_sock("silent-client");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut talker = connect_manager(&socket_path);
        let mut silent = UnixStream::connect(&socket_path).expect("silent client connects");
        silent
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("set read timeout");
        // Proves fan-out is live: rules out "delta never left the sender".
        let mut listener_peer = connect_manager(&socket_path);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));
        assert_eq!(
            handle.announced_peer_count(),
            2,
            "only the two managers announce a route; the silent client never does",
        );
        let _ = talker.poll();
        let _ = listener_peer.poll();

        talker.send(CollabRequest::SyncIncremental {
            data: b"for-announced-peers".to_vec(),
        });

        let heard = poll_until(&mut listener_peer, 1, Duration::from_millis(500));
        assert!(
            heard.iter().any(|e| matches!(
                e,
                CollabEvent::CRDTIncremental { data }
                    if data.as_slice() == b"for-announced-peers"
            )),
            "the announced peer must receive the delta, got {heard:?}"
        );

        let mut len_buf = [0u8; 4];
        let read = silent.read_exact(&mut len_buf);
        assert!(
            read.is_err(),
            "a client that never announced a route must receive no frame, \
             got a {} byte length prefix",
            u32::from_le_bytes(len_buf),
        );

        let req = serde_json::to_vec(&CollabRequest::SyncIncremental {
            data: b"from-the-silent-one".to_vec(),
        })
        .expect("serialize request");
        silent
            .write_all(&(req.len() as u32).to_le_bytes())
            .expect("write len");
        silent.write_all(&req).expect("write body");

        let after = poll_until(&mut listener_peer, 1, Duration::from_millis(500));
        assert!(
            !after.iter().any(|e| matches!(
                e,
                CollabEvent::CRDTIncremental { data }
                    if data.as_slice() == b"from-the-silent-one"
            )),
            "a delta from a client that never announced a route must not be \
             fanned out, got {after:?}"
        );

        drop(talker);
        drop(silent);
        drop(listener_peer);
        drop(handle);
    }

    // -----------------------------------------------------------------
    // Launch → sandbox env → collaboration room.
    //
    // Tests below drive the production chain: launcher argv → bwrap argv
    // → child env → broker delivery, nothing stubbed, so a break
    // anywhere surfaces as a delivery failure here.
    // -----------------------------------------------------------------

    /// Write a minimal real package manifest and return the matching
    /// [`InstalledApp`].
    fn staged_app(dir: &Path, package_id: &str) -> crate::shell::launcher::InstalledApp {
        let manifest_path = dir.join(format!("{package_id}.toml"));
        std::fs::write(
            &manifest_path,
            format!(
                "package_id = \"{package_id}\"\n\
                 version = \"1.0.0\"\n\
                 [entrypoint]\n\
                 bin = \"bin/app\"\n\
                 [sandbox]\n",
            ),
        )
        .expect("stage manifest");
        crate::shell::launcher::InstalledApp {
            package_id: package_id.to_string(),
            display_name: package_id.to_string(),
            category: None,
            version: "1.0.0".to_string(),
            manifest_path,
            payload_root: dir.join("payload"),
            has_gui: true,
            opens_extensions: Vec::new(),
        }
    }

    /// Read one flag's value out of an argv, or panic naming the flag.
    fn flag_value(argv: &[String], flag: &str) -> String {
        argv.windows(2).find(|w| w[0] == flag).map_or_else(
            || panic!("argv is missing {flag}: {argv:?}"),
            |w| w[1].clone(),
        )
    }

    /// The environment a child opened into `workspace` actually
    /// receives, derived by walking the real launch chain: the
    /// `qos-launcher` invocation → its parsed flags → the bwrap argv the
    /// launcher builds from the manifest, workspace spliced in exactly
    /// as `run_with` does → every `--setenv K V` in that argv, which is
    /// what `auto_hello_env` reads.
    fn sandbox_env_for_launch(
        dir: &Path,
        package_id: &str,
        room: crate::shell::state::RoomId,
    ) -> Vec<(String, String)> {
        let app = staged_app(dir, package_id);
        let launch = crate::shell::launcher::build_launch_command(
            &app,
            Path::new("/usr/libexec/qos-launcher"),
            Path::new("/run/user/1000/wayland-1"),
            room,
            None,
            None,
            None,
        );

        let manifest_path = PathBuf::from(flag_value(&launch.args, "--manifest"));
        let payload_root = PathBuf::from(flag_value(&launch.args, "--payload-root"));
        let workspace_id = flag_value(&launch.args, "--workspace-id");

        let manifest = launcher::manifest::read_manifest(&manifest_path)
            .expect("launcher parses the manifest");
        let mut argv = launcher::sandbox::build_bwrap_argv(
            &manifest,
            &payload_root,
            None,
            &[],
            None,
            None,
            &launcher::capabilities::CapabilityPlan::empty(),
        );
        // Same splice `run_with` performs: before the trailing entrypoint
        // token, so bwrap still reads it as a flag.
        let entry = argv.pop().expect("argv non-empty");
        launcher::sandbox::push_workspace_id(&mut argv, &workspace_id);
        argv.push(entry);

        let mut env = Vec::new();
        let mut i = 0;
        while i + 2 < argv.len() {
            if argv[i] == "--setenv" {
                env.push((argv[i + 1].clone(), argv[i + 2].clone()));
                i += 3;
            } else {
                i += 1;
            }
        }
        env
    }

    /// Connect a manager under the env a real launch into `room` would
    /// give the app. `instance` (→ `QUICK_APP_ID`) is deliberately
    /// different per peer, to prove it can't influence routing.
    fn connect_as_launched_app(
        sock: &PathBuf,
        dir: &Path,
        package_id: &str,
        room: crate::shell::state::RoomId,
        instance: &str,
    ) -> CollaborationManager {
        let env = sandbox_env_for_launch(dir, package_id, room);
        // SAFETY: the tests that call this hold both the `collab_broker` and
        // the `process_env` serial keys. The second one matters: these are the
        // launcher's own variables, and tests elsewhere in the crate (the
        // document-open handler's, for one) *read* them. Holding only
        // `collab_broker` serialised this module against itself and left those
        // readers racing a write from here.
        unsafe {
            std::env::set_var("QUICK_COLLAB_SOCK", sock);
            std::env::set_var("QUICK_APP_ID", instance);
            std::env::remove_var("QUICK_CLUSTER_ID");
            std::env::remove_var("QOS_PACKAGE_ID");
            std::env::remove_var("QOS_WORKSPACE_ID");
            for (k, v) in &env {
                std::env::set_var(k, v);
            }
        }
        let mgr = CollaborationManager::new();
        // Leave no launched-app env behind for the next manager.
        unsafe {
            for (k, _) in &env {
                std::env::remove_var(k);
            }
        }
        mgr
    }

    /// Every `CRDTIncremental` payload in `events`.
    fn crdt_payloads(events: &[CollabEvent]) -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|e| match e {
                CollabEvent::CRDTIncremental { data } => Some(data.clone()),
                _ => None,
            })
            .collect()
    }

    /// Scratch directory for staged manifests, unique per test.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qos-launch-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    #[serial_test::serial(collab_broker, process_env)]
    fn apps_launched_into_the_same_workspace_exchange_deltas() {
        let dir = scratch_dir("same-ws");
        let socket_path = temp_sock("same-workspace");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(7),
            "here",
        );
        // Two windows of one app; their per-instance dev ids differ.
        let mut first =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", workspace.room, "alice");
        let mut second =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", workspace.room, "bob");

        wait_for_announced(&handle, 2, Duration::from_secs(2));
        let _ = first.poll();
        let _ = second.poll();

        first.send(CollabRequest::SyncIncremental {
            data: b"same-room".to_vec(),
        });

        let received = poll_until(&mut second, 1, Duration::from_secs(1));
        assert_eq!(
            crdt_payloads(&received),
            vec![b"same-room".to_vec()],
            "two windows of one app opened in the same workspace must \
             exchange deltas — their QUICK_APP_ID differs, and that must \
             not split the room. Got {received:?}"
        );

        drop(first);
        drop(second);
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial_test::serial(collab_broker, process_env)]
    fn apps_launched_into_different_workspaces_do_not_exchange_deltas() {
        // The half that matters: a workspace is a room, so app-in-7 must
        // never see app-in-8's document.
        let dir = scratch_dir("diff-ws");
        let socket_path = temp_sock("diff-workspace");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let here = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(7),
            "here",
        );
        let elsewhere = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(8),
            "elsewhere",
        );

        // Second peer in `here` is the control, ruling out "delta never
        // left the sender" as the reason the other workspace heard nothing.
        let mut sender =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", here.room, "alice");
        let mut same_workspace =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", here.room, "bob");
        let mut other_workspace =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", elsewhere.room, "carol");

        wait_for_announced(&handle, 3, Duration::from_secs(2));
        let _ = sender.poll();
        let _ = same_workspace.poll();
        let _ = other_workspace.poll();

        sender.send(CollabRequest::SyncIncremental {
            data: b"workspace-seven-only".to_vec(),
        });

        let heard = poll_until(&mut same_workspace, 1, Duration::from_secs(1));
        assert_eq!(
            crdt_payloads(&heard),
            vec![b"workspace-seven-only".to_vec()],
            "the peer in the sender's workspace must receive the delta, \
             got {heard:?}"
        );

        let leaked = poll_until(&mut other_workspace, 1, Duration::from_secs(1));
        assert!(
            crdt_payloads(&leaked).is_empty(),
            "an app opened in workspace {} must receive no CRDT payload \
             from the same app opened in workspace {}; it received {:?}",
            elsewhere.id.raw(),
            here.id.raw(),
            crdt_payloads(&leaked),
        );

        drop(sender);
        drop(same_workspace);
        drop(other_workspace);
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial_test::serial(collab_broker, process_env)]
    fn a_launch_tells_the_app_its_package_and_its_workspace() {
        let dir = scratch_dir("env");
        let workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(7),
            "here",
        );
        let env = sandbox_env_for_launch(&dir, "dev.qos.notes", workspace.room);
        let lookup = |key: &str| {
            env.iter().find(|(k, _)| k == key).map_or_else(
                || panic!("sandbox env is missing {key}: {env:?}"),
                |(_, v)| v.clone(),
            )
        };

        assert_eq!(
            lookup("QOS_PACKAGE_ID"),
            "dev.qos.notes",
            "the launcher must tell the app which package it is"
        );
        assert_eq!(
            lookup("QOS_WORKSPACE_ID"),
            workspace.collab_cluster_id().to_string(),
            "the launcher must tell the app which workspace it was opened into"
        );
        // Durable identity, pinned against the stored room verbatim.
        assert_eq!(
            lookup("QOS_WORKSPACE_ID"),
            workspace.room.to_string(),
            "the cluster the app is launched into must be the workspace's \
             stored room id, which is the only value a peer on another \
             machine can also hold"
        );
        assert_ne!(
            crate::shell::state::Workspace::test_default(crate::shell::state::WorkspaceId(7), "a",)
                .collab_cluster_id(),
            crate::shell::state::Workspace::test_default(crate::shell::state::WorkspaceId(8), "b",)
                .collab_cluster_id(),
            "distinct workspaces must have distinct cluster ids"
        );
        assert_ne!(
            crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(7),
                "mine",
            )
            .collab_cluster_id(),
            crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(7),
                "theirs",
            )
            .collab_cluster_id(),
            "two unrelated workspaces that happen to share a local id must \
             not be the same collaboration room"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The route recorded for the peer accepted at `index`, read from
    /// the live peer table so the test ships production's actual value.
    fn announced_route(handle: &BrokerHandle, index: usize) -> CrdtRoute {
        handle
            .peers
            .snapshot()
            .get(index)
            .and_then(|p| p.route())
            .expect("peer should have announced a route")
    }

    /// One hop of the peer-sync wire, minus Iroh — both ends of the
    /// bridge verbatim. Gossip is a broadcast: a frame reaches every
    /// host on the topic, and which of their apps see it is decided
    /// entirely by the route. That's the decision under test.
    fn deliver_over_the_wire(to: &BrokerHandle, wire_route: &CrdtRoute, data: &[u8]) {
        to.fanout_event(
            &CollabEvent::CRDTIncremental {
                data: data.to_vec(),
            },
            None,
            wire_route,
        );
    }

    #[test]
    #[serial_test::serial(collab_broker, process_env)]
    fn a_workspace_is_the_same_room_after_a_restart() {
        // A room id that changed on restart would strand every invited
        // peer and orphan the app windows reopened into it.
        let dir = scratch_dir("restart");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let state_path = state_dir.path().join("state.json");
        let socket_path = temp_sock("restart");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let ws_id = crate::shell::state::WorkspaceId(7);
        let other_id = crate::shell::state::WorkspaceId(8);

        // First run of the compositor.
        let mut first_run = crate::shell::state::WorkspaceTree::default();
        first_run.persist_path = Some(state_path.clone());
        first_run
            .workspaces
            .push(crate::shell::state::Workspace::test_default(ws_id, "Home"));
        first_run
            .workspaces
            .push(crate::shell::state::Workspace::test_default(
                other_id, "Other",
            ));
        let room_before = first_run.room_of(ws_id).expect("workspace exists");
        let other_room = first_run.room_of(other_id).expect("workspace exists");
        first_run.flush_to_disk();

        let mut before =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", room_before, "alice");

        // The process dies; a new one loads the same state file.
        let (mut second_run, _) = crate::shell::persist::load_or_default(&state_path);
        second_run.persist_path = Some(state_path);
        // Boot-time migration must find nothing to do: re-minting an
        // already-identified workspace is the bug this test catches.
        let minted = second_run.adopt_roomless_workspaces();
        assert!(
            minted.is_empty(),
            "a workspace loaded with a room already recorded must not have \
             a new one minted for it; {} were re-minted",
            minted.len()
        );
        let room_after = second_run
            .room_of(ws_id)
            .expect("the workspace must survive the restart");
        assert_eq!(
            room_after, room_before,
            "a workspace's collaboration room must be the same after a \
             restart as before it"
        );

        let mut after =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", room_after, "bob");
        let room_other_after = second_run
            .room_of(other_id)
            .expect("the second workspace must survive too");
        assert_eq!(room_other_after, other_room);
        let mut elsewhere = connect_as_launched_app(
            &socket_path,
            &dir,
            "dev.qos.notes",
            room_other_after,
            "carol",
        );

        wait_for_announced(&handle, 3, Duration::from_secs(2));
        let _ = before.poll();
        let _ = after.poll();
        let _ = elsewhere.poll();

        after.send(CollabRequest::SyncIncremental {
            data: b"still-the-same-room".to_vec(),
        });

        let heard = poll_until(&mut before, 1, Duration::from_secs(1));
        assert_eq!(
            crdt_payloads(&heard),
            vec![b"still-the-same-room".to_vec()],
            "an app launched after the restart must land in the same room as \
             one launched before it, so their edits still reach each other. \
             Got {heard:?}"
        );

        let leaked = poll_until(&mut elsewhere, 1, Duration::from_secs(1));
        assert!(
            crdt_payloads(&leaked).is_empty(),
            "the other workspace must still be a different room after the \
             restart; it received {:?}",
            crdt_payloads(&leaked),
        );

        drop(before);
        drop(after);
        drop(elsewhere);
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[serial_test::serial(collab_broker, process_env)]
    fn an_invited_host_joins_the_room_and_an_uninvited_one_does_not() {
        // Three hosts, three brokers, deliberately the same local
        // workspace id on all three — that must not decide the room.
        // Only the second host accepts the invite.
        let dir = scratch_dir("cross-host");
        let host_sock = temp_sock("cross-host-a");
        let guest_sock = temp_sock("cross-host-b");
        let stranger_sock = temp_sock("cross-host-c");
        let host_broker = spawn(host_sock.clone()).expect("host broker should bind");
        let guest_broker = spawn(guest_sock.clone()).expect("guest broker should bind");
        let stranger_broker = spawn(stranger_sock.clone()).expect("stranger broker should bind");

        let local_id = crate::shell::state::WorkspaceId(7);

        let mut host_tree = crate::shell::state::WorkspaceTree::default();
        host_tree
            .workspaces
            .push(crate::shell::state::Workspace::test_default(
                local_id, "Shared",
            ));
        let mut guest_tree = crate::shell::state::WorkspaceTree::default();
        guest_tree
            .workspaces
            .push(crate::shell::state::Workspace::test_default(
                local_id, "Mine",
            ));
        let mut stranger_tree = crate::shell::state::WorkspaceTree::default();
        stranger_tree
            .workspaces
            .push(crate::shell::state::Workspace::test_default(
                local_id, "Theirs",
            ));

        let host_room = host_tree.room_of(local_id).expect("host workspace");
        let guest_room_before = guest_tree.room_of(local_id).expect("guest workspace");
        let stranger_room = stranger_tree.room_of(local_id).expect("stranger workspace");
        assert_ne!(
            host_room, guest_room_before,
            "three hosts that have never met must start in three different \
             rooms even when their local workspace ids collide"
        );
        assert_ne!(host_room, stranger_room);
        assert_ne!(guest_room_before, stranger_room);

        // The guest's app before the invite, for attribution.
        let mut guest_before_invite = connect_as_launched_app(
            &guest_sock,
            &dir,
            "dev.qos.notes",
            guest_room_before,
            "bob-before",
        );

        // The invite, production path on both ends.
        let owner_key = iroh::SecretKey::generate();
        let owner_addr = iroh::EndpointAddr::new(owner_key.public())
            .with_ip_addr("127.0.0.1:4001".parse().unwrap());
        let token = crate::network::invite::make_invite_token(
            &owner_key,
            host_tree
                .workspaces
                .iter()
                .find(|w| w.id == local_id)
                .expect("host workspace"),
            &owner_addr,
            3600,
        )
        .expect("the host issues an invite");
        let adopted = crate::network::invite::accept_invite_into(&mut guest_tree, local_id, &token)
            .expect("the guest accepts it");
        assert_eq!(
            adopted, host_room,
            "accepting an invite must put the guest in the host's room"
        );
        assert_eq!(
            guest_tree.room_of(local_id),
            Some(host_room),
            "the guest's workspace must now address the host's room"
        );
        assert_eq!(
            stranger_tree.room_of(local_id),
            Some(stranger_room),
            "the uninvited host must be untouched by someone else's invite"
        );

        let mut host_app =
            connect_as_launched_app(&host_sock, &dir, "dev.qos.notes", host_room, "alice");
        let mut guest_app = connect_as_launched_app(
            &guest_sock,
            &dir,
            "dev.qos.notes",
            guest_tree.room_of(local_id).expect("guest workspace"),
            "bob",
        );
        let mut stranger_app = connect_as_launched_app(
            &stranger_sock,
            &dir,
            "dev.qos.notes",
            stranger_tree.room_of(local_id).expect("stranger workspace"),
            "carol",
        );

        wait_for_announced(&host_broker, 1, Duration::from_secs(2));
        wait_for_announced(&guest_broker, 2, Duration::from_secs(2));
        wait_for_announced(&stranger_broker, 1, Duration::from_secs(2));
        let _ = host_app.poll();
        let _ = guest_app.poll();
        let _ = guest_before_invite.poll();
        let _ = stranger_app.poll();

        // One frame, broadcast to both other hosts.
        let wire_route = announced_route(&host_broker, 0);
        let payload = b"from-the-host".to_vec();
        deliver_over_the_wire(&guest_broker, &wire_route, &payload);
        deliver_over_the_wire(&stranger_broker, &wire_route, &payload);

        let guest_heard = poll_until(&mut guest_app, 1, Duration::from_secs(1));
        assert_eq!(
            crdt_payloads(&guest_heard),
            vec![payload],
            "an app on a host that accepted the invite must receive the \
             inviter's delta — that is the whole point of the invite \
             carrying the room. Got {guest_heard:?}"
        );

        let stranger_heard = poll_until(&mut stranger_app, 1, Duration::from_secs(1));
        assert!(
            crdt_payloads(&stranger_heard).is_empty(),
            "an app on a host that was never invited must receive nothing \
             from the same broadcast frame; it received {:?}",
            crdt_payloads(&stranger_heard),
        );
        let pre_invite_heard = poll_until(&mut guest_before_invite, 1, Duration::from_secs(1));
        assert!(
            crdt_payloads(&pre_invite_heard).is_empty(),
            "an app the guest opened before accepting the invite is in the \
             guest's old room and must receive nothing — this is what makes \
             the delivery above attributable to the invite rather than to \
             the guest's broker being indiscriminate. It received {:?}",
            crdt_payloads(&pre_invite_heard),
        );

        drop(host_app);
        drop(guest_app);
        drop(guest_before_invite);
        drop(stranger_app);
        drop(host_broker);
        drop(guest_broker);
        drop(stranger_broker);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
