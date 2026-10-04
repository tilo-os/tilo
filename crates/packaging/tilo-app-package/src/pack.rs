//! `pack` — walk a payload directory, hash each file into the
//! [`Collection`], encode the manifest, and build + sign the signed-root.
//! See [`crate::signed`] for the on-wire layout of the signed-root and what
//! the publisher signs over. The payload bytes stay where they are; the
//! result names them so a writer can stream them out later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ed25519_dalek::Signer;
use getrandom::{SysRng, rand_core::UnwrapErr};
use walkdir::WalkDir;

use collab_types::Ed25519PublicKey;
use threshold_identity::ThresholdIdentity;

use crate::blobs::{BlobHash, BlobSource, Collection, OpenBlob};
use crate::id::PackageId;
use crate::manifest::Manifest;
use crate::package::{MetaError, PackageMeta};
use crate::signed::SignedRoot;
use crate::validate::{ManifestError, validate_manifest_with_payload};

/// A signing identity that can sign the package's signed-root bytes.
/// Wraps either a plain `ed25519_dalek::SigningKey` or a fully
/// initialised [`ThresholdIdentity`].
///
/// The threshold path only supports 1-of-1 bootstrap mode (byte-
/// equivalent to plain Ed25519); true `t-of-n` signing at pack-time
/// needs the multi-device ceremony, which belongs to the publisher
/// pipeline, not this crate — use `from_threshold_aggregated` for an
/// already-aggregated signature from such a ceremony.
#[allow(
    clippy::large_enum_variant,
    reason = "ThresholdIdentity is already boxed (see its own doc below); the remaining gap to \
              Ed25519's plain SigningKey is inherent to ed25519_dalek's expanded-key caching, \
              not something a second box would meaningfully shrink for a type constructed once \
              per pack/sign call, never per-item in a hot loop"
)]
pub enum SigningIdentity {
    /// Plain Ed25519 — simplest case; used by tests, CI signing, and `tilo-app-package sign --identity`.
    Ed25519(ed25519_dalek::SigningKey),
    /// A 1-of-1 threshold identity; goes through the same `sign_raw` path and produces a regular Ed25519 sig.
    /// Boxed: `ThresholdIdentity` carries frost-core key-package material far
    /// larger than the 32-byte `SigningKey` in the other variant.
    ThresholdSingleDevice(Box<ThresholdIdentity>),
}

impl SigningIdentity {
    /// Generate a fresh random Ed25519 identity, for tests + ad-hoc `tilo-app-package sign` signing.
    pub fn generate() -> Self {
        let mut rng = UnwrapErr(SysRng);
        Self::Ed25519(ed25519_dalek::SigningKey::generate(&mut rng))
    }

    /// Wrap an existing `SigningKey`.
    pub fn from_signing_key(sk: ed25519_dalek::SigningKey) -> Self {
        Self::Ed25519(sk)
    }

