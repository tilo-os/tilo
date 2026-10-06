//! The signed metadata of one package: the three small blobs a package is
//! without its payload bytes.
//!
//! ```text
//! signed root (173 B) ──hash──▶ PackageId
//!   ├─ manifest_hash     ──▶ manifest.toml
//!   └─ payload_root_hash ──▶ collection (path → payload blob hash)
//! ```
//!
//! [`PackageMeta::from_parts`] checks every hash link, so holding a
//! `PackageMeta` means the manifest and collection are exactly the ones the
//! root commits to. Whether the root's signature is trusted is a separate
//! question, answered by [`crate::verify()`].

use crate::blobs::{BlobHash, Collection, CollectionDecodeError};
use crate::id::PackageId;
use crate::manifest::{Manifest, ManifestParseError};
use crate::signed::{SignedRoot, SignedRootError};

/// A package's signed root, manifest and payload collection, with every
/// hash link between them checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageMeta {
    pub id: PackageId,
    pub root: SignedRoot,
    pub manifest: Manifest,
    pub collection: Collection,
    /// The exact manifest bytes the root commits to.
    pub manifest_bytes: Vec<u8>,
    /// The exact encoded collection the root commits to.
    pub collection_bytes: Vec<u8>,
}

/// A structural failure: the pieces do not form one package.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    #[error("signed root: {0}")]
    SignedRoot(#[from] SignedRootError),
    #[error("signed root hashes to {found}, not the expected {expected}")]
    IdMismatch {
        expected: PackageId,
        found: PackageId,
    },
    #[error("manifest does not hash to the value the signed root commits to")]
    ManifestHashMismatch,
    #[error("payload collection does not hash to the value the signed root commits to")]
    CollectionHashMismatch,
    #[error("manifest is not UTF-8")]
    ManifestNotUtf8,
    #[error("manifest: {0}")]
    Manifest(#[from] ManifestParseError),
    #[error("payload collection: {0}")]
    Collection(#[from] CollectionDecodeError),
    #[error("payload path {0:?} is absolute or escapes the payload root")]
    UnsafePath(String),
}

impl PackageMeta {
    /// Assemble a package from its three metadata blobs, checking that
    /// `signed_root` hashes to `id` and commits to the other two.
    ///
    /// # Errors
    ///
    /// Returns the first broken link as a [`MetaError`].
    pub fn from_parts(
        id: PackageId,
        signed_root: &[u8],
        manifest_bytes: Vec<u8>,
        collection_bytes: Vec<u8>,
    ) -> Result<Self, MetaError> {
        let root = SignedRoot::decode(signed_root)?;
        let found = root.package_id();
        if found != id {
            return Err(MetaError::IdMismatch {
                expected: id,
                found,
            });
        }
        Self::from_root(root, manifest_bytes, collection_bytes)
    }

    /// Assemble a package from a decoded root and the blobs it commits to.
    ///
    /// # Errors
    ///
    /// Returns the first broken link as a [`MetaError`].
    pub fn from_root(
        root: SignedRoot,
        manifest_bytes: Vec<u8>,
        collection_bytes: Vec<u8>,
    ) -> Result<Self, MetaError> {
        if BlobHash::of(&manifest_bytes) != root.manifest_hash {
            return Err(MetaError::ManifestHashMismatch);
        }
        if BlobHash::of(&collection_bytes) != root.payload_root_hash {
            return Err(MetaError::CollectionHashMismatch);
        }
        let text = std::str::from_utf8(&manifest_bytes).map_err(|_| MetaError::ManifestNotUtf8)?;
        let manifest = Manifest::from_toml_str(text)?;
        let collection = Collection::decode(&collection_bytes)?;
        if let Some(path) = collection.entries.keys().find(|path| !is_safe_path(path)) {
            return Err(MetaError::UnsafePath(path.clone()));
        }
        Ok(Self {
            id: root.package_id(),
            root,
            manifest,
            collection,
            manifest_bytes,
            collection_bytes,
        })
    }

    /// The encoded signed root, whose hash is [`Self::id`].
    pub fn signed_root_bytes(&self) -> [u8; crate::SIGNED_ROOT_LEN] {
        self.root.encode()
    }

    /// The publisher key the root names.
    pub fn publisher(&self) -> [u8; 32] {
        self.root.publisher_identity
    }

    /// Each distinct payload blob, once, in hash order.
    pub fn payload_hashes(&self) -> std::collections::BTreeSet<BlobHash> {
        self.collection.entries.values().copied().collect()
    }

    /// The entrypoint path the launcher executes, normalised to the
    /// collection's forward-slash form, for kinds that have one. A runtime
    /// may declare its internal engine host; this does not make it an app.
    pub fn executable_path(&self) -> Option<String> {
        match self.manifest.package.kind {
            crate::Kind::App | crate::Kind::Service | crate::Kind::Runtime => self
                .manifest
                .entrypoint
                .as_ref()
                .and_then(|entry| entry.bin.to_str())
                .map(|path| path.trim_start_matches("./").replace('\\', "/")),
            _ => None,
        }
    }
}

/// A collection path is relative, non-empty, and has no `..`, `.` or empty
/// segments, so joining it under a directory cannot leave that directory.
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_paths_are_refused() {
        for path in ["", "/etc/passwd", "a/../b", "a//b", "./a", "a\\b", ".."] {
            assert!(!is_safe_path(path), "{path:?} must be refused");
        }
        for path in ["bin/app", "share/icons/app.png", "a.b/c..d"] {
            assert!(is_safe_path(path), "{path:?} must be accepted");
        }
    }
}
