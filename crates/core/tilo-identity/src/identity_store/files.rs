//! This device's identity store as the identity files hold it.
use std::path::PathBuf;

use ed25519_dalek::VerifyingKey;
use tilo_device_identity::IdentityPaths;
use tilo_user_identity::{DeviceCert, RevocationEntry};

use super::IdentityStore;

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

impl IdentityStore for Files {
    fn device(
        &self,
    ) -> Result<
        (
            DeviceCert,
            std::sync::Arc<dyn collab_types::signer::DeviceSigner>,
        ),
        String,
    > {
        let key = tilo_device_identity::persist::load_device_key(&self.paths)
            .map_err(|e| e.to_string())?;
        let cert = tilo_device_identity::persist::read_device_cert(&self.paths.device_cert)
            .map_err(|e| e.to_string())?;
        if cert.device_public_key != key.verifying_key().to_bytes() {
            return Err("the collaboration device key and its certificate differ".into());
        }
        Ok((cert, std::sync::Arc::new(key)))
    }

    fn read(
        &self,
        _workspace: super::WorkspaceId,
        device: &VerifyingKey,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        let stored_key = tilo_device_identity::persist::load_device_key(&self.paths)
            .map_err(|e| e.to_string())?;
        if stored_key.verifying_key() != *device {
            return Err("native device identity store changed".into());
        }
        let store = crate::store::IdentityRegistryStore::from_env().map_err(|e| e.to_string())?;
        let registry = store.load_reconciled_cached().map_err(|e| e.to_string())?;
        // The certificate these paths hold names the person whose device this is: the registry
        // knows that person, and the host that holds their devices says which keys they took
        // back.
        let cert = tilo_device_identity::persist::read_device_cert(&self.paths.device_cert)
            .map_err(|e| e.to_string())?;
        if registry.identity_of(&cert.user_public_key).is_none() {
            return Err("this device has no identity to sign in with".into());
        }
        let revocations = crate::sources::device_trust()
            .map(|trust| trust.revocations(&cert.user_public_key))
            .unwrap_or_default();
        Ok((cert, revocations))
    }

    fn directories(&self) -> Vec<PathBuf> {
        self.paths
            .device_cert
            .parent()
            .map(|device| device.join("certs"))
            .into_iter()
            .collect()
    }
}
