//! Content-addressed blob store with **range reads** as the primitive.
//!
//! Unlike `qos-pkg::BlobStore`'s whole-blob `get`/`put` (packing needs
//! the whole bundle in hand), the mount layer serves small windows
//! (often one 4 KiB page) into multi-hundred-MB blobs, and the whole
//! point of the mount is to avoid pulling unrequested bytes. So it
//! exposes `get_range(hash, offset, len)`, `has(hash)`, and
//! `size(hash)` (for `getattr()`, without reading content).
//!
//! Implementations: [`MemoryBlobStore`] (tests/CLI pack flow, slices),
//! [`DiskBlobStore`] (files at `<dir>/<blake3-hex>`, `pread()`), and
//! [`CountingStore`] (wraps any store, counts calls — tests prove
//! partial reads don't over-fetch). A real iroh-blobs adapter is out
//! of scope; its async/back-pressure semantics belong in a separate crate.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

/// Errors a `BlobStore` can return; the mount layer translates these to `EIO` for the kernel.
#[derive(Debug, Error)]
pub enum BlobError {
    /// The hash isn't in the store.
    #[error("blob not present: {0}")]
    Missing(String),
    /// The on-disk hex filename failed to decode.
    #[error("malformed blob filename: {0}")]
    BadName(String),
    /// Underlying I/O error from the backing store.
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// Synchronous, content-addressed blob store with range reads.
/// Implementations must be cheap to clone or wrap in `Arc` — the
/// mount holds one shared store across all FUSE worker threads.
pub trait BlobStore: Send + Sync {
    /// Read `len` bytes starting at `offset` from the blob keyed by `hash`.
    ///
    /// Implementations MUST NOT read more than `len` bytes from their
    /// backing source — the point of this trait is to stream only what
    /// the caller asked for. A short read (`Vec` shorter than `len`)
    /// signals end-of-blob.
    fn get_range(&self, hash: &[u8; 32], offset: u64, len: u32) -> Result<Vec<u8>, BlobError>;

    /// Whether `hash` is known to this store.
    fn has(&self, hash: &[u8; 32]) -> bool;

    /// Total byte length of the blob; backs `getattr()`'s size field so
    /// the kernel knows the logical end without reading content.
    fn size(&self, hash: &[u8; 32]) -> Result<u64, BlobError>;
}

/// In-memory blob store for tests and the CLI's pack flow; cheap to
/// clone (backing state in `Arc<Mutex<_>>`).
#[derive(Debug, Default, Clone)]
pub struct MemoryBlobStore {
    blobs: Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>,
}

impl MemoryBlobStore {
    /// Fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `bytes`; returns its BLAKE3 hash.
    pub fn put(&self, bytes: &[u8]) -> [u8; 32] {
        let h = *blake3::hash(bytes).as_bytes();
        self.blobs.lock().unwrap().insert(h, bytes.to_vec());
        h
    }

    /// Number of blobs stored.
    pub fn len(&self) -> usize {
        self.blobs.lock().unwrap().len()
    }

    /// Whether the store contains no blobs.
    pub fn is_empty(&self) -> bool {
        self.blobs.lock().unwrap().is_empty()
    }
}

impl BlobStore for MemoryBlobStore {
    fn get_range(&self, hash: &[u8; 32], offset: u64, len: u32) -> Result<Vec<u8>, BlobError> {
        let blobs = self.blobs.lock().unwrap();
        let blob = blobs
            .get(hash)
            .ok_or_else(|| BlobError::Missing(hex(hash)))?;
        let start = offset.min(blob.len() as u64) as usize;
        let end = (offset.saturating_add(u64::from(len))).min(blob.len() as u64) as usize;
        Ok(blob[start..end].to_vec())
    }

    fn has(&self, hash: &[u8; 32]) -> bool {
        self.blobs.lock().unwrap().contains_key(hash)
    }

    fn size(&self, hash: &[u8; 32]) -> Result<u64, BlobError> {
        let blobs = self.blobs.lock().unwrap();
        blobs
            .get(hash)
            .map(|b| b.len() as u64)
            .ok_or_else(|| BlobError::Missing(hex(hash)))
    }
}

/// On-disk blob store. Each blob lives at `<dir>/<blake3-hex>`.
///
/// `get_range` uses `pread()` so concurrent FUSE workers don't trip
/// over a shared file cursor; the file handle is opened and dropped
/// per call (fine here — cache open `File`s later if profiling shows
/// the syscall cost dominating).
#[derive(Debug, Clone)]
pub struct DiskBlobStore {
    root: PathBuf,
}

impl DiskBlobStore {
    /// Anchor at `root`. The directory must already exist.
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self { root: root.into() }
    }

