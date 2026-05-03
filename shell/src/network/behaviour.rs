//! libp2p `NetworkBehaviour` for nirios — Phase 2 Task 2.1.
//!
//! Combines gossipsub (for [`crate::network::protocol::NeighbourMsg`] fan-out
//! across the workspace topic) and mDNS (for LAN peer discovery) into a single
//! derived behaviour. The peer identity is an ed25519 keypair persisted at
//! `/var/lib/quick-fs/peer-key` so the same `PeerId` survives compositor
//! restarts.

use libp2p::{gossipsub, mdns, swarm::NetworkBehaviour};

/// Combined libp2p behaviour for the compositor's peer-sync transport.
///
/// `derive(NetworkBehaviour)` produces a `NiriosBehaviourEvent` enum with one
/// variant per field; the actor matches on it to fan messages back out.
#[derive(NetworkBehaviour)]
pub struct NiriosBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

impl NiriosBehaviour {
    /// Build the behaviour with the project-wide defaults: 1 s gossipsub
    /// heartbeat, mesh size 8 (low 4 / high 12), strict signed messages,
    /// LAN mDNS discovery keyed off `local_key`'s `PeerId`.
    pub fn new(
        local_key: &libp2p::identity::Keypair,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let cfg = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(std::time::Duration::from_secs(1))
            .mesh_n(8)
            .mesh_n_low(4)
            .mesh_n_high(12)
            .validation_mode(gossipsub::ValidationMode::Strict)
            .build()?;
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(local_key.clone()),
            cfg,
        )?;
        let mdns = mdns::tokio::Behaviour::new(
            mdns::Config::default(),
            local_key.public().to_peer_id(),
        )?;
        Ok(Self { gossipsub, mdns })
    }
}

/// Load (or create) the persistent ed25519 keypair at `path`.
///
/// On first call (or if the file is unreadable / unparseable) a fresh
/// ed25519 keypair is generated, written to `path` (creating any missing
/// parent directories), and returned. Subsequent calls return the same
/// keypair so the compositor's `PeerId` is stable across restarts.
pub fn load_or_create_keypair(
    path: &std::path::Path,
) -> std::io::Result<libp2p::identity::Keypair> {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        if let Ok(kp) = libp2p::identity::Keypair::from_protobuf_encoding(&bytes) {
            return Ok(kp);
        }
        // File exists but is unreadable as a keypair — fall through and
        // overwrite it with a fresh one rather than panicking.
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let kp = libp2p::identity::Keypair::generate_ed25519();
    let bytes = kp
        .to_protobuf_encoding()
        .map_err(std::io::Error::other)?;
    std::fs::write(path, bytes)?;
    Ok(kp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity::Keypair;

    // `mdns::tokio::Behaviour::new` calls `tokio::net::UdpSocket::from_std`
    // which requires a running Tokio runtime, so the smoke test runs under
    // `#[tokio::test]` even though we don't await anything.
    #[tokio::test]
    async fn behaviour_constructs_with_default_config() {
        let key = Keypair::generate_ed25519();
        let _behaviour = NiriosBehaviour::new(&key).expect("behaviour builds");
    }

    #[test]
    fn keypair_persists_across_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("peer-key");

        let kp1 = load_or_create_keypair(&path).expect("first load creates");
        let kp2 = load_or_create_keypair(&path).expect("second load reads");

        assert_eq!(kp1.public().to_peer_id(), kp2.public().to_peer_id());
    }

    #[test]
    fn keypair_is_created_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("peer-key");
        assert!(!nested.exists());

        let _kp = load_or_create_keypair(&nested).expect("creates dirs + file");
        assert!(nested.exists(), "key file must exist after first call");
    }
}