    /// The Ed25519 identity for a 32-byte secret-key seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self::Ed25519(ed25519_dalek::SigningKey::from_bytes(seed))
    }

    /// Public-key bytes — used for the manifest's `publisher.identity`
    /// field and the signed-root's `publisher_identity` slot.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        match self {
            Self::Ed25519(sk) => sk.verifying_key().to_bytes(),
            Self::ThresholdSingleDevice(ti) => *ti.group_public_key().as_bytes(),
        }
    }

    /// Return the public key in `collab_types` newtype form.
    pub fn public_key(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.public_key_bytes())
    }

    /// Sign the signing-bytes prefix of a [`SignedRoot`]; errors only in
    /// the (unsupported) true-threshold mode — Ed25519/1-of-1 paths are infallible.
    ///
    /// # Errors
    ///
    /// Returns [`PackError::ThresholdSigningUnsupported`] if this identity is
    /// a true `t`-of-`n` threshold identity (`t >= 2`); single-device and
    /// plain Ed25519 identities never fail.
    pub fn sign_signed_root(&self, root: &SignedRoot) -> Result<[u8; 64], PackError> {
        let preimage = root.signing_bytes();
        self.sign_message(&preimage)
    }

    /// Sign an arbitrary message with the identity's Ed25519 key.
    ///
    /// Used for out-of-band attestations binding to the same publisher
    /// identity as the package — e.g. a registry index entry vouching
    /// "this publisher's build of app X at version V has tapp
    /// content-hash H". Verification is a plain Ed25519 check against
    /// [`Self::public_key_bytes`], so consumers need only the verifying
    /// key, not threshold machinery.
    ///
    /// Errors only in the (unsupported) true-threshold mode; Ed25519/1-of-1 paths are infallible.
    ///
    /// # Errors
    ///
    /// Returns [`PackError::ThresholdSigningUnsupported`] if this identity is
    /// a true `t`-of-`n` threshold identity, or [`PackError::ThresholdInternal`]
    /// if the underlying single-device threshold signing call fails.
    pub fn sign_message(&self, msg: &[u8]) -> Result<[u8; 64], PackError> {
        match self {
            Self::Ed25519(sk) => Ok(sk.sign(msg).to_bytes()),
            Self::ThresholdSingleDevice(ti) => {
                if !ti.is_single_device() {
                    return Err(PackError::ThresholdSigningUnsupported);
                }
                let mut rng = UnwrapErr(SysRng);
                let sig = ti
                    .sign_raw(msg, &mut rng)
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
    #[error("packed metadata is inconsistent: {0}")]
    Meta(#[from] MetaError),
    #[error("manifest re-encode failed: {0}")]
    ManifestEncode(#[from] crate::manifest::ManifestParseError),
    #[error("publisher identity in manifest does not match signing identity")]
    IdentityMismatch,
    #[error(
        "true-threshold signing is not implemented at pack time; use the multi-device ceremony to produce an aggregated signature and pass it to `pack_with_signature`"
    )]
    ThresholdSigningUnsupported,
    #[error("threshold-internal error: {0}")]
    ThresholdInternal(String),
    #[error("payload path {0:?} is not valid UTF-8 — Tilo packages require UTF-8 path names")]
    NonUtf8Path(PathBuf),
    #[error("payload path {0:?} is reserved for a file the packer writes")]
    ReservedPath(String),
}

/// A signed package built from a payload directory: its metadata, plus
/// where each payload blob's bytes are on disk. Nothing is read into
/// memory; files are hashed as they stream past and read again only when
/// the package is written out ([`crate::tapp::write`]) or installed.
#[derive(Clone, Debug)]
pub struct Packed {
    pub meta: PackageMeta,
    files: BTreeMap<BlobHash, PathBuf>,
    /// Payload files the packer generated rather than found on disk.
    inline: BTreeMap<BlobHash, Vec<u8>>,
}

impl Packed {
    pub fn id(&self) -> PackageId {
        self.meta.id
    }

    /// Payload files in `(hash, path)` order, one per distinct blob found
    /// on disk. Files the packer generated (an image's
    /// [`crate::image::TREE_FILE`]) are read through [`BlobSource`].
    pub fn files(&self) -> impl Iterator<Item = (&BlobHash, &Path)> {
        self.files.iter().map(|(hash, path)| (hash, path.as_path()))
    }
}

impl BlobSource for Packed {
    fn open_blob(&self, hash: &BlobHash) -> std::io::Result<Option<OpenBlob<'_>>> {
        if let Some(bytes) = self.inline.get(hash) {
            return Ok(Some(OpenBlob {
                len: bytes.len() as u64,
                reader: Box::new(bytes.as_slice()),
            }));
        }
        let Some(path) = self.files.get(hash) else {
            return Ok(None);
        };
        let file = std::fs::File::open(path)?;
        Ok(Some(OpenBlob {
            len: file.metadata()?.len(),
            reader: Box::new(file),
        }))
    }
}

/// A walked payload directory.
struct Walked {
    collection: Collection,
    /// The source path of each distinct blob.
    files: BTreeMap<BlobHash, PathBuf>,
    /// Generated files, by hash.
    inline: BTreeMap<BlobHash, Vec<u8>>,
    /// Every payload path, for payload-aware manifest validation.
    paths: Vec<PathBuf>,
}

/// Walk `payload_dir` and hash every regular file, then add the generated
/// `extra` files, which must not collide with a walked path.
fn walk_payload(payload_dir: &Path, extra: Vec<(String, Vec<u8>)>) -> Result<Walked, PackError> {
    let mut entries = Vec::new();
    let mut files = BTreeMap::new();
    let mut paths = Vec::new();
    for entry in WalkDir::new(payload_dir).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = abs
            .strip_prefix(payload_dir)
            .map_err(|_| {
                PackError::Io(std::io::Error::other(format!(
                    "walkdir entry {} is not under payload_dir {}",
                    abs.display(),
                    payload_dir.display()
                )))
            })?
            .to_path_buf();
        let rel_str = rel
            .to_str()
            .ok_or_else(|| PackError::NonUtf8Path(rel.clone()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let (hash, _len) = BlobHash::of_reader(&mut std::fs::File::open(abs)?)?;
        files.entry(hash).or_insert_with(|| abs.to_path_buf());
        entries.push((rel_str, hash));
        paths.push(rel);
    }
    let mut inline = BTreeMap::new();
    for (rel, bytes) in extra {
        if entries.iter().any(|(walked, _)| *walked == rel) {
            return Err(PackError::ReservedPath(rel));
        }
        let hash = BlobHash::of(&bytes);
        paths.push(PathBuf::from(&rel));
        entries.push((rel, hash));
        inline.insert(hash, bytes);
    }
    Ok(Walked {
        collection: Collection::from_entries(entries),
        files,
        inline,
        paths,
    })
}

/// Everything but the signature: the payload index and the manifest blob a
/// root is built from.
struct Prepared {
    /// The root to sign, with a placeholder signature.
    root: SignedRoot,
    manifest_bytes: Vec<u8>,
    collection_bytes: Vec<u8>,
    files: BTreeMap<BlobHash, PathBuf>,
    inline: BTreeMap<BlobHash, Vec<u8>>,
}

impl Prepared {
    fn finish(self, signature: [u8; 64]) -> Result<Packed, PackError> {
        let root = SignedRoot {
            signature: Some(signature),
            ..self.root
        };
        let meta = PackageMeta::from_root(root, self.manifest_bytes, self.collection_bytes)?;
        Ok(Packed {
            meta,
            files: self.files,
            inline: self.inline,
        })
    }
}

fn prepare(
    payload_dir: &Path,
    manifest: &Manifest,
    publisher_identity: [u8; 32],
    extra: Vec<(String, Vec<u8>)>,
) -> Result<Prepared, PackError> {
    let declared = manifest
        .publisher
        .identity_bytes()
        .map_err(|e| PackError::Manifest(ManifestError::BadPublisherIdentity(e.to_string())))?;
    if declared != publisher_identity {
        return Err(PackError::IdentityMismatch);
    }
    let walked = walk_payload(payload_dir, extra)?;
    validate_manifest_with_payload(manifest, &walked.paths)?;
    let collection_bytes = walked.collection.encode();
    let mut to_sign_manifest = manifest.clone();
    to_sign_manifest.publisher.signature.clear();
    let manifest_bytes = to_sign_manifest.to_toml_string()?.into_bytes();
    let root = SignedRoot {
        manifest_hash: BlobHash::of(&manifest_bytes),
        payload_root_hash: BlobHash::of(&collection_bytes),
        publisher_identity,
        // The signed prefix includes `has_signature = 1`, so the root is
        // signed with a placeholder in place; see `crate::signed`.
        signature: Some([0u8; 64]),
    };
    Ok(Prepared {
        root,
        manifest_bytes,
        collection_bytes,
        files: walked.files,
        inline: walked.inline,
    })
}

/// Pack a payload directory and manifest into a signed package.
///
/// 1. Check the manifest's publisher is `identity`, validate the manifest
///    alone and against the payload paths.
/// 2. Hash every payload file into the [`Collection`].
/// 3. Encode the manifest with an empty signature field.
/// 4. Build the [`SignedRoot`] over both hashes and sign it.
///
/// # Errors
///
/// Returns [`PackError::IdentityMismatch`] if the manifest's declared
/// publisher identity does not match `identity`'s public key,
/// [`PackError::Manifest`] if manifest validation (standalone or against the
/// payload) fails, [`PackError::Walk`] or [`PackError::Io`] if walking or
/// reading `payload_dir` fails, [`PackError::ManifestEncode`] if re-encoding
/// the manifest fails, or whatever [`SigningIdentity::sign_signed_root`]
/// returns.
pub fn pack(
    payload_dir: &Path,
    manifest: &Manifest,
    identity: &SigningIdentity,
) -> Result<Packed, PackError> {
    pack_with_files(payload_dir, manifest, identity, Vec::new())
}

/// [`pack`], with `extra` generated files added to the payload beside the
/// walked ones.
pub(crate) fn pack_with_files(
    payload_dir: &Path,
    manifest: &Manifest,
    identity: &SigningIdentity,
    extra: Vec<(String, Vec<u8>)>,
) -> Result<Packed, PackError> {
    let prepared = prepare(payload_dir, manifest, identity.public_key_bytes(), extra)?;
    let signature = identity.sign_signed_root(&prepared.root)?;
    prepared.finish(signature)
}

/// [`pack_with_files`] for `publisher`, whose key is not here: the root
/// carries the placeholder signature it is signed with
/// ([`sign_placeholder_root`] attaches the real one).
pub(crate) fn pack_with_files_for(
    payload_dir: &Path,
    manifest: &Manifest,
    publisher: [u8; 32],
    extra: Vec<(String, Vec<u8>)>,
) -> Result<Packed, PackError> {
    prepare(payload_dir, manifest, publisher, extra)?.finish([0u8; 64])
}

/// Sign a package packed for `identity` by someone without its key
/// ([`crate::image::seal_image_for`]): the root's placeholder signature is
/// replaced by `identity`'s, which changes the package id.
///
/// # Errors
///
/// Returns [`PackError::IdentityMismatch`] when the package names another
/// publisher, or what signing and reassembling the package return.
pub fn sign_placeholder_root(
    meta: &PackageMeta,
    identity: &SigningIdentity,
) -> Result<PackageMeta, PackError> {
    if meta.root.publisher_identity != identity.public_key_bytes() {
        return Err(PackError::IdentityMismatch);
    }
    let placeholder = SignedRoot {
        signature: Some([0u8; 64]),
        ..meta.root.clone()
    };
    let signature = identity.sign_signed_root(&placeholder)?;
    Ok(PackageMeta::from_root(
        SignedRoot {
            signature: Some(signature),
            ..placeholder
        },
        meta.manifest_bytes.clone(),
        meta.collection_bytes.clone(),
    )?)
}

/// Pack with a pre-computed aggregated threshold signature — used by
/// the multi-device publisher pipeline where round-2 partials are
/// collected and aggregated *outside* this crate.
///
/// # Errors
///
/// Returns [`PackError::IdentityMismatch`] if the manifest's declared
/// publisher identity does not match `publisher_identity`, or any of the
/// walk/validate/encode errors [`pack`] can return — this variant skips only
/// the signing step, substituting the pre-computed `signature`.
pub fn pack_with_signature(
    payload_dir: &Path,
    manifest: &Manifest,
    publisher_identity: [u8; 32],
    signature: [u8; 64],
) -> Result<Packed, PackError> {
    prepare(payload_dir, manifest, publisher_identity, Vec::new())?.finish(signature)
}
