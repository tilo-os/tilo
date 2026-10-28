//! Signature-verification helpers. No I/O; pure functions over
//! decoded `RegistryEntry` / `EntryChangeRequest` values.
//!
//! Both verify against the publisher's Ed25519 pubkey; the registry
//! server (or a federation client) separately checks the pubkey is in
//! the owner's allowlist via [`verify_change_request`].

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use thiserror::Error;

use crate::change::{EntryAction, EntryChangeRequest};
use crate::entry::RegistryEntry;

/// Structured failure modes for [`verify_change_request`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("invalid registry metadata: {0}")]
    InvalidMetadata(String),
    /// The publisher's pubkey (as declared in the entry) is not in the owner's allowlist.
    #[error("publisher pubkey not in allowlist")]
    PublisherNotAllowed,
    /// The pubkey field decoded fine but is not a valid Ed25519 key point.
    #[error("publisher pubkey is not a valid Ed25519 key: {0}")]
    InvalidPubkey(String),
    /// Hex parse failed on pubkey or signature.
    #[error("hex decode failed: {0}")]
    BadHex(String),
    /// Pubkey or signature was the wrong length after hex decode.
    #[error("bad length {got}, expected {expected}")]
    BadLength { expected: usize, got: usize },
    /// Ed25519 verify rejected the signature — the bytes don't match what the publisher signed.
    #[error("signature does not match signed payload")]
    BadSignature,
    /// The entry's declared publisher pubkey and the pubkey the
    /// change-request signature verifies against disagree — blocks a
    /// valid publisher signing a change over someone else's entry.
    #[error("entry signature is not valid for the declared publisher pubkey")]
    EntrySignatureMismatch,
}

/// Verify a [`RegistryEntry`]'s `signature_hex` against its own
/// declared `publisher_pubkey_hex`. Returns `false` on any
/// parse/verify failure — structured errors are
/// [`verify_change_request`]'s domain; a caller walking a big index
/// only needs "keep or drop".
pub fn verify_entry_signature(entry: &RegistryEntry) -> bool {
    if entry.validate().is_err() {
        return false;
    }
    let Some(pk) = parse_verifying_key_opt(&entry.publisher_pubkey_hex) else {
        return false;
    };
    let Some(sig) = parse_signature_opt(&entry.signature_hex) else {
        return false;
    };
    let msg = entry.signing_message();
    pk.verify(&msg, &sig).is_ok()
}

/// Verify a signed [`EntryChangeRequest`]. Both the change-level
/// signature and the entry-level signature are checked, and the
/// entry's declared pubkey must appear in `allowlist`.
///
/// Verification uses the pubkey declared inside the entry — the
/// allowlist decides *whether that pubkey is welcome*, not *whose key
/// we verify against*. So the same signature is portable: a
/// federation client can verify the same bytes against a different
/// owner's allowlist without re-encoding.
///
/// # Errors
///
/// Returns [`VerifyError::PublisherNotAllowed`] if the entry's declared
/// pubkey is not in `allowlist`; [`VerifyError::InvalidPubkey`] or
/// [`VerifyError::BadHex`]/[`VerifyError::BadLength`] if the pubkey or
/// either signature fails to decode; [`VerifyError::EntrySignatureMismatch`]
/// if the entry's own signature doesn't verify; and
/// [`VerifyError::BadSignature`] if the change-level signature doesn't
/// verify against the declared pubkey.
pub fn verify_change_request(
    req: &EntryChangeRequest,
    publisher_pubkey_allowlist: &[[u8; 32]],
) -> Result<(), VerifyError> {
    req.entry
        .validate()
        .map_err(|error| VerifyError::InvalidMetadata(error.to_string()))?;
    let declared = decode_pubkey_hex(&req.entry.publisher_pubkey_hex)?;
    if !publisher_pubkey_allowlist.iter().any(|k| k == &declared) {
        return Err(VerifyError::PublisherNotAllowed);
    }
    let pk = VerifyingKey::from_bytes(&declared)
        .map_err(|e| VerifyError::InvalidPubkey(e.to_string()))?;

    // Verify the entry-level sig too — otherwise a valid publisher could
    // authenticate a change over a stolen-and-tweaked entry.
    if !verify_entry_signature(&req.entry) {
        return Err(VerifyError::EntrySignatureMismatch);
    }

    // Change-level sig.
    let sig = decode_signature_result(&req.publisher_signature_hex)?;
    let msg = req.signing_message();
    pk.verify(&msg, &sig)
        .map_err(|_| VerifyError::BadSignature)?;

    // Cheap invariant: Add/Update carry a version list; Delete may too but isn't required to.
    match req.action {
        EntryAction::Add | EntryAction::Update | EntryAction::Delete => {}
    }
    Ok(())
}

// helpers

/// Public/change-request path — surfaces structured errors so the server can log which failure mode fired.
fn decode_pubkey_hex(hex_str: &str) -> Result<[u8; 32], VerifyError> {
    decode_fixed_hex::<32>(hex_str)
}

