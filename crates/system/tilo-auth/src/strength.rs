use crate::secret::Secret;

/// Whether `secret` may stand alone as a passphrase, with nothing throttling
/// guesses at it: the installer's disk-passphrase rule.
pub fn passphrase_is_strong(secret: &Secret) -> bool {
    tilo_unlock_protocol::startup_secret::validate_typed(secret.as_str(), false).is_ok()
}
