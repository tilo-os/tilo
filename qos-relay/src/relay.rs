//! The relay's libp2p swarm + event loop.
//!
//! ## What the loop does
//!
//! - Listens on every multiaddr in `RelayConfig.listen`.
//! - Dials every `bootstrap` multiaddr at startup and feeds its
//!   `/p2p/<peerid>` suffix into the Kademlia routing table.
//! - On a freshly-discovered peer (gossipsub `Subscribed`, mdns
//!   `Discovered`, identify `Received`), adds it as an explicit
//!   gossipsub peer if it advertised any topic on our allowlist.
//! - Forwards gossipsub messages by virtue of being a mesh
//!   participant — gossipsub itself does the forwarding once a
//!   peer subscribes to a topic we already see traffic on. The
//!   relay's role is to *stay subscribed*, persistently, to every
//!   matching topic.
//! - Tracks metrics if `metrics_listen` is set.
//! - Handles SIGTERM/SIGINT by breaking out of the loop and
//!   returning from `run()`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use libp2p::futures::StreamExt as _;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, Swarm, gossipsub, identify, identity, kad, mdns, noise, tcp, yamux};
use tokio::signal::unix::{SignalKind, signal};

use crate::behaviour::{RelayBehaviour, RelayBehaviourEvent};
use crate::config::RelayConfig;
use crate::identity::load_or_create_keypair;
use crate::metrics::{RelayMetrics, TopicLabels};

/// Per (topic, peer) sliding-window rate limiter.
///
/// Tracks the count of messages seen in the current minute. When the
/// minute rolls, the counter resets. Cheap: a `HashMap<(String,
/// PeerId), (Instant, u32)>` checked once per inbound message.
#[derive(Default)]
struct RateLimiter {
    window: HashMap<(String, PeerId), (Instant, u32)>,
}

