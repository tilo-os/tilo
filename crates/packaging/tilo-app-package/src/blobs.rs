//! Content addressing for package payloads.
//!
//! A payload file is named by the BLAKE3 hash of its bytes, the same hash
//! the device's content store and iroh-blobs use, so a package's files are
//! ordinary content wherever they are held. This module owns the naming
//! ([`BlobHash`]), the signed path index ([`Collection`]), and the two
//! streaming edges a codec needs: [`BlobSource`] to read payload bytes that
//! live somewhere else and [`BlobSink`] to hand decoded bytes to whoever
//! stores them. Nothing here keeps bytes itself.

use std::collections::BTreeMap;
use std::io::{self, Read};

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 32-byte BLAKE3 hash of a blob's bytes.
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

    /// Hash everything `reader` yields, returning the hash and the length.
    ///
    /// # Errors
    ///
    /// Returns any error from reading.
    pub fn of_reader(reader: &mut dyn Read) -> io::Result<(Self, u64)> {
        let mut hashing = HashingReader::new(reader);
        io::copy(&mut hashing, &mut io::sink())?;
        Ok(hashing.finish())
    }

    /// Lower-case hex encoding (64 chars).
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse the 64-character hex form.
    pub fn from_hex(text: &str) -> Option<Self> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(text, &mut out).ok()?;
        Some(Self(out))
    }

    /// Raw 32 bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hex = self.to_hex();
        let short = hex.get(..8).unwrap_or(&hex);
        write!(f, "blake3:{short}…")
    }
}

impl std::fmt::Display for BlobHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A reader that hashes what passes through it.
pub struct HashingReader<R> {
    inner: R,
    hasher: Hasher,
    len: u64,
}

impl<R: Read> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Hasher::new(),
            len: 0,
        }
    }

    /// The hash and length of everything read so far.
    pub fn finish(&self) -> (BlobHash, u64) {
        (BlobHash(*self.hasher.finalize().as_bytes()), self.len)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(buf.get(..n).unwrap_or_default());
        self.len = self.len.saturating_add(n as u64);
        Ok(n)
    }
}

/// An open blob: its claimed length and a reader over its bytes.
pub struct OpenBlob<'a> {
    pub len: u64,
    pub reader: Box<dyn Read + 'a>,
}

/// Where payload bytes can be read from: a directory being packed, a
/// materialised install, an in-memory map in a test.
pub trait BlobSource {
    /// Open the bytes of `hash`, or `Ok(None)` when this source lacks them.
    /// Callers verify the hash while reading; a source is not trusted.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes exist but cannot be opened.
    fn open_blob(&self, hash: &BlobHash) -> io::Result<Option<OpenBlob<'_>>>;
}

/// Where decoded payload bytes go. The codec verifies each blob against its
/// name after the sink has consumed it, so a sink may store eagerly: a blob
/// that fails verification is only ever stored under its true hash.
pub trait BlobSink {
    /// Called once with the package's metadata, before the first blob (or
    /// at the end of an archive that carries none). Returning an error
    /// aborts decoding before any payload byte reaches the sink — the place
    /// to apply trust policy.
    ///
    /// # Errors
    ///
    /// Returns an error to refuse the package.
    fn metadata(&mut self, _meta: &crate::package::PackageMeta) -> io::Result<()> {
        Ok(())
    }

    /// Consume the `len` bytes of the blob named `hash`.
    ///
    /// # Errors
    ///
    /// Returns an error to abort decoding.
    fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> io::Result<()>;
}

/// Where the payload blobs a thin package does not carry can be fetched
/// from: a peer, a registry, a mirror. Whoever receives the bytes verifies
/// each against its hash; a fetcher need not be trusted.
pub trait BlobFetcher: Sync {
    /// Deliver each of `hashes` to `sink`.
    ///
    /// # Errors
    ///
    /// Returns an error when a blob cannot be fetched or `sink` refuses it.
    fn fetch(&self, hashes: &[BlobHash], sink: &mut dyn BlobSink) -> io::Result<()>;
}

impl BlobSource for BTreeMap<BlobHash, Vec<u8>> {
    fn open_blob(&self, hash: &BlobHash) -> io::Result<Option<OpenBlob<'_>>> {
        Ok(self.get(hash).map(|bytes| OpenBlob {
            len: bytes.len() as u64,
            reader: Box::new(bytes.as_slice()),
        }))
    }
}

impl BlobSink for BTreeMap<BlobHash, Vec<u8>> {
    fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> io::Result<()> {
        let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0).min(1 << 24));
        bytes.read_to_end(&mut buf)?;
        self.insert(*hash, buf);
        Ok(())
    }
}

/// A sink that keeps nothing; decoding still verifies every blob.
#[derive(Debug, Default, Clone, Copy)]
pub struct DiscardBlobs;

