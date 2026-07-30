//! libp2p `NetworkBehaviour` for nirios.
//!
//! Combines gossipsub (for [`crate::network::protocol::NeighbourMsg`] fan-out
//! across the workspace topic) and mDNS (for LAN peer discovery) into a single
//! derived behaviour. The peer identity is the machine's device key, read from
//! [`DEVICE_KEY_PATH`] — the same file the FUSE mount derives its iroh node id
//! from — so the `PeerId` survives compositor restarts *and* names the same
//! device the filesystem and the CRDT author do.
//!
//! # Video substream
//!
//! Encoded video frames ride a dedicated per-share substream — not
//! gossipsub. See [`crate::network::surface_stream`] for the framing
//! format (u64 sequence, u32 length, u8 keyframe flag) and the
//! per-subscriber bounded queue with drop-oldest backpressure.
//!
//! `NeighbourMsg::SurfaceStreamStart` / `SurfaceStreamEnd` remain on
//! gossipsub for workspace-wide discovery — peers use those to open
//! the substream. Only frame bytes ride the direct substream.
//!
//! The QUIC transport itself is available through `libp2p_quic 0.13`
//! (pulled in transitively via `libp2p`), so a follow-up can extend
//! this behaviour to open `substream` handles alongside the existing
//! TCP/yamux transport without adding a new dep. For v1 the substream
//! substrate lives in [`crate::network::surface_stream`] and can be
//! driven over any bidirectional byte stream — the wire binding is
//! decoupled from the framing.

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
        let mdns =
            mdns::tokio::Behaviour::new(mdns::Config::default(), local_key.public().to_peer_id())?;
        Ok(Self { gossipsub, mdns })
    }
}

/// Derive a deterministic 16-bit peer salt from the local libp2p
/// keypair. The salt occupies the high 16 bits of every id
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