impl RateLimiter {
    fn check_and_record(&mut self, topic: &str, peer: PeerId, cap: u32) -> bool {
        if cap == 0 {
            return true;
        }
        let now = Instant::now();
        let entry = self
            .window
            .entry((topic.to_string(), peer))
            .or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        if entry.1 >= cap {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// The relay's public handle. After [`Relay::build`] the swarm is
/// listening on every configured multiaddr; [`Relay::run`] takes
/// ownership and drives the event loop until shutdown.
pub struct Relay {
    swarm: Swarm<RelayBehaviour>,
    config: RelayConfig,
    keypair: identity::Keypair,
    topic_matcher: globset::GlobSet,
    metrics: Arc<RelayMetrics>,
    metrics_handle: Option<tokio::task::JoinHandle<()>>,
    rate_limiter: RateLimiter,
    /// Subscribed gossipsub topics (those that matched the allowlist
    /// after some peer announced them). We subscribe on first sight
    /// and never unsubscribe — a relay's whole job is to stay
    /// available.
    subscribed_topics: HashSet<String>,
    connected_peers: HashSet<PeerId>,
}

impl Relay {
    /// Build a relay from `config`. Loads / generates the keypair,
    /// constructs the swarm, starts listening, dials bootstrap
    /// peers, and (optionally) starts the metrics HTTP server.
    pub async fn build(config: RelayConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let topic_matcher = config.compile_topic_matcher()?;

        let keypair = load_or_create_keypair(&config.identity_path)
            .with_context(|| format!("loading identity at {}", config.identity_path.display()))?;
        let local_peer_id = keypair.public().to_peer_id();
        tracing::info!(%local_peer_id, "qos-relay starting");

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
            .with_tokio()
            .with_tcp(
                tcp::Config::default().nodelay(true),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_quic()
            .with_dns()?
            .with_behaviour(|key| {
                RelayBehaviour::new(key)
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(300)))
            .build();

        for addr in &config.listen {
            match swarm.listen_on(addr.clone()) {
                Ok(_) => tracing::info!(%addr, "listening"),
                Err(err) => tracing::warn!(%addr, ?err, "listen_on failed"),
            }
        }

        // Seed Kademlia with bootstrap multiaddrs. Each must carry a
        // `/p2p/<peerid>` suffix so we can authenticate the dial.
        for addr in &config.bootstrap {
            if let Some(peer_id) = extract_peer_id(addr) {
                swarm
                    .behaviour_mut()
                    .kademlia
                    .add_address(&peer_id, addr.clone());
                tracing::info!(%addr, %peer_id, "added bootstrap peer to kademlia");
            } else {
                tracing::warn!(
                    %addr,
                    "bootstrap multiaddr lacks /p2p/<peerid>; cannot \
                     authenticate. Skipping."
                );
            }
            if let Err(err) = swarm.dial(addr.clone()) {
                tracing::warn!(%addr, ?err, "bootstrap dial failed");
            }
        }
        // Kick off a Kademlia bootstrap query so we populate our
        // routing table from the configured peers. Failures here are
        // non-fatal (e.g. zero bootstrap peers on the seed relay).
        if !config.bootstrap.is_empty()
            && let Err(err) = swarm.behaviour_mut().kademlia.bootstrap()
        {
            tracing::warn!(?err, "kademlia bootstrap returned error");
        }

        let metrics = Arc::new(RelayMetrics::new());
        let metrics_handle = if let Some(addr) = config.metrics_listen {
            match crate::metrics::spawn_server(addr, metrics.clone()).await {
                Ok((bound, h)) => {
                    tracing::info!(%bound, "metrics endpoint listening");
                    Some(h)
                }
                Err(err) => {
                    tracing::warn!(%addr, ?err, "failed to bind metrics endpoint");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            swarm,
            config,
            keypair,
            topic_matcher,
            metrics,
            metrics_handle,
            rate_limiter: RateLimiter::default(),
            subscribed_topics: HashSet::new(),
            connected_peers: HashSet::new(),
        })
    }

    /// Local peer id — operators publish `<listen>/p2p/<local_peer_id>`
    /// as the bootstrap addr for other relays.
    pub fn local_peer_id(&self) -> PeerId {
        self.keypair.public().to_peer_id()
    }

    /// Reference to the keypair. The relay binary uses this once at
    /// startup to log the public addr; not exposed for re-signing.
    pub fn keypair(&self) -> &identity::Keypair {
        &self.keypair
    }

    /// Snapshot of the metrics struct. Integration tests assert on
    /// counters via this handle.
    pub fn metrics(&self) -> Arc<RelayMetrics> {
        self.metrics.clone()
    }

    /// Drive the swarm until SIGTERM/SIGINT. Returns `Ok(())` on
    /// graceful shutdown, `Err` only if the signal handlers fail to
    /// install.
    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut sigterm = signal(SignalKind::terminate())
            .context("installing SIGTERM handler")?;
        let mut sigint = signal(SignalKind::interrupt())
            .context("installing SIGINT handler")?;

        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    self.handle_swarm_event(event).await;
                }
                _ = sigterm.recv() => {
                    tracing::info!("SIGTERM received; shutting down");
                    break;
                }
                _ = sigint.recv() => {
                    tracing::info!("SIGINT received; shutting down");
                    break;
                }
            }
        }

        if let Some(h) = self.metrics_handle.take() {
            h.abort();
        }
        Ok(())
    }

    /// Drive the swarm until `predicate` returns `Some(value)`.
    /// Intended for integration tests that need to wait for a
    /// specific swarm event without racing the loop.
    pub async fn run_until<F, T>(&mut self, mut predicate: F) -> Option<T>
    where
        F: FnMut(&SwarmEvent<RelayBehaviourEvent>) -> Option<T>,
    {
        loop {
            let event = self.swarm.select_next_some().await;
            let outcome = predicate(&event);
            // Process the event *after* the predicate so the test
            // can observe raw events without our handler having
            // mutated state first. The handler still runs for state
            // bookkeeping.
            self.handle_swarm_event(event).await;
            if outcome.is_some() {
                return outcome;
            }
        }
    }

    /// Single swarm-event tick — exposed for the in-process
    /// integration tests that want to interleave their own logic
    /// with the relay's reactions.
    pub async fn step(&mut self) {
        let event = self.swarm.select_next_some().await;
        self.handle_swarm_event(event).await;
    }

    async fn handle_swarm_event(
        &mut self,
        event: SwarmEvent<RelayBehaviourEvent>,
    ) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                let local = self.local_peer_id();
                tracing::info!(%address, %local, "listening on /p2p/...");
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                self.connected_peers.insert(peer_id);
                self.metrics.connected_peers.set(self.connected_peers.len() as i64);
                tracing::info!(%peer_id, total = self.connected_peers.len(), "peer connected");
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.connected_peers.remove(&peer_id);
                self.metrics.connected_peers.set(self.connected_peers.len() as i64);
                tracing::info!(%peer_id, total = self.connected_peers.len(), "peer disconnected");
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Gossipsub(ev)) => {
                self.handle_gossipsub(ev).await;
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Kademlia(ev)) => {
                self.handle_kademlia(ev);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Identify(ev)) => {
                self.handle_identify(ev);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Mdns(ev)) => {
                self.handle_mdns(ev);
            }
            SwarmEvent::Behaviour(RelayBehaviourEvent::Ping(_)) => {
                // Pings are bookkeeping only; the connection-closed
                // path handles real failures.
            }
            _ => {}
        }
    }

    async fn handle_gossipsub(&mut self, ev: gossipsub::Event) {
        match ev {
            gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            } => {
                let topic = message.topic.as_str().to_string();
                let allowed = self.topic_matcher.is_match(&topic);
                if !allowed {
                    tracing::debug!(%topic, "message on non-allowlisted topic; not forwarded");
                    return;
                }
                let cap = self.config.rate_limit_per_topic_per_minute;
                if !self.rate_limiter.check_and_record(&topic, propagation_source, cap) {
                    tracing::warn!(%topic, peer = %propagation_source, "rate limit hit; dropping");
                    return;
                }
                self.metrics
                    .messages_forwarded
                    .get_or_create(&TopicLabels { topic: topic.clone() })
                    .inc();
                self.metrics.bytes_in.inc_by(message.data.len() as u64);
                tracing::trace!(%topic, peer = %propagation_source, bytes = message.data.len(), "forwarded");
            }
            gossipsub::Event::Subscribed { peer_id, topic } => {
                let topic_str = topic.as_str().to_string();
                if self.topic_matcher.is_match(&topic_str) {
                    // Subscribe ourselves so we're part of the mesh.
                    let ident = gossipsub::IdentTopic::new(topic_str.clone());
                    if self.subscribed_topics.insert(topic_str.clone()) {
                        if let Err(err) = self.swarm.behaviour_mut().gossipsub.subscribe(&ident) {
                            tracing::warn!(%topic_str, ?err, "subscribe failed");
                        } else {
                            tracing::info!(%topic_str, %peer_id, "relay subscribed to topic");
                        }
                    }
                    self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                }
            }
            gossipsub::Event::Unsubscribed { peer_id, topic } => {
                tracing::debug!(topic = %topic.as_str(), %peer_id, "peer unsubscribed");
            }
            _ => {}
        }
    }

    fn handle_kademlia(&mut self, ev: kad::Event) {
        match ev {
            kad::Event::OutboundQueryProgressed { .. } => {
                self.metrics.kademlia_queries.inc();
            }
            kad::Event::RoutingUpdated { peer, .. } => {
                tracing::debug!(%peer, "kademlia routing updated");
            }
            _ => {}
        }
    }

    fn handle_identify(&mut self, ev: identify::Event) {
        if let identify::Event::Received { peer_id, info, .. } = ev {
            tracing::info!(
                %peer_id,
                agent = %info.agent_version,
                protocol = %info.protocol_version,
                "identify",
            );
            // Feed the peer's listen addrs into Kademlia so DHT
            // queries can find them later. Without this, the relay
            // only knows the dialled addr, not the peer's other
            // listening sockets.
            for addr in info.listen_addrs {
                self.swarm
                    .behaviour_mut()
                    .kademlia
                    .add_address(&peer_id, addr);
            }
        }
    }

    fn handle_mdns(&mut self, ev: mdns::Event) {
        match ev {
            mdns::Event::Discovered(list) => {
                for (peer_id, addr) in list {
                    tracing::info!(%peer_id, %addr, "mdns discovered");
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .add_explicit_peer(&peer_id);
                    self.swarm
                        .behaviour_mut()
                        .kademlia
                        .add_address(&peer_id, addr);
                }
            }
            mdns::Event::Expired(list) => {
                for (peer_id, _addr) in list {
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .remove_explicit_peer(&peer_id);
                }
            }
        }
    }
}

