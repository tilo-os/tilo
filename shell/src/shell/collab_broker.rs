//! Local CRDT fan-out broker for the `QUICK_COLLAB_SOCK` Unix socket.
//!
//! Every quick-ui app instantiates a [`ui::collaboration::CollaborationManager`]
//! that connects to `QUICK_COLLAB_SOCK` and exchanges length-prefixed JSON
//! frames (`u32` little-endian length + JSON body). Pre-broker, nothing in
//! production bound that socket — the protocol was only exercised by the
//! `ui/tests/real_socket_two_peer.rs` test-only relay.
//!
//! This module lifts the relay's fan-out pattern into the shell so two
//! co-located apps (e.g. two `cargo run -p terminal` instances pointed at
//! the same socket) share scrollback CRDT updates live.
//!
//! Behaviour:
//! - Bind the configured socket path (best-effort `unlink` first so a
//!   crashed prior shell doesn't lock us out).
//! - Accept any number of peer connections (unlike the test broker's
//!   hardcoded N=2).
//! - For each peer, spawn a reader thread that decodes `CollabRequest`s
//!   and translates them to the matching `CollabEvent`, then fans the
//!   event to the peers the event is addressed to (see "Routing").
//! - Drop dead peers (write failure) from the fan-out table.
//!
//! The mapping from request → event mirrors the test relay verbatim:
//! - `SyncIncremental { data }` → `CRDTIncremental { data }`
//! - `SyncCRDT { data }` → `CRDTUpdate { data }`
//! - `Presence { key, payload }` → `RemotePresenceUpdate { user_id:
//!   "peer-<src_id>", key, payload }`
//! - `Hello { .. }` → swallowed (no fan-out; the hello records the
//!   connection's [`CrdtRoute`])
//!
//! ## Routing
//!
//! Every event is delivered against a [`CrdtRoute`] — the `(app_id,
//! cluster_id)` pair the sender announced on its `Hello`. A connection
//! only receives an event when its own announced route matches:
//!
//! - **CRDT payloads** (`CRDTIncremental`, `CRDTUpdate`,
//!   `SignedCRDTIncremental`, cooperative `ScrollbackBatch`) carry a
//!   full `(app_id, cluster_id)` route: the Notes app never sees the
//!   Terminal's document deltas, and cluster A never sees cluster B.
//! - **Chrome traffic** (presence, cursors, context) carries a
//!   cluster-only route — every app in the cluster is a legitimate
//!   audience for "who else is here".
//!
//! Routing **fails closed**: a connection that has not said `Hello`
//! receives nothing at all, and a frame whose sender has not said
//! `Hello` is neither fanned out locally nor republished over libp2p.
//! There is no broadcast primitive left in the broker.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use collab_types::{
    ArtifactId, ChannelState, Ed25519PublicKey, OpClass, OpId, RedactedMarker, SignedOp,
    ValidationError, parse_redact_payload, validate_inbound_op,
};
use ui::collaboration::{CollabEvent, CollabRequest};

use crate::shell::collab_broker_capnp::CapnpBrokerState;
use crate::shell::collab_outbound::{PeerPublisher, Transport};
use crate::shell::network::ShellNetwork;

/// Maximum number of audit-log entries retained per channel before
/// the in-memory ring buffer evicts its oldest entry. Persistence
/// (and per-channel retention policy) is a Phase 3.5 follow-up —
/// the in-memory record is enough for current debugging and tests
/// that want to assert "this op was dropped".
const AUDIT_LOG_RING_CAPACITY: usize = 256;

/// One entry in the per-channel audit log. The full envelope is
/// captured by reference (cloned out) so that the operator can pick
/// over the dropped op later — author, declared op class, payload
/// length — without having to re-parse the wire.
#[derive(Clone, Debug)]
pub struct AuditEntry {
    /// The signed envelope that triggered the entry. Successful ops
    /// also surface here when explicitly recorded (currently only
    /// failures are logged, but the type carries the same shape so
    /// the writer can be extended without a breaking API change).
    pub envelope: Box<SignedOp>,
    /// Why the entry was logged. `None` is a successful op recorded
    /// for tracing; `Some(reason)` is a rejection.
    pub rejection: Option<ValidationError>,
    /// Wall-clock time the entry was minted on this host. Used by
    /// the chrome's "Activity pane" widget (Phase 1) to render the
    /// timeline.
    pub at: std::time::SystemTime,
}

/// Per-channel audit log, fixed-capacity ring buffer.
///
/// Keyed by [`ArtifactId`] because a `SignedOp` carries an artifact
/// id but not a channel id directly — and the broker's v1 channel
/// state is keyed by artifact id too (see the
/// [`Broker::channel_state_for`] note below). Phase 3.5 reshapes
/// this to a true channel-id keyed map.
#[derive(Default)]
pub struct AuditLog {
    pub entries: HashMap<ArtifactId, VecDeque<AuditEntry>>,
}

