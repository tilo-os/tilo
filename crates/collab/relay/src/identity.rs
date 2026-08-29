//! Persistent ed25519 keypair for the relay's libp2p identity.
//!
//! Mirrors `shell/src/network/behaviour.rs::load_or_create_keypair` so a
//! relay reads the same on-disk format the compositor does. The keypair
//! survives restarts so the relay's `PeerId` stays stable — operators can
//! publish a permanent multiaddr without updating it after every reboot.

use std::path::Path;

use libp2p::identity::Keypair;

/// Loads (or creates) the persistent ed25519 keypair at `path`.
///
/// First call (or an unrecoverable file) generates a fresh keypair,
/// written with mode `0600`; later calls return the same one, keeping
/// `PeerId` stable across restarts.
///
/// If the file exists but doesn't parse as a protobuf keypair, this
/// returns `Err` rather than silently overwriting it — unlike the
/// shell's variant, since a public-facing relay should alert the
/// operator rather than discard data on a transient read fault.
pub fn load_or_create_keypair(path: &Path) -> std::io::Result<Keypair> {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes)?;
        if bytes.is_empty() {
            // Truncated file from a partial first-run write: regenerate.
            return generate_and_write(path);
        }
        return Keypair::from_protobuf_encoding(&bytes).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "identity file at {} is unreadable as a libp2p keypair: \
                     {e}. Move it aside and let qos-relay regenerate, or \
                     restore from a backup.",
                    path.display()
                ),
            )
        });
    }
    generate_and_write(path)
}

fn generate_and_write(path: &Path) -> std::io::Result<Keypair> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let kp = Keypair::generate_ed25519();
    let bytes = kp.to_protobuf_encoding().map_err(std::io::Error::other)?;
    std::fs::write(path, bytes)?;
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

    #[test]
    fn keypair_persists_across_calls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity.key");

        let kp1 = load_or_create_keypair(&path).expect("first load creates");
        let kp2 = load_or_create_keypair(&path).expect("second load reads");

        assert_eq!(kp1.public().to_peer_id(), kp2.public().to_peer_id());
    }

    #[test]
    fn keypair_is_created_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("a").join("b").join("identity.key");
        assert!(!nested.exists());

        let _kp = load_or_create_keypair(&nested).expect("creates dirs + file");
        assert!(nested.exists(), "key file must exist after first call");
    }

    #[test]
    fn keypair_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity.key");
        let _kp = load_or_create_keypair(&path).expect("creates key");

        let meta = std::fs::metadata(&path).expect("stat key file");
        let mode = meta.permissions().mode();
        assert_eq!(
            mode & 0o7777,
            0o600,
            "identity file must be mode 0600, got {:o}",
            mode & 0o7777,
        );
    }

    #[test]
    fn corrupted_file_surfaces_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("identity.key");

        // Garbage that is definitely not a valid libp2p protobuf keypair.
        std::fs::write(&path, b"this is not a libp2p keypair").expect("write garbage");

        let err = load_or_create_keypair(&path).expect_err("must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("unreadable"),
            "expected clear message, got: {msg}"
        );
    }
}
