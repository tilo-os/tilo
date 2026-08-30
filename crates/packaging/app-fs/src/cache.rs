//! Page cache that sits between the FUSE worker and the
//! [`BlobStore`].
//!
//! The kernel's page-fault path calls `read()` once per faulted
//! page (typically 4 KiB) for an mmap'd binary. Without a cache,
//! a binary that maps in the first 8 KiB then faults each page
//! twice would issue two store fetches per page — wasteful for a
//! remote store and pointless for a local one too.
//!
//! The cache is keyed on `(blob_hash, page_index)`. A "page" is
//! [`PAGE_SIZE`] bytes (4 KiB), so a [`get`] for an arbitrary
//! (offset, len) translates into one or more page-aligned
//! fetches that are then sliced and concatenated for the caller.
//!
//! Why 4 KiB? It's the kernel page size on every supported
//! arch, so mmap-driven fault patterns hit the cache cleanly. If
//! the FUSE max-read is larger than a page (the mount uses 1 MiB
//! by default), a single `read()` syscall just touches multiple
//! cache entries in one trip.

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use lru::LruCache;

use crate::store::{BlobError, BlobStore};

/// The cache's page size. Matches the kernel page size on the
/// architectures qOS supports.
pub const PAGE_SIZE: u32 = 4096;

/// LRU page cache wrapping a [`BlobStore`].
///
/// Owned (and shared via `Arc`) by the FUSE filesystem, so all
/// worker threads see the same cache state.
pub struct PageCache<S: BlobStore> {
    store: S,
    pages: Mutex<LruCache<PageKey, Vec<u8>>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// `(blob, page-index)` — the unit a cache entry is keyed by.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PageKey {
    hash: [u8; 32],
    page: u64,
}

/// Snapshot of cache hit/miss counters, for tests + debug
/// surfaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageCacheStats {
    pub hits: u64,
    pub misses: u64,
}

