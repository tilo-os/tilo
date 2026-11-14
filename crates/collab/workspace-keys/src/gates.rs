//! Where a group's plaintext gate is enforced: one gate per group this device takes part in,
//! consulted on every ingress path of the host that admits an app's ops.
//!
//! The group engine registers its gates here ([`GateRegistry`]); a host's broker holds the map
//! beside the state it admits ops against and never a key.

use std::collections::HashMap;
use std::sync::Mutex;

use collab_types::{EncryptionMetadata, WorkspaceId};

/// What a key service registers its gates with: the ingress check of the host that admits an
/// app's ops.
pub trait GateRegistry: collab_types::signer::MaybeSendSync {
    /// Enforce `gate` on every op of `workspace` from now on.
    fn register_gate(&self, workspace: WorkspaceId, gate: key_group::SealGate);
}

/// The gates of a host that keeps none of its own: a service that registers into it refuses nothing
/// there, and only the encryption its participants agree keeps a group sealed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGates;

impl GateRegistry for NoGates {
    fn register_gate(&self, _workspace: WorkspaceId, _gate: key_group::SealGate) {}
}

/// The gate of every group this device takes part in.
#[derive(Default)]
pub struct Gates(Mutex<HashMap<WorkspaceId, key_group::SealGate>>);

impl Gates {
    /// Install or refresh a group's ingress gate.
    pub fn register(&self, workspace: WorkspaceId, gate: key_group::SealGate) {
        if let Ok(mut gates) = self.0.lock() {
            gates.insert(workspace, gate);
        }
    }

    /// The gate governing every record of `workspace`: its group's.
    #[must_use]
    pub fn gate_for(&self, workspace: &str) -> Option<key_group::SealGate> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.0.lock().ok()?.get(&workspace).copied()
    }

    /// A route with no gate has not been opened encrypted on this device yet. Once its gate closes
    /// plaintext, no live plaintext envelope can pass either ingress path, including after a
    /// restart. Content sealed under any of the group's epochs passes: a member's write reaches
    /// this device after the group's next membership change.
    #[must_use]
    pub fn allows_document(
        &self,
        workspace: &str,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        self.gate_for(workspace)
            .is_none_or(|gate| gate.accepts(encryption))
    }
}
impl GateRegistry for Gates {
    fn register_gate(&self, workspace: WorkspaceId, gate: key_group::SealGate) {
        self.register(workspace, gate);
    }
}

impl GateRegistry for std::sync::Arc<dyn GateRegistry> {
    fn register_gate(&self, workspace: WorkspaceId, gate: key_group::SealGate) {
        (**self).register_gate(workspace, gate);
    }
}
