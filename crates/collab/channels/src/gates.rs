//! Where a channel's plaintext gate is enforced: one gate per channel this device takes part in,
//! consulted on every ingress path of the host that admits an app's ops.
//!
//! The channel engine registers its gates here ([`ChannelGates`]); a host's broker holds the map
//! beside the state it admits ops against and never a key.

use std::collections::HashMap;
use std::sync::Mutex;

use collab_types::{EncryptionMetadata, WorkspaceId};

/// What a channel service registers its gates with: the ingress check of the host that admits an
/// app's ops.
pub trait ChannelGates: collab_types::signer::MaybeSendSync {
    /// Enforce `gate` on every op of `workspace` from now on.
    fn register_channel_gate(&self, workspace: WorkspaceId, gate: channel_crypto::ChannelGate);
}

/// The gates of a host that keeps none of its own: a service that registers into it refuses nothing
/// there, and only the encryption its participants agree keeps a channel sealed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGates;

impl ChannelGates for NoGates {
    fn register_channel_gate(&self, _workspace: WorkspaceId, _gate: channel_crypto::ChannelGate) {}
}

/// The gate of every channel this device takes part in.
#[derive(Default)]
pub struct Gates(Mutex<HashMap<WorkspaceId, channel_crypto::ChannelGate>>);

impl Gates {
    /// Install or refresh a channel's ingress gate.
    pub fn register(&self, workspace: WorkspaceId, gate: channel_crypto::ChannelGate) {
        if let Ok(mut gates) = self.0.lock() {
            gates.insert(workspace, gate);
        }
    }

    /// The gate governing every record of `workspace`: its group's.
    #[must_use]
    pub fn gate_for(&self, workspace: &str) -> Option<channel_crypto::ChannelGate> {
        let workspace = WorkspaceId::parse(workspace)?;
        self.0.lock().ok()?.get(&workspace).copied()
    }

    /// A route with no gate has not been opened encrypted on this device yet. Once its gate closes
    /// plaintext, no live plaintext envelope can pass either ingress path, including after a
    /// restart. Content sealed under any of the channel's epochs passes: a member's write reaches
    /// this device after the channel's next membership change.
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
impl ChannelGates for Gates {
    fn register_channel_gate(&self, workspace: WorkspaceId, gate: channel_crypto::ChannelGate) {
        self.register(workspace, gate);
    }
}

impl ChannelGates for std::sync::Arc<dyn ChannelGates> {
    fn register_channel_gate(&self, workspace: WorkspaceId, gate: channel_crypto::ChannelGate) {
        (**self).register_channel_gate(workspace, gate);
    }
}