impl AuditLog {
    /// Append `entry` to the ring buffer for `artifact`, evicting the
    /// oldest entry if the buffer is at capacity.
    pub fn record(&mut self, artifact: ArtifactId, entry: AuditEntry) {
        let buf = self.entries.entry(artifact).or_default();
        if buf.len() >= AUDIT_LOG_RING_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// Read-only view of the entries logged for one artifact. Mostly
    /// used by tests.
    pub fn for_artifact(&self, artifact: ArtifactId) -> Vec<AuditEntry> {
        self.entries
            .get(&artifact)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Phase 9: one entry in the per-channel **redaction record** log.
///
/// Independent of the per-artifact rejection-audit ring buffer — a
/// redaction is a positive governance event, not a failure. Stored in
/// its own map so the chrome's compliance/audit surface can iterate
/// only over redactions without filtering through every dropped
/// envelope.
///
/// The record retains the Redact op's full envelope (so the original
/// signature is verifiable forever) plus the parsed payload fields
/// promoted to typed columns for fast lookup.
#[derive(Clone, Debug)]
pub struct RedactionRecord {
    /// The Redact op envelope itself. Its signature is verifiable
    /// against `redactor_pk` long after this record is consulted.
    pub redact_envelope: Box<SignedOp>,
    /// The id of the op that was redacted. Cross-references the
    /// `target_op_hash` to a specific seq + author tuple.
    pub target_op_id: OpId,
    /// SHA-256 of the target op's canonical bytes at the moment the
    /// redaction was minted. The validator preserves this even after
    /// the op-log entry is rewritten to the redacted marker.
    pub target_op_hash: [u8; 32],
    /// Public key of the redactor (also recorded in the envelope's
    /// `author` field but lifted here for direct match in audit
    /// queries).
    pub redactor_pk: Ed25519PublicKey,
    /// Free-text justification (capped at
    /// `collab_types::MAX_REDACTION_REASON_BYTES`). Used by the
    /// chrome's tooltip and the compliance system's case record.
    pub reason: String,
    /// Wall-clock time the redactor recorded in the request payload.
    pub redacted_at: std::time::SystemTime,
}

/// Phase 9: per-artifact redaction-record log.
///
/// Like [`AuditLog`] this is a fixed-capacity ring buffer to avoid
/// unbounded growth, but redaction events are rare by construction
/// (manual compliance acts) so the cap is mostly defensive.
#[derive(Default)]
pub struct RedactionLog {
    pub entries: HashMap<ArtifactId, VecDeque<RedactionRecord>>,
}

impl RedactionLog {
    /// Append `record` to the ring buffer for `artifact`, evicting
    /// the oldest entry if the buffer is at capacity.
    pub fn record(&mut self, artifact: ArtifactId, record: RedactionRecord) {
        let buf = self.entries.entry(artifact).or_default();
        if buf.len() >= AUDIT_LOG_RING_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(record);
    }

    /// Read-only view of the redaction records for one artifact.
    /// Exposed via [`BrokerHandle::redactions_for`] for tests and the
    /// chrome's compliance surface.
    pub fn for_artifact(&self, artifact: ArtifactId) -> Vec<RedactionRecord> {
        self.entries
            .get(&artifact)
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// Per-peer entry in the broker's fan-out table.
struct Peer {
    /// Stable id assigned at accept time. Identifies the *connection*
    /// — it is what the fan-out filter excludes a sender by — and is
    /// deliberately not an identity: two apps one person has open are
    /// two connections and one participant. Who that participant is
    /// gets answered by [`LocalIdentity`].
    id: u64,
    /// The name this peer asked to be shown as in presence, from its
    /// `Hello`. Informational: never consulted for routing or
    /// authority, and outranked by whatever the room says this
    /// machine's peer is called — see [`LocalIdentity`].
    handle: Mutex<Option<String>>,
    /// Write side of the connected stream. The read side is owned by
    /// the per-peer reader thread.
    writer: Mutex<UnixStream>,
    /// The route this connection announced on its
    /// `CollabRequest::Hello`. Both the delivery filter and the stamp
    /// the libp2p bridge puts on outbound `NeighbourMsg`s. `None`
    /// until the peer says hello — and a peer with `None` neither
    /// receives nor originates routed traffic.
    route: Mutex<Option<CrdtRoute>>,
}

impl Peer {
    /// Snapshot the announced route, if any.
    fn route(&self) -> Option<CrdtRoute> {
        self.route.lock().ok().and_then(|g| g.clone())
    }

    /// What this connection said it wanted to be called, if anything.
    /// One input to [`LocalIdentity::resolve`], and the weakest one.
    fn announced_handle(&self) -> Option<String> {
        self.handle.lock().ok().and_then(|g| g.clone())
    }
}

/// What a room calls the peers in it, keyed by the handle presence
/// frames identify their sender by. Same shape as
/// [`crate::shell::peer_presence::PeerLabels`].
pub type PeerLabels = std::collections::BTreeMap<u64, String>;

/// How long a room's answer is reused before it is asked again.
/// Reading it takes the replica's lock and parses every peer id the
/// room document holds, and the paths that need it run at pointer
/// rate. Matches the cadence the peer-sync drain uses for the same
/// lookup.
const LABEL_TTL: Duration = Duration::from_secs(2);

/// Who the traffic from a **locally connected** app belongs to.
///
/// The broker stamps an identity on everything an app sends before
/// another app sees it: presence, pointers, and the signed deltas a
/// caret is drawn from. Those stamps used to be minted per connection
/// (`peer-<n>`), so one person showed up as a different stranger in
/// every app they had open, and no amount of the room knowing their
/// name could reach them — there was no lookup on those paths at all.
///
/// A local connection is *this machine's* peer, so the name is the one
/// the room wrote for this machine's peer id: the same governance fact
/// [`crate::shell::state::WorkspaceTree::room_peer_labels`] answers for
/// remote peers, read through the same lookup, so a participant reads
/// the same on the caret, the roster and the pointer.
///
/// The order is deliberate:
///
/// 1. **What the room says.** Written by the inviter beside the
///    capability grant, on the replicated channel `apply_room_changes`
///    refuses from anyone but the room's authority. A peer that could
///    name itself could name itself anything, next to a caret editing
///    a document that room's capabilities gate.
/// 2. **What the connection calls itself.** No authority whatsoever —
///    it is kept only because an agent shares a machine with the person
///    running it, and it names a local process rather than asserting
///    who is in the room. It can never overrule (1).
/// 3. **This machine's own user id.** Not a stand-in and not invented:
///    it is the identity the compositor already publishes its own
///    pointer under, so an unnamed local participant falls back to an
///    id — which is what an unnamed remote one does too.
pub struct LocalIdentity {
    /// What the room calls a peer, keyed by liveness handle.
    ///
    /// A parameter rather than a lookup done here, for the reason
    /// [`crate::shell::peer_presence::fold_inbound`] takes the same
    /// one: so this is exercisable without standing a network up, and
    /// so production has exactly one implementation of it — the room
    /// document, via [`crate::shell::network::ShellNetwork::peer_labels`].
    labels: Box<dyn Fn(&str) -> PeerLabels + Send + Sync>,
    /// This machine's own liveness handle — the key its name is
    /// written under. Presence frames identify their sender by that
    /// digest while the room document keys names by base58 `PeerId`,
    /// and `room_peer_labels` is where the two are bridged; a lookup
    /// done against anything else silently misses and falls back to an
    /// id, which is the failure this type exists to end.
    local_handle: u64,
    /// The last answer per cluster, with the instant it was given.
    cache: Mutex<HashMap<String, (Instant, Option<String>)>>,
}

impl LocalIdentity {
    /// The production wiring: ask the room document through the
    /// peer-sync network. A session without one names nobody, which
    /// leaves every local participant on the `local_user_id` fallback.
    pub fn from_network(net: Option<Arc<ShellNetwork>>) -> Arc<Self> {
        let local_handle = net.as_ref().map(|n| n.local_peer_id_hash()).unwrap_or(0);
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

/// The address a CRDT (or chrome) event is delivered to.
///
/// Built from the `(app_id, cluster_id)` pair a connection announces
/// on `CollabRequest::Hello`. Two granularities exist:
///
/// - **app scope** — both fields set. Used for every CRDT payload so
///   one app's document deltas never reach a different app.
/// - **cluster scope** — `app_id` empty. Used for chrome traffic
///   (presence / cursors / context), where every app in the cluster is
///   a legitimate audience, and for the Cap'n Proto transport, whose
///   `joinChannel(channelId, identity)` surface carries a channel id
///   but no app identity (the SDK sets `channelId = cluster_id`).
///
/// A route with an empty `cluster_id` addresses nobody: that is the
/// fail-closed case for a connection that never said hello, or for a
/// libp2p frame published by a peer that could not stamp one.
/// Rendering of `crate::shell::state::RoomId::nil` — the cluster id a
/// workspace announces when its durable room identity has not been
/// minted yet. Never delivered to or from; see
/// [`CrdtRoute::is_addressable`].
const UNMINTED_CLUSTER_ID: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrdtRoute {
    /// App identity from the sender's `Hello`. Empty means "this
    /// transport does not announce an app identity" — matched at
    /// cluster granularity.
    pub app_id: String,
    /// Collaboration cluster id (a uuid rendered as a string on the
    /// legacy dialect, the `channelId` on the capnp one).
    pub cluster_id: String,
    /// The **record** the sender announced, or empty for an app whose
    /// state is still one document per `(app, cluster)`.
    ///
    /// This is the finest granularity, and the reason it exists is
    /// that a document is what people share — not an app. Two people
    /// editing two different files in one room announce the same app
    /// and the same cluster; only the record tells them apart, and
    /// without it each was handed the other's document state.
    ///
    /// It is a *routing* key and nothing more: the broker still never
    /// materialises a payload. What it now knows is which stream a
    /// payload belongs to, because the sender said so.
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

    /// Narrow this route to a record. An empty `record_id` leaves it
    /// as it was, which is how a connection that announced no record
    /// keeps the pre-record addressing.
    pub fn on_record(mut self, record_id: impl Into<String>) -> Self {
        self.record_id = record_id.into();
        self
    }

    /// Widen an app-scoped route to its cluster. Presence frames from
    /// an app-scoped sender ride this.
    ///
    /// The record is dropped along with the app id, and deliberately:
    /// "who else is here" is a fact about the room, so a peer editing
    /// a different document in the same room is still a legitimate
    /// audience for it. Only *document* payloads are record-scoped.
    pub(crate) fn to_cluster_scope(&self) -> Self {
        Self::cluster(self.cluster_id.clone())
    }

    /// The channel this route names, as one string.
    ///
    /// The capnp transport subscribes by channel id rather than by
    /// route fields, so a connection has to be able to name its
    /// address in the same vocabulary — see
    /// [`crate::shell::collab_broker_capnp`]. The encoding lives in
    /// [`ui::collaboration::channel_key`] because the SDK writes these
    /// keys and the shell reads them; one definition is what keeps the
    /// two ends from drifting.
    pub fn channel_key(&self) -> String {
        ui::collaboration::channel_key::format(&self.app_id, &self.cluster_id, &self.record_id)
    }

    /// Split a capnp `channelId` back into its route. The inverse of
    /// [`Self::channel_key`], and total: a key naming no app and no
    /// record decodes to a cluster-scoped route, which is what a
    /// connection announcing neither should address.
    pub fn from_channel_key(key: &str) -> Self {
        let (app_id, cluster_id, record_id) = ui::collaboration::channel_key::parse(key);
        Self {
            app_id,
            cluster_id,
            record_id,
        }
    }

    /// Whether a cluster id names a real room.
    ///
    /// Two values do not: the empty string (a connection that never
    /// said `Hello`, or a frame nobody could stamp) and the nil uuid (a
    /// workspace whose durable room identity has not been minted yet —
    /// see `crate::shell::state::RoomId::nil`). The nil case matters
    /// because it is not empty: without this, every workspace still
    /// carrying the sentinel would share one enormous room, on every
    /// machine at once, which is precisely the failure that deriving
    /// the cluster from a local counter used to cause.
    fn is_addressable(cluster_id: &str) -> bool {
        !cluster_id.is_empty() && cluster_id != UNMINTED_CLUSTER_ID
    }

    /// Whether this route may be stamped onto a frame leaving the
    /// machine.
    ///
    /// The same test [`Self::delivers_to`] applies to the sender side,
    /// exposed because the outbound half
    /// ([`crate::shell::collab_outbound`]) has to make the identical
    /// call before a publish. A route that cannot deliver locally
    /// cannot deliver remotely either: the receiving broker rebuilds
    /// this route from the wire fields and runs it through
    /// `delivers_to`, so publishing an unaddressable one puts a frame
    /// in the room that every receiver silently drops.
    pub fn is_publishable(&self) -> bool {
        Self::is_addressable(&self.cluster_id)
    }

    /// Whether an event addressed to `self` should be delivered to a
    /// subscriber that announced `subscriber`.
    ///
    /// Fails closed on either side lacking an addressable cluster. App
    /// granularity applies only when both ends announced an app id; an
    /// end that announced none stays cluster-scoped rather than
    /// silently receiving nothing. Both transports do announce one —
    /// capnp carries it in the channel id
    /// ([`Self::from_channel_key`]) — so that fallback now covers a
    /// connection that predates the app segment, not a whole transport.
    ///
    /// **Record granularity is the innermost test and it fails closed
    /// both ways.** Two app-scoped ends agree only when they name the
    /// same record, and "no record" is a value like any other: a
    /// converted app's document deltas are not handed to an
    /// unconverted connection of the same app, and vice versa, because
    /// those two are not looking at the same document. An app that has
    /// not adopted records still matches its own kind exactly as
    /// before (both ends empty), which is what keeps the rest of the
    /// fleet working untouched.
    pub fn delivers_to(&self, subscriber: &CrdtRoute) -> bool {
        if !Self::is_addressable(&self.cluster_id) || !Self::is_addressable(&subscriber.cluster_id)
        {
            return false;
        }
        if self.cluster_id != subscriber.cluster_id {
            return false;
        }
        if self.app_id.is_empty() || subscriber.app_id.is_empty() {
            // Cluster-scoped on at least one end: chrome traffic, or a
            // transport that announces no app identity. Records do not
            // narrow it — see `to_cluster_scope`.
            return true;
        }
        if self.app_id != subscriber.app_id {
            return false;
        }
        self.record_id == subscriber.record_id
    }
}

/// Phase 3 channel-state cache.
///
/// The broker keeps one [`ChannelState`] per artifact id (a v1
/// proxy — see the TODO inside `state_for`). Reader threads acquire
/// the lock for the duration of one validation, which is cheap (the
/// validator only mutates `last_seq_per_author` on success).
///
/// Bootstrap policy: the first time an artifact is touched, we create
/// a default `ChannelState` with empty `authorised_signers`. That
/// makes every signed op fail step 2 of the validator
/// ([`ValidationError::UnauthorisedAttestation`]) — which is the
/// **correct, secure** default for an artifact whose role table the
/// broker has never been told about. Tests (and the eventual Phase 3.5
/// bootstrap path) populate the state via
/// [`Broker::register_channel_state`] before driving traffic through
/// the broker. The audit log records the rejection so an operator can
/// see that an envelope was dropped because the channel was
/// un-bootstrapped vs. genuinely-invalid.
///
/// TODO(phase-3.5): wire the channel state up from the persisted
/// `Workspace.collaborators` + governance log so production traffic
/// no longer needs the bootstrap dance.
#[derive(Default)]
pub struct BrokerChannels {
    /// Per-artifact channel state. v1 keys on artifact id rather than
    /// channel id because the wire only carries artifact id; the
    /// 3.5 follow-up adds a real artifact→channel resolver.
    pub states: HashMap<ArtifactId, ChannelState>,
    /// Per-artifact audit log, fixed-capacity. Records rejected
    /// envelopes (and Phase 9's `OpArrivedRedacted` late-arrival
    /// records via the same shape) so an operator can triage drops.
    pub audit: AuditLog,
    /// Phase 9: per-artifact redaction record log. Separate from the
    /// rejection-audit because a redaction is a positive governance
    /// event, not a failure.
    pub redactions: RedactionLog,
}

impl BrokerChannels {
    /// Look up the [`ChannelState`] for `artifact`, lazily inserting
    /// a default. The lazy default has empty `authorised_signers` so
    /// every signed op fails step 2 of the validator until a test
    /// or the eventual Phase 3.5 bootstrap fills it in.
    pub fn state_for(&mut self, artifact: ArtifactId) -> &mut ChannelState {
        self.states.entry(artifact).or_default()
    }

    /// Inject a populated [`ChannelState`] for `artifact`, replacing
    /// any existing entry. Tests use this to set up role tables; the
    /// eventual Phase 3.5 bootstrap path calls the same helper from
    /// the persisted workspace metadata.
    pub fn register_channel_state(&mut self, artifact: ArtifactId, state: ChannelState) {
        self.states.insert(artifact, state);
    }
}

/// Shared registry of connected peers. Reader threads call
/// [`PeerTable::fanout_to_route`] for every translated event.
///
/// The table owns:
/// - the legacy JSON peer connections,
/// - the process-wide peer-id counter (shared with the capnp
///   listener so ids are unique across transports),
/// - an optional [`CapnpBrokerState`] handle that layers the same
///   fanout onto per-subscription mpsc queues held by the capnp
///   RPC layer. When the capnp listener is bound alongside the
///   legacy one, every event dispatched here reaches both.
/// A participant asking this session to open an application.
///
/// Everything the session needs to decide, and nothing it should take on
/// trust. The `app_id` is what was asked for; `room` and `who` are the
/// broker's own answers about the connection that asked — resolved the same
/// way every other frame's attribution is — because the question the session
/// has to answer is not "should this app open" but "may *this participant*
/// open it *in this room*", and a caller cannot be allowed to supply the
/// second half of that.
#[derive(Clone, Debug)]
pub struct OpenAppRequest {
    /// The application's package id, as the caller named it. Unvalidated: the
    /// session resolves it, and an id that names nothing installed is a
    /// refusal the caller is told about.
    pub app_id: String,
    /// The room the asking connection announced on its `Hello`. Empty when it
    /// never announced one, which the session treats as a refusal — there is
    /// no workspace whose capabilities could answer.
    pub room: String,
    /// Who asked, as the broker resolved them for every other frame.
    ///
    /// The room's own label for the connection where it has one. Right for
    /// attribution and wrong for authorisation: it names the *machine* a
    /// participant is connected through, which for a participant without a
    /// window is not the participant.
    pub who: String,
    /// The name the connection announced on its `Hello`.
    ///
    /// The only thing on this socket that names the participant itself, and
    /// **it is not authenticated** — the field is documented as informational
    /// and nothing checks it. A session that decides on it is deciding on a
    /// name anything with access to the socket could choose, so the
    /// capabilities it looks up must be the workspace's own and the answer
    /// must be no by default. It narrows a grant; it never makes one. See
    /// `shell::shell::agent_opens`.
    pub handle: Option<String>,
    /// The connection to answer a refusal on. See
    /// [`BrokerHandle::refuse_open`].
    pub peer: u64,
}

/// What a session does when a participant asks it to open an application.
///
/// The broker holds the door and does not decide who comes through it: it
/// knows sockets, rooms and names, and nothing about workspaces, capabilities
/// or launchers. So the decision is the compositor's, taken on its own thread
/// against its own state, and this is only how the question reaches it.
///
/// The sink runs on the reader thread that decoded the frame. Keep it short —
/// hand the request to a queue and return.
pub type AppOpener = Box<dyn Fn(OpenAppRequest) + Send + Sync>;

pub struct PeerTable {
    peers: Mutex<Vec<Arc<Peer>>>,
    /// Monotonic id source. Shared with the capnp listener so a
    /// capnp-side "peer" and a legacy-side peer are never confused
    /// when the `except` filter runs on fanout.
    peer_id_counter: AtomicU64,
    /// Optional capnp fanout sink. Set once during `spawn_*` when
    /// the capnp listener is bound.
    capnp: Mutex<Option<Arc<CapnpBrokerState>>>,
    /// Subscribers that live inside the shell process rather than on
    /// the far side of a socket. See [`RoomSubscription`].
    local_subs: Mutex<Vec<Arc<RoomSubscription>>>,
    /// Where a `CollabRequest::OpenApp` goes. `None` in a session that
    /// installed none — a broker with no desktop behind it — and a request
    /// then gets the honest answer rather than silence.
    app_opener: Mutex<Option<Arc<AppOpener>>>,
}

/// A subscriber inside the shell process, addressed by room.
///
/// The shell hosts the broker but was never a reader of it, so the
/// presence its own apps publish went out to the other apps in the
/// room and nowhere else — the compositor drawing those windows never
/// saw it.
///
/// A room subscription is how it reads. It is addressed the way every
/// other subscriber is: a room is a cluster, and delivery is decided
/// by [`CrdtRoute::delivers_to`] against `CrdtRoute::cluster(room)`.
/// That is deliberate rather than convenient — presence is widened to
/// cluster scope on the way out ([`CrdtRoute::to_cluster_scope`])
/// precisely so a peer working in a different document of the same
/// room is still a legitimate audience, and a subscriber that names
/// the room inherits exactly that reach. Two apps in one room reach
/// one subscription, which is what makes a cursor crossing from one
/// app to the other a single peer moving rather than two peers
/// appearing.
///
/// Rooms are mutable because a session's set of them is: workspaces
/// are created, and a workspace joins a peer's room when an invite is
/// accepted.
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

    /// Whether `route` addresses any room this subscription holds.
    /// Uses the broker's own delivery predicate, so a subscription
    /// can never receive something a socket peer in the same room
    /// would not have.
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
            app_opener: Mutex::new(None),
        }
    }

    /// Register an in-process subscriber and hand back its handle so
    /// the caller can keep its room set current.
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
        self.local_subs.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Mint the next peer id. Legacy accept + capnp accept both go
    /// through this so their ids never collide.
    pub(crate) fn next_peer_id(&self) -> u64 {
        self.peer_id_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// How many connections this broker has ever accepted, across both
    /// transports.
    ///
    /// Read as a generation rather than a count: it only ever rises, so
    /// a value different from the one recorded beside an announcement
    /// means somebody has connected who was not there to hear it. See
    /// [`remote_participant_id`], which uses it to decide that a name
    /// already said still needs saying.
    pub(crate) fn connections_seen(&self) -> u64 {
        self.peer_id_counter.load(Ordering::Relaxed)
    }

    /// Attach the capnp fanout sink. Called at most once at
    /// broker-spawn time. Silently ignores repeat calls (defensive
    /// but not expected).
    pub(crate) fn attach_capnp(&self, state: Arc<CapnpBrokerState>) {
        if let Ok(mut g) = self.capnp.lock() {
            if g.is_none() {
                *g = Some(state);
            }
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

    /// Serialize `event` and write it to every peer that `route`
    /// addresses, skipping the peer whose id is `except` (the
    /// originator; pass [`u64::MAX`] when the event came from off-host
    /// and no local peer should be skipped).
    ///
    /// Delivery is decided by [`CrdtRoute::delivers_to`] against the
    /// route each peer announced on its `Hello`. A peer that has not
    /// announced one is skipped — there is no broadcast fallback, so a
    /// silent app receives nothing rather than everything.
    ///
    /// Drops any peer that errors on write. Also pushes the event onto
    /// the capnp subscription queues that the same route addresses (if
    /// a [`CapnpBrokerState`] is attached) so both transports observe
    /// the same routing.
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
                // Connected but silent: it has not told us which app
                // or cluster it belongs to, so nothing is addressed
                // to it yet.
                continue;
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

    /// Send `event` to exactly one peer by id. Used by the Phase 6
    /// `FetchScrollback` path — the reply only belongs to the
    /// requester, not the whole fan-out. Drops the peer if the write
    /// fails. Returns whether the send happened (false when the peer
    /// id was not in the table at lookup time).
    ///
    /// Also consults the capnp fanout sink (if attached) so a
    /// scrollback reply routed to a capnp-side requester still lands
    /// on their per-request reply channel.
    /// The installed opener, if this session has one.
    fn app_opener(&self) -> Option<Arc<AppOpener>> {
        self.app_opener.lock().ok().and_then(|g| g.clone())
    }

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

/// Outcome of [`translate`]. The broker hot path consumes one of these
/// per inbound request and decides whether to fan out, reply to one
/// peer, or do nothing. Splitting the cases as a typed enum (rather
/// than two `Option`s + a bool) keeps `reader_loop`'s post-translate
/// logic dispatch-on-pattern clean.
enum TranslatedAction {
    /// Nothing to do — `Hello` swallowed, invalid op dropped after
    /// auditing, sender never announced a route, etc.
    Nothing,
    /// Forward `event` to the peers `route` addresses, skipping the
    /// originating one.
    Fanout {
        event: CollabEvent,
        route: CrdtRoute,
    },
    /// Reply only to the originating peer. Used by `FetchScrollback`
    /// — peers other than the requester have no interest in a paged
    /// history fetch they didn't ask for.
    ReplyToSource(CollabEvent),
    /// A `SignedSyncIncremental` validated and was a `Compaction`
    /// checkpoint. The broker should still fan the envelope out
    /// (peers downstream need to evict their own logs too) but the
    /// local channel state has already been mutated by the time this
    /// variant is returned.
    ///
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

/// Phase 9: apply a validated Redact envelope to the shared channel
/// state.
///
/// Hoisted to a helper because both the local-broker hot path
/// (`translate`) and the libp2p drain thread reach the same code
/// after a Redact validates. Both paths have already pushed the
/// envelope onto the op log via `record_validated_op`; this helper
/// only owns the derived-state mutation + the redaction-record
/// audit entry.
pub(crate) fn apply_redaction_from_envelope(
    envelope: &SignedOp,
    artifact: ArtifactId,
    channels: &Arc<Mutex<BrokerChannels>>,
) {
    // Parse the payload defensively — the validator's step 5b already
    // ran parse_redact_payload, so a Some(_) here should never come
    // back malformed; we keep the parse so this helper can be reused
    // outside the validated path (drain thread + future replays).
    let req = match parse_redact_payload(envelope) {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(
                artifact = ?artifact,
                "collab_broker: malformed Redact payload after validation: {err}",
            );
            return;
        }
    };
    if let Ok(mut ch) = channels.lock() {
        let state = ch.state_for(artifact);
        match state.apply_redaction(&req) {
            Ok(rewritten) => {
                tracing::info!(
                    artifact = ?artifact,
                    target = ?req.target_op_id,
                    rewritten,
                    "collab_broker: applied redaction",
                );
            }
            Err(err) => {
                tracing::warn!(
                    artifact = ?artifact,
                    target = ?req.target_op_id,
                    "collab_broker: apply_redaction failed: {err}",
                );
                return;
            }
        }
        ch.redactions.record(
            artifact,
            RedactionRecord {
                redact_envelope: Box::new(envelope.clone()),
                target_op_id: req.target_op_id,
                target_op_hash: req.target_op_hash,
                redactor_pk: req.redacted_by,
                reason: req.reason,
                redacted_at: req.redacted_at,
            },
        );
    }
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

/// Translate one `CollabRequest` (already deserialized off the wire)
/// to the action peers should observe. Returns
/// [`TranslatedAction::Nothing`] for `Hello` (which is swallowed —
/// see module docs).
///
/// `src_route` is the route the sending connection announced on its
/// `Hello`. `None` means it never announced one: every fan-out case
/// then resolves to [`TranslatedAction::Nothing`], because the broker
/// has no idea who the frame is for. Only [`CollabRequest::FetchScrollback`]
/// still answers, since that reply is addressed to the requester by
/// peer id rather than by route.
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
        CollabRequest::OfferBlob { hash, bytes } => {
            // Cluster scope, not record scope: content is addressed by its own
            // hash, and two apps in a room may reference the same picture from
            // two different documents. The room is the audience; a different
            // room is not.
            let Some(route) = src_route.map(CrdtRoute::to_cluster_scope) else {
                return unrouted(src_id, "OfferBlob");
            };
            TranslatedAction::Fanout {
                event: CollabEvent::BlobArrived { hash, bytes },
                route,
            }
        }
        // Answered by the outbound bridge, which is the half that can reach
        // this machine's store and the room. Nothing to fan locally: an app
        // that lacks content is not news to its neighbours.
        CollabRequest::WantBlob { .. } => TranslatedAction::Nothing,
        // Intercepted before this, in the reader loop, because it is addressed
        // to this session rather than to an audience. Named here anyway so the
        // day something reaches translate with one it is a dropped frame with
        // a reason beside it, not a wildcard swallowing it.
        CollabRequest::OpenApp { ref app_id } => {
            tracing::warn!(
                src_id, %app_id,
                "collab_broker: an OpenApp reached the fan-out; it has no \
                 audience and was dropped"
            );
            TranslatedAction::Nothing
        }
        CollabRequest::Presence { key, payload } => {
            // Chrome traffic: cluster scope. Every app in the cluster
            // is a legitimate audience for "who else is here", but a
            // different cluster is not.
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
            // A signed envelope is still a CRDT payload: it belongs to
            // the sender's app + cluster and goes nowhere else. Bail
            // before validation when the sender never said hello —
            // there is no audience to validate for.
            let Some(route) = src_route.cloned() else {
                return unrouted(src_id, "SignedSyncIncremental");
            };
            // Phase 3 hot path: validate the envelope against the
            // per-artifact `ChannelState` before fanning out. Default
            // state has empty `authorised_signers`, so a freshly-
            // touched artifact rejects every signed op until its
            // role table is registered (the secure default).
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
                    // Phase 6 housekeeping after a green validation:
                    // record the op in the per-artifact in-memory log,
                    // then — if this op is itself a Compaction
                    // checkpoint — apply it. The order matters: a
                    // checkpoint op's own envelope should NOT be
                    // evicted by its own application (its seq is
                    // strictly greater than `up_to_seq` by
                    // construction), so recording first and
                    // compacting afterwards is safe.
                    //
                    // Phase 9 housekeeping: if this op is a Redact,
                    // parse the payload and apply the redaction —
                    // rewrites local op-log entries matching
                    // `target_op_id` to the redacted marker. Records
                    // a `RedactionRecord` in the redaction log so
                    // `BrokerHandle::redactions_for` can surface the
                    // governance event to the chrome.
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
                                    // Malformed payload from a peer
                                    // who passed signature + role
                                    // checks. Log + skip the
                                    // compaction; the envelope still
                                    // fans out (other peers can audit
                                    // independently).
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
                        // The same identity this connection's presence
                        // is stamped with. A delta is what a caret is
                        // drawn from, so attributing it differently
                        // from the pointer beside it put one person on
                        // screen twice, under two names and two
                        // colours.
                        from_user_id: src_user_id.to_string(),
                    };
                    if compaction_evicted.is_some() {
                        TranslatedAction::CompactionApplied {
                            event,
                            route,
                        }
                    } else {
                        TranslatedAction::Fanout { event, route }
                    }
                }
                Err(ValidationError::OpAlreadyRedacted { target }) => {
                    // Phase 9 §7: a buggy or offline peer re-broadcast
                    // an op whose id we have already redacted. Do NOT
                    // fan it out, but DO push a marker-payload entry
                    // into the local op log so the seq stream doesn't
                    // form a hole (late-joiners would otherwise hunt
                    // cooperative scrollback for an op that we deleted
                    // by policy). Also audit-log the late arrival so
                    // an operator can see if a peer is repeatedly
                    // resurrecting a redacted op.
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
            // Phase 6: read-only access to the artifact's in-memory
            // op log. The broker returns up to `max` ops in seq
            // order; the `exhausted` flag tells the requester
            // whether to fall back to the cooperative path.
            //
            // The `ScrollbackBatch` goes ONLY to the requesting
            // peer — other locally-connected apps did not ask, and
            // would just have to filter on `artifact` anyway.
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

/// Handle the read side of one connected peer. Runs until EOF / broken
/// pipe, then evicts the peer from the table so concurrent writers
/// stop targeting a dead stream.
///
/// Reaching other machines is [`PeerPublisher`]'s job — the same
/// instance the capnp transport holds, so an edit typed in a
/// sandboxed app and one typed in a JSON-dialect app take the identical
/// route out of this host.
fn reader_loop(
    peer: Arc<Peer>,
    mut stream: UnixStream,
    table: Arc<PeerTable>,
    peer_sync: Option<Arc<ShellNetwork>>,
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
        // Capture the Hello route before translating — the request is
        // consumed by `translate`, and both the local fan-out filter
        // and the libp2p bridge need the announced address.
        if let CollabRequest::Hello {
            ref app_id,
            cluster_id,
            ref record,
            ref handle,
        } = req
        {
            if let Ok(mut guard) = peer.route.lock() {
                *guard = Some(CrdtRoute::app(app_id.clone(), cluster_id.to_string()).on_record(
                    record.clone().unwrap_or_default(),
                ));
            }
            if let Ok(mut guard) = peer.handle.lock() {
                *guard = handle.clone().filter(|h| !h.trim().is_empty());
            }
        }
        let src_route = peer.route();
        // Outbound bridge: send the payload to the room before we fan
        // it to local peers. Cloning the data here is unavoidable
        // because we still need the original payload for the local
        // `translate` path. A peer that never said hello has no route
        // to stamp, so its frame is not republished either — the same
        // fail-closed rule the local fan-out applies.
        match (&src_route, &req) {
            (Some(route), CollabRequest::SyncIncremental { data }) => {
                publisher.document_incremental(route, Transport::Json, data.clone());
            }
            (Some(route), CollabRequest::SignedSyncIncremental { envelope }) => {
                // Validation happens on the receiving side, against
                // that host's own channel state — publishing an
                // envelope is not vouching for it, and a peer that
                // has not been told the artifact's role table will
                // reject it exactly as this host would.
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
                // A whole document is the frame that makes two
                // *separately created* documents one document. The
                // toolkit sends it in reply to a peer's `Arrived`,
                // never on a commit, so publishing it answers a
                // question rather than announcing into the room —
                // `ShellNetwork` holds that gate and the gossip size
                // cap. Without it the deltas above cross correctly and
                // refer to objects the receiving document has never
                // seen, and the peer's caret moves over a blank page.
                publisher.document_snapshot(route, Transport::Json, data.clone());
            }
            (Some(route), CollabRequest::OfferBlob { hash, bytes }) => {
                // The bytes behind a reference the app has just published.
                // This is the only path they have out of the app's sandbox,
                // and until this landed the shell held a document naming a
                // photograph it did not have and could send nobody.
                if !publisher.offer_blob(route, Transport::Json, hash, bytes) {
                    tracing::warn!(
                        hash = %hash, bytes = bytes.len(),
                        "collab_broker: could not take content from peer-{}; a document \
                         referencing it will show a placeholder on every other machine",
                        peer.id,
                    );
                }
            }
            (Some(route), CollabRequest::WantBlob { hash }) => {
                // Answer from this machine's store if the content is already
                // here — a peer may have offered it before the app that needs
                // it existed. Otherwise ask the room.
                if let Some(bytes) = publisher.blob(hash) {
                    let evt = CollabEvent::BlobArrived {
                        hash: hash.clone(),
                        bytes,
                    };
                    if let Ok(json) = serde_json::to_vec(&evt) {
                        let _ = write_frame(&peer.writer, &json);
                    }
                } else {
                    publisher.want_blob(route, Transport::Json, hash);
                }
            }
            (
                None,
                CollabRequest::OfferBlob { .. } | CollabRequest::WantBlob { .. },
            ) => {
                tracing::warn!(
                    "collab_broker: peer-{} offered or asked for document content \
                     before saying Hello — it has no route, so the content cannot \
                     reach or come from another machine",
                    peer.id,
                );
            }
            (Some(route), CollabRequest::Presence { .. }) => {
                // Presence has to take the bridge too: the local
                // fan-out below reaches apps on this host, so without
                // this a peer's caret stopped at the machine that
                // produced it and every collaborator looked like they
                // were doing nothing.
                //
                // The frame crosses exactly as it arrived — the shell
                // interprets a caret no more than it interprets a
                // change blob — and the receiving broker re-addresses
                // it to the peer it came from before handing it to
                // local apps.
                publisher.presence(route, Transport::Json, frame.clone());
            }
            (
                None,
                CollabRequest::SyncCRDT { .. } | CollabRequest::Presence { .. },
            ) => {
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
        // Phase 6 cooperative-fetch outbound bridge: a local
        // `FetchScrollback` always replies first from the local op
        // log (handled by `translate`). When peer-sync is wired AND
        // the local broker's reply turned out to be empty, ALSO
        // publish a cooperative request so remote peers can chime in.
        // The remote responses funnel back through the drain thread
        // and reach the requesting app as a follow-up
        // `ScrollbackBatch`.
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
        // Resolved per frame rather than once at Hello: the route is
        // only known after the Hello, and the room can name a peer at
        // any point after that. `LocalIdentity` caches the room lookup
        // so this stays cheap at pointer rate.
        let src_user_id = identity.resolve(src_route.as_ref(), peer.announced_handle().as_deref());
        // Opening an application never reaches `translate`, because it has no
        // audience to be translated for. It is addressed to *this session*:
        // relaying it would put a window on every peer's desktop because
        // somebody else's participant needed one, which nobody asked for and
        // no peer could refuse. So it leaves the fan-out here.
        if let CollabRequest::OpenApp { app_id } = &req {
            let room = src_route
                .as_ref()
                .map(|r| r.cluster_id.clone())
                .unwrap_or_default();
            let opener = table.app_opener();
            match opener {
                Some(open) => {
                    tracing::info!(
                        peer = peer.id, %app_id, %room, who = %src_user_id,
                        "collab_broker: a participant asked this session to open \
                         an application"
                    );
                    open(OpenAppRequest {
                        app_id: app_id.clone(),
                        room,
                        who: src_user_id.clone(),
                        handle: peer.announced_handle(),
                        peer: peer.id,
                    });
                }
                // No desktop behind this broker. Said rather than dropped:
                // the caller is blocked on an answer, and "nothing here can
                // open applications" is one, where silence is not.
                None => {
                    tracing::warn!(
                        peer = peer.id, %app_id,
                        "collab_broker: asked to open an application, but this \
                         session has no desktop to open one in"
                    );
                    let _ = table.send_to_peer(
                        peer.id,
                        &CollabEvent::AppOpenRefused {
                            app_id: app_id.clone(),
                            why: "this session cannot open applications"
                                .to_string(),
                        },
                    );
                }
            }
            continue;
        }
        match translate(peer.id, &src_user_id, src_route.as_ref(), req, &channels) {
            TranslatedAction::Nothing => {}
            TranslatedAction::Fanout { event, route } => {
                table.fanout_to_route(&event, peer.id, &route);
            }
            TranslatedAction::ReplyToSource(event) => {
                // Phase 6 targeted reply: scrollback batches only go
                // to the peer that asked. Failure is silently
                // dropped; the peer can re-ask.
                let _ = table.send_to_peer(peer.id, &event);
                // Cooperative follow-up: if the local reply was
                // empty (the in-memory log holds nothing in the
                // requested range), broadcast a `ScrollbackRequest`
                // over libp2p. Any peer holding the range responds
                // via `ScrollbackResponse`, which the broker drain
                // thread then fans into a follow-up `ScrollbackBatch`
                // for the locally-connected app.
                if let Some((art, before_seq, max)) = cooperative_fetch {
                    let is_empty = matches!(
                        &event,
                        CollabEvent::ScrollbackBatch { ops, .. } if ops.is_empty()
                    );
                    if is_empty {
                        // Remember who is waiting. The gossipsub
                        // response comes back with an artifact id and
                        // nothing else, so without this note the
                        // broker would have to guess at delivery —
                        // and guessing means broadcasting.
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
            TranslatedAction::CompactionApplied {
                event,
                route,
            } => {
                // The local compaction has already been applied
                // inside `translate`. Fan the envelope out so peers
                // can replicate the same eviction; without this they
                // never converge on the new horizon.
                table.fanout_to_route(&event, peer.id, &route);
            }
        }
    }
    tracing::debug!("collab_broker: peer-{} disconnected", peer.id);
    table.remove(peer.id);
}

/// Public handle returned by [`spawn`]. Owns the listener thread's join
/// handle so the broker doesn't get torn down mid-session — drop this
/// (e.g. at shell shutdown) to start the cleanup chain. The socket
/// file is best-effort-removed on `Drop` too, so a crashed shell can
/// be restarted without a stale-inode collision.
pub struct BrokerHandle {
    /// Path the legacy JSON listener is bound to. Held for the
    /// `Drop` `unlink`.
    socket_path: PathBuf,
    /// Optional capnp listener socket path — only present when the
    /// capnp interface is bound alongside the legacy one. Removed on
    /// `Drop` best-effort.
    capnp_socket_path: Option<PathBuf>,
    /// Listener thread handle. Wrapped in `Option` so `Drop` can
    /// `take()` and `join()` it without violating the moved-out rule.
    listener_thread: Option<JoinHandle<()>>,
    /// Phase 2 inbound-drain thread handle. Spawned only when the
    /// broker has a `ShellNetwork` to pump.
    drain_thread: Option<JoinHandle<()>>,
    /// Drain-thread quit flag. Drop sets it; the drainer exits at
    /// its next wake.
    drain_quit: Arc<AtomicBool>,
    /// Capnp listener thread handle. `None` when the capnp interface
    /// is not bound. The thread runs a tokio LocalSet driving
    /// capnp-rpc futures.
    capnp_thread: Option<JoinHandle<()>>,
    /// Capnp listener quit flag. Drop sets it; the capnp accept-loop
    /// exits at its next wake.
    capnp_quit: Arc<AtomicBool>,
    /// Shared peer table — the reader threads use it; the broker keeps
    /// a reference so external callers (Phase 2: libp2p bridge) can
    /// fan a remote event in to every locally-connected peer.
    pub(crate) peers: Arc<PeerTable>,
    /// Phase 3 channel-state cache + audit log. Reader threads pull
    /// the lock for the duration of one validation; tests + the
    /// eventual Phase 3.5 bootstrap path mutate the state map to
    /// register role attestations before traffic flows.
    channels: Arc<Mutex<BrokerChannels>>,
}

impl BrokerHandle {
    /// Snapshot the number of currently-connected peers. Used by tests
    /// to wait for the listener to register everyone before sending.
    pub fn peer_count(&self) -> usize {
        self.peers.peers.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// How many connected peers have announced a route (i.e. have
    /// said `Hello`). Connecting and announcing are separate steps, so
    /// this is strictly ≤ [`Self::peer_count`]; anything above the
    /// difference is a peer the broker cannot address yet.
    pub fn announced_peer_count(&self) -> usize {
        self.peers
            .peers
            .lock()
            .map(|g| g.iter().filter(|p| p.route().is_some()).count())
            .unwrap_or(0)
    }

    /// How many presence streams the capnp transport has registered.
    ///
    /// An app is told it is connected as soon as the socket is up, but
    /// its subscriptions are opened afterwards, from another thread.
    /// Presence is fanned out live and never replayed, so a frame
    /// published into that gap reaches nobody and never will. This is
    /// how a caller waits for the gap to close: start a session, watch
    /// this rise, then publish. Zero when the capnp transport is not
    /// bound.
    pub fn capnp_presence_subscribers(&self) -> usize {
        self.peers
            .capnp_state()
            .map(|s| s.presence_subscriber_count())
            .unwrap_or(0)
    }

    /// Fan a `CollabEvent` to the connected peers `route` addresses.
    /// The optional `except` skips a single peer id (used by the
    /// libp2p bridge: a remote update should reach every addressed
    /// local peer, but a locally-originated update has already been
    /// delivered to the broker's other peers via the per-reader
    /// fan-out and only needs to go onto the wire).
    ///
    /// There is deliberately no "reach everyone" form — see
    /// [`PeerTable::fanout_to_route`].
    pub fn fanout_event(&self, event: &CollabEvent, except: Option<u64>, route: &CrdtRoute) {
        self.peers
            .fanout_to_route(event, except.unwrap_or(u64::MAX), route);
    }

    /// Subscribe this process to the rooms it hosts.
    ///
    /// The shell runs the broker but is not otherwise a reader of it,
    /// so everything its apps published reached the other apps and
    /// stopped there. This is how the compositor reads the same
    /// stream — addressed by room, and delivered by the broker's own
    /// [`CrdtRoute::delivers_to`], so a subscription sees exactly what
    /// a socket peer in that room would see and nothing more.
    ///
    /// The sink runs on the reader thread that decoded the frame.
    /// Keep it short: hand the event to a queue and return.
    pub fn subscribe_rooms(
        &self,
        rooms: Vec<String>,
        sink: Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>,
    ) -> Arc<RoomSubscription> {
        self.peers.subscribe_rooms(rooms, sink)
    }

    /// Shared handle to the broker's per-channel state + audit log.
    /// Tests use this to install a [`ChannelState`] *before* sending
    /// signed envelopes through the broker. The eventual Phase 3.5
    /// bootstrap path is the production caller.
    /// The bound Cap'n Proto socket, or `None` if this broker has no
    /// app-facing transport.
    ///
    /// `None` after asking for one means the bind failed: the legacy
    /// JSON listener is up but no launcher-injected app can reach it,
    /// which is a session with collaboration switched off. Callers
    /// that requested a capnp path are expected to check, because
    /// `spawn_full` returning `Ok` only speaks for the JSON socket.
    pub fn capnp_socket(&self) -> Option<&std::path::Path> {
        self.capnp_socket_path.as_deref()
    }

    pub fn channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.channels)
    }

    /// Install what this session does when a participant asks it to open an
    /// application.
    ///
    /// Installed after the broker is bound rather than passed to `spawn`,
    /// because the thing that answers — the desktop — is stood up around the
    /// broker and not before it. Until one is installed every request is
    /// refused with the reason, which is the right answer for a broker with no
    /// desktop behind it (the two-peer tests, a headless relay) and a much
    /// better one than accepting a request nothing will ever act on.
    pub fn set_app_opener(&self, opener: AppOpener) {
        if let Ok(mut guard) = self.peers.app_opener.lock() {
            *guard = Some(Arc::new(opener));
        }
    }

    /// Tell the connection that asked that its application will not be opened.
    ///
    /// Addressed to the one peer, never fanned: a refusal is an answer to a
    /// question, and nobody else asked it. Called from the session's own
    /// thread once it has consulted the workspace whose capabilities decide —
    /// which is why this is a separate call rather than a return value from
    /// [`Self::set_app_opener`]'s sink.
    pub fn refuse_open(&self, peer: u64, app_id: &str, why: &str) {
        let delivered = self.peers.send_to_peer(
            peer,
            &CollabEvent::AppOpenRefused {
                app_id: app_id.to_string(),
                why: why.to_string(),
            },
        );
        if !delivered {
            // Loud, because the caller is now waiting out a timeout for a
            // question that has already been answered, and will report the
            // wait rather than the reason.
            tracing::warn!(
                peer, %app_id,
                "collab_broker: could not deliver the refusal; the caller will \
                 see this as an app that never opened"
            );
        }
    }

    /// Convenience: install a populated [`ChannelState`] for one
    /// artifact id. Equivalent to
    /// `self.channels().lock()?.register_channel_state(...)` but
    /// avoids the lock-and-unwrap dance at every call site.
    pub fn register_channel_state(&self, artifact: ArtifactId, state: ChannelState) {
        if let Ok(mut ch) = self.channels.lock() {
            ch.register_channel_state(artifact, state);
        }
    }

    /// Snapshot the audit-log entries logged for one artifact. Used by
    /// tests to assert that a dropped envelope landed in the right
    /// rejection bucket.
    pub fn audit_for(&self, artifact: ArtifactId) -> Vec<AuditEntry> {
        self.channels
            .lock()
            .map(|g| g.audit.for_artifact(artifact))
            .unwrap_or_default()
    }

    /// Phase 9: snapshot the structured redaction records for one
    /// artifact. Used by chrome widgets (the activity timeline +
    /// history scrubber) and by tests that want to assert "this
    /// op was redacted by this owner at this time with this reason."
    /// Independent of the rejection-audit `audit_for` so the
    /// compliance surface doesn't have to filter through every
    /// dropped envelope.
    pub fn redactions_for(&self, artifact: ArtifactId) -> Vec<RedactionRecord> {
        self.channels
            .lock()
            .map(|g| g.redactions.for_artifact(artifact))
            .unwrap_or_default()
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        // Signal the inbound-drain thread to exit at its next wake.
        self.drain_quit.store(true, Ordering::Relaxed);
        // Signal the capnp accept loop to exit.
        self.capnp_quit.store(true, Ordering::Relaxed);
        // Removing the socket file is best-effort — if the listener
        // thread already cleaned up (or another shell instance rebound
        // the path), don't surface a spurious error.
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(p) = &self.capnp_socket_path {
            let _ = std::fs::remove_file(p);
        }
        // We can't reliably join the listener thread here because it's
        // blocked on `listener.accept()`; the socket-file removal alone
        // doesn't unblock `accept`. Detach the join handle — the
        // process-exit path will reap it. Tests that need a clean
        // teardown drop their connections first; reader threads
        // observe EOF and exit on their own.
        let _ = self.listener_thread.take();
        let _ = self.capnp_thread.take();
        // Best-effort join the drain thread — it polls its quit flag
        // on a 100ms cadence so a clean shutdown is bounded.
        if let Some(t) = self.drain_thread.take() {
            let _ = t.join();
        }
    }
}

/// Bind `socket_path` and detach the listener thread.
///
/// Returns the [`BrokerHandle`] — keep it alive for the lifetime of
/// the session so the broker doesn't get dropped (and its socket
/// `unlink`ed) out from under live apps.
///
/// On bind failure (path already locked by another process, no write
/// permission, …) returns the underlying `io::Error` so the caller
/// can log a `tracing::warn!` and continue without collab — the shell
/// must keep working even without the broker.
pub fn spawn(socket_path: PathBuf) -> std::io::Result<BrokerHandle> {
    spawn_full(socket_path, None, None)
}

/// Legacy shim retained so existing hosted-mode callers keep working
/// after `capnp_socket_path` was factored out into [`spawn_full`].
/// Callers that want the capnp interface should call `spawn_full`.
pub fn spawn_with_peer_sync(
    socket_path: PathBuf,
    peer_sync: Option<Arc<ShellNetwork>>,
) -> std::io::Result<BrokerHandle> {
    spawn_full(socket_path, None, peer_sync)
}

/// Full-featured broker spawn.
///
/// - `socket_path`: legacy JSON socket (`QUICK_COLLAB_SOCK`). Bound
///   unconditionally so the existing SDK-migration path (bridge +
///   legacy tests) keeps working.
/// - `capnp_socket_path`: optional native Cap'n Proto socket
///   (`QOS_SVC_COLLAB` in `runtime-portal` terms — the path the
///   launcher fd-injects, or a well-known Unix path for local dev
///   and tests). When `Some`, the shell also serves the
///   `runtime-collab` schema natively; both transports share fanout
///   through the same [`PeerTable`], so an event from either side
///   reaches every subscriber on either side.
/// - `peer_sync`: optional libp2p bridge; unchanged semantics.
pub fn spawn_full(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    peer_sync: Option<Arc<ShellNetwork>>,
) -> std::io::Result<BrokerHandle> {
    let identity = LocalIdentity::from_network(peer_sync.clone());
    // The outbound half, built once and shared by both transports.
    // One instance is the whole point: when the capnp server held no
    // network handle and the JSON reader loop held one, "does a local
    // edit leave this machine" had two answers, and the one sandboxed
    // apps got was "no".
    let publisher = PeerPublisher::from_network(peer_sync.clone());
    spawn_inner(socket_path, capnp_socket_path, peer_sync, identity, publisher)
}

/// [`spawn_full`], with the "who is this" lookup supplied rather than
/// built from the network.
///
/// The room document is the only implementation production uses —
/// `spawn_full` builds it and every caller goes through that. This
/// exists so the attribution the broker stamps can be exercised
/// against a room whose answers are known, without standing a libp2p
/// node up to be told them.
pub fn spawn_full_with_identity(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    peer_sync: Option<Arc<ShellNetwork>>,
    identity: Arc<LocalIdentity>,
) -> std::io::Result<BrokerHandle> {
    let publisher = PeerPublisher::from_network(peer_sync.clone());
    spawn_inner(socket_path, capnp_socket_path, peer_sync, identity, publisher)
}

/// Same broker, with the outbound half supplied by the caller.
///
/// The seam exists so a test can watch what actually reaches the wire
/// from the real server methods — the alternative is standing up
/// libp2p, which would prove the transport works and say nothing about
/// whether the broker handed it anything. `peer_sync` stays `None`
/// here: the inbound drain is a separate concern and has its own
/// coverage.
pub fn spawn_with_publisher(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    publisher: Arc<PeerPublisher>,
) -> std::io::Result<BrokerHandle> {
    let identity = LocalIdentity::from_network(None);
    spawn_inner(socket_path, capnp_socket_path, None, identity, publisher)
}

fn spawn_inner(
    socket_path: PathBuf,
    capnp_socket_path: Option<PathBuf>,
    peer_sync: Option<Arc<ShellNetwork>>,
    identity: Arc<LocalIdentity>,
    publisher: Arc<PeerPublisher>,
) -> std::io::Result<BrokerHandle> {
    // Best-effort unlink: a previous shell instance may have left a
    // stale inode behind. Ignore the result — if the file doesn't
    // exist (the common case) the `unlink` errors and we continue;
    // if the file exists but `bind` will still fail (e.g. wrong
    // owner), the listener creation below surfaces the real error.
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;

    let table = Arc::new(PeerTable::new());
    // Phase 3 per-artifact channel state + audit log. Created here so
    // every reader thread gets the same `Arc`, and so the
    // `BrokerHandle::channels()` accessor returns a handle to the
    // same instance reader threads consult during validation.
    let channels: Arc<Mutex<BrokerChannels>> = Arc::new(Mutex::new(BrokerChannels::default()));
    // Who is waiting on a cooperative scrollback answer, keyed by the
    // artifact they asked about. A `ScrollbackResponse` off gossipsub
    // carries an artifact id and nothing else, so this is the only
    // record of where the answer belongs; without it the drain thread
    // would have to broadcast the batch to every local app.
    let pending_scrollback: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let listener_table = Arc::clone(&table);
    let listener_peer_sync = peer_sync.clone();
    let listener_identity = Arc::clone(&identity);
    let listener_publisher = Arc::clone(&publisher);
    let listener_channels = Arc::clone(&channels);
    let listener_pending = Arc::clone(&pending_scrollback);
    let listener_thread = std::thread::Builder::new()
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
        })?;

    // Phase 2 inbound-drain: when peer-sync is on, drain the
    // `ShellNetwork`'s `inbound_app_crdt` buffer every 100 ms and
    // fan each entry out to every locally-connected app as a
    // `CRDTIncremental`. 100 ms is well under typical CRDT
    // convergence cadence and keeps the drainer's CPU footprint
    // negligible.
    //
    // Phase 6 stretches the same drainer to cover three new queues:
    //   - inbound app-CRDT-signed envelopes (validated + fanned to
    //     local apps as `SignedCRDTIncremental`),
    //   - inbound cooperative-scrollback requests (consult local op
    //     log; if matching ops exist, publish a response over
    //     gossipsub),
    //   - inbound cooperative-scrollback responses (delivered to the
    //     route that asked, recorded in `pending_scrollback` when the
    //     request went out).
    //
    // Every remote entry is routed the same way a local one is: the
    // `(app_id, cluster_id)` the publishing host stamped on the wire
    // decides which local apps see it. A frame that arrives without a
    // route reaches nobody.
    let drain_quit = Arc::new(AtomicBool::new(false));
    let drain_thread = peer_sync
        .as_ref()
        .map(|net| {
            let inbound = net.inbound_app_crdt_handle();
            let presence_inbound = net.inbound_app_presence_handle();
            let snapshot_inbound = net.inbound_app_crdt_snapshot_handle();
            let blob_inbound = net.inbound_blob_handle();
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
                    // The name already announced for each peer, per
                    // cluster. Presence arrives at pointer rate, and the
                    // roster it implies changes almost never, so the
                    // announcement is made when the answer changes and
                    // not on every frame.
                    let mut announced: std::collections::HashMap<(String, u64), (String, u64)> =
                        std::collections::HashMap::new();
                    // And what the room said, the last time it was
                    // asked. Reading it means taking the replica's lock
                    // and parsing every peer id the room document holds,
                    // which is not a thing to do thirty times a second
                    // per peer on the path the sync round also needs
                    // that lock for. Re-asked on a cadence instead, so a
                    // name that changes still lands promptly.
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
                            // The publishing host stamped the originating
                            // app + cluster onto the wire; deliver only to
                            // local apps that announced the same pair. No
                            // local peer is skipped by id — the update
                            // originated outside this host.
                            let route = CrdtRoute::record(
                                entry.app_id,
                                entry.cluster_id,
                                entry.record_id,
                            );
                            drain_table.fanout_to_route(&event, u64::MAX, &route);
                        }

                        // Drain inbound whole-document snapshots. A
                        // peer sent one because this host's app told
                        // it that it had just arrived, so the app on
                        // this side is the one with no history and
                        // this is the frame it has been waiting for.
                        //
                        // Delivered as `CRDTUpdate`, which the toolkit
                        // *merges* rather than assigns — a snapshot
                        // arriving after the local user has already
                        // typed must not discard what they typed.
                        //
                        // Routed by the full record triple, like an
                        // increment: a document is the state of one
                        // record, and handing it to a different one
                        // would be unrecoverable rather than noisy.
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
                            let route = CrdtRoute::record(
                                entry.app_id,
                                entry.cluster_id,
                                entry.record_id,
                            );
                            drain_table.fanout_to_route(&event, u64::MAX, &route);
                        }

                        // Drain content that arrived from the room and hand it
                        // to the local apps that are waiting on it.
                        //
                        // This shell holding a photograph is not a photograph
                        // on a page: the app that draws it is another process
                        // with a store of its own, deliberately — the launcher
                        // gives every app a private `HOME` and
                        // `XDG_CACHE_HOME`, so there is no directory the two
                        // can share. Until the bytes are handed down, the app
                        // goes on drawing the placeholder while the shell one
                        // layer below has everything needed to stop it.
                        //
                        // Cluster scope with no app and no record, because
                        // content is addressed by its own hash: two apps in a
                        // room may reference the same picture from two
                        // different documents, and both should get it.
                        let drained_blobs: Vec<_> = {
                            let mut buf = match blob_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_blobs {
                            let Some(bytes) = drain_net.holds_blob(&entry.hash) else {
                                tracing::warn!(
                                    room = %entry.cluster_id, hash = %entry.hash,
                                    "collab_broker: content was reported as arrived but is \
                                     not in this machine's store; the image using it stays \
                                     a placeholder",
                                );
                                continue;
                            };
                            tracing::info!(
                                room = %entry.cluster_id, hash = %entry.hash,
                                bytes = bytes.len(),
                                "collab_broker: handing arrived content to the apps in this room",
                            );
                            let event = CollabEvent::BlobArrived {
                                hash: entry.hash,
                                bytes,
                            };
                            let route = CrdtRoute::cluster(entry.cluster_id);
                            drain_table.fanout_to_route(&event, u64::MAX, &route);
                        }

                        // Drain inbound presence and fan it to local
                        // apps, attributed to the peer that sent it.
                        //
                        // Cluster scope, matching what a local presence
                        // frame gets (`CrdtRoute::to_cluster_scope`).
                        // Narrowing remote presence to a record would
                        // make a peer visible to people on their own
                        // machine and invisible to everyone else, which
                        // is the asymmetry this bridge exists to remove.
                        let drained_presence: Vec<_> = {
                            let mut buf = match presence_inbound.lock() {
                                Ok(g) => g,
                                Err(_) => break,
                            };
                            std::mem::take(&mut *buf)
                        };
                        for entry in drained_presence {
                            // The frame is the sender's own
                            // `CollabRequest::Presence`. Anything else
                            // is a peer speaking a protocol we do not
                            // have, and is dropped rather than guessed at.
                            let Ok(CollabRequest::Presence { key, payload }) =
                                serde_json::from_slice::<CollabRequest>(&entry.frame)
                            else {
                                continue;
                            };
                            // What this host's apps can do anything
                            // with. A frame refused here is refused
                            // once, for every app at the same time —
                            // as opposed to being handed to all of
                            // them and sorted out afterwards by each.
                            if !relays_to_local_apps(&payload) {
                                continue;
                            }
                            // A peer saying it has arrived is a peer
                            // asking for a document it has none of.
                            // Note it, so the next whole document this
                            // host's apps produce is published instead
                            // of stopping at the local fan-out — and so
                            // the ones produced when nobody is waiting
                            // are not. See
                            // `ShellNetwork::note_snapshot_request`.
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

                        // Drain inbound signed envelopes — validate
                        // against the local channel state, record into
                        // the op log on success, then fan to local apps.
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
                                        if envelope.op_class == OpClass::Compaction {
                                            if let Ok(cp) =
                                                collab_types::parse_compaction_payload(&envelope)
                                            {
                                                let _ = state.apply_compaction(&cp);
                                            }
                                        }
                                    }
                                    if envelope.op_class == OpClass::Redact {
                                        apply_redaction_from_envelope(
                                            &envelope,
                                            artifact,
                                            &drain_channels,
                                        );
                                    }
                                    // The id the *same* peer's presence
                                    // arrives under, and named through
                                    // the same room lookup. It used to
                                    // be `peer-net-<handle>` while the
                                    // pointer beside it was
                                    // `peer@<handle>`, so one person was
                                    // two participants on the receiving
                                    // screen — two colours, and a roster
                                    // entry that named only one of them.
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
                                    // Signed envelopes carry the same
                                    // app + cluster stamp as the unsigned
                                    // ones. A peer too old to stamp one
                                    // leaves both fields empty, and an
                                    // unaddressed envelope is dropped
                                    // rather than shown to every app.
                                    let route = CrdtRoute::record(
                                        entry.app_id.clone(),
                                        entry.cluster_id.clone(),
                                        entry.record_id.clone(),
                                    );
                                    drain_table.fanout_to_route(&event, u64::MAX, &route);
                                }
                                Err(ValidationError::OpAlreadyRedacted { target }) => {
                                    // Phase 9 §7: late-arriving op via the
                                    // libp2p bridge whose id is already in
                                    // our redacted set. Don't fan; do push
                                    // a marker entry into the log so the
                                    // seq stream stays gapless.
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

                        // Drain inbound scrollback requests — consult
                        // local op log and publish a response if we have
                        // matching ops. Empty matches are silently
                        // dropped so the requester doesn't get spammed
                        // by every peer saying "I don't have it either".
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

                        // Drain inbound scrollback responses — deliver the
                        // payload to the route(s) that asked for it. The
                        // request side recorded them in
                        // `pending_scrollback`; a response for an artifact
                        // nobody here asked about is dropped rather than
                        // shown to every app.
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
                            // A response from gossipsub is the closing
                            // half of a cooperative round-trip; mark
                            // exhausted=true so the receiving app
                            // doesn't immediately re-ask.
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

    // Optional capnp interface. Attaches its `CapnpBrokerState` to the
    // shared `PeerTable` so every existing fanout call (`translate` →
    // `table.fanout_to_route`, drain thread → `drain_table.fanout_to_route`,
    // `BrokerHandle::fanout_event`) also reaches capnp subscribers.
    // Bind failure surfaces on the return path; a capnp bind failure
    // does NOT tear down the legacy broker (the shell must keep
    // running even if only one transport comes up).
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
        listener_thread: Some(listener_thread),
        drain_thread,
        drain_quit,
        capnp_thread,
        capnp_quit,
        peers: table,
        channels,
    })
}

/// What, if anything, to tell local apps this peer is called.
///
/// `said` is the name last announced for the peer, `known` what the room
/// says now, and `id` the `peer@<handle>` the presence frames carry.
///
/// Three cases, and the last two are the ones worth spelling out:
///
/// - The room names the peer and that is not what was last said: say it.
/// - The room has **stopped** naming a peer that was named: say the id.
///   An app told a name once holds it until told otherwise, so a label
///   taken back in the room would otherwise stay on screen for the rest
///   of the session — and it would be a name over a caret that nobody
///   is entitled to any more.
/// - The room has never named the peer and still does not: say nothing.
///   The id is already what an app that was told nothing shows, so an
///   announcement would be a frame per presence frame saying so.
/// The id a **remote** peer's traffic is attributed under, announcing
/// the room's name for it first when the room has one and nobody has
/// said it yet.
///
/// Every path that shows a remote participant goes through here — the
/// pointer, the presence, and the signed delta a caret is drawn from —
/// because they have to agree. They did not: presence used
/// `peer@<handle>` and deltas used `peer-net-<handle>`, so the roster
/// entry naming the first left the second showing a number, and the
/// palette gave one person two colours in one document.
///
/// The id stays the peer handle even once a name is known. A roster
/// entry names a participant; it does not recolour one, and a peer
/// whose colour changed the moment it was named would be a peer nobody
/// could keep track of.
///
/// `labels` caches what the room said, on [`LABEL_TTL`]: reading it
/// takes the replica's lock and parses every peer id the room document
/// holds, which is not a thing to do per pointer sample. `announced`
/// remembers what has already been said, so the roster is published
/// when the answer changes rather than on every frame.
/// Whether a presence frame that arrived from another host is
/// something this host's apps can place.
///
/// The cluster fan-out below hands one arriving frame to every app in
/// the room. That is right for everything a room shares — a caret, a
/// selection, a typing run, an arrival — because those are positions
/// in a *document*, and the document is the same document on both
/// machines. It is wrong for every pointer that arrives on this
/// queue, and the reason is structural rather than a matter of which
/// scope the sender happened to name.
///
/// This queue carries frames an *app* published. An app measures its
/// pointer where winit hands it to them: inside its own window, at
/// that machine's window size, scroll position and scale. No window
/// here is the one those pixels describe, whatever the frame calls
/// the scope it was published under — and the old spelling called it
/// the room, which named the audience and said nothing about the
/// space at all. Relaying them is what drew one person once per open
/// window: each app took the numbers for its own and placed them at
/// the same offset in its own content.
///
/// Where that person's pointer *is* arrives by the path that can say
/// so, and does so whatever version they are running: their
/// compositor republishes the position in desktop coordinates
/// (`shell::peer_presence`), and this host's compositor paints it
/// once, above every window. So nothing is lost by refusing the
/// in-window copy — the second answer to a question that already has
/// one is exactly what is being removed.
///
/// Refused for every pointer rather than only for the ones that admit
/// which surface they were measured in, because the rule has to hold
/// against a peer running an older build. One that names no space is
/// not a pointer whose space is the desktop; it is a pointer that did
/// not say, and this side cannot place what was never located.
pub(crate) fn relays_to_local_apps(payload: &ui::collaboration::PresencePayload) -> bool {
    !matches!(payload, ui::collaboration::PresencePayload::Pointer { .. })
}

fn remote_participant_id(
    cluster_id: &str,
    peer: u64,
    labels_of: &dyn Fn(&str) -> PeerLabels,
    table: &PeerTable,
    labels: &mut HashMap<String, (Instant, PeerLabels)>,
    announced: &mut HashMap<(String, u64), (String, u64)>,
) -> String {
    // Attributed to the publishing host, not to a local peer slot:
    // this is what the receiving app colours the caret by, so two
    // peers must never collapse onto one id.
    let user_id = format!("peer@{peer}");
    let now = Instant::now();
    let fresh = labels
        .get(cluster_id)
        .is_some_and(|(at, _)| now.duration_since(*at) < LABEL_TTL);
    if !fresh {
        labels.insert(cluster_id.to_string(), (now, labels_of(cluster_id)));
    }
    // What this peer is called, according to the room -- which is to
    // say, according to whoever invited them. A peer that has not been
    // named has no entry, and is left labelled with the id above
    // rather than given a name nobody chose.
    let slot = (cluster_id.to_string(), peer);
    let known = labels
        .get(cluster_id)
        .and_then(|(_, m)| m.get(&peer))
        .map(String::as_str);
    let prior = announced.get(&slot);
    let generation = table.connections_seen();
    let say = roster_announcement(prior.map(|(n, _)| n.as_str()), known, &user_id)
        // A name is said when the answer changes, which is right for a
        // roster that changes almost never and wrong for an app that
        // connected after it was said: the audience for that
        // announcement did not include it, and nothing would ever say
        // it again. That is how a caret ended up showing a handle on a
        // screen whose chrome, which reads the room directly, showed
        // the name — the app had simply missed the one broadcast.
        //
        // So a connection this answer has not been said in front of is
        // itself a reason to say it again.
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
                // The palette colour the id already resolves to.
                color: [0.0, 0.0, 0.0],
            }],
            permissions: Vec::new(),
        };
        // Cluster scope: a roster is chrome traffic, and every app in
        // the room is a legitimate audience for who is in it.
        table.fanout_to_route(&roster, u64::MAX, &CrdtRoute::cluster(cluster_id.to_string()));
    }
    user_id
}

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
    /// The peer id a presence frame carries. A digest, which is why a
    /// name is worth attaching to it and why it is not one.
    const PEER: &str = "peer@5537458103208628978";

    /// The demo beat: a peer the room has named is announced under that
    /// name, once.
    #[test]
    fn a_named_peer_is_announced_and_then_left_alone() {
        assert_eq!(
            super::roster_announcement(None, Some("Xaver"), PEER),
            Some("Xaver".to_string()),
        );
        // Presence arrives at pointer rate. Saying it again every frame
        // would be a roster update per pointer move.
        assert_eq!(super::roster_announcement(Some("Xaver"), Some("Xaver"), PEER), None);
    }

    /// A peer nobody has named produces no announcement at all. The id
    /// is already what an app that was told nothing shows.
    #[test]
    fn an_unnamed_peer_is_not_announced() {
        assert_eq!(super::roster_announcement(None, None, PEER), None);
    }

    /// And a name taken back has to be taken back on screen. An app
    /// holds a name until it is told otherwise, so a label the room has
    /// revoked would otherwise sit over that peer's caret for the rest
    /// of the session.
    #[test]
    fn a_revoked_name_reverts_the_peer_to_its_id() {
        assert_eq!(
            super::roster_announcement(Some("Xaver"), None, PEER),
            Some(PEER.to_string()),
        );
        // …and once said, not repeated.
        assert_eq!(super::roster_announcement(Some(PEER), None, PEER), None);
    }

    /// A remote peer is one participant on the receiving screen, and
    /// the same one whichever kind of frame it sent.
    ///
    /// Presence carried `peer@<handle>` and signed deltas carried
    /// `peer-net-<handle>`, so a peer that typed and pointed became two
    /// entries in the palette, and the roster entry — announced against
    /// the first form — named only one of them. The caret said a number
    /// while the pointer beside it said a name.
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

        // The pointer arrives first, then the delta the caret is drawn
        // from — the order a person typing produces.
        let from_presence = super::remote_participant_id(
            "room-1", HANDLE, &room, &table, &mut labels, &mut announced,
        );
        let from_delta = super::remote_participant_id(
            "room-1", HANDLE, &room, &table, &mut labels, &mut announced,
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

    /// An app that starts after the name was said still learns it.
    ///
    /// The roster is published when the room's answer changes, because
    /// presence arrives at pointer rate and a roster update per pointer
    /// move is not a roster. But "changes" was measured against what
    /// had been *said*, not against who had *heard* it, so an app that
    /// connected a second later missed the one broadcast and nothing
    /// ever repeated it. Its caret drew the peer's handle while the
    /// chrome two centimetres away — which reads the room directly —
    /// drew the name. Both were reporting honestly; only one had been
    /// told.
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
                "room-1", HANDLE, &room, &table, &mut labels, &mut announced,
            );
        };

        say();
        say();
        assert_eq!(
            heard.lock().unwrap().clone(),
            vec!["Xaver".to_string()],
            "a name that has not changed is not re-broadcast per frame",
        );

        // Somebody connects — an app opening, on either transport.
        let _ = table.next_peer_id();
        say();
        assert_eq!(
            heard.lock().unwrap().clone(),
            vec!["Xaver".to_string(), "Xaver".to_string()],
            "the app that just connected was not in the audience for the \
             first announcement, so the name has to be said again",
        );

        // And then it settles again.
        say();
        assert_eq!(
            heard.lock().unwrap().len(),
            2,
            "once everyone connected has heard it, it stops being said",
        );
    }

    /// The same claim, on the production drain rather than the helper
    /// it calls.
    ///
    /// The unit test above pins `remote_participant_id`'s contract; it
    /// says nothing about whether the signed-delta drain actually calls
    /// it, and that is precisely where the bug lived — the delta path
    /// minted `peer-net-<handle>` inline, right beside a presence path
    /// that had been resolving names correctly for months. So this
    /// drives a real room, a real signed envelope through
    /// `ShellNetwork`'s inbound buffer, and reads what a connected app
    /// is handed.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_remote_delta_reaches_an_app_under_the_name_the_room_wrote() {
        use collab_types::{ArtifactId, ArtifactKind, ChannelId, ChannelState, OpClass, Role};

        // A room this machine governs, in which the inviter has named
        // the peer that is about to send something.
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let ws = tree.append_empty_workspace("Design review");
        let room = tree.room_of(ws).expect("a fresh workspace mints a room");
        let owner = "quinten".to_string();
        tree.adopt_ownerless_workspaces(&owner);
        let xaver = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        assert!(
            tree.name_peer(room, &owner, &xaver.to_base58(), "Xaver"),
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

        // A document the sender may write to.
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
            cluster.clone(),
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

        let events = poll_until(&mut watcher, 2, Duration::from_millis(2000));
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

    /// Unique-per-test socket path so concurrent test runs (or a
    /// resurrected stale inode) don't collide.
    fn temp_sock(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "qos-collab-broker-{}-{}-{:?}.sock",
            tag,
            std::process::id(),
            std::thread::current().id(),
        ))
    }

    /// Drain a manager until at least one of each desired variant
    /// shows up or `timeout` elapses. Returns whatever it managed
    /// to collect.
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

    /// Block briefly until the broker has accepted exactly `n`
    /// connections. Replaces a wall-clock sleep with a poll on the
    /// peer table.
    fn wait_for_peers(handle: &BrokerHandle, n: usize, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if handle.peer_count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Block until `n` peers have announced a route. Accepting a
    /// connection and reading its `Hello` are separate steps, and only
    /// the second one makes a peer addressable — a send that races the
    /// second peer's `Hello` would otherwise reach nobody.
    fn wait_for_announced(handle: &BrokerHandle, n: usize, timeout: Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if handle.announced_peer_count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Connect a CollaborationManager to `path` announcing `app_id` in
    /// cluster `cluster_id`, mirroring the toolkit's own `new()`
    /// constructor but using explicit values so the test is
    /// independent of process-wide env state.
    ///
    /// The env vars are what `ui::collaboration::auto_hello_env` reads
    /// to build the auto-Hello, so a manager built here announces
    /// exactly the route the caller names — which is what the broker
    /// filters delivery on. Left unset, `QUICK_APP_ID` defaults to a
    /// fresh random uuid per manager, i.e. every peer would be a
    /// different app and nothing would be delivered.
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

    /// Connect a manager announcing a **record** as well as the app and
    /// cluster — one document's channel. Same env-var mechanism as
    /// [`connect_manager_as`]; the record is passed to the toolkit
    /// directly because a process can hold several records at once and
    /// an environment variable cannot say which of them a given
    /// connection is for.
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

    /// A fixed cluster uuid the peers in a scenario share. Any valid
    /// uuid works; a constant keeps the scenarios readable.
    const TEST_CLUSTER: &str = "3f2b1c00-0000-4000-8000-000000000001";

    /// Connect a manager as a peer of the same app in the shared test
    /// cluster — the common case for "N instances of one app".
    fn connect_manager(path: &PathBuf) -> CollaborationManager {
        connect_manager_as(path, "test-app", TEST_CLUSTER)
    }

    /// Apply every CRDT payload that reached `mgr` into `doc`, the way an
    /// app's own `ui::App` does (`State::load_incremental` over
    /// `CollabEvent::CRDTIncremental`). Returns how many payloads landed.
    ///
    /// Assertions then read the *receiver's own document*, which is the only
    /// thing that answers "did this peer get that document's contents".
    fn absorb(mgr: &mut CollaborationManager, doc: &mut automerge::AutoCommit) -> usize {
        absorb_within(mgr, doc, Duration::from_millis(400))
    }

    /// [`absorb`] with an explicit window — the peer-bridge drain thread runs
    /// on a 100 ms cadence, so a frame that crossed the wire needs longer than
    /// one that was fanned out inline.
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

    /// A document with one field set, plus the incremental delta that
    /// produced it — what an app sends after an edit.
    fn edited(key: &str, value: &str) -> (automerge::AutoCommit, Vec<u8>) {
        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::new();
        doc.put(&automerge::ROOT, key, value).expect("put");
        let delta = doc.save_incremental();
        (doc, delta)
    }

    /// Two people editing two **different** documents in one room must not
    /// merge each other's state.
    ///
    /// Before records both announced the identical `(app, cluster)` pair, so
    /// the broker delivered each edit to the other and every keystroke in one
    /// file landed in the other file's document.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn two_documents_in_one_room_do_not_merge() {
        let socket_path = temp_sock("record-isolation");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let report = ui::record::RecordId::for_file("/work/report.odt");
        let minutes = ui::record::RecordId::for_file("/work/minutes.odt");
        // Same app, same room — only the document differs.
        let mut alice =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &report);
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

        // Bob is editing minutes.odt. Whatever arrives, apply it to his own
        // document and then ask *his* document what it contains.
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

    /// Fold inbound **full-state** frames into `doc` the way the toolkit
    /// does: load the snapshot as its own document and merge it, rather
    /// than assigning over the top. A snapshot that arrives after the
    /// local user has typed must not discard what they typed.
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

    /// A document created here adopts the state of the one it meets.
    ///
    /// This is the shape two people get when they each *start* the same
    /// app rather than opening a document someone shared: two automerge
    /// documents with no history in common. Deltas between them name
    /// changes the receiver has never seen, so the receiving widgets
    /// read nothing — the app looks alive, a peer's caret moves, and the
    /// page stays blank.
    ///
    /// The frame that resolves it is the whole-document snapshot the
    /// toolkit sends when a peer announces it has arrived. This drives
    /// the real inbound path for it: queued exactly as
    /// `ShellNetwork::tick` queues one off gossipsub, picked up by the
    /// broker's drain thread, delivered as `CRDTUpdate`, and routed by
    /// the record the sender stamped.
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
        let mut on_other = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &other,
        );
        wait_for_peers(&handle, 2, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));
        let _ = on_record.poll();
        let _ = on_other.poll();