    /// Where this store keeps its blobs.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write `bytes` under their BLAKE3 hash; returns the hash. Exposed
    /// for tests and one-off ingest — production packers go through
    /// `qos-pkg`, which lays out a directory the same way.
    pub fn put(&self, bytes: &[u8]) -> Result<[u8; 32], BlobError> {
        let h = *blake3::hash(bytes).as_bytes();
        let path = self.path_for(&h);
        std::fs::write(&path, bytes)?;
        Ok(h)
    }

    fn path_for(&self, hash: &[u8; 32]) -> PathBuf {
        self.root.join(hex(hash))
    }
}

impl BlobStore for DiskBlobStore {
    fn get_range(&self, hash: &[u8; 32], offset: u64, len: u32) -> Result<Vec<u8>, BlobError> {
        let path = self.path_for(hash);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(BlobError::Missing(hex(hash)));
            }
            Err(e) => return Err(e.into()),
        };
        // Clamp the read against EOF so a short trailing read returns the
        // truncated tail, not zero-padded garbage; EOF comes from metadata to
        // keep non-trailing reads at one pread.
        let file_len = file.metadata()?.len();
        if offset >= file_len {
            return Ok(Vec::new());
        }
        let want = ((file_len - offset).min(u64::from(len))) as usize;
        let mut buf = vec![0u8; want];
        let mut filled = 0usize;
        while filled < want {
            let n = file.read_at(&mut buf[filled..], offset + filled as u64)?;
            if n == 0 {
                buf.truncate(filled);
                break;
            }
            filled += n;
        }
        Ok(buf)
    }

    fn has(&self, hash: &[u8; 32]) -> bool {
        self.path_for(hash).exists()
    }

    fn size(&self, hash: &[u8; 32]) -> Result<u64, BlobError> {
        let path = self.path_for(hash);
        match std::fs::metadata(&path) {
            Ok(md) => Ok(md.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(BlobError::Missing(hex(hash)))
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// A wrapping store that counts every call; tests use it to prove the mount only fetches what was requested.
#[derive(Debug, Clone)]
pub struct CountingStore<S: BlobStore> {
    inner: S,
    /// Total `get_range` calls.
    pub range_calls: Arc<AtomicU64>,
    /// Sum of `len` parameters across all `get_range` calls.
    pub bytes_requested: Arc<AtomicU64>,
    /// Total `size` calls.
    pub size_calls: Arc<AtomicU64>,
    /// Total `has` calls.
    pub has_calls: Arc<AtomicU64>,
}

impl<S: BlobStore> CountingStore<S> {
    /// Wrap `inner`. Counts start at zero.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            range_calls: Arc::new(AtomicU64::new(0)),
            bytes_requested: Arc::new(AtomicU64::new(0)),
            size_calls: Arc::new(AtomicU64::new(0)),
            has_calls: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Snapshot the call counters.
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            range_calls: self.range_calls.load(Ordering::SeqCst),
            bytes_requested: self.bytes_requested.load(Ordering::SeqCst),
            size_calls: self.size_calls.load(Ordering::SeqCst),
            has_calls: self.has_calls.load(Ordering::SeqCst),
        }
    }
}

/// Snapshot of a [`CountingStore`]'s counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CounterSnapshot {
    pub range_calls: u64,
    pub bytes_requested: u64,
    pub size_calls: u64,
    pub has_calls: u64,
}

impl<S: BlobStore> BlobStore for CountingStore<S> {
    fn get_range(&self, hash: &[u8; 32], offset: u64, len: u32) -> Result<Vec<u8>, BlobError> {
        self.range_calls.fetch_add(1, Ordering::SeqCst);
        self.bytes_requested
            .fetch_add(u64::from(len), Ordering::SeqCst);
        self.inner.get_range(hash, offset, len)
    }

    fn has(&self, hash: &[u8; 32]) -> bool {
        self.has_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.has(hash)
    }

    fn size(&self, hash: &[u8; 32]) -> Result<u64, BlobError> {
        self.size_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.size(hash)
    }
}

