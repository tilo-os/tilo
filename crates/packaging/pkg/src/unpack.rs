//! `unpack` — given a [`PackageId`] and a [`BlobStore`] that
//! holds the signed-root, manifest, collection, and per-file
//! blobs, write the payload to `dest_dir` and return the
//! decoded [`Manifest`].
//!
//! This is the reverse of [`crate::pack::pack`]. It does **not**
//! verify the signature — see [`crate::verify`] for that.
//! Unpack succeeds on an unsigned bundle so build tooling can
//! round-trip without keys.

use std::path::Path;

use crate::blobs::{BlobStore, Collection, CollectionDecodeError};
use crate::id::PackageId;
use crate::manifest::Manifest;
use crate::signed::{SignedRoot, SignedRootError};

#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    #[error("package {0} not present in store")]
    MissingRoot(PackageId),
    #[error("signed-root decode: {0}")]
    SignedRoot(#[from] SignedRootError),
    #[error("manifest blob missing")]
    MissingManifest,
    #[error("payload-collection blob missing")]
    MissingCollection,
    #[error("payload file {0} (declared at {1:?}) missing")]
    MissingPayloadBlob(String, String),
    #[error("manifest parse: {0}")]
    Manifest(#[from] crate::manifest::ManifestParseError),
    #[error("collection decode: {0}")]
    Collection(#[from] CollectionDecodeError),
    #[error("blob store: {0}")]
    Store(#[from] crate::blobs::BlobStoreError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("payload path {0:?} escapes destination dir")]
    PathTraversal(String),
    #[error("application payload has {found} entries; limit is {limit}")]
    TooManyPayloadEntries { found: usize, limit: usize },
    #[error("application payload materializes to {found} bytes; limit is {limit}")]
    PayloadTooLarge { found: u64, limit: u64 },
}

/// Validate the filesystem work represented by a decoded payload collection.
///
/// This is separate from [`unpack`] so registries can reject amplification
/// before publishing or approving a bundle, without creating a temporary
/// destination directory.
///
/// # Errors
///
/// Returns [`UnpackError::TooManyPayloadEntries`] or
/// [`UnpackError::PayloadTooLarge`] when an application exceeds its
/// materialization budget, [`UnpackError::MissingPayloadBlob`] when a
/// collection entry has no stored bytes, or [`UnpackError::Store`] when the
/// backing store cannot be read.
pub fn validate_materialized_payload<S: BlobStore>(
    manifest: &Manifest,
    collection: &Collection,
    store: &S,
) -> Result<(), UnpackError> {
    if manifest.package.kind != crate::id::Kind::App {
        return Ok(());
    }
    if collection.entries.len() > crate::MAX_APP_PAYLOAD_ENTRIES {
        return Err(UnpackError::TooManyPayloadEntries {
            found: collection.entries.len(),
            limit: crate::MAX_APP_PAYLOAD_ENTRIES,
        });
    }
    let mut materialized = 0u64;
    for (rel_path, hash) in &collection.entries {
        let bytes = store
            .get(hash)?
            .ok_or_else(|| UnpackError::MissingPayloadBlob(hash.to_hex(), rel_path.clone()))?;
        materialized = materialized.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if materialized > crate::MAX_APP_UNPACKED_BYTES {
            return Err(UnpackError::PayloadTooLarge {
                found: materialized,
                limit: crate::MAX_APP_UNPACKED_BYTES,
            });
        }
    }
    Ok(())
}

/// Reconstruct the manifest + write the payload to `dest_dir`.
///
/// # Errors
///
/// Returns [`UnpackError::MissingRoot`] if `id` is not in `store`,
/// [`UnpackError::SignedRoot`] if the signed-root blob fails to decode,
/// [`UnpackError::MissingManifest`] or [`UnpackError::MissingCollection`] if
/// their blobs are missing, [`UnpackError::Manifest`] or
/// [`UnpackError::Collection`] if either fails to decode,
/// [`UnpackError::PathTraversal`] if a payload entry's path is absolute or
/// contains a `..` segment, [`UnpackError::MissingPayloadBlob`] if a
/// declared per-file blob is missing from `store`, or [`UnpackError::Io`] or
/// [`UnpackError::Store`] if creating `dest_dir`, writing a file, or reading
/// from `store` fails.
pub fn unpack<S: BlobStore>(
    id: PackageId,
    store: &S,
    dest_dir: &Path,
) -> Result<Manifest, UnpackError> {
    let signed_bytes = store
        .get(&crate::blobs::BlobHash(*id.as_bytes()))?
        .ok_or(UnpackError::MissingRoot(id))?;
    let signed = SignedRoot::decode(&signed_bytes)?;
    let manifest_bytes = store
        .get(&signed.manifest_hash)?
        .ok_or(UnpackError::MissingManifest)?;
    let manifest_str = std::str::from_utf8(&manifest_bytes)
        .map_err(|e| UnpackError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
    let manifest = Manifest::from_toml_str(manifest_str)?;
    let collection_bytes = store
        .get(&signed.payload_root_hash)?
        .ok_or(UnpackError::MissingCollection)?;
    let collection = Collection::decode(&collection_bytes)?;

    // Validate the complete materialized shape before creating `dest_dir`.
    // A collection can point many paths at one blob, so archive/store size is
    // not an upper bound on disk writes or inode consumption.
    validate_materialized_payload(&manifest, &collection, store)?;

    std::fs::create_dir_all(dest_dir)?;
    let dest_dir = dest_dir.canonicalize()?;
    // The entrypoint declared by the manifest MUST be executable — the
    // launcher execs it directly. We record the normalised path here so
    // we can mark it +x once its bytes hit disk. Path values inside a
    // Collection are always forward-slash-normalised regardless of host,
    // so the comparison is stable.
    #[cfg(unix)]
    let entrypoint_rel: Option<String> = match manifest.package.kind {
        crate::id::Kind::App | crate::id::Kind::Service => manifest
            .entrypoint
            .as_ref()
            .and_then(|e| e.bin.to_str())
            .map(|s| s.trim_start_matches("./").replace('\\', "/")),
        _ => None,
    };
    for (rel_path, hash) in &collection.entries {
        // Path-traversal guard: reject `..` segments and absolute
        // paths inside the collection. Done as a string check
        // before joining so a crafted entry can't escape.
        if std::path::Path::new(rel_path).is_absolute()
            || rel_path.split('/').any(|seg| seg == "..")
        {
            return Err(UnpackError::PathTraversal(rel_path.clone()));
        }
        let out_path = dest_dir.join(rel_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = store
            .get(hash)?
            .ok_or_else(|| UnpackError::MissingPayloadBlob(hash.to_hex(), rel_path.clone()))?;
        std::fs::write(&out_path, bytes)?;
        #[cfg(unix)]
        if entrypoint_rel.as_deref() == Some(rel_path.as_str()) {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = std::fs::metadata(&out_path)?.permissions();
            perm.set_mode(perm.mode() | 0o111); // preserve read/write bits, add exec
            std::fs::set_permissions(&out_path, perm)?;
        }
    }
    Ok(manifest)
}
