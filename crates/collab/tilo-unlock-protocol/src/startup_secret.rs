//! Shared startup-secret policy used during host preparation and native installation.
//! Host preparation uses the independent disk passphrase policy until the target
//! proves its measured-boot TPM policy.

/// A stable reason for rejecting a startup secret, independent of presentation
/// language. Hosts can explain the actual failed requirement in their locale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    SurroundingSpaces,
    PinTooShort,
    PinNotNumeric,
    PinPredictable,
    PassphraseTooShort,
    PassphraseCommon,
    NumericPassphraseTooShort,
    PassphraseRepetitive,
    PassphraseRepeatedPattern,
}

impl ValidationError {
    /// The existing wire/helper diagnostic, retained for callers of `validate`.
    pub const fn message(self) -> &'static str {
        match self {
            Self::SurroundingSpaces => "Do not begin or end the disk passphrase with spaces",
            Self::PinTooShort => "Use at least 6 digits for the TPM-protected startup PIN",
            Self::PinNotNumeric => "The TPM-protected startup PIN must contain digits only",
            Self::PinPredictable => "Choose a less predictable startup PIN",
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
        }
    }
}

/// Validate a destination startup secret against its actual unlock policy.
///
/// # Errors
/// Returns the precise requirement that the supplied secret does not satisfy.
pub fn validate(secret: &str, tpm_pin: bool) -> Result<(), &'static str> {
    validate_typed(secret, tpm_pin).map_err(ValidationError::message)
}

/// Validate against the same startup policy with a presentation-independent
/// rejection reason. `validate` retains its original diagnostic contract.
pub fn validate_typed(secret: &str, tpm_pin: bool) -> Result<(), ValidationError> {
    const COMMON_BASES: &[&str] = &[
        "password",
        "qwerty",
        "letmein",
        "welcome",
        "administrator",
        "admin",
        "tilo",
        "quickdesk",
        "tilo",
    ];

    if secret.trim() != secret {
        return Err(ValidationError::SurroundingSpaces);
    }
    let chars: Vec<char> = secret.chars().collect();
    if tpm_pin {
        if chars.len() < 6 {
            return Err(ValidationError::PinTooShort);
        }
        if !chars.iter().all(char::is_ascii_digit) {
            return Err(ValidationError::PinNotNumeric);
        }
        if chars
            .first()
            .is_some_and(|first| chars.iter().all(|c| c == first))
            || ["012345", "123456", "234567", "345678", "456789", "987654"]
                .iter()
                .any(|bad| secret.contains(bad))
        {
            return Err(ValidationError::PinPredictable);
        }
        return Ok(());
    }
    if chars.len() < 15 {
        return Err(ValidationError::PassphraseTooShort);
    }
    let lower = secret.to_lowercase();
    if COMMON_BASES.iter().any(|base| {
        lower
            .strip_prefix(base)
            .is_some_and(|tail| tail.chars().all(|c| !c.is_alphabetic()))
    }) {
        return Err(ValidationError::PassphraseCommon);
    }
    if chars.iter().all(char::is_ascii_digit) && chars.len() < 20 {
        return Err(ValidationError::NumericPassphraseTooShort);
    }
    let unique: std::collections::HashSet<char> = chars.iter().copied().collect();
    if unique.len() < 5 {
        return Err(ValidationError::PassphraseRepetitive);
    }
    for period in 1..=4 {
        if chars.len().is_multiple_of(period)
            && chars
                .get(..period)
                .is_some_and(|prefix| chars.chunks(period).all(|chunk| chunk == prefix))
        {
            return Err(ValidationError::PassphraseRepeatedPattern);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_startup_reasons_preserve_policy_and_legacy_diagnostics() {
        for (secret, tpm_pin, reason, diagnostic) in [
            (
                " several unrelated installation words",
                false,
                ValidationError::SurroundingSpaces,
                "Do not begin or end the disk passphrase with spaces",
            ),
            (
                "short",
                false,
                ValidationError::PassphraseTooShort,
                "Use at least 15 characters for the disk passphrase",
            ),
            (
                "password123456789",
                false,
                ValidationError::PassphraseCommon,
                "That passphrase is too easy to guess; use several unrelated words",
            ),
            (
                "1234567890123456",
                false,
                ValidationError::NumericPassphraseTooShort,
                "Use at least 20 digits for an all-numeric disk passphrase",
            ),
            (
                "aaaaaaaaaaaaaaa",
                false,
                ValidationError::PassphraseRepetitive,
                "That passphrase is too repetitive; use several unrelated words",
            ),
            (
                "abcde",
                true,
                ValidationError::PinTooShort,
                "Use at least 6 digits for the TPM-protected startup PIN",
            ),
            (
                "abc123",
                true,
                ValidationError::PinNotNumeric,
                "The TPM-protected startup PIN must contain digits only",
            ),
            (
                "123456",
                true,
                ValidationError::PinPredictable,
                "Choose a less predictable startup PIN",
            ),
        ] {
            assert_eq!(validate_typed(secret, tpm_pin), Err(reason));
            assert_eq!(validate(secret, tpm_pin), Err(diagnostic));
        }
        for (secret, tpm_pin) in [
            ("several unrelated installation words", false),
            ("123456789012345a", false),
            ("10394857261039485726", false),
            ("539172", true),
        ] {
            assert_eq!(validate_typed(secret, tpm_pin), Ok(()));
            assert_eq!(validate(secret, tpm_pin), Ok(()));
        }
    }
}
