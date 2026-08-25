//! Network actor.
//!
//! Owns a [`Swarm<NiriosBehaviour>`](libp2p::Swarm) and runs it as a tokio
//! task. The shell drives the actor over an `mpsc::Sender<NetCmd>`; per-
//! room event subscribers receive `NetEvent`s back over their own
//! `mpsc::Sender<NetEvent>` (registered via [`NetCmd::JoinRoom`]).
//!
//! The actor is transport-agnostic at the type level: it only speaks
//! [`NeighbourMsg`] in/out.
//!
//! # One topic per room, and admission before discovery
//!
//! A gossipsub topic is a broadcast channel: every peer subscribed to it
//! receives every message published on it, and the recipient's own
//! filtering happens after delivery — on the wrong side of the wire.
//! `main.md` states the model that makes this decisive:
//!
//! > read access is enforced by not sending the data. You cannot un-send
//! > what a peer already holds.
//!
//! So the topic is the room: [`room_topic`] derives it from the room's
//! own 122-bit uuid, and a shell subscribes only to the rooms it is
//! actually in. A machine that was never invited to a room does not know
//! its id, cannot name its topic, and is therefore never sent its
//! traffic — rather than being sent it and asked not to look.
//!
//! Discovery is not admission either. mDNS says a qOS machine exists on
//! this network; it says nothing about whether the two are collaborating.
//! A discovered peer's address is remembered so an invite can be acted on
//! later, but only a peer this session was told to admit — by redeeming
//! an invite that names it — is added to the gossip mesh.

use crate::network::behaviour::{NiriosBehaviour, NiriosBehaviourEvent};
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::RoomId;
use libp2p::futures::StreamExt;
use libp2p::{Multiaddr, Swarm, gossipsub, mdns, swarm::SwarmEvent};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Interval between heartbeat publishes per joined room.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Silence threshold after which a peer is considered gone.
const PEER_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the actor scans `peers_seen` for stale peers.
const EVICTION_INTERVAL: Duration = Duration::from_secs(1);

/// Re-export of libp2p's `PeerId` for callers that build commands.
pub type PeerId = libp2p::PeerId;

/// Commands accepted by the [`Actor`] over its `mpsc::Sender<NetCmd>`.
#[derive(Debug)]
pub enum NetCmd {
    /// Subscribe to the room's gossipsub topic and register a per-room
    /// event sink. Multiple `JoinRoom` calls for the same `room` replace
    /// the prior sink.
    JoinRoom {
        room: RoomId,
        on_event: mpsc::Sender<NetEvent>,
    },
    /// Drop the room subscription and event sink.
    ///
    /// This is what makes leaving a room *mean* something at the
    /// transport: an unsubscribed topic stops being delivered, so a
    /// former member is no longer sent the room's traffic.
    LeaveRoom { room: RoomId },
    /// Publish `msg` to `room`'s gossipsub topic. Failures (no peers,
    /// oversized payload, etc.) are silently dropped — the actor logs
    /// but does not propagate the error.
    Publish { room: RoomId, msg: NeighbourMsg },
    /// Admit `peer` to the gossip mesh.
    ///
    /// Sent when this session redeems an invite that names the peer. It
    /// is the only route into [`Actor::admitted`]: discovery does not
    /// admit, so a machine that merely shares a LAN is never meshed
    /// with.
    AdmitPeer { peer: PeerId },
    /// Dial a multiaddr. An invite carries one, so redeeming it does not
    /// have to wait on discovery.
    Connect { addr: Multiaddr },
}

/// Events emitted by the [`Actor`] back to per-room subscribers.
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// Peer was added to the room's gossipsub mesh.
    PeerJoined { room: RoomId, peer: PeerId },
    /// Peer left the room's gossipsub mesh.
    PeerLeft { room: RoomId, peer: PeerId },
    /// A `NeighbourMsg` arrived on the room's topic from `peer`.
    Msg {
        room: RoomId,
        peer: PeerId,
        msg: NeighbourMsg,
    },
}

