//! libp2p `NetworkBehaviour` for nirios — Phase 2 Task 2.1.
//!
//! Combines gossipsub (for [`crate::network::protocol::NeighbourMsg`] fan-out
//! across the workspace topic) and mDNS (for LAN peer discovery) into a single
//! derived behaviour. The peer identity is an ed25519 keypair persisted at
//! `/var/lib/qos-fs/peer-key` so the same `PeerId` survives compositor
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

/// Derive a deterministic 16-bit peer salt from the local libp2p
/// keypair (C-1). The salt occupies the high 16 bits of every id
/// minted by [`crate::shell::state::next_id`] so two peers minting
/// ids in parallel can never collide.
///
/// Properties we require:
/// - **Deterministic**: a given keypair always maps to the same salt
///   so the salt survives compositor restarts (the keypair on disk
///   does too). Stable hashers in `std` are explicitly NOT promised
///   across Rust versions, so we hand-roll an FNV-1a-64 fold over the
///   protobuf-encoded public key.
/// - **Cheap**: called once at boot; the fold runs in a few hundred
///   nanoseconds against the ~50-byte ed25519 public key.
/// - **No collision-resistance promises**: 16 bits gives 65 536 distinct
///   salts, which is enough to make accidental collisions vanishingly
///   unlikely for the small peer counts nirios targets (single-digit
///   to low-double-digit collaborators). A cryptographic guarantee is
///   not required because the id format only protects against
///   accidental cross-peer collision under CRDT sync, not against an
///   adversary actively forging an id with a chosen salt.
pub fn peer_salt_from_keypair(kp: &libp2p::identity::Keypair) -> u16 {
    let bytes = kp.public().encode_protobuf();
    let h = fnv1a_64(&bytes);
    // Fold the 64-bit hash down to 16 bits by XOR'ing the four
    // 16-bit slices. Keeps every output bit a function of every input
    // bit; collisions are roughly uniform.
    let h16 = (h as u16) ^ ((h >> 16) as u16) ^ ((h >> 32) as u16) ^ ((h >> 48) as u16);
    h16
}

/// FNV-1a-64 hash. Hand-rolled rather than pulled from a crate because
/// the algorithm is a 5-line constant-time fold and we don't need any
/// additional surface area. Standard primes from
/// <http://www.isthe.com/chongo/tech/comp/fnv/>.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
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
    // C-8 (security): the ed25519 private key impersonates this peer
    // in the libp2p mesh. Default `std::fs::write` honours the
    // process umask which is typically `0644` — that lets any local
    // user copy the key. Tighten to `0600` immediately after the
    // write. A separate process that beat us between the write and
    // the chmod can only see the bytes for that brief window; we
    // can't atomically create-with-mode without an `openat2` dance,
    // which is more complexity than this single-host secret needs.
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
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

    /// C-1: the peer salt derived from a keypair must be deterministic
    /// — the same keypair always maps to the same 16-bit salt. The salt
    /// gets persisted indirectly via the keypair on disk, so a
    /// non-deterministic derivation would change the salt on every
    /// restart and partition the workspace tree against itself.
    #[test]
    fn peer_salt_is_deterministic_from_keypair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("peer-key");

        // Load the same keypair twice and derive the salt from each.
        // `load_or_create_keypair` is the production entry point so we
        // exercise the full round-trip (generate → disk → reload → hash).
        let kp1 = load_or_create_keypair(&path).expect("first load creates");
        let kp2 = load_or_create_keypair(&path).expect("second load reads");

        let salt1 = peer_salt_from_keypair(&kp1);
        let salt2 = peer_salt_from_keypair(&kp2);

        assert_eq!(
            salt1, salt2,
            "peer_salt_from_keypair must be deterministic for a given \
             keypair; got salt1=0x{:04x} salt2=0x{:04x} (C-1 regression)",
            salt1, salt2
        );

        // Sanity: two distinct keypairs almost certainly yield distinct
        // salts (collision space is 1/65536; the test would be flaky
        // only at astronomical odds). Skip an exact != assert to avoid
        // a 1/65536 flake; just exercise the derivation on a second
        // independent keypair so a panic in FNV-1a etc. surfaces.
        let other = Keypair::generate_ed25519();
        let _salt_other = peer_salt_from_keypair(&other);
    }

    /// C-8 (security): the freshly-generated peer keypair must land on
    /// disk with mode `0600`. Default `std::fs::write` uses the process
    /// umask which is typically `0644` — that lets any local user copy
    /// the ed25519 private key and impersonate this peer in the libp2p
    /// mesh. Regression-guard the mode bits explicitly.
    #[test]
    fn load_or_create_keypair_sets_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("peer-key");
        let _kp = load_or_create_keypair(&path).expect("creates key");

        let meta = std::fs::metadata(&path).expect("stat key file");
        let mode = meta.permissions().mode();
        // Compare the low 12 bits (file-type bits live above 0o7777).
        assert_eq!(
            mode & 0o7777,
            0o600,
            "peer keypair must be mode 0600, got {:o} (full mode: {:o})",
            mode & 0o7777,
            mode
        );
    }
}
