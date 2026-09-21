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

use collab_types::{
    ArtifactId, ArtifactKind, OpClass, RedactedMarker, ValidationError,
    validate_admitted_inbound_op,
};
#[cfg(test)]
use collab_types::{DocumentMutation, document_artifact_id, document_attestation_id};
use ui::collaboration::CollabEvent;

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

pub use qos_action_broker::route::CrdtRoute;

pub use qos_action_broker::collab_broker::{
    AppActionBroker, FanoutSink, FanoutState, RoomSubscription, SessionFollowDispatcher,
};

pub struct BrokerHandle {
    inner: qos_action_broker::collab_broker::BrokerHandle,
    local_session: crate::shell::collab_broker_capnp::LocalSessionAuthority,
    realtime_fabric: Option<crate::shell::collab_broker_capnp::RealtimeFabricAppBridge>,
    /// Spawned only when there's a `ShellNetwork` to pump.
    drain_thread: Option<JoinHandle<()>>,
    drain_quit: Arc<AtomicBool>,
    /// Trusted shell-only MLS lifecycle transaction boundary. This is never
    /// exposed through the app-facing Cap'n Proto service.
    mls_authority: Option<
        Arc<
            Mutex<
                crate::shell::collab_mls_authority::MlsAuthorityCoordinator<
                    crate::shell::collab_mls_authority::SecretServiceMlsVault,
                >,
            >,
        >,
    >,
    peer_departures: Option<Arc<crate::shell::network::PeerDepartureHistory>>,
}

