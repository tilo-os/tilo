//! Typed collaboration broker and transport-neutral routing core.
//!
//! Apps connect through the Cap'n Proto service. This module owns durable
//! policy state, peer-network draining, routing, and in-process subscribers;
//! the RPC implementation lives in `collab_broker_capnp`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use collab_types::ArtifactId;
#[cfg(test)]
use collab_types::{
    DocumentMutation, OpKind, ValidationError, document_artifact_id, document_attestation_id,
};
use runtime_collab::event::CollabEvent;

pub use crate::shell::collab_core::{
    AuditEntry, BrokerChannels, RedactionRecord, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::PeerPublisher;
use crate::shell::network::{
    InboundAppPresence, InboundDocument, InboundScrollbackRequest, InboundScrollbackResponse,
    ShellNetwork,
};
#[cfg(test)]
use tilo_action_broker::collab_inbound::validate_route as validate_document_route;

mod personal_session;
pub(crate) use personal_session::{
    NativePersonalSession, NativePersonalSource, RetainedProofRecipient,
};

pub use crate::shell::peer_presence::PeerLabels;

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

pub use tilo_action_broker::route::CrdtRoute;

pub use tilo_action_broker::collab_broker::{
    AppActionBroker, FanoutSink, FanoutState, RoomSubscription, SessionFollowDispatcher,
};

pub struct BrokerHandle {
    inner: tilo_action_broker::collab_broker::BrokerHandle,
    local_session: crate::shell::collab_broker_capnp::LocalSessionAuthority,
    _personal_source: Option<Arc<NativePersonalSource>>,
    native_media: Option<super::native_media::NativeMediaService>,
    tracker: Option<Arc<super::broker_services::NativeTrackerRuns>>,
    realtime_fabric: Option<crate::shell::collab_broker_capnp::RealtimeFabricAppBridge>,
    /// Spawned only when there's a `ShellNetwork` to pump.
    drain_thread: Option<JoinHandle<()>>,
    drain_quit: Arc<AtomicBool>,
    drain_wake: Option<std::sync::Arc<crate::idle_wake::DrainPark>>,
    /// End-to-end encryption of this device's collaboration channels.
    channel_encryption: Option<crate::shell::channel_encryption::ChannelKeyAccess>,
    peer_departures: Option<Arc<crate::shell::network::PeerDepartureHistory>>,
}

impl std::ops::Deref for BrokerHandle {
    type Target = tilo_action_broker::collab_broker::BrokerHandle;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl BrokerHandle {
    /// The rooms' trackers, for following an issue's run in the shell.
    pub(crate) fn tracker(&self) -> Option<Arc<super::broker_services::NativeTrackerRuns>> {
        self.tracker.clone()
    }

    pub(crate) fn native_media_service(&self) -> Option<super::native_media::NativeMediaService> {
        self.native_media.clone()
    }

    /// This device's channel encryption service, for the identity service
    /// that hands application endpoints their content keys.
    pub fn channel_encryption(&self) -> Option<crate::shell::channel_encryption::ChannelKeyAccess> {
        self.channel_encryption.clone()
    }

    /// The channel keys this session's identity service hands an app, for
    /// an app connected with an explicit endpoint identity
    /// ([`ui::collaboration::CertifiedEndpointIdentity::with_channel_keys`]):
    /// what its writes are encrypted under, as a launched app's are.
    #[cfg(test)]
    pub(crate) fn endpoint_channel_keys(&self) -> ui::collaboration::EndpointChannelKeys {
        use tilo_identity_host::broker::ChannelKeySource as _;
        let keys = session_channel_keys(self.channel_encryption.clone());
        Arc::new(
            move |workspace_id: &str, app_id: &str, record_id: &str, generation: u64| {
                let unavailable = |why: String| {
                    runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::other(why))
                };
                let room = crate::shell::state::RoomId::parse(workspace_id)
                    .ok_or_else(|| unavailable(format!("{workspace_id} is not a room")))?;
                let route = collab_types::ChannelRoute::new(app_id, record_id);
                let (access, generation, bytes) = match keys.access(room, &route, generation) {
                    Some(Ok(answer)) => answer,
                    Some(Err(error)) => return Err(unavailable(error)),
                    None => (
                        runtime_identity::wire::ChannelAccess::Unavailable,
                        generation,
                        Vec::new(),
                    ),
                };
                Ok(runtime_identity::wire::ChannelKeysReply {
                    access,
                    generation,
                    keys: (!bytes.is_empty()).then_some(bytes),
                })
            },
        )
    }

    pub(crate) fn local_session_lifecycle(
        &self,
    ) -> crate::shell::collab_broker_capnp::LocalSessionLifecycle {
        self.local_session.lifecycle()
    }

    /// Cloneable facade over the canonical shared broker's live action
    /// directory. Declared on the native wrapper as well as available through
    /// `Deref` so UFCS call sites work on every target.
    #[must_use]
    pub fn app_action_broker(&self) -> Option<AppActionBroker> {
        self.inner.app_action_broker()
    }

    /// Native device-fabric and compositor seam for realtime continuity.
    #[must_use]
    pub fn realtime_fabric_bridge(
        &self,
    ) -> Option<crate::shell::collab_broker_capnp::RealtimeFabricAppBridge> {
        self.realtime_fabric.clone()
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
        self.local_session.lifecycle().close();
        self.drain_quit.store(true, Ordering::Relaxed);
        if let Some(wake) = &self.drain_wake {
            wake.poke();
        }
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
/// cannot be bound. The parent must already be an owner-only real directory
/// (normally mode 0700); live sockets and non-private parents are rejected,
/// while proven stale socket inodes are reclaimed.
pub fn spawn_native(
    capnp_socket_path: PathBuf,
    peer_sync: &Option<Arc<ShellNetwork>>,
    membership: crate::shell::state::WorkspaceTreeHandle,
) -> std::io::Result<BrokerHandle> {
    spawn_native_offloading(capnp_socket_path, peer_sync, membership, None)
}

/// [`spawn_native`] whose action door asks `action_offload` where an
/// admitted offloadable invocation runs (`HostServices::action_offload`);
/// `None` runs each through its app's own handler, as [`spawn_native`] does.
///
/// # Errors
///
/// As [`spawn_native`].
pub fn spawn_native_offloading(
    capnp_socket_path: PathBuf,
    peer_sync: &Option<Arc<ShellNetwork>>,
    membership: crate::shell::state::WorkspaceTreeHandle,
    action_offload: Option<Arc<dyn tilo_actions::offload::ActionOffload>>,
) -> std::io::Result<BrokerHandle> {
    // Records this device lacks are brought from the room's other devices
    // over the same network this broker answers their requests on.
    if let Some(network) = peer_sync {
        crate::shell::record_hydration::set_source(Arc::new(
            crate::shell::record_hydration::NetworkSource(Arc::downgrade(network)),
        ));
    }
    let publisher = PeerPublisher::with_optional_owner_commit(
        peer_sync
            .clone()
            .map(|network| network as Arc<dyn crate::shell::collab_outbound::PeerPublishSink>),
        Arc::new(super::broker_services::NativeOwnerCommitService::new(
            membership.clone(),
            peer_sync.clone(),
        )),
    );
    spawn_inner(
        capnp_socket_path,
        peer_sync.as_ref(),
        &publisher,
        Some(membership),
        CertificateProvisioning::Production,
        None,
        action_offload,
        None,
    )
}

/// Spawn the production broker pipeline with an explicit enrolled-device set.
/// Intended for integration harnesses that do not have the desktop identity
/// registry on disk; certificate, proof, membership, role, and op validation
/// remain identical to production.
#[cfg(any(test, feature = "test-support"))]
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
        CertificateProvisioning::HarnessDevices(Arc::new(trusted_devices), None),
        None,
        None,
        None,
    )
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) type NativeReadHarnessWrapper = Arc<
    dyn Fn(
            Arc<dyn tilo_action_broker::services::NativeDocumentReads>,
        ) -> Arc<dyn tilo_action_broker::services::NativeDocumentReads>
        + Send
        + Sync,
>;
#[cfg(test)]
/// Production owner/read pipeline with already-authorized synthetic devices.
/// The wrapper is confined to this test harness; enrollment and custody checks stay intact.
pub(crate) fn spawn_native_owner_harness_with_reads(
    socket: PathBuf,
    membership: crate::shell::state::WorkspaceTreeHandle,
    trusted_devices: Vec<[u8; 32]>,
    network: &Arc<ShellNetwork>,
    reads: Option<NativeReadHarnessWrapper>,
) -> std::io::Result<BrokerHandle> {
    spawn_device_harness(socket, membership, trusted_devices, network, reads, None)
}

#[cfg(test)]
/// [`spawn_native_owner_harness_with_reads`] for a device whose channel
/// state is kept in `channel_store`, as a session's is under its secret
/// store: the broker's channel service opens it, so the device has the
/// keys of channels another device stewards.
pub(crate) fn spawn_device_harness(
    socket: PathBuf,
    membership: crate::shell::state::WorkspaceTreeHandle,
    trusted_devices: Vec<[u8; 32]>,
    network: &Arc<ShellNetwork>,
    reads: Option<NativeReadHarnessWrapper>,
    channel_store: Option<crate::shell::channel_encryption::ChannelEncryptionConfig>,
) -> std::io::Result<BrokerHandle> {
    let publisher = PeerPublisher::with_optional_owner_commit(
        Some(network.clone()),
        Arc::new(super::broker_services::NativeOwnerCommitService::new(
            membership.clone(),
            Some(network.clone()),
        )),
    );
    spawn_inner(
        socket,
        Some(network),
        &publisher,
        Some(membership),
        CertificateProvisioning::HarnessDevices(Arc::new(trusted_devices), reads),
        None,
        None,
        channel_store,
    )
}

/// Start the ordinary native service inside an independently owned peer.
/// No global identity registry, Secret Service store or desktop network actor
/// is opened. The explicit authority, journal and real signed-document publisher
/// remain owned by the calling application's session.
///
/// # Errors
///
/// Returns an error if `publisher` has no attached signed-document sink, or if
/// the Cap'n Proto socket at `capnp_socket_path` cannot be bound.
pub fn spawn_native_peer(
    capnp_socket_path: PathBuf,
    membership: crate::shell::state::WorkspaceTreeHandle,
    publisher: &Arc<PeerPublisher>,
    config: &crate::shell::peer_actions::NativePeerBrokerConfig,
) -> std::io::Result<BrokerHandle> {
    if !publisher.is_wired() {
        return Err(std::io::Error::other(
            "peer broker requires an attached signed-document publisher",
        ));
    }
    spawn_inner(
        capnp_socket_path,
        None,
        publisher,
        Some(membership),
        CertificateProvisioning::Production,
        Some(config),
        None,
        None,
    )
}

/// Selects certificate provisioning at a host-owned construction boundary.
/// The fixture variant is unavailable from ordinary production builds and is
/// selected only by `spawn_native_harness`.
enum CertificateProvisioning {
    Production,
    #[cfg(any(test, feature = "test-support"))]
    HarnessDevices(Arc<Vec<[u8; 32]>>, Option<NativeReadHarnessWrapper>),
}

impl CertificateProvisioning {
    fn uses_harness_devices(&self) -> bool {
        match self {
            Self::Production => false,
            #[cfg(any(test, feature = "test-support"))]
            Self::HarnessDevices(..) => true,
        }
    }
}

/// The history page a peer's scrollback request is answered with: from
/// durable history as well as the memory window, so a member who joined on
/// another device can page back through everything this device retained,
/// but only when the artifact belongs to the room whose topic asked, and
/// never for the person's own room. An artifact whose room this device never
/// learned is answered to nobody.
pub(crate) fn scrollback_answer(
    net: &ShellNetwork,
    channels: &Mutex<BrokerChannels>,
    entry: &InboundScrollbackRequest,
) -> Vec<collab_types::SignedOp> {
    let Ok(mut ch) = channels.lock() else {
        return Vec::new();
    };
    if !ch
        .room_of(entry.artifact)
        .is_some_and(|room| net.scrollback_answer_allowed(&room, entry.workspace_topic))
    {
        return Vec::new();
    }
    ch.scrollback_page(entry.artifact, entry.before_seq, entry.max)
        .0
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
    remote_actions: Arc<Mutex<Vec<crate::shell::remote_action_link::InboundRemoteAction>>>,
    /// Other devices' requests for, and answers with, records' content.
    record_content_inbound: Arc<Mutex<Vec<crate::shell::network::InboundRecordContent>>>,
    /// The record-content service those reach; absent on a peer broker.
    record_content: Option<Arc<tilo_action_broker::record_content::RouteContent>>,
    /// The action host remote requests are answered by, built once the
    /// application broker is up; its invocation ledger lives as long as
    /// the drainer so retries stay idempotent.
    action_host: std::sync::OnceLock<crate::shell::remote_actions::BrokerActionHost>,
    /// Set while remote requests wait for the application broker. The drain
    /// thread polls instead of parking until the next inbound message.
    awaiting_action_host: std::sync::atomic::AtomicBool,
    table: Arc<FanoutState>,
    channels: Arc<Mutex<BrokerChannels>>,
    net: Arc<ShellNetwork>,
    pending: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>>,
    /// This device's channel encryption service, pumped before documents
    /// drain so a channel's gate is in place before its first op.
    channel_encryption: Option<crate::shell::channel_encryption::ChannelKeyAccess>,
}

impl DrainQueues {
    /// Run channel governance once, so a channel's gate is in place before
    /// the pass's first op. Returns how long until governance next has work
    /// that no inbound frame brings; `None` parks the drain until one does.
    fn pump_governance(&self) -> Option<Duration> {
        self.channel_encryption
            .as_ref()
            .and_then(|service| service.pump_until_due(&*self.net))
    }

    /// Arm `watch` while stewards wait for a policy announcement; `false`
    /// when one already arrived and governance must be pumped again now.
    fn arm_policy_watch(&self, watch: &tilo_identity_host::policy_change::Watch) -> bool {
        self.channel_encryption
            .as_ref()
            .is_none_or(|service| service.arm_policy_watch(watch))
    }

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
        announced: &mut Announced,
    ) -> bool {
        self.drain_owner_views();
        // The sealed writes rooms' hosts sent, queued by the network actor,
        // join the inbound documents drained below; the opens waiting for a
        // host's replay are told once what it replayed was taken.
        let replayed = self.net.take_channel_traffic();
        let drained = self.drain_presence(labels, announced)
            && self.drain_documents(labels, announced)
            && self.drain_person_snapshot_requests()
            && self.drain_scrollback_requests()
            && self.drain_scrollback_responses()
            && self.drain_remote_actions()
            && self.drain_record_content();
        self.net.sealed_replays_taken(&replayed);
        drained
    }

    fn drain_owner_views(&self) {
        for (view, source_route) in self.net.pump_owner_views() {
            let route = tilo_action_broker::route::CrdtRoute::record(
                &view.app,
                &view.room,
                format!("continuation:{}", view.instance),
            );
            let event = CollabEvent::Handoff {
                request: collab_types::handoff::HandoffRequest::OwnerView { view },
            };
            if let Some(source) = source_route {
                let _ = self.net.with_current_personal_route(&source, || {
                    self.table.fanout_to_route(&event, u64::MAX, &route);
                });
            } else {
                self.table.fanout_to_route(&event, u64::MAX, &route);
            }
        }
    }

    fn drain_record_content(&self) -> bool {
        match &self.record_content {
            Some(content) => {
                crate::shell::record_content::drain(&self.record_content_inbound, content)
            }
            None => self
                .record_content_inbound
                .lock()
                .map(|mut inbound| inbound.clear())
                .is_ok(),
        }
    }

    /// Answer remote-action requests through the same broker local callers
    /// use, as the caller the transport authenticated.
    fn drain_remote_actions(&self) -> bool {
        let drained: Vec<_> = {
            let Ok(mut buf) = self.remote_actions.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        if drained.is_empty() {
            return true;
        }
        let host = self.action_host.get().or_else(|| {
            let state = self.table.capnp_state()?;
            Some(self.action_host.get_or_init(|| {
                crate::shell::remote_actions::BrokerActionHost::new(AppActionBroker::from_state(
                    state,
                ))
            }))
        });
        let drained = if host.is_some() {
            self.awaiting_action_host.store(false, Ordering::Relaxed);
            drained
        } else {
            // The drain thread starts before the broker attaches. A request
            // that arrives first waits for it instead of being refused; a
            // device that has just started otherwise loses the first open
            // sent to it. Records need no broker.
            let (records, waiting): (Vec<_>, Vec<_>) = drained.into_iter().partition(|request| {
                matches!(
                    request.message,
                    tilo_actions::remote::RemoteActionMessage::RecordRequest { .. }
                )
            });
            let any_waiting = !waiting.is_empty();
            let Some(overflow) = wait_for_action_host(&self.remote_actions, waiting) else {
                return false;
            };
            self.awaiting_action_host
                .store(any_waiting, Ordering::Relaxed);
            records.into_iter().chain(overflow).collect()
        };
        answer_remote_actions(&self.net, host, drained);
        true
    }

    /// Drain inbound presence, attributed to its sender. Cluster scope,
    /// matching a local presence frame (`CrdtRoute::to_cluster_scope`)
    /// — narrowing to a record would make a peer visible locally but
    /// invisible remotely.
    fn drain_presence(
        &self,
        labels: &mut HashMap<String, (Instant, PeerLabels)>,
        announced: &mut Announced,
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
                &|peer| self.net.peer_colour(peer),
                &self.table,
                labels,
                announced,
            );
            // Who acted is what the sender's certified identity supports.
            let (user_id, payload) = match payload {
                collab_types::presence::PresencePayload::Interaction(update) => {
                    let actor = update.actor.from_peer(&user_id, entry.person.as_deref());
                    let sequence = update.sequence;
                    (
                        actor.id.clone(),
                        collab_types::presence::PresencePayload::Interaction(Box::new(
                            update.normalized_by_broker(actor, sequence),
                        )),
                    )
                }
                payload => (user_id, payload),
            };
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
        announced: &mut Announced,
    ) -> bool {
        let drained_signed: Vec<_> = {
            let Ok(mut buf) = self.documents.lock() else {
                return false;
            };
            std::mem::take(&mut *buf)
        };
        for entry in drained_signed {
            let peer = entry.peer;
            let (app_id, cluster_id, record_id) = (
                entry.app_id.clone(),
                entry.cluster_id.clone(),
                entry.record_id.clone(),
            );
            let document = tilo_action_broker::collab_inbound::RemoteDocument {
                app_id: entry.app_id,
                cluster_id: entry.cluster_id,
                record_id: entry.record_id,
                author: entry.author,
                replayed: entry.replayed,
                envelope: *entry.envelope,
            };
            // Same id and room lookup the peer's presence arrives under, so
            // one person reads as one participant.
            let delivered = tilo_action_broker::collab_inbound::deliver(
                &self.channels,
                &self.table,
                document,
                |route| {
                    remote_participant_id(
                        &route.cluster_id,
                        peer,
                        &|c| self.net.peer_labels(c),
                        &|peer| self.net.peer_colour(peer),
                        &self.table,
                        labels,
                        announced,
                    )
                },
            );
            if !delivered {
                return false;
            }
            // After it is admitted: a window that subscribes from now on is
            // told it with the records its room holds
            // (`BrokerChannels::tree_records_held`), and one that subscribed
            // before is told here, so none misses it.
            self.announce_tree_record(&app_id, &cluster_id, &record_id);
        }
        true
    }

    /// A peer wrote a record of an app's tree (a chain's next part, a
    /// container's child) that no window here holds: tell the app's windows
    /// in that room, which then hold it like their own and sync it on its
    /// route, receiving what was admitted for it before
    /// (`CapnpBrokerState::subscribe_ops`). A window that opens after this
    /// learns the record from what the broker holds
    /// (`BrokerChannels::tree_records_held`).
    fn announce_tree_record(&self, app_id: &str, cluster_id: &str, record_id: &str) {
        if ui::record::RecordId::parse(record_id)
            .and_then(|id| id.tree())
            .is_none()
        {
            return;
        }
        let record = CrdtRoute::record(app_id, cluster_id, record_id);
        if self.table.holds_route(&record) {
            return;
        }
        self.table.fanout_to_route(
            &CollabEvent::RecordsArrived {
                records: vec![record_id.to_owned()],
            },
            u64::MAX,
            &CrdtRoute::app(app_id, cluster_id),
        );
    }

    /// When another of this person's devices came back, ask the local apps
    /// holding person-room records for their snapshots, which then travel to
    /// it over the personal fabric like any other edit.
    fn drain_person_snapshot_requests(&self) -> bool {
        while let Some(person_room) = self.net.take_person_snapshot_request() {
            let asked = self.table.request_room_snapshots(&person_room);
            tracing::debug!(asked, "asked local apps to catch a returning device up");
        }
        true
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
            let ops = scrollback_answer(&self.net, &self.channels, &entry);
            if !ops.is_empty() {
                let boxed: Vec<Box<collab_types::SignedOp>> =
                    ops.into_iter().map(Box::new).collect();
                self.net
                    .publish_scrollback_response(entry.workspace_topic, entry.artifact, boxed);
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
            // Pages this device asked for land in durable history, where the
            // requesting endpoint's next page request finds them.
            if let Ok(mut ch) = self.channels.lock() {
                let stored = ch.accept_peer_history(entry.artifact, &entry.ops);
                if stored > 0 {
                    tracing::debug!(artifact = ?entry.artifact, stored, "collab history: stored peer page");
                }
            }
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
            let oldest_seq = entry.ops.iter().map(|op| op.seq).min();
            let event = CollabEvent::ScrollbackBatch {
                artifact: entry.artifact,
                ops: entry.ops.into_iter().map(Box::new).collect(),
                decrypted_payloads: Vec::new(),
                oldest_seq,
                exhausted: true,
            };
            for route in waiting {
                self.table.fanout_to_route(&event, u64::MAX, &route);
            }
        }
        true
    }
}

