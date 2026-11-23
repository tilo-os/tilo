//! The device identity store a host authenticates with to Tilo workspace hosts,
//! and the certificate it presents for each workspace it takes part in.
use std::path::PathBuf;
use std::sync::Arc;

use collab_types::signer::{DeviceSigner, MaybeSendSync};
use ed25519_dalek::VerifyingKey;
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

#[cfg(unix)]
pub mod files;

pub use collab_types::WorkspaceId;

/// Where a host reads its device identity store from, when it keeps one beyond memory: the identity
/// files of the person's own machine, the page's store on the web.
pub trait IdentityStore: MaybeSendSync {
    /// This device's own certificate and the signer that holds its key, when this host holds an
    /// identity at all.
    ///
    /// # Errors
    /// This host has no identity, or its stored key and certificate differ.
    fn device(&self) -> Result<(DeviceCert, Arc<dyn DeviceSigner>), String>;

    /// The certificate this device presents in `workspace` and the revocations advertised beside
    /// it, as the store holds them now: never an older snapshot than the last write. A workspace
    /// this device presents the same certificate in answers that one.
    ///
    /// # Errors
    /// The stored device key is not `device`'s, the store cannot be read, or this login holds
    /// no record of the person the device signs as.
    fn read(
        &self,
        workspace: WorkspaceId,
        device: &VerifyingKey,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String>;

    /// The workspaces this device presents a certificate of another root in.
    fn workspaces(&self) -> Vec<WorkspaceId> {
        Vec::new()
    }

    /// The places [`Self::read`] reads, for a watcher of them: none when the identity store is not
    /// files.
    fn directories(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Device certificate chain used to authenticate to a Tilo workspace host.
#[derive(Clone)]
pub struct DeviceIdentity {
    /// The certificate this device was issued, as it was read.
    pub device_cert: DeviceCert,
    /// Signs with the key that certificate names; the key stays in identity store.
    pub signer: Arc<dyn DeviceSigner>,
    /// Where the identity store above is read from when it changes: the identity files of this host, or
    /// the material it keeps in memory.
    pub identity_store: Arc<dyn IdentityStore>,
}

/// The identity this host keeps in memory: the material a page was handed at start, and what tests
/// hold. A certificate of another root this device presents in one workspace is kept beside the
/// device's own, which answers for every workspace that has none.
pub struct Kept {
    device: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    workspaces: std::collections::HashMap<WorkspaceId, (DeviceCert, Vec<RevocationEntry>)>,
}

impl Kept {
    /// The identity of `device` and the signer of the key it names.
    #[must_use]
    pub fn new(device: DeviceCert, signer: Arc<dyn DeviceSigner>) -> Self {
        Self {
            device,
            signer,
            workspaces: std::collections::HashMap::new(),
        }
    }

    /// The same identity store, presenting `cert` in `workspace` with the revocations advertised beside
    /// it.
    #[must_use]
    pub fn in_workspace(
        mut self,
        workspace: WorkspaceId,
        cert: DeviceCert,
        revocations: Vec<RevocationEntry>,
    ) -> Self {
        self.workspaces.insert(workspace, (cert, revocations));
        self
    }
}

impl IdentityStore for Kept {
    fn device(&self) -> Result<(DeviceCert, Arc<dyn DeviceSigner>), String> {
        Ok((self.device.clone(), Arc::clone(&self.signer)))
    }

    fn read(
        &self,
        workspace: WorkspaceId,
        device: &VerifyingKey,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        if self.signer.device_public_key() != device.to_bytes() {
            return Err("kept device identity store changed".into());
        }
        Ok(self
            .workspaces
            .get(&workspace)
            .cloned()
            .unwrap_or_else(|| (self.device.clone(), Vec::new())))
    }

    fn workspaces(&self) -> Vec<WorkspaceId> {
        self.workspaces.keys().copied().collect()
    }
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept")
            .field("device", &hex::encode(self.device.device_public_key))
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    /// This device's identity as `identity store` keeps it, when this host holds one: the certificate it
    /// presents and the key it signs with.
    #[must_use]
    pub fn open(identity_store: Arc<dyn IdentityStore>) -> Option<Self> {
        let (device_cert, signer) = match identity_store.device() {
            Ok(device) => device,
            Err(error) => {
                tracing::warn!(%error, "this device holds no identity to sign in with");
                return None;
            }
        };
        Some(Self {
            device_cert,
            signer,
            identity_store,
        })
    }

    /// The identity `cert` and `key` are, with `identity store` keeping it current: for a host that read
    /// the certificate it means to present itself, which is not always the one every workspace
    /// uses.
    #[must_use]
    pub fn held(
        device_cert: DeviceCert,
        signer: impl DeviceSigner + 'static,
        identity_store: Arc<dyn IdentityStore>,
    ) -> Self {
        Self {
            device_cert,
            signer: Arc::new(signer),
            identity_store,
        }
    }

    /// This device's identity kept in memory alone: the certificate and the signer it was given
    /// (a native key, or a page's non-extractable one).
    #[must_use]
    pub fn kept(device_cert: DeviceCert, signer: impl DeviceSigner + 'static) -> Self {
        let signer: Arc<dyn DeviceSigner> = Arc::new(signer);
        let identity_store = Arc::new(Kept::new(device_cert.clone(), Arc::clone(&signer)));
        Self {
            device_cert,
            signer,
            identity_store,
        }
    }

    /// The device's public key.
    #[must_use]
    pub fn device_public_key(&self) -> [u8; 32] {
        self.signer.device_public_key()
    }

    fn device_verifying_key(&self) -> Result<VerifyingKey, String> {
        VerifyingKey::from_bytes(&self.device_public_key()).map_err(|error| error.to_string())
    }

    /// This device's identity store as its host keeps it now: what an enrolled operation signs and
    /// presents with.
    ///
    /// # Errors
    /// The store cannot be read, the key or certificate it holds changed or does not verify, this
    /// login holds no record of the person the device signs as, or the device is revoked.
    pub fn current_material(
        &self,
        workspace: WorkspaceId,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        let (cert, revocations) = self
            .identity_store
            .read(workspace, &self.device_verifying_key()?)?;
        tilo_user_identity::verify_device_cert(&cert).map_err(|e| e.to_string())?;
        let mut checked = RevocationList::new();
        for entry in &revocations {
            checked.insert(entry.clone()).map_err(|e| e.to_string())?;
        }
        if checked.refuses(&cert) {
            return Err("native selected device is revoked".into());
        }
        Ok((cert, revocations))
    }

    /// The certificate and revocations this device presents in `workspace`, falling back to the
    /// device's own when the store cannot answer.
    #[must_use]
    pub fn material_for(&self, workspace: WorkspaceId) -> (DeviceCert, Vec<RevocationEntry>) {
        let read = self
            .device_verifying_key()
            .and_then(|device| self.identity_store.read(workspace, &device))
            .ok()
            .filter(|(cert, _)| {
                tilo_user_identity::verify_device_cert(cert).is_ok()
                    && cert.device_public_key == self.device_public_key()
            });
        read.unwrap_or_else(|| (self.device_cert.clone(), Vec::new()))
    }

    /// The directories [`Self::material_for`] reads: what this device announces changes only with
    /// them. None for an identity kept in memory.
    #[must_use]
    pub fn material_directories(&self) -> Vec<PathBuf> {
        self.identity_store.directories()
    }
}
