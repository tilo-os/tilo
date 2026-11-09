//! Where a channel's plaintext gate is enforced: one gate per channel this device takes part in,
//! consulted on every ingress path of the host that admits an app's ops.
//!
//! The channel engine registers its gates here ([`ChannelGates`]); a host's broker holds the map
//! beside the state it admits ops against and never a key.

use std::collections::HashMap;
use std::sync::Mutex;

use collab_types::{ChannelId, EncryptionMetadata};

/// What a channel service registers its gates with: the ingress check of the host that admits an
/// app's ops.
pub trait ChannelGates: collab_types::signer::MaybeSendSync {
    /// Enforce `gate` on every op of `channel` from now on.
    fn register_channel_gate(&self, channel: ChannelId, gate: channel_crypto::ChannelGate);
}

/// The gates of a host that keeps none of its own: a service that registers into it refuses nothing
/// there, and only the encryption its participants agree keeps a channel sealed.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoGates;

impl ChannelGates for NoGates {
    fn register_channel_gate(&self, _channel: ChannelId, _gate: channel_crypto::ChannelGate) {}
}

/// The gate of every channel this device takes part in.
#[derive(Default)]
pub struct Gates(Mutex<HashMap<ChannelId, channel_crypto::ChannelGate>>);

impl Gates {
    /// Install or refresh a channel's ingress gate.
    pub fn register(&self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        if let Ok(mut gates) = self.0.lock() {
            gates.insert(channel, gate);
        }
    }

    /// The gate governing `channel`, one of `workspace`'s channels: the unit's own when it has a
    /// group of its own (a record whose readers the policy narrows, or one whose group was made
    /// before key scopes), and the workspace's key scope's otherwise, since a unit seals under its
    /// scope's keys while naming its own channel.
    #[must_use]
    pub fn gate_for(
        &self,
        workspace: &str,
        channel: ChannelId,
    ) -> Option<channel_crypto::ChannelGate> {
        let gates = self.0.lock().ok()?;
        gates.get(&channel).copied().or_else(|| {
            gates
                .get(&collab_types::workspace_scope(workspace))
                .copied()
        })
    }

    /// A route with no gate has not been opened encrypted on this device yet. Once its gate closes
    /// plaintext, no live plaintext envelope can pass either ingress path, including after a
    /// restart. Content sealed under any of the channel's epochs passes: a member's write reaches
    /// this device after the channel's next membership change.
    #[must_use]
    pub fn allows_document(
        &self,
        workspace: &str,
        channel: ChannelId,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        self.gate_for(workspace, channel)
            .is_none_or(|gate| gate.accepts(encryption, channel))
    }

    /// Whether a host's replay of `channel`'s retained history may enter: plaintext history passes
    /// (a record seals from some write on, and its cleartext writes stay the base its sealed writes
    /// are changes over until a sealed checkpoint covers them), live plaintext of every shape is
    /// still refused.
    #[must_use]
    pub fn allows_history(
        &self,
        workspace: &str,
        channel: ChannelId,
        encryption: Option<&EncryptionMetadata>,
    ) -> bool {
        encryption.is_none() || self.allows_document(workspace, channel, encryption)
    }

    /// The verdict on an op for `channel` from the gate governing it
    /// ([`channel_crypto::admission`]). A broker holding every channel's governance state knows a
    /// channel with no gate is one that is not encrypted.
    #[must_use]
    pub fn admission(
        &self,
        workspace: &str,
        channel: ChannelId,
        encryption: Option<&EncryptionMetadata>,
    ) -> collab_types::inbound::OpAdmission {
        channel_crypto::admission(self.gate_for(workspace, channel), true, channel, encryption)
    }
}

impl ChannelGates for Gates {
    fn register_channel_gate(&self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        self.register(channel, gate);
    }
}

impl ChannelGates for std::sync::Arc<dyn ChannelGates> {
    fn register_channel_gate(&self, channel: ChannelId, gate: channel_crypto::ChannelGate) {
        (**self).register_channel_gate(channel, gate);
    }
}
