//! Network actor — Phase 2 Task 2.2.
//!
//! Owns a [`Swarm<NiriosBehaviour>`](libp2p::Swarm) and runs it as a tokio
//! task. The shell drives the actor over an `mpsc::Sender<NetCmd>`; per-
//! workspace event subscribers receive `NetEvent`s back over their own
//! `mpsc::Sender<NetEvent>` (registered via [`NetCmd::JoinWorkspace`]).
//!
//! The actor is transport-agnostic at the type level: it only speaks
//! [`NeighbourMsg`] in/out. Workspace subscriptions are namespaced as
//! gossipsub topics `nirios/ws/{ws}`. mDNS-discovered peers are auto-
//! added to the gossipsub mesh.

use crate::network::behaviour::{NiriosBehaviour, NiriosBehaviourEvent};
use crate::network::protocol::NeighbourMsg;
use libp2p::futures::StreamExt;
use libp2p::{Multiaddr, Swarm, gossipsub, mdns, swarm::SwarmEvent};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Interval between heartbeat publishes per joined workspace.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Silence threshold after which a peer is considered gone.
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the actor scans `peers_seen` for stale peers.
const EVICTION_INTERVAL: Duration = Duration::from_secs(1);

/// Numeric workspace identifier — mirrors the shell-side workspace ID.
pub type WorkspaceId = u64;

/// Re-export of libp2p's `PeerId` for callers that build commands.
pub type PeerId = libp2p::PeerId;

/// Commands accepted by the [`Actor`] over its `mpsc::Sender<NetCmd>`.
#[derive(Debug)]
pub enum NetCmd {
    /// Subscribe to the workspace's gossipsub topic and register a
    /// per-workspace event sink. Multiple `JoinWorkspace` calls for
    /// the same `ws` replace the prior sink.
    JoinWorkspace {
        ws: WorkspaceId,
        on_event: mpsc::Sender<NetEvent>,
    },
    /// Drop the workspace subscription and event sink.
    LeaveWorkspace { ws: WorkspaceId },
    /// Publish `msg` to the workspace's gossipsub topic. Failures
    /// (no peers, oversized payload, etc.) are silently dropped — the
    /// actor logs but does not propagate the error.
    Publish { ws: WorkspaceId, msg: NeighbourMsg },
    /// Dial a multiaddr. Used by the loopback test and by future
    /// out-of-band bootstrap mechanisms; mDNS handles LAN discovery.
    Connect { addr: Multiaddr },
}

/// Events emitted by the [`Actor`] back to per-workspace subscribers.
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// Peer was added to the workspace's gossipsub mesh.
    PeerJoined { ws: WorkspaceId, peer: PeerId },
    /// Peer left the workspace's gossipsub mesh.
    PeerLeft { ws: WorkspaceId, peer: PeerId },
    /// A `NeighbourMsg` arrived on the workspace topic from `peer`.
    Msg {
        ws: WorkspaceId,
        peer: PeerId,
        msg: NeighbourMsg,
    },
}

/// Tokio actor wrapping a `Swarm<NiriosBehaviour>`.
pub struct Actor {
    swarm: Swarm<NiriosBehaviour>,
    cmd_rx: mpsc::Receiver<NetCmd>,
    /// Per-workspace event subscribers, keyed by `WorkspaceId`.
    subscribers: HashMap<WorkspaceId, mpsc::Sender<NetEvent>>,
    /// Last time we observed any gossipsub message from each peer; used
    /// to evict peers that have gone silent for `PEER_TIMEOUT`.
    peers_seen: HashMap<libp2p::PeerId, Instant>,
}

/// Build the workspace's gossipsub topic identifier.
fn ws_topic(ws: WorkspaceId) -> gossipsub::IdentTopic {
    gossipsub::IdentTopic::new(format!("nirios/ws/{ws}"))
}

/// Hash a libp2p `PeerId` to a `u64` for embedding in protocol messages.
///
/// Uses [`std::collections::hash_map::DefaultHasher`] over the peer id's
/// raw protobuf bytes; collisions are not security-critical because the
/// authenticated `PeerId` is already carried by gossipsub itself — the
/// `Heartbeat::peer` field is only an opaque liveness identifier.
fn peer_id_to_u64(peer_id: &libp2p::PeerId) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    peer_id.to_bytes().hash(&mut hasher);
    hasher.finish()
}

