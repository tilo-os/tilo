//! The device custody a host authenticates with to Tilo workspace hosts,
//! and the certificate it presents for each workspace it takes part in.
use std::collections::HashMap;
use tilo_user_identity::{DeviceCert, RevocationList};

/// A workspace's transport topic: the workspace id its hosts shard it by.
pub type WorkspaceId = u64;

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
    pub workspace_revocations: HashMap<WorkspaceId, Vec<tilo_user_identity::RevocationEntry>>,
    /// System device-key/certificate paths. Production actors read
    /// workspace identity material from these and announce it again when
    /// they change; tests may use the in-memory maps alone.
    pub identity_paths: Option<tilo_user_identity::IdentityPaths>,
}

impl HostIdentity {
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
    ) -> Result<(DeviceCert, Vec<tilo_user_identity::RevocationEntry>), String> {
        self.current_material(Some(ws))
    }

    fn current_material(
        &self,
        ws: Option<WorkspaceId>,
    ) -> Result<(DeviceCert, Vec<tilo_user_identity::RevocationEntry>), String> {
        let (cert, revocations) = if let Some(paths) = &self.identity_paths {
            let stored_key =
                tilo_user_identity::persist::load_device_key(paths).map_err(|e| e.to_string())?;
            if stored_key.verifying_key() != self.device_signing_key.verifying_key() {
                return Err("native device custody changed".into());
            }
            let store =
                crate::registry::IdentityRegistryStore::from_env().map_err(|e| e.to_string())?;
            let registry = store.load_reconciled_cached().map_err(|e| e.to_string())?;
            let profile = registry
                .person_identity()
                .ok_or("this device has no identity to sign in with")?;
            let cert = tilo_user_identity::persist::read_device_cert(
                &paths.device_cert_for_user(&profile.user_public_key),
            )
            .map_err(|e| e.to_string())?;
            if cert.user_public_key != profile.user_public_key {
                return Err("native certificate root differs from the person it names".into());
            }
            let fleet =
                crate::device_fleet::DeviceFleetStore::at(store.identity_dir(), paths.clone())
                    .load(&profile.id)
                    .map_err(|e| e.to_string())?;
            (cert, fleet.revocations)
        } else {
            (
                ws.and_then(|ws| self.workspace_device_certs.get(&ws))
                    .unwrap_or(&self.device_cert)
                    .clone(),
                ws.and_then(|ws| self.workspace_revocations.get(&ws))
                    .cloned()
                    .unwrap_or_default(),
            )
        };
        tilo_user_identity::verify_device_cert(&cert).map_err(|e| e.to_string())?;
        if cert.device_public_key != self.device_signing_key.verifying_key().to_bytes() {
            return Err("native certificate does not match retained device custody".into());
        }
        let mut checked = RevocationList::new();
        for entry in &revocations {
            checked.insert(entry.clone()).map_err(|e| e.to_string())?;
        }
        if checked.refuses(&cert) {
            return Err("native selected device is revoked".into());
        }
        Ok((cert, revocations))
    }

    pub fn material_for(
        &self,
        ws: WorkspaceId,
    ) -> (DeviceCert, Vec<tilo_user_identity::RevocationEntry>) {
        let fallback_cert = self
            .workspace_device_certs
            .get(&ws)
            .cloned()
            .unwrap_or_else(|| self.device_cert.clone());
        let fallback_revocations = self
            .workspace_revocations
            .get(&ws)
            .cloned()
            .unwrap_or_default();
        let Some((cert, fleet)) = self.selected() else {
            return (fallback_cert, fallback_revocations);
        };
        (
            cert,
            fleet.map_or(fallback_revocations, |fleet| fleet.revocations),
        )
    }

    /// The directories [`Self::material_for`]
    /// read: what this device announces changes only with them. None for an
    /// identity kept in memory.
    #[must_use]
    pub fn material_directories(&self) -> Vec<std::path::PathBuf> {
        let Some(paths) = &self.identity_paths else {
            return Vec::new();
        };
        let mut directories: Vec<_> = paths
            .device_cert
            .parent()
            .map(|device| device.join("certs"))
            .into_iter()
            .chain(
                paths
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

    /// This device's certificate for the person it signs as, and that
    /// person's fleet when it loads; `None` when no record of the person is
    /// at hand.
    fn selected(&self) -> Option<(DeviceCert, Option<crate::device_fleet::DeviceFleet>)> {
        let paths = self.identity_paths.as_ref()?;
        let registry_store = crate::registry::IdentityRegistryStore::from_env().ok()?;
        let registry = registry_store.load_reconciled_cached().ok()?;
        let profile = registry.person_identity()?;
        let cert = tilo_user_identity::persist::read_device_cert(
            &paths.device_cert_for_user(&profile.user_public_key),
        )
        .ok()?;
        if tilo_user_identity::verify_device_cert(&cert).is_err()
            || cert.user_public_key != profile.user_public_key
            || cert.device_public_key != self.device_signing_key.verifying_key().to_bytes()
        {
            return None;
        }
        let fleet =
            crate::device_fleet::DeviceFleetStore::at(registry_store.identity_dir(), paths.clone())
                .load(&profile.id)
                .ok();
        Some((cert, fleet))
    }
}
