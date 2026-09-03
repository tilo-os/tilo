//! Iroh-gossip actor for workspace collaboration.
//!
//! Direct paths, NAT traversal and stateless relay fallback are selected by
//! iroh. qOS only names authenticated endpoint ids and workspace topics.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::address_lookup::memory::MemoryLookup;
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::{Event as GossipEvent, GossipSender};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};
use iroh_gossip::proto::TopicId;
use qos_relay::{Inbound as HostInbound, RelayClient, RelayCoeditSession};
use qos_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::network::protocol::{MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg};

#[cfg(test)]
static DEDICATED_SURFACE_FRAMES_RECEIVED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
const EVICTION_INTERVAL: Duration = Duration::from_secs(1);
const HOST_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(600);
const MAX_GOSSIP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SURFACE_FRAME_BYTES: usize = 64 * 1024 * 1024;
const SURFACE_STREAM_ALPN: &[u8] = b"qos-surface-stream/1";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SurfaceWireFrame {
    workspace: WorkspaceId,
    message: NeighbourMsg,
}

pub type WorkspaceId = u64;
pub type PeerId = EndpointId;

/// Enrollment-bound identity expected behind a distinct relay service key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePeerExpectation {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
}

/// Device certificate chain used to authenticate to a qOS workspace host.
#[derive(Clone)]
pub struct HostIdentity {
    pub device_cert: DeviceCert,
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// Workspace-specific user certificates for this same device key.
    /// Missing entries deliberately fall back to `device_cert` for legacy and
    /// unbound workspaces.
    pub workspace_device_certs: HashMap<WorkspaceId, DeviceCert>,
    /// Root-signed revocations advertised beside each workspace certificate.
    pub workspace_revocations: HashMap<WorkspaceId, Vec<qos_user_identity::RevocationEntry>>,
    /// System device-key/certificate paths. Production actors use these to
    /// refresh workspace identity material on every heartbeat; tests may use
    /// the in-memory maps alone.
    pub identity_paths: Option<qos_user_identity::IdentityPaths>,
}

impl HostIdentity {
    pub(crate) fn material_for(
        &self,
        ws: WorkspaceId,
    ) -> (DeviceCert, Vec<qos_user_identity::RevocationEntry>) {
        let fallback_cert = self
            .workspace_device_certs
            .get(&ws)
            .cloned()
            .unwrap_or_else(|| self.device_cert.clone());
        let fallback_revocations = self
            .workspace_revocations
            .get(&ws)
            .cloned()
            .unwrap_or_default();
        let Some(paths) = self.identity_paths.as_ref() else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(registry_store) = crate::identity_registry::IdentityRegistryStore::from_env() else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(registry) = registry_store.migrate_legacy_primary() else {
            return (fallback_cert, fallback_revocations);
        };
        let selected = registry.workspace_identities.iter().find_map(|(room, id)| {
            let room = crate::shell::state::RoomId::parse(room)?;
            (crate::shell::network::room_topic_id(room) == ws).then_some(id)
        });
        let identity_id = selected
            .cloned()
            .or_else(|| registry.default_identity.clone());
        let Some(identity_id) = identity_id else {
            return (fallback_cert, fallback_revocations);
        };
        let Some(profile) = registry.identities.get(&identity_id) else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(cert) = qos_user_identity::persist::read_device_cert(
            &paths.device_cert_for_user(&profile.user_public_key),
        ) else {
            return (fallback_cert, fallback_revocations);
        };
        if qos_user_identity::verify_device_cert(&cert).is_err()
            || cert.user_public_key != profile.user_public_key
            || cert.device_public_key != self.device_signing_key.verifying_key().to_bytes()
        {
            return (fallback_cert, fallback_revocations);
        }
        let revocations =
            crate::device_fleet::DeviceFleetStore::at(registry_store.identity_dir(), paths.clone())
                .load(&identity_id)
                .map_or_else(|_| fallback_revocations, |fleet| fleet.revocations);
        (cert, revocations)
    }
}

#[derive(Debug)]
pub enum NetCmd {
    JoinWorkspace {
        ws: WorkspaceId,
        on_event: mpsc::Sender<NetEvent>,
    },
    LeaveWorkspace {
        ws: WorkspaceId,
    },
    Publish {
        ws: WorkspaceId,
        msg: NeighbourMsg,
    },
    /// Publish a shell-tree message that is known to contain the complete
    /// room history. Only the deliberate `HostReady` baseline uses this;
    /// ordinary `SyncResp` messages may be partial anti-entropy responses.
    PublishCheckpoint {
        ws: WorkspaceId,
        msg: NeighbourMsg,
    },
    Connect {
        addr: EndpointAddr,
    },
    /// Connect to an always-on qOS host. Messages continue to use the
    /// direct gossip path too; the host adds persistence and offline delivery.
    ConnectHost {
        addr: EndpointAddr,
        expectation: Option<HomePeerExpectation>,
    },
    /// Stop using a host removed from the authenticated local registry.
    DisconnectHost {
        id: EndpointId,
    },
}

#[derive(Debug, Clone)]
pub enum NetEvent {
    PeerJoined {
        ws: WorkspaceId,
        peer: PeerId,
    },
    PeerLeft {
        ws: WorkspaceId,
        peer: PeerId,
        cause: PeerDepartureCause,
    },
    /// An authenticated durable host finished subscribing to this workspace.
    /// Distinct from a direct peer arrival: the shell uses this edge to seed
    /// transport-level checkpoints for future first-time/offline devices.
    HostReady {
        ws: WorkspaceId,
        peer: PeerId,
    },
    Msg {
        ws: WorkspaceId,
        peer: PeerId,
        msg: NeighbourMsg,
    },
}

/// Why the transport stopped treating a peer as live on a workspace topic.
///
/// This is transport evidence, not a statement about the person's intent:
/// a timeout can be an unreliable network just as easily as a closed app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDepartureCause {
    /// Iroh gossip reported that the neighbour went away.
    GossipNeighborDown,
    /// A durable host's relay session closed.
    HostSessionClosed,
    /// The peer stopped refreshing its liveness heartbeat.
    LivenessTimedOut,
}

enum InternalEvent {
    Topic {
        ws: WorkspaceId,
        event: GossipEvent,
    },
    Host {
        ws: WorkspaceId,
        peer: EndpointId,
        event: HostEvent,
    },
    HostConnected {
        ws: WorkspaceId,
        peer: EndpointId,
        addr: EndpointAddr,
        // Boxed: `RelayCoeditSession` (an authenticated relay client plus
        // its subscription state) is far larger than every sibling variant,
        // so an unboxed `Result` here would size the whole enum to fit it.
        result: Box<Result<RelayCoeditSession, String>>,
    },
    SurfaceFrame {
        ws: WorkspaceId,
        peer: EndpointId,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    },
    Closed {
        ws: WorkspaceId,
    },
}

enum HostEvent {
    Ready,
    Message {
        author: EndpointId,
        fingerprint: [u8; 32],
        msg: NeighbourMsg,
    },
    Closed,
}

struct TopicRuntime {
    sender: GossipSender,
    pump: tokio::task::JoinHandle<()>,
}

struct HostRuntime {
    session: Arc<RelayCoeditSession>,
    pump: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct SurfaceStreamProtocol {
    internal_tx: mpsc::Sender<InternalEvent>,
}

impl std::fmt::Debug for SurfaceStreamProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SurfaceStreamProtocol")
    }
}