/// Implement `BlobStore` for `Arc<T>` so the mount can hold one shared
/// handle across FUSE workers without every call site derefing.
impl<T: BlobStore + ?Sized> BlobStore for Arc<T> {
    fn get_range(&self, hash: &[u8; 32], offset: u64, len: u32) -> Result<Vec<u8>, BlobError> {
        (**self).get_range(hash, offset, len)
    }
    fn has(&self, hash: &[u8; 32]) -> bool {
        (**self).has(hash)
    }
    fn size(&self, hash: &[u8; 32]) -> Result<u64, BlobError> {
        (**self).size(hash)
    }
}

/// Hex-encode a 32-byte hash. Lower-case, 64 chars.
pub fn hex(hash: &[u8; 32]) -> String {
    const CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in hash {
        s.push(CHARS[(b >> 4) as usize] as char);
        s.push(CHARS[(b & 0xf) as usize] as char);
    }
    s
}

/// Decode a 64-char hex string back to a 32-byte hash; used by disk-store recovery/debugging, not the hot path.
pub fn from_hex(s: &str) -> Result<[u8; 32], BlobError> {
    if s.len() != 64 {
        return Err(BlobError::BadName(s.to_string()));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = nibble(chunk[0]).ok_or_else(|| BlobError::BadName(s.to_string()))?;
        let lo = nibble(chunk[1]).ok_or_else(|| BlobError::BadName(s.to_string()))?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Convenience: read a whole blob via repeated `get_range` calls; not
/// on the FUSE hot path — used by the mount loader for the (tiny) Collection blob.
pub fn read_all<S: BlobStore + ?Sized>(store: &S, hash: &[u8; 32]) -> Result<Vec<u8>, BlobError> {
    let total = store.size(hash)?;
    let mut buf = Vec::with_capacity(total as usize);
    let mut offset = 0u64;
    const CHUNK: u32 = 64 * 1024;
    while offset < total {
        let want = ((total - offset).min(u64::from(CHUNK))) as u32;
        let part = store.get_range(hash, offset, want)?;
        if part.is_empty() {
            break;
        }
        offset += part.len() as u64;
        buf.extend(part);
    }
    Ok(buf)
}

/// Read the full contents of a file as a blob into the disk store; useful for one-off packing in tests.
pub fn put_file_disk(store: &DiskBlobStore, path: &Path) -> Result<[u8; 32], BlobError> {
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    store.put(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryBlobStore::new();
        let h = s.put(b"hello");
        assert!(s.has(&h));
        assert_eq!(s.size(&h).unwrap(), 5);
        assert_eq!(&s.get_range(&h, 0, 5).unwrap(), b"hello");
        assert_eq!(&s.get_range(&h, 1, 3).unwrap(), b"ell");
        // Past-EOF read returns the tail, not pad.
        assert_eq!(&s.get_range(&h, 3, 10).unwrap(), b"lo");
    }

    #[test]
    fn disk_store_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(tmp.path());
        let h = store.put(b"hello world").unwrap();
        assert!(store.has(&h));
        assert_eq!(store.size(&h).unwrap(), 11);
        assert_eq!(&store.get_range(&h, 0, 5).unwrap(), b"hello");
        assert_eq!(&store.get_range(&h, 6, 5).unwrap(), b"world");
        assert_eq!(store.get_range(&h, 11, 5).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn disk_store_missing_is_distinct_from_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DiskBlobStore::new(tmp.path());
        let h = [0xab; 32];
        assert!(!store.has(&h));
        let err = store.size(&h).unwrap_err();
        assert!(matches!(err, BlobError::Missing(_)));
    }

    #[test]
    fn counting_store_records_calls() {
        let inner = MemoryBlobStore::new();
        let h = inner.put(b"abcdef");
        let counted = CountingStore::new(inner);
        let _ = counted.get_range(&h, 1, 3).unwrap();
        let _ = counted.get_range(&h, 0, 6).unwrap();
        let snap = counted.snapshot();
        assert_eq!(snap.range_calls, 2);
        assert_eq!(snap.bytes_requested, 9);
    }

    #[test]
    fn hex_roundtrip() {
        let h = [
            0u8, 1, 2, 3, 255, 254, 0xab, 0xcd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0,
        ];
        let s = hex(&h);
        assert_eq!(s.len(), 64);
        assert_eq!(from_hex(&s).unwrap(), h);
    }

    #[test]
    fn from_hex_rejects_bad_input() {
        assert!(from_hex("nope").is_err());
        assert!(from_hex(&"x".repeat(64)).is_err());
    }
}
