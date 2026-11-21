//! This computer's own workspace service: the one every device runs, holding the workspaces the
//! person's login takes part in as member copies synced with the server that hosts them.
//!
//! The device runs one endpoint under its key and one key service: the session the login holds
//! with its server dials from that endpoint and reads and asks with those keys
//! ([`tilo_session_client::Lent`]), and the member copies sync over the same endpoint and settle
//! their groups' keys through the same key service. A second endpoint or key service under the
//! device's key would take the first one's place at the server and in each group.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use collab_types::signer::DeviceSigner;
use ed25519_dalek::SigningKey;
use tilo_session_client::device::Device as Entered;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::own_device::{self, KeyService};
use tilo_workspace_service::service::WorkspaceService;

/// Where the service keeps its store, below the device's directory.
const SERVICE: &str = "service";
/// Where the key service keeps its sealed group state, below the device's directory.
const GROUPS: &str = "groups";

/// The device's own service, its key service, and the endpoint both and the session run on.
pub(crate) struct OwnDevice {
    endpoint: Arc<OnceLock<iroh::Endpoint>>,
    service: WorkspaceService,
    keys: KeyService,
}

impl OwnDevice {
    /// Open the service `key`'s device (`cert`) keeps below `dir`, on an endpoint bound under its
    /// key the way a session reaches `server`.
    ///
    /// # Errors
    /// The endpoint does not bind, or the key service's or the service's store cannot be opened.
    pub(crate) async fn open(
        dir: &Path,
        key: &SigningKey,
        cert: &DeviceCert,
        server: &tilo_server_client::ServerAddress,
    ) -> Result<Self, String> {
        let signer: Arc<dyn DeviceSigner> = Arc::new(key.clone());
        let keys = KeyService::open(
            "tilo-device-groups",
            &dir.join(GROUPS),
            vault_key(key),
            Arc::clone(&signer),
            Arc::new(tilo_workspace_keys::NoSealChecks),
        )
        .map_err(|error| format!("this device's key service: {error}"))?;
        let endpoint = server
            .bind(Some(key.to_bytes()))
            .await
            .map_err(|error| format!("this device's endpoint: {error}"))?;
        let service = own_device::open(
            dir.join(SERVICE),
            own_device::Device {
                cert,
                signer: &signer,
            },
            endpoint.clone(),
            keys.groups(),
            false,
            |config| config,
        )
        .await
        .map_err(|error| format!("this device's workspace service: {error}"))?;
        keys.attach(service.workspaces());
        let bound = Arc::new(OnceLock::new());
        let _ = bound.set(endpoint);
        Ok(Self {
            endpoint: bound,
            service,
            keys,
        })
    }

    /// What the login's session runs on: this device's endpoint and key service.
    pub(crate) fn lent(&self) -> tilo_session_client::Lent {
        tilo_session_client::Lent {
            keys: self.keys.keys().get(),
            endpoint: Some(Arc::clone(&self.endpoint)),
            own: true,
        }
    }

    /// Hold each workspace `device` takes part in as a member copy here, synced with the server
    /// that hosts it: those not held yet.
    pub(crate) async fn hold(&self, device: &Entered) {
        let engine = device.session().engine();
        let held = engine
            .workspaces()
            .into_iter()
            .filter_map(|workspace| Some((workspace, engine.member_of(workspace)?)));
        own_device::hold_as_members(self.service.workspaces(), held).await;
    }

    /// The workspaces the service holds.
    #[cfg(test)]
    pub(crate) const fn workspaces(&self) -> &tilo_workspace_service::workspaces::Workspaces {
        self.service.workspaces()
    }
}

/// The key sealing the device's group state: derived from the device key, which this computer
/// keeps beside it.
fn vault_key(key: &SigningKey) -> key_group::VaultKey {
    key_group::VaultKey::new(blake3::derive_key(
        "tilo device workspace key vault v1",
        &key.to_bytes(),
    ))
}

#[cfg(test)]
#[path = "own_device_tests.rs"]
mod tests;
