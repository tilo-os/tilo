//! The content plane: a content-addressed blob store with an in-memory cache.
//!
//! File bytes are iroh-blobs blobs addressed by blake3 hash; the namespace
//! CRDT only ever holds those hashes. Backing store is an iroh-blobs
//! `FsStore` rooted at `--cache-dir` (survives restarts, keeps the bootc root
//! read-only), with a `moka` cache in front for hot range reads. Tests use `MemStore`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh_blobs::api::Store;
use iroh_blobs::provider::events::EventSender;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::store::{GcConfig, ProtectOutcome};
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
    /// Serialises blob publication with selective GC.  An add is protected by
    /// its temporary tag until it completes, but without this gate it could
    /// become untagged between an eviction's mark and sweep.
    mutation: tokio::sync::Mutex<()>,
    gc_live: Arc<RwLock<Option<HashSet<Hash>>>>,
}

impl ContentStore {
    /// Open the persistent store rooted at `cache_dir` (created if missing).
    pub async fn open(cache_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = cache_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let gc_live = Arc::new(RwLock::new(None));
        let mut options = iroh_blobs::store::fs::options::Options::new(dir);
        options.gc = Some(eviction_gc(gc_live.clone()));
        let store = FsStore::load_with_opts(dir.join("blobs.db"), options).await?;
        Ok(Self {
            backend: Backend::Fs(store),
            cache: Cache::new(2048),
            mutation: tokio::sync::Mutex::new(()),
            gc_live,
        })
    }

    /// An ephemeral in-memory store (tests, and as a fallback).
    pub fn memory() -> Self {
        let gc_live = Arc::new(RwLock::new(None));
        let options = iroh_blobs::store::mem::Options {
            gc_config: Some(eviction_gc(gc_live.clone())),
        };
        Self {
            backend: Backend::Mem(MemStore::new_with_opts(options)),
            cache: Cache::new(2048),
            mutation: tokio::sync::Mutex::new(()),
            gc_live,
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
        let _mutation = self.mutation.lock().await;
        let data = data.into();
        let tag = self
            .store()
            .blobs()
            .add_bytes(data.clone())
            .temp_tag()
            .await?;
        let hash = tag.hash();
        self.cache.insert(hash, data);
        Ok(hash)
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
        let _mutation = self.mutation.lock().await;
        // add_bytes re-derives the hash; persisting makes future reads local.
        let _temporary = self
            .store()
            .blobs()
            .add_bytes(data.clone())
            .temp_tag()
            .await?;
        self.cache.insert(hash, data);
        Ok(())
    }

    /// Remove only the named physical blobs while preserving every other blob
    /// and all namespace metadata.  Callers must first prove that the blobs are
    /// remotely durable and are not shared by a file that must stay local.
    ///
    /// The mutation gate closes the otherwise subtle race where a concurrent
    /// add loses its temporary tag after the live set has been enumerated but
    /// before GC sweeps.
    pub async fn evict_blobs(&self, selected: &HashSet<Hash>) -> Result<u64> {
        if selected.is_empty() {
            return Ok(0);
        }
        let _mutation = self.mutation.lock().await;
        let before = self.store().blobs().list().hashes().await?;
        let before_count = before.len();
        let live: HashSet<Hash> = before
            .into_iter()
            .filter(|hash| !selected.contains(hash))
            .collect();
        for hash in selected {
            self.cache.invalidate(hash);
        }
        // `add_bytes().await` deliberately creates a persistent anonymous tag.
        // Remove only tags whose value is selected; GC will continue to honour
        // every tag outside the selection and the explicit complete live set.
        let mut tags = self.store().tags().list().await?;
        let mut selected_tags = Vec::new();
        while let Some(info) = tags.next().await {
            let info = info?;
            if selected.contains(&info.hash) {
                selected_tags.push(info.name);
            }
        }
        for name in selected_tags {
            self.store().tags().delete(name).await?;
        }
        *self.gc_live.write().unwrap() = Some(live);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let mut remaining = false;
            for hash in selected {
                if self.store().blobs().has(*hash).await.unwrap_or(true) {
                    remaining = true;
                    break;
                }
            }
            if !remaining {
                *self.gc_live.write().unwrap() = None;
                let after_count = self.store().blobs().list().hashes().await?.len();
                return Ok(before_count.saturating_sub(after_count) as u64);
            }
            if tokio::time::Instant::now() >= deadline {
                *self.gc_live.write().unwrap() = None;
                // A large store may still be sweeping. Report only completed
                // removals; the sweep's immutable live set can still remove
                // selected blobs, but can never expand its target set.
                let after_count = self.store().blobs().list().hashes().await?.len();
                return Ok(before_count.saturating_sub(after_count) as u64);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    pub async fn has(&self, hash: Hash) -> bool {
        if self.cache.contains_key(&hash) {
            return true;
        }
        self.store().blobs().has(hash).await.unwrap_or(false)
    }
}

fn eviction_gc(live: Arc<RwLock<Option<HashSet<Hash>>>>) -> GcConfig {
    GcConfig {
        interval: std::time::Duration::from_millis(50),
        add_protected: Some(Arc::new(move |protected| {
            match live.read().unwrap().clone() {
                Some(snapshot) => {
                    protected.extend(snapshot);
                    Box::pin(async { ProtectOutcome::Continue })
                }
                None => Box::pin(async { ProtectOutcome::Abort }),
            }
        })),
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

    #[tokio::test]
    async fn selective_eviction_preserves_every_unselected_blob() {
        let store = ContentStore::memory();
        let remove = store
            .ingest(Bytes::from_static(b"remote clean copy"))
            .await
            .unwrap();
        let keep = store
            .ingest(Bytes::from_static(b"dirty or pinned copy"))
            .await
            .unwrap();
        store.evict_blobs(&HashSet::from([remove])).await.unwrap();
        assert!(!store.has(remove).await);
        assert!(store.has(keep).await);
    }

    #[tokio::test]
    async fn persistent_store_selective_eviction_preserves_unselected_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContentStore::open(dir.path()).await.unwrap();
        let remove = store
            .ingest(Bytes::from_static(b"remove from disk"))
            .await
            .unwrap();
        let keep = store
            .ingest(Bytes::from_static(b"keep on disk"))
            .await
            .unwrap();
        store.evict_blobs(&HashSet::from([remove])).await.unwrap();
        assert!(!store.has(remove).await);
        assert!(store.has(keep).await);
    }

    #[tokio::test]
    async fn add_cannot_fall_between_eviction_mark_and_sweep() {
        let store = Arc::new(ContentStore::memory());
        let remove = store.ingest(Bytes::from_static(b"remove")).await.unwrap();
        let add_store = store.clone();
        let adding = tokio::spawn(async move {
            add_store
                .ingest(Bytes::from_static(b"concurrent add"))
                .await
                .unwrap()
        });
        store.evict_blobs(&HashSet::from([remove])).await.unwrap();
        let added = adding.await.unwrap();
        assert!(
            store.has(added).await,
            "a concurrent publication must survive GC"
        );
    }
}