impl<S: BlobStore> PageCache<S> {
    /// Build a cache that retains the most recent `capacity_pages`
    /// pages. A 4 KiB page * 4096 capacity = 16 MiB of cache, the
    /// default the mount uses.
    pub fn new(store: S, capacity_pages: usize) -> Self {
        let cap = NonZeroUsize::new(capacity_pages.max(1)).unwrap();
        Self {
            store,
            pages: Mutex::new(LruCache::new(cap)),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Read `(offset, len)` from `hash`, going through the cache.
    /// The returned `Vec` is the concatenation of all pages
    /// touched, sliced down to the requested window.
    ///
    /// A short read (returned `Vec` shorter than `len`) signals
    /// end-of-blob — same contract as `BlobStore::get_range`.
    pub fn read(
        &self,
        hash: &[u8; 32],
        blob_size: u64,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, BlobError> {
        if offset >= blob_size || len == 0 {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(u64::from(len)).min(blob_size);
        let want = (end - offset) as usize;
        let mut out = Vec::with_capacity(want);

        let first_page = offset / u64::from(PAGE_SIZE);
        let last_page = (end - 1) / u64::from(PAGE_SIZE);
        for page in first_page..=last_page {
            let page_start = page * u64::from(PAGE_SIZE);
            let page_end = (page_start + u64::from(PAGE_SIZE)).min(blob_size);
            let page_bytes = self.fetch_page(hash, page, page_start, page_end)?;
            // Slice this page down to the requested window.
            let slice_start = offset.saturating_sub(page_start) as usize;
            let slice_end = (end - page_start).min(page_bytes.len() as u64) as usize;
            // Bound `slice_start` so an interior page always
            // takes its full payload.
            let slice_start = slice_start.min(page_bytes.len());
            if slice_start < slice_end {
                out.extend_from_slice(&page_bytes[slice_start..slice_end]);
            }
        }
        Ok(out)
    }

    /// Look the page up; on miss, fetch from the store and
    /// install. The fetch reads exactly the page's byte window —
    /// never more.
    fn fetch_page(
        &self,
        hash: &[u8; 32],
        page: u64,
        page_start: u64,
        page_end: u64,
    ) -> Result<Vec<u8>, BlobError> {
        let key = PageKey { hash: *hash, page };
        {
            let mut pages = self.pages.lock().unwrap();
            if let Some(bytes) = pages.get(&key) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(bytes.clone());
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        let want = (page_end - page_start) as u32;
        let bytes = self.store.get_range(hash, page_start, want)?;
        let mut pages = self.pages.lock().unwrap();
        pages.put(key, bytes.clone());
        Ok(bytes)
    }

    /// Snapshot the hit/miss counters.
    pub fn stats(&self) -> PageCacheStats {
        PageCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }

    /// Borrow the wrapped store. Useful for callers that need
    /// to bypass the cache (e.g. loading the Collection blob).
    pub fn store(&self) -> &S {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{CountingStore, MemoryBlobStore};

    #[test]
    fn read_same_page_twice_hits_cache_once() {
        let inner = MemoryBlobStore::new();
        let payload = vec![0xAB; 8192];
        let h = inner.put(&payload);
        let counted = CountingStore::new(inner);
        let cache = PageCache::new(counted.clone(), 16);
        let _ = cache.read(&h, 8192, 0, 100).unwrap();
        let _ = cache.read(&h, 8192, 50, 100).unwrap();
        // Both reads land entirely inside the first page so only
        // one store fetch should have happened.
        let snap = counted.snapshot();
        assert_eq!(snap.range_calls, 1, "expected exactly one store fetch");
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 1);
    }

    #[test]
    fn read_across_pages_returns_correct_bytes() {
        let inner = MemoryBlobStore::new();
        let mut payload = Vec::with_capacity(PAGE_SIZE as usize * 3);
        for i in 0..PAGE_SIZE as usize * 3 {
            payload.push((i & 0xff) as u8);
        }
        let h = inner.put(&payload);
        let cache = PageCache::new(inner, 16);
        let got = cache
            .read(&h, payload.len() as u64, u64::from(PAGE_SIZE) - 5, 10)
            .unwrap();
        assert_eq!(got.len(), 10);
        assert_eq!(got, payload[PAGE_SIZE as usize - 5..PAGE_SIZE as usize + 5]);
    }

    #[test]
    fn read_past_end_returns_short() {
        let inner = MemoryBlobStore::new();
        let h = inner.put(b"hello");
        let cache = PageCache::new(inner, 4);
        let got = cache.read(&h, 5, 3, 10).unwrap();
        assert_eq!(&got, b"lo");
    }

    #[test]
    fn lru_evicts_old_pages() {
        let inner = MemoryBlobStore::new();
        // 4 pages of distinct bytes.
        let mut payload = vec![0u8; PAGE_SIZE as usize * 4];
        for i in 0..4 {
            for j in 0..PAGE_SIZE as usize {
                payload[i * PAGE_SIZE as usize + j] = i as u8;
            }
        }
        let h = inner.put(&payload);
        let counted = CountingStore::new(inner);
        // Only 2 pages of cache capacity.
        let cache = PageCache::new(counted.clone(), 2);
        // Touch pages 0, 1, 2, 0 in order — page 0's entry is
        // evicted by page 2, so the second read of page 0 must
        // re-fetch.
        let _ = cache.read(&h, payload.len() as u64, 0, 16).unwrap();
        let _ = cache
            .read(&h, payload.len() as u64, u64::from(PAGE_SIZE), 16)
            .unwrap();
        let _ = cache
            .read(&h, payload.len() as u64, 2 * u64::from(PAGE_SIZE), 16)
            .unwrap();
        let _ = cache.read(&h, payload.len() as u64, 0, 16).unwrap();
        let snap = counted.snapshot();
        assert_eq!(snap.range_calls, 4);
    }
}
