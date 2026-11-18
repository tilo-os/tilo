//! The check a broker checks every document op of a workspace against.

use collab_types::EncryptionMetadata;

/// What a broker checks every document op of a workspace against: whether
/// plaintext is closed off there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SealCheck {
    pub encryption_required: bool,
}

impl SealCheck {
    /// Refuse plaintext once encryption is required.
    ///
    /// An op sealed under an epoch the workspace has since moved past is
    /// admitted: its author was a member when they wrote it, this device
    /// still holds the key it was sealed under, and a member's write
    /// routinely reaches another device after the workspace's next membership
    /// change. Refusing it would lose that write for being a moment old,
    /// and reading it takes nothing from a device removed since: content
    /// written before the removal reached its author was readable by that
    /// device at the time.
    pub fn accepts(&self, encryption: Option<&EncryptionMetadata>) -> bool {
        !self.encryption_required || encryption.is_some()
    }
}

#[cfg(test)]
mod seal_check_tests {
    use super::SealCheck;
    use collab_types::{ContentRetention, EncryptionMetadata};

    /// Content sealed under an earlier epoch is admitted live as it is from
    /// the host's history; plaintext is refused.
    #[test]
    fn content_sealed_under_an_earlier_epoch_is_admitted_live() {
        let seal_check = SealCheck {
            encryption_required: true,
        };
        let old = EncryptionMetadata::content_key(1, ContentRetention::Delta);
        assert!(seal_check.accepts(Some(&old)));
        assert!(!seal_check.accepts(None));
    }
}