/// Extract the `PeerId` from a `/p2p/<id>` component of a multiaddr.
/// Returns `None` if the multiaddr lacks the `p2p` protocol tail.
fn extract_peer_id(addr: &Multiaddr) -> Option<PeerId> {
    use libp2p::multiaddr::Protocol;
    addr.iter().find_map(|p| match p {
        Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_resets_after_minute() {
        let mut rl = RateLimiter::default();
        let peer = PeerId::random();
        // 5 hits, cap 3: first 3 pass, next 2 dropped.
        assert!(rl.check_and_record("qos.a", peer, 3));
        assert!(rl.check_and_record("qos.a", peer, 3));
        assert!(rl.check_and_record("qos.a", peer, 3));
        assert!(!rl.check_and_record("qos.a", peer, 3));
        assert!(!rl.check_and_record("qos.a", peer, 3));

        // Different topic shares no state.
        assert!(rl.check_and_record("qos.b", peer, 3));
        // Different peer shares no state.
        let other = PeerId::random();
        assert!(rl.check_and_record("qos.a", other, 3));
    }

    #[test]
    fn rate_limiter_cap_zero_disables() {
        let mut rl = RateLimiter::default();
        let peer = PeerId::random();
        for _ in 0..10_000 {
            assert!(rl.check_and_record("qos.flood", peer, 0));
        }
    }

    #[test]
    fn extract_peer_id_finds_suffix() {
        let key = identity::Keypair::generate_ed25519();
        let peer = key.public().to_peer_id();
        let addr: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}").parse().unwrap();
        assert_eq!(extract_peer_id(&addr), Some(peer));
    }

    #[test]
    fn extract_peer_id_none_without_suffix() {
        let addr: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
        assert_eq!(extract_peer_id(&addr), None);
    }
}