impl ProtocolHandler for SurfaceStreamProtocol {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let peer = connection.remote_id();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            loop {
                let mut recv = match connection.accept_uni().await {
                    Ok(recv) => recv,
                    Err(_) => return,
                };
                let bytes = match recv.read_to_end(MAX_SURFACE_FRAME_BYTES).await {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::debug!(peer = %peer, %error, "surface frame stream read failed");
                        continue;
                    }
                };
                let Ok(frame) = postcard::from_bytes::<SurfaceWireFrame>(&bytes) else {
                    tracing::warn!(peer = %peer, "invalid dedicated surface frame");
                    continue;
                };
                if !matches!(frame.message, NeighbourMsg::SurfaceFrame { .. }) {
                    tracing::warn!(peer = %peer, "non-frame message on surface stream ALPN");
                    continue;
                }
                #[cfg(test)]
                DEDICATED_SURFACE_FRAMES_RECEIVED
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Ok(message_bytes) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                let fingerprint = message_fingerprint(&message_bytes);
                if tx
                    .send(InternalEvent::SurfaceFrame {
                        ws: frame.workspace,
                        peer,
                        fingerprint,
                        msg: frame.message,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        Ok(())
    }
}

pub struct Actor;

impl Actor {
    pub fn spawn(
        endpoint: Endpoint,
        lookup: MemoryLookup,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_reporting_addrs(endpoint, lookup, None, Arc::new(Mutex::new(Vec::new())))
    }

    pub fn spawn_reporting_addrs(
        endpoint: Endpoint,
        lookup: MemoryLookup,
        host_identity: Option<HostIdentity>,
        listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        if let Ok(mut out) = listen_addrs.lock() {
            out.push(endpoint.addr());
        }
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let task = tokio::spawn(run(endpoint, lookup, host_identity, listen_addrs, cmd_rx));
        (cmd_tx, task)
    }
}

/// Mutable state shared across every `select!` branch of [`run`]'s event
/// loop, bundled into a struct so each branch's handling can be split into
/// its own method instead of one long function body.
struct ActorState {
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    gossip: Gossip,
    internal_tx: mpsc::Sender<InternalEvent>,
    topics: HashMap<WorkspaceId, TopicRuntime>,
    subscribers: HashMap<WorkspaceId, mpsc::Sender<NetEvent>>,
    peers: HashMap<EndpointId, EndpointAddr>,
    hosts: HashMap<EndpointId, (EndpointAddr, Option<HomePeerExpectation>)>,
    host_sessions: HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    connecting_hosts: HashSet<(WorkspaceId, EndpointId)>,
    content_discovery_tasks: HashMap<(WorkspaceId, EndpointId), tokio::task::JoinHandle<()>>,
    peers_seen: HashMap<(WorkspaceId, EndpointId), Instant>,
    recent: HashSet<(WorkspaceId, [u8; 32])>,
    recent_order: VecDeque<(WorkspaceId, [u8; 32])>,
    /// Long-lived direct media connections. One unidirectional QUIC stream
    /// per frame avoids head-of-line blocking between viewers and frames.
    surface_connections: HashMap<EndpointId, iroh::endpoint::Connection>,
}

impl ActorState {
    /// Handle one [`NetCmd`]. Returns `false` once the command channel has
    /// closed, so [`run`]'s loop should stop.
    async fn handle_command(&mut self, command: Option<NetCmd>) -> bool {
        match command {
            Some(NetCmd::JoinWorkspace { ws, on_event }) => {
                self.handle_join_workspace(ws, on_event).await;
            }
            Some(NetCmd::LeaveWorkspace { ws }) => {
                self.subscribers.remove(&ws);
                self.peers_seen.retain(|(peer_ws, _), _| *peer_ws != ws);
                if let Some(topic) = self.topics.remove(&ws) {
                    topic.pump.abort();
                }
                let leaving: Vec<_> = self
                    .host_sessions
                    .keys()
                    .filter(|(session_ws, _)| *session_ws == ws)
                    .copied()
                    .collect();
                for key in leaving {
                    if let Some(runtime) = self.host_sessions.remove(&key) {
                        runtime.pump.abort();
                    }
                }
                let discoveries: Vec<_> = self
                    .content_discovery_tasks
                    .keys()
                    .filter(|(task_ws, _)| *task_ws == ws)
                    .copied()
                    .collect();
                for key in discoveries {
                    if let Some(task) = self.content_discovery_tasks.remove(&key) {
                        task.abort();
                    }
                }
            }
            Some(NetCmd::Publish { ws, msg }) => {
                let gossip_fallback = if matches!(msg, NeighbourMsg::SurfaceFrame { .. }) {
                    !self.publish_surface_direct(ws, &msg).await
                } else {
                    true
                };
                publish(
                    &self.topics,
                    &self.host_sessions,
                    ws,
                    msg,
                    false,
                    gossip_fallback,
                )
                .await;
            }
            Some(NetCmd::PublishCheckpoint { ws, msg }) => {
                publish(&self.topics, &self.host_sessions, ws, msg, true, true).await;
            }
            Some(NetCmd::Connect { addr }) => {
                let id = addr.id;
                self.lookup.add_endpoint_info(addr.clone());
                self.peers.insert(id, addr);
                for topic in self.topics.values() {
                    if let Err(error) = topic.sender.join_peers(vec![id]).await {
                        tracing::warn!(peer = %id, %error, "iroh gossip peer join failed");
                    }
                }
            }
            Some(NetCmd::ConnectHost { addr, expectation }) => {
                let id = addr.id;
                let changed = self
                    .hosts
                    .get(&id)
                    .is_some_and(|known| known != &(addr.clone(), expectation.clone()));
                if changed {
                    // A new address for the same certified service replaces
                    // only its relay session. Its already authenticated Files
                    // endpoint remains usable until refreshed discovery proves
                    // a replacement; treating a reconnect as removal creates
                    // a needless offline window (and can strand hydration if
                    // the new relay hint is temporarily unreachable).
                    self.teardown_host(id, false);
                }
                self.lookup.add_endpoint_info(addr.clone());
                self.hosts.insert(id, (addr.clone(), expectation.clone()));
                let Some(identity) = self.host_identity.as_ref() else {
                    tracing::warn!(peer = %id, "cannot connect workspace host without a device certificate");
                    return true;
                };
                for ws in self.topics.keys().copied() {
                    schedule_host_topic(
                        &self.endpoint,
                        identity,
                        ws,
                        addr.clone(),
                        expectation.clone(),
                        &self.internal_tx,
                        &self.host_sessions,
                        &mut self.connecting_hosts,
                        &mut self.content_discovery_tasks,
                    );
                }
            }
            Some(NetCmd::DisconnectHost { id }) => self.teardown_host(id, true),
            None => return false,
        }
        true
    }

    /// Stop relay sessions and discovery tasks for one host.  `remove_content`
    /// is reserved for an explicit enrolled-home-peer removal: an address
    /// refresh must never revoke a still-valid Files hydration source.
    fn teardown_host(&mut self, id: EndpointId, remove_content: bool) {
        let enrolled_home_peer = self
            .hosts
            .remove(&id)
            .is_some_and(|(_, expectation)| expectation.is_some());
        self.connecting_hosts.retain(|(_, host)| *host != id);
        let sessions: Vec<_> = self
            .host_sessions
            .keys()
            .filter(|(_, host)| *host == id)
            .copied()
            .collect();
        for key in sessions {
            if let Some(runtime) = self.host_sessions.remove(&key) {
                runtime.pump.abort();
            }
        }
        let discoveries: Vec<_> = self
            .content_discovery_tasks
            .keys()
            .filter(|(_, host)| *host == id)
            .copied()
            .collect();
        for key in discoveries {
            if let Some(task) = self.content_discovery_tasks.remove(&key) {
                task.abort();
            }
        }
        if remove_content && enrolled_home_peer {
            // Discovery registrations are tied to the relay service identity,
            // not merely the direct Files endpoint.  Tear down that durable
            // route too: otherwise a removed home peer can return after an
            // fs-daemon restart from its persisted hydration catalog.
            tokio::spawn(async move {
                let _ = unregister_content_peers_for_relay(id).await;
            });
        }
    }

