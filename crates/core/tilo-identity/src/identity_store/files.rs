//! This device's identity store as the identity files hold it.
use std::path::PathBuf;

use collab_types::signer::DeviceSigner as _;
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
        // The key this process took as the device's daemon, else the daemon asked to sign.
        let signer =
            crate::trusted_device::device_signer(&self.paths).map_err(|e| e.to_string())?;
        let cert = tilo_device_identity::persist::read_owner_cert(&self.paths)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "this device holds no certificate of its person".to_owned())?;
        if cert.device_public_key != signer.device_public_key() {
            return Err("the collaboration device key and its certificate differ".into());
        }
        Ok((cert, signer))
    }

    fn read(
        &self,
        _workspace: super::WorkspaceId,
        device: &VerifyingKey,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        // The certificate at these paths names this device's key, and the person whose device
        // this is: the registry knows that person, and the host that keeps their devices says
        // which keys they took back.
        let cert = tilo_device_identity::persist::read_owner_cert(&self.paths)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "this device holds no certificate of its person".to_owned())?;
        if cert.device_public_key != device.to_bytes() {
            return Err("native device identity store changed".into());
        }
        let store = crate::store::IdentityRegistryStore::from_env().map_err(|e| e.to_string())?;
        let registry = store.load_reconciled_cached().map_err(|e| e.to_string())?;
        if registry.identity_of(&cert.user_public_key).is_none() {
            return Err("this device has no identity to sign in with".into());
        }
        let trust = crate::sources::device_trust();
        // The record of the person's devices holds this one's root-signed revocation: it signs
        // nothing more as them.
        if trust.and_then(|trust| trust.revoked(&cert.user_public_key, &cert.device_public_key))
            == Some(true)
        {
            return Err("this device has been revoked for the selected identity".into());
        }
        let revocations = trust
            .map(|trust| trust.revocations(&cert.user_public_key))
            .unwrap_or_default();
        Ok((cert, revocations))
    }

    fn directories(&self) -> Vec<PathBuf> {
        vec![self.paths.certs_dir()]
    }
}
