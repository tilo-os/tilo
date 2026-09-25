//! `verify` — given a [`PackageId`] and the blob store holding the
//! package, checks every cryptographic invariant:
//!
//! 1. The signed-root blob is present and its hash matches the `PackageId`.
//! 2. The signed-root carries a signature (not unsigned).
//! 3. The signature verifies against the embedded publisher identity
//!    over the signing-bytes prefix.
//! 4. The publisher identity is not in the caller-supplied revocation list.
//! 5. The manifest blob is present and re-hashes to the value embedded
//!    in the signed-root (catches manifest tampering).
//! 6. The payload-collection blob is present and re-hashes to the
//!    value embedded in the signed-root.
//! 7. Every file blob the collection references is present and
//!    re-hashes to its declared per-file hash (catches payload tampering).
//! 8. If a `[[trust.attested_by]]` entry is signed by a trusted
//!    attestor whose scope covers the package, the bundle is
//!    [`Verification::AttestedBy`]; otherwise the embedded publisher
//!    identity is checked against `trusted_keys`.
//!
//! Any failure short-circuits — verify reports the *first* invariant
//! that broke, with enough context to fix it.

use ed25519_dalek::{Signature as DalekSig, Verifier, VerifyingKey};

use crate::blobs::{BlobHash, BlobStore};
use crate::id::PackageId;
use crate::manifest::Manifest;
use crate::signed::{SignedRoot, SignedRootError};
use crate::trust::{RevocationList, TrustedAttestor, attestation_preimage};

/// The outcome of [`verify`]. `Trusted`/`AttestedBy` mean every check
/// passed; every other variant names the first invariant that broke.
#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    /// All checks passed and the publisher's identity is in the trusted-keys list.
    Trusted { manifest: Manifest },
    /// All checks passed AND a trusted attestor signed the bundle — the
    /// strongest trust outcome. `attestor` is the id from the attestor registry that verified.
    AttestedBy {
        attestor: String,
        manifest: Manifest,
    },
    /// The publisher's pubkey is in the caller-supplied revocation list —
    /// refuse to install/launch. The manifest may still parse and the
    /// signature may still verify; revocation is a policy override, not a crypto failure.
    Revoked {
        pubkey: [u8; 32],
        reason: String,
        manifest: Manifest,
    },
    /// All cryptographic checks passed but the publisher's identity is
    /// not in the trusted-keys list and no trusted attestor's signature
    /// was found. The bundle is authentic; the trust policy is on the consumer.
    UntrustedPublisher {
        identity: [u8; 32],
        manifest: Manifest,
    },
    /// The signed-root blob is not in the store.
    MissingRoot,
    /// The signed-root decodes but `package_id != id`.
    IdMismatch {
        expected: PackageId,
        found: PackageId,
    },
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

/// Backwards-compatible verify: no attestor registry, no revocation
/// list; see [`verify_with_trust`] for the full API.
pub fn verify<S: BlobStore>(id: PackageId, store: &S, trusted_keys: &[[u8; 32]]) -> Verification {
    verify_with_trust(id, store, trusted_keys, &[], &RevocationList::empty())
}

/// Verify a package with a trusted-attestor registry AND a revocation list.
///
/// Trust decision order (assuming all crypto invariants pass):
/// 1. Publisher pubkey in `revoked` → [`Verification::Revoked`].
/// 2. Walk `manifest.trust.attested_by`: for each row with a
///    `signature`, look up the attestor by id in `trusted_attestors`;
///    if found, its signature verifies over the canonical preimage,
///    and its scope covers this package → [`Verification::AttestedBy`].
/// 3. Otherwise, publisher pubkey in `trusted_keys` → [`Verification::Trusted`].
/// 4. Otherwise → [`Verification::UntrustedPublisher`].
pub fn verify_with_trust<S: BlobStore>(
    id: PackageId,
    store: &S,
    trusted_keys: &[[u8; 32]],
    trusted_attestors: &[TrustedAttestor],
    revoked: &RevocationList,
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
    let Ok(vk) = VerifyingKey::from_bytes(&signed.publisher_identity) else {
        return Verification::InvalidPublisherKey;
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
    let Ok(manifest_str) = std::str::from_utf8(&manifest_bytes) else {
        return Verification::ManifestParseFailed("non-UTF-8 manifest".into());
    };
    let manifest = match Manifest::from_toml_str(manifest_str) {
        Ok(m) => m,
        Err(e) => return Verification::ManifestParseFailed(e.to_string()),
    };

    // Revocation check — refuse before honouring attestor / trusted-key.
    if let Some(entry) = revoked.find(&signed.publisher_identity) {
        return Verification::Revoked {
            pubkey: signed.publisher_identity,
            reason: entry.reason.clone(),
            manifest,
        };
    }

    // Attestor chain — walk the [[trust.attested_by]] rows for a trusted attestor's signature.
    if let Some(attestor_id) = find_matching_attestor(
        &manifest,
        &signed.payload_root_hash,
        &signed.publisher_identity,
        trusted_attestors,
    ) {
        return Verification::AttestedBy {
            attestor: attestor_id,
            manifest,
        };
    }

    if !trusted_keys.iter().any(|k| k == &signed.publisher_identity) {
        return Verification::UntrustedPublisher {
            identity: signed.publisher_identity,
            manifest,
        };
    }
    Verification::Trusted { manifest }
}

/// Walk the manifest's [[`trust.attested_by`]] rows against the
/// trusted-attestor registry; returns the id of the first row whose
/// signature verifies against a trusted attestor's pubkey with matching scope.
fn find_matching_attestor(
    manifest: &Manifest,
    payload_root_hash: &BlobHash,
    publisher_pubkey: &[u8; 32],
    trusted_attestors: &[TrustedAttestor],
) -> Option<String> {
    let preimage = attestation_preimage(payload_root_hash, publisher_pubkey);
    for row in &manifest.trust.attested_by {
        let Ok(Some(sig_bytes)) = row.signature_bytes() else {
            continue; // legacy claim-only row, no signature to verify
        };
        let Some(attestor) = trusted_attestors.iter().find(|a| a.id == row.attestor) else {
            continue;
        };
        // Scope check: package id + declared mime types.
        let mime = &manifest.providers.mime;
        if !attestor.scope.covers(&manifest.package.id, mime) {
            continue;
        }
        let Ok(pubkey_bytes) = attestor.pubkey_bytes() else {
            continue;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&pubkey_bytes) else {
            continue;
        };
        let sig = DalekSig::from_bytes(&sig_bytes);
        if vk.verify(&preimage, &sig).is_ok() {
            return Some(attestor.id.clone());
        }
    }
    None
}
