//! The payload Collection — a deterministic `path -> blob_hash`
//! table.
//!
//! The encoding is byte-identical to the one in `qos-pkg::blobs`
//! so a Collection blob produced by `qos-pkg pack` can be read
//! directly by the mount. We don't pull `qos-pkg` in as a
//! dependency here because the qos-pkg `BlobStore` trait is
//! whole-blob (no `get_range`) and we want this crate's
//! dependency tree to stay minimal — the mount has to be cheap
//! enough that `qos-app-host` can spawn one per running app.
//!
//! ## Wire format
//!
//! ```text
//! MAGIC      "QOSCOL\0\0"          (8 bytes)
//! count      u32 LE                (4 bytes)
//! for each entry, sorted by path:
//!     path-len    u32 LE           (4 bytes)
//!     path-bytes  UTF-8, no NUL    (path-len bytes)
//!     hash        [u8; 32]
//! ```
//!
//! The format MUST stay byte-stable across versions of this
//! crate — the package id is BLAKE3 of the signed-root, which
//! commits to the collection root, which is BLAKE3 of these
//! bytes. Reformat = different package id = breakage.

use std::collections::BTreeMap;

use thiserror::Error;

const MAGIC: &[u8; 8] = b"QOSCOL\0\0";

/// A payload Collection: payload-relative path -> BLAKE3 blob
/// hash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collection {
    /// Sorted by path (`BTreeMap` iteration order), so [`encode`]
    /// is deterministic.
    ///
    /// [`encode`]: Self::encode
    pub entries: BTreeMap<String, [u8; 32]>,
}

impl Collection {
    /// Build from an unsorted iterator. Entries are stored
    /// sorted so encoding is deterministic regardless of how
    /// they arrived.
    pub fn from_entries<I: IntoIterator<Item = (String, [u8; 32])>>(it: I) -> Self {
        Self {
            entries: it.into_iter().collect(),
        }
    }

    /// Encode in the wire format described in the module docs.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            MAGIC
                .len()
                .saturating_add(4)
                .saturating_add(self.entries.len().saturating_mul(4 + 32)),
        );
        out.extend_from_slice(MAGIC);
        let count = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&count.to_le_bytes());
        for (path, hash) in &self.entries {
            let pb = path.as_bytes();
            let plen = u32::try_from(pb.len()).unwrap_or(u32::MAX);
            out.extend_from_slice(&plen.to_le_bytes());
            out.extend_from_slice(pb);
            out.extend_from_slice(hash);
        }
        out
    }

    /// Decode bytes produced by [`Self::encode`]. Strict — any
    /// trailing bytes, duplicate paths, or NUL bytes in a path
    /// are rejected.
    ///
    /// # Errors
    ///
    /// Returns a [`CollectionDecodeError`] describing exactly what was
    /// wrong: bad/missing magic, a length that runs past the end of
    /// `bytes`, a non-UTF-8 or NUL-containing path, a duplicate path,
    /// or unconsumed trailing bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, CollectionDecodeError> {
        if bytes.len() < 12 {
            return Err(CollectionDecodeError::BadMagic);
        }
        let Some(magic) = bytes.get(0..8) else {
            return Err(CollectionDecodeError::BadMagic);
        };
        if magic != MAGIC {
            return Err(CollectionDecodeError::BadMagic);
        }
        let count_bytes = bytes
            .get(8..12)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .ok_or(CollectionDecodeError::Truncated)?;
        let count = u32::from_le_bytes(count_bytes) as usize;
        let mut cursor = 12usize;
        let mut out: BTreeMap<String, [u8; 32]> = BTreeMap::new();
        for _ in 0..count {
            let plen_end = cursor.saturating_add(4);
            let plen_bytes = bytes
                .get(cursor..plen_end)
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .ok_or(CollectionDecodeError::Truncated)?;
            let plen = u32::from_le_bytes(plen_bytes) as usize;
            cursor = plen_end;

            let path_end = cursor.saturating_add(plen);
            let hash_end = path_end.saturating_add(32);
            let raw = bytes
                .get(cursor..path_end)
                .ok_or(CollectionDecodeError::Truncated)?;
            if raw.contains(&0) {
                return Err(CollectionDecodeError::BadPath);
            }
            let path = std::str::from_utf8(raw)
                .map_err(|_| CollectionDecodeError::BadPath)?
                .to_string();
            cursor = path_end;

            let hash_bytes = bytes
                .get(cursor..hash_end)
                .ok_or(CollectionDecodeError::Truncated)?;
            let mut h = [0u8; 32];
            h.copy_from_slice(hash_bytes);
            cursor = hash_end;

            if out.insert(path, h).is_some() {
                return Err(CollectionDecodeError::DuplicatePath);
            }
        }
        if cursor != bytes.len() {
            return Err(CollectionDecodeError::Trailing);
        }
        Ok(Self { entries: out })
    }

    /// BLAKE3 hash of the encoded form. This is the
    /// "collection root" the signed manifest binds into.
    pub fn root_hash(&self) -> [u8; 32] {
        *blake3::hash(&self.encode()).as_bytes()
    }
}

/// Errors from [`Collection::decode`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CollectionDecodeError {
    #[error("missing or wrong magic header")]
    BadMagic,
    #[error("truncated collection encoding")]
    Truncated,
    #[error("non-UTF-8 path, NUL byte in path, or other bad bytes")]
    BadPath,
    #[error("duplicate path")]
    DuplicatePath,
    #[error("unexpected trailing bytes")]
    Trailing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_deterministic_across_insertion_orders() {
        let a = Collection::from_entries([
            ("a/x.bin".into(), [1u8; 32]),
            ("b/y.bin".into(), [2u8; 32]),
        ]);
        let b = Collection::from_entries([
            ("b/y.bin".into(), [2u8; 32]),
            ("a/x.bin".into(), [1u8; 32]),
        ]);
        assert_eq!(a.encode(), b.encode());
        assert_eq!(a.root_hash(), b.root_hash());
    }

    #[test]
    fn roundtrip() {
        let c = Collection::from_entries([
            ("share/icon.png".into(), [9u8; 32]),
            ("bin/app".into(), [7u8; 32]),
        ]);
        let bytes = c.encode();
        let back = Collection::decode(&bytes).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn rejects_truncated() {
        let c = Collection::from_entries([("foo".into(), [0u8; 32])]);
        let mut bytes = c.encode();
        let last = bytes.len() - 1;
        bytes.truncate(last);
        assert!(matches!(
            Collection::decode(&bytes),
            Err(CollectionDecodeError::Truncated)
        ));
    }

    #[test]
    fn rejects_bad_magic() {
        let bytes = b"GARBAGE\0\0\0\0\0".to_vec();
        assert!(matches!(
            Collection::decode(&bytes),
            Err(CollectionDecodeError::BadMagic)
        ));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let c = Collection::from_entries([("foo".into(), [0u8; 32])]);
        let mut bytes = c.encode();
        bytes.extend_from_slice(b"\x00\x00");
        assert!(matches!(
            Collection::decode(&bytes),
            Err(CollectionDecodeError::Trailing)
        ));
    }
}
