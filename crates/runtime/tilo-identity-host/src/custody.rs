//! The device custody a host authenticates with to Tilo workspace hosts,
//! and the certificate it presents for each room it takes part in.
use std::collections::HashMap;
use tilo_desktop_model::RoomId;
use tilo_user_identity::{DeviceCert, RevocationList};

/// A room's transport topic: the workspace id its hosts shard it by.
pub type WorkspaceId = u64;

/// The transport topic of `room`, as the desktop session's network routes it.
fn room_topic(room: RoomId) -> WorkspaceId {
    if room.is_nil() {
        return 0;
    }
    tilo_relay_protocol::room_workspace_id(&room.to_string()).unwrap_or(0)
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
    pub workspace_revocations: HashMap<WorkspaceId, Vec<tilo_user_identity::RevocationEntry>>,
    /// System device-key/certificate paths. Production actors use these to
    /// refresh workspace identity material on every heartbeat; tests may use
    /// the in-memory maps alone.
    pub identity_paths: Option<tilo_user_identity::IdentityPaths>,
}

impl HostIdentity {
    /// Read current selected custody for enrolled operations. Configured disk failures
    /// never fall back to an old in-memory certificate or revocation snapshot.
    ///
    /// # Errors
    /// The stored device key or certificate changed, cannot be read or does
    /// not verify, the room's identity selection is missing or ambiguous, or
    /// the selected device is revoked.
    pub fn current_native_material(
        &self,
        ws: WorkspaceId,
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
            let mut selected = registry
                .workspace_identities
                .iter()
                .filter_map(|(room, id)| {
                    let room = RoomId::parse(room)?;
                    (room_topic(room) == ws).then_some(id)
                });
            let selected_id = selected
                .next()
                .or(registry.default_identity.as_ref())
                .ok_or("native identity selection missing")?;
            if selected.next().is_some() {
                return Err("ambiguous native identity topic".into());
            }
            let profile = registry
                .identities
                .get(selected_id)
                .ok_or("selected native identity is absent")?;
            let cert = tilo_user_identity::persist::read_device_cert(
                &paths.device_cert_for_user(&profile.user_public_key),
            )
            .map_err(|e| e.to_string())?;
            if cert.user_public_key != profile.user_public_key {
                return Err("native certificate root differs from selection".into());
            }
            let fleet =
                crate::device_fleet::DeviceFleetStore::at(store.identity_dir(), paths.clone())
                    .load(selected_id)
                    .map_err(|e| e.to_string())?;
            (cert, fleet.revocations)
        } else {
            (
                self.workspace_device_certs
                    .get(&ws)
                    .unwrap_or(&self.device_cert)
                    .clone(),
                self.workspace_revocations
                    .get(&ws)
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
        if checked.is_revoked(&cert.device_public_key) {
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
        let Some(paths) = self.identity_paths.as_ref() else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(registry_store) = crate::registry::IdentityRegistryStore::from_env() else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(registry) = registry_store.load_reconciled_cached() else {
            return (fallback_cert, fallback_revocations);
        };
        let selected = registry.workspace_identities.iter().find_map(|(room, id)| {
            let room = RoomId::parse(room)?;
            (room_topic(room) == ws).then_some(id)
        });
        let identity_id = selected
            .cloned()
            .or_else(|| registry.default_identity.clone());
        let Some(identity_id) = identity_id else {
            return (fallback_cert, fallback_revocations);
        };
        let Some(profile) = registry.identities.get(&identity_id) else {
            return (fallback_cert, fallback_revocations);
        };
        let Ok(cert) = tilo_user_identity::persist::read_device_cert(
            &paths.device_cert_for_user(&profile.user_public_key),
        ) else {
            return (fallback_cert, fallback_revocations);
        };
        if tilo_user_identity::verify_device_cert(&cert).is_err()
            || cert.user_public_key != profile.user_public_key
            || cert.device_public_key != self.device_signing_key.verifying_key().to_bytes()
        {
            return (fallback_cert, fallback_revocations);
        }
        let revocations =
            crate::device_fleet::DeviceFleetStore::at(registry_store.identity_dir(), paths.clone())
                .load(&identity_id)
                .map_or_else(|_| fallback_revocations, |fleet| fleet.revocations);
        (cert, revocations)
    }
}
