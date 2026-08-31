//! Iroh endpoint identity helpers for qOS peer sync.
//!
//! Every network protocol on a device uses the same Ed25519 seed from
//! [`DEVICE_KEY_PATH`]. Iroh exposes the public half directly as an
//! [`iroh::EndpointId`], so there is no transport-specific identity encoding
//! to keep in sync with the filesystem.

/// Deterministic 16-bit peer salt from the durable iroh endpoint key.
pub fn peer_salt_from_keypair(key: &iroh::SecretKey) -> u16 {
    let h = fnv1a_64(key.public().as_bytes());
    ((h & 0xFFFF) as u16)
        ^ (((h >> 16) & 0xFFFF) as u16)
        ^ (((h >> 32) & 0xFFFF) as u16)
        ^ (((h >> 48) & 0xFFFF) as u16)
}

/// Fold an authenticated endpoint id to the numeric liveness handle carried
/// by presence messages. Authority decisions retain the complete endpoint id.
pub fn peer_liveness_handle(peer: &iroh::EndpointId) -> u64 {
    fnv1a_64(peer.as_bytes())
}

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

/// Machine-wide device key used by the compositor, filesystem and host.
pub const DEVICE_KEY_PATH: &str = "/var/lib/qos/device/keypair.bin";

/// Pre-device-key location used by the libp2p compositor. It is read only
/// during a one-way migration into [`DEVICE_KEY_PATH`].
pub const LEGACY_PEER_KEY_PATH: &str = "/var/lib/qos-fs/peer-key";

/// Load the device key, creating it only when the path is genuinely absent.
/// Malformed, unreadable or over-permissive files are never replaced.
///
/// # Errors
///
/// Returns an error if the key file exists but is malformed or unreadable,
/// if migrating a legacy libp2p key fails, or if creating a fresh key or its
/// parent directory fails.
pub fn load_or_create_keypair(path: &std::path::Path) -> std::io::Result<iroh::SecretKey> {
    load_or_create_keypair_with_legacy(path, std::path::Path::new(LEGACY_PEER_KEY_PATH))
}

fn load_or_create_keypair_with_legacy(
    path: &std::path::Path,
    legacy_path: &std::path::Path,
) -> std::io::Result<iroh::SecretKey> {
    let canonical_paths = qos_user_identity::IdentityPaths::under(
        path.parent()
            .and_then(std::path::Path::parent)
            .unwrap_or_else(|| std::path::Path::new("/var/lib/qos")),
    );

    let signing = if path == canonical_paths.device_privkey {
        if !path.try_exists()? && legacy_path.try_exists()? {
            migrate_legacy_path(&canonical_paths, legacy_path)?
        } else {
            match qos_user_identity::load_or_create_device_key(&canonical_paths) {
                Ok((key, _)) => key,
                Err(qos_user_identity::PersistError::WrongKeyLength(_)) => {
                    migrate_protobuf_key(path)?
                }
                Err(error) => return Err(persist_error(error)),
            }
        }
    } else if path.exists() {
        match qos_user_identity::persist::read_device_key(path) {
            Ok(key) => key,
            Err(qos_user_identity::PersistError::WrongKeyLength(_)) => migrate_protobuf_key(path)?,
            Err(error) => return Err(persist_error(error)),
        }
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key = ed25519_dalek::SigningKey::generate(&mut rand_core::OsRng);
        qos_user_identity::persist::write_device_key(path, &key)?;
        key
    };
    Ok(iroh::SecretKey::from_bytes(&signing.to_bytes()))
}

fn migrate_legacy_path(
    paths: &qos_user_identity::IdentityPaths,
    legacy_path: &std::path::Path,
) -> std::io::Result<ed25519_dalek::SigningKey> {
    let bytes = std::fs::read(legacy_path)?;
    let seed = seed_from_libp2p_protobuf(&bytes).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is not an Ed25519 libp2p private key; refusing to replace device identity",
                legacy_path.display()
            ),
        )
    })?;
    qos_user_identity::persist::adopt_device_key(paths, &seed).map_err(persist_error)
}

fn migrate_protobuf_key(path: &std::path::Path) -> std::io::Result<ed25519_dalek::SigningKey> {
    let bytes = std::fs::read(path)?;
    let seed = seed_from_libp2p_protobuf(&bytes).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is neither a 32-byte device key nor an Ed25519 libp2p private key; refusing to replace it",
                path.display()
            ),
        )
    })?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    qos_user_identity::persist::write_device_key(path, &signing)?;
    Ok(signing)
}

