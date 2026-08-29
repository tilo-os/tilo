//! libp2p `NetworkBehaviour` for nirios.
//!
//! Combines gossipsub ([`crate::network::protocol::NeighbourMsg`] fan-out) and
//! mDNS (LAN discovery). Peer identity is the machine's device key from
//! [`DEVICE_KEY_PATH`] — the same file the FUSE mount derives its iroh node id
//! from — so the `PeerId` survives restarts and names the same device the
//! filesystem and CRDT author do.
//!
//! Encoded video frames ride a dedicated per-share substream rather than
//! gossipsub (see [`crate::network::surface_stream`]); only the
//! `SurfaceStreamStart`/`End` announcements stay on gossipsub.

use libp2p::{gossipsub, mdns, swarm::NetworkBehaviour};

/// Combined libp2p behaviour for the compositor's peer-sync transport.
/// `derive(NetworkBehaviour)` produces a `NiriosBehaviourEvent` enum with one variant per field, matched to fan messages back out.
#[derive(NetworkBehaviour)]
pub struct NiriosBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

impl NiriosBehaviour {
    /// Build the behaviour with the project-wide defaults: 1 s gossipsub
    /// heartbeat, mesh size 8 (low 4 / high 12), strict signed messages, LAN mDNS discovery keyed off `local_key`'s `PeerId`.
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
        let mdns =
            mdns::tokio::Behaviour::new(mdns::Config::default(), local_key.public().to_peer_id())?;
        Ok(Self { gossipsub, mdns })
    }
}

/// Deterministic 16-bit peer salt from the local libp2p keypair,
/// occupying the high 16 bits of every id
/// [`crate::shell::state::next_id`] mints so two peers minting in parallel cannot collide.
///
/// Hand-rolled FNV-1a-64 rather than a `std` hasher, whose output is not
/// promised stable across Rust versions — the salt must survive
/// restarts. 16 bits is enough for the peer counts nirios targets; guards against accidental collision, not a forged salt.
pub fn peer_salt_from_keypair(kp: &libp2p::identity::Keypair) -> u16 {
    let bytes = kp.public().encode_protobuf();
    let h = fnv1a_64(&bytes);
    // Fold the 64-bit hash down to 16 bits by XOR'ing the four 16-bit
    // slices — keeps every output bit a function of every input bit, roughly uniform collisions.

    (h as u16) ^ ((h >> 16) as u16) ^ ((h >> 32) as u16) ^ ((h >> 48) as u16)
}

/// Fold a `PeerId` to the `u64` the wire carries as an **opaque liveness
/// handle** — never an authority decision: anything turning on *who* a
/// peer is uses [`crate::shell::state::PeerOrigin`] with the full base58
/// id, since an equality check against a 64-bit digest is not an identity check.
///
/// Same hand-rolled FNV-1a as the salt — both ends must agree, and
/// `DefaultHasher` isn't stable across Rust versions, so peers on different toolchains would stop recognising each other's publishes.
pub fn peer_liveness_handle(peer: &libp2p::PeerId) -> u64 {
    fnv1a_64(&peer.to_bytes())
}

/// FNV-1a-64 hash, standard primes from <http://www.isthe.com/chongo/tech/comp/fnv/>.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Where the machine's one device key lives — the FUSE mount reads the
/// same file, so the compositor's libp2p `PeerId` and the filesystem's iroh `NodeId` are two encodings of one identity.
pub const DEVICE_KEY_PATH: &str = "/var/lib/qos/device/keypair.bin";

/// Pre-device-key location of the compositor's own libp2p keypair, read only to migrate an existing installation's peer id to [`DEVICE_KEY_PATH`].
pub const LEGACY_PEER_KEY_PATH: &str = "/var/lib/qos-fs/peer-key";

