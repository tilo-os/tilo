//! The identities this device can sign as, and which one it presents where.
//!
//! An identity is the person's root: their key, the name this device calls it, and the record that
//! holds it. A device presents the person its own certificate was issued by; a device without one
//! presents the identity this login marked active. The profiles are read from the record files
//! this store keeps and the `active` selection; nothing here keeps a second index.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tilo_user_identity::DeviceCert;

mod store;
pub(crate) use store::DirStamp;
pub use store::{
    IdentityRegistryStore, identity_for_workspace, selected_actor_id_prompt, selected_identity_path,
};

/// Public metadata for one locally available signing identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityProfile {
    /// Lowercase Ed25519 public-key hex. Also the identity's key here.
    pub id: String,
    /// User-selected label. Empty records receive a fingerprint label.
    pub display_name: String,
    /// Absolute path to the private identity record. Never replicated.
    pub record_path: PathBuf,
    /// The person actor represented by this identity.
    pub user_public_key: [u8; 32],
}

impl IdentityProfile {
    /// The person this identity is, as the workspace's governance names them: their key in
    /// lowercase hex.
    #[must_use]
    pub fn person_id(&self) -> String {
        hex::encode(self.user_public_key)
    }
}

/// The identities this account holds, and the one it presents by default.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IdentityRegistry {
    /// Identity this login marked active; the person a device without a
    /// certificate of its own presents.
    pub default_identity: Option<String>,
    pub identities: BTreeMap<String, IdentityProfile>,
}

impl IdentityRegistry {
    /// The identity this device presents in `workspace` (its id): the person its own
    /// certificate was issued by, whom the workspace's roster has as a member, or the identity
    /// this login marked active.
    ///
    /// The person's own workspace ([`collab_types::person_workspace`]) is derived from their
    /// root key, so it names that person by itself.
    pub fn for_workspace(
        &self,
        workspace: &str,
        device: Option<&DeviceCert>,
    ) -> Option<IdentityProfile> {
        self.person_workspace_identity(workspace)
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

    /// The identity `workspace` is the own workspace of, when it is one of this registry's
    /// identities.
    fn person_workspace_identity(&self, workspace: &str) -> Option<IdentityProfile> {
        self.identities
            .values()
            .find(|profile| {
                collab_types::person_workspace(&profile.user_public_key).to_string() == workspace
            })
            .cloned()
    }
}

/// The first characters of a key, as a name a person can tell from another: 8 hex digits.
pub(crate) fn short_fingerprint(hex: &str) -> String {
    hex.get(..8).unwrap_or(hex).to_ascii_uppercase()
}