/// Decode the only legacy private-key shape qOS ever generated:
/// protobuf `PrivateKey { Type: Ed25519, Data: secret || public }`.
/// Keeping this tiny decoder local avoids retaining libp2p just for migration.
fn seed_from_libp2p_protobuf(bytes: &[u8]) -> Option<[u8; 32]> {
    const HEADER: &[u8] = &[0x08, 0x01, 0x12, 0x40];
    if bytes.len() != HEADER.len().saturating_add(64) || !bytes.starts_with(HEADER) {
        return None;
    }
    let keypair: [u8; 64] = bytes.get(HEADER.len()..)?.try_into().ok()?;
    let seed: [u8; 32] = keypair[..32].try_into().ok()?;
    let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
    if signing.verifying_key().to_bytes() != keypair[32..] {
        return None;
    }
    Some(seed)
}

/// Convert an Ed25519 libp2p `PeerId` to the Iroh endpoint that uses the same
/// public key. Only inline-public-key `PeerIds` are accepted.
pub fn endpoint_from_legacy_peer_id(text: &str) -> Option<iroh::EndpointId> {
    const PREFIX: &[u8] = &[0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
    let bytes = bs58::decode(text).into_vec().ok()?;
    if bytes.len() != PREFIX.len().saturating_add(32) || !bytes.starts_with(PREFIX) {
        return None;
    }
    let public: [u8; 32] = bytes.get(PREFIX.len()..)?.try_into().ok()?;
    iroh::EndpointId::from_bytes(&public).ok()
}

fn persist_error(error: qos_user_identity::PersistError) -> std::io::Error {
    let kind = match error {
        qos_user_identity::PersistError::Io(ref e) => e.kind(),
        _ => std::io::ErrorKind::InvalidData,
    };
    std::io::Error::new(kind, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_persists_and_is_the_iroh_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("device/keypair.bin");
        let first = load_or_create_keypair(&path).expect("create");
        let second = load_or_create_keypair(&path).expect("load");
        assert_eq!(first.public(), second.public());
        let seed: [u8; 32] = std::fs::read(&path).unwrap().try_into().unwrap();
        assert_eq!(first.to_bytes(), seed);
    }

    #[test]
    fn malformed_key_is_never_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        std::fs::write(&path, b"not a key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            load_or_create_keypair(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(path).unwrap(), b"not a key");
    }

    fn legacy_protobuf(seed: [u8; 32]) -> Vec<u8> {
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let mut bytes = vec![0x08, 0x01, 0x12, 0x40];
        bytes.extend_from_slice(&seed);
        bytes.extend_from_slice(&signing.verifying_key().to_bytes());
        bytes
    }

    #[test]
    fn protobuf_device_key_is_migrated_in_place() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("device/keypair.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, legacy_protobuf([7; 32])).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let key = load_or_create_keypair_with_legacy(&path, &dir.path().join("unused")).unwrap();
        assert_eq!(key.to_bytes(), [7; 32]);
        assert_eq!(std::fs::read(path).unwrap(), [7; 32]);
    }

    #[test]
    fn legacy_path_is_adopted_when_device_key_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("device/keypair.bin");
        let legacy = dir.path().join("qos-fs/peer-key");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, legacy_protobuf([8; 32])).unwrap();

        let key = load_or_create_keypair_with_legacy(&path, &legacy).unwrap();
        assert_eq!(key.to_bytes(), [8; 32]);
        assert_eq!(std::fs::read(path).unwrap(), [8; 32]);
    }

    #[test]
    fn legacy_peer_id_maps_to_same_iroh_public_key() {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let mut peer_id = vec![0x00, 0x24, 0x08, 0x01, 0x12, 0x20];
        peer_id.extend_from_slice(&signing.verifying_key().to_bytes());
        let peer_id = bs58::encode(peer_id).into_string();
        assert_eq!(
            endpoint_from_legacy_peer_id(&peer_id),
            Some(iroh::SecretKey::from_bytes(&[9; 32]).public())
        );
    }

    #[test]
    fn liveness_and_salt_are_stable() {
        let key = iroh::SecretKey::from_bytes(&[3; 32]);
        assert_eq!(peer_salt_from_keypair(&key), peer_salt_from_keypair(&key));
        assert_eq!(
            peer_liveness_handle(&key.public()),
            peer_liveness_handle(&key.public())
        );
        assert_ne!(
            peer_liveness_handle(&key.public()),
            peer_liveness_handle(&iroh::SecretKey::from_bytes(&[4; 32]).public())
        );
    }

    #[cfg(unix)]
    #[test]
    fn created_device_key_is_not_world_accessible() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("keypair.bin");
        load_or_create_keypair(&path).unwrap();
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o007, 0);
    }
}