    /// Send an encoded surface frame over the media ALPN to every directly
    /// connected participant in this workspace. Returns true only when every
    /// currently-known direct participant was reached; otherwise the caller
    /// retains gossip as a compatibility fallback. The workspace host is
    /// still published to separately.
    async fn publish_surface_direct(&mut self, ws: WorkspaceId, msg: &NeighbourMsg) -> bool {
        let wire = SurfaceWireFrame {
            workspace: ws,
            message: msg.clone(),
        };
        let Ok(bytes) = postcard::to_allocvec(&wire) else {
            return false;
        };
        if bytes.len() > MAX_SURFACE_FRAME_BYTES {
            tracing::warn!(ws, bytes = bytes.len(), "surface frame exceeds wire limit");
            return false;
        }
        let workspace_peer_count = self
            .peers_seen
            .keys()
            .filter(|(peer_ws, _)| *peer_ws == ws)
            .count();
        let targets: Vec<_> = self
            .peers
            .values()
            .filter(|addr| self.peers_seen.contains_key(&(ws, addr.id)))
            .cloned()
            .collect();
        let mut sent = 0usize;
        for addr in targets {
            let peer = addr.id;
            let connection = if let Some(connection) = self.surface_connections.get(&peer) {
                connection.clone()
            } else {
                let Ok(Ok(connection)) = tokio::time::timeout(
                    Duration::from_millis(300),
                    self.endpoint.connect(addr, SURFACE_STREAM_ALPN),
                )
                .await
                else {
                    continue;
                };
                self.surface_connections.insert(peer, connection.clone());
                connection
            };
            let send = async {
                let mut stream = connection.open_uni().await.map_err(|_| ())?;
                stream.write_all(&bytes).await.map_err(|_| ())?;
                stream.finish().map_err(|_| ())?;
                Ok::<(), ()>(())
            };
            match tokio::time::timeout(Duration::from_millis(40), send).await {
                Ok(Ok(())) => sent = sent.saturating_add(1),
                _ => {
                    self.surface_connections.remove(&peer);
                }
            }
        }
        workspace_peer_count > 0 && sent == workspace_peer_count
    }

    /// [`NetCmd::JoinWorkspace`] arm of [`Self::handle_command`], split out
    /// to keep that dispatcher short.
    async fn handle_join_workspace(&mut self, ws: WorkspaceId, on_event: mpsc::Sender<NetEvent>) {
        self.subscribers.insert(ws, on_event);
        if let std::collections::hash_map::Entry::Vacant(e) = self.topics.entry(ws) {
            let bootstrap = self.peers.keys().copied().collect();
            match self.gossip.subscribe(topic_id(ws), bootstrap).await {
                Ok(topic) => {
                    let (sender, mut receiver) = topic.split();
                    let tx = self.internal_tx.clone();
                    let pump = tokio::spawn(async move {
                        while let Some(result) = receiver.next().await {
                            match result {
                                Ok(event) => {
                                    if tx.send(InternalEvent::Topic { ws, event }).await.is_err() {
                                        return;
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(ws, %error, "iroh gossip subscription failed");
                                    break;
                                }
                            }
                        }
                        let _ = tx.send(InternalEvent::Closed { ws }).await;
                    });
                    e.insert(TopicRuntime { sender, pump });
                }
                Err(error) => tracing::warn!(ws, %error, "could not join iroh workspace topic"),
            }
        }
        if let Some(identity) = self.host_identity.as_ref() {
            for (addr, expectation) in self.hosts.values().cloned() {
                schedule_host_topic(
                    &self.endpoint,
                    identity,
                    ws,
                    addr.clone(),
                    expectation,
                    &self.internal_tx,
                    &self.host_sessions,
                    &mut self.connecting_hosts,
                    &mut self.content_discovery_tasks,
                );
            }
        }
    }

    /// Handle one [`InternalEvent`]. Returns `false` once the internal
    /// channel has closed, so [`run`]'s loop should stop.
    async fn handle_internal(&mut self, internal: Option<InternalEvent>) -> bool {
        match internal {
            Some(InternalEvent::Topic { ws, event }) => {
                self.handle_topic_event(ws, event).await;
            }
            Some(InternalEvent::Closed { ws }) => {
                self.topics.remove(&ws);
            }
            Some(InternalEvent::HostConnected {
                ws,
                peer,
                addr,
                result,
            }) => {
                self.handle_host_connected(ws, peer, &addr, result);
            }
            Some(InternalEvent::Host { ws, peer, event }) => {
                self.handle_host_event(ws, peer, event).await;
            }
            Some(InternalEvent::SurfaceFrame {
                ws,
                peer,
                fingerprint,
                msg,
            }) => {
                // A bare ALPN connection is not workspace admission. Require
                // the peer to have appeared on this workspace's authenticated
                // gossip topic first; ShellNetwork performs the capability
                // and claimed-publisher checks before dispatch.
                if !self.peers_seen.contains_key(&(ws, peer)) {
                    tracing::warn!(ws, peer = %peer, "surface frame from unknown workspace peer");
                    return true;
                }
                self.peers_seen.insert((ws, peer), Instant::now());
                if mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order)
                    && let Some(tx) = self.subscribers.get(&ws)
                {
                    let _ = tx.send(NetEvent::Msg { ws, peer, msg }).await;
                }
            }
            None => return false,
        }
        true
    }

    /// [`InternalEvent::Topic`] arm of [`Self::handle_internal`], split out
    /// to keep that dispatcher short.
    async fn handle_topic_event(&mut self, ws: WorkspaceId, event: GossipEvent) {
        if matches!(event, GossipEvent::NeighborUp(_)) {
            // Authenticate this device as soon as the link is usable. State
            // sync can otherwise arrive before the periodic identity
            // announcement and be rejected by the peer.
            self.announce_identity(ws).await;
        }
        let Some(tx) = self.subscribers.get(&ws) else {
            return;
        };
        let event = match event {
            GossipEvent::NeighborUp(peer) => {
                self.peers_seen.insert((ws, peer), Instant::now());
                Some(NetEvent::PeerJoined { ws, peer })
            }
            GossipEvent::NeighborDown(peer) => {
                self.peers_seen.remove(&(ws, peer));
                Some(NetEvent::PeerLeft {
                    ws,
                    peer,
                    cause: PeerDepartureCause::GossipNeighborDown,
                })
            }
            GossipEvent::Received(message) => {
                let peer = message.delivered_from;
                let rediscovered = self.peers_seen.insert((ws, peer), Instant::now()).is_none();
                let fingerprint = message_fingerprint(&message.content);
                if !mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order) {
                    return;
                }
                match postcard::from_bytes::<NeighbourMsg>(&message.content) {
                    Ok(NeighbourMsg::Heartbeat { .. }) if rediscovered => {
                        Some(NetEvent::PeerJoined { ws, peer })
                    }
                    Ok(NeighbourMsg::Heartbeat { .. }) => None,
                    Ok(msg) => Some(NetEvent::Msg { ws, peer, msg }),
                    Err(error) => {
                        tracing::warn!(ws, %error, "invalid collaboration message");
                        None
                    }
                }
            }
            GossipEvent::Lagged => {
                tracing::warn!(
                    ws,
                    "iroh gossip receiver lagged; anti-entropy will recover durable state"
                );
                None
            }
        };
        if let Some(event) = event {
            let _ = tx.send(event).await;
        }
    }