/// Give `service` its call factory, which needs the native owner. The broker
/// is built on the compositor's loop, which must not wait for the owner's
/// open, so a factory for an owner still opening is installed from a thread
/// of its own once the open ends. Until then a call is refused as
/// unavailable, never placed against a guessed owner.
fn install_native_media_factory(
    network: &Arc<ShellNetwork>,
    service: &super::native_media::NativeMediaService,
) {
    fn install(
        network: &ShellNetwork,
        service: &super::native_media::NativeMediaService,
        owner: Result<crate::shell::native_workspace_owner::NativeWorkspaceOwner, String>,
    ) {
        let factory = owner.and_then(|owner| {
            network.runtime_handle().map(|runtime| {
                crate::network::native_media::Factory::new(
                    network.cmd_tx.clone(),
                    owner,
                    service.capture_broker(),
                    runtime,
                )
            })
        });
        match factory {
            Ok(factory) => {
                service.install_factory(Arc::new(factory));
            }
            Err(error) => {
                tracing::warn!(%error, "native media factory prerequisites unavailable");
            }
        }
    }
    match network.native_owner_status() {
        crate::shell::network::NativeOwnerStatus::Open(owner) => {
            install(network, service, Ok(owner));
        }
        crate::shell::network::NativeOwnerStatus::Opening
        | crate::shell::network::NativeOwnerStatus::Unavailable => {
            let network = Arc::clone(network);
            let service = service.clone();
            let spawned = std::thread::Builder::new()
                .name("native-media-factory".into())
                .spawn(move || install(&network, &service, network.native_owner()));
            if let Err(error) = spawned {
                tracing::warn!(%error, "cannot wait for the native owner to install native media");
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one ordered broker assembly: vaults, services, media, authority, then network attachment"
)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "the test-support provisioning variant moves its device set into the broker"
)]
fn spawn_inner(
    capnp_socket_path: PathBuf,
    peer_sync: Option<&Arc<ShellNetwork>>,
    publisher: &Arc<PeerPublisher>,
    membership: Option<crate::shell::state::WorkspaceTreeHandle>,
    certificate_provisioning: CertificateProvisioning,
    config: Option<&crate::shell::peer_actions::NativePeerBrokerConfig>,
    action_offload: Option<Arc<dyn tilo_actions::offload::ActionOffload>>,
    channel_store: Option<crate::shell::channel_encryption::ChannelEncryptionConfig>,
) -> std::io::Result<BrokerHandle> {
    let table = Arc::new(FanoutState::new());
    let channels: Arc<Mutex<BrokerChannels>> = Arc::new(Mutex::new(BrokerChannels::default()));
    // A configured peer broker runs without the session's vault.
    let channel_encryption = open_session_services(
        membership.as_ref(),
        peer_sync,
        &channels,
        config.is_none(),
        channel_store,
    );
    // Who is waiting on a cooperative scrollback answer, keyed by
    // artifact — a `ScrollbackResponse` off gossip carries an
    // artifact id and nothing else, so without this the drain thread
    // would have to broadcast the batch to every local app.
    let pending_scrollback: Arc<Mutex<HashMap<ArtifactId, Vec<CrdtRoute>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Inbound-drain: when peer-sync is on, drain signed documents, presence,
    // and cooperative scrollback. The thread parks until a frame arrives,
    // a channel is opened, or channel governance has work due (a steward's
    // policy re-check, a member's access-request backoff).
    // Every remote entry is routed like a local one, by the `(app_id,
    // cluster_id)` the publishing host stamped on the wire.
    let drain_quit = Arc::new(AtomicBool::new(false));
    let record_content = peer_sync
        .filter(|_| config.is_none())
        .map(crate::shell::record_content::session_record_content);
    if let Some(content) = &record_content {
        crate::shell::room_app_versions::fetch_through(content);
    }
    let drain_thread = peer_sync.and_then(|net| {
        let queues = DrainQueues {
            presence_inbound: net.inbound_app_presence_handle(),
            documents: net.inbound_documents_handle(),
            sb_req_inbound: net.inbound_scrollback_request_handle(),
            sb_resp_inbound: net.inbound_scrollback_response_handle(),
            remote_actions: net.inbound_remote_actions_handle(),
            record_content_inbound: net.inbound_record_content_handle(),
            record_content: record_content.clone(),
            action_host: std::sync::OnceLock::new(),
            awaiting_action_host: std::sync::atomic::AtomicBool::new(false),
            table: Arc::clone(&table),
            channels: Arc::clone(&channels),
            net: Arc::clone(net),
            pending: Arc::clone(&pending_scrollback),
            channel_encryption: channel_encryption.clone(),
        };
        let quit = Arc::clone(&drain_quit);
        let wake = std::sync::Arc::clone(&net.drain_wake);
        std::thread::Builder::new()
            .name("tilo-collab-drain".into())
            .spawn(move || {
                // Name already announced per (peer, cluster) — said
                // again only when it changes, not on every
                // pointer-rate presence frame.
                let mut announced = Announced::new();
                // What the room last said, on a re-ask cadence
                // rather than every frame — the lookup takes the
                // replica's lock and parses every peer id it holds.
                let mut labels: HashMap<String, (Instant, PeerLabels)> = HashMap::new();
                // A steward re-checks its channels' access policy when an
                // input announces a change or an expiry it registered comes
                // due, not on a clock. The watch is armed only
                // while stewards wait for that, so a device stewarding no
                // channel sleeps through every announcement.
                let policy_watch = {
                    let wake = std::sync::Arc::clone(&wake);
                    tilo_identity_host::policy_change::Watch::new(move || wake.poke())
                };
                queues.net.channel_drain_runs(true);
                while !quit.load(Ordering::Relaxed) {
                    let mut governance_due = queues.pump_governance();
                    if !queues.drain_once(&mut labels, &mut announced) {
                        break;
                    }
                    if !queues.arm_policy_watch(&policy_watch) {
                        governance_due = Some(Duration::ZERO);
                    }
                    let timeout = if queues.awaiting_action_host.load(Ordering::Relaxed) {
                        Some(
                            governance_due
                                .map_or(ACTION_HOST_POLL, |due| due.min(ACTION_HOST_POLL)),
                        )
                    } else {
                        governance_due
                    };
                    wake.wait(timeout);
                }
                queues.net.channel_drain_runs(false);
            })
            .ok()
    });

    let local_session = crate::shell::collab_broker_capnp::LocalSessionAuthority::new();
    let channel_keys = if config.is_none() {
        Some(session_channel_keys(channel_encryption.clone()))
    } else {
        None
    };
    let mut services = if config.is_none() {
        super::broker_services::native_services(
            membership.clone(),
            peer_sync.cloned(),
            channel_keys.clone(),
            Some(Arc::clone(&channels)),
        )
    } else {
        tilo_action_broker::services::HostServices::default()
    };
    // The broker's runs and the shell's composer read one tracker.
    let tracker = membership
        .as_ref()
        .filter(|_| config.is_none())
        .map(|tree| {
            super::broker_services::native_tracker(tree, peer_sync.cloned(), channel_keys.clone())
        });
    if let Some(tracker) = &tracker {
        services.tracker_runs =
            Some(tracker.clone() as Arc<dyn tilo_action_broker::services::TrackerRuns>);
    }
    #[cfg(any(test, feature = "test-support"))]
    if let CertificateProvisioning::HarnessDevices(_, Some(wrap)) = &certificate_provisioning {
        services.committed_documents = services.committed_documents.map(|reads| wrap(reads));
    }
    services.record_content = record_content;
    services.action_offload = action_offload;
    // The same service the identity broker answers key requests from: its
    // changes tell each route's endpoint when to ask again. A session broker
    // follows even without one; a configured peer broker never has one and
    // still says it cannot follow.
    services.channel_keys = channel_keys;
    services.audio_output = Some(Arc::new(super::audio_output::AudioOutputService::new(
        local_session.lifecycle(),
    )));
    if certificate_provisioning.uses_harness_devices() && config.is_none() {
        services.native_certificates = None;
        services.local_person = None;
    }
    let realtime_fabric = if config.is_none() {
        peer_sync.and_then(crate::shell::collab_broker_capnp::NativeRealtimeServices::from_network)
    } else {
        None
    };
    if let Some((service, _)) = realtime_fabric.as_ref() {
        services.realtime = Some(Arc::clone(service));
    }
    let native_media = peer_sync.and_then(|network| {
        let (device, _) = network.local_device_fabric_identity()?;
        let (_, bridge) = realtime_fabric.as_ref()?;
        Some(super::native_media::NativeMediaService::new(
            local_session.lifecycle(),
            bridge.realtime_capture_broker()?,
            device,
        ))
    });
    if let Some(service) = native_media.as_ref() {
        if let Some(network) = peer_sync {
            install_native_media_factory(network, service);
        }
        services.native_media = Some(Arc::new(service.clone()));
    }
    let authority = membership.as_ref().map(|tree| {
        Arc::new(super::broker_services::NativeWorkspaceAuthority(
            tree.clone(),
        )) as tilo_action_broker::services::WorkspaceAuthorityHandle
    });
    let personal_socket = capnp_socket_path.clone();
    let inner = match certificate_provisioning {
        CertificateProvisioning::Production => {
            tilo_action_broker::collab_broker::spawn_with_services(
                capnp_socket_path,
                authority,
                publisher.clone(),
                config,
                services,
                channels,
                table,
            )?
        }
        #[cfg(any(test, feature = "test-support"))]
        CertificateProvisioning::HarnessDevices(trusted_devices, _) => {
            tilo_action_broker::collab_broker::spawn_with_test_services(
                capnp_socket_path,
                authority,
                publisher.clone(),
                config,
                services,
                Some(trusted_devices),
                channels,
                table,
            )?
        }
    };
    if let (Some(network), Some(tree), Some(broker)) =
        (peer_sync, membership, inner.app_action_broker())
        && let Ok((queue, host)) = broker.peer_action_attachment()
    {
        let rooms = tree
            .read()
            .desktop
            .workspaces
            .iter()
            .filter_map(|w| uuid::Uuid::parse_str(&w.room().to_string()).ok())
            .collect();
        network
            .cmd_tx
            .try_send(crate::network::actor::NetCmd::AttachActionBroker { queue, host, rooms })
            .map_err(|_| std::io::Error::other("action broker network attachment unavailable"))?;
    }
    let personal_source = inner
        .app_action_broker()
        .zip(peer_sync.and_then(|network| network.messaging_host_identity()))
        .and_then(|(broker, host)| {
            NativePersonalSource::install(personal_socket, broker, local_session.lifecycle(), host)
        });
    Ok(BrokerHandle {
        tracker,
        inner,
        local_session,
        _personal_source: personal_source,
        native_media,
        realtime_fabric: realtime_fabric.map(|(_, bridge)| bridge),
        drain_thread,
        drain_quit,
        drain_wake: peer_sync.map(|net| std::sync::Arc::clone(&net.drain_wake)),
        channel_encryption,
        peer_departures: peer_sync.map(|net| net.peer_departure_history()),
    })
}

/// What a session broker's endpoints follow for their channels' key changes
/// (`channelKeysChanged`): the session's channel service, or none.
///
/// Without a service a route holds no keys (the identity service answers
/// from the same slot, so it says `Unavailable`), and nothing changes for
/// this broker's lifetime. The follow then stays pending at no cost.
/// Refusing it instead tells the endpoint that this broker cannot say when
/// keys change, and it falls back to asking the identity service for every
/// route once a second, forever.
fn session_channel_keys(
    access: Option<crate::shell::channel_encryption::ChannelKeyAccess>,
) -> crate::shell::channel_encryption::ChannelKeyProvider {
    let keys = crate::shell::channel_encryption::ChannelKeyProvider::default();
    keys.set(access);
    keys
}

/// The broker's session services: the durable history store and the
/// channel encryption service, which is all a caller keeps.
type SessionServices = Option<crate::shell::channel_encryption::ChannelKeyAccess>;

/// Open the session's services. The channel encryption service runs
/// whenever this session reaches room hosts (`peer_sync`) or has a secret
/// store, unless it is a configured peer broker (`own_channels`): keys of the rooms this device's
/// own room host stewards come from that host and need nothing else. The
/// secret store only adds this device's own channel state, which rooms
/// hosted elsewhere need ([`session_channel_store`]).
fn open_session_services(
    membership: Option<&crate::shell::state::WorkspaceTreeHandle>,
    peer_sync: Option<&Arc<ShellNetwork>>,
    channels: &Arc<Mutex<BrokerChannels>>,
    own_channels: bool,
    channel_store: Option<crate::shell::channel_encryption::ChannelEncryptionConfig>,
) -> SessionServices {
    open_session_history(channels);
    if !own_channels {
        return None;
    }
    let workspaces = membership?.clone();
    let store = channel_store.or_else(session_channel_store);
    // With neither a store nor a room host to ask, no channel has keys.
    if store.is_none() && peer_sync.is_none() {
        return None;
    }
    channel_key_access(store, workspaces, peer_sync.cloned(), Arc::clone(channels))
}

/// Keep the session's collaboration history durably. Tests keep it in
/// memory, since the store lives in the session's state root.
#[cfg(test)]
fn open_session_history(_: &Arc<Mutex<BrokerChannels>>) {}

#[cfg(not(test))]
fn open_session_history(channels: &Arc<Mutex<BrokerChannels>>) {
    match crate::shell::collab_history::CollabHistoryStore::open_for_session() {
        Ok(history) => {
            if let Ok(mut state) = channels.lock() {
                state.history = Some(history);
            }
        }
        Err(error) => {
            tracing::warn!(%error, "durable collaboration history unavailable; scrollback is limited to memory");
        }
    }
}

/// This device's own channel state: kept under the session's secret-store
/// vault and signed with its certified device key. `None` without either;
/// the channel service then serves only rooms this device's own host
/// stewards, and never falls back to plaintext. Tests have no secret store.
#[cfg(test)]
fn session_channel_store() -> Option<crate::shell::channel_encryption::ChannelEncryptionConfig> {
    None
}

#[cfg(not(test))]
fn session_channel_store() -> Option<crate::shell::channel_encryption::ChannelEncryptionConfig> {
    let vault = match crate::shell::mls_vault::SecretServiceMlsVault::open_for_session() {
        Ok(vault) => vault,
        Err(error) => {
            tracing::warn!(%error, "no system secret store answered: only rooms this device hosts have channel keys");
            return None;
        }
    };
    let paths = crate::firstboot::collaboration_identity_paths();
    let device_key = match tilo_user_identity::persist::read_device_key(&paths.device_privkey) {
        Ok(key) => key,
        Err(error) => {
            tracing::warn!(%error, "no certified device key: only rooms this device hosts have channel keys");
            return None;
        }
    };
    Some(crate::shell::channel_encryption::ChannelEncryptionConfig {
        root: vault.channel_state_root(),
        vault: vault.channel_state_key(),
        device_key,
    })
}

/// Open this device's channel encryption service, with its own channel
/// state under `store` when a secret store keeps one: its directory follows
/// `workspaces`, the devices `net` authenticates and each room's steward as
/// this device's native owner holds the room.
pub(crate) fn channel_key_access(
    store: Option<crate::shell::channel_encryption::ChannelEncryptionConfig>,
    workspaces: crate::shell::state::WorkspaceTreeHandle,
    net: Option<Arc<ShellNetwork>>,
    channels: Arc<Mutex<BrokerChannels>>,
) -> Option<crate::shell::channel_encryption::ChannelKeyAccess> {
    // An endpoint opening a channel queues governance the parked drain
    // must send now (a genesis, an access request): it pokes the drain.
    let drain_wake = net.as_ref().map(|net| Arc::clone(&net.drain_wake));
    let (authenticated, authenticated_changes, stewards, hosted): (
        crate::shell::channel_policy::AuthenticatedDevices,
        crate::shell::channel_policy::AuthenticatedChanges,
        crate::shell::channel_policy::Stewards,
        Option<crate::shell::channel_policy::HostedChannelKeys>,
    ) = match net {
        Some(net) => {
            let changes = Arc::clone(&net);
            let stewards = crate::shell::network::ShellNetwork::room_stewards(&net);
            let hosted = hosted_channel_keys_of(Arc::clone(&net));
            (
                Box::new(move |user| net.authenticated_devices_of(user)),
                Box::new(move |after| changes.authenticated_devices_next_change(after)),
                stewards,
                Some(hosted),
            )
        }
        None => (
            Box::new(|_| Vec::new()),
            Box::new(|_| None),
            Box::new(|_| None),
            None,
        ),
    };
    let directory = crate::shell::channel_policy::WorkspaceChannelDirectory::new(
        workspaces,
        crate::shell::channel_policy::WorkspaceChannelDirectory::session_fleet_with(authenticated),
    )
    .with_authenticated_changes(authenticated_changes)
    .with_stewards(stewards);
    let directory = match hosted {
        Some(hosted) => directory.with_hosted_channel_keys(hosted),
        None => directory,
    };
    let opened = match store {
        Some(store) => crate::shell::channel_encryption::ChannelEncryption::open(
            store,
            Box::new(directory),
            channels,
        ),
        None => Ok(
            crate::shell::channel_encryption::ChannelEncryption::without_store(
                Box::new(directory),
                channels,
            ),
        ),
    };
    match opened {
        Ok(service) => Some(match drain_wake {
            Some(wake) => crate::shell::channel_encryption::ChannelKeyAccess::with_wake(
                service,
                Arc::new(move || wake.poke()),
            ),
            None => crate::shell::channel_encryption::ChannelKeyAccess::new(service),
        }),
        Err(error) => {
            tracing::warn!(%error, "channel encryption state could not be opened");
            None
        }
    }
}

/// Requests kept while the application broker is not attached yet.
const MAX_REQUESTS_AWAITING_ACTION_HOST: usize = 32;
/// How often the drain thread looks for the broker while requests wait.
const ACTION_HOST_POLL: Duration = Duration::from_millis(250);

/// Put `waiting` back ahead of anything that arrived while they were out of
/// the queue. Returns the oldest requests past the bound, to be refused, or
/// `None` when the queue lock is poisoned.
fn wait_for_action_host(
    queue: &Mutex<Vec<crate::shell::remote_action_link::InboundRemoteAction>>,
    waiting: Vec<crate::shell::remote_action_link::InboundRemoteAction>,
) -> Option<Vec<crate::shell::remote_action_link::InboundRemoteAction>> {
    let mut buf = queue.lock().ok()?;
    let arrived_since = std::mem::take(&mut *buf);
    buf.extend(waiting);
    buf.extend(arrived_since);
    let excess = buf.len().saturating_sub(MAX_REQUESTS_AWAITING_ACTION_HOST);
    Some(buf.drain(..excess).collect())
}

/// Answer each request through `host` and send the answer back to the
/// device that asked, on the topic it asked on.
pub(crate) fn answer_remote_actions(
    net: &Arc<ShellNetwork>,
    host: Option<&crate::shell::remote_actions::BrokerActionHost>,
    requests: Vec<crate::shell::remote_action_link::InboundRemoteAction>,
) {
    // A waiting request parks on the broker's change signal, a task on the
    // session runtime; without one it is answered at once, as before.
    let _runtime = crate::session_runtime::runtime()
        .ok()
        .map(tokio::runtime::Runtime::enter);
    for request in requests {
        // A record is read from where this device keeps it, by the device
        // itself; no action host is asked.
        if let tilo_actions::remote::RemoteActionMessage::RecordRequest { correlation, query } =
            &request.message
        {
            let workspace = crate::shell::state::RoomId::parse(&query.room)
                .and_then(|room| net.workspace_of_room(room));
            let result = crate::shell::record_hydration::serve(
                workspace.as_ref(),
                ::launcher::appdir::host_data_home().as_deref(),
                &request.caller,
                query,
            );
            net.send_remote_action(
                request.workspace_topic,
                request.from,
                tilo_actions::remote::RemoteActionMessage::RecordResponse {
                    correlation: *correlation,
                    result,
                },
            );
            continue;
        }
        let answer = match host {
            Some(host) => tilo_actions::remote::serve(host, &request.caller, request.message),
            None => tilo_actions::remote::refuse(
                &request.message,
                tilo_actions::remote::ActionHostError::Unavailable {
                    reason: "the application broker is not running".into(),
                },
            )
            .map(tilo_actions::remote::Pending::ready),
        };
        // Answered whenever the host has the answer — an application that
        // re-resolves a directory row never holds up this drainer.
        if let Some(answer) = answer {
            let net = Arc::clone(net);
            answer.then(move |reply| {
                if let Some(reply) = reply {
                    net.send_remote_action(request.workspace_topic, request.from, reply);
                }
            });
        }
    }
}

/// What has been announced per (room, peer): the name, the colour, and the
/// connection generation it was said in front of.
type Announced = HashMap<(String, u64), (String, Option<collab_types::presence::PresenceHue>, u64)>;

/// The id a remote peer's traffic is attributed under, announcing the
/// room's name for it and the colour its own machine says it wears
/// (`colour_of`) first if nobody has said them yet. Every path showing a
/// remote participant goes through here so they agree. The id stays the
/// peer handle even once named or coloured — a roster entry names and
/// colours a participant, it does not make it someone else. `labels`
/// caches the room's answer on [`LABEL_TTL`]; `announced` remembers what has
/// been said, so the roster publishes on change rather than every frame.
fn remote_participant_id(
    cluster_id: &str,
    peer: u64,
    labels_of: &dyn Fn(&str) -> PeerLabels,
    colour_of: &dyn Fn(u64) -> Option<collab_types::presence::PresenceHue>,
    table: &FanoutState,
    labels: &mut HashMap<String, (Instant, PeerLabels)>,
    announced: &mut Announced,
) -> String {
    let now = Instant::now();
    let fresh = labels
        .get(cluster_id)
        .is_some_and(|(at, _)| now.duration_since(*at) < LABEL_TTL);
    if !fresh {
        labels.insert(cluster_id.to_string(), (now, labels_of(cluster_id)));
    }
    // Attributed to the publishing session, not a local peer slot: this is
    // what colours the caret, so two peers must never collapse to one id.
    // What the room (i.e. whoever invited them) calls this peer; an
    // unnamed peer has no name and stays labelled with its id.
    let unnamed = PeerLabels::new();
    let room = labels.get(cluster_id).map_or(&unnamed, |(_, room)| room);
    let crate::shell::peer_presence::PresencePrincipal { id: user_id, name } =
        crate::shell::peer_presence::resolve_presence_principal(room, peer);
    let slot = (cluster_id.to_string(), peer);
    let known = name.as_deref();
    let prior = announced.get(&slot);
    let generation = table.connections_seen();
    let colour = colour_of(peer);
    let say = roster_announcement(prior.map(|(n, _, _)| n.as_str()), known, &user_id)
        // "Changed" alone misses an app that connected after the name
        // was said — it was never in the audience, and nothing would
        // say it again. A connection this answer hasn't been said in
        // front of is itself a reason to say it again, and so is a
        // colour the peer wears that has not been said.
        .or_else(|| {
            let unheard = prior.is_some_and(|(_, _, at)| *at != generation);
            let recoloured = colour.is_some() && prior.is_none_or(|(_, said, _)| *said != colour);
            (unheard || recoloured).then(|| known.unwrap_or(&user_id).to_string())
        });
    if let Some(name) = say {
        announced.insert(slot, (name.clone(), colour, generation));
        let roster = CollabEvent::Context {
            collaborators: vec![collab_types::presence::Collaborator {
                id: user_id.clone(),
                name,
                colour,
                // Always a remote network peer here — an agent joins
                // over the local socket and is marked there instead.
                is_agent: false,
                is_self: false,
                joined_at: None,
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

/// The keys of the channels of rooms `net`'s own room host governs, from
/// that host's steward.
pub(crate) fn hosted_channel_keys_of(
    net: Arc<crate::shell::network::ShellNetwork>,
) -> crate::shell::channel_policy::HostedChannelKeys {
    Box::new(move |room, channel, route, create| {
        net.hosted_channel_keys(room, channel, route, create)
    })
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each test keeps its fanout table alive for the whole scenario"
)]
mod tests {
    use super::*;

    fn inbound_for_test(topic: u64) -> crate::shell::remote_action_link::InboundRemoteAction {
        crate::shell::remote_action_link::InboundRemoteAction {
            workspace_topic: topic,
            from: iroh::SecretKey::generate().public(),
            caller: tilo_actions::remote::Caller::authenticated([1; 32], [2; 32]),
            message: tilo_actions::remote::RemoteActionMessage::Observe {
                invocation: tilo_actions::InvocationId::new(),
                wait_ms: None,
                seen: None,
            },
        }
    }

    /// A session broker without a channel service answers an endpoint's
    /// `channelKeysChanged` with a wait, not a refusal: a refusal makes the
    /// endpoint ask the identity service for every route once a second.
    /// The wait still ends when the slot it follows changes.
    #[test]
    fn a_session_broker_without_a_channel_service_keeps_key_follows_pending() {
        use std::future::Future as _;
        let keys = session_channel_keys(None);
        let channel = collab_types::document_channel_id(
            "so.tilo.terminal",
            "00000000-0000-0000-0000-000000000000",
            "tab",
        );
        let mut follow = std::pin::pin!(keys.channel_changed(channel, 0));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            follow.as_mut().poll(&mut cx).is_pending(),
            "with no channel service nothing changes, so the follow waits"
        );
        assert!(
            follow.as_mut().poll(&mut cx).is_pending(),
            "and keeps waiting"
        );
        keys.set(None);
        assert_eq!(
            follow.as_mut().poll(&mut cx),
            std::task::Poll::Ready(0),
            "a change of the followed slot ends the wait"
        );
    }

    #[test]
    fn requests_wait_in_order_for_the_action_host_up_to_a_bound() {
        let queue = Mutex::new(vec![inbound_for_test(2)]);
        let overflow = wait_for_action_host(&queue, vec![inbound_for_test(1)]).expect("queue");
        assert!(overflow.is_empty());
        let topics: Vec<u64> = queue
            .lock()
            .expect("queue")
            .iter()
            .map(|action| action.workspace_topic)
            .collect();
        assert_eq!(topics, vec![1, 2], "a waiting request keeps its place");

        let waiting = (3..=u64::try_from(MAX_REQUESTS_AWAITING_ACTION_HOST).unwrap() + 2)
            .map(inbound_for_test)
            .collect();
        let overflow = wait_for_action_host(&queue, waiting).expect("queue");
        assert_eq!(
            overflow
                .iter()
                .map(|action| action.workspace_topic)
                .collect::<Vec<_>>(),
            vec![3, 4],
            "the oldest requests past the bound are refused"
        );
        assert_eq!(
            queue.lock().expect("queue").len(),
            MAX_REQUESTS_AWAITING_ACTION_HOST
        );
    }

    #[test]
    fn application_peer_ids_skip_reserved_values_and_never_wrap() {
        fn allocated_application_peers_skip_reserved_ids_and_preserve_connection_counts() {
            let table = FanoutState::new();
            assert_eq!(table.connections_seen(), 0);
            assert_eq!(table.next_peer_id(), 1);
            assert_eq!(table.connections_seen(), 1);
            assert_eq!(table.next_peer_id(), 2);
            assert_eq!(table.connections_seen(), 2);
        }

        fn exhausted_application_peer_ids_do_not_wrap_or_issue_the_current_connection_sentinel() {
            let table = FanoutState::new();
            table.set_peer_counter_for_test(u64::MAX - 2);
            assert_eq!(table.next_peer_id(), u64::MAX - 1);
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| table.next_peer_id()))
                    .is_err()
            );
            assert_eq!(table.connections_seen(), u64::MAX - 1);
        }

        allocated_application_peers_skip_reserved_ids_and_preserve_connection_counts();
        exhausted_application_peer_ids_do_not_wrap_or_issue_the_current_connection_sentinel();
    }

    /// A digest-shaped peer id, as presence frames carry it.
    const PEER: &str = "peer@5537458103208628978";

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn document_routes_stay_bound_to_their_app_record_and_signed_channel() {
        fn document_routes_stay_with_their_app_and_record() {
            let document = CrdtRoute::record("so.tilo.notes", "room-1", "note-1");

            assert!(document.delivers_to(&document));
            assert!(document.delivers_to(&CrdtRoute::cluster("room-1")));
            assert!(
                !document.delivers_to(&CrdtRoute::record("so.tilo.files", "room-1", "note-1",))
            );
            assert!(
                !document.delivers_to(&CrdtRoute::record("so.tilo.notes", "room-1", "note-2",))
            );
            assert!(
                !document.delivers_to(&CrdtRoute::record("so.tilo.notes", "room-2", "note-1",))
            );
        }

        fn remote_document_route_preserves_signed_kind_and_rejects_relabeling() {
            let route = CrdtRoute::record("so.tilo.notes", "room-1", "record-1");
            let (signing, author) = collab_types::generate_keypair();
            let mut envelope = collab_types::SignedOp::new_unsigned(
                DocumentMutation::Snapshot(b"whole document".to_vec())
                    .encode()
                    .expect("typed snapshot"),
                document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
                author,
                document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
                OpKind::Change,
                std::time::SystemTime::now(),
                1,
            );
            collab_types::sign_op(&mut envelope, &signing).expect("signed snapshot");

            assert!(matches!(
                validate_document_route(&route, &envelope),
                Ok(collab_types::RoutedDocumentPayload::Cleartext(
                    DocumentMutation::Snapshot(data)
                )) if data == b"whole document"
            ));
            assert_eq!(
                validate_document_route(
                    &CrdtRoute::record("so.tilo.notes", "room-1", "record-2"),
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

        fn remote_document_broker_routes_opaque_mls_only_for_bound_channel() {
            let route = CrdtRoute::record("so.tilo.notes", "room-1", "record-1");
            let expected_channel = collab_types::document_channel_id(
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            );
            let (_, author) = collab_types::generate_keypair();
            let mut envelope = collab_types::SignedOp::new_unsigned_with_encryption(
                b"opaque-not-a-document-mutation".to_vec(),
                document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
                author,
                document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
                OpKind::Change,
                std::time::SystemTime::now(),
                1,
                collab_types::EncryptionMetadata::mls_application(9, expected_channel),
            );
            assert_eq!(
                validate_document_route(&route, &envelope),
                Ok(collab_types::RoutedDocumentPayload::Encrypted {
                    channel: expected_channel,
                    epoch: 9,
                })
            );
            envelope.encryption.as_mut().expect("metadata").channel =
                collab_types::ChannelId::new();
            assert_eq!(
                validate_document_route(&route, &envelope),
                Err(ValidationError::DocumentRouteMismatch)
            );
        }

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
            let dispatcher = super::SessionFollowDispatcher::new(Arc::clone(&table));
            dispatcher.set("room-2", Some("peer@wrong".into()));
            dispatcher.set("room-1", Some("peer@7".into()));
            dispatcher.set("room-1", None);
            assert_eq!(
                seen.lock().unwrap().as_slice(),
                &[Some("peer@7".to_string()), None]
            );
        }

        document_routes_stay_with_their_app_and_record();
        remote_document_route_preserves_signed_kind_and_rejects_relabeling();
        remote_document_broker_routes_opaque_mls_only_for_bound_channel();
        follow_dispatcher_is_local_and_room_scoped();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn peer_names_are_announced_once_per_app_and_revert_when_revoked() {
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

        fn an_unnamed_peer_is_not_announced() {
            assert_eq!(super::roster_announcement(None, None, PEER), None);
        }

        fn a_revoked_name_reverts_the_peer_to_its_id() {
            assert_eq!(
                super::roster_announcement(Some("bob"), None, PEER),
                Some(PEER.to_string()),
            );
            assert_eq!(super::roster_announcement(Some(PEER), None, PEER), None);
        }

        /// An app that starts after the name was said still learns it —
        /// "changes" is measured against what was said, not against who heard it.
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
                    &|_| None,
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
                &|_| None,
                &table,
                &mut labels,
                &mut announced,
            );
            let from_delta = super::remote_participant_id(
                "room-1",
                HANDLE,
                &room,
                &|_| None,
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

        fn the_colour_a_peer_wears_reaches_every_app_once() {
            const HANDLE: u64 = 77;

            type Heard = Arc<Mutex<Vec<(String, Option<collab_types::presence::PresenceHue>)>>>;
            let table = Arc::new(super::FanoutState::new());
            let heard: Heard = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::clone(&heard);
            let _sub = table.subscribe_rooms(
                vec!["room-1".to_string()],
                Box::new(move |event, _route| {
                    if let CollabEvent::Context { collaborators, .. } = event {
                        for c in collaborators {
                            if let Ok(mut g) = sink.lock() {
                                g.push((c.name.clone(), c.colour));
                            }
                        }
                    }
                }),
            );
            let unnamed = |_c: &str| PeerLabels::new();
            let colour = Mutex::new(None);
            let colour_of = |_peer: u64| *colour.lock().unwrap();
            let mut labels = HashMap::new();
            let mut announced = super::Announced::new();
            let mut say = || {
                super::remote_participant_id(
                    "room-1",
                    HANDLE,
                    &unnamed,
                    &colour_of,
                    &table,
                    &mut labels,
                    &mut announced,
                )
            };

            say();
            assert!(
                heard.lock().unwrap().is_empty(),
                "an unnamed peer that said no colour is not announced"
            );
            *colour.lock().unwrap() = Some(collab_types::presence::PresenceHue::Violet);
            say();
            say();
            let id = format!("peer@{HANDLE}");
            assert_eq!(
                heard.lock().unwrap().clone(),
                vec![(
                    id.clone(),
                    Some(collab_types::presence::PresenceHue::Violet)
                )],
                "the colour is said once, under the id"
            );
            *colour.lock().unwrap() = Some(collab_types::presence::PresenceHue::Teal);
            say();
            assert_eq!(
                heard.lock().unwrap().last().cloned(),
                Some((id, Some(collab_types::presence::PresenceHue::Teal))),
                "a new choice is said again"
            );
        }

        a_named_peer_is_announced_and_then_left_alone();
        an_unnamed_peer_is_not_announced();
        the_colour_a_peer_wears_reaches_every_app_once();
        a_revoked_name_reverts_the_peer_to_its_id();
        a_name_already_announced_is_said_again_for_an_app_that_missed_it();
        a_remote_peer_is_one_id_across_presence_and_deltas();
    }
}