impl BlobSink for DiscardBlobs {
    fn accept(&mut self, _hash: &BlobHash, _len: u64, bytes: &mut dyn Read) -> io::Result<()> {
        io::copy(bytes, &mut io::sink()).map(drop)
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
    /// MAGIC (8 bytes) "TLOCOL\0\0"
    /// u32 LE count
    /// for each entry, sorted by path:
    ///     u32 LE path-byte-length
    ///     path-bytes (UTF-8, no NUL)
    ///     [u8; 32] hash
    /// ```
    ///
    /// Hand-rolled (not postcard/serde) so the wire format doesn't churn with serde framework versions.
    pub fn encode(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(12usize.saturating_add(self.entries.len().saturating_mul(36)));
        out.extend_from_slice(b"TLOCOL\0\0");
        out.extend_from_slice(
            &u32::try_from(self.entries.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for (path, hash) in &self.entries {
            let pb = path.as_bytes();
            out.extend_from_slice(&u32::try_from(pb.len()).unwrap_or(u32::MAX).to_le_bytes());
            out.extend_from_slice(pb);
            out.extend_from_slice(&hash.0);
        }
        out
    }

    /// Decode bytes produced by [`Self::encode`]; strict — extra trailing
    /// bytes or malformed lengths are rejected rather than truncated or
    /// silently accepted.
    ///
    /// # Errors
    ///
    /// Returns [`CollectionDecodeError::BadMagic`] if the header doesn't
    /// match, [`CollectionDecodeError::Truncated`] if a declared length runs
    /// past the end of `bytes`, [`CollectionDecodeError::BadPath`] if a path
    /// is not valid UTF-8, [`CollectionDecodeError::DuplicatePath`] if the
    /// same path appears twice, and [`CollectionDecodeError::Trailing`] if
    /// bytes remain after the declared entries.
    pub fn decode(bytes: &[u8]) -> Result<Self, CollectionDecodeError> {
        let header = bytes.get(0..8).ok_or(CollectionDecodeError::BadMagic)?;
        if header != b"TLOCOL\0\0" {
            return Err(CollectionDecodeError::BadMagic);
        }
        let count_bytes = bytes.get(8..12).ok_or(CollectionDecodeError::Truncated)?;
        let count_arr: [u8; 4] = count_bytes
            .try_into()
            .map_err(|_| CollectionDecodeError::Truncated)?;
        let count = u32::from_le_bytes(count_arr) as usize;
        let mut cursor: usize = 12;
        let mut out: BTreeMap<String, BlobHash> = BTreeMap::new();
        for _ in 0..count {
            let len_end = cursor
                .checked_add(4)
                .ok_or(CollectionDecodeError::Truncated)?;
            let plen_bytes = bytes
                .get(cursor..len_end)
                .ok_or(CollectionDecodeError::Truncated)?;
            let plen_arr: [u8; 4] = plen_bytes
                .try_into()
                .map_err(|_| CollectionDecodeError::Truncated)?;
            let plen = u32::from_le_bytes(plen_arr) as usize;
            cursor = len_end;

            let path_end = cursor
                .checked_add(plen)
                .ok_or(CollectionDecodeError::Truncated)?;
            let hash_end = path_end
                .checked_add(32)
                .ok_or(CollectionDecodeError::Truncated)?;
            let path_bytes = bytes
                .get(cursor..path_end)
                .ok_or(CollectionDecodeError::Truncated)?;
            let path = std::str::from_utf8(path_bytes)
                .map_err(|_| CollectionDecodeError::BadPath)?
                .to_string();
            cursor = path_end;

            let hash_bytes = bytes
                .get(cursor..hash_end)
                .ok_or(CollectionDecodeError::Truncated)?;
            let mut h = [0u8; 32];
            h.copy_from_slice(hash_bytes);
            cursor = hash_end;

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
    fn blob_maps_hash_store_and_encode_deterministically() {
        // phase: hashing_a_reader_matches_hashing_the_bytes
        {
            let bytes = b"hello payload".to_vec();
            let (hash, len) = BlobHash::of_reader(&mut bytes.as_slice()).unwrap();
            assert_eq!(hash, BlobHash::of(&bytes));
            assert_eq!(len, bytes.len() as u64);
            assert_eq!(BlobHash::from_hex(&hash.to_hex()), Some(hash));
            assert_eq!(BlobHash::from_hex("zz"), None);
        }
        // phase: a_map_is_both_a_source_and_a_sink
        {
            let mut map = BTreeMap::new();
            let hash = BlobHash::of(b"abc");
            map.accept(&hash, 3, &mut &b"abc"[..]).unwrap();
            let mut out = Vec::new();
            let blob = map.open_blob(&hash).unwrap().unwrap();
            assert_eq!(blob.len, 3);
            blob.reader.take(3).read_to_end(&mut out).unwrap();
            assert_eq!(out, b"abc");
            assert!(map.open_blob(&BlobHash([0; 32])).unwrap().is_none());
        }
        // phase: collection_encoding_is_deterministic
        {
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
}
