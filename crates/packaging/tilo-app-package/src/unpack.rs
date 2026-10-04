//! `unpack` — write a package's payload tree to a directory, streaming each
//! file from a [`BlobSource`] and checking its hash on the way.
//!
//! This is the reverse of [`crate::pack::pack`]. It does **not** check the
//! publisher signature — see [`crate::verify()`] for that. Unpack succeeds on
//! an unsigned package so build tooling can round-trip without keys.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::blobs::{BlobHash, BlobSource, Collection, HashingReader};
use crate::manifest::Manifest;
use crate::package::PackageMeta;

#[derive(Debug, thiserror::Error)]
pub enum UnpackError {
    #[error("payload file {1:?} (blob {0}) is not available")]
    MissingPayloadBlob(String, String),
    #[error("payload file {0:?} does not match its hash")]
    PayloadHashMismatch(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("destination {0} already exists")]
    DestinationExists(String),
    #[error("application payload has {found} entries; limit is {limit}")]
    TooManyPayloadEntries { found: usize, limit: usize },
    #[error("application payload materializes to {found} bytes; limit is {limit}")]
    PayloadTooLarge { found: u64, limit: u64 },
    #[error("image tree: {0}")]
    BadTree(String),
}

/// Validate the filesystem work a decoded payload collection represents.
///
/// `len_of` reports the size of each blob the caller holds, and `None` for
/// one it lacks. Separate from [`unpack`] so a registry or an installer can
/// reject amplification before writing a single file: a collection may point
/// many paths at one blob, so neither archive size nor stored bytes bound
/// the disk writes or inodes of a materialised tree.
///
/// # Errors
///
/// Returns [`UnpackError::TooManyPayloadEntries`] or
/// [`UnpackError::PayloadTooLarge`] when an application exceeds its
/// materialization budget, or [`UnpackError::MissingPayloadBlob`] when a
/// collection entry has no known size.
pub fn validate_materialized_payload(
    manifest: &Manifest,
    collection: &Collection,
    mut len_of: impl FnMut(&BlobHash) -> Option<u64>,
) -> Result<(), UnpackError> {
    let Some(limit) = crate::app_size_limit(manifest) else {
        return Ok(());
    };
    if collection.entries.len() > crate::MAX_APP_PAYLOAD_ENTRIES {
        return Err(UnpackError::TooManyPayloadEntries {
            found: collection.entries.len(),
            limit: crate::MAX_APP_PAYLOAD_ENTRIES,
        });
    }
    let mut sizes = BTreeMap::new();
    let mut materialized = 0u64;
    for (rel_path, hash) in &collection.entries {
        let len = if let Some(len) = sizes.get(hash) {
            *len
        } else {
            let len = len_of(hash)
                .ok_or_else(|| UnpackError::MissingPayloadBlob(hash.to_hex(), rel_path.clone()))?;
            sizes.insert(*hash, len);
            len
        };
        materialized = materialized.saturating_add(len);
        if materialized > limit {
            return Err(UnpackError::PayloadTooLarge {
                found: materialized,
                limit,
            });
        }
    }
    Ok(())
}

/// Write `meta`'s payload tree under `dest_dir`, which must not exist yet.
///
/// Each file streams from `source` and must hash to its collection entry;
/// the entrypoint gets its execute bit. On error the partial tree is left
/// for the caller to remove.
///
/// # Errors
///
/// Returns [`UnpackError::DestinationExists`] if `dest_dir` exists,
/// [`UnpackError::MissingPayloadBlob`] if `source` lacks a file,
/// [`UnpackError::PayloadHashMismatch`] if a file's bytes do not match, or
/// [`UnpackError::Io`] if writing fails.
pub fn unpack(
    meta: &PackageMeta,
    source: &dyn BlobSource,
    dest_dir: &Path,
) -> Result<(), UnpackError> {
    if std::fs::symlink_metadata(dest_dir).is_ok() {
        return Err(UnpackError::DestinationExists(
            dest_dir.display().to_string(),
        ));
    }
    std::fs::create_dir_all(dest_dir)?;
    let executable = meta.executable_path();
    for (rel_path, hash) in &meta.collection.entries {
        let out_path = dest_dir.join(rel_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let blob = source
            .open_blob(hash)?
            .ok_or_else(|| UnpackError::MissingPayloadBlob(hash.to_hex(), rel_path.clone()))?;
        // A source is untrusted: never write more than it claims, and let
        // the hash check below catch a wrong claim.
        let mut hashing = HashingReader::new(std::io::Read::take(blob.reader, blob.len));
        let mut file = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&out_path)?;
        std::io::copy(&mut hashing, &mut file)?;
        file.flush()?;
        if hashing.finish().0 != *hash {
            return Err(UnpackError::PayloadHashMismatch(rel_path.clone()));
        }
        #[cfg(unix)]
        if executable.as_deref() == Some(rel_path.as_str()) {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = file.metadata()?.permissions();
            perm.set_mode(perm.mode() | 0o111);
            file.set_permissions(perm)?;
        }
    }
    if meta.manifest.package.kind.is_image() {
        let tree = crate::image::TreeSpec::from_bytes(&std::fs::read(
            dest_dir.join(crate::image::TREE_FILE),
        )?)
        .and_then(|tree| tree.check(&meta.collection).map(|()| tree))
        .map_err(|error| UnpackError::BadTree(error.to_string()))?;
        #[cfg(unix)]
        for path in &tree.executable {
            use std::os::unix::fs::PermissionsExt;
            let path = dest_dir.join(path);
            let mut perm = std::fs::metadata(&path)?.permissions();
            perm.set_mode(perm.mode() | 0o111);
            std::fs::set_permissions(&path, perm)?;
        }
        tree.apply(dest_dir)?;
    }
    Ok(())
}
