//! `verify` — given a [`PackageId`] and the blob store that
//! holds the package, check every cryptographic invariant:
//!
//! 1. The signed-root blob is present and its hash matches the
//!    PackageId.
//! 2. The signed-root carries a signature (i.e. not unsigned).
//! 3. The signature verifies against the embedded publisher
//!    identity over the signing-bytes prefix.
//! 4. The embedded publisher identity is in the caller-supplied
//!    `trusted_keys` list.
//! 5. The manifest blob is present and re-hashes to the value
//!    embedded in the signed-root (catches manifest tampering).
//! 6. The payload-collection blob is present and re-hashes to
//!    the value embedded in the signed-root.
//! 7. Every file blob referenced by the collection is present
//!    and re-hashes to the declared per-file hash (catches
//!    payload tampering).
//!
//! Any failure short-circuits — verify reports the *first*
//! invariant that broke, with enough context to fix it.

use ed25519_dalek::{Signature as DalekSig, Verifier, VerifyingKey};

use crate::blobs::{BlobHash, BlobStore};
use crate::id::PackageId;
use crate::manifest::Manifest;
use crate::signed::{SignedRoot, SignedRootError};

/// The outcome of [`verify`]. `Trusted` means every check
/// passed; every other variant names the first invariant that
/// broke.
#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    /// All checks passed and the publisher's identity is in the
    /// trusted-keys list.
    Trusted { manifest: Manifest },
    /// All cryptographic checks passed but the publisher's
    /// identity is not in the trusted-keys list. The bundle is
    /// authentic; the trust-policy is on the consumer.
    UntrustedPublisher {
        identity: [u8; 32],
        manifest: Manifest,
    },
    /// The signed-root blob is not in the store.
    MissingRoot,
    /// The signed-root decodes but `package_id != id`.
    IdMismatch { expected: PackageId, found: PackageId },
    /// The signed-root has no signature attached.
    NotSigned,
    /// The signature doesn't validate against the embedded key.
    SignatureMismatch,
    /// The embedded publisher key isn't a valid Ed25519 point.
    InvalidPublisherKey,
    /// The signed-root decoder failed.
    BadSignedRoot(SignedRootError),
    /// The manifest blob is missing or has the wrong hash.
    ManifestHashMismatch,
    /// The manifest blob is missing.
    MissingManifestBlob,
    /// The manifest is present but does not parse as TOML.
    ManifestParseFailed(String),
    /// The payload-collection blob is missing.
    MissingCollectionBlob,
    /// The payload-collection blob hash doesn't match.
    CollectionHashMismatch,
    /// The payload-collection is present but malformed.
    CollectionDecodeFailed(crate::blobs::CollectionDecodeError),
    /// A per-file blob is missing or has the wrong hash.
    PayloadBlobHashMismatch { path: String },
    /// A per-file blob is missing.
    MissingPayloadBlob { path: String },
    /// Some other blob-store error surfaced while reading.
    StoreError(String),
}

/// Verify a package against the caller-supplied trusted keys.
/// Trusted keys are Ed25519 32-byte values (use `VerifyingKey`
/// at the consumer side); we accept the raw bytes here so this
/// crate stays free of `ed25519_dalek` newtype churn for
/// callers.
pub fn verify<S: BlobStore>(
    id: PackageId,
    store: &S,
    trusted_keys: &[[u8; 32]],
) -> Verification {
    let signed_bytes = match store.get(&BlobHash(*id.as_bytes())) {
        Ok(Some(b)) => b,
        Ok(None) => return Verification::MissingRoot,
        Err(e) => return Verification::StoreError(e.to_string()),
    };
    let signed = match SignedRoot::decode(&signed_bytes) {
        Ok(s) => s,
        Err(e) => return Verification::BadSignedRoot(e),
    };
    // Re-derive the PackageId and check it matches the caller's.
    let computed = signed.package_id();
    if computed != id {
        return Verification::IdMismatch {
            expected: id,
            found: computed,
        };
    }
    let Some(sig_bytes) = signed.signature else {
        return Verification::NotSigned;
    };
    let vk = match VerifyingKey::from_bytes(&signed.publisher_identity) {
        Ok(vk) => vk,
        Err(_) => return Verification::InvalidPublisherKey,
    };
    let signature = DalekSig::from_bytes(&sig_bytes);
    if vk.verify(&signed.signing_bytes(), &signature).is_err() {
        return Verification::SignatureMismatch;
    }
    // Manifest blob hash check.
    let manifest_bytes = match store.get(&signed.manifest_hash) {
        Ok(Some(b)) => b,
        Ok(None) => return Verification::MissingManifestBlob,
        Err(e) => return Verification::StoreError(e.to_string()),
    };
    if BlobHash::of(&manifest_bytes) != signed.manifest_hash {
        return Verification::ManifestHashMismatch;
    }
    // Collection blob hash check.
    let collection_bytes = match store.get(&signed.payload_root_hash) {
        Ok(Some(b)) => b,
        Ok(None) => return Verification::MissingCollectionBlob,
        Err(e) => return Verification::StoreError(e.to_string()),
    };
    if BlobHash::of(&collection_bytes) != signed.payload_root_hash {
        return Verification::CollectionHashMismatch;
    }
    let collection = match crate::blobs::Collection::decode(&collection_bytes) {
        Ok(c) => c,
        Err(e) => return Verification::CollectionDecodeFailed(e),
    };
    // Per-file blob hash checks.
    for (path, declared) in &collection.entries {
        match store.get(declared) {
            Ok(Some(bytes)) => {
                if BlobHash::of(&bytes) != *declared {
                    return Verification::PayloadBlobHashMismatch { path: path.clone() };
                }
            }
            Ok(None) => return Verification::MissingPayloadBlob { path: path.clone() },
            Err(e) => return Verification::StoreError(e.to_string()),
        }
    }
    // Decode the manifest.
    let manifest_str = match std::str::from_utf8(&manifest_bytes) {
        Ok(s) => s,
        Err(_) => return Verification::ManifestParseFailed("non-UTF-8 manifest".into()),
    };
    let manifest = match Manifest::from_toml_str(manifest_str) {
        Ok(m) => m,
        Err(e) => return Verification::ManifestParseFailed(e.to_string()),
    };
    if !trusted_keys
        .iter()
        .any(|k| k == &signed.publisher_identity)
    {
        return Verification::UntrustedPublisher {
            identity: signed.publisher_identity,
            manifest,
        };
    }
    Verification::Trusted { manifest }
}