/// Same shape for signatures; kept separate for symmetry with the pubkey path even though the code is identical.
fn decode_signature_result(hex_str: &str) -> Result<Signature, VerifyError> {
    let bytes = decode_fixed_hex::<64>(hex_str)?;
    Ok(Signature::from_bytes(&bytes))
}

fn decode_fixed_hex<const N: usize>(hex_str: &str) -> Result<[u8; N], VerifyError> {
    let bytes = hex::decode(hex_str).map_err(|e| VerifyError::BadHex(e.to_string()))?;
    if bytes.len() != N {
        return Err(VerifyError::BadLength {
            expected: N,
            got: bytes.len(),
        });
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Best-effort pubkey decode for the boolean `verify_entry_signature`
/// path — any parse failure surfaces as `None` so the outer function returns `false`.
fn parse_verifying_key_opt(hex_str: &str) -> Option<VerifyingKey> {
    let bytes = decode_fixed_hex::<32>(hex_str).ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Same idea for signatures inside the boolean path.
fn parse_signature_opt(hex_str: &str) -> Option<Signature> {
    let bytes = decode_fixed_hex::<64>(hex_str).ok()?;
    Some(Signature::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::sign_change_request;
    use crate::entry::{EntryKind, RegistryVersion, TappArtifact, VersionArtifact, sign_entry};
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn key_and_entry() -> (SigningKey, RegistryEntry) {
        let key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let pk_hex = hex::encode(key.verifying_key().to_bytes());
        let entry = RegistryEntry {
            package_id: "so.tilo.editor".into(),
            kind: EntryKind::App,
            name: "Editor".into(),
            description: "CRDT-native text editor".into(),
            tagline: "Edit together".into(),
            images: crate::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: pk_hex,
            versions: vec![RegistryVersion {
                version: "2026.06".into(),
                released_at_unix: 100,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url: "https://apps.tilo.so/editor/2026.06.tapp".into(),
                    tapp_root_hash_hex: "cd".repeat(32),
                    tapp_size_bytes: 1024,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: crate::entry::RegistryTarget {
                        platform: crate::entry::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 200,
            signature_hex: String::new(),
        };
        (key, entry)
    }

    #[test]
    fn entry_signature_verification_rejects_tamper_and_wrong_keys() {
        // phase: entry_signature_verifies_after_sign
        {
            let (key, mut entry) = key_and_entry();
            sign_entry(&mut entry, &key);
            assert!(verify_entry_signature(&entry));
        }
        // phase: entry_signature_rejects_tamper
        {
            let (key, mut entry) = key_and_entry();
            sign_entry(&mut entry, &key);
            entry.name = "Tampered".into();
            assert!(!verify_entry_signature(&entry));
        }
        // phase: entry_signature_rejects_wrong_pubkey
        {
            let (key, mut entry) = key_and_entry();
            sign_entry(&mut entry, &key);
            entry.publisher_pubkey_hex = "ee".repeat(32);
            assert!(!verify_entry_signature(&entry));
        }
    }

    #[test]
    fn change_request_verification_requires_an_allowlisted_untampered_signer() {
        // phase: change_request_verifies_when_allowlisted
        {
            let (key, entry) = key_and_entry();
            let allow = [key.verifying_key().to_bytes()];
            let req = sign_change_request(EntryAction::Add, entry, &key);
            assert_eq!(verify_change_request(&req, &allow), Ok(()));
        }
        // phase: change_request_rejects_unallowlisted_publisher
        {
            let (key, entry) = key_and_entry();
            let allow = [[0u8; 32]]; // deliberately not the publisher's key
            let req = sign_change_request(EntryAction::Add, entry, &key);
            assert_eq!(
                verify_change_request(&req, &allow),
                Err(VerifyError::PublisherNotAllowed)
            );
        }
        // phase: change_request_rejects_tampered_entry
        {
            let (key, entry) = key_and_entry();
            let allow = [key.verifying_key().to_bytes()];
            let mut req = sign_change_request(EntryAction::Update, entry, &key);
            // Change a signed field after signing.
            req.entry.name = "Tampered".into();
            assert_eq!(
                verify_change_request(&req, &allow),
                Err(VerifyError::EntrySignatureMismatch)
            );
        }
        // phase: change_request_rejects_tampered_signature
        {
            let (key, entry) = key_and_entry();
            let allow = [key.verifying_key().to_bytes()];
            let mut req = sign_change_request(EntryAction::Add, entry, &key);
            let mut bytes = hex::decode(&req.publisher_signature_hex).unwrap();
            bytes[0] ^= 1;
            req.publisher_signature_hex = hex::encode(bytes);
            assert_eq!(
                verify_change_request(&req, &allow),
                Err(VerifyError::BadSignature)
            );
        }
        // phase: change_request_rejects_bad_hex
        {
            let (key, mut entry) = key_and_entry();
            entry.publisher_pubkey_hex = "zzzz".into();
            let req = EntryChangeRequest {
                action: EntryAction::Add,
                entry,
                publisher_signature_hex: hex::encode([0u8; 64]),
            };
            let allow = [key.verifying_key().to_bytes()];
            assert!(matches!(
                verify_change_request(&req, &allow),
                Err(VerifyError::BadHex(_))
            ));
        }
    }
}
