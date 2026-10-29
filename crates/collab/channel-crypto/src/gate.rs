//! The gate a broker checks every document op of a channel against.

use collab_types::EncryptionMetadata;
use collab_types::inbound::OpAdmission;

/// What a broker checks every document op of a channel against: whether
/// plaintext is closed off, and the epoch of the latest membership change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelGate {
    pub encryption_required: bool,
    pub head_epoch: Option<u64>,
}

impl ChannelGate {
    /// Refuse plaintext once encryption is required, ciphertext bound to
    /// another channel, and content sealed under a content key older than
    /// the latest membership change: a removed member still holds those
    /// keys, so it must not inject content under them, and a lagging member
    /// must catch up first (otherwise the removed member could read it).
    pub fn accepts(
        &self,
        encryption: Option<&EncryptionMetadata>,
        channel: collab_types::ChannelId,
    ) -> bool {
        if let Some(metadata) = encryption
            && metadata.retention().is_some()
            && self.head_epoch.is_some_and(|head| metadata.epoch < head)
        {
            return false;
        }
        !self.encryption_required || encryption.is_some_and(|metadata| metadata.channel == channel)
    }

    /// [`Self::accepts`] for history a workspace host replays in its own
    /// order: content sealed under an older epoch was current when the host
    /// admitted it (the host admits only the room's members), so only
    /// plaintext on an encrypted channel and ciphertext bound to another
    /// channel are refused.
    pub fn accepts_history(
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

    /// Live content sealed under a key older than the latest membership
    /// change is refused (a removed member still holds it); the same content
    /// replayed by the host from its history was current when admitted and
    /// is accepted. Plaintext and other channels are refused either way.
    #[test]
    fn host_history_under_an_older_epoch_is_accepted_live_content_is_not() {
        let channel = ChannelId::new();
        let gate = ChannelGate {
            encryption_required: true,
            head_epoch: Some(3),
        };
        let old = EncryptionMetadata::content_key(1, channel, ContentRetention::Delta);
        assert!(!gate.accepts(Some(&old), channel));
        assert!(gate.accepts_history(Some(&old), channel));
        assert!(!gate.accepts_history(None, channel));
        let other = EncryptionMetadata::content_key(3, ChannelId::new(), ContentRetention::Delta);
        assert!(!gate.accepts_history(Some(&other), channel));
    }

    #[test]
    fn cleartext_waits_until_governance_is_known_unless_a_gate_refuses_it() {
        use collab_types::inbound::OpAdmission;
        let channel = ChannelId::new();
        let open = ChannelGate::default();
        let required = ChannelGate {
            encryption_required: true,
            head_epoch: None,
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
