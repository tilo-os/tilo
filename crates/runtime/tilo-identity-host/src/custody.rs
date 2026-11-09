//! The device custody a host authenticates with to Tilo workspace hosts,
//! and the certificate it presents for each workspace it takes part in.
use std::collections::HashMap;
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

    /// The certificate this device presents and the revocations advertised beside it, as the
    /// store holds them now: never an older snapshot than the last write.
    ///
    /// # Errors
    /// The stored device key is not `device`'s, the store cannot be read, or this login holds
    /// no record of the person the device signs as.
    fn read(&self, device: &VerifyingKey) -> Result<(DeviceCert, Vec<RevocationEntry>), String>;

    /// The places [`Self::read`] reads, for a watcher of them: none when the custody is not
    /// files.
    fn directories(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Device certificate chain used to authenticate to a Tilo workspace host.
#[derive(Clone)]
pub struct HostIdentity {
    pub device_cert: DeviceCert,
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// Workspace-specific user certificates for this same device key.
    /// Missing entries deliberately fall back to `device_cert` for legacy and
    /// unbound workspaces.
    pub workspace_device_certs: HashMap<WorkspaceId, DeviceCert>,
    /// Root-signed revocations advertised beside each workspace certificate.
    pub workspace_revocations: HashMap<WorkspaceId, Vec<RevocationEntry>>,
    /// Where this device reads its custody from when it changes. Production actors read
    /// workspace identity material from it and announce it again when it changes; tests may use
    /// the in-memory maps alone.
    pub custody: Option<Arc<dyn Custody>>,
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
        Some(Self {
            device_cert,
            device_signing_key,
            workspace_device_certs: HashMap::new(),
            workspace_revocations: HashMap::new(),
            custody: Some(custody),
        })
    }

    /// Read this device's current custody for enrolled operations.
    /// Configured disk failures never fall back to an old in-memory
    /// certificate or revocation snapshot.
    ///
    /// # Errors
    /// The stored device key or certificate changed, cannot be read or does
    /// not verify, this login holds no record of the person the device signs
    /// as, or the device is revoked.
    pub fn current_native_material(
        &self,
        ws: WorkspaceId,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        self.current_material(Some(ws))
    }

    fn current_material(
        &self,
        ws: Option<WorkspaceId>,
    ) -> Result<(DeviceCert, Vec<RevocationEntry>), String> {
        let Some(custody) = &self.custody else {
            return Ok(self.workspace_material(ws));
        };
        let (cert, revocations) = custody.read(&self.device_signing_key.verifying_key())?;
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

    pub fn material_for(&self, ws: WorkspaceId) -> (DeviceCert, Vec<RevocationEntry>) {
        let selected = self.custody.as_ref().and_then(|custody| {
            let (cert, revocations) = custody
                .read(&self.device_signing_key.verifying_key())
                .ok()?;
            let subject =
                cert.device_public_key == self.device_signing_key.verifying_key().to_bytes();
            (tilo_user_identity::verify_device_cert(&cert).is_ok() && subject)
                .then_some((cert, revocations))
        });
        selected.unwrap_or_else(|| self.workspace_material(Some(ws)))
    }

    /// The directories [`Self::material_for`]
    /// read: what this device announces changes only with them. None for an
    /// identity kept in memory.
    #[must_use]
    pub fn material_directories(&self) -> Vec<PathBuf> {
        self.custody
            .as_ref()
            .map_or_else(Vec::new, |custody| custody.directories())
    }

    /// The material kept beside the device's own: the workspace's certificate when it has one,
    /// and the revocations recorded under it.
    fn workspace_material(&self, ws: Option<WorkspaceId>) -> (DeviceCert, Vec<RevocationEntry>) {
        (
            ws.and_then(|ws| self.workspace_device_certs.get(&ws))
                .unwrap_or(&self.device_cert)
                .clone(),
            ws.and_then(|ws| self.workspace_revocations.get(&ws))
                .cloned()
                .unwrap_or_default(),
        )
    }
}
