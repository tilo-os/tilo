//! `verify` — decide whether a package's signature is valid and whom it is
//! trusted through.
//!
//! Structure is checked before this runs: a [`PackageMeta`] only exists
//! when the signed root hashes to the package id and commits to the exact
//! manifest and payload collection (see [`crate::package`]), and payload
//! bytes are checked against the collection by whoever reads them (the
//! `.tapp` decoder, [`crate::unpack()`], an installer). What remains is the
//! signature and the trust policy:
//!
//! 1. The signed root carries a signature (not unsigned).
//! 2. The signature verifies against the embedded publisher identity over
//!    the signing-bytes prefix.
//! 3. The publisher identity is not in the caller-supplied revocation list.
//! 4. If a `[[trust.attested_by]]` entry is signed by a trusted attestor
//!    whose scope covers the package, the package is
//!    [`Verification::AttestedBy`]; otherwise the publisher identity is
//!    checked against `trusted_keys`.

use ed25519_dalek::{Signature as DalekSig, Verifier, VerifyingKey};

use crate::blobs::BlobHash;
use crate::manifest::Manifest;
use crate::package::PackageMeta;
use crate::trust::{RevocationList, TrustedAttestor, attestation_preimage};

/// The outcome of [`verify`]. `Trusted`/`AttestedBy` mean every check
/// passed; every other variant names the first invariant that broke.
#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    /// All checks passed and the publisher's identity is in the trusted-keys list.
    Trusted { manifest: Manifest },
    /// All checks passed AND a trusted attestor signed the package — the
    /// strongest trust outcome. `attestor` is the id from the attestor registry that verified.
    AttestedBy {
        attestor: String,
        manifest: Manifest,
    },
    /// The publisher's pubkey is in the caller-supplied revocation list —
    /// refuse to install/launch. The signature may still verify; revocation
    /// is a policy override, not a crypto failure.
    Revoked {
        pubkey: [u8; 32],
        reason: String,
        manifest: Manifest,
    },
    /// The signature is valid but the publisher's identity is not in the
    /// trusted-keys list and no trusted attestor's signature was found. The
    /// package is authentic; the trust policy is on the consumer.
    UntrustedPublisher {
        identity: [u8; 32],
        manifest: Manifest,
    },
    /// The signed-root has no signature attached.
    NotSigned,
    /// The signature doesn't validate against the embedded key.
    SignatureMismatch,
    /// The embedded publisher key isn't a valid Ed25519 point.
    InvalidPublisherKey,
}

impl Verification {
    /// The manifest, when the signature itself is valid.
    pub fn manifest(&self) -> Option<&Manifest> {
        match self {
            Self::Trusted { manifest }
            | Self::AttestedBy { manifest, .. }
            | Self::Revoked { manifest, .. }
            | Self::UntrustedPublisher { manifest, .. } => Some(manifest),
            Self::NotSigned | Self::SignatureMismatch | Self::InvalidPublisherKey => None,
        }
    }
}

/// Verify with no attestor registry and no revocation list; see
/// [`verify_with_trust`] for the full API.
pub fn verify(meta: &PackageMeta, trusted_keys: &[[u8; 32]]) -> Verification {
    verify_with_trust(meta, trusted_keys, &[], &RevocationList::default())
}

/// Verify a package with a trusted-attestor registry AND a revocation list.
///
/// Trust decision order (assuming the signature is valid):
/// 1. Publisher pubkey in `revoked` → [`Verification::Revoked`].
/// 2. Walk `manifest.trust.attested_by`: for each row with a
///    `signature`, look up the attestor by id in `trusted_attestors`;
///    if found, its signature verifies over the canonical preimage,
///    and its scope covers this package → [`Verification::AttestedBy`].
/// 3. Otherwise, publisher pubkey in `trusted_keys` → [`Verification::Trusted`].
/// 4. Otherwise → [`Verification::UntrustedPublisher`].
pub fn verify_with_trust(
    meta: &PackageMeta,
    trusted_keys: &[[u8; 32]],
    trusted_attestors: &[TrustedAttestor],
    revoked: &RevocationList,
) -> Verification {
    let signed = &meta.root;
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
    let manifest = meta.manifest.clone();

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
