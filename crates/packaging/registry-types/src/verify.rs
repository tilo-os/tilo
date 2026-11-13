//! Signature verification over decoded `RegistryEntry` and `EntryChangeRequest` values: pure, no
//! I/O. The registry server checks the publisher key against the owner's allowlist separately.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use thiserror::Error;

use crate::change::{EntryAction, EntryChangeRequest};
use crate::entry::RegistryEntry;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("invalid registry metadata: {0}")]
    InvalidMetadata(String),
    #[error("publisher pubkey not in allowlist")]
    PublisherNotAllowed,
    #[error("publisher pubkey is not a valid Ed25519 key: {0}")]
    InvalidPubkey(String),
    #[error("hex decode failed: {0}")]
    BadHex(String),
    #[error("bad length {got}, expected {expected}")]
    BadLength { expected: usize, got: usize },
    #[error("signature does not match signed payload")]
    BadSignature,
    /// The entry's declared pubkey and the one the change verifies against disagree, which stops a
    /// publisher signing a change over someone else's entry.
    #[error("entry signature is not valid for the declared publisher pubkey")]
    EntrySignatureMismatch,
}

/// Whether `entry`'s signature verifies against its own declared `publisher_pubkey_hex`. Any parse
/// or verify failure is `false`: a caller walking a big index only needs keep or drop.
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

/// Verify a signed [`EntryChangeRequest`]: both the change-level and the entry-level signature, with
/// the entry's declared pubkey in `allowlist`.
///
/// The declared pubkey is what the signature verifies against; the allowlist only decides whether that
/// key is welcome, so the same bytes verify against another owner's allowlist.
///
/// # Errors
///
/// [`VerifyError`] names the failure: a publisher not allowed, a key or signature that does not
/// decode, an entry signature that does not verify, or a change signature that does not verify.
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

    // The entry's own signature too, or a publisher could authenticate a change over a stolen entry.
    if !verify_entry_signature(&req.entry) {
        return Err(VerifyError::EntrySignatureMismatch);
    }

    let sig = decode_signature_result(&req.publisher_signature_hex)?;
    let msg = req.signing_message();
    pk.verify(&msg, &sig)
        .map_err(|_| VerifyError::BadSignature)?;

    // Add and Update carry a version list; Delete may.
    match req.action {
        EntryAction::Add | EntryAction::Update | EntryAction::Delete => {}
    }
    Ok(())
}

/// Errors name the failure mode, for the server to log.
fn decode_pubkey_hex(hex_str: &str) -> Result<[u8; 32], VerifyError> {
    decode_fixed_hex::<32>(hex_str)
}

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

fn parse_verifying_key_opt(hex_str: &str) -> Option<VerifyingKey> {
    let bytes = decode_fixed_hex::<32>(hex_str).ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

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
        {
            let (key, mut entry) = key_and_entry();
            sign_entry(&mut entry, &key);
            assert!(verify_entry_signature(&entry));
        }
        {
            let (key, mut entry) = key_and_entry();
            sign_entry(&mut entry, &key);
            entry.name = "Tampered".into();
            assert!(!verify_entry_signature(&entry));
        }
        {
            let (key, mut entry) = key_and_entry();
            sign_entry(&mut entry, &key);
            entry.publisher_pubkey_hex = "ee".repeat(32);
            assert!(!verify_entry_signature(&entry));
        }
    }

    #[test]
    fn change_request_verification_requires_an_allowlisted_untampered_signer() {
        {
            let (key, entry) = key_and_entry();
            let allow = [key.verifying_key().to_bytes()];
            let req = sign_change_request(EntryAction::Add, entry, &key);
            assert_eq!(verify_change_request(&req, &allow), Ok(()));
        }
        {
            let (key, entry) = key_and_entry();
            let allow = [[0u8; 32]]; // deliberately not the publisher's key
            let req = sign_change_request(EntryAction::Add, entry, &key);
            assert_eq!(
                verify_change_request(&req, &allow),
                Err(VerifyError::PublisherNotAllowed)
            );
        }
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