/// Load the device key at `path` as a libp2p keypair, generating one on
/// first boot. The stored form is the bare 32-byte Ed25519 seed — the
/// same file the filesystem reads for its iroh node key, so the compositor has the device's identity, not one of its own.
///
/// **A key file that exists but cannot be read is an error**: silently
/// replacing it would rejoin the mesh under a new `PeerId`,
/// indistinguishable from first boot, killing every invite naming the
/// old id — the bytes stay on disk so a restore is possible.
///
/// Migrates a libp2p protobuf keypair (the older on-disk shape) to
/// device-key form, preserving the `PeerId` peers already know.
pub fn load_or_create_keypair(
    path: &std::path::Path,
) -> std::io::Result<libp2p::identity::Keypair> {
    if path.exists() {
        return read_device_keypair(path);
    }
    // Nothing at `path`. On a system predating the shared device key, the
    // compositor's own keypair is the identity this machine is already known by — carry it over rather than renaming the machine.
    let legacy = std::path::Path::new(LEGACY_PEER_KEY_PATH);
    if path == std::path::Path::new(DEVICE_KEY_PATH) && legacy.exists() {
        if let Some(seed) = seed_from_libp2p_protobuf(&std::fs::read(legacy)?) {
            write_seed(path, &seed)?;
            return keypair_from_seed(&seed);
        }
        tracing::warn!(
            legacy = %legacy.display(),
            "peer-sync: legacy peer key is not an ed25519 libp2p keypair; minting a device key"
        );
    }
    let seed = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng).to_bytes();
    write_seed(path, &seed)?;
    keypair_from_seed(&seed)
}

/// Read an existing key file, migrating the older libp2p-protobuf shape in place; every failure is returned, none is a reason to re-key.
fn read_device_keypair(path: &std::path::Path) -> std::io::Result<libp2p::identity::Keypair> {
    match qos_user_identity::persist::read_device_key(path) {
        Ok(sk) => keypair_from_seed(&sk.to_bytes()),
        Err(qos_user_identity::PersistError::WrongKeyLength(_)) => {
            // Not 32 bytes — either the older protobuf keypair (migratable without
            // changing the identity) or damage, which must be reported.
            let bytes = std::fs::read(path)?;
            let seed = seed_from_libp2p_protobuf(&bytes).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{} is neither a 32-byte device key nor an ed25519 libp2p keypair \
                         ({} bytes); refusing to replace it",
                        path.display(),
                        bytes.len()
                    ),
                )
            })?;
            write_seed(path, &seed)?;
            keypair_from_seed(&seed)
        }
        Err(e) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} could not be read as a device key: {e}", path.display()),
        )),
    }
}

/// Pull the Ed25519 seed out of a libp2p protobuf-encoded keypair, or `None` if the bytes aren't one (or a non-ed25519 key type).
fn seed_from_libp2p_protobuf(bytes: &[u8]) -> Option<[u8; 32]> {
    let kp = libp2p::identity::Keypair::from_protobuf_encoding(bytes).ok()?;
    let ed = kp.try_into_ed25519().ok()?;
    ed.secret().as_ref().try_into().ok()
}

/// Expand a seed into the libp2p keypair — same 32 bytes, same public key, so the `PeerId` is a function of the device key alone.
fn keypair_from_seed(seed: &[u8; 32]) -> std::io::Result<libp2p::identity::Keypair> {
    libp2p::identity::Keypair::ed25519_from_bytes(*seed).map_err(std::io::Error::other)
}

