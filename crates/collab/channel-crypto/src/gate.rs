//! The gate a broker checks every document op of a channel against.

use collab_types::EncryptionMetadata;

/// What a broker checks every document op of a channel against: whether
/// plaintext is closed off there, and that ciphertext names the channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelGate {
    pub encryption_required: bool,
}

impl ChannelGate {
    /// Refuse plaintext once encryption is required, and ciphertext bound to
    /// another channel.
    ///
    /// An op sealed under an epoch the channel has since moved past is
    /// admitted: its author was a member when they wrote it, this device
    /// still holds the key it was sealed under, and a member's write
    /// routinely reaches another device after the channel's next membership
    /// change. Refusing it would lose that write for being a moment old,
    /// and reading it takes nothing from a device removed since: content
    /// written before the removal reached its author was readable by that
    /// device at the time.
    pub fn accepts(
        &self,
        encryption: Option<&EncryptionMetadata>,
        channel: collab_types::ChannelId,
    ) -> bool {
        !self.encryption_required || encryption.is_some_and(|metadata| metadata.channel == channel)
    }
}

#[cfg(test)]
mod gate_tests {
    use super::ChannelGate;
    use collab_types::{ChannelId, ContentRetention, EncryptionMetadata};

    /// Content sealed under an earlier epoch is admitted live as it is from
    /// the host's history; plaintext and other channels are refused either
    /// way.
    #[test]
    fn content_sealed_under_an_earlier_epoch_is_admitted_live() {
        let channel = ChannelId::new();
        let gate = ChannelGate {
            encryption_required: true,
        };
        let old = EncryptionMetadata::content_key(1, channel, ContentRetention::Delta);
        assert!(gate.accepts(Some(&old), channel));
        assert!(!gate.accepts(None, channel));
        let other = EncryptionMetadata::content_key(3, ChannelId::new(), ContentRetention::Delta);
        assert!(!gate.accepts(Some(&other), channel));
    }
}