/// Fold a `PeerId` to the `u64` the wire carries as an **opaque
/// liveness handle** — `Heartbeat::peer`, `PresenceMoved::peer`, and the
/// self-filter on cooperative-scrollback messages.
///
/// Never an authority decision. Anything that turns on *who* a peer is
/// uses [`crate::shell::state::PeerOrigin`], which carries the whole
/// base58 peer id: an equality check a 64-bit digest can be ground
/// against is not an identity check.
///
/// Both ends of a link must agree on this value, so it uses the same
/// hand-rolled FNV-1a as the id salt rather than
/// `std::collections::hash_map::DefaultHasher`, whose output std
/// explicitly does not promise across Rust versions — two peers built
/// with different toolchains would compute different handles for the
/// same peer and each would stop recognising its own publishes.
pub fn peer_liveness_handle(peer: &libp2p::PeerId) -> u64 {
    fnv1a_64(&peer.to_bytes())
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

/// Where the machine's one device key lives. The FUSE mount reads the
/// same file, so the compositor's libp2p `PeerId` and the filesystem's
/// iroh `NodeId` are two encodings of a single identity.
pub const DEVICE_KEY_PATH: &str = "/var/lib/qos/device/keypair.bin";

/// Where the compositor used to keep a libp2p keypair of its own,
/// unrelated to any other identity on the machine. Read once, to carry
/// an existing installation's peer id over to [`DEVICE_KEY_PATH`] — see
/// [`load_or_create_keypair`].
pub const LEGACY_PEER_KEY_PATH: &str = "/var/lib/qos-fs/peer-key";

/// Load the device key at `path` and present it as a libp2p keypair,
/// generating one on first boot.
///
/// The stored form is the bare 32-byte Ed25519 seed — the same file, in
/// the same format, that the filesystem reads for its iroh node key. So
/// the compositor does not have a network identity of its own: it has
/// the device's, expressed in libp2p's encoding.
///
/// **A key file that exists but cannot be read is an error.** It used to
/// be silently replaced with a fresh keypair, which made identity loss
/// indistinguishable from first boot: the machine rejoined the mesh
/// under a new `PeerId`, every invite naming the old one was dead, and
/// nothing reported a problem. Now a bad mode, a wrong length, or an
/// I/O failure all propagate, and the bytes are left on disk so a
/// restore is still possible.
///
/// One migration is performed: when `path` holds a libp2p protobuf
/// keypair (the older on-disk shape) its Ed25519 seed is adopted and the
/// file rewritten in device-key form, so an existing installation keeps
/// the `PeerId` its peers already know it by.
pub fn load_or_create_keypair(
    path: &std::path::Path,
) -> std::io::Result<libp2p::identity::Keypair> {
    if path.exists() {
        return read_device_keypair(path);
    }
    // Nothing at `path`. On a system that predates the shared device
    // key, the compositor's own keypair is the identity this machine is
    // already known by — carry it over rather than renaming the machine.
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

/// Read an existing key file, migrating the older libp2p-protobuf shape
/// in place. Every failure is returned; none is a reason to re-key.
fn read_device_keypair(
    path: &std::path::Path,
) -> std::io::Result<libp2p::identity::Keypair> {
    match qos_user_identity::persist::read_signing_key(path) {
        Ok(sk) => keypair_from_seed(&sk.to_bytes()),
        Err(qos_user_identity::PersistError::WrongKeyLength(_)) => {
            // Not 32 bytes. Either the older protobuf keypair — which
            // we can migrate without changing the identity — or damage,
            // which must be reported.
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

/// Pull the Ed25519 seed out of a libp2p protobuf-encoded keypair, or
/// `None` if the bytes are not one (or are a non-ed25519 key type).
fn seed_from_libp2p_protobuf(bytes: &[u8]) -> Option<[u8; 32]> {
    let kp = libp2p::identity::Keypair::from_protobuf_encoding(bytes).ok()?;
    let ed = kp.try_into_ed25519().ok()?;
    Some(ed.secret().as_ref().try_into().ok()?)
}

/// Expand a seed into the libp2p keypair. Same 32 bytes, same public
/// key, so the `PeerId` is a function of the device key alone.
fn keypair_from_seed(seed: &[u8; 32]) -> std::io::Result<libp2p::identity::Keypair> {
    libp2p::identity::Keypair::ed25519_from_bytes(*seed).map_err(std::io::Error::other)
}

/// Write the seed with mode `0600` and no window where it is readable
/// at its final path — the private key impersonates this device on the
/// mesh, and the previous write-then-chmod left it briefly at the
/// process umask.
fn write_seed(path: &std::path::Path, seed: &[u8; 32]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    qos_user_identity::persist::write_signing_key(
        path,
        &ed25519_dalek::SigningKey::from_bytes(seed),
    )
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

    /// The defining property of P2-2's fix: a key file that exists but
    /// cannot be read must raise. Overwriting it would rejoin the mesh
    /// under a new `PeerId`, silently killing every invite that names
    /// the old one, with identity loss looking exactly like first boot.
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

    /// A world-readable key is a reportable condition too, not a reason
    /// to mint a replacement.
    #[test]
    fn world_readable_key_file_raises_instead_of_rotating_identity() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        let kp = load_or_create_keypair(&path).expect("first load creates");
        let before = kp.public().to_peer_id();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(
            load_or_create_keypair(&path).is_err(),
            "a key readable by other local users must be reported"
        );
        // Still the same key on disk — tightening the mode recovers it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            load_or_create_keypair(&path).unwrap().public().to_peer_id(),
            before
        );
    }

    /// An installation that already had a libp2p protobuf keypair keeps
    /// its `PeerId` — the seed is adopted into the device-key format
    /// rather than the machine being renamed.
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

    /// The libp2p `PeerId` is a function of the device key alone: the
    /// same 32 bytes the FUSE mount reads for its iroh node id. Without
    /// this the compositor and the filesystem are two anonymous
    /// identities on one machine.
    #[test]
    fn peer_id_is_derived_from_the_device_key_seed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        let kp = load_or_create_keypair(&path).expect("creates");

        let seed: [u8; 32] = std::fs::read(&path).unwrap().try_into().expect("32 bytes");
        let expected = Keypair::ed25519_from_bytes(seed).unwrap();
        assert_eq!(kp.public().to_peer_id(), expected.public().to_peer_id());

        // …and it is the public key dalek derives from the same seed,
        // which is what makes the peer id, the node id and the CRDT
        // author one identity.
        let dalek = ed25519_dalek::SigningKey::from_bytes(&seed);
        assert_eq!(
            kp.public().try_into_ed25519().unwrap().to_bytes(),
            dalek.verifying_key().to_bytes()
        );
    }

    /// Both ends of a link compute the liveness handle independently,
    /// so it has to be a fixed function of the peer id — not
    /// `DefaultHasher`, whose value std may change between Rust
    /// releases. Pinned against a literal: if the derivation moves,
    /// this fails rather than two toolchains silently disagreeing.
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
