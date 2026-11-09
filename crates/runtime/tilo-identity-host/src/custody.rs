//! The device custody a host authenticates with to Tilo workspace hosts,
//! and the certificate it presents for each workspace it takes part in.
use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::VerifyingKey;
use tilo_user_identity::{DeviceCert, RevocationEntry, RevocationList};

#[cfg(not(target_arch = "wasm32"))]
pub mod files;

/// A workspace's transport topic: the workspace id its hosts shard it by.
pub type WorkspaceId = u64;

/// Where a host reads its device custody from, when it keeps one beyond memory: the identity
/// files of the person's own machine, the page's store on the web.
pub trait Custody: Send + Sync {
    /// This device's own certificate and the key it signs with, when this host holds an identity
    /// at all.
    ///
    /// # Errors
    /// This host has no identity, or its stored key and certificate differ.
    fn device(&self) -> Result<(DeviceCert, ed25519_dalek::SigningKey), String>;

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

    /// The places [`Self::read`] reads, for a watcher of them: none when the custody is not
    /// files.
    fn directories(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Device certificate chain used to authenticate to a Tilo workspace host.
#[derive(Clone)]
pub struct HostIdentity {
    /// The certificate this device was issued, as it was read.
    pub device_cert: DeviceCert,
    /// The key that certificate names.
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// Where the custody above is read from when it changes: the identity files of this host, or
    /// the material it keeps in memory.
    pub custody: Arc<dyn Custody>,
}

/// Custody this host keeps in memory: the material a page was handed at start, and what tests
/// hold. A certificate of another root this device presents in one workspace is kept beside the
/// device's own, which answers for every workspace that has none.
pub struct Kept {
    device: DeviceCert,
    key: ed25519_dalek::SigningKey,
    workspaces: std::collections::HashMap<WorkspaceId, (DeviceCert, Vec<RevocationEntry>)>,
}

impl Kept {
    /// Custody of `device` and the key it names.
    #[must_use]
    pub fn new(device: DeviceCert, key: ed25519_dalek::SigningKey) -> Self {
        Self {
            device,
            key,
            workspaces: std::collections::HashMap::new(),
        }
    }

    /// The same custody, presenting `cert` in `workspace` with the revocations advertised beside
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

impl Custody for Kept {
    fn device(&self) -> Result<(DeviceCert, ed25519_dalek::SigningKey), String> {
        Ok((self.device.clone(), self.key.clone()))
    }

    fn read(
        &self,
        workspace: WorkspaceId,
        device: &VerifyingKey,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        if self.key.verifying_key() != *device {
            return Err("kept device custody changed".into());
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

impl HostIdentity {
    /// This device's identity as `custody` keeps it, when this host holds one: the certificate it
    /// presents and the key it signs with.
    #[must_use]
    pub fn open(custody: Arc<dyn Custody>) -> Option<Self> {
        let (device_cert, device_signing_key) = match custody.device() {
            Ok(device) => device,
            Err(error) => {
                tracing::warn!(%error, "this device holds no identity to sign in with");
                return None;
            }
        };
        Some(Self::held(device_cert, device_signing_key, custody))
    }

    /// The identity `cert` and `key` are, with `custody` keeping it current: for a host that read
    /// the certificate it means to present itself, which is not always the one every workspace
    /// uses.
    #[must_use]
    pub fn held(
        device_cert: DeviceCert,
        device_signing_key: ed25519_dalek::SigningKey,
        custody: Arc<dyn Custody>,
    ) -> Self {
        Self {
            device_cert,
            device_signing_key,
            custody,
        }
    }

    /// This device's identity kept in memory alone: the certificate and the key it was given.
    #[must_use]
    pub fn kept(device_cert: DeviceCert, device_signing_key: ed25519_dalek::SigningKey) -> Self {
        let custody = Arc::new(Kept::new(device_cert.clone(), device_signing_key.clone()));
        Self::held(device_cert, device_signing_key, custody)
    }

    /// This device's custody as its host keeps it now: what an enrolled operation signs and
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
            .custody
            .read(workspace, &self.device_signing_key.verifying_key())?;
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
            .custody
            .read(workspace, &self.device_signing_key.verifying_key())
            .ok()
            .filter(|(cert, _)| {
                tilo_user_identity::verify_device_cert(cert).is_ok()
                    && cert.device_public_key == self.device_signing_key.verifying_key().to_bytes()
            });
        read.unwrap_or_else(|| (self.device_cert.clone(), Vec::new()))
    }

    /// The directories [`Self::material_for`] reads: what this device announces changes only with
    /// them. None for an identity kept in memory.
    #[must_use]
    pub fn material_directories(&self) -> Vec<PathBuf> {
        self.custody.directories()
    }
}
