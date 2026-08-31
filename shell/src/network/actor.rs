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
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::{Event as GossipEvent, GossipSender};
use iroh_gossip::net::{GOSSIP_ALPN, Gossip};
use iroh_gossip::proto::TopicId;
use qos_relay::{Inbound as HostInbound, RelayClient, RelayCoeditSession};
use qos_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::network::protocol::NeighbourMsg;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
const EVICTION_INTERVAL: Duration = Duration::from_secs(1);
const HOST_CHECKPOINT_INTERVAL: Duration = Duration::from_mins(10);
const MAX_GOSSIP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub type WorkspaceId = u64;
pub type PeerId = EndpointId;

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
    fn material_for(
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
        result: Result<RelayCoeditSession, String>,
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

async fn run(
    endpoint: Endpoint,
    lookup: MemoryLookup,
    host_identity: Option<HostIdentity>,
    listen_addrs: Arc<Mutex<Vec<EndpointAddr>>>,
    mut cmd_rx: mpsc::Receiver<NetCmd>,
) {
    let gossip = Gossip::builder()
        .max_message_size(MAX_GOSSIP_MESSAGE_BYTES)
        .spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();
    let (internal_tx, mut internal_rx) = mpsc::channel(256);
    let mut topics: HashMap<WorkspaceId, TopicRuntime> = HashMap::new();
    let mut subscribers: HashMap<WorkspaceId, mpsc::Sender<NetEvent>> = HashMap::new();
    let mut peers: HashMap<EndpointId, EndpointAddr> = HashMap::new();
    let mut hosts: HashMap<EndpointId, EndpointAddr> = HashMap::new();
    let mut host_sessions: HashMap<(WorkspaceId, EndpointId), HostRuntime> = HashMap::new();
    let mut connecting_hosts = HashSet::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    let mut eviction = tokio::time::interval(EVICTION_INTERVAL);
    let mut host_checkpoint = tokio::time::interval(HOST_CHECKPOINT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    eviction.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    host_checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut peers_seen: HashMap<(WorkspaceId, EndpointId), Instant> = HashMap::new();
    let mut recent = HashSet::new();
    let mut recent_order = VecDeque::new();
    heartbeat.tick().await;
    eviction.tick().await;
    host_checkpoint.tick().await;

    loop {
        tokio::select! {
            command = cmd_rx.recv() => match command {
                Some(NetCmd::JoinWorkspace { ws, on_event }) => {
                    subscribers.insert(ws, on_event);
                    if let std::collections::hash_map::Entry::Vacant(e) = topics.entry(ws) {
                        let bootstrap = peers.keys().copied().collect();
                        match gossip.subscribe(topic_id(ws), bootstrap).await {
                            Ok(topic) => {
                                let (sender, mut receiver) = topic.split();
                                let tx = internal_tx.clone();
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
                    if let Some(identity) = host_identity.as_ref() {
                        for addr in hosts.values().cloned() {
                            schedule_host_topic(
                                &endpoint,
                                identity,
                                ws,
                                addr,
                                &internal_tx,
                                &host_sessions,
                                &mut connecting_hosts,
                            );
                        }
                    }
                }
                Some(NetCmd::LeaveWorkspace { ws }) => {
                    subscribers.remove(&ws);
                    peers_seen.retain(|(peer_ws, _), _| *peer_ws != ws);
                    if let Some(topic) = topics.remove(&ws) {
                        topic.pump.abort();
                    }
                    let leaving: Vec<_> = host_sessions
                        .keys()
                        .filter(|(session_ws, _)| *session_ws == ws)
                        .copied()
                        .collect();
                    for key in leaving {
                        if let Some(runtime) = host_sessions.remove(&key) {
                            runtime.pump.abort();
                        }
                    }
                }
                Some(NetCmd::Publish { ws, msg }) => {
                    publish(&topics, &host_sessions, ws, msg, false).await;
                }
                Some(NetCmd::PublishCheckpoint { ws, msg }) => {
                    publish(&topics, &host_sessions, ws, msg, true).await;
                }
                Some(NetCmd::Connect { addr }) => {
                    let id = addr.id;
                    lookup.add_endpoint_info(addr.clone());
                    peers.insert(id, addr);
                    for topic in topics.values() {
                        if let Err(error) = topic.sender.join_peers(vec![id]).await {
                            tracing::warn!(peer = %id, %error, "iroh gossip peer join failed");
                        }
                    }
                }
                Some(NetCmd::ConnectHost { addr }) => {
                    let id = addr.id;
                    lookup.add_endpoint_info(addr.clone());
                    hosts.insert(id, addr.clone());
                    let Some(identity) = host_identity.as_ref() else {
                        tracing::warn!(peer = %id, "cannot connect workspace host without a device certificate");
                        continue;
                    };
                    for ws in topics.keys().copied() {
                        schedule_host_topic(
                            &endpoint,
                            identity,
                            ws,
                            addr.clone(),
                            &internal_tx,
                            &host_sessions,
                            &mut connecting_hosts,
                        );
                    }
                }
                None => break,
            },
            internal = internal_rx.recv() => match internal {
                Some(InternalEvent::Topic { ws, event }) => {
                    let Some(tx) = subscribers.get(&ws) else { continue };
                    let event = match event {
                        GossipEvent::NeighborUp(peer) => {
                            peers_seen.insert((ws, peer), Instant::now());
                            Some(NetEvent::PeerJoined { ws, peer })
                        }
                        GossipEvent::NeighborDown(peer) => {
                            peers_seen.remove(&(ws, peer));
                            Some(NetEvent::PeerLeft { ws, peer })
                        }
                        GossipEvent::Received(message) => {
                            let peer = message.delivered_from;
                            let rediscovered = peers_seen.insert((ws, peer), Instant::now()).is_none();
                            let fingerprint = message_fingerprint(&message.content);
                            if !mark_new_message(ws, fingerprint, &mut recent, &mut recent_order) {
                                continue;
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
                            tracing::warn!(ws, "iroh gossip receiver lagged; anti-entropy will recover durable state");
                            None
                        }
                    };
                    if let Some(event) = event {
                        let _ = tx.send(event).await;
                    }
                }
                Some(InternalEvent::Closed { ws }) => { topics.remove(&ws); }
                Some(InternalEvent::HostConnected { ws, peer, result }) => {
                    connecting_hosts.remove(&(ws, peer));
                    if !topics.contains_key(&ws) {
                        continue;
                    }
                    match result {
                        Ok(session) => {
                            let session = Arc::new(session);
                            let pump = spawn_host_pump(ws, peer, Arc::clone(&session), internal_tx.clone());
                            host_sessions.insert((ws, peer), HostRuntime { session, pump });
                        }
                        Err(error) => {
                            tracing::warn!(ws, peer = %peer, %error, "workspace host connection failed");
                        }
                    }
                }
                Some(InternalEvent::Host { ws, peer, event }) => {
                    let mapped = match event {
                        HostEvent::Ready => Some(NetEvent::HostReady { ws, peer }),
                        HostEvent::Message { author, fingerprint, msg } => {
                            if !mark_new_message(ws, fingerprint, &mut recent, &mut recent_order) {
                                continue;
                            }
                            Some(NetEvent::Msg { ws, peer: author, msg })
                        }
                        HostEvent::Closed => {
                            if let Some(runtime) = host_sessions.remove(&(ws, peer)) {
                                runtime.pump.abort();
                            }
                            Some(NetEvent::PeerLeft { ws, peer })
                        }
                    };
                    if let (Some(event), Some(tx)) = (mapped, subscribers.get(&ws)) {
                        let _ = tx.send(event).await;
                    }
                }
                None => break,
            },
            _ = eviction.tick() => {
                let now = Instant::now();
                let stale: Vec<_> = peers_seen
                    .iter()
                    .filter_map(|(&(ws, peer), seen)| {
                        (now.duration_since(*seen) > PEER_TIMEOUT).then_some((ws, peer))
                    })
                    .collect();
                for (ws, peer) in stale {
                    peers_seen.remove(&(ws, peer));
                    if let Some(tx) = subscribers.get(&ws) {
                        let _ = tx.send(NetEvent::PeerLeft { ws, peer }).await;
                    }
                }
            },
            _ = heartbeat.tick() => {
                if let Ok(mut out) = listen_addrs.lock() {
                    out.clear();
                    out.push(endpoint.addr());
                }
                if let Some(identity) = host_identity.as_ref() {
                    for ws in topics.keys().copied() {
                        for addr in hosts.values().cloned() {
                            schedule_host_topic(
                                &endpoint,
                                identity,
                                ws,
                                addr,
                                &internal_tx,
                                &host_sessions,
                                &mut connecting_hosts,
                            );
                        }
                    }
                }
                if let Some(identity) = host_identity.as_ref() {
                    for (&ws, topic) in &topics {
                        let (cert, revocations) = identity.material_for(ws);
                        let hello = NeighbourMsg::IdentityHello {
                            cert: Box::new(cert),
                            revocations,
                        };
                        if let Ok(bytes) = postcard::to_allocvec(&hello) {
                            let _ = topic.sender.broadcast(Bytes::from(bytes)).await;
                        }
                    }
                }
                let msg = NeighbourMsg::Heartbeat {
                    peer: crate::network::behaviour::peer_liveness_handle(&endpoint.id()),
                    ts: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
                };
                if let Ok(bytes) = postcard::to_allocvec(&msg) {
                    for topic in topics.values() {
                        let _ = topic.sender.broadcast(Bytes::from(bytes.clone())).await;
                    }
                }
            }
            _ = host_checkpoint.tick() => {
                for &(ws, peer) in host_sessions.keys() {
                    if let Some(tx) = subscribers.get(&ws) {
                        let _ = tx.send(NetEvent::HostReady { ws, peer }).await;
                    }
                }
            }
        }
    }

    for (_, topic) in topics {
        topic.pump.abort();
    }
    for (_, host) in host_sessions {
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
) {
    let Some(topic) = topics.get(&ws) else { return };
    let bytes = match postcard::to_allocvec(&msg) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(ws, %error, "collaboration message encode failed");
            return;
        }
    };
    if let Err(error) = topic.sender.broadcast(Bytes::from(bytes.clone())).await {
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
        NeighbourMsg::AppCrdtSnapshot {
            app_id, record_id, ..
        } => {
            let route = format!("app\0{app_id}\0{record_id}");
            return Some((
                *blake3::hash(route.as_bytes()).as_bytes(),
                Retention::Checkpoint,
            ));
        }
        NeighbourMsg::AppCrdtIncremental {
            app_id, record_id, ..
        }
        | NeighbourMsg::AppCrdtIncrementalSigned {
            app_id, record_id, ..
        } => {
            let route = format!("app\0{app_id}\0{record_id}");
            return Some((*blake3::hash(route.as_bytes()).as_bytes(), Retention::Delta));
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
    internal_tx: &mpsc::Sender<InternalEvent>,
    sessions: &HashMap<(WorkspaceId, EndpointId), HostRuntime>,
    connecting: &mut HashSet<(WorkspaceId, EndpointId)>,
) {
    let host = addr.id;
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
    let tx = internal_tx.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            RelayClient::connect_to_pinned_host(
                endpoint,
                addr,
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
                result,
            })
            .await;
    });
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
                | HostInbound::SignedDelta {
                    author,
                    op_bytes: data,
                }
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
    use qos_relay::{Relay, RelayConfig, WorkspaceBinding, daemon::RelayIdentity};
    use qos_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    use tokio::time::timeout;

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
                &NeighbourMsg::AppCrdtSnapshot {
                    peer: 1,
                    app_id: "app".into(),
                    cluster_id: "room".into(),
                    record_id: "doc".into(),
                    data: vec![2],
                },
                false,
            ),
            Some((_, Retention::Checkpoint))
        ));
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
        let a_addr = a.addr();
        let b_addr = b.addr();
        let (cmd_a, _a_task) = Actor::spawn(a, lookup_a);
        let (cmd_b, _b_task) = Actor::spawn(b, lookup_b);
        let (event_a, mut rx_a) = mpsc::channel(16);
        let (event_b, _rx_b) = mpsc::channel(16);
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
        cmd_b.send(NetCmd::Connect { addr: a_addr }).await.unwrap();
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
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_actors_exchange_through_authenticated_workspace_host() {
        use rand_core::OsRng;
        let root = ed25519_dalek::SigningKey::generate(&mut OsRng);
        let (host_key, host_cert) = enrol(&root, DeviceCapabilities::host(), "host");
        let (a_key, a_cert) = enrol(&root, DeviceCapabilities::primary(), "a");
        let (b_key, b_cert) = enrol(&root, DeviceCapabilities::member(), "b");
        let state = tempfile::tempdir().unwrap();
        let config = RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![WorkspaceBinding { id: 42, pin: true }],
            ..RelayConfig::default()
        };
        let relay = Relay::start(
            config,
            RelayIdentity {
                user_public_key: root.verifying_key().to_bytes(),
                device_signing_key: host_key,
                device_cert: host_cert,
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay
            .set_workspace_membership(
                42,
                true,
                vec![
                    a_key.verifying_key().to_bytes(),
                    b_key.verifying_key().to_bytes(),
                ],
                state.path().to_path_buf(),
            )
            .await
            .unwrap();

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
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::ConnectHost {
                addr: relay.node_addr(),
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
        cmd_a
            .send(NetCmd::Publish {
                ws: 42,
                msg: NeighbourMsg::SyncReq {
                    since: vec![],
                    room: "hosted".into(),
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
}
