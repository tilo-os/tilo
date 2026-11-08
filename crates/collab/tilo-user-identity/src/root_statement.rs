//! What a person's root signs so a host keeps a record of theirs: their root
//! envelope, and their profile.
//!
//! Both are JSON text a root signs under a fixed label: the label's bytes,
//! the issue time as a `u64` (milliseconds, little-endian), then the record
//! exactly as it travels. A host verifies that statement with the root public
//! key the record itself names, so a record cannot be dated by anyone but its
//! root, and an old record (with a passkey since dropped) cannot be replayed
//! over a newer one.
//!
//! The bytes are one rule for the page that publishes and the hosts that
//! keep: the page signs here, [`verify`] is what a host runs.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// What a root signs to have its envelope kept.
pub const ENVELOPE_LABEL: &[u8] = b"tilo root envelope v1\0";
/// What a root signs to have its profile kept.
pub const PROFILE_LABEL: &[u8] = b"tilo person profile v1\0";

/// Which record of a person's a statement carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// The root envelope.
    Envelope,
    /// The person's profile.
    Profile,
}

impl Record {
    /// The label the root signs this record's statements under.
    #[must_use]
    pub const fn label(self) -> &'static [u8] {
        match self {
            Self::Envelope => ENVELOPE_LABEL,
            Self::Profile => PROFILE_LABEL,
        }
    }

    /// The record named `name`, as a page or a route says it.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "envelope" => Some(Self::Envelope),
            "profile" => Some(Self::Profile),
            _ => None,
        }
    }

    /// The record's name in JSON bodies and fetch answers.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Envelope => "envelope",
            Self::Profile => "profile",
        }
    }
}

/// The bytes a root signs for `record`, issued at `issued_at_ms`.
#[must_use]
pub fn statement(record: Record, issued_at_ms: u64, json: &str) -> Vec<u8> {
    let label = record.label();
    let mut bytes = Vec::with_capacity(label.len().saturating_add(8).saturating_add(json.len()));
    bytes.extend_from_slice(label);
    bytes.extend_from_slice(&issued_at_ms.to_le_bytes());
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

/// `root`'s signature over [`statement`] of `record`.
#[must_use]
pub fn sign(root: &SigningKey, record: Record, issued_at_ms: u64, json: &str) -> [u8; 64] {
    root.sign(&statement(record, issued_at_ms, json)).to_bytes()
}

/// Whether `signature` is `root`'s over [`statement`] of `record`.
///
/// # Errors
///
/// [`SignatureError`] when `root` is not an Ed25519 public key or the
/// signature does not verify.
pub fn verify(
    root: &[u8; 32],
    record: Record,
    issued_at_ms: u64,
    json: &str,
    signature: &[u8; 64],
) -> Result<(), ed25519_dalek::SignatureError> {
    VerifyingKey::from_bytes(root)?.verify(
        &statement(record, issued_at_ms, json),
        &Signature::from_bytes(signature),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    #[test]
    fn a_statement_binds_the_label_the_time_and_the_record() {
        let root = root();
        let public = root.verifying_key().to_bytes();
        let text = r#"{"version":1}"#;
        let signature = sign(&root, Record::Envelope, 1_700, text);
        assert!(verify(&public, Record::Envelope, 1_700, text, &signature).is_ok());
        assert_eq!(
            statement(Record::Envelope, 1_700, text),
            [ENVELOPE_LABEL, &1_700_u64.to_le_bytes(), text.as_bytes()].concat()
        );
        // Another record kind, another time, another text: none verifies.
        for (record, at, other) in [
            (Record::Profile, 1_700, text),
            (Record::Envelope, 1_701, text),
            (Record::Envelope, 1_700, r#"{"version":2}"#),
        ] {
            assert!(verify(&public, record, at, other, &signature).is_err());
        }
        // Signed by another root.
        let other = SigningKey::from_bytes(&[4; 32]);
        assert!(
            verify(
                &other.verifying_key().to_bytes(),
                Record::Envelope,
                1_700,
                text,
                &signature
            )
            .is_err()
        );
        assert_eq!(Record::named("profile"), Some(Record::Profile));
        assert_eq!(Record::named("envelope"), Some(Record::Envelope));
        assert_eq!(Record::named("photo"), None);
        assert_eq!(Record::Profile.name(), "profile");
    }
}
