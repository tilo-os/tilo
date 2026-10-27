use crate::secret::Secret;

/// Whether `secret` may stand alone as a passphrase, with nothing throttling
/// guesses at it: the installer's disk-passphrase rule.
pub fn passphrase_is_strong(secret: &Secret) -> bool {
    tilo_unlock_protocol::startup_secret::validate_typed(secret.as_str(), false).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_secret_that_carries_its_own_entropy_stands_alone() {
        assert!(passphrase_is_strong(&Secret::new(
            "violet ladder orbit pancake"
        )));
        for weak in [
            "123456",
            "tilo",
            "password123456789",
            "aaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(!passphrase_is_strong(&Secret::new(weak)), "{weak}");
        }
    }
}