    /// [`InternalEvent::HostConnected`] arm of [`Self::handle_internal`],
    /// split out to keep that dispatcher short.
    fn handle_host_connected(
        &mut self,
        ws: WorkspaceId,
        peer: EndpointId,
        attempted_addr: &EndpointAddr,
        result: Box<Result<RelayCoeditSession, String>>,
    ) {
        self.connecting_hosts.remove(&(ws, peer));
        if !self.topics.contains_key(&ws)
            || !self
                .hosts
                .get(&peer)
                .is_some_and(|(current, _)| current == attempted_addr)
        {
            return;
        }
        match *result {
            Ok(session) => {
                let session = Arc::new(session);
                let pump =
                    spawn_host_pump(ws, peer, Arc::clone(&session), self.internal_tx.clone());
                self.host_sessions
                    .insert((ws, peer), HostRuntime { session, pump });
            }
            Err(error) => {
                tracing::warn!(ws, peer = %peer, %error, "workspace host connection failed");
            }
        }
    }

    /// [`InternalEvent::Host`] arm of [`Self::handle_internal`], split out
    /// to keep that dispatcher short.
    async fn handle_host_event(&mut self, ws: WorkspaceId, peer: EndpointId, event: HostEvent) {
        let mapped = match event {
            HostEvent::Ready => Some(NetEvent::HostReady { ws, peer }),
            HostEvent::Message {
                author,
                fingerprint,
                msg,
            } => {
                if !mark_new_message(ws, fingerprint, &mut self.recent, &mut self.recent_order) {
                    return;
                }
                Some(NetEvent::Msg {
                    ws,
                    peer: author,
                    msg,
                })
            }
            HostEvent::Closed => {
                if let Some(runtime) = self.host_sessions.remove(&(ws, peer)) {
                    runtime.pump.abort();
                }
                Some(NetEvent::PeerLeft {
                    ws,
                    peer,
                    cause: PeerDepartureCause::HostSessionClosed,
                })
            }
        };
        if let (Some(event), Some(tx)) = (mapped, self.subscribers.get(&ws)) {
            let _ = tx.send(event).await;
        }
    }

    /// Drop peers that have gone quiet for longer than [`PEER_TIMEOUT`].
    async fn run_eviction_tick(&mut self) {
        let now = Instant::now();
        let stale: Vec<_> = self
            .peers_seen
            .iter()
            .filter_map(|(&(ws, peer), seen)| {
                (now.duration_since(*seen) > PEER_TIMEOUT).then_some((ws, peer))
            })
            .collect();
        for (ws, peer) in stale {
            self.peers_seen.remove(&(ws, peer));
            if let Some(tx) = self.subscribers.get(&ws) {
                let _ = tx
                    .send(NetEvent::PeerLeft {
                        ws,
                        peer,
                        cause: PeerDepartureCause::LivenessTimedOut,
                    })
                    .await;
            }
        }
    }

    /// Refresh the advertised listen address, retry any workspace hosts not
    /// yet connected, and broadcast this device's identity + a liveness
    /// heartbeat on every joined topic.
    async fn run_heartbeat_tick(&mut self) {
        if let Ok(mut out) = self.listen_addrs.lock() {
            out.clear();
            out.push(self.endpoint.addr());
        }
        if let Some(identity) = self.host_identity.as_ref() {
            for ws in self.topics.keys().copied() {
                for (addr, expectation) in self.hosts.values().cloned() {
                    schedule_host_topic(
                        &self.endpoint,
                        identity,
                        ws,
                        addr,
                        expectation,
                        &self.internal_tx,
                        &self.host_sessions,
                        &mut self.connecting_hosts,
                        &mut self.content_discovery_tasks,
                    );
                }
            }
        }
        let workspaces: Vec<_> = self.topics.keys().copied().collect();
        for ws in workspaces {
            self.announce_identity(ws).await;
        }
        let msg = NeighbourMsg::Heartbeat {
            peer: crate::network::behaviour::peer_liveness_handle(&self.endpoint.id()),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        };
        if let Ok(bytes) = postcard::to_allocvec(&msg) {
            for topic in self.topics.values() {
                let _ = topic.sender.broadcast(Bytes::from(bytes.clone())).await;
            }
        }
    }

    /// Send this device's certificate to its direct neighbors on `ws`.
    ///
    /// Identity announcements are deliberately neighbor-only: receivers bind
    /// the certificate to the endpoint that delivered it, so forwarding one
    /// through another gossip peer would make that check fail.
    async fn announce_identity(&self, ws: WorkspaceId) {
        let (Some(identity), Some(topic)) = (self.host_identity.as_ref(), self.topics.get(&ws))
        else {
            return;
        };
        let (cert, revocations) = identity.material_for(ws);
        let hello = NeighbourMsg::IdentityHello {
            cert: Box::new(cert),
            revocations,
        };
        let bytes = match postcard::to_allocvec(&hello) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(ws, %error, "could not encode collaboration identity announcement");
                return;
            }
        };
        if let Err(error) = topic.sender.broadcast_neighbors(Bytes::from(bytes)).await {
            tracing::warn!(ws, %error, "could not send collaboration identity announcement");
        }
    }

    /// Re-announce every live workspace-host session so a late-joining
    /// subscriber still sees `HostReady`.
    async fn run_host_checkpoint_tick(&self) {
        for &(ws, peer) in self.host_sessions.keys() {
            if let Some(tx) = self.subscribers.get(&ws) {
                let _ = tx.send(NetEvent::HostReady { ws, peer }).await;
            }
        }
    }
}

async fn run(
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    mut cmd_rx: mpsc::Receiver<NetCmd>,
) {
    let (internal_tx, mut internal_rx) = mpsc::channel(256);
    let gossip = Gossip::builder()
        .max_message_size(MAX_GOSSIP_MESSAGE_BYTES)
        .spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(
            SURFACE_STREAM_ALPN,
            SurfaceStreamProtocol {
                internal_tx: internal_tx.clone(),
            },
        )
        .spawn();
    let mut state = ActorState {
        endpoint,
        lookup,
        host_identity,
        listen_addrs,
        gossip,
        internal_tx,
        topics: HashMap::new(),
        subscribers: HashMap::new(),
        peers: HashMap::new(),
        hosts: HashMap::new(),
        host_sessions: HashMap::new(),
        connecting_hosts: HashSet::new(),
        content_discovery_tasks: HashMap::new(),
        peers_seen: HashMap::new(),
        recent: HashSet::new(),
        recent_order: VecDeque::new(),
        surface_connections: HashMap::new(),
    };
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    let mut eviction = tokio::time::interval(EVICTION_INTERVAL);
    let mut host_checkpoint = tokio::time::interval(HOST_CHECKPOINT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    eviction.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    host_checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await;
    eviction.tick().await;
    host_checkpoint.tick().await;

    loop {
        tokio::select! {
            command = cmd_rx.recv() => {
                if !state.handle_command(command).await {
                    break;
                }
            }
            internal = internal_rx.recv() => {
                if !state.handle_internal(internal).await {
                    break;
                }
            }
            _ = eviction.tick() => state.run_eviction_tick().await,
            _ = heartbeat.tick() => state.run_heartbeat_tick().await,
            _ = host_checkpoint.tick() => state.run_host_checkpoint_tick().await,
        }
    }

    for (_, topic) in state.topics {
        topic.pump.abort();
    }
    for (_, host) in state.host_sessions {
        host.pump.abort();
    }
    let _ = router.shutdown().await;
}

async fn publish(
    topics: &HashMap<WorkspaceId, TopicRuntime>,
    host_sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    ws: WorkspaceId,
    msg: NeighbourMsg,
    shell_checkpoint: bool,
    broadcast_gossip: bool,
) {
    let Some(topic) = topics.get(&ws) else { return };
    if let NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. } = &msg
        && snapshot.len() > MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES
    {
        tracing::warn!(
            ws,
            bytes = snapshot.len(),
            "personal continuity checkpoint exceeds transport limit"
        );
        return;
    }
    let bytes = match postcard::to_allocvec(&msg) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(ws, %error, "collaboration message encode failed");
            return;
        }
    };
    if broadcast_gossip && let Err(error) = topic.sender.broadcast(Bytes::from(bytes.clone())).await
    {
        tracing::warn!(ws, %error, "iroh gossip publish failed");
    }
    for ((session_ws, _), runtime) in host_sessions {
        if *session_ws != ws {
            continue;
        }
        let result = match retention(&msg, shell_checkpoint) {
            Some((channel, Retention::Checkpoint)) => {
                runtime.session.publish_checkpoint(channel, bytes.clone())
            }
            Some((channel, Retention::Delta)) => runtime
                .session
                .publish_retained_delta(channel, bytes.clone()),
            None => runtime.session.publish_presence(bytes.clone()),
        };
        if let Err(error) = result {
            tracing::warn!(ws, %error, "workspace host publish failed");
        }
    }
}