/// Tokio actor wrapping a `Swarm<NiriosBehaviour>`.
pub struct Actor {
    swarm: Swarm<NiriosBehaviour>,
    cmd_rx: mpsc::Receiver<NetCmd>,
    /// Per-room event subscribers, keyed by the room this session is in.
    subscribers: HashMap<RoomId, mpsc::Sender<NetEvent>>,
    /// Last time we observed any gossipsub message from each peer; used
    /// to evict peers that have gone silent for `PEER_TIMEOUT`.
    peers_seen: HashMap<libp2p::PeerId, Instant>,
    /// Peers this session has been told to mesh with, by
    /// [`NetCmd::AdmitPeer`].
    ///
    /// Empty until an invite is redeemed. Being reachable is not being
    /// admitted, so this never fills itself from discovery.
    admitted: HashSet<libp2p::PeerId>,
    /// Addresses mDNS has reported, for peers that may be admitted later.
    ///
    /// Kept because discovery and admission arrive in either order: a
    /// peer can be on the LAN before the invite naming it is redeemed,
    /// and dialing it then must not have to wait for the next mDNS
    /// sweep.
    discovered: HashMap<libp2p::PeerId, Vec<Multiaddr>>,
    /// Every address the swarm has reported listening on, newest last.
    /// Shared with the shell so [`crate::shell::network::ShellNetwork`]
    /// can put a dialable address inside an invite token.
    listen_addrs: Arc<Mutex<Vec<Multiaddr>>>,
}

/// The gossipsub topic a room's traffic rides.
///
/// Derived from the room's own id, which is a random 122-bit uuid minted
/// once per workspace and handed on only through an invite. That is what
/// scopes delivery: naming this topic requires already holding the room
/// id, so subscribing to a room you were not invited to is not a check
/// you can fail — it is a string you cannot produce.
///
/// The `/v1` suffix is the wire version, so a future change to what rides
/// the topic lands on a different one rather than being mixed into this.
pub fn room_topic(room: RoomId) -> gossipsub::IdentTopic {
    gossipsub::IdentTopic::new(format!("/nirios/ws/{room}/v1"))
}

