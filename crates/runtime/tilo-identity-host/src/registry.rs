//! The identities this device can sign as, and which one it presents where.
//!
//! An identity is the person's root: their key, the name this device calls it, and the record that
//! holds it. A device presents the person its own certificate was issued by; a device without one
//! presents the identity this login marked active. Which records a host holds is its own
//! ([`store`] on a Linux desktop).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

#[cfg(not(target_arch = "wasm32"))]
mod store;
#[cfg(all(not(unix), not(target_arch = "wasm32")))]
pub use crate::unsupported_host::registry::IdentityRegistryStore;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use store::DirStamp;
#[cfg(not(target_arch = "wasm32"))]
pub use store::{
    IdentityRegistryStore, identity_for_workspace, selected_identity_path,
    selected_principal_id_prompt,
};

pub(crate) const REGISTRY_SCHEMA_VERSION: u32 = 1;

/// Public metadata for one locally available signing identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityProfile {
    /// Lowercase Ed25519 public-key hex. Also the stable registry key.
    pub id: String,
    /// User-selected label. Empty records receive a fingerprint label.
    pub display_name: String,
    /// Absolute path to the private identity record. Never replicated.
    pub record_path: PathBuf,
    /// The person principal represented by this identity.
    pub user_public_key: [u8; 32],
}

impl IdentityProfile {
    /// Capability-model person id for this identity.
    #[must_use]
    pub fn principal_id(&self) -> String {
        match collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(self.user_public_key),
        ) {
            collab_types::caps::Principal::Person(id) => id,
            _ => self.id.clone(),
        }
    }
}

/// Versioned local identity index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityRegistry {
    pub schema_version: u32,
    /// Identity this login marked active; the person a device without a
    /// certificate of its own presents.
    pub default_identity: Option<String>,
    pub identities: BTreeMap<String, IdentityProfile>,
    /// Durable workspace-id string to [`IdentityProfile::id`], as builds that
    /// let a workspace pick an identity stored it. Read for those files,
    /// never written.
    pub workspace_identities: BTreeMap<String, String>,
}

impl Default for IdentityRegistry {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            default_identity: None,
            identities: BTreeMap::new(),
            workspace_identities: BTreeMap::new(),
        }
    }
}

impl IdentityRegistry {
    /// The identity this device presents in `workspace` (its id): the person its own
    /// certificate was issued by, whom the workspace's roster has as a member, or the identity
    /// this login marked active.
    ///
    /// A personal cluster ([`collab_types::personal_cluster`]) is derived from its person's root
    /// key, so it names that person by itself.
    pub fn for_workspace(
        &self,
        workspace: &str,
        device: Option<&DeviceCert>,
    ) -> Option<IdentityProfile> {
        self.personal_cluster_identity(workspace)
            .or_else(|| self.person_identity(device))
    }

    /// The identity this device presents wherever it goes: the person its own certificate was
    /// issued by, or, without one, the identity this login marked active.
    #[must_use]
    pub fn person_identity(&self, device: Option<&DeviceCert>) -> Option<IdentityProfile> {
        device
            .and_then(|cert| self.of_root(&cert.user_public_key))
            .or_else(|| {
                self.default_identity
                    .as_ref()
                    .and_then(|id| self.identities.get(id))
                    .cloned()
            })
    }

    /// The identity of the person whose root key is `user_public_key`, when this registry holds
    /// it.
    #[must_use]
    pub fn of_root(&self, user_public_key: &[u8; 32]) -> Option<IdentityProfile> {
        self.identities.get(&hex::encode(user_public_key)).cloned()
    }

    /// The identity `workspace` is the personal cluster of, when it is one of this registry's
    /// identities.
    fn personal_cluster_identity(&self, workspace: &str) -> Option<IdentityProfile> {
        self.identities
            .values()
            .find(|profile| {
                collab_types::personal_cluster(&profile.user_public_key).to_string() == workspace
            })
            .cloned()
    }
}

/// The first characters of a key, as a name a person can tell from another: 8 hex digits.
pub(crate) fn short_fingerprint(hex: &str) -> String {
    hex.get(..8).unwrap_or(hex).to_ascii_uppercase()
}