/// Write the seed at the device-key mode with no window where it's
/// readable at its final path — a write-then-chmod would briefly leave the private key at the process umask.
fn write_seed(path: &std::path::Path, seed: &[u8; 32]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    qos_user_identity::persist::write_device_key(path, &ed25519_dalek::SigningKey::from_bytes(seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity::Keypair;

    // `mdns::tokio::Behaviour::new` calls `tokio::net::UdpSocket::from_std`,
    // which needs a running Tokio runtime, so the smoke test runs under `#[tokio::test]` even without awaiting anything.
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

    /// A key file that exists but cannot be read must raise: overwriting it
    /// would rejoin the mesh under a new `PeerId`, silently killing every invite that names the old one — identity loss looking like first boot.
    #[test]
    fn unreadable_key_file_raises_instead_of_rotating_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        std::fs::write(&path, b"not a key, not a keypair").expect("write junk");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let err = load_or_create_keypair(&path).expect_err("must not replace the file");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"not a key, not a keypair",
            "the bytes must be left on disk so a restore is possible"
        );
    }

    /// A key reachable by users outside the device-trust group is a
    /// reportable condition, not a reason to mint a replacement —
    /// group-readable is expected, since the filesystem daemon and the compositor read this file as two different uids.
    #[test]
    fn key_reachable_outside_the_trust_group_raises_instead_of_rotating() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        let kp = load_or_create_keypair(&path).expect("first load creates");
        let before = kp.public().to_peer_id();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            load_or_create_keypair(&path).is_err(),
            "a key any local user can read must be reported"
        );

        // Still the same key on disk — tightening the mode recovers it, and group-read is a legitimate mode for this file.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            load_or_create_keypair(&path).unwrap().public().to_peer_id(),
            before
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_or_create_keypair(&path).unwrap().public().to_peer_id(),
            before
        );
    }

    /// An installation that already had a libp2p protobuf keypair keeps its
    /// `PeerId` — the seed is adopted into the device-key format rather than the machine being renamed.
    #[test]
    fn legacy_libp2p_keypair_is_adopted_without_changing_the_peer_id() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");

        let legacy = Keypair::generate_ed25519();
        let expected = legacy.public().to_peer_id();
        std::fs::write(&path, legacy.to_protobuf_encoding().unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let loaded = load_or_create_keypair(&path).expect("migrates in place");
        assert_eq!(
            loaded.public().to_peer_id(),
            expected,
            "migration must preserve the identity peers already know"
        );
        // Rewritten in device-key form, so the mount reads the same file.
        assert_eq!(std::fs::read(&path).unwrap().len(), 32);
        assert_eq!(
            load_or_create_keypair(&path).unwrap().public().to_peer_id(),
            expected
        );
    }

    /// The libp2p `PeerId` is a function of the device key alone: the same
    /// 32 bytes the FUSE mount reads for its iroh node id — without this the compositor and filesystem are two anonymous identities on one machine.
    #[test]
    fn peer_id_is_derived_from_the_device_key_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        let kp = load_or_create_keypair(&path).expect("creates");

        let seed: [u8; 32] = std::fs::read(&path).unwrap().try_into().expect("32 bytes");
        let expected = Keypair::ed25519_from_bytes(seed).unwrap();
        assert_eq!(kp.public().to_peer_id(), expected.public().to_peer_id());

        // …and it's the public key dalek derives from the same seed, which is what makes the peer id, node id and CRDT author one identity.
        let dalek = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(
            kp.public().try_into_ed25519().unwrap().to_bytes(),
            dalek.verifying_key().to_bytes()
        );
    }

    /// Pinned against a literal so a moved derivation fails here rather than two toolchains silently disagreeing on the wire.
    #[test]
    fn peer_liveness_handle_is_pinned_to_the_peer_id() {
        let kp = Keypair::ed25519_from_bytes([3u8; 32]).unwrap();
        let id = kp.public().to_peer_id();
        let h = peer_liveness_handle(&id);
        assert_eq!(
            h, 0xa19b_77cb_7c08_58d7_u64,
            "the liveness handle derivation moved; two peers on different \
             builds would stop recognising each other's publishes"
        );

        let other = Keypair::ed25519_from_bytes([4u8; 32]).unwrap();
        assert_ne!(peer_liveness_handle(&other.public().to_peer_id()), h);
    }

    #[test]
    fn keypair_is_created_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("peer-key");
        assert!(!nested.exists());

        let _kp = load_or_create_keypair(&nested).expect("creates dirs + file");
        assert!(nested.exists(), "key file must exist after first call");
    }

    /// A non-deterministic salt would change on every restart and partition the workspace tree against itself.
    #[test]
    fn peer_salt_is_deterministic_from_keypair() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("peer-key");

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

        // No exact != assert here (1/65536 collision odds would make it flaky)
        // — just exercise the derivation on a second keypair so a panic surfaces.
        let other = Keypair::generate_ed25519();
        let _salt_other = peer_salt_from_keypair(&other);
    }

    /// Default `std::fs::write` uses the umask (typically `0644`), which would let any local user copy the key and impersonate this device.
    #[test]
    fn load_or_create_keypair_writes_the_device_key_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        let _kp = load_or_create_keypair(&path).expect("creates key");

        let meta = std::fs::metadata(&path).expect("stat key file");
        // Compare the low 12 bits (file-type bits live above 0o7777).
        let mode = meta.permissions().mode() & 0o7777;
        assert_eq!(
            mode,
            qos_user_identity::persist::DEVICE_KEY_MODE,
            "device key must be mode {:o}, got {mode:o}",
            qos_user_identity::persist::DEVICE_KEY_MODE
        );
        assert_eq!(mode & 0o007, 0, "no access for anyone outside the group");
    }
}
