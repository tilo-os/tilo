//! Startup password admission for host-prepared installation artifacts.
//! A host cannot attest the destination's TPM policy, so preparation always
//! requires an independent disk passphrase rather than a TPM-protected PIN.

/// Presentation-independent rejection of a prepared installation password.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    SurroundingSpaces,
    PassphraseTooShort,
    PassphraseCommon,
    NumericPassphraseTooShort,
    PassphraseRepetitive,
    PassphraseRepeatedPattern,
    Invalid,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::SurroundingSpaces => "Do not begin or end the disk passphrase with spaces",
            Self::PassphraseTooShort => "Use at least 15 characters for the disk passphrase",
            Self::PassphraseCommon => {
                "That passphrase is too easy to guess; use several unrelated words"
            }
            Self::NumericPassphraseTooShort => {
                "Use at least 20 digits for an all-numeric disk passphrase"
            }
            Self::PassphraseRepetitive => {
                "That passphrase is too repetitive; use several unrelated words"
            }
            Self::PassphraseRepeatedPattern => {
                "That passphrase is a repeated pattern; use several unrelated words"
            }
            Self::Invalid => "Choose a valid disk passphrase",
        })
    }
}

impl std::error::Error for ValidationError {}

/// Admit a password for preparing identity, account and media artifacts.
///
/// # Errors
/// Returns the failed disk-passphrase requirement, without retaining the secret.
pub fn validate(password: &str) -> Result<(), ValidationError> {
    use tilo_unlock_protocol::startup_secret::{self, ValidationError as PolicyError};
    startup_secret::validate_typed(password, false).map_err(|reason| match reason {
        PolicyError::SurroundingSpaces => ValidationError::SurroundingSpaces,
        PolicyError::PassphraseTooShort => ValidationError::PassphraseTooShort,
        PolicyError::PassphraseCommon => ValidationError::PassphraseCommon,
        PolicyError::NumericPassphraseTooShort => ValidationError::NumericPassphraseTooShort,
        PolicyError::PassphraseRepetitive => ValidationError::PassphraseRepetitive,
        PolicyError::PassphraseRepeatedPattern => ValidationError::PassphraseRepeatedPattern,
        PolicyError::PinTooShort | PolicyError::PinNotNumeric | PolicyError::PinPredictable => {
            ValidationError::Invalid
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_requires_a_passphrase_even_for_a_valid_tpm_pin() {
        assert!(tilo_unlock_protocol::startup_secret::validate_typed("539172", true).is_ok());
        assert_eq!(validate("539172"), Err(ValidationError::PassphraseTooShort));
    }

    #[test]
    fn typed_admission_preserves_policy_reasons_and_diagnostics() {
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
