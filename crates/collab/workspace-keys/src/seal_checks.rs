//! Where a group's plaintext check is enforced: one check per group this device takes part in,
//! consulted on every ingress path of the host that admits an app's ops.
//!
//! The group engine registers its checks here ([`SealCheckRegistry`]); a host's broker holds the map
//! beside the state it admits ops against and never a key.

use std::collections::HashMap;
use std::sync::Mutex;

use collab_types::{EncryptionMetadata, WorkspaceId};

/// What a key service registers its checks with: the ingress check of the host that admits an
/// app's ops.
pub trait SealCheckRegistry: collab_types::signer::MaybeSendSync {
    /// Enforce `check` on every op of `workspace` from now on.
    fn register_seal_check(&self, workspace: WorkspaceId, seal_check: key_group::SealCheck);
}

/// The checks of a host that keeps none of its own: a service that registers into it refuses nothing
/// there, and only the encryption its key holders agree keeps a group sealed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSealChecks;

impl SealCheckRegistry for NoSealChecks {
    fn register_seal_check(&self, _workspace: WorkspaceId, _check: key_group::SealCheck) {}
}

/// The check of every group this device takes part in.
#[derive(Default)]
pub struct SealChecks(Mutex<HashMap<WorkspaceId, key_group::SealCheck>>);

impl SealChecks {
    /// Install or refresh a group's ingress check.
    pub fn register(&self, workspace: WorkspaceId, seal_check: key_group::SealCheck) {
        if let Ok(mut seal_checks) = self.0.lock() {
            seal_checks.insert(workspace, seal_check);
        }
    }

    /// The check governing every record of `workspace`: its group's.
    #[must_use]
    pub fn seal_check_for(&self, workspace: &str) -> Option<key_group::SealCheck> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.0.lock().ok()?.get(&workspace).copied()
    }

    /// A route with no check has not been opened encrypted on this device yet. Once its check closes
    /// plaintext, no live plaintext envelope can pass either ingress path, including after a
    /// restart. Content sealed under any of the group's epochs passes: a member's write reaches
    /// this device after the group's next membership change.
    #[must_use]
    pub fn allows_document(
        &self,
        workspace: &str,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        self.seal_check_for(workspace)
            .is_none_or(|seal_check| seal_check.accepts(encryption))
    }
}
impl SealCheckRegistry for SealChecks {
    fn register_seal_check(&self, workspace: WorkspaceId, seal_check: key_group::SealCheck) {
        self.register(workspace, seal_check);
    }
}

impl SealCheckRegistry for std::sync::Arc<dyn SealCheckRegistry> {
    fn register_seal_check(&self, workspace: WorkspaceId, seal_check: key_group::SealCheck) {
        (**self).register_seal_check(workspace, seal_check);
    }
}
