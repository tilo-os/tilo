//! libp2p `NetworkBehaviour` for `qos-relay`.
//!
//! Composes the protocols the relay needs to participate fully in
//! the qOS mesh:
//!
//! - **gossipsub**: the forwarding mesh. Configured identically to
//!   `shell/src/network/behaviour.rs` (1 s heartbeat, mesh sizes
//!   4/8/12, `ValidationMode::Strict`, signed authenticity) so the
//!   relay is wire-compatible with every shell on the network.
//! - **kademlia**: DHT bootstrap. The relay runs in `Server` mode so
//!   it answers find-peer queries from NATted clients.
//! - **identify**: announces the agent string `qos-relay/<version>`
//!   so operators can fingerprint the network's relay topology.
//! - **mdns**: LAN peer discovery for development. Disabled
//!   silently if the host kernel doesn't allow multicast (the
//!   `mdns::tokio::Behaviour::new` `Result` is propagated).
//! - **ping**: keep-alive so connections through stateful NATs
//!   don't get evicted while idle.

use libp2p::StreamProtocol;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{gossipsub, identify, identity, kad, mdns, ping};

/// Agent string the relay advertises via `identify`. Bumps with
/// the crate version; operators can pattern-match against this in
/// network telemetry.
pub const AGENT_STRING: &str = concat!("qos-relay/", env!("CARGO_PKG_VERSION"));

/// Protocol identifier for the qOS Kademlia DHT. Distinct from the
/// default `/ipfs/kad/1.0.0` so we don't get tangled with public
/// IPFS DHTs that share a transport.
pub const KAD_PROTOCOL: &str = "/qos/kad/1.0.0";

/// Protocol identifier for `identify`. Same namespacing rationale
/// as [`KAD_PROTOCOL`].
pub const IDENTIFY_PROTOCOL: &str = "/qos/identify/1.0.0";

#[derive(NetworkBehaviour)]
pub struct RelayBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub ping: ping::Behaviour,
}

impl RelayBehaviour {
    /// Build the combined behaviour from the relay's keypair.
    /// Errors only surface if mdns can't bind its multicast socket
    /// (kernel without IP_MULTICAST_TTL etc.) or if gossipsub's
    /// config builder rejects the parameters (which is fatal — the
    /// values are hard-coded).
    pub fn new(
        local_key: &identity::Keypair,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // ---- gossipsub --------------------------------------------------
        let gs_cfg = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(std::time::Duration::from_secs(1))
            .mesh_n(8)
            .mesh_n_low(4)
            .mesh_n_high(12)
            .validation_mode(gossipsub::ValidationMode::Strict)
            // Forwarding-friendly: keep our cache deep enough that we
            // serve IWANT replies for a few seconds. Default is 5s/3
            // history-length-gossip; we leave that.
            .build()?;
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key.clone()),
            gs_cfg,
        )?;

        // ---- kademlia ---------------------------------------------------
        let mut kad_cfg = kad::Config::new(StreamProtocol::new(KAD_PROTOCOL));
        kad_cfg.set_query_timeout(std::time::Duration::from_secs(60));
        let store = kad::store::MemoryStore::new(local_key.public().to_peer_id());
        let mut kademlia =
            kad::Behaviour::with_config(local_key.public().to_peer_id(), store, kad_cfg);
        // Server-mode so we answer GetClosestPeers queries from NATted
        // clients — the whole point of running a relay.
        kademlia.set_mode(Some(kad::Mode::Server));

        // ---- identify ---------------------------------------------------
        let identify = identify::Behaviour::new(
            identify::Config::new(IDENTIFY_PROTOCOL.to_string(), local_key.public())
                .with_agent_version(AGENT_STRING.to_string()),
        );

        // ---- mdns -------------------------------------------------------
        let mdns =
            mdns::tokio::Behaviour::new(mdns::Config::default(), local_key.public().to_peer_id())?;

        // ---- ping -------------------------------------------------------
        let ping = ping::Behaviour::new(ping::Config::new());

        Ok(Self {
            gossipsub,
            kademlia,
            identify,
            mdns,
            ping,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn behaviour_constructs_with_default_config() {
        let key = identity::Keypair::generate_ed25519();
        let _b = RelayBehaviour::new(&key).expect("builds");
    }

    #[test]
    fn agent_string_carries_version() {
        assert!(AGENT_STRING.starts_with("qos-relay/"));
        // The version is parseable as semver-ish (at least non-empty).
        let suffix = AGENT_STRING.strip_prefix("qos-relay/").unwrap();
        assert!(!suffix.is_empty());
    }
}