impl std::ops::Deref for BrokerHandle {
    type Target = qos_action_broker::collab_broker::BrokerHandle;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl BrokerHandle {
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

    /// Apply a typed, handle-only MLS membership transaction at the trusted
    /// workspace lifecycle boundary. Apps cannot call this interface.
    ///
    /// # Errors
    ///
    /// Returns an error when lifecycle authority is unavailable or rejects the
    /// transaction.
    pub fn apply_mls_authority_transaction(
        &self,
        transaction: runtime_collab::MlsAuthorityTransaction,
    ) -> Result<[u8; 32], crate::shell::collab_mls_authority::MlsAuthorityError> {
        let authority = self.mls_authority.as_ref().ok_or_else(|| {
            crate::shell::collab_mls_authority::MlsAuthorityError::VaultUnavailable(
                "Secret Service MLS lifecycle authority is unavailable".into(),
            )
        })?;
        authority
            .lock()
            .map_err(|_| {
                crate::shell::collab_mls_authority::MlsAuthorityError::Persistence(
                    "MLS lifecycle authority lock poisoned".into(),
                )
            })?
            .apply(transaction)
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
        CertificateProvisioning::Production,
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
        CertificateProvisioning::HarnessDevices(Arc::new(trusted_devices)),
        None,
    )
}

/// Start the ordinary native service inside an independently owned peer.
/// No global identity registry, Secret Service store or desktop network actor
/// is opened. The explicit authority, journal and real signed-document publisher
/// remain owned by the calling application's session.
pub fn spawn_native_peer(
    capnp_socket_path: PathBuf,
    membership: crate::shell::state::WorkspaceTreeHandle,
    publisher: Arc<PeerPublisher>,
    config: crate::shell::peer_actions::NativePeerBrokerConfig,
) -> std::io::Result<BrokerHandle> {
    if !publisher.is_wired() {
        return Err(std::io::Error::other(
            "peer broker requires an attached signed-document publisher",
        ));
    }
    spawn_inner(
        capnp_socket_path,
        None,
        &publisher,
        Some(membership),
        CertificateProvisioning::Production,
        Some(&config),
    )
}

/// Selects certificate provisioning at a host-owned construction boundary.
/// The fixture variant is unavailable from ordinary production builds and is
/// selected only by `spawn_native_harness`.
enum CertificateProvisioning {
    Production,
    #[cfg(any(test, feature = "test-support"))]
    HarnessDevices(Arc<Vec<[u8; 32]>>),
}

impl CertificateProvisioning {
    fn uses_harness_devices(&self) -> bool {
        match self {
            Self::Production => false,
            #[cfg(any(test, feature = "test-support"))]
            Self::HarnessDevices(_) => true,
        }
    }
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
            let payload = validate_document_route(&route, &envelope);
            let validation = if let Ok(payload) = &payload {
                let Ok(mut ch) = self.channels.lock() else {
                    return false;
                };
                let channel = match payload {
                    collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded) => {
                        collab_types::document_channel_id_for_version(
                            decoded.version,
                            &entry.app_id,
                            &entry.cluster_id,
                            &entry.record_id,
                        )
                    }
                    collab_types::VersionedRoutedDocumentPayload::Encrypted { channel, .. } => {
                        *channel
                    }
                };
                if ch.document_encryption_allowed(channel, envelope.encryption.as_ref()) {
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
                } else {
                    Err(ValidationError::DocumentRouteMismatch)
                }
            } else {
                Err(payload.as_ref().expect_err("checked error").clone())
            };
            match (payload, validation) {
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
            let mls_vault = ch.mls_vault.clone();
            let state = ch.state_for(artifact);
            state.record_validated_op(envelope.clone());
            if envelope.op_class == OpClass::Compaction
                && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
            {
                let _ = state.apply_compaction(&cp);
            }
            if envelope.op_class == OpClass::Compaction
                && let Some(metadata) = envelope.encryption
                && let Some(vault) = mls_vault
                && let Err(error) = vault.mint_snapshot_receipt(metadata.channel, &envelope)
            {
                tracing::warn!(%error, "encrypted snapshot was accepted but durable receipt minting failed");
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
            decrypted_payload: None,
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
                decrypted_payloads: Vec::new(),
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
) -> Result<collab_types::VersionedRoutedDocumentPayload, ValidationError> {
    collab_types::validate_document_payload_route_versioned(
        &route.app_id,
        &route.cluster_id,
        &route.record_id,
        envelope,
    )
    .map_err(|_| ValidationError::DocumentRouteMismatch)
}

fn spawn_inner(
    capnp_socket_path: PathBuf,
    peer_sync: Option<&Arc<ShellNetwork>>,
    publisher: &Arc<PeerPublisher>,
    membership: Option<crate::shell::state::WorkspaceTreeHandle>,
    certificate_provisioning: CertificateProvisioning,
    config: Option<&crate::shell::peer_actions::NativePeerBrokerConfig>,
) -> std::io::Result<BrokerHandle> {
    let table = Arc::new(FanoutState::new());
    let channels: Arc<Mutex<BrokerChannels>> = Arc::new(Mutex::new(BrokerChannels::default()));
    #[cfg(not(test))]
    let mut mls_authority = None;
    #[cfg(test)]
    let mls_authority = None;
    #[cfg(not(test))]
    if config.is_none() {
        match crate::shell::collab_mls_authority::SecretServiceMlsVault::open_for_session() {
            Ok(vault) => {
                let vault = Arc::new(vault);
                if let Ok(mut state) = channels.lock() {
                    state.install_mls_vault(vault.clone());
                }
                if let Some(workspaces) = membership.clone() {
                    match crate::shell::collab_mls_authority::MlsAuthorityCoordinator::open(
                        vault.authority_root(),
                        vault,
                        workspaces,
                        Arc::clone(&channels),
                    ) {
                        Ok(authority) => mls_authority = Some(Arc::new(Mutex::new(authority))),
                        Err(error) => tracing::warn!(%error, "MLS lifecycle authority unavailable"),
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "MLS collaboration unavailable: Secret Service provider was not installed");
            }
        }
    }
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

    let mut services = if config.is_none() {
        super::broker_services::native_services(membership.clone(), peer_sync.cloned())
    } else {
        qos_action_broker::services::HostServices::default()
    };
    if certificate_provisioning.uses_harness_devices() && config.is_none() {
        services.native_certificates = None;
    }
    let realtime_fabric = if config.is_none() {
        peer_sync.and_then(crate::shell::collab_broker_capnp::NativeRealtimeServices::from_network)
    } else {
        None
    };
    if let Some((service, _)) = realtime_fabric.as_ref() {
        services.realtime = Some(Arc::clone(service));
    }
    let authority = membership.as_ref().map(|tree| {
        Arc::new(super::broker_services::NativeWorkspaceAuthority(
            tree.clone(),
        )) as qos_action_broker::services::WorkspaceAuthorityHandle
    });
    let inner = match certificate_provisioning {
        CertificateProvisioning::Production => {
            qos_action_broker::collab_broker::spawn_with_services(
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
        CertificateProvisioning::HarnessDevices(trusted_devices) => {
            qos_action_broker::collab_broker::spawn_with_test_services(
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
    {
        if let Ok((queue, host)) = broker.peer_action_attachment() {
            let rooms = tree
                .read()
                .workspaces
                .iter()
                .filter_map(|w| uuid::Uuid::parse_str(&w.room.to_string()).ok())
                .collect();
            network
                .cmd_tx
                .try_send(crate::network::actor::NetCmd::AttachActionBroker { queue, host, rooms })
                .map_err(|_| {
                    std::io::Error::other("action broker network attachment unavailable")
                })?;
        }
    }
    Ok(BrokerHandle {
        inner,
        local_session: crate::shell::collab_broker_capnp::LocalSessionAuthority::new(),
        realtime_fabric: realtime_fabric.map(|(_, bridge)| bridge),
        drain_thread,
        drain_quit,
        mls_authority,
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

    #[test]
    fn allocated_application_peers_skip_reserved_ids_and_preserve_connection_counts() {
        let table = FanoutState::new();
        assert_eq!(table.connections_seen(), 0);
        assert_eq!(table.next_peer_id(), 1);
        assert_eq!(table.connections_seen(), 1);
        assert_eq!(table.next_peer_id(), 2);
        assert_eq!(table.connections_seen(), 2);
    }

    #[test]
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
            Ok(collab_types::VersionedRoutedDocumentPayload::Cleartext(
                collab_types::DecodedDocumentMutation {
                    version: collab_types::DocumentWireVersion::TiloV2,
                    mutation: DocumentMutation::Snapshot(data),
                }
            )) if data == b"whole document"
        ));
        let legacy = collab_types::DocumentWireVersion::QosV1;
        let mut legacy_envelope = collab_types::SignedOp::new_unsigned(
            DocumentMutation::Incremental(b"retained legacy edit".to_vec())
                .encode_for(legacy)
                .expect("legacy mutation"),
            collab_types::document_artifact_id_for_version(
                legacy,
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            envelope.author,
            collab_types::document_attestation_id_for_version(
                legacy,
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            OpClass::Splice,
            std::time::SystemTime::now(),
            2,
        );
        collab_types::sign_op(&mut legacy_envelope, &signing).expect("signed legacy edit");
        assert!(
            matches!(
                validate_document_route(&route, &legacy_envelope),
                Ok(collab_types::VersionedRoutedDocumentPayload::Cleartext(
                    collab_types::DecodedDocumentMutation {
                        version: collab_types::DocumentWireVersion::QosV1,
                        mutation: DocumentMutation::Incremental(data),
                    }
                )) if data == b"retained legacy edit"
            ),
            "authenticated receive preserves a retained legacy route"
        );
        legacy_envelope.attestation_id = collab_types::document_attestation_id(
            &route.app_id,
            &route.cluster_id,
            &route.record_id,
        );
        collab_types::sign_op(&mut legacy_envelope, &signing)
            .expect("re-signed mixed namespace edit");
        assert_eq!(
            validate_document_route(&route, &legacy_envelope),
            Err(ValidationError::DocumentRouteMismatch),
            "authenticated receive still rejects a mixed route namespace",
        );
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
    fn remote_document_broker_routes_opaque_mls_only_for_bound_channel() {
        let route = CrdtRoute::record("dev.qos.notes", "room-1", "record-1");
        let expected_channel =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record_id);
        let (_, author) = collab_types::generate_keypair();
        let mut envelope = collab_types::SignedOp::new_unsigned_with_encryption(
            b"opaque-not-a-document-mutation".to_vec(),
            document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id),
            author,
            document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id),
            OpClass::Splice,
            std::time::SystemTime::now(),
            1,
            collab_types::EncryptionMetadata {
                epoch: 9,
                channel: expected_channel,
            },
        );
        assert_eq!(
            validate_document_route(&route, &envelope),
            Ok(collab_types::VersionedRoutedDocumentPayload::Encrypted {
                channel: expected_channel,
                epoch: 9,
            })
        );
        envelope.encryption.as_mut().expect("metadata").channel = collab_types::ChannelId::new();
        assert_eq!(
            validate_document_route(&route, &envelope),
            Err(ValidationError::DocumentRouteMismatch)
        );

        let legacy = collab_types::DocumentWireVersion::QosV1;
        let legacy_channel = collab_types::document_channel_id_for_version(
            legacy,
            &route.app_id,
            &route.cluster_id,
            &route.record_id,
        );
        let mut legacy_envelope = collab_types::SignedOp::new_unsigned_with_encryption(
            b"opaque-legacy-mls-frame".to_vec(),
            collab_types::document_artifact_id_for_version(
                legacy,
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            author,
            collab_types::document_attestation_id_for_version(
                legacy,
                &route.app_id,
                &route.cluster_id,
                &route.record_id,
            ),
            OpClass::Splice,
            std::time::SystemTime::now(),
            2,
            collab_types::EncryptionMetadata {
                epoch: 10,
                channel: legacy_channel,
            },
        );
        assert_eq!(
            validate_document_route(&route, &legacy_envelope),
            Ok(collab_types::VersionedRoutedDocumentPayload::Encrypted {
                channel: legacy_channel,
                epoch: 10,
            })
        );
        legacy_envelope
            .encryption
            .as_mut()
            .expect("legacy encryption metadata")
            .channel = expected_channel;
        assert_eq!(
            validate_document_route(&route, &legacy_envelope),
            Err(ValidationError::DocumentRouteMismatch),
            "a retained legacy artifact cannot select the current governance channel",
        );
    }

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
        let dispatcher = super::SessionFollowDispatcher::new(Arc::clone(&table));
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
}
