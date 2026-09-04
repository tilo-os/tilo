//! Typed collaboration broker and transport-neutral routing core.
//!
//! Apps connect through the Cap'n Proto service. This module owns durable
//! policy state, peer-network draining, routing, and in-process subscribers;
//! the RPC implementation lives in `collab_broker_capnp`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use collab_types::{
    ArtifactId, ArtifactKind, ChannelState, DocumentMutation, OpClass, RedactedMarker,
    ValidationError, document_artifact_id, document_attestation_id, validate_admitted_inbound_op,
};
use ui::collaboration::CollabEvent;

use crate::shell::collab_broker_capnp::CapnpBrokerState;
pub use crate::shell::collab_core::{
    AuditEntry, BrokerChannels, RedactionRecord, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::PeerPublisher;
use crate::shell::network::{
    InboundAppPresence, InboundDocument, InboundScrollbackRequest, InboundScrollbackResponse,
    ShellNetwork,
};

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
    /// Collaboration cluster id, carried in the typed `channelId`.
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
    #[must_use]
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

/// Callback an in-process fanout subscriber registers to receive
/// delivered events. Aliased because the raw `Box<dyn Fn(...)>` shape
/// recurs across [`RoomSubscription`], [`FanoutState::subscribe_rooms`]
/// and [`BrokerHandle::subscribe_rooms`], and the last of those is public
/// API other modules call.
pub type FanoutSink = Box<dyn Fn(&CollabEvent, &CrdtRoute) + Send + Sync>;

/// Shared fanout registry for typed RPC subscribers and in-process shell consumers.
pub struct FanoutState {
    peer_id_counter: AtomicU64,
    capnp: Mutex<Option<Arc<CapnpBrokerState>>>,
    local_subs: Mutex<Vec<Arc<RoomSubscription>>>,
}

/// A subscriber inside the shell process, addressed by room — how the
/// compositor reads presence its apps publish. Delivery goes through the
/// same [`CrdtRoute::delivers_to`] check as a typed subscriber, against
/// `CrdtRoute::cluster(room)`; since presence widens to cluster scope on
/// the way out, two apps in one room reach one subscription, so a cursor
/// crossing between them is one peer moving, not a second appearing.
/// Rooms are mutable — workspaces are created, and join a peer's room
/// when an invite is accepted.
pub struct RoomSubscription {
    rooms: Mutex<Vec<String>>,
    sink: FanoutSink,
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

impl FanoutState {
    pub(super) fn new() -> Self {
        Self {
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
        sink: FanoutSink,
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

    /// Mint the next typed connection id.
    pub(crate) fn next_peer_id(&self) -> u64 {
        self.peer_id_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// How many typed connections this broker has ever accepted. Read as a
    /// generation, not a count: it only rises, so
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

    /// Deliver `event` to every typed or in-process subscriber addressed by
    /// `route`, skipping the originating typed peer id.
    pub(crate) fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if route.cluster_id.is_empty() {
            tracing::debug!(
                "collab_broker: dropping event with an unaddressed route \
                 (sender never announced a cluster)"
            );
            return;
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
}

/// Owns the typed broker and optional peer-network drain.
pub struct BrokerHandle {
    capnp_socket_path: PathBuf,
    /// Spawned only when there's a `ShellNetwork` to pump.
    drain_thread: Option<JoinHandle<()>>,
    drain_quit: Arc<AtomicBool>,
    /// `None` when the capnp interface is not bound; runs a tokio
    /// `LocalSet` driving capnp-rpc futures.
    capnp_thread: Option<JoinHandle<()>>,
    capnp_quit: Arc<AtomicBool>,
    pub(crate) fanout: Arc<FanoutState>,
    channels: Arc<Mutex<BrokerChannels>>,
    peer_departures: Option<Arc<crate::shell::network::PeerDepartureHistory>>,
}

/// Cloneable, thread-safe slice of the broker used by shell chrome. Keeping
/// the join handles out of the closure avoids extending broker lifetime or
/// creating an ownership cycle with the root widget.
#[derive(Clone)]
pub struct SessionFollowDispatcher {
    fanout: Arc<FanoutState>,
}

impl SessionFollowDispatcher {
    pub fn set(&self, room: &str, peer_id: Option<String>) {
        self.fanout.fanout_to_route(
            &CollabEvent::FollowChanged { peer_id },
            u64::MAX,
            &CrdtRoute::cluster(room.to_string()),
        );
    }
}

/// Cloneable, transport-owning facade used by shell search providers. It
/// keeps only the shared action directory alive, never the broker threads or
/// socket owner.
#[derive(Clone)]
pub struct AppActionBroker {
    state: Arc<CapnpBrokerState>,
}

impl AppActionBroker {
    pub fn surfaces(&self) -> Vec<crate::shell::collab_broker_capnp::PublishedSurface> {
        self.state.published_surfaces()
    }

    /// Invoke one published application action.
    ///
    /// # Errors
    ///
    /// Returns an error if the surface or action is unavailable or dispatch
    /// to its application connection fails.
    pub fn invoke(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<
        std::sync::mpsc::Receiver<crate::shell::collab_broker_capnp::InvocationCompletion>,
        String,
    > {
        self.state
            .invoke(peer_id, resource, action, argument, actor)
    }

    /// Start a published action and return its lifecycle job.
    ///
    /// # Errors
    ///
    /// Returns an error when the action is unavailable or cannot be dispatched.
    pub fn start(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<crate::shell::collab_broker_capnp::InvocationJob, String> {
        self.state
            .start_invocation(peer_id, resource, action, argument, actor)
    }

    pub fn status(&self, id: u64) -> Option<qos_actions::InvocationUpdate> {
        self.state.invocation_status(id)
    }

    pub fn statuses(&self, actor: &str) -> Vec<(u64, qos_actions::InvocationUpdate)> {
        self.state.invocation_statuses(actor)
    }

    /// Request cooperative cancellation for an owned invocation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown, foreign, or non-cancellable invocation.
    pub fn cancel(&self, id: u64, actor: &str) -> Result<(), String> {
        self.state.request_cancellation(id, actor)
    }

    pub fn can_cancel(&self, id: u64, actor: &str) -> bool {
        self.state.can_cancel(id, actor)
    }

    /// True only while the named admitted agent has a live task subscription.
    pub fn agent_is_live_task_capable(&self, room: &str, agent: &str) -> bool {
        self.state.agent_is_live_task_capable(room, agent)
    }

    /// Dispatch a task to its assigned live agent. There is intentionally no
    /// offline queue or application launch fallback in this boundary.
    pub fn dispatch_task(&self, room: &str, task_id: &str) -> Result<u64, String> {
        self.state.dispatch_workspace_task(room, task_id)
    }

    #[allow(clippy::too_many_arguments)]
    /// Prepare a one-use confirmation token for the exact invocation inputs.
    ///
    /// # Errors
    ///
    /// Returns an error when the action disappeared or confirmation evidence is invalid.
    pub fn prepare_confirmation(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        phrase: Option<&str>,
    ) -> Result<String, String> {
        self.state
            .prepare_confirmation(peer_id, resource, action, argument, actor, phrase)
    }

    #[allow(clippy::too_many_arguments)]
    /// Start a published action using a previously prepared confirmation token.
    ///
    /// # Errors
    ///
    /// Returns an error when the action or token is unavailable or invalid.
    pub fn start_confirmed(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
        token: &str,
    ) -> Result<crate::shell::collab_broker_capnp::InvocationJob, String> {
        self.state.start_invocation_confirmed(
            peer_id,
            resource,
            action,
            argument,
            actor,
            Some(token),
        )
    }
}

impl BrokerHandle {
    /// Cloneable handle for the live resource/action search provider.
    pub fn app_action_broker(&self) -> Option<AppActionBroker> {
        self.fanout
            .capnp_state()
            .map(|state| AppActionBroker { state })
    }

    /// Number of typed connections accepted since this broker started.
    pub fn peer_count(&self) -> usize {
        // Saturates rather than truncates on a 32-bit target — this is a
        // connection count, not an index, so clamping to `usize::MAX`
        // loses nothing a real desktop session would ever reach.
        usize::try_from(self.fanout.connections_seen()).unwrap_or(usize::MAX)
    }

    /// Every typed connection joins a channel as part of setup.
    pub fn announced_peer_count(&self) -> usize {
        self.peer_count()
    }

    /// Fan a `CollabEvent` to the peers `route` addresses. `except`
    /// skips one peer id, for the Iroh bridge. No "reach everyone"
    /// form exists — see [`FanoutState::fanout_to_route`].
    pub fn fanout_event(&self, event: &CollabEvent, except: Option<u64>, route: &CrdtRoute) {
        self.fanout
            .fanout_to_route(event, except.unwrap_or(u64::MAX), route);
    }

    /// Bind every local app in `room` to one participant's published view,
    /// or release the binding. This is session-local and never enters the
    /// peer publisher; each app already receives that participant's normal
    /// view-presence frames and applies them through `ui::App::follow`.
    pub fn set_follow_target(&self, room: &str, peer_id: Option<String>) {
        self.fanout_event(
            &CollabEvent::FollowChanged { peer_id },
            None,
            &CrdtRoute::cluster(room.to_string()),
        );
    }

    #[must_use]
    pub fn follow_dispatcher(&self) -> SessionFollowDispatcher {
        SessionFollowDispatcher {
            fanout: Arc::clone(&self.fanout),
        }
    }

    /// Subscribe this process to the rooms it hosts. The sink runs on
    /// the reader thread that decoded the frame: keep it short.
    pub fn subscribe_rooms(&self, rooms: Vec<String>, sink: FanoutSink) -> Arc<RoomSubscription> {
        self.fanout.subscribe_rooms(rooms, sink)
    }

    /// The bound Cap'n Proto socket.
    pub fn capnp_socket(&self) -> &std::path::Path {
        &self.capnp_socket_path
    }

    /// Shared handle to per-channel state + audit log. Tests use this
    /// to install a [`ChannelState`] before sending signed envelopes.
    pub fn channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.channels)
    }

    /// Snapshot the resource/action surfaces live applications published to
    /// this session. The values are inert; invoking is a separate operation.
    pub fn published_action_surfaces(
        &self,
    ) -> Vec<crate::shell::collab_broker_capnp::PublishedSurface> {
        self.fanout
            .capnp_state()
            .map_or_else(Vec::new, |state| state.published_surfaces())
    }

    /// Direct one invocation to the exact application connection that
    /// published the addressed operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the action broker, surface, or action is
    /// unavailable or dispatch to its application connection fails.
    pub fn invoke_published_action(
        &self,
        peer_id: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        actor: &str,
    ) -> Result<
        std::sync::mpsc::Receiver<crate::shell::collab_broker_capnp::InvocationCompletion>,
        String,
    > {
        self.fanout
            .capnp_state()
            .ok_or_else(|| "the application action broker is unavailable".to_string())?
            .invoke(peer_id, resource, action, argument, actor)
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

    /// Recent authenticated peer departures observed by the attached network.
    /// Empty when this broker was started without peer sync.
    pub fn peer_departures(&self) -> Vec<crate::shell::network::PeerDeparture> {
        self.peer_departures
            .as_ref()
            .map_or_else(Vec::new, |history| history.snapshot())
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.drain_quit.store(true, Ordering::Relaxed);
        self.capnp_quit.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.capnp_socket_path);
        let _ = self.capnp_thread.take();
        if let Some(t) = self.drain_thread.take() {
            let _ = t.join();
        }
    }
}

/// Bind the typed Cap'n Proto service.
///
/// # Errors
///
/// Returns an error if the Cap'n Proto socket at `capnp_socket_path`
/// cannot be bound (e.g. a stale socket file blocks it, or the parent
/// directory is not writable).
pub fn spawn_native(
    capnp_socket_path: PathBuf,
    peer_sync: &Option<Arc<ShellNetwork>>,
    membership: crate::shell::state::WorkspaceTreeHandle,
) -> std::io::Result<BrokerHandle> {
    let publisher = PeerPublisher::from_network(peer_sync.clone());
    spawn_inner(
        capnp_socket_path,
        peer_sync.as_ref(),
        &publisher,
        Some(membership),
        None,
    )
}

/// Spawn the production broker pipeline with an explicit enrolled-device set.
/// Intended for integration harnesses that do not have the desktop identity
/// registry on disk; certificate, proof, membership, role, and op validation
/// remain identical to production.
#[doc(hidden)]
pub fn spawn_native_harness(
    capnp_socket_path: PathBuf,
    membership: crate::shell::state::WorkspaceTreeHandle,
    trusted_devices: Vec<[u8; 32]>,
) -> std::io::Result<BrokerHandle> {
    let publisher = PeerPublisher::disabled();
    spawn_inner(
        capnp_socket_path,
        None,
        &publisher,
        Some(membership),
        Some(Arc::new(trusted_devices)),
    )
}

/// Inbound queues + shared fanout state the drain thread polls each
/// cycle. Grouped into one struct so the thread closure in
/// [`spawn_inner`] doesn't have to carry the whole per-iteration match
/// itself — see the `drain_*` methods below, extracted from what used
/// to be one large loop body.
struct DrainQueues {
    presence_inbound: Arc<Mutex<Vec<InboundAppPresence>>>,
    documents: Arc<Mutex<Vec<InboundDocument>>>,
    sb_req_inbound: Arc<Mutex<Vec<InboundScrollbackRequest>>>,
    sb_resp_inbound: Arc<Mutex<Vec<InboundScrollbackResponse>>>,
    table: Arc<FanoutState>,
    channels: Arc<Mutex<BrokerChannels>>,
    net: Arc<ShellNetwork>,
    pending: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>>,
}

impl DrainQueues {
    /// Run one pass over every inbound queue, in the same order the
    /// original inline loop drained them. Returns `false` when a
    /// queue's lock is poisoned — the signal the caller uses to stop
    /// the thread, matching the original inline `Err(_) => break` on
    /// each queue; short-circuits the same way that `break` did, so a
    /// poisoned queue skips whatever would have run after it this
    /// cycle too.
    fn drain_once(
        &self,
        labels: &mut HashMap<String, (Instant, PeerLabels)>,
        announced: &mut HashMap<(String, u64), (String, u64)>,
    ) -> bool {
        self.drain_presence(labels, announced)
            && self.drain_documents(labels, announced)
            && self.drain_scrollback_requests()
            && self.drain_scrollback_responses()
    }

    /// Drain inbound presence, attributed to its sender. Cluster scope,
    /// matching a local presence frame (`CrdtRoute::to_cluster_scope`)
    /// — narrowing to a record would make a peer visible locally but
    /// invisible remotely.
    fn drain_presence(
        &self,
        labels: &mut HashMap<String, (Instant, PeerLabels)>,
        announced: &mut HashMap<(String, u64), (String, u64)>,
    ) -> bool {
        let drained_presence: Vec<_> = {
            let Ok(mut buf) = self.presence_inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        for entry in drained_presence {
            let decoded = crate::shell::collab_presence::from_peer(entry.presence);
            let Some((key, payload)) = decoded else {
                continue;
            };
            let user_id = remote_participant_id(
                &entry.cluster_id,
                entry.peer,
                &|c| self.net.peer_labels(c),
                &self.table,
                labels,
                announced,
            );
            let route = CrdtRoute::cluster(entry.cluster_id.clone());
            let event = CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            };
            self.table.fanout_to_route(&event, u64::MAX, &route);
        }
        true
    }

    /// Validate inbound signed ops against local channel state, record
    /// on success, fan to local apps.
    fn drain_documents(
        &self,
        labels: &mut HashMap<String, (Instant, PeerLabels)>,
        announced: &mut HashMap<(String, u64), (String, u64)>,
    ) -> bool {
        let drained_signed: Vec<_> = {
            let Ok(mut buf) = self.documents.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        for entry in drained_signed {
            let envelope = *entry.envelope;
            let artifact = envelope.artifact;
            let route = CrdtRoute::record(&entry.app_id, &entry.cluster_id, &entry.record_id);
            let mutation = validate_document_route(&route, &envelope);
            let validation = if let Err(reason) = &mutation {
                Err(reason.clone())
            } else {
                let Ok(mut ch) = self.channels.lock() else {
                    return false;
                };
                let state = ch.state_for(artifact);
                state.artifacts.insert(artifact, ArtifactKind::Note);
                state.artifact_apps.insert(artifact, entry.app_id.clone());
                validate_admitted_inbound_op(
                    &envelope,
                    entry.role,
                    state,
                    std::time::SystemTime::now(),
                    Duration::from_secs(300),
                )
            };
            match (mutation, validation) {
                (Ok(_), Ok(())) => {
                    self.accept_signed_op(envelope, entry.peer, &route, labels, announced);
                }
                (_, Err(ValidationError::OpAlreadyRedacted { target })) => {
                    self.record_already_redacted(envelope, artifact, target);
                }
                (_, Err(reason)) => {
                    tracing::warn!("collab_broker (net): dropping invalid signed op: {reason}",);
                    if let Ok(mut ch) = self.channels.lock() {
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
                (Err(_), Ok(())) => return false,
            }
        }
        true
    }

    /// Record a signed op that passed validation and fan it to local
    /// apps — the `Ok(())` arm of [`Self::drain_documents`]'s per-entry
    /// match, split out to keep that match's arms short.
    fn accept_signed_op(
        &self,
        envelope: collab_types::SignedOp,
        peer: u64,
        route: &CrdtRoute,
        labels: &mut HashMap<String, (Instant, PeerLabels)>,
        announced: &mut HashMap<(String, u64), (String, u64)>,
    ) {
        let artifact = envelope.artifact;
        if let Ok(mut ch) = self.channels.lock() {
            let state = ch.state_for(artifact);
            state.record_validated_op(envelope.clone());
            if envelope.op_class == OpClass::Compaction
                && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
            {
                let _ = state.apply_compaction(&cp);
            }
        }
        if envelope.op_class == OpClass::Redact {
            apply_redaction_from_envelope(&envelope, artifact, &self.channels);
        }
        // Same id and room lookup the peer's
        // presence arrives under, so one
        // person reads as one participant.
        let from_user_id = remote_participant_id(
            &route.cluster_id,
            peer,
            &|c| self.net.peer_labels(c),
            &self.table,
            labels,
            announced,
        );
        let event = CollabEvent::Document {
            envelope: Box::new(envelope),
            from_user_id,
        };
        // An unaddressed route is dropped rather than shown to every app.
        self.table.fanout_to_route(&event, u64::MAX, route);
    }

    /// Late-arriving op via Iroh, already redacted: don't fan, push a
    /// marker entry so the seq stream stays gapless — the
    /// `OpAlreadyRedacted` arm of [`Self::drain_signed`]'s per-entry
    /// match, split out to keep that match's arms short.
    fn record_already_redacted(
        &self,
        envelope: collab_types::SignedOp,
        artifact: ArtifactId,
        target: collab_types::OpId,
    ) {
        tracing::warn!(
            artifact = ?artifact,
            target = ?target,
            "collab_broker (net): late-arriving op already redacted",
        );
        if let Ok(mut ch) = self.channels.lock() {
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
                    rejection: Some(ValidationError::OpAlreadyRedacted { target }),
                    at: std::time::SystemTime::now(),
                },
            );
        }
    }

    /// Drain inbound scrollback requests; publish a response only if
    /// we have matching ops, so the requester isn't spammed by every
    /// peer's "I don't have it either".
    fn drain_scrollback_requests(&self) -> bool {
        let drained_req: Vec<_> = {
            let Ok(mut buf) = self.sb_req_inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        for entry in drained_req {
            let (ops, _exhausted) = {
                let Ok(mut ch) = self.channels.lock() else {
                    continue;
                };
                let state = ch.state_for(entry.artifact);
                state.scrollback_for(entry.artifact, entry.before_seq, entry.max)
            };
            if !ops.is_empty() {
                let boxed: Vec<Box<collab_types::SignedOp>> =
                    ops.into_iter().map(Box::new).collect();
                self.net.publish_scrollback_response(entry.artifact, boxed);
            }
        }
        true
    }

    /// Drain inbound scrollback responses, deliver to the route(s)
    /// recorded in `pending`; an unrequested response is dropped, not
    /// broadcast.
    fn drain_scrollback_responses(&self) -> bool {
        let drained_resp: Vec<_> = {
            let Ok(mut buf) = self.sb_resp_inbound.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        for entry in drained_resp {
            let waiting = self
                .pending
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
                ops: entry.ops.into_iter().map(Box::new).collect(),
                exhausted: true,
            };
            for route in waiting {
                self.table.fanout_to_route(&event, u64::MAX, &route);
            }
        }
        true
    }
}

fn validate_document_route(
    route: &CrdtRoute,
    envelope: &collab_types::SignedOp,
) -> Result<DocumentMutation, ValidationError> {
    if envelope.artifact != document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id)
        || envelope.attestation_id
            != document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id)
        || envelope.encryption.is_some()
    {
        return Err(ValidationError::DocumentRouteMismatch);
    }
    DocumentMutation::decode(&envelope.payload).map_err(|_| ValidationError::DocumentRouteMismatch)
}

fn spawn_inner(
    capnp_socket_path: PathBuf,
    peer_sync: Option<&Arc<ShellNetwork>>,
    publisher: &Arc<PeerPublisher>,
    membership: Option<crate::shell::state::WorkspaceTreeHandle>,
    trusted_devices: Option<Arc<Vec<[u8; 32]>>>,
) -> std::io::Result<BrokerHandle> {
    let table = Arc::new(FanoutState::new());
    let channels: Arc<Mutex<BrokerChannels>> = Arc::new(Mutex::new(BrokerChannels::default()));
    // Who is waiting on a cooperative scrollback answer, keyed by
    // artifact — a `ScrollbackResponse` off gossip carries an
    // artifact id and nothing else, so without this the drain thread
    // would have to broadcast the batch to every local app.
    let pending_scrollback: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Inbound-drain: when peer-sync is on, drain signed documents, presence,
    // and cooperative scrollback requests/responses every 100ms.
    // Every remote entry is routed like a local one, by the `(app_id,
    // cluster_id)` the publishing host stamped on the wire.
    let drain_quit = Arc::new(AtomicBool::new(false));
    let drain_thread = peer_sync.and_then(|net| {
        let queues = DrainQueues {
            presence_inbound: net.inbound_app_presence_handle(),
            documents: net.inbound_documents_handle(),
            sb_req_inbound: net.inbound_scrollback_request_handle(),
            sb_resp_inbound: net.inbound_scrollback_response_handle(),
            table: Arc::clone(&table),
            channels: Arc::clone(&channels),
            net: Arc::clone(net),
            pending: Arc::clone(&pending_scrollback),
        };
        let quit = Arc::clone(&drain_quit);
        std::thread::Builder::new()
            .name("qos-collab-broker-drain".into())
            .spawn(move || {
                // Name already announced per (peer, cluster) — said
                // again only when it changes, not on every
                // pointer-rate presence frame.
                let mut announced: HashMap<(String, u64), (String, u64)> = HashMap::new();
                // What the room last said, on a re-ask cadence
                // rather than every frame — the lookup takes the
                // replica's lock and parses every peer id it holds.
                let mut labels: HashMap<String, (Instant, PeerLabels)> = HashMap::new();
                while !quit.load(Ordering::Relaxed) {
                    if !queues.drain_once(&mut labels, &mut announced) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            })
            .ok()
    });

    // Typed RPC interface, attached to the shared fanout state.
    let capnp_quit = Arc::new(AtomicBool::new(false));
    let capnp_thread = crate::shell::collab_broker_capnp::spawn(
        capnp_socket_path.clone(),
        &table,
        Arc::clone(&channels),
        Arc::clone(publisher),
        membership,
        trusted_devices,
        peer_sync.cloned(),
        Arc::clone(&capnp_quit),
    )?;

    Ok(BrokerHandle {
        capnp_socket_path,
        drain_thread,
        drain_quit,
        capnp_thread: Some(capnp_thread),
        capnp_quit,
        fanout: table,
        channels,
        peer_departures: peer_sync.map(|net| net.peer_departure_history()),
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
    table: &FanoutState,
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
        None if now == id => None,
        _ => Some(now.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A digest-shaped peer id, as presence frames carry it.
    const PEER: &str = "peer@5537458103208628978";

    #[test]
    fn document_routes_stay_with_their_app_and_record() {
        let document = CrdtRoute::record("dev.qos.notes", "room-1", "note-1");

        assert!(document.delivers_to(&document));
        assert!(document.delivers_to(&CrdtRoute::cluster("room-1")));
        assert!(!document.delivers_to(&CrdtRoute::record("dev.qos.files", "room-1", "note-1",)));
        assert!(!document.delivers_to(&CrdtRoute::record("dev.qos.notes", "room-1", "note-2",)));
        assert!(!document.delivers_to(&CrdtRoute::record("dev.qos.notes", "room-2", "note-1",)));
    }

    #[test]
    fn remote_document_route_preserves_signed_kind_and_rejects_relabeling() {
        let route = CrdtRoute::record("dev.qos.notes", "room-1", "record-1");
        let (signing, author) = collab_types::generate_keypair();
        let mut envelope = collab_types::SignedOp::new_unsigned(
            DocumentMutation::Snapshot(b"whole document".to_vec())
                .encode()
                .expect("typed snapshot"),
            document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
            author,
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            OpClass::Splice,
            std::time::SystemTime::now(),
            1,
        );
        collab_types::sign_op(&mut envelope, &signing).expect("signed snapshot");

        assert!(matches!(
            validate_document_route(&route, &envelope),
            Ok(DocumentMutation::Snapshot(data)) if data == b"whole document"
        ));
        assert_eq!(
            validate_document_route(
                &CrdtRoute::record("dev.qos.notes", "room-1", "record-2"),
                &envelope,
            ),
            Err(ValidationError::DocumentRouteMismatch),
            "an envelope signed for one record cannot be replayed as another",
        );
        envelope.payload = b"untyped bytes".to_vec();
        assert_eq!(
            validate_document_route(&route, &envelope),
            Err(ValidationError::DocumentRouteMismatch),
        );
    }

    #[test]
    fn a_named_peer_is_announced_and_then_left_alone() {
        assert_eq!(
            super::roster_announcement(None, Some("bob"), PEER),
            Some("bob".to_string()),
        );
        assert_eq!(
            super::roster_announcement(Some("bob"), Some("bob"), PEER),
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
            super::roster_announcement(Some("bob"), None, PEER),
            Some(PEER.to_string()),
        );
        assert_eq!(super::roster_announcement(Some(PEER), None, PEER), None);
    }

    #[test]
    fn a_remote_peer_is_one_id_across_presence_and_deltas() {
        const HANDLE: u64 = 721_275_257_761_426_039;

        let table = Arc::new(super::FanoutState::new());
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

        let room = |_c: &str| {
            let mut m = PeerLabels::new();
            m.insert(HANDLE, "bob".to_string());
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
            vec![(format!("peer@{HANDLE}"), "bob".to_string())],
            "the room's name is announced exactly once, against the id \
             both frames arrived under",
        );
    }

    #[test]
    fn follow_dispatcher_is_local_and_room_scoped() {
        let table = Arc::new(super::FanoutState::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let _sub = table.subscribe_rooms(
            vec!["room-1".to_string()],
            Box::new(move |event, _route| {
                if let CollabEvent::FollowChanged { peer_id } = event
                    && let Ok(mut rows) = sink_seen.lock()
                {
                    rows.push(peer_id.clone());
                }
            }),
        );
        let dispatcher = super::SessionFollowDispatcher {
            fanout: Arc::clone(&table),
        };
        dispatcher.set("room-2", Some("peer@wrong".into()));
        dispatcher.set("room-1", Some("peer@7".into()));
        dispatcher.set("room-1", None);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[Some("peer@7".to_string()), None]
        );
    }

    /// An app that starts after the name was said still learns it —
    /// "changes" is measured against what was said, not against who heard it.
    #[test]
    fn a_name_already_announced_is_said_again_for_an_app_that_missed_it() {
        const HANDLE: u64 = 4_242_424_242;

        let table = Arc::new(super::FanoutState::new());
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

        let room = |_c: &str| {
            let mut m = PeerLabels::new();
            m.insert(HANDLE, "bob".to_string());
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
            vec!["bob".to_string()],
            "a name that has not changed is not re-broadcast per frame",
        );

        let _ = table.next_peer_id();
        say();
        assert_eq!(
            heard.lock().unwrap().clone(),
            vec!["bob".to_string(), "bob".to_string()],
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
}
