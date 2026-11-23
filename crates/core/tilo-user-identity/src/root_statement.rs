//! What a person's root signs so a machine keeps a copy of theirs: their
//! profile.
//!
//! A statement is a [`View`] a root signs under a fixed label, encoded through
//! the crate's one signing path — the BLAKE3 digest of the label and the
//! canonical encoding of the statement's kind, its issue time and the JSON text
//! exactly as it travels. A machine verifies that statement with the root
//! public key the statement itself names, so a statement cannot be dated by
//! anyone but its root, and an old one cannot be replayed over a newer one.
//!
//! The digest is one rule for the page that publishes and the machines that
//! keep: the page signs here, [`verify`] is what those machines run.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Serialize;

use crate::canonical::{CanonicalError, signing_digest};
use crate::sign::{SignError, StatementSigner};

/// What a root signs to have its profile kept.
pub const PROFILE_LABEL: &[u8] = b"tilo person profile v2\0";

/// Why a root statement was not signed, or does not verify.
#[derive(Debug, thiserror::Error)]
pub enum RootStatementError {
    /// The statement could not be encoded.
    #[error("the statement cannot be encoded: {0}")]
    Canonical(#[from] CanonicalError),
    /// The root's key refused.
    #[error("the root did not sign the statement: {0}")]
    Sign(#[from] SignError),
    /// The signature is not the root's over this statement.
    #[error("the statement does not verify: {0}")]
    Signature(#[from] ed25519_dalek::SignatureError),
}

/// What a root signs for its profile, issued at `issued_at_ms`.
#[derive(Serialize)]
struct View<'a> {
    kind: &'static str,
    issued_at_ms: u64,
    json: &'a str,
}

/// The digest a root signs for its profile, issued at `issued_at_ms`.
///
/// # Errors
///
/// [`CanonicalError::Serialize`] if the statement cannot be encoded
/// (effectively unreachable for a JSON text).
pub fn digest(issued_at_ms: u64, json: &str) -> Result<[u8; 32], CanonicalError> {
    signing_digest(
        PROFILE_LABEL,
        &View {
            kind: "profile",
            issued_at_ms,
            json,
        },
    )
}

/// `root`'s signature over [`digest`] of its profile.
///
/// # Errors
///
/// [`RootStatementError`] when the statement cannot be encoded or the root
/// refuses: then nothing is signed.
pub fn sign(
    root: &dyn StatementSigner,
    issued_at_ms: u64,
    json: &str,
) -> Result<[u8; 64], RootStatementError> {
    Ok(root.sign_digest(&digest(issued_at_ms, json)?)?)
}

/// Whether `signature` is `root`'s over [`digest`] of its profile.
///
/// # Errors
///
/// [`RootStatementError`] when the statement cannot be encoded, `root` is not
/// an Ed25519 public key, or the signature does not verify.
pub fn verify(
    root: &[u8; 32],
    issued_at_ms: u64,
    json: &str,
    signature: &[u8; 64],
) -> Result<(), RootStatementError> {
    VerifyingKey::from_bytes(root)?.verify(
        &digest(issued_at_ms, json)?,
        &Signature::from_bytes(signature),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[3; 32])
    }

    #[test]
    fn a_statement_binds_the_label_the_time_and_the_record() {
        let root = root();
        let public = root.verifying_key().to_bytes();
        let text = r#"{"version":1}"#;
        let signature = sign(&root, 1_700, text).unwrap();
        assert!(verify(&public, 1_700, text, &signature).is_ok());
        // Another time, another text: neither verifies.
        for (at, other) in [(1_701, text), (1_700, r#"{"version":2}"#)] {
            assert!(verify(&public, at, other, &signature).is_err());
        }
        // Signed by another root.
        let other = SigningKey::from_bytes(&[4; 32]);
        assert!(verify(&other.verifying_key().to_bytes(), 1_700, text, &signature).is_err());
    }

    /// A root that refuses: the statement is not signed at all.
    struct Refusing;

    impl StatementSigner for Refusing {
        fn public_key(&self) -> [u8; 32] {
            root().verifying_key().to_bytes()
        }

        fn sign_digest(&self, _digest: &[u8; 32]) -> Result<[u8; 64], SignError> {
            Err(SignError("locked".into()))
        }
    }

    #[test]
    fn a_refused_signature_issues_no_statement() {
        assert!(matches!(
            sign(&Refusing, 1_700, r#"{"version":1}"#),
            Err(RootStatementError::Sign(_))
        ));
    }
}
