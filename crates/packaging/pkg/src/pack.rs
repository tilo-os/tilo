//! `pack` — walk a payload directory, write each file as a
//! BLAKE3-keyed blob, build the [`Collection`] root, write the
//! manifest blob, build + sign the signed-root.
//!
//! See module docs in [`crate::signed`] for the on-wire layout
//! of the signed-root and what the publisher signs over.

use std::path::{Path, PathBuf};

use ed25519_dalek::Signer;
use rand::rngs::OsRng;
use walkdir::WalkDir;

use qos_collab_types::Ed25519PublicKey;
use qos_threshold_identity::ThresholdIdentity;

use crate::blobs::{BlobStore, Collection};
use crate::id::PackageId;
use crate::manifest::Manifest;
use crate::signed::SignedRoot;
use crate::validate::{ManifestError, validate_manifest_with_payload};

/// A signing identity that can sign the package's signed-root
/// bytes. Wraps either a plain `ed25519_dalek::SigningKey` or a
/// fully-initialised [`ThresholdIdentity`].
///
/// The threshold path is implemented for the 1-of-1 bootstrap
/// mode (where signing is byte-equivalent to plain Ed25519);
/// signing in a true `t-of-n` mode at pack-time is not supported
/// — that requires the multi-device round-1/round-2 ceremony,
/// which is the publisher pipeline's job, not the foundation
/// crate's. Use `from_threshold_aggregated` when you have an
/// already-aggregated signature from such a ceremony.
pub enum SigningIdentity {
    /// Plain Ed25519 — the simplest case. Most useful for tests,
    /// CI signing, and the `qos-pkg sign --identity` CLI path.
    Ed25519(ed25519_dalek::SigningKey),
    /// A 1-of-1 threshold identity. Goes through the same
    /// `sign_raw` path and produces a regular Ed25519 sig.
    ThresholdSingleDevice(ThresholdIdentity),
}

impl SigningIdentity {
    /// Generate a fresh random Ed25519 identity. Convenient for
    /// tests + `qos-pkg sign --identity <none>` ad-hoc signing.
    pub fn generate() -> Self {
        let mut rng = OsRng;
        Self::Ed25519(ed25519_dalek::SigningKey::generate(&mut rng))
    }

    /// Wrap an existing `SigningKey`.
    pub fn from_signing_key(sk: ed25519_dalek::SigningKey) -> Self {
        Self::Ed25519(sk)
    }

    /// Public-key bytes — used for the manifest's
    /// `publisher.identity` field and for the signed-root's
    /// `publisher_identity` slot.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        match self {
            Self::Ed25519(sk) => sk.verifying_key().to_bytes(),
            Self::ThresholdSingleDevice(ti) => *ti.group_public_key().as_bytes(),
        }
    }

    /// Return the public key in `qos_collab_types` newtype form.
    pub fn public_key(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.public_key_bytes())
    }

    /// Sign the signing-bytes prefix of a [`SignedRoot`]. Errors
    /// only in the (currently unsupported) true-threshold mode;
    /// the Ed25519 / 1-of-1 paths are infallible.
    pub fn sign_signed_root(&self, root: &SignedRoot) -> Result<[u8; 64], PackError> {
        let preimage = root.signing_bytes();
        match self {
            Self::Ed25519(sk) => {
                let sig = sk.sign(&preimage);
                Ok(sig.to_bytes())
            }
            Self::ThresholdSingleDevice(ti) => {
                if !ti.is_single_device() {
                    return Err(PackError::ThresholdSigningUnsupported);
                }
                let mut rng = OsRng;
                let sig = ti
                    .sign_raw(&preimage, &mut rng)
                    .map_err(|e| PackError::ThresholdInternal(e.to_string()))?;
                Ok(*sig.as_bytes())
            }
        }
    }
}

