//! `verify` — decide whether a package's signature is valid and whom it is
//! trusted through.

use ed25519_dalek::{Signature as DalekSig, Verifier, VerifyingKey};

use crate::blobs::BlobHash;
use crate::manifest::Manifest;
use crate::package::PackageMeta;
use crate::trust::{RevocationList, TrustedAttestor, attestation_preimage};

#[derive(Debug, PartialEq, Eq)]
pub enum Verification {
    Trusted {
        manifest: Manifest,
    },
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

pub fn verify(meta: &PackageMeta, trusted_keys: &[[u8; 32]]) -> Verification {
    verify_with_trust(meta, trusted_keys, &[], &RevocationList::default())
}

/// Verify a package with a trusted-attestor registry AND a revocation list.
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
