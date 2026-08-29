//! The content plane: a content-addressed blob store with an in-memory cache.
//!
//! File bytes are iroh-blobs blobs addressed by blake3 hash; the namespace
//! CRDT only ever holds those hashes. Backing store is an iroh-blobs
//! `FsStore` rooted at `--cache-dir` (survives restarts, keeps the bootc root
//! read-only), with a `moka` cache in front for hot range reads. Tests use `MemStore`.

use std::path::Path;

use anyhow::Result;
use bytes::Bytes;
use iroh_blobs::api::Store;
use iroh_blobs::provider::events::EventSender;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::{BlobsProtocol, Hash};
use moka::sync::Cache;

/// Either an on-disk or in-memory iroh-blobs store. Both `Deref` to `Store`.
enum Backend {
    Mem(MemStore),
    Fs(FsStore),
}

pub struct ContentStore {
    backend: Backend,
    cache: Cache<Hash, Bytes>,
}

impl ContentStore {
    /// Open the persistent store rooted at `cache_dir` (created if missing).
    pub async fn open(cache_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = cache_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let store = FsStore::load(dir).await?;
        Ok(Self {
            backend: Backend::Fs(store),
            cache: Cache::new(2048),
        })
    }

    /// An ephemeral in-memory store (tests, and as a fallback).
    pub fn memory() -> Self {
        Self {
            backend: Backend::Mem(MemStore::new()),
            cache: Cache::new(2048),
        }
    }

    pub fn store(&self) -> &Store {
        match &self.backend {
            Backend::Mem(m) => m,
            Backend::Fs(f) => f,
        }
    }

    /// Build a `BlobsProtocol` handler backed by this store (for the router).
    pub fn blobs_protocol(&self, events: Option<EventSender>) -> BlobsProtocol {
        BlobsProtocol::new(self.store(), events)
    }

    /// Ingest bytes and return their blake3 content hash. Warms the cache.
    pub async fn ingest(&self, data: impl Into<Bytes>) -> Result<Hash> {
        let data = data.into();
        let tag = self.store().blobs().add_bytes(data.clone()).await?;
        self.cache.insert(tag.hash, data);
        Ok(tag.hash)
    }

    /// Content-defined-chunk `data`, ingest each chunk (deduped) + a manifest
    /// describing them. Returns the manifest hash (`None` if empty); editing part of a large file only stores the changed chunks.
    pub async fn write_content(&self, data: &[u8]) -> Result<Option<Hash>> {
        if data.is_empty() {
            return Ok(None);
        }
        let mut chunks = Vec::new();
        for chunk in crate::content::split(data) {
            let h = self.ingest(Bytes::copy_from_slice(chunk)).await?;
            chunks.push(crate::content::ChunkRef {
                hash: *h.as_bytes(),
                len: chunk.len() as u32,
            });
        }
        let manifest = crate::content::Manifest { chunks };
        let mh = self.ingest(Bytes::from(manifest.encode())).await?;
        Ok(Some(mh))
    }

    /// Locally-available bytes for `hash` (cache → store), or `None` if absent.
    pub async fn local(&self, hash: Hash) -> Option<Bytes> {
        if let Some(b) = self.cache.get(&hash) {
            return Some(b);
        }
        match self.store().blobs().get_bytes(hash).await {
            Ok(b) => {
                self.cache.insert(hash, b.clone());
                Some(b)
            }
            Err(_) => None,
        }
    }

    /// Persist + cache bytes that were fetched and verified from a peer.
    pub async fn store_fetched(&self, hash: Hash, data: Bytes) -> Result<()> {
        // add_bytes re-derives the hash; persisting makes future reads local.
        let _ = self.store().blobs().add_bytes(data.clone()).await?;
        self.cache.insert(hash, data);
        Ok(())
    }

    pub async fn has(&self, hash: Hash) -> bool {
        if self.cache.contains_key(&hash) {
            return true;
        }
        self.store().blobs().has(hash).await.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ingest_roundtrips_locally() {
        let store = ContentStore::memory();
        let data = Bytes::from_static(b"the quick brown fox");
        let hash = store.ingest(data.clone()).await.unwrap();

        assert!(store.has(hash).await);
        let got = store.local(hash).await.expect("bytes present locally");
        assert_eq!(got, data);
    }

    #[tokio::test]
    async fn missing_hash_is_none() {
        let store = ContentStore::memory();
        let absent = Hash::from([42u8; 32]);
        assert!(!store.has(absent).await);
        assert!(store.local(absent).await.is_none());
    }

    #[tokio::test]
    async fn identical_bytes_share_a_hash() {
        let store = ContentStore::memory();
        let a = store.ingest(Bytes::from_static(b"dedup me")).await.unwrap();
        let b = store.ingest(Bytes::from_static(b"dedup me")).await.unwrap();
        assert_eq!(a, b, "content addressing: same bytes -> same hash");
    }
}