/// Errors produced by [`pack`].
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("manifest validation: {0}")]
    Manifest(#[from] ManifestError),
    #[error("payload-dir walk failed: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("blob-store error: {0}")]
    Store(#[from] crate::blobs::BlobStoreError),
    #[error("manifest re-encode failed: {0}")]
    ManifestEncode(#[from] crate::manifest::ManifestParseError),
    #[error("publisher identity in manifest does not match signing identity")]
    IdentityMismatch,
    #[error("true-threshold signing is not implemented at pack time; use the multi-device ceremony to produce an aggregated signature and pass it to `pack_with_signature`")]
    ThresholdSigningUnsupported,
    #[error("threshold-internal error: {0}")]
    ThresholdInternal(String),
    #[error("payload path {0:?} is not valid UTF-8 — qOS packages require UTF-8 path names")]
    NonUtf8Path(PathBuf),
}

/// Walk `payload_dir`, ingest every regular file as a blob, and
/// return both the [`Collection`] and the list of relative paths
/// (used downstream for payload-aware validation).
pub(crate) fn walk_payload<S: BlobStore>(
    payload_dir: &Path,
    store: &mut S,
) -> Result<(Collection, Vec<PathBuf>), PackError> {
    let mut entries = Vec::new();
    let mut paths = Vec::new();
    for entry in WalkDir::new(payload_dir).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = abs
            .strip_prefix(payload_dir)
            .expect("walkdir entry under payload_dir")
            .to_path_buf();
        let rel_str = rel
            .to_str()
            .ok_or_else(|| PackError::NonUtf8Path(rel.clone()))?
            .to_string();
        let bytes = std::fs::read(abs)?;
        let h = store.put(&bytes)?;
        entries.push((rel_str, h));
        paths.push(rel);
    }
    let collection = Collection::from_entries(entries);
    Ok((collection, paths))
}

/// Pack a payload directory + manifest into the given store.
/// Returns the [`PackageId`] of the resulting (signed) bundle.
///
/// The flow is:
/// 1. Validate the manifest in isolation, then with the payload
///    paths once they've been gathered.
/// 2. Walk `payload_dir`; ingest each file as a blob; build the
///    [`Collection`] root and store its encoding too.
/// 3. Re-encode the manifest with the publisher's identity hex
///    written in but the signature still empty; ingest the
///    manifest blob.
/// 4. Build the unsigned [`SignedRoot`], call
///    [`SigningIdentity::sign_signed_root`], reattach the
///    signature, ingest the signed-root blob, return its
///    `PackageId`.
pub fn pack<S: BlobStore>(
    payload_dir: &Path,
    manifest: &Manifest,
    identity: &SigningIdentity,
    store: &mut S,
) -> Result<PackageId, PackError> {
    // Lightweight check: manifest's declared publisher identity
    // must match the key we'll sign with.
    let declared = manifest
        .publisher
        .identity_bytes()
        .map_err(|e| PackError::Manifest(ManifestError::BadPublisherIdentity(e.to_string())))?;
    if declared != identity.public_key_bytes() {
        return Err(PackError::IdentityMismatch);
    }

    // Walk payload + ingest blobs.
    let (collection, payload_paths) = walk_payload(payload_dir, store)?;
    validate_manifest_with_payload(manifest, &payload_paths)?;

    // Ingest the collection itself.
    let collection_bytes = collection.encode();
    let payload_root_hash = store.put(&collection_bytes)?;

    // Re-encode the manifest with signature still empty; ingest.
    let mut to_sign_manifest = manifest.clone();
    to_sign_manifest.publisher.signature.clear();
    let manifest_toml = to_sign_manifest.to_toml_string()?;
    let manifest_hash = store.put(manifest_toml.as_bytes())?;

    // Build the about-to-be-signed root: has_signature byte
    // already set to 1 so the signing-bytes view matches what
    // `verify` will reconstruct after decode. A placeholder
    // zero-signature is overwritten with the real one below.
    let pre_signed = SignedRoot {
        manifest_hash,
        payload_root_hash,
        publisher_identity: identity.public_key_bytes(),
        signature: Some([0u8; 64]),
    };
    let sig = identity.sign_signed_root(&pre_signed)?;
    let signed = SignedRoot {
        signature: Some(sig),
        ..pre_signed
    };
    let signed_bytes = signed.encode();
    store.put(&signed_bytes)?;
    Ok(signed.package_id())
}

/// Pack with a pre-computed aggregated threshold signature.
/// Used by the multi-device publisher pipeline where the
/// round-2 partials are collected and aggregated *outside*
/// this crate.
pub fn pack_with_signature<S: BlobStore>(
    payload_dir: &Path,
    manifest: &Manifest,
    publisher_identity: [u8; 32],
    signature: [u8; 64],
    store: &mut S,
) -> Result<PackageId, PackError> {
    let declared = manifest
        .publisher
        .identity_bytes()
        .map_err(|e| PackError::Manifest(ManifestError::BadPublisherIdentity(e.to_string())))?;
    if declared != publisher_identity {
        return Err(PackError::IdentityMismatch);
    }
    let (collection, payload_paths) = walk_payload(payload_dir, store)?;
    validate_manifest_with_payload(manifest, &payload_paths)?;
    let payload_root_hash = store.put(&collection.encode())?;
    let mut to_sign_manifest = manifest.clone();
    to_sign_manifest.publisher.signature.clear();
    let manifest_toml = to_sign_manifest.to_toml_string()?;
    let manifest_hash = store.put(manifest_toml.as_bytes())?;
    let signed = SignedRoot {
        manifest_hash,
        payload_root_hash,
        publisher_identity,
        signature: Some(signature),
    };
    store.put(&signed.encode())?;
    Ok(signed.package_id())
}