/// Returns peers in `peers` whose last-seen timestamp is older than
/// `now - threshold`. Pure function so the eviction policy is unit-
/// testable without spinning up a swarm.
fn evict_stale(
    peers: &HashMap<libp2p::PeerId, Instant>,
    now: Instant,
    threshold: Duration,
) -> Vec<libp2p::PeerId> {
    peers
        .iter()
        .filter(|(_, last)| now.duration_since(**last) > threshold)
        .map(|(p, _)| *p)
        .collect()
}

/// Reverse-lookup: find the `WorkspaceId` whose topic-hash matches
/// the gossipsub message. Linear scan over `subscribers`; the count
/// is small (one per active workspace) so this is fine.
fn ws_for_topic(
    subscribers: &HashMap<WorkspaceId, mpsc::Sender<NetEvent>>,
    hash: &gossipsub::TopicHash,
) -> Option<WorkspaceId> {
    subscribers
        .keys()
        .copied()
        .find(|ws| &ws_topic(*ws).hash() == hash)
}

impl Actor {
    /// Spawn the actor on the current tokio runtime. Returns the
    /// command sender and the spawned task's `JoinHandle` so the
    /// caller can await shutdown.
    pub fn spawn(
        swarm: Swarm<NiriosBehaviour>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let mut actor = Self {
            swarm,
            cmd_rx,
            subscribers: HashMap::new(),
            peers_seen: HashMap::new(),
        };
        let handle = tokio::spawn(async move { actor.run().await });
        (cmd_tx, handle)
    }