        // The peer's document: created on the other machine, with
        // content, sharing no history with anything here.
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

        // A document that started empty here now reads what was typed
        // there. This is the assertion the whole path exists for.
        let mut here = automerge::AutoCommit::new();
        assert_eq!(
            absorb_snapshots_within(&mut on_record, &mut here, Duration::from_millis(2000)),
            1,
            "the app on that record receives the snapshot"
        );
        assert_eq!(
            field(&here, "body").as_deref(),
            Some("written on another machine"),
            "the document created here adopted the peer's state"
        );

        // And the record boundary holds for a whole document exactly as
        // it does for a delta — more so, since a document merged into
        // the wrong record cannot be taken back out.
        let mut elsewhere = automerge::AutoCommit::new();
        assert_eq!(
            absorb_snapshots_within(&mut on_other, &mut elsewhere, Duration::from_millis(1000)),
            0,
            "the app on a different record receives nothing"
        );
        assert_eq!(field(&elsewhere, "body"), None);

        drop(on_record);
        drop(on_other);
        drop(handle);
    }

    /// The record boundary holds across hosts, not just within one.
    ///
    /// Gossipsub is a broadcast: a frame reaches every host on the topic, and
    /// which of their apps see it is decided entirely by the route. So the
    /// record has to be *on the wire* — a field of `NeighbourMsg::AppCrdt*` —
    /// and the receiving host has to rebuild it. This drives the real inbound
    /// path: a frame queued as if it had just arrived off gossipsub, picked up
    /// by the broker's drain thread, routed by what the sender stamped.
    /// The two transports must publish through the same thing.
    ///
    /// They did not. The JSON reader loop republished every
    /// `SyncIncremental` over libp2p; the capnp server, which is the
    /// one a sandboxed app dials, held no network handle at all — so
    /// "does a local edit leave this machine" had two answers and the
    /// sandboxed one was no. Asserting a shared publisher here is what
    /// stops that from being re-derived independently a third time.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_json_edit_reaches_the_same_publisher_the_capnp_transport_uses() {
        use crate::shell::collab_outbound::recording::{RecordingSink, Sent};
        use crate::shell::collab_outbound::{PeerPublishSink, PeerPublisher};

        let socket_path = temp_sock("json-shared-publisher");
        let sink = std::sync::Arc::new(RecordingSink::default());
        let publisher = PeerPublisher::new(sink.clone() as Arc<dyn PeerPublishSink>);
        let handle = spawn_with_publisher(socket_path.clone(), None, publisher)
            .expect("broker should bind");

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
        let mut on_theirs =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &theirs);
        let mut on_mine =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &mine);
        wait_for_peers(&handle, 2, Duration::from_millis(500));
        wait_for_announced(&handle, 2, Duration::from_millis(500));
        let _ = on_theirs.poll();
        let _ = on_mine.poll();

        // A frame as it arrives off the wire from another host, stamped with
        // the record it belongs to. Queued exactly as `ShellNetwork::tick`
        // queues a received `AppCrdtIncremental`; the drain thread takes it
        // from here.
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
            absorb_within(&mut on_theirs, &mut theirs_doc, Duration::from_millis(2000)),
            1,
            "the app on that record receives it"
        );
        assert_eq!(
            field(&theirs_doc, "body").as_deref(),
            Some("written on another machine")
        );

        let mut mine_doc = automerge::AutoCommit::new();
        assert_eq!(
            absorb_within(&mut on_mine, &mut mine_doc, Duration::from_millis(1000)),
            0,
            "the app on a different record receives nothing"
        );
        assert_eq!(field(&mine_doc, "body"), None);

        drop(on_theirs);
        drop(on_mine);
        drop(handle);
    }

    /// Sharing one record shares **only** that record.
    ///
    /// The sender holds two documents open — the shape of a reviewer with two
    /// files, or the PDF reader with two documents' markup. The receiver was
    /// given a reference to one of them. It must converge on that one and
    /// never see the other.
    #[test]
    #[serial_test::serial(collab_broker)]
    fn a_peer_given_one_record_receives_only_that_record() {
        let socket_path = temp_sock("record-share-one");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let shared = ui::record::RecordId::for_file("/work/shared.odt");
        let private = ui::record::RecordId::for_file("/work/private.odt");

        // One process, two records, two channels.
        let mut sender_shared =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &shared);
        let mut sender_private = connect_manager_on_record(
            &socket_path,
            "dev.qos.word-processor",
            TEST_CLUSTER,
            &private,
        );
        // The peer the shared record was shared with.
        let mut receiver =
            connect_manager_on_record(&socket_path, "dev.qos.word-processor", TEST_CLUSTER, &shared);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        wait_for_announced(&handle, 3, Duration::from_millis(500));
        let _ = sender_shared.poll();
        let _ = sender_private.poll();
        let _ = receiver.poll();

        let (_, shared_delta) = edited("shared_body", "everyone may read this");
        let (_, private_delta) = edited("private_body", "nobody else may read this");
        sender_shared.send(CollabRequest::SyncIncremental { data: shared_delta });
        sender_private.send(CollabRequest::SyncIncremental { data: private_delta });

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
        // The property the compositor's peer cursors rest on.
        //
        // Presence is widened to cluster scope on the way out, so a
        // subscription that names a room hears every app in it. Two
        // different apps pointing inside one room reach one
        // subscriber, which is what makes a cursor crossing from one
        // to the other a single peer moving rather than a second peer
        // appearing. A third app in a different room must not be
        // heard at all, or "who is here" would leak between rooms.
        let socket_path = temp_sock("room-sub");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let other_room = "3f2b1c00-0000-4000-8000-0000000000ff";
        let heard: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_heard = Arc::clone(&heard);
        let _sub = handle.subscribe_rooms(
            vec![TEST_CLUSTER.to_string()],
            Box::new(move |event, route| {
                if let CollabEvent::RemotePresenceUpdate { payload, .. } = event {
                    if let ui::collaboration::PresencePayload::Pointer { x, .. } = payload {
                        sink_heard
                            .lock()
                            .unwrap()
                            .push((route.cluster_id.clone(), format!("{x}")));
                    }
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
            key: key.clone(),
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
        // Give the third frame the same chance to arrive as the two
        // that should — otherwise "it did not leak" only says "it was
        // slower", and the test would pass on a race it never ran.
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
        // Phase 1 acceptance test: prove N=3 fan-out, not just N=2.
        // Mirrors `ui/tests/real_socket_two_peer.rs` but exercises the
        // production module and a peer count beyond the test broker.
        let socket_path = temp_sock("fanout3");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut peer_a = connect_manager(&socket_path);
        let mut peer_b = connect_manager(&socket_path);
        let mut peer_c = connect_manager(&socket_path);

        wait_for_peers(&handle, 3, Duration::from_millis(500));
        assert_eq!(handle.peer_count(), 3, "broker should see all three peers");
        wait_for_announced(&handle, 3, Duration::from_millis(500));

        // Drain the Hello echoes. The broker swallows them, so this is
        // just a steady-state hygiene poll.
        let _ = peer_a.poll();
        let _ = peer_b.poll();
        let _ = peer_c.poll();

        // --- Assertion 1: SyncIncremental from A fans out to B and C.
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

        // --- Assertion 2: source peer does NOT see its own update.
        let events_a = peer_a.poll();
        assert!(
            !events_a.iter().any(|e| matches!(
                e,
                CollabEvent::CRDTIncremental { data } if data.as_slice() == b"hello-world"
            )),
            "peer A must not see its own SyncIncremental echoed back, got {events_a:?}"
        );

        // --- Assertion 3: presence is attributed to a participant, and
        // the same one on every screen it reaches.
        //
        // It used to be attributed to a *connection* — peer B was the
        // second to connect, so it arrived as "peer-1" — which meant one
        // person was a different stranger in each app they had open and
        // could never resolve to a name. There is no room here to name
        // anyone, so what B is called is this machine's own user id;
        // what is asserted is that it is an identity rather than a slot,
        // and that A and C agree on it.
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

        // Drop B; its reader thread observes EOF and removes it from
        // the table. We then assert a send from A reaches only C.
        drop(peer_b);
        // Give the reader thread a chance to notice EOF and evict.
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
        // The leak this closes: the broker used to write every CRDT
        // payload to every connected socket, so a note-taking app
        // received a terminal's document deltas.
        //
        // Three peers in ONE cluster: two instances of "app-alpha" and
        // one of "app-beta". Alpha-1 sends a delta. Alpha-2 must get
        // it (otherwise this test would pass by simply breaking
        // delivery) and beta must not see it at all.
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

        // The delta reaches the other instance of the same app.
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

        // The other app sees nothing. Polled over the same window the
        // positive half needed, so "nothing" is not just "not yet".
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
        // Same app id, different collaboration clusters: a document
        // shared in one cluster must not leak into the other.
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

    /// The nil room addresses nobody, in either direction.
    ///
    /// It is not the empty string, so without an explicit rule every
    /// workspace still carrying the unminted sentinel — on every
    /// machine at once — would be one enormous shared room. That is the
    /// exact failure that deriving the cluster from a local counter
    /// used to cause, and it must not reappear through the migration
    /// window.
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
        // Fail-closed: a connection that never said Hello has no route,
        // so nothing is addressed to it. It must not fall back to
        // receiving everything.
        //
        // Driven over a raw socket rather than a CollaborationManager,
        // because the manager always auto-Hellos on construct — the
        // silent case only exists at the wire level.
        let socket_path = temp_sock("silent-client");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let mut talker = connect_manager(&socket_path);
        let mut silent = UnixStream::connect(&socket_path).expect("silent client connects");
        silent
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("set read timeout");
        // A second announced peer proves the fan-out is live at all —
        // otherwise "silent got nothing" could just mean the delta
        // never left the sender.
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

        // Nothing was ever written to the silent socket: the read
        // times out rather than yielding a frame header.
        let mut len_buf = [0u8; 4];
        let read = silent.read_exact(&mut len_buf);
        assert!(
            read.is_err(),
            "a client that never announced a route must receive no frame, \
             got a {} byte length prefix",
            u32::from_le_bytes(len_buf),
        );

        // And a frame *from* the silent client is not fanned out
        // either — the broker cannot address it anywhere.
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
    // The tests above address peers by hand. The ones below drive the
    // production chain instead: the argv the compositor builds when the
    // user opens an app, the bwrap argv the launcher builds from it,
    // the env that argv gives the child, and finally what the live
    // broker does or does not deliver to a manager built under that
    // env. Nothing in between is stubbed, so a break anywhere along the
    // chain — a dropped flag, a renamed variable, a lost precedence
    // rule — surfaces here as a delivery failure.
    // -----------------------------------------------------------------

    /// Write a minimal but real package manifest and return the
    /// [`InstalledApp`] the registry reader would have produced for it.
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
        argv.windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].clone())
            .unwrap_or_else(|| panic!("argv is missing {flag}: {argv:?}"))
    }

    /// The environment a child opened into `workspace` actually
    /// receives, derived by walking the real launch chain:
    ///
    /// 1. the compositor renders the `qos-launcher` invocation,
    /// 2. we read back the flags `qos-launcher` parses from it,
    /// 3. the launcher builds the bwrap argv from that manifest and
    ///    splices the workspace in exactly where `run_with` does,
    /// 4. every `--setenv K V` triple in that argv is collected.
    ///
    /// Step 4 is the child's view of the world, which is precisely what
    /// `auto_hello_env` reads.
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
        // The same splice `run_with` performs: in front of the trailing
        // entrypoint token, so bwrap still reads it as a flag.
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

    /// Connect a manager under the env a real launch into `room`
    /// would have given the app.
    ///
    /// `room` is the workspace's durable identity, which is what the
    /// launch chain actually carries — the local `WorkspaceId` never
    /// reaches the sandbox. Tests read it off a real
    /// `Workspace`/`WorkspaceTree` rather than inventing one, so what
    /// they exercise is the value production would have used.
    ///
    /// `instance` goes to `QUICK_APP_ID` — the standalone-dev
    /// per-instance identity, deliberately *different* for every peer
    /// here. Under a launched session it must not influence routing,
    /// and leaving it set and distinct is what proves that.
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
        // Two windows of one app, opened into the same workspace. Their
        // per-instance dev ids differ; that must not matter.
        let mut first =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", workspace.room, "alice");
        let mut second =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", workspace.room, "bob");

        wait_for_announced(&handle, 2, Duration::from_millis(2000));
        let _ = first.poll();
        let _ = second.poll();

        first.send(CollabRequest::SyncIncremental {
            data: b"same-room".to_vec(),
        });

        let received = poll_until(&mut second, 1, Duration::from_millis(1000));
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
        // The half that matters. A workspace is a room: an app opened
        // in workspace 7 must never see the document of the same app
        // opened in workspace 8. Restoring the old
        // broadcast-to-every-socket behaviour makes this fail, which a
        // positive-only test would not.
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

        // Two peers in `here` — the second is the control. Without it,
        // "the other workspace heard nothing" could just mean the delta
        // never left the sender.
        let mut sender =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", here.room, "alice");
        let mut same_workspace =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", here.room, "bob");
        let mut other_workspace =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", elsewhere.room, "carol");

        wait_for_announced(&handle, 3, Duration::from_millis(2000));
        let _ = sender.poll();
        let _ = same_workspace.poll();
        let _ = other_workspace.poll();

        sender.send(CollabRequest::SyncIncremental {
            data: b"workspace-seven-only".to_vec(),
        });

        // Control: the room the sender is in does receive it.
        let heard = poll_until(&mut same_workspace, 1, Duration::from_millis(1000));
        assert_eq!(
            crdt_payloads(&heard),
            vec![b"workspace-seven-only".to_vec()],
            "the peer in the sender's workspace must receive the delta, \
             got {heard:?}"
        );

        // The assertion this whole change exists for. Polled over the
        // same window the control needed, so "nothing" is not "not yet".
        let leaked = poll_until(&mut other_workspace, 1, Duration::from_millis(1000));
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
        // The precedence rule read straight off the env the launcher
        // produces: under a session the package id and the workspace
        // are both present, so they win over the dev-only override.
        let dir = scratch_dir("env");
        let workspace = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(7),
            "here",
        );
        let env = sandbox_env_for_launch(&dir, "dev.qos.notes", workspace.room);
        let lookup = |key: &str| {
            env.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("sandbox env is missing {key}: {env:?}"))
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
        // And it must be the workspace's *durable* identity, not
        // anything derived from the local handle. Pinned against the
        // stored room verbatim: if `collab_cluster_id` ever goes back
        // to deriving from `id`, the two differ and this fails.
        assert_eq!(
            lookup("QOS_WORKSPACE_ID"),
            workspace.room.to_string(),
            "the cluster the app is launched into must be the workspace's \
             stored room id, which is the only value a peer on another \
             machine can also hold"
        );
        // Two workspaces are two rooms, not one — even when their local
        // handles are adjacent counter values.
        assert_ne!(
            crate::shell::state::Workspace::test_default(crate::shell::state::WorkspaceId(7), "a",)
                .collab_cluster_id(),
            crate::shell::state::Workspace::test_default(crate::shell::state::WorkspaceId(8), "b",)
                .collab_cluster_id(),
            "distinct workspaces must have distinct cluster ids"
        );
        // The half a local-id derivation could never give us: two
        // workspaces that share a local handle — "workspace 7" on two
        // different machines — are still two different rooms.
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

    /// The route the broker recorded for the peer it accepted at
    /// `index` — the `(app_id, cluster_id)` pair the app announced on
    /// its `Hello`, derived inside the app from the `QOS_WORKSPACE_ID`
    /// the launch put in its sandbox.
    ///
    /// Read out of the live peer table rather than reconstructed, so a
    /// test that ships this route over the wire ships the value
    /// production would have shipped.
    fn announced_route(handle: &BrokerHandle, index: usize) -> CrdtRoute {
        handle
            .peers
            .snapshot()
            .get(index)
            .and_then(|p| p.route())
            .expect("peer should have announced a route")
    }

    /// One hop of the peer-sync wire, minus libp2p.
    ///
    /// Both ends of the bridge, verbatim. The sending host's
    /// `reader_loop` stamps the sender's announced route onto the frame
    /// and hands it to `ShellNetwork::publish_app_crdt_incremental`;
    /// the receiving host's drain thread rebuilds
    /// `CrdtRoute::app(app_id, cluster_id)` from the frame and calls
    /// `fanout_to_route`. Everything between the two is transport, and
    /// gossipsub is a broadcast — so a frame reaches *every* host
    /// subscribed to the topic, and which of their apps see it is
    /// decided entirely by the route. That is the decision under test.
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
        // "The workspace I had yesterday is the workspace I have
        // today." A room id that changed on restart would quietly
        // strand every peer who had been invited to it, and quietly
        // orphan the app windows the user reopens into it.
        let dir = scratch_dir("restart");
        let state_dir = tempfile::tempdir().expect("tempdir");
        let state_path = state_dir.path().join("state.json");
        let socket_path = temp_sock("restart");
        let handle = spawn(socket_path.clone()).expect("broker should bind");

        let ws_id = crate::shell::state::WorkspaceId(7);
        let other_id = crate::shell::state::WorkspaceId(8);

        // --- First run of the compositor. ---------------------------
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

        // An app the user opened before the restart, through the real
        // launch chain.
        let mut before =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", room_before, "alice");

        // --- The process dies; a new one loads the same state file. --
        let (mut second_run, _) = crate::shell::persist::load_or_default(&state_path);
        second_run.persist_path = Some(state_path.clone());
        // The boot-time migration must find nothing to do: these
        // workspaces already have identities, and re-minting them is
        // exactly the bug this test exists to catch.
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

        // An app the user opens after the restart, through the same
        // launch chain. Nothing here is told about `before`.
        let mut after =
            connect_as_launched_app(&socket_path, &dir, "dev.qos.notes", room_after, "bob");
        // Control: an app in the *other* workspace, so "the delta
        // arrived" cannot be the broker having lost its routing.
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

        wait_for_announced(&handle, 3, Duration::from_millis(2000));
        let _ = before.poll();
        let _ = after.poll();
        let _ = elsewhere.poll();

        after.send(CollabRequest::SyncIncremental {
            data: b"still-the-same-room".to_vec(),
        });

        let heard = poll_until(&mut before, 1, Duration::from_millis(1000));
        assert_eq!(
            crdt_payloads(&heard),
            vec![b"still-the-same-room".to_vec()],
            "an app launched after the restart must land in the same room as \
             one launched before it, so their edits still reach each other. \
             Got {heard:?}"
        );

        let leaked = poll_until(&mut elsewhere, 1, Duration::from_millis(1000));
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
        // The cross-machine claim. Three hosts, three brokers, three
        // independently-built workspace trees — and, deliberately, the
        // same local workspace id on all three, because that number
        // used to be what decided the room and it must now decide
        // nothing.
        //
        // Only the second host accepts the first's invite. One frame
        // goes onto the (broadcast) wire; the assertions are about
        // which apps it reaches.
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

        // The guest's app as it stood *before* the invite. Same broker,
        // same package, same everything except the room — which is what
        // makes the post-invite result attributable to the invite.
        let mut guest_before_invite = connect_as_launched_app(
            &guest_sock,
            &dir,
            "dev.qos.notes",
            guest_room_before,
            "bob-before",
        );

        // --- The invite. Production path on both ends. ---------------
        let owner_key = libp2p::identity::Keypair::generate_ed25519();
        let token = crate::network::invite::make_invite_token(
            &owner_key,
            host_tree
                .workspaces
                .iter()
                .find(|w| w.id == local_id)
                .expect("host workspace"),
            "/ip4/127.0.0.1/tcp/4001",
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

        // --- Each host launches the app into its own workspace. ------
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

        wait_for_announced(&host_broker, 1, Duration::from_millis(2000));
        wait_for_announced(&guest_broker, 2, Duration::from_millis(2000));
        wait_for_announced(&stranger_broker, 1, Duration::from_millis(2000));
        let _ = host_app.poll();
        let _ = guest_app.poll();
        let _ = guest_before_invite.poll();
        let _ = stranger_app.poll();

        // --- One frame, broadcast to both other hosts. ---------------
        let wire_route = announced_route(&host_broker, 0);
        let payload = b"from-the-host".to_vec();
        deliver_over_the_wire(&guest_broker, &wire_route, &payload);
        deliver_over_the_wire(&stranger_broker, &wire_route, &payload);

        let guest_heard = poll_until(&mut guest_app, 1, Duration::from_millis(1000));
        assert_eq!(
            crdt_payloads(&guest_heard),
            vec![payload.clone()],
            "an app on a host that accepted the invite must receive the \
             inviter's delta — that is the whole point of the invite \
             carrying the room. Got {guest_heard:?}"
        );

        // Both negatives are polled over the same window the positive
        // needed, so "nothing" is not "not yet".
        let stranger_heard = poll_until(&mut stranger_app, 1, Duration::from_millis(1000));
        assert!(
            crdt_payloads(&stranger_heard).is_empty(),
            "an app on a host that was never invited must receive nothing \
             from the same broadcast frame; it received {:?}",
            crdt_payloads(&stranger_heard),
        );
        let pre_invite_heard = poll_until(&mut guest_before_invite, 1, Duration::from_millis(1000));
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
