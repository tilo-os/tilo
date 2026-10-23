//! First-boot bridge to the other-computer handoff receiver.
//!
//! The descriptor is public and sits in the account's private state directory
//! after the destination has been installed. The receiver proves this
//! computer's certified device identity, receives the package over the
//! existing peer transport, and writes the ordinary prepared-setup transport.
//! The canonical onboarding consumer then imports it with the received
//! password; only after that succeeds does this bridge release the source with
//! a receipt for the exact package.
//!
//! The transport lives in a small dedicated crate rather than linking the
//! whole installer backend into the compositor.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

pub(crate) use tilo_handoff::Received;

pub(crate) const FILE: &str = tilo_handoff::DESCRIPTOR_FILE;

/// A connected receiver, driven by the first-boot loop without blocking it.
pub(crate) struct Peer {
    receiver: tilo_handoff::Receiver,
}

impl Peer {
    /// Whether a descriptor exists, without connecting to its source. A
    /// malformed or linked object is still pending, so detection cannot erase
    /// a failed preparation.
    pub(crate) fn pending_at(home: &Path) -> bool {
        std::fs::symlink_metadata(home.join(".local/state/tilo").join(FILE)).is_ok()
    }

    /// The person the published descriptor names, if a usable descriptor is
    /// present. First boot uses this to distinguish a fresh computer from one
    /// whose active identity belongs to another person before offering a join.
    pub(crate) fn descriptor_identity_at(home: &Path) -> std::io::Result<Option<[u8; 32]>> {
        Ok(tilo_handoff::read_descriptor(home)?.map(|handoff| handoff.identity_public_key))
    }

    /// Join the descriptor's person as a new member device. This is the fresh
    /// computer path: no person exists yet, the source's root signs this
    /// computer's own new device key after approval, and only then does the
    /// ordinary receiver run. Nothing is written over an existing identity.
    pub(crate) fn join(
        home: &Path,
        identity_root: &Path,
        identity_dir: &Path,
        device_label: &str,
    ) -> std::io::Result<Option<Self>> {
        let Some(handoff) = tilo_handoff::read_descriptor(home)? else {
            return Ok(None);
        };
        if runtime_identity::resolve_active_identity(identity_dir).is_some()
            || match runtime_identity::list_identities(identity_dir) {
                Ok(identities) => !identities.is_empty(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error),
            }
        {
            return Err(std::io::Error::other(
                "This computer already has an identity",
            ));
        }
        // The package's importer later creates its custody in the real
        // identity directory, so that directory must already be private.
        std::fs::create_dir_all(identity_dir)?;
        std::fs::set_permissions(identity_dir, std::fs::Permissions::from_mode(0o700))?;
        // The joined member identity is staged privately while the package is
        // being received. The package's own canonical importer then installs
        // the person's custody into the real identity directory, so a fresh
        // computer never has two competing records for the same person.
        let join_identity = identity_dir.join(".other-computer-join");
        let receiver = tilo_handoff::Receiver::join(
            handoff,
            tilo_handoff::ReceiverIdentity {
                identity_root: identity_root.to_path_buf(),
                identity_dir: join_identity,
            },
            home.join(".local/state/tilo/prepared-setup"),
            device_label.to_owned(),
        )?;
        Ok(Some(Self { receiver }))
    }

    /// Open the handoff described for `home`, if one is present. A malformed,
    /// linked or expired descriptor is an error, never silently ignored.
    pub(crate) fn open_at(
        home: &Path,
        identity_root: &Path,
        identity_dir: &Path,
    ) -> std::io::Result<Option<Self>> {
        let Some(handoff) = tilo_handoff::read_descriptor(home)? else {
            return Ok(None);
        };
        if runtime_identity::resolve_active_identity(identity_dir).is_none() {
            return Ok(None);
        }
        let paths = tilo_user_identity::persist::IdentityPaths::under(identity_root);
        if !paths.device_privkey.exists()
            || (!paths.device_cert.exists()
                && !paths
                    .device_cert
                    .parent()
                    .is_some_and(|parent| parent.join("certs").is_dir()))
        {
            return Ok(None);
        }
        let receiver = tilo_handoff::Receiver::open(
            handoff,
            tilo_handoff::ReceiverIdentity {
                identity_root: identity_root.to_path_buf(),
                identity_dir: identity_dir.to_path_buf(),
            },
            home.join(".local/state/tilo/prepared-setup"),
        )?;
        Ok(Some(Self { receiver }))
    }

    /// The package, once it has arrived and verified.
    pub(crate) fn poll(&self) -> Option<std::io::Result<Received>> {
        self.receiver.poll()
    }

    /// The code both screens show before a person approves this computer on
    /// the preparing computer.
    pub(crate) fn verification_code(&self) -> Option<String> {
        self.receiver.verification_code()
    }

    /// The canonical importer verified and adopted this exact package.
    pub(crate) fn acknowledge(&self, received: &Received) -> std::io::Result<()> {
        self.receiver.acknowledge(received)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_descriptor_is_not_a_pending_transfer() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(
            Peer::open_at(temporary.path(), temporary.path(), temporary.path())
                .unwrap()
                .is_none()
        );
        assert!(!Peer::pending_at(temporary.path()));
    }

    #[test]
    fn a_malformed_descriptor_is_refused() {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join(".local/state/tilo");
        std::fs::create_dir_all(&state).unwrap();
        for path in [
            temporary.path().join(".local"),
            temporary.path().join(".local/state"),
            state.clone(),
        ] {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(state.join(FILE), b"{\"schema\":1}").unwrap();
        std::fs::set_permissions(state.join(FILE), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Peer::pending_at(temporary.path()));
        assert!(Peer::open_at(temporary.path(), temporary.path(), temporary.path()).is_err());
    }

    #[test]
    fn a_linked_or_public_descriptor_is_refused() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join(".local/state/tilo");
        std::fs::create_dir_all(&state).unwrap();
        for path in [
            temporary.path().join(".local"),
            temporary.path().join(".local/state"),
            state.clone(),
        ] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let real = temporary.path().join("descriptor.json");
        std::fs::write(&real, b"{}").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&real, state.join(FILE)).unwrap();
        assert!(tilo_handoff::read_descriptor(temporary.path()).is_err());

        std::fs::remove_file(state.join(FILE)).unwrap();
        std::fs::write(state.join(FILE), b"{}").unwrap();
        std::fs::set_permissions(state.join(FILE), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(tilo_handoff::read_descriptor(temporary.path()).is_err());
    }
}