/// Only replayable state is allowed into the host's durable queue. Everything
/// else is live session traffic; persisting input, presence, media, permission
/// prompts or request/response control frames would make them execute late.
#[derive(Clone, Copy)]
enum Retention {
    Checkpoint,
    Delta,
}

fn retention(msg: &NeighbourMsg, shell_checkpoint: bool) -> Option<([u8; 32], Retention)> {
    let (route, kind) = match msg {
        NeighbourMsg::SyncResp { .. } => (
            b"shell-tree".as_slice(),
            if shell_checkpoint {
                Retention::Checkpoint
            } else {
                Retention::Delta
            },
        ),
        NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } => {
            let route = format!("app\0{app_id}\0{record_id}");
            let mutation = collab_types::DocumentMutation::decode(&envelope.payload).ok()?;
            let retention = match mutation {
                collab_types::DocumentMutation::Snapshot(_) => Retention::Checkpoint,
                collab_types::DocumentMutation::Incremental(_) => Retention::Delta,
            };
            return Some((*blake3::hash(route.as_bytes()).as_bytes(), retention));
        }
        NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key, ..
        } => {
            let mut route = Vec::with_capacity(20 + user_public_key.len());
            route.extend_from_slice(b"personal-continuity\0");
            route.extend_from_slice(user_public_key);
            return Some((*blake3::hash(&route).as_bytes(), Retention::Checkpoint));
        }
        _ => return None,
    };
    Some((*blake3::hash(route).as_bytes(), kind))
}

fn message_fingerprint(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn mark_new_message(
    ws: WorkspaceId,
    fingerprint: [u8; 32],
    recent: &mut HashSet<(WorkspaceId, [u8; 32])>,
    order: &mut VecDeque<(WorkspaceId, [u8; 32])>,
) -> bool {
    const MAX_RECENT: usize = 1024;
    let key = (ws, fingerprint);
    if !recent.insert(key) {
        return false;
    }
    order.push_back(key);
    if order.len() > MAX_RECENT
        && let Some(oldest) = order.pop_front()
    {
        recent.remove(&oldest);
    }
    true
}

fn schedule_host_topic(
    endpoint: &Endpoint,
    identity: &HostIdentity,
    ws: WorkspaceId,
    addr: EndpointAddr,
    expectation: Option<HomePeerExpectation>,
    internal_tx: &mpsc::Sender<InternalEvent>,
    sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    connecting: &mut HashSet<(WorkspaceId, EndpointId)>,
    content_discovery_tasks: &mut HashMap<(WorkspaceId, EndpointId), tokio::task::JoinHandle<()>>,
) {
    let host = addr.id;
    if let Some(expectation) = expectation.clone()
        && !content_discovery_tasks.contains_key(&(ws, host))
    {
        let discovery_endpoint = endpoint.clone();
        let discovery_addr = addr.clone();
        let discovery_identity = identity.clone();
        let task = tokio::spawn(async move {
            let mut retry = Duration::from_secs(1);
            loop {
                let (discovery_cert, entries) = discovery_identity.material_for(ws);
                let mut discovery_revocations = RevocationList::new();
                if entries
                    .into_iter()
                    .any(|entry| discovery_revocations.insert(entry).is_err())
                {
                    tracing::warn!(ws, "workspace identity has an invalid revocation entry");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
                let discovered = tokio::time::timeout(
                    Duration::from_secs(10),
                    RelayClient::discover_content_peer(
                        &discovery_endpoint,
                        discovery_addr.clone(),
                        ws,
                        expectation.user_public_key,
                        expectation.device_public_key,
                        discovery_cert.clone(),
                        discovery_identity.device_signing_key.clone(),
                        discovery_revocations.clone(),
                    ),
                )
                .await;
                match discovered {
                    Ok(Ok(content_peer)) => {
                        let _ = register_content_peer(content_peer).await;
                        // Re-advertise periodically. Registration persists the
                        // peer, while this also repairs a daemon replacement
                        // without coupling its lifecycle to the relay session.
                        retry = Duration::from_secs(30);
                    }
                    Ok(Err(error)) => {
                        tracing::debug!(%error, ws, "relay content discovery is not ready");
                        retry = retry.saturating_mul(2).min(Duration::from_secs(10));
                    }
                    Err(_) => {
                        tracing::debug!(ws, "relay content discovery timed out");
                        retry = retry.saturating_mul(2).min(Duration::from_secs(10));
                    }
                }
                tokio::time::sleep(retry).await;
            }
        });
        content_discovery_tasks.insert((ws, host), task);
    }
    if sessions.contains_key(&(ws, host)) || !connecting.insert((ws, host)) {
        return;
    }
    let endpoint = endpoint.clone();
    let identity = identity.clone();
    let (device_cert, revocation_entries) = identity.material_for(ws);
    let mut revocations = RevocationList::new();
    for entry in revocation_entries {
        if revocations.insert(entry).is_err() {
            tracing::warn!(ws, "workspace identity has an invalid revocation entry");
            connecting.remove(&(ws, host));
            return;
        }
    }
    let Some(expectation) = expectation else {
        // Invited/provider hosts can carry collaboration state, but only an
        // enrolled home peer may nominate a Files hydration endpoint.
        let tx = internal_tx.clone();
        tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                RelayClient::connect_to_pinned_host(
                    endpoint,
                    addr.clone(),
                    device_cert,
                    identity.device_signing_key.clone(),
                    revocations,
                ),
            )
            .await
            .map_err(|_| "workspace host connection timed out".to_string())
            .and_then(|result| result.map_err(|error| error.to_string()))
            .and_then(|client| {
                RelayCoeditSession::join(
                    client,
                    ws,
                    identity.device_signing_key.verifying_key().to_bytes(),
                )
                .map_err(|error| error.to_string())
            });
            let _ = tx
                .send(InternalEvent::HostConnected {
                    ws,
                    peer: host,
                    addr,
                    result: Box::new(result),
                })
                .await;
        });
        return;
    };
    let tx = internal_tx.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            RelayClient::connect_to_enrolled_home_peer(
                endpoint,
                addr.clone(),
                expectation.user_public_key,
                expectation.device_public_key,
                device_cert,
                identity.device_signing_key.clone(),
                revocations,
            ),
        )
        .await
        .map_err(|_| "workspace host connection timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|client| {
            RelayCoeditSession::join(
                client,
                ws,
                identity.device_signing_key.verifying_key().to_bytes(),
            )
            .map_err(|error| error.to_string())
        });
        let _ = tx
            .send(InternalEvent::HostConnected {
                ws,
                peer: host,
                addr,
                result: Box::new(result),
            })
            .await;
    });
}

