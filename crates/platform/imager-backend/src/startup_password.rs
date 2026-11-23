//! Startup password admission for host-prepared installation artifacts.
//! A host cannot attest the destination's TPM policy, so preparation always
//! requires an independent disk passphrase rather than a TPM-protected PIN.

/// The destination's own rule on the live installer, which can attest its
/// TPM policy: a short TPM PIN when `tpm_pin`, else the strong passphrase.
pub fn validate_on_destination(secret: &str, tpm_pin: bool) -> Result<(), &'static str> {
    tilo_unlock_protocol::startup_secret::validate(secret, tpm_pin)
}

/// Presentation-independent rejection of a prepared installation password.
pub use tilo_unlock_protocol::startup_secret::ValidationError;

/// Admit a password for preparing identity, account and media artifacts.
///
/// # Errors
/// Returns the failed disk-passphrase requirement, without retaining the secret.
pub fn validate(password: &str) -> Result<(), ValidationError> {
    tilo_unlock_protocol::startup_secret::validate_typed(password, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// Preparing and admitting the startup password.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_startup_password_is_prepared_and_admitted_by_policy;

        phase preparation_requires_a_passphrase_even_for_a_valid_tpm_pin;
        {
            assert!(tilo_unlock_protocol::startup_secret::validate_typed("539172", true).is_ok());
            assert_eq!(validate("539172"), Err(ValidationError::PassphraseTooShort));
        }

        phase typed_admission_preserves_policy_reasons_and_diagnostics;
        {
            for (password, reason) in [
                (
                    " several unrelated installation words",
                    ValidationError::SurroundingSpaces,
                ),
                ("short", ValidationError::PassphraseTooShort),
                ("password123456789", ValidationError::PassphraseCommon),
                (
                    "1234567890123456",
                    ValidationError::NumericPassphraseTooShort,
                ),
                ("aaaaaaaaaaaaaaa", ValidationError::PassphraseRepetitive),
            ] {
                assert_eq!(validate(password), Err(reason));
                assert_eq!(
                    reason.to_string(),
                    tilo_unlock_protocol::startup_secret::validate(password, false).unwrap_err()
                );
            }
            assert_eq!(validate("several unrelated installation words"), Ok(()));
        }
    }
}
