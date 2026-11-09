//! This device's custody as the identity files hold it.
use std::path::PathBuf;

use ed25519_dalek::VerifyingKey;
use tilo_user_identity::{DeviceCert, IdentityPaths, RevocationEntry};

use super::Custody;

/// The identity files of the person whose devices this host is one of.
pub struct Files {
    paths: IdentityPaths,
}

impl Files {
    #[must_use]
    pub fn new(paths: IdentityPaths) -> Self {
        Self { paths }
    }
}

impl Custody for Files {
    fn device(&self) -> Result<(DeviceCert, ed25519_dalek::SigningKey), String> {
        let key =
            tilo_user_identity::persist::load_device_key(&self.paths).map_err(|e| e.to_string())?;
        let cert = tilo_user_identity::persist::read_device_cert(&self.paths.device_cert)
            .map_err(|e| e.to_string())?;
        if cert.device_public_key != key.verifying_key().to_bytes() {
            return Err("the collaboration device key and its certificate differ".into());
        }
        Ok((cert, key))
    }

    fn read(&self, device: &VerifyingKey) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        let stored_key =
            tilo_user_identity::persist::load_device_key(&self.paths).map_err(|e| e.to_string())?;
        if stored_key.verifying_key() != *device {
            return Err("native device custody changed".into());
        }
        let store =
            crate::registry::IdentityRegistryStore::from_env().map_err(|e| e.to_string())?;
        let registry = store.load_reconciled_cached().map_err(|e| e.to_string())?;
        let profile = registry
            .certified_identity()
            .ok_or("this device has no identity to sign in with")?;
        let cert = tilo_user_identity::persist::read_device_cert(
            &self.paths.device_cert_for_user(&profile.user_public_key),
        )
        .map_err(|e| e.to_string())?;
        if cert.user_public_key != profile.user_public_key {
            return Err("native certificate root differs from the person it names".into());
        }
        let fleet =
            crate::device_fleet::DeviceFleetStore::at(store.identity_dir(), self.paths.clone())
                .load(&profile.id)
                .map_err(|e| e.to_string())?;
        Ok((cert, fleet.revocations))
    }

    fn directories(&self) -> Vec<PathBuf> {
        let mut directories: Vec<_> = self
            .paths
            .device_cert
            .parent()
            .map(|device| device.join("certs"))
            .into_iter()
            .chain(
                self.paths
                    .user_pubkey
                    .parent()
                    .and_then(std::path::Path::parent)
                    .map(tilo_relay_protocol::home_peer_registry_path)
                    .and_then(|registry| registry.parent().map(std::path::Path::to_path_buf)),
            )
            .collect();
        if let Ok(store) = crate::registry::IdentityRegistryStore::from_env() {
            directories.push(store.identity_dir().join("fleets"));
            directories.push(store.identity_dir().to_path_buf());
        }
        directories
    }
}