/// Say what just failed to go out, at a level that matches what it costs.
///
/// # Why this exists
///
/// This publish used to be `let _ = ...publish(topic, bytes)`. A 14 KiB
/// constant guarding one path became a product-wide defect nobody noticed for
/// months precisely because the other paths dropped frames and returned without
/// a word: a session put a photograph into a shared document, gossipsub refused
/// the oversized RPC, and every peer's copy froze at the line before the image
/// while both ends still reported themselves connected.
///
/// The levels are graded by consequence rather than by how unusual the error
/// is. Having no peers is the ordinary state of a session nobody has joined
/// yet, and a lost caret is corrected by the next one — those are noise at
/// `warn`. A lost CRDT frame is not recoverable by repetition: Automerge queues
/// every later change naming it as an ancestor, so one loss silently freezes
/// the document for good. That is an `error`, and it names the size, because
/// size is nearly always why.
fn report_publish_failure(
    room: RoomId,
    kind: &'static str,
    loss_is_permanent: bool,
    bytes: usize,
    err: &gossipsub::PublishError,
) {
    use gossipsub::PublishError as P;
    match err {
        // Nobody to send to yet. Expected while a room is still assembling,
        // and the sync kick re-offers state once a peer appears.
        P::NoPeersSubscribedToTopic | P::Duplicate => {
            tracing::trace!(room = %room, kind, bytes, error = %err, "peer-sync: publish skipped");
        }
        P::MessageTooLarge if loss_is_permanent => {
            tracing::error!(
                room = %room, kind, bytes,
                limit = gossipsub::Config::default_max_transmit_size(),
                "peer-sync: a document frame exceeded the gossip limit and was dropped. \
                 This document will NOT converge for peers that lack this change: every \
                 later change naming it as an ancestor stays queued and unapplied, and \
                 the document simply stops at the point before it",
            );
        }
        P::MessageTooLarge => {
            tracing::warn!(
                room = %room, kind, bytes,
                limit = gossipsub::Config::default_max_transmit_size(),
                "peer-sync: message exceeded the gossip limit and was dropped",
            );
        }
        _ if loss_is_permanent => {
            tracing::error!(
                room = %room, kind, bytes, error = %err,
                "peer-sync: a document frame could not be published and is lost; this \
                 document will not converge for peers that lack this change",
            );
        }
        _ => {
            tracing::warn!(room = %room, kind, bytes, error = %err, "peer-sync: publish failed");
        }
    }
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

/// Reverse-lookup: find the room whose topic-hash matches the gossipsub
/// message. Linear scan over `subscribers`; the count is small (one per
/// room this session is in) so this is fine.
///
/// A hash that matches nothing is a topic we are not in, and its message
/// is dropped rather than fanned to some other room's subscriber.
fn room_for_topic(
    subscribers: &HashMap<RoomId, mpsc::Sender<NetEvent>>,
    hash: &gossipsub::TopicHash,
) -> Option<RoomId> {
    subscribers
        .keys()
        .copied()
        .find(|room| &room_topic(*room).hash() == hash)
}

impl Actor {
    /// Spawn the actor on the current tokio runtime. Returns the
    /// command sender and the spawned task's `JoinHandle` so the
    /// caller can await shutdown.
    pub fn spawn(
        swarm: Swarm<NiriosBehaviour>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        Self::spawn_reporting_addrs(swarm, Arc::new(Mutex::new(Vec::new())))
    }

    /// [`Self::spawn`], plus a slot the actor writes every address the
    /// swarm starts listening on into.
    ///
    /// An invite has to carry an address a peer can dial, and only the
    /// swarm knows what it bound — the port is assigned by the kernel.
    /// Without this the address is unknowable outside the actor task,
    /// and an invite can only carry a guess.
    pub fn spawn_reporting_addrs(
        swarm: Swarm<NiriosBehaviour>,
        listen_addrs: Arc<Mutex<Vec<Multiaddr>>>,
    ) -> (mpsc::Sender<NetCmd>, tokio::task::JoinHandle<()>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let mut actor = Self {
            swarm,
            cmd_rx,
            subscribers: HashMap::new(),
            peers_seen: HashMap::new(),
            admitted: HashSet::new(),
            discovered: HashMap::new(),
            listen_addrs,
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

    /// Publish a `NeighbourMsg::Heartbeat` to every joined room's
    /// gossipsub topic. Failures (no peers, oversized payload, etc.) are
    /// silently dropped — heartbeat loss is itself the signal to evict.
    ///
    /// Per room, not once globally: a beacon on a room's topic reaches
    /// that room's members and nobody else, which is the same rule the
    /// payload traffic follows.
    fn publish_heartbeat(&mut self) {
        let peer = crate::network::behaviour::peer_liveness_handle(self.swarm.local_peer_id());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let msg = NeighbourMsg::Heartbeat { peer, ts };
        let bytes = match serde_json::to_vec(&msg) {
            Ok(b) => b,
            Err(_) => return,
        };
        let rooms: Vec<RoomId> = self.subscribers.keys().copied().collect();
        for room in rooms {
            let topic = room_topic(room);
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
            // Fan out PeerLeft on every room; the receiver decides what
            // to do with peers it has not been tracking.
            let rooms: Vec<RoomId> = self.subscribers.keys().copied().collect();
            for room in rooms {
                if let Some(tx) = self.subscribers.get(&room) {
                    let _ = tx.send(NetEvent::PeerLeft { room, peer }).await;
                }
            }
        }
    }

    async fn handle_cmd(&mut self, cmd: NetCmd) {
        match cmd {
            NetCmd::JoinRoom { room, on_event } => {
                let topic = room_topic(room);
                let _ = self.swarm.behaviour_mut().gossipsub.subscribe(&topic);
                self.subscribers.insert(room, on_event);
            }
            NetCmd::LeaveRoom { room } => {
                let topic = room_topic(room);
                let _ = self.swarm.behaviour_mut().gossipsub.unsubscribe(&topic);
                self.subscribers.remove(&room);
            }
            NetCmd::Publish { room, msg } => {
                let topic = room_topic(room);
                let kind = msg.kind();
                let permanent = msg.loss_is_permanent();
                let bytes = match serde_json::to_vec(&msg) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!(
                            room = %room, kind, error = %e,
                            "peer-sync: a message could not be encoded and is lost",
                        );
                        return;
                    }
                };
                let len = bytes.len();
                if let Err(e) = self.swarm.behaviour_mut().gossipsub.publish(topic, bytes) {
                    report_publish_failure(room, kind, permanent, len, &e);
                }
            }
            NetCmd::AdmitPeer { peer } => {
                self.admit(peer);
            }
            NetCmd::Connect { addr } => {
                let _ = self.swarm.dial(addr);
            }
        }
    }

    /// Record that an invite named `peer`, and mesh with it.
    ///
    /// Idempotent: redeeming a second invite from the same host, or
    /// discovering it again afterwards, changes nothing.
    fn admit(&mut self, peer: libp2p::PeerId) {
        if !self.admitted.insert(peer) {
            return;
        }
        self.mesh_with(peer);
    }

    /// Put an already-admitted peer into the gossip mesh, dialing any
    /// address discovery has for it.
    ///
    /// `add_explicit_peer` alone will dial a peer the swarm has an
    /// address for; the explicit dial covers the case where mDNS knows
    /// one and the swarm's own address book does not yet.
    fn mesh_with(&mut self, peer: libp2p::PeerId) {
        self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
        if let Some(addrs) = self.discovered.get(&peer).cloned() {
            for addr in addrs {
                let _ = self.swarm.dial(
                    libp2p::swarm::dial_opts::DialOpts::peer_id(peer)
                        .addresses(vec![addr])
                        .build(),
                );
            }
        }
    }

    async fn handle_swarm_event(&mut self, event: SwarmEvent<NiriosBehaviourEvent>) {
        match event {
            // The bound address is only knowable here — the port comes
            // from the kernel — and an invite that cannot name one is an
            // invite nobody can act on.
            SwarmEvent::NewListenAddr { address, .. } => {
                tracing::info!(addr = %address, "peer-sync: listening");
                if let Ok(mut addrs) = self.listen_addrs.lock() {
                    addrs.push(address);
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => {
                // Touch liveness for the propagation source — that is the
                // peer we have a direct connection to, regardless of who
                // originally signed the message.
                self.peers_seen.insert(propagation_source, Instant::now());

                let Some(room) = room_for_topic(&self.subscribers, &message.topic) else {
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
                if let Some(tx) = self.subscribers.get(&room) {
                    let _ = tx
                        .send(NetEvent::Msg {
                            room,
                            peer,
                            msg: decoded,
                        })
                        .await;
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Gossipsub(
                gossipsub::Event::Subscribed { peer_id, topic },
            )) => {
                if let Some(room) = room_for_topic(&self.subscribers, &topic)
                    && let Some(tx) = self.subscribers.get(&room)
                {
                    let _ = tx
                        .send(NetEvent::PeerJoined {
                            room,
                            peer: peer_id,
                        })
                        .await;
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Gossipsub(
                gossipsub::Event::Unsubscribed { peer_id, topic },
            )) => {
                if let Some(room) = room_for_topic(&self.subscribers, &topic)
                    && let Some(tx) = self.subscribers.get(&room)
                {
                    let _ = tx
                        .send(NetEvent::PeerLeft {
                            room,
                            peer: peer_id,
                        })
                        .await;
                }
            }
            // Discovery is an address book, not a guest list.
            //
            // This used to `add_explicit_peer` every machine mDNS turned
            // up, which both dials it and makes gossipsub forward it
            // everything on every topic we are subscribed to. Sharing a
            // network is not being in a room together, so the address is
            // remembered and nothing else happens until an invite names
            // the peer.
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                for (peer_id, addr) in list {
                    let known = self.discovered.entry(peer_id).or_default();
                    if !known.contains(&addr) {
                        known.push(addr);
                    }
                    if self.admitted.contains(&peer_id) {
                        // Already invited, only now reachable.
                        self.mesh_with(peer_id);
                    }
                }
            }
            SwarmEvent::Behaviour(NiriosBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                for (peer_id, addr) in list {
                    if let Some(known) = self.discovered.get_mut(&peer_id) {
                        known.retain(|a| a != &addr);
                        if known.is_empty() {
                            self.discovered.remove(&peer_id);
                        }
                    }
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

    /// A room every test in this module can name.
    fn room(n: u128) -> RoomId {
        RoomId::from(uuid::Uuid::from_u128(0x6b3f_9a10_0000_4000_8000_000000000000 | n))
    }

    #[test]
    fn a_rooms_topic_is_derived_from_its_id() {
        let r = room(0xa);
        assert_eq!(room_topic(r).to_string(), format!("/nirios/ws/{r}/v1"));
        // Two rooms are two channels; nothing collapses them onto one.
        assert_ne!(room_topic(room(0xa)).hash(), room_topic(room(0xb)).hash());
    }

    /// Two actors in the same process, connected over TCP loopback, exchange
    /// a `NeighbourMsg::SyncReq` via gossipsub. The 10 s `tokio::time::timeout`
    /// budget for both PeerJoined and Msg keeps the test bounded; mesh
    /// formation typically completes well under 1 s on loopback.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_actors_exchange_publish_via_loopback() {
        let ws = room(0x42);

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

        async fn first_listen_addr(swarm: &mut Swarm<NiriosBehaviour>) -> Multiaddr {
            loop {
                if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
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
            .send(NetCmd::JoinRoom {
                room: ws,
                on_event: ev_tx_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinRoom {
                room: ws,
                on_event: ev_tx_b,
            })
            .await
            .unwrap();

        // 4. Dial B from A.
        cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();

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
            room: "6b3f9a10-0000-4000-8000-00000000000a".to_string(),
            invite: None,
        };
        cmd_a
            .send(NetCmd::Publish {
                room: ws,
                msg: payload.clone(),
            })
            .await
            .unwrap();

        // 7. B should observe the message. Phase 6's reconnect-sync
        // path also broadcasts an empty `SyncReq { since: vec![] }` on
        // PeerJoined, so the loop keeps draining until it sees the
        // payload this test actually published (`since: [1, 2, 3]`).
        let received = timeout(Duration::from_secs(10), async {
            loop {
                match ev_rx_b.recv().await {
                    Some(NetEvent::Msg { room, msg, .. }) => {
                        if msg == payload {
                            return Some((room, msg));
                        }
                        // Skip transport-layer SyncReqs from the
                        // reconnect-sync path; keep waiting for the
                        // test's payload.
                        continue;
                    }
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await
        .expect("message arrived within 10 s")
        .expect("channel still open");

        assert_eq!(received.0, ws);
        assert_eq!(received.1, payload);

        // Drain any final A-side events so the actor's task can shut down
        // without warnings during runtime drop.
        let _ = ev_rx_a.try_recv();
    }

    /// **The negative.** Two connected, meshed actors that are in
    /// *different* rooms: everything one publishes for its room must
    /// never arrive at the other.
    ///
    /// This is the shape the old single topic could not have: both were
    /// on `nirios/ws/0`, so every room's `AppCrdtIncremental`,
    /// `AppCrdtSnapshot`, `AppPresence` and `SyncResp` was delivered to
    /// every qOS machine on the network and only then filtered, in the
    /// recipient's own process.
    ///
    /// The assertion is on B's event stream — what actually crossed the
    /// wire — not on a filter having run. B is deliberately meshed with
    /// A and subscribed to a room of its own, so a message reaching it
    /// would be a routing fact rather than a discovery accident.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_room_you_are_not_in_never_reaches_you() {
        let theirs = room(0x1);
        let mine = room(0x2);

        let mut swarm_a = build_swarm();
        let mut swarm_b = build_swarm();
        swarm_a
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        swarm_b
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();

        async fn first_listen_addr(swarm: &mut Swarm<NiriosBehaviour>) -> Multiaddr {
            loop {
                if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                    return address;
                }
            }
        }
        let addr_b = timeout(Duration::from_secs(5), first_listen_addr(&mut swarm_b))
            .await
            .expect("peer B listen addr");
        let _ = timeout(Duration::from_secs(5), first_listen_addr(&mut swarm_a))
            .await
            .expect("peer A listen addr");

        let (cmd_a, _h_a) = Actor::spawn(swarm_a);
        let (cmd_b, _h_b) = Actor::spawn(swarm_b);

        let (ev_tx_a, _ev_rx_a) = mpsc::channel::<NetEvent>(32);
        let (ev_tx_b, mut ev_rx_b) = mpsc::channel::<NetEvent>(32);

        // A is in the room it will publish to. B is in a different one —
        // it is a real, live, subscribed qOS session, just not a member.
        cmd_a
            .send(NetCmd::JoinRoom {
                room: theirs,
                on_event: ev_tx_a,
            })
            .await
            .unwrap();
        cmd_b
            .send(NetCmd::JoinRoom {
                room: mine,
                on_event: ev_tx_b,
            })
            .await
            .unwrap();

        cmd_a.send(NetCmd::Connect { addr: addr_b }).await.unwrap();
        // Let the connection and gossipsub handshake settle. Longer than
        // the mesh needs, so "nothing arrived" is not "nothing had time
        // to arrive": the positive test above sees its message inside
        // this window.
        tokio::time::sleep(Duration::from_secs(2)).await;

        let secret = NeighbourMsg::AppCrdtSnapshot {
            peer: 7,
            app_id: "notes".to_string(),
            cluster_id: theirs.to_string(),
            record_id: "private.md".to_string(),
            data: b"the contents of a room B was never invited to".to_vec(),
        };
        for _ in 0..5 {
            cmd_a
                .send(NetCmd::Publish {
                    room: theirs,
                    msg: secret.clone(),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut delivered = Vec::new();
        while let Ok(ev) = ev_rx_b.try_recv() {
            if let NetEvent::Msg { room, msg, .. } = ev {
                delivered.push((room, msg));
            }
        }
        assert!(
            delivered.is_empty(),
            "a session that is not in the room must receive none of its traffic, got {delivered:?}"
        );
    }
}
