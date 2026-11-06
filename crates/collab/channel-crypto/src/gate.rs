//! The gate a broker checks every document op of a channel against.

use collab_types::EncryptionMetadata;
use collab_types::inbound::OpAdmission;

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

/// The verdict on an op for `channel` from the gates that govern it there,
/// one implementation for every endpoint that decides: a sealed op is sealed
/// because the channel requires it, and cleartext is refused by any gate
/// that refuses it. Otherwise cleartext is admitted once the endpoint knows
/// the channel's whole governance state (`governance_known`), where no gate
/// means the channel is not encrypted; until then there is no verdict and
/// the op waits.
pub fn admission(
    gates: impl IntoIterator<Item = ChannelGate>,
    governance_known: bool,
    channel: collab_types::ChannelId,
    encryption: Option<&EncryptionMetadata>,
) -> OpAdmission {
    if encryption.is_some() {
        return OpAdmission::EncryptionRequired;
    }
    if !gates.into_iter().all(|gate| gate.accepts(None, channel)) {
        OpAdmission::EncryptionRequired
    } else if governance_known {
        OpAdmission::CleartextAdmitted
    } else {
        OpAdmission::Unstated
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

    #[test]
    fn cleartext_waits_until_governance_is_known_unless_a_gate_refuses_it() {
        use collab_types::inbound::OpAdmission;
        let channel = ChannelId::new();
        let open = ChannelGate::default();
        let required = ChannelGate {
            encryption_required: true,
        };
        assert_eq!(
            super::admission([], false, channel, None),
            OpAdmission::Unstated
        );
        assert_eq!(
            super::admission([open], false, channel, None),
            OpAdmission::Unstated
        );
        assert_eq!(
            super::admission([required], false, channel, None),
            OpAdmission::EncryptionRequired
        );
        assert_eq!(
            super::admission([], true, channel, None),
            OpAdmission::CleartextAdmitted
        );
        assert_eq!(
            super::admission([open, required], true, channel, None),
            OpAdmission::EncryptionRequired
        );
        let sealed = EncryptionMetadata::content_key(1, channel, ContentRetention::Delta);
        assert_eq!(
            super::admission([], false, channel, Some(&sealed)),
            OpAdmission::EncryptionRequired
        );
    }
}