    async fn run(&mut self) {
        let mut hb_tick = tokio::time::interval(HEARTBEAT_INTERVAL);
        let mut ev_tick = tokio::time::interval(EVICTION_INTERVAL);
        // Discard the immediate first tick on each interval so we don't
        // spam a heartbeat / eviction sweep before any peers exist.
        hb_tick.tick().await;
        ev_tick.tick().await;
        loop {
            tokio::select! {
                maybe_cmd = self.cmd_rx.recv() => {
                    match maybe_cmd {
                        Some(cmd) => self.handle_cmd(cmd).await,
                        None => break, // all senders dropped — clean shutdown
                    }
                }
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event).await;
                }
                _ = hb_tick.tick() => self.publish_heartbeat(),
                _ = ev_tick.tick() => self.evict_stale_peers().await,
            }
        }
    }

    /// Publish a `NeighbourMsg::Heartbeat` to every joined workspace's
    /// gossipsub topic. Failures (no peers, oversized payload, etc.) are
    /// silently dropped — heartbeat loss is itself the signal to evict.
    fn publish_heartbeat(&mut self) {
        let peer = peer_id_to_u64(self.swarm.local_peer_id());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let msg = NeighbourMsg::Heartbeat { peer, ts };
        let bytes = match serde_json::to_vec(&msg) {
            Ok(b) => b,
            Err(_) => return,
        };
        let workspaces: Vec<WorkspaceId> = self.subscribers.keys().copied().collect();
        for ws in workspaces {
            let topic = ws_topic(ws);
            let _ = self
                .swarm
                .behaviour_mut()
                .gossipsub
                .publish(topic, bytes.clone());
        }
    }

    /// Sweep `peers_seen` for entries older than `PEER_TIMEOUT`; for
    /// every stale peer, emit `NetEvent::PeerLeft` to all workspace
    /// subscribers and drop the entry.
    async fn evict_stale_peers(&mut self) {
        let stale = evict_stale(&self.peers_seen, Instant::now(), PEER_TIMEOUT);
        if stale.is_empty() {
            return;
        }
        for peer in &stale {
            self.peers_seen.remove(peer);
        }
        for peer in stale {
            // Fan out PeerLeft on every workspace; the receiver decides
            // what to do with peers it has not been tracking.
            let workspaces: Vec<WorkspaceId> = self.subscribers.keys().copied().collect();
            for ws in workspaces {
                if let Some(tx) = self.subscribers.get(&ws) {
                    let _ = tx.send(NetEvent::PeerLeft { ws, peer }).await;
                }
            }
        }
    }

    async fn handle_cmd(&mut self, cmd: NetCmd) {
        match cmd {
            NetCmd::JoinWorkspace { ws, on_event } => {
                let topic = ws_topic(ws);
                let _ = self.swarm.behaviour_mut().gossipsub.subscribe(&topic);
                self.subscribers.insert(ws, on_event);
            }
            NetCmd::LeaveWorkspace { ws } => {
                let topic = ws_topic(ws);
                let _ = self.swarm.behaviour_mut().gossipsub.unsubscribe(&topic);
                self.subscribers.remove(&ws);
            }
            NetCmd::Publish { ws, msg } => {
                let topic = ws_topic(ws);
                let bytes = match serde_json::to_vec(&msg) {
                    Ok(b) => b,
                    Err(_) => return,
                };
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(topic, bytes);
            }
            NetCmd::Connect { addr } => {
                let _ = self.swarm.dial(addr);
            }
        }
    }

    async fn handle_swarm_event(
        &mut self,
        event: SwarmEvent<NiriosBehaviourEvent>,
    ) {
        match event {
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Gossipsub(
                gossipsub::Event::Message {
                    propagation_source,
                    message,
                    ..
                },
            )) => {
                // Touch liveness for the propagation source — that is the
                // peer we have a direct connection to, regardless of who
                // originally signed the message.
                self.peers_seen.insert(propagation_source, Instant::now());

                let Some(ws) = ws_for_topic(&self.subscribers, &message.topic) else {
                    return;
                };
                let Ok(decoded) = serde_json::from_slice::<NeighbourMsg>(&message.data) else {
                    return;
                };
                // Heartbeats are transport-internal noise — they keep
                // `peers_seen` warm but never surface to subscribers.
                if matches!(decoded, NeighbourMsg::Heartbeat { .. }) {
                    return;
                }
                let peer = message.source.unwrap_or(propagation_source);
                if let Some(tx) = self.subscribers.get(&ws) {
                    let _ = tx
                        .send(NetEvent::Msg {
                            ws,
                            peer,
                            msg: decoded,
                        })
                        .await;
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Gossipsub(
                gossipsub::Event::Subscribed { peer_id, topic },
            )) => {
                if let Some(ws) = ws_for_topic(&self.subscribers, &topic) {
                    if let Some(tx) = self.subscribers.get(&ws) {
                        let _ = tx
                            .send(NetEvent::PeerJoined { ws, peer: peer_id })
                            .await;
                    }
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Gossipsub(
                gossipsub::Event::Unsubscribed { peer_id, topic },
            )) => {
                if let Some(ws) = ws_for_topic(&self.subscribers, &topic) {
                    if let Some(tx) = self.subscribers.get(&ws) {
                        let _ = tx
                            .send(NetEvent::PeerLeft { ws, peer: peer_id })
                            .await;
                    }
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Mdns(mdns::Event::Discovered(
                list,
            ))) => {
                let any_discovered = !list.is_empty();
                for (peer_id, _addr) in list {
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .add_explicit_peer(&peer_id);
                }
                // Phase 6 Task 6.2 — kick a "give me everything" SyncReq
                // out on every joined workspace so a returning peer
                // catches up without waiting for the next local edit.
                if any_discovered {
                    let msg = NeighbourMsg::SyncReq { since: vec![] };
                    let bytes = match serde_json::to_vec(&msg) {
                        Ok(b) => b,
                        Err(_) => return,
                    };
                    let workspaces: Vec<WorkspaceId> =
                        self.subscribers.keys().copied().collect();
                    for ws in workspaces {
                        let topic = ws_topic(ws);
                        let _ = self
                            .swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish(topic, bytes.clone());
                    }
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Mdns(mdns::Event::Expired(
                list,
            ))) => {
                for (peer_id, _addr) in list {
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .remove_explicit_peer(&peer_id);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::behaviour::NiriosBehaviour;
    use libp2p::{noise, tcp, yamux};
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn evict_stale_returns_peers_past_threshold() {
        let now = Instant::now();
        let fresh_peer = libp2p::PeerId::random();
        let stale_peer = libp2p::PeerId::random();
        let mut peers: HashMap<libp2p::PeerId, Instant> = HashMap::new();
        peers.insert(fresh_peer, now - Duration::from_secs(5));
        peers.insert(stale_peer, now - Duration::from_secs(20));
        let stale = evict_stale(&peers, now, Duration::from_secs(15));
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], stale_peer);
    }

    #[test]
    fn evict_stale_returns_empty_when_all_fresh() {
        let now = Instant::now();
        let p1 = libp2p::PeerId::random();
        let p2 = libp2p::PeerId::random();
        let mut peers: HashMap<libp2p::PeerId, Instant> = HashMap::new();
        peers.insert(p1, now - Duration::from_secs(1));
        peers.insert(p2, now - Duration::from_secs(10));
        let stale = evict_stale(&peers, now, Duration::from_secs(15));
        assert!(stale.is_empty());
    }

    fn build_swarm() -> Swarm<NiriosBehaviour> {
        libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .expect("tcp transport")
            .with_behaviour(|key| NiriosBehaviour::new(key).expect("behaviour"))
            .expect("with_behaviour")
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build()
    }

    /// Two actors in the same process, connected over TCP loopback, exchange
    /// a `NeighbourMsg::SyncReq` via gossipsub. The 10 s `tokio::time::timeout`
    /// budget for both PeerJoined and Msg keeps the test bounded; mesh
    /// formation typically completes well under 1 s on loopback.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_actors_exchange_publish_via_loopback() {
        const WS: WorkspaceId = 42;

        // 1. Build two swarms.
        let mut swarm_a = build_swarm();
        let mut swarm_b = build_swarm();

        // 2. Start each listening on an ephemeral loopback port and capture
        //    the resulting concrete multiaddr.
        swarm_a
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        swarm_b
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();

        async fn first_listen_addr(
            swarm: &mut Swarm<NiriosBehaviour>,
        ) -> Multiaddr {
            loop {
                if let SwarmEvent::NewListenAddr { address, .. } =
                    swarm.select_next_some().await
                {
                    return address;
                }
            }
        }
        let addr_b = timeout(Duration::from_secs(5), first_listen_addr(&mut swarm_b))
            .await
            .expect("peer B listen addr");
        // Drain peer A's NewListenAddr too so it does not stall the swarm
        // before we hand it to the actor.
        let _ = timeout(Duration::from_secs(5), first_listen_addr(&mut swarm_a))
            .await
            .expect("peer A listen addr");

        // 3. Spawn actors.
        let (cmd_a, _h_a) = Actor::spawn(swarm_a);
        let (cmd_b, _h_b) = Actor::spawn(swarm_b);

        let (ev_tx_a, mut ev_rx_a) = mpsc::channel::<NetEvent>(32);
        let (ev_tx_b, mut ev_rx_b) = mpsc::channel::<NetEvent>(32);

        cmd_a
            .send(NetCmd::JoinWorkspace {
                ws: WS,
                on_event: ev_tx_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinWorkspace {
                ws: WS,
                on_event: ev_tx_b,
            })
            .await
            .unwrap();

        // 4. Dial B from A.
        cmd_a
            .send(NetCmd::Connect { addr: addr_b })
            .await
            .unwrap();

        // 5. Wait for the gossipsub mesh to form. Peer B emits PeerJoined
        //    once A subscribes to the topic over the established connection.
        let _peer_joined = timeout(Duration::from_secs(10), async {
            loop {
                match ev_rx_b.recv().await {
                    Some(NetEvent::PeerJoined { .. }) => return Ok::<(), ()>(()),
                    Some(_) => continue,
                    None => return Err(()),
                }
            }
        })
        .await
        .expect("peer joined within 10 s")
        .expect("event channel still open");

        // Briefly let the mesh stabilise before publishing.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 6. Publish a SyncReq from A.
        let payload = NeighbourMsg::SyncReq {
            since: vec![1, 2, 3],
        };
        cmd_a
            .send(NetCmd::Publish {
                ws: WS,
                msg: payload.clone(),
            })
            .await
            .unwrap();

        // 7. B should observe the message.
        let received = timeout(Duration::from_secs(10), async {
            loop {
                match ev_rx_b.recv().await {
                    Some(NetEvent::Msg { ws, msg, .. }) => return Some((ws, msg)),
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await
        .expect("message arrived within 10 s")
        .expect("channel still open");

        assert_eq!(received.0, WS);
        assert_eq!(received.1, payload);

        // Drain any final A-side events so the actor's task can shut down
        // without warnings during runtime drop.
        let _ = ev_rx_a.try_recv();
    }
}