/// Submit an authenticated relay-discovered Files peer to the running FUSE
/// daemon. This is deliberately a detached worker: endpoint registration may
/// wait on the group-restricted daemon socket but must never delay networking
/// or the compositor. The filesystem owns persistence and hydration policy.
async fn register_content_peer(registration: qos_relay::ContentPeerRegistration) -> bool {
    if cfg!(test) && std::env::var_os("QOS_FS_CONTENT_PEER_PUBLISHER").is_none() {
        return true;
    }
    let publisher = std::env::var_os("QOS_FS_CONTENT_PEER_PUBLISHER")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/usr/bin/fs"),
            std::path::PathBuf::from,
        );
    let cache_dir = std::env::var_os("QOS_FS_CONTENT_PEER_CACHE_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/var/cache/quick-fs"),
            std::path::PathBuf::from,
        );
    let Ok(payload) = serde_json::to_vec(&registration) else {
        tracing::warn!("could not serialize relay-discovered filesystem registration");
        return false;
    };
    let mut child = match tokio::process::Command::new(&publisher)
        .arg("register-content-peer")
        .arg("--cache-dir")
        .arg(&cache_dir)
        .arg("--endpoint-stdin")
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, publisher = %publisher.display(), "could not start filesystem content-peer registration");
            return false;
        }
    };
    use tokio::io::AsyncWriteExt as _;
    let Some(mut stdin) = child.stdin.take() else {
        tracing::warn!("filesystem content-peer registration has no stdin");
        return false;
    };
    if let Err(error) = stdin.write_all(&payload).await {
        tracing::warn!(%error, "could not send discovered endpoint to filesystem daemon");
        return false;
    }
    drop(stdin);
    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            tracing::warn!(?status, "filesystem content-peer registration failed");
            false
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "could not wait for filesystem content-peer registration");
            false
        }
        Err(_) => {
            tracing::warn!("filesystem content-peer registration timed out");
            false
        }
    }
}

/// Ask the running filesystem daemon to forget all endpoint/ticket pairs
/// authenticated by a removed enrolled relay. The daemon owns both its live
/// Hydrator and the durable catalog, so the shell cannot leave one stale.
async fn unregister_content_peers_for_relay(relay: EndpointId) -> bool {
    #[cfg(test)]
    noted_content_peer_removals()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(relay);
    if cfg!(test) && std::env::var_os("QOS_FS_CONTENT_PEER_PUBLISHER").is_none() {
        return true;
    }
    let publisher = std::env::var_os("QOS_FS_CONTENT_PEER_PUBLISHER")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/usr/bin/fs"),
            std::path::PathBuf::from,
        );
    let cache_dir = std::env::var_os("QOS_FS_CONTENT_PEER_CACHE_DIR")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || std::path::PathBuf::from("/var/cache/quick-fs"),
            std::path::PathBuf::from,
        );
    match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(&publisher)
            .arg("unregister-content-peer")
            .arg("--cache-dir")
            .arg(cache_dir)
            .arg("--relay-service-id")
            .arg(hex::encode(relay.as_bytes()))
            .status(),
    )
    .await
    {
        Ok(Ok(status)) if status.success() => true,
        Ok(Ok(status)) => {
            tracing::warn!(?status, relay = %relay, "filesystem content-peer removal failed");
            false
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, relay = %relay, "could not start filesystem content-peer removal");
            false
        }
        Err(_) => {
            tracing::warn!(relay = %relay, "filesystem content-peer removal timed out");
            false
        }
    }
}

#[cfg(test)]
fn noted_content_peer_removals() -> &'static Mutex<Vec<EndpointId>> {
    static REMOVALS: std::sync::OnceLock<Mutex<Vec<EndpointId>>> = std::sync::OnceLock::new();
    REMOVALS.get_or_init(|| Mutex::new(Vec::new()))
}

fn spawn_host_pump(
    ws: WorkspaceId,
    host: EndpointId,
    session: Arc<RelayCoeditSession>,
    tx: mpsc::Sender<InternalEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(inbound) = session.recv().await {
            let event = match inbound {
                HostInbound::Ready { .. } => HostEvent::Ready,
                HostInbound::Delta { author, data }
                | HostInbound::Presence {
                    author,
                    payload: data,
                } => {
                    let Ok(author) = EndpointId::from_bytes(&author) else {
                        continue;
                    };
                    let fingerprint = message_fingerprint(&data);
                    let Ok(msg) = postcard::from_bytes(&data) else {
                        continue;
                    };
                    HostEvent::Message {
                        author,
                        fingerprint,
                        msg,
                    }
                }
            };
            if tx
                .send(InternalEvent::Host {
                    ws,
                    peer: host,
                    event,
                })
                .await
                .is_err()
            {
                return;
            }
        }
        let _ = tx
            .send(InternalEvent::Host {
                ws,
                peer: host,
                event: HostEvent::Closed,
            })
            .await;
    })
}

