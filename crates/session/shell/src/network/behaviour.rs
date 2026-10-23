//! Iroh endpoint identity helpers for Tilo peer sync.
//!
//! Every network protocol on a device uses the same Ed25519 seed from
//! [`DEVICE_KEY_PATH`]. Iroh exposes the public half directly as an
//! [`iroh::EndpointId`], so there is no transport-specific identity encoding
//! to keep in sync with the filesystem.

/// Deterministic 16-bit peer salt from the durable iroh endpoint key.
pub fn peer_salt_from_keypair(key: &iroh::SecretKey) -> u16 {
    let h = fnv1a_64(key.public().as_bytes());
    tilo_desktop_model::ids::device_salt(
        ((h & 0xFFFF) as u16)
            ^ (((h >> 16) & 0xFFFF) as u16)
            ^ (((h >> 32) & 0xFFFF) as u16)
            ^ (((h >> 48) & 0xFFFF) as u16),
    )
}

/// Fold an authenticated endpoint id to the numeric liveness handle carried
/// by presence messages. Authority decisions retain the complete endpoint id.
pub fn peer_liveness_handle(peer: &iroh::EndpointId) -> u64 {
    neighbour_wire::peer_handle(peer.as_bytes())
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, bytes);
    std::hash::Hasher::finish(&hasher)
}

/// Machine-wide device key used by the compositor, filesystem and host.
pub const DEVICE_KEY_PATH: &str = "/var/lib/tilo/device-private/keypair.bin";

/// Load the device key, creating it only when the path is genuinely absent.
/// Malformed, unreadable or over-permissive files are never replaced.
pub fn load_or_create_keypair(path: &std::path::Path) -> std::io::Result<iroh::SecretKey> {
    let canonical_paths = tilo_user_identity::IdentityPaths::under(
        path.parent()
            .and_then(std::path::Path::parent)
            .unwrap_or_else(|| std::path::Path::new("/var/lib/tilo")),
    );

    let signing = if path == canonical_paths.device_privkey {
        tilo_user_identity::load_or_create_device_key(&canonical_paths)
            .map_err(persist_error)?
            .0
    } else if path.exists() {
        tilo_user_identity::persist::read_device_key(path).map_err(persist_error)?
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
            getrandom::SysRng,
        ));
        tilo_user_identity::persist::write_device_key(path, &key)?;
        key
    };
    Ok(iroh::SecretKey::from_bytes(&signing.to_bytes()))
}

fn persist_error(error: tilo_user_identity::PersistError) -> std::io::Error {
    let kind = match error {
        tilo_user_identity::PersistError::Io(ref e) => e.kind(),
        _ => std::io::ErrorKind::InvalidData,
    };
    std::io::Error::new(kind, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_key_persists_and_is_never_replaced() {
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("device/keypair.bin");
            let first = load_or_create_keypair(&path).expect("create");
            let second = load_or_create_keypair(&path).expect("load");
            assert_eq!(first.public(), second.public());
            let seed: [u8; 32] = std::fs::read(&path).unwrap().try_into().unwrap();
            assert_eq!(first.to_bytes(), seed);
        }
        {
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
        {
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
