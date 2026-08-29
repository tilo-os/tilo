//! Content-addressed blob storage abstracted behind a small synchronous trait.
//!
//! Why not iroh-blobs directly: `qos-pkg` packages must also be
//! produceable as Flatpaks with no iroh runtime, and `iroh-blobs` is
//! async-tokio with a large surface that would force every consumer
//! (CLI, build tools, tests) into a tokio runtime. The on-wire
//! BLAKE3-rooted content addressing is identical either way — a
//! "Collection" is just `(name → hash)` pairs, encoded here as a
//! small deterministic blob (see [`Collection`]).
//!
//! The iroh-blobs-backed adapter lives in the loader (a follow-on
//! crate), implementing [`BlobStore`] over a real iroh `Store` so a
//! node can ingest a `.qpkg` and re-serve its blobs to peers.

use std::collections::BTreeMap;

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 32-byte BLAKE3 hash. The blob-store key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
    /// Hash bytes with BLAKE3 to produce a key.
    pub fn of(bytes: &[u8]) -> Self {
        let mut h = Hasher::new();
        h.update(bytes);
        Self(*h.finalize().as_bytes())
    }

    /// Lower-case hex encoding (64 chars).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "blake3:{}…", &self.to_hex()[..8])
    }
}

impl std::fmt::Display for BlobHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A blob-store backend, synchronous, errors as `BlobStoreError`; the
/// in-memory impl suffices for tests + CLI, the loader provides an iroh-blobs adapter.
pub trait BlobStore {
    /// Insert `bytes`; return the BLAKE3 hash. Idempotent — re-inserting the same bytes returns the same hash.
    fn put(&mut self, bytes: &[u8]) -> Result<BlobHash, BlobStoreError>;

    /// Borrow the bytes for `hash`; `None` if not present.
    fn get(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>, BlobStoreError>;

    /// Whether `hash` is present.
    fn has(&self, hash: &BlobHash) -> bool {
        matches!(self.get(hash), Ok(Some(_)))
    }
}

/// Errors from a `BlobStore`; the in-memory impl is infallible but
/// disk-backed impls (`FsStore` in the loader) can surface I/O errors here.
#[derive(Debug, Error)]
pub enum BlobStoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("blob {0} not found")]
    Missing(BlobHash),
}

/// A trivial in-memory `BlobStore` — used by the CLI, tests, and the
/// canonical .qpkg builder; persistence is the loader's job.
#[derive(Debug, Default, Clone)]
pub struct MemoryBlobStore {
    blobs: BTreeMap<BlobHash, Vec<u8>>,
}

impl MemoryBlobStore {
    /// Fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot every (hash, bytes) pair; used when serialising a .qpkg to tar.
    pub fn iter(&self) -> impl Iterator<Item = (&BlobHash, &Vec<u8>)> {
        self.blobs.iter()
    }

    /// Number of blobs stored.
    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    /// Whether the store contains no blobs.
    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

impl BlobStore for MemoryBlobStore {
    fn put(&mut self, bytes: &[u8]) -> Result<BlobHash, BlobStoreError> {
        let h = BlobHash::of(bytes);
        self.blobs.entry(h).or_insert_with(|| bytes.to_vec());
        Ok(h)
    }

    fn get(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>, BlobStoreError> {
        Ok(self.blobs.get(hash).cloned())
    }

    fn has(&self, hash: &BlobHash) -> bool {
        self.blobs.contains_key(hash)
    }
}

/// The payload "collection": payload-relative path → blob hash.
///
/// Stands in for iroh-blobs' `Collection` at the wire-stable layer.
/// Deterministic on-disk encoding (`BTreeMap`, so the hash is
/// reproducible) as a length-prefixed `Vec<(String, [u8;32])>` blob —
/// hand-encoded rather than via serde, so the format doesn't depend
/// on a framework choice at the canonical layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub entries: BTreeMap<String, BlobHash>,
}

impl Collection {
    /// Build a collection from an unsorted iterator; entries are stored
    /// sorted by path so the collection's own hash is stable across iteration orders.
    pub fn from_entries<I: IntoIterator<Item = (String, BlobHash)>>(it: I) -> Self {
        Self {
            entries: it.into_iter().collect(),
        }
    }

    /// Canonical encoding for hashing + signing. Layout:
    ///
    /// ```text
    /// MAGIC (8 bytes) "QOSCOL\0\0"
    /// u32 LE count
    /// for each entry, sorted by path:
    ///     u32 LE path-byte-length
    ///     path-bytes (UTF-8, no NUL)
    ///     [u8; 32] hash
    /// ```
    ///
    /// Hand-rolled (not postcard/serde) so the wire format doesn't churn with serde framework versions.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 4 + self.entries.len() * (4 + 32));
        out.extend_from_slice(b"QOSCOL\0\0");
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (path, hash) in &self.entries {
            let pb = path.as_bytes();
            out.extend_from_slice(&(pb.len() as u32).to_le_bytes());
            out.extend_from_slice(pb);
            out.extend_from_slice(&hash.0);
        }
        out
    }

    /// Decode bytes produced by [`Self::encode`]; strict — extra trailing bytes or malformed lengths are rejected.
    pub fn decode(bytes: &[u8]) -> Result<Self, CollectionDecodeError> {
        if bytes.len() < 12 || &bytes[0..8] != b"QOSCOL\0\0" {
            return Err(CollectionDecodeError::BadMagic);
        }
        let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let mut cursor = 12usize;
        let mut out: BTreeMap<String, BlobHash> = BTreeMap::new();
        for _ in 0..count {
            if cursor + 4 > bytes.len() {
                return Err(CollectionDecodeError::Truncated);
            }
            let plen = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
            cursor += 4;
            if cursor + plen + 32 > bytes.len() {
                return Err(CollectionDecodeError::Truncated);
            }
            let path = std::str::from_utf8(&bytes[cursor..cursor + plen])
                .map_err(|_| CollectionDecodeError::BadPath)?
                .to_string();
            cursor += plen;
            let mut h = [0u8; 32];
            h.copy_from_slice(&bytes[cursor..cursor + 32]);
            cursor += 32;
            if out.insert(path, BlobHash(h)).is_some() {
                return Err(CollectionDecodeError::DuplicatePath);
            }
        }
        if cursor != bytes.len() {
            return Err(CollectionDecodeError::Trailing);
        }
        Ok(Self { entries: out })
    }

    /// Hash of the encoded form — the "collection root" the signed manifest binds into.
    pub fn root_hash(&self) -> BlobHash {
        BlobHash::of(&self.encode())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CollectionDecodeError {
    #[error("missing or wrong magic header")]
    BadMagic,
    #[error("truncated collection encoding")]
    Truncated,
    #[error("non-UTF-8 path")]
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
    fn put_and_get_roundtrip() {
        let mut s = MemoryBlobStore::new();
        let h = s.put(b"hello").unwrap();
        assert_eq!(h, BlobHash::of(b"hello"));
        assert_eq!(s.get(&h).unwrap().unwrap(), b"hello");
    }

    #[test]
    fn collection_encoding_is_deterministic() {
        let c = Collection::from_entries([
            ("b.txt".into(), BlobHash([2u8; 32])),
            ("a.txt".into(), BlobHash([1u8; 32])),
        ]);
        let bytes = c.encode();
        let again = c.encode();
        assert_eq!(bytes, again);
        let back = Collection::decode(&bytes).unwrap();
        assert_eq!(back, c);
    }
}