fn topic_id(ws: WorkspaceId) -> TopicId {
    let mut hasher = blake3::Hasher::new_derive_key("qos-workspace-gossip-v1");
    hasher.update(&ws.to_le_bytes());
    TopicId::from_bytes(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::endpoint::presets;
    use qos_relay::{
        Relay, RelayConfig, ReplicationIntent, WorkspaceBinding, daemon::RelayIdentity,
    };
    use qos_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };
    use tokio::time::timeout;

    fn document(mutation: &collab_types::DocumentMutation) -> NeighbourMsg {
        let envelope = collab_types::SignedOp::new_unsigned(
            mutation.encode().expect("typed mutation"),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([1; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        NeighbourMsg::AppDocument {
            peer: 1,
            app_id: "app".into(),
            cluster_id: "room".into(),
            record_id: "doc".into(),
            envelope: Box::new(envelope),
        }
    }

    #[test]
    fn host_persists_state_but_never_late_session_actions() {
        assert!(matches!(
            retention(
                &NeighbourMsg::SyncResp {
                    changes: vec![vec![1]],
                    room: "room".into(),
                },
                true,
            ),
            Some((_, Retention::Checkpoint))
        ));
        assert!(matches!(
            retention(
                &NeighbourMsg::SyncResp {
                    changes: vec![vec![1]],
                    room: "room".into(),
                },
                false,
            ),
            Some((_, Retention::Delta))
        ));
        assert!(matches!(
            retention(
                &document(&collab_types::DocumentMutation::Snapshot(vec![2])),
                false,
            ),
            Some((_, Retention::Checkpoint))
        ));
        assert!(matches!(
            retention(
                &document(&collab_types::DocumentMutation::Incremental(vec![2])),
                false,
            ),
            Some((_, Retention::Delta))
        ));
        let root_a = [0xA1; 32];
        let root_b = [0xB2; 32];
        let checkpoint_a = NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: root_a,
            publisher: 1,
            snapshot: vec![7],
        };
        let checkpoint_b = NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: root_b,
            publisher: 2,
            snapshot: vec![8],
        };
        let Some((channel_a, Retention::Checkpoint)) = retention(&checkpoint_a, false) else {
            panic!("personal continuity checkpoints must be retained baselines");
        };
        let Some((channel_b, Retention::Checkpoint)) = retention(&checkpoint_b, false) else {
            panic!("personal continuity checkpoints must be retained baselines");
        };
        assert_ne!(
            channel_a, channel_b,
            "each user root has an isolated host channel"
        );
        assert!(
            retention(
                &NeighbourMsg::PersonalContinuityRequest {
                    user_public_key: root_a,
                    requester: 1,
                },
                false,
            )
            .is_none(),
            "requests are live probes, never retained work"
        );
        for live in [
            NeighbourMsg::RemoteInput {
                peer: 1,
                surface: 2,
                event: vec![3],
            },
            NeighbourMsg::PermissionGrant {
                peer: 1,
                capability: "remote-input".into(),
            },
            NeighbourMsg::AppPresenceV2 {
                peer: 1,
                app_id: "app".into(),
                cluster_id: "room".into(),
                presence: collab_types::PeerPresence {
                    key: vec![4],
                    payload: collab_types::PeerPresencePayload::Arrived,
                },
            },
        ] {
            assert!(retention(&live, false).is_none());
        }
    }

    async fn endpoint() -> (Endpoint, MemoryLookup) {
        let lookup = MemoryLookup::new();
        let endpoint = Endpoint::builder(presets::N0)
            .address_lookup(lookup.clone())
            .bind()
            .await
            .unwrap();
        (endpoint, lookup)
    }

    async fn identified_endpoint(key: &ed25519_dalek::SigningKey) -> (Endpoint, MemoryLookup) {
        let lookup = MemoryLookup::new();
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .address_lookup(lookup.clone())
            .bind()
            .await
            .unwrap();
        (endpoint, lookup)
    }

    fn enrol(
        root: &ed25519_dalek::SigningKey,
        capabilities: DeviceCapabilities,
        label: &str,
    ) -> (ed25519_dalek::SigningKey, DeviceCert) {
        use rand_core::OsRng;
        let key = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_actors_exchange_over_iroh() {
        let (a, lookup_a) = endpoint().await;
        let (b, lookup_b) = endpoint().await;
        let source_peer = crate::network::behaviour::peer_liveness_handle(&a.id());
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: b_addr }).await.unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, NetEvent::PeerJoined { .. }) {
                break;
            }
        }
        cmd_b
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: String::new(),
                    invite_id: None,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SyncReq { .. },
                    ..
                }
            ) {
                break;
            }
        }

        let dedicated_before =
            DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed);
        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer,
                    encoded_h264: vec![0, 0, 0, 1, 0x65],
                    ts: 99,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(10), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SurfaceFrame {
                        surface: 7,
                        ts: 99,
                        ..
                    },
                    ..
                }
            ) {
                break;
            }
        }
        assert!(
            DEDICATED_SURFACE_FRAMES_RECEIVED.load(std::sync::atomic::Ordering::Relaxed)
                > dedicated_before,
            "surface frame should arrive through the dedicated QUIC ALPN"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actors_announce_identity_when_a_neighbor_connects() {
        use rand_core::OsRng;

        let root_a = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let root_b = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let (key_a, cert_a) = enrol(&root_a, DeviceCapabilities::primary(), "a");
        let (key_b, cert_b) = enrol(&root_b, DeviceCapabilities::primary(), "b");
        let expected_a = cert_a.device_public_key;
        let expected_b = cert_b.device_public_key;
        let (endpoint_a, lookup_a) = identified_endpoint(&key_a).await;
        let (endpoint_b, lookup_b) = identified_endpoint(&key_b).await;
        let addr_b = endpoint_b.addr();
        let identity_a = HostIdentity {
            device_cert: cert_a,
            device_signing_key: key_a,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let identity_b = HostIdentity {
            device_cert: cert_b,
            device_signing_key: key_b,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
            endpoint_a,
            lookup_a,
            Some(identity_a),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
            endpoint_b,
            lookup_b,
            Some(identity_b),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events_a, mut rx_a) = mpsc::channel(16);
        let (events_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events_b,
            })
            .await
            .unwrap();
        cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();

        for (rx, expected) in [(&mut rx_a, expected_b), (&mut rx_b, expected_a)] {
            timeout(Duration::from_secs(4), async {
                loop {
                    if let Some(NetEvent::Msg {
                        msg: NeighbourMsg::IdentityHello { cert, .. },
                        ..
                    }) = rx.recv().await
                        && cert.device_public_key == expected
                    {
                        break;
                    }
                }
            })
            .await
            .expect("identity announcement should not wait for the heartbeat");
        }
    }

    /// Spin up a relay pinned to workspace 42 and admit `a_pub`/`b_pub` to
    /// it. The returned `TempDir` must be kept alive for as long as the
    /// relay is used — dropping it deletes the on-disk state root.
    async fn spawn_pinned_relay_for_two_members(
        root: &ed25519_dalek::SigningKey,
        a_pub: [u8; 32],
        b_pub: [u8; 32],
    ) -> (Relay, tempfile::TempDir, DeviceCert) {
        let (host_key, host_cert) = enrol(root, DeviceCapabilities::host(), "host");
        let state = tempfile::tempdir().unwrap();
        let config = RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![WorkspaceBinding {
                id: 42,
                pin: true,
                replication: ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
            }],
            ..RelayConfig::default()
        };
        let relay = Relay::start(
            config,
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(42, true, vec![a_pub, b_pub], state.path().to_path_buf())
            .await
            .unwrap();
        (relay, state, host_cert)
    }

    /// Exercise the exact scheduled enrolled-home-peer route, rather than a
    /// direct RelayClient unit call. In particular, this protects against an
    /// innocuous-looking refactor that drops `HomePeerExpectation` before the
    /// relay dial.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn scheduled_enrolled_host_rejects_wrong_root_device_and_revocation() {
        use rand_core::OsRng;
        let root = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let other_root = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let (client_key, client_cert) = enrol(&root, DeviceCapabilities::primary(), "client");
        let (host_key, host_cert) = enrol(&root, DeviceCapabilities::host(), "home");
        let (_, wrong_device_cert) = enrol(&root, DeviceCapabilities::host(), "other-home");
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                workspaces: vec![WorkspaceBinding {
                    id: 42,
                    pin: true,
                    replication: ReplicationIntent::FullWorkspace,
                    admitted_users: Vec::new(),
                }],
                ..RelayConfig::default()
            },
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(
                42,
                true,
                vec![client_key.verifying_key().to_bytes()],
                state.path().to_path_buf(),
            )
            .await
            .unwrap();

        let attempt = |expectation: HomePeerExpectation,
                       revocations: Vec<qos_user_identity::RevocationEntry>| {
            let relay_addr = relay.node_addr();
            let client_key = client_key.clone();
            let client_cert = client_cert.clone();
            async move {
                let (endpoint, _) = identified_endpoint(&client_key).await;
                let identity = HostIdentity {
                    device_cert: client_cert,
                    device_signing_key: client_key,
                    workspace_device_certs: HashMap::new(),
                    workspace_revocations: HashMap::from([(42, revocations)]),
                    identity_paths: None,
                };
                let (tx, mut rx) = mpsc::channel(2);
                let mut connecting = HashSet::new();
                let mut discoveries = HashMap::new();
                schedule_host_topic(
                    &endpoint,
                    &identity,
                    42,
                    relay_addr,
                    Some(expectation),
                    &tx,
                    &HashMap::new(),
                    &mut connecting,
                    &mut discoveries,
                );
                let rejected = matches!(
                    timeout(Duration::from_secs(5), rx.recv()).await,
                    Ok(Some(InternalEvent::HostConnected { result, .. })) if result.is_err()
                );
                for (_, task) in discoveries {
                    task.abort();
                }
                endpoint.close().await;
                rejected
            }
        };
        let host_device = host_cert.device_public_key;
        assert!(
            attempt(
                HomePeerExpectation {
                    user_public_key: other_root.verifying_key().to_bytes(),
                    device_public_key: host_device,
                },
                Vec::new(),
            )
            .await
        );
        assert!(
            attempt(
                HomePeerExpectation {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: wrong_device_cert.device_public_key,
                },
                Vec::new(),
            )
            .await
        );
        let mut revocation = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: host_device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &root).unwrap();
        assert!(
            attempt(
                HomePeerExpectation {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: host_device,
                },
                vec![revocation],
            )
            .await
        );
    }

    /// Spawn two actors, join workspace 42 on each, connect both to `relay`,
    /// and block until both report `HostReady`. Returns each actor's
    /// command sender and event receiver.
    async fn join_workspace_and_await_host_ready(
        a: Endpoint,
        lookup_a: MemoryLookup,
        identity_a: HostIdentity,
        b: Endpoint,
        lookup_b: MemoryLookup,
        identity_b: HostIdentity,
        relay: &Relay,
    ) -> (
        mpsc::Sender<NetCmd>,
        mpsc::Receiver<NetEvent>,
        mpsc::Sender<NetCmd>,
        mpsc::Receiver<NetEvent>,
    ) {
        let (cmd_a, _task_a) = Actor::spawn_reporting_addrs(
            a,
            lookup_a,
            Some(identity_a),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (cmd_b, _task_b) = Actor::spawn_reporting_addrs(
            b,
            lookup_b,
            Some(identity_b),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, mut rx_b) = mpsc::channel(16);
        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: event_b,
            })
            .await
            .unwrap();
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        for rx in [&mut rx_a, &mut rx_b] {
            loop {
                let event = timeout(Duration::from_secs(15), rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if matches!(event, NetEvent::HostReady { .. }) {
                    break;
                }
            }
        }
        (cmd_a, rx_a, cmd_b, rx_b)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_actors_exchange_through_authenticated_workspace_host() {
        use rand_core::OsRng;
        let root = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;

        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let identity_a = HostIdentity {
            device_cert: a_cert,
            device_signing_key: a_key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let identity_b = HostIdentity {
            device_cert: b_cert,
            device_signing_key: b_key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _rx_a, _cmd_b, mut rx_b) = join_workspace_and_await_host_ready(
            a, lookup_a, identity_a, b, lookup_b, identity_b, &relay,
        )
        .await;

        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: "hosted".into(),
                    invite_id: None,
                },
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(15), rx_b.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(
                event,
                NetEvent::Msg {
                    msg: NeighbourMsg::SyncReq { .. },
                    ..
                }
            ) {
                break;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn failed_host_address_refresh_retains_files_route_until_explicit_removal() {
        use rand_core::OsRng;
        let root = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let a_public = a_key.verifying_key().to_bytes();
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, host_cert) =
            spawn_pinned_relay_for_two_members(&root, a_public, b_key.verifying_key().to_bytes())
                .await;
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let identity = |key, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, mut rx_a, _cmd_b, _rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity(a_key, a_cert),
            b,
            lookup_b,
            identity(b_key, b_cert),
            &relay,
        )
        .await;

        // Upgrade the existing host route to an enrolled home-peer route.
        // The subsequent address refresh must retain that route's Files
        // endpoint; it is a reconnect, not a home-peer removal.
        let expectation = HomePeerExpectation {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: host_cert.device_public_key,
        };
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: Some(expectation.clone()),
            })
            .await
            .unwrap();
        loop {
            let event = timeout(Duration::from_secs(15), rx_a.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, NetEvent::HostReady { .. }) {
                break;
            }
        }

        // A replacement address that cannot respond must not tear down the
        // already-discovered Files endpoint. The old actor implementation
        // called the permanent-removal path before attempting this dial.
        let refreshed =
            EndpointAddr::new(relay.node_addr().id).with_ip_addr("192.0.2.1:9".parse().unwrap());
        cmd_a
            .send(NetCmd::ConnectHost {
                addr: refreshed,
                expectation: Some(expectation),
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !noted_content_peer_removals()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&relay.node_addr().id),
            "an address refresh must retain the enrolled peer's Files route"
        );

        cmd_a
            .send(NetCmd::DisconnectHost {
                id: relay.node_addr().id,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if noted_content_peer_removals()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains(&relay.node_addr().id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("explicit removal must revoke enrolled Files routes");
        for _ in 0..100 {
            if !relay.is_subscribed(42, &a_public).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!relay.is_subscribed(42, &a_public).await);
        tokio::time::sleep(HEARTBEAT_INTERVAL + Duration::from_millis(250)).await;
        assert!(
            !relay.is_subscribed(42, &a_public).await,
            "a removed host must not be redialled by the heartbeat retry loop"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn host_replays_the_latest_personal_continuity_checkpoint_after_close() {
        use rand_core::OsRng;
        let root = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let (relay, _state, _host_cert) = spawn_pinned_relay_for_two_members(
            &root,
            a_key.verifying_key().to_bytes(),
            b_key.verifying_key().to_bytes(),
        )
        .await;
        let (a, lookup_a) = identified_endpoint(&a_key).await;
        let (b, lookup_b) = identified_endpoint(&b_key).await;
        let b_restart_key = b_key.clone();
        let b_restart_cert = b_cert.clone();
        let identity = |key, cert| HostIdentity {
            device_cert: cert,
            device_signing_key: key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            identity_paths: None,
        };
        let (cmd_a, _rx_a, _cmd_b, mut rx_b) = join_workspace_and_await_host_ready(
            a,
            lookup_a,
            identity(a_key, a_cert),
            b,
            lookup_b,
            identity(b_key, b_cert),
            &relay,
        )
        .await;
        let checkpoint = |bytes| NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key: root.verifying_key().to_bytes(),
            publisher: 7,
            snapshot: bytes,
        };
        cmd_a
            .send(NetCmd::PublishCheckpoint {
                ws: 42,
                msg: checkpoint(vec![1]),
            })
            .await
            .unwrap();
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                ..
            }) = timeout(Duration::from_secs(10), rx_b.recv()).await.unwrap()
            {
                assert_eq!(snapshot, vec![1]);
                break;
            }
        }
        cmd_a
            .send(NetCmd::PublishCheckpoint {
                ws: 42,
                msg: checkpoint(Vec::new()),
            })
            .await
            .unwrap();
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                ..
            }) = timeout(Duration::from_secs(10), rx_b.recv()).await.unwrap()
            {
                assert!(snapshot.is_empty(), "closure replaces live checkpoint");
                break;
            }
        }

        // A fresh actor with B's durable device identity models a shell
        // restart while A is offline. The host must replay the retained
        // closure baseline rather than resurrecting A's earlier checkpoint.
        let (b_restarted, lookup_restarted) = identified_endpoint(&b_restart_key).await;
        let (cmd_restarted, _task_restarted) = Actor::spawn_reporting_addrs(
            b_restarted,
            lookup_restarted,
            Some(identity(b_restart_key, b_restart_cert)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (events, mut replay) = mpsc::channel(16);
        cmd_restarted
            .send(NetCmd::JoinWorkspace {
                ws: 42,
                on_event: events,
            })
            .await
            .unwrap();
        cmd_restarted
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
                expectation: None,
            })
            .await
            .unwrap();
        loop {
            if let Some(NetEvent::Msg {
                msg: NeighbourMsg::PersonalContinuityCheckpoint { snapshot, .. },
                ..
            }) = timeout(Duration::from_secs(10), replay.recv())
                .await
                .unwrap()
            {
                assert!(snapshot.is_empty(), "restarted B receives retained closure");
                break;
            }
        }
    }
}
