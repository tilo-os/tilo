//! The content plane: a content-addressed blob store with an in-memory cache.
//!
//! File bytes are iroh-blobs blobs addressed by blake3 hash; the namespace
//! CRDT only ever holds those hashes. Backing store is an iroh-blobs
//! `FsStore` rooted at `--cache-dir` (survives restarts, keeps the bootc root
//! read-only), with a `moka` cache in front for hot range reads. Tests use `MemStore`.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh_blobs::api::Store;
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::provider::events::EventSender;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::store::{GcConfig, ProtectOutcome};
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use moka::sync::Cache;

use crate::availability::{AvailabilityRegistry, EvictionRequirements};
use crate::eviction::{
    BackendGcAuthorizationError, BlobExtent, EvictionCatalog, EvictionObservation, EvictionPlan,
    EvictionQueue, EvictionRequestId, ObservedBlobState, VersionPayload,
};
use crate::namespace::{LiveRecordBlobRoots, LiveRecordBlobScanError, Namespace};

pub const MAX_BLOB_LOOKUP_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BLOB_LOOKUP_CHUNKS_PER_RECORD: usize = 100_000;

const OWNED_BLOB_TAG_PREFIX: &str = "qos/blob/v1/";
const OWNED_TAG_MIGRATION_MARKER: &str = ".qos-owned-blob-tags-v1";
const OWNED_TAG_MIGRATION_MARKER_CONTENTS: &[u8] = b"qos-owned-blob-tags-v1\n";

/// Persistent stores sweep unreferenced blobs periodically. Explicit eviction
/// does not wait for this interval; callers observe the queue to learn when a
/// sweep has actually removed every requested hash.
pub const DEFAULT_BLOB_GC_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobRecordLookupLimits {
    pub max_live_records: usize,
    pub max_manifest_bytes: usize,
    pub max_chunks_per_record: usize,
}

impl Default for BlobRecordLookupLimits {
    fn default() -> Self {
        Self {
            max_live_records: 16_384,
            max_manifest_bytes: 2 * 1024 * 1024,
            max_chunks_per_record: 50_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobRecordLookupError {
    #[error("invalid bounded blob-record lookup limits")]
    InvalidLimits,
    #[error(transparent)]
    Namespace(#[from] LiveRecordBlobScanError),
    #[error("live record {record_id:?} manifest {manifest} is not locally available")]
    ManifestUnavailable {
        record_id: collab_types::FileRecordId,
        manifest: Hash,
    },
    #[error("live record {record_id:?} manifest exceeds {limit} encoded bytes")]
    ManifestTooLarge {
        record_id: collab_types::FileRecordId,
        limit: usize,
    },
    #[error("live record {record_id:?} has a malformed manifest")]
    MalformedManifest {
        record_id: collab_types::FileRecordId,
    },
    #[error("live record {record_id:?} manifest exceeds {limit} chunks")]
    TooManyChunks {
        record_id: collab_types::FileRecordId,
        limit: usize,
    },
}

/// Explicit reclamation errors. iroh-blobs intentionally exposes deletion
/// only through GC, so an unqueued plan remains insufficient authority.
#[derive(Debug, thiserror::Error)]
pub enum StoreEvictionError {
    #[error(
        "a dry-run plan cannot delete blobs; enqueue it and use the freshly authorized GC bridge"
    )]
    ExplicitDeletionUnsupported,
    #[error(transparent)]
    Authorization(#[from] BackendGcAuthorizationError),
    #[error("reading owned blob tag {tag} failed: {message}")]
    ReadOwnedTag { tag: String, message: String },
    #[error("owned blob tag {tag} points at a different blob or format")]
    InvalidOwnedTag { tag: String },
    #[error("releasing owned blob tag {tag} failed: {message}")]
    ReleaseOwnedTag { tag: String, message: String },
    #[error("making released blob tags durable failed: {0}")]
    Sync(String),
}

/// Durable handoff from the eviction queue to the configured background GC.
/// This is deliberately not a deletion receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendGcRelease {
    pub id: EvictionRequestId,
    pub authorized_hashes: Vec<Hash>,
    pub released_owned_tags: u64,
}

/// Either an on-disk or in-memory iroh-blobs store. Both `Deref` to `Store`.
enum Backend {
    Mem(MemStore),
    Fs(FsStore),
}

pub struct ContentStore {
    backend: Backend,
    cache: Cache<Hash, Bytes>,
    /// Serializes owned-tag installation and release. A concurrent ingest of
    /// the same hash therefore wins either wholly before or wholly after an
    /// eviction handoff, never between tag inspection and durable release.
    owned_tags: tokio::sync::Mutex<()>,
    /// Serializes publication with the legacy selective-GC API.
    mutation: tokio::sync::Mutex<()>,
    /// Exact live set supplied only for an authorized selective sweep.
    gc_live: Arc<RwLock<Option<HashSet<Hash>>>>,
}

impl ContentStore {
    /// Open the persistent store rooted at `cache_dir` (created if missing).
    ///
    /// # Errors
    ///
    /// Returns an error if `cache_dir` cannot be created or the on-disk blob store fails to load.
    pub async fn open(cache_dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_gc_interval(cache_dir, DEFAULT_BLOB_GC_INTERVAL).await
    }

    /// Open a persistent store with background garbage collection enabled.
    ///
    /// GC starts in an aborting state so the first sweep cannot race legacy
    /// tag migration. Only a fully successful migration enables sweeping.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, directory creation, store loading,
    /// or legacy-tag migration fails.
    pub async fn open_with_gc_interval(
        cache_dir: impl AsRef<Path>,
        gc_interval: Duration,
    ) -> Result<Self> {
        anyhow::ensure!(!gc_interval.is_zero(), "blob GC interval must be non-zero");
        let dir = cache_dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let gc_ready = Arc::new(AtomicBool::new(false));
        let ready = Arc::clone(&gc_ready);
        let gc_live = Arc::new(RwLock::new(None));
        let live = Arc::clone(&gc_live);
        let mut options = iroh_blobs::store::fs::options::Options::new(dir);
        options.gc = Some(GcConfig {
            // Selective eviction is an explicit user action and must not wait
            // for the ordinary maintenance cadence. Unscoped passes abort in
            // the callback below, so a short poll is cheap and cannot collect
            // anything until an exact authorized live set is installed.
            interval: gc_interval.min(Duration::from_millis(50)),
            add_protected: Some(Arc::new(move |live_set: &mut HashSet<Hash>| {
                if !ready.load(Ordering::Acquire) {
                    return Box::pin(async { ProtectOutcome::Abort });
                }
                let snapshot = live
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                match snapshot {
                    Some(snapshot) => {
                        live_set.extend(snapshot);
                        Box::pin(async { ProtectOutcome::Continue })
                    }
                    None => Box::pin(async { ProtectOutcome::Abort }),
                }
            })),
        });
        let store = FsStore::load_with_opts(dir.join("blobs.db"), options).await?;
        migrate_legacy_auto_tags(&store, dir).await?;
        gc_ready.store(true, Ordering::Release);
        Ok(Self {
            backend: Backend::Fs(store),
            cache: Cache::new(2048),
            owned_tags: tokio::sync::Mutex::new(()),
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
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc_live,
        }
    }

    #[cfg(test)]
    fn memory_with_gc_interval(gc_interval: Duration) -> Self {
        Self {
            backend: Backend::Mem(MemStore::new_with_opts(iroh_blobs::store::mem::Options {
                gc_config: Some(GcConfig {
                    interval: gc_interval,
                    add_protected: None,
                }),
            })),
            cache: Cache::new(2048),
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc_live: Arc::new(RwLock::new(None)),
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying blob store fails to accept the data.
    pub async fn ingest(&self, data: impl Into<Bytes>) -> Result<Hash> {
        let _mutation = self.mutation.lock().await;
        let data = data.into();
        self.persist_owned(data).await
    }

    /// Content-defined-chunk `data`, ingest each chunk (deduped) + a manifest
    /// describing them. Returns the manifest hash (`None` if empty); editing part of a large file only stores the changed chunks.
    ///
    /// # Errors
    ///
    /// Returns an error if a chunk or the manifest fails to ingest, or the manifest fails to serialize.
    pub async fn write_content(&self, data: &[u8]) -> Result<Option<Hash>> {
        if data.is_empty() {
            return Ok(None);
        }
        let chunk_bytes = crate::content::split(data)
            .into_iter()
            .map(Bytes::copy_from_slice)
            .collect::<Vec<_>>();
        let chunk_hashes = self.persist_owned_batch(chunk_bytes.clone()).await?;
        let chunks = chunk_hashes
            .into_iter()
            .zip(chunk_bytes)
            .map(|(hash, bytes)| crate::content::ChunkRef {
                hash: *hash.as_bytes(),
                len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            })
            .collect();
        let manifest = crate::content::Manifest { chunks };
        let mh = self.ingest(Bytes::from(manifest.encode()?)).await?;
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying blob store fails to accept the data.
    pub async fn store_fetched(&self, hash: Hash, data: Bytes) -> Result<()> {
        let actual = Hash::new(&data);
        anyhow::ensure!(
            actual == hash,
            "fetched blob hash mismatch: expected {hash}, got {actual}"
        );
        // The temporary tag closes the window between import and installing
        // the deterministic service-owned tag. Dropping it afterwards leaves
        // no anonymous `auto-*` tag behind.
        let actual = self.persist_owned(data).await?;
        debug_assert_eq!(actual, hash);
        Ok(())
    }

    /// Remove only selected physical blobs while preserving every other blob.
    ///
    /// # Errors
    ///
    /// Returns an error when backend enumeration, tagging, GC, or verification
    /// fails.
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
        *self
            .gc_live
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(live);
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_secs(3))
            .unwrap_or_else(tokio::time::Instant::now);
        loop {
            let mut remaining = false;
            for hash in selected {
                let remaining_time =
                    deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining_time.is_zero() {
                    *self
                        .gc_live
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    anyhow::bail!("selected blobs remained after eviction deadline");
                }
                // Backends may stall a `has` query during a sweep. Bound the
                // query itself so readiness cannot remain blocked forever or
                // claim a completed prune without a physical verification.
                let present = tokio::time::timeout(remaining_time, self.store().blobs().has(*hash))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(true);
                if present {
                    remaining = true;
                    break;
                }
            }
            if !remaining {
                *self
                    .gc_live
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                let after_count = self.store().blobs().list().hashes().await?.len();
                return Ok(before_count.saturating_sub(after_count) as u64);
            }
            if tokio::time::Instant::now() >= deadline {
                *self
                    .gc_live
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                anyhow::bail!("selected blobs remained after eviction deadline");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn persist_owned(&self, data: Bytes) -> Result<Hash> {
        self.persist_owned_batch(vec![data])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("owned blob batch unexpectedly returned no hash"))
    }

    async fn persist_owned_batch(&self, data: Vec<Bytes>) -> Result<Vec<Hash>> {
        let _owned_tags = self.owned_tags.lock().await;
        let mut protected = Vec::with_capacity(data.len());
        for bytes in data {
            let temporary = self
                .store()
                .blobs()
                .add_bytes(bytes.clone())
                .temp_tag()
                .await?;
            let hash = temporary.hash();
            self.store()
                .tags()
                .set(
                    owned_blob_tag(hash, temporary.format()),
                    temporary.hash_and_format(),
                )
                .await?;
            protected.push((temporary, bytes));
        }
        // `FsStore` explicitly does not promise that dropping it flushes recent
        // writes. Make the named protection durable before releasing the
        // temporary protection. This is a no-op-compatible boundary for the
        // in-memory backend.
        self.store().sync_db().await?;
        let hashes = protected
            .iter()
            .map(|(temporary, _)| temporary.hash())
            .collect::<Vec<_>>();
        for (temporary, bytes) in protected {
            self.cache.insert(temporary.hash(), bytes);
            drop(temporary);
        }
        Ok(hashes)
    }

    pub async fn has(&self, hash: Hash) -> bool {
        if self.cache.contains_key(&hash) {
            return true;
        }
        self.store().blobs().has(hash).await.unwrap_or(false)
    }

    /// Resolve a blob to every exact live stable record that currently
    /// references it as a manifest root, manifest chunk, or thumbnail.
    ///
    /// The lookup is bounded and fail-closed. It never returns paths or other
    /// records' metadata, and an unavailable/malformed live manifest prevents
    /// a partial authorization result.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid limits or an incomplete/malformed bounded
    /// live record graph.
    pub async fn live_record_ids_for_blob(
        &self,
        namespace: &Namespace,
        requested: Hash,
        limits: BlobRecordLookupLimits,
    ) -> std::result::Result<Vec<collab_types::FileRecordId>, BlobRecordLookupError> {
        if limits.max_manifest_bytes == 0
            || limits.max_manifest_bytes > MAX_BLOB_LOOKUP_MANIFEST_BYTES
            || limits.max_chunks_per_record == 0
            || limits.max_chunks_per_record > MAX_BLOB_LOOKUP_CHUNKS_PER_RECORD
        {
            return Err(BlobRecordLookupError::InvalidLimits);
        }
        let roots = namespace.live_record_blob_roots(limits.max_live_records)?;
        self.live_record_ids_for_blob_roots(roots, requested, limits)
            .await
    }

    /// Variant for async protocol handlers: snapshot the minimal roots under
    /// the namespace lock, release it, then perform backend reads here.
    pub(crate) async fn live_record_ids_for_blob_roots(
        &self,
        roots: Vec<LiveRecordBlobRoots>,
        requested: Hash,
        limits: BlobRecordLookupLimits,
    ) -> std::result::Result<Vec<collab_types::FileRecordId>, BlobRecordLookupError> {
        if limits.max_manifest_bytes == 0
            || limits.max_manifest_bytes > MAX_BLOB_LOOKUP_MANIFEST_BYTES
            || limits.max_chunks_per_record == 0
            || limits.max_chunks_per_record > MAX_BLOB_LOOKUP_CHUNKS_PER_RECORD
        {
            return Err(BlobRecordLookupError::InvalidLimits);
        }
        let mut records = std::collections::BTreeSet::new();
        for roots in roots {
            if roots.manifest == Some(requested) || roots.thumbnail == Some(requested) {
                records.insert(roots.record_id);
            }
            let Some(manifest_hash) = roots.manifest else {
                continue;
            };
            let bytes = self.local(manifest_hash).await.ok_or(
                BlobRecordLookupError::ManifestUnavailable {
                    record_id: roots.record_id,
                    manifest: manifest_hash,
                },
            )?;
            if bytes.len() > limits.max_manifest_bytes {
                return Err(BlobRecordLookupError::ManifestTooLarge {
                    record_id: roots.record_id,
                    limit: limits.max_manifest_bytes,
                });
            }
            let manifest = crate::content::Manifest::decode(&bytes).map_err(|_| {
                BlobRecordLookupError::MalformedManifest {
                    record_id: roots.record_id,
                }
            })?;
            if manifest.chunks.len() > limits.max_chunks_per_record {
                return Err(BlobRecordLookupError::TooManyChunks {
                    record_id: roots.record_id,
                    limit: limits.max_chunks_per_record,
                });
            }
            if manifest
                .chunks
                .iter()
                .any(|chunk| Hash::from(chunk.hash) == requested)
            {
                records.insert(roots.record_id);
            }
        }
        Ok(records.into_iter().collect())
    }

    /// Inspect the exact locally resident graph for a version manifest.
    ///
    /// The manifest must itself be complete and decodable; chunk accounting is
    /// read from the backend rather than inferred from namespace metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the manifest or backend status cannot be read or
    /// decoded.
    pub async fn inspect_version_payload(
        &self,
        record_id: collab_types::FileRecordId,
        version_id: collab_types::FileVersionId,
        manifest_hash: Hash,
    ) -> Result<VersionPayload> {
        let manifest_bytes = self
            .local(manifest_hash)
            .await
            .ok_or_else(|| anyhow::anyhow!("version manifest {manifest_hash} is not resident"))?;
        let manifest = crate::content::Manifest::decode(&manifest_bytes)?;
        let mut chunks = Vec::with_capacity(manifest.chunks.len());
        for chunk in manifest.chunks {
            let hash = Hash::from(chunk.hash);
            let (locally_present, resident_bytes) = match self.store().blobs().status(hash).await? {
                BlobStatus::Complete { size } => (true, Some(size)),
                BlobStatus::Partial { size } => (true, size),
                BlobStatus::NotFound => (false, None),
            };
            chunks.push(BlobExtent {
                hash,
                locally_present,
                resident_bytes,
            });
        }
        Ok(VersionPayload {
            record_id,
            version_id,
            manifest: BlobExtent {
                hash: manifest_hash,
                locally_present: true,
                resident_bytes: Some(u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX)),
            },
            chunks,
        })
    }

    /// Prove that an exact signed revision's manifest and every declared
    /// content chunk are complete in the persistent backend. Hot cache entries
    /// and partial blobs never satisfy this durability boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision is invalid or any declared payload is
    /// missing, partial, malformed, or has the wrong length.
    pub async fn verify_durable_revision(
        &self,
        revision: &collab_types::ContentRevisionV1,
    ) -> Result<()> {
        revision.validate()?;
        let Some(content_hash) = revision.content_hash else {
            anyhow::ensure!(
                revision.byte_len == 0,
                "hashless revision declares content bytes"
            );
            return Ok(());
        };
        let manifest_hash = Hash::from(content_hash.0);
        let manifest_size = match self.store().blobs().status(manifest_hash).await? {
            BlobStatus::Complete { size } => size,
            BlobStatus::Partial { .. } | BlobStatus::NotFound => {
                anyhow::bail!("version manifest is not durably complete")
            }
        };
        let manifest_bytes = self
            .store()
            .blobs()
            .get_bytes(manifest_hash)
            .await
            .map_err(|error| anyhow::anyhow!("reading durable manifest failed: {error}"))?;
        anyhow::ensure!(
            manifest_bytes.len() as u64 == manifest_size,
            "durable manifest size changed during verification"
        );
        let manifest = crate::content::Manifest::decode(&manifest_bytes)?;
        anyhow::ensure!(
            manifest.total_len() == revision.byte_len,
            "manifest length does not match signed revision"
        );
        for chunk in manifest.chunks {
            let hash = Hash::from(chunk.hash);
            match self.store().blobs().status(hash).await? {
                BlobStatus::Complete { size } if size == u64::from(chunk.len) => {}
                BlobStatus::Complete { .. } => anyhow::bail!("durable chunk length mismatch"),
                BlobStatus::Partial { .. } | BlobStatus::NotFound => {
                    anyhow::bail!("version chunk is not durably complete")
                }
            }
        }
        Ok(())
    }

    /// Refuse to execute a bare, potentially stale dry-run plan.
    ///
    /// Use [`EvictionQueue::enqueue`] followed by
    /// [`Self::release_eviction_to_backend_gc`]. Keeping this API fail-closed
    /// prevents an enqueue-time or ad-hoc plan from becoming stale deletion
    /// authority.
    ///
    /// # Errors
    ///
    /// Always returns [`StoreEvictionError::ExplicitDeletionUnsupported`].
    pub fn execute_eviction(
        &self,
        _plan: &EvictionPlan,
    ) -> std::result::Result<(), StoreEvictionError> {
        Err(StoreEvictionError::ExplicitDeletionUnsupported)
    }

    /// Freshly authorize a queued request and make exactly its approved blobs
    /// collectible by releasing Quickdesktop-owned named tags.
    ///
    /// The caller must keep `registry` and `catalog` stable for this complete
    /// call, as required by [`EvictionQueue::authorize_backend_gc`]. Other
    /// applications' tags and temporary transfer tags are never touched. A
    /// successful result proves only a durable GC handoff; use
    /// [`Self::observe_eviction_request`] for exact completion.
    ///
    /// # Errors
    ///
    /// Returns an error when fresh authorization, tag release, backend GC, or
    /// durable observation fails.
    pub async fn release_eviction_to_backend_gc(
        &self,
        queue: &EvictionQueue,
        id: EvictionRequestId,
        registry: &AvailabilityRegistry,
        catalog: &EvictionCatalog,
        now_ms: u64,
        requirements: EvictionRequirements,
    ) -> std::result::Result<BackendGcRelease, StoreEvictionError> {
        let _owned_tags = self.owned_tags.lock().await;
        let hashes = queue.authorize_backend_gc(id, registry, catalog, now_ms, requirements)?;

        // All content written by this service is currently Raw, but checking
        // both deterministic namespaces makes the ownership contract complete
        // and keeps a future HashSeq writer safe without a tag scan.
        let mut tags = Vec::with_capacity(hashes.len().saturating_mul(2));
        for &hash in &hashes {
            for format in [BlobFormat::Raw, BlobFormat::HashSeq] {
                let tag = owned_blob_tag(hash, format);
                let info = self.store().tags().get(&tag).await.map_err(|error| {
                    StoreEvictionError::ReadOwnedTag {
                        tag: tag.clone(),
                        message: error.to_string(),
                    }
                })?;
                if let Some(info) = info {
                    if info.hash != hash || info.format != format {
                        return Err(StoreEvictionError::InvalidOwnedTag { tag });
                    }
                    tags.push(tag);
                }
            }
        }

        let mut released_owned_tags = 0u64;
        for tag in tags {
            released_owned_tags = released_owned_tags.saturating_add(
                self.store().tags().delete(&tag).await.map_err(|error| {
                    StoreEvictionError::ReleaseOwnedTag {
                        tag,
                        message: error.to_string(),
                    }
                })?,
            );
        }
        self.store()
            .sync_db()
            .await
            .map_err(|error| StoreEvictionError::Sync(error.to_string()))?;

        // Main's store backend now aborts every unscoped background sweep.
        // Supply the freshly authorized exact set to its selective-GC bridge
        // before returning the durable handoff. Observation below remains the
        // authority for completion: a timed-out or partial backend sweep is
        // still reported as present, never as a deletion receipt.
        let mut selected: HashSet<Hash> = hashes.iter().copied().collect();
        // A remaining tag is foreign or temporary authority. Never make that
        // blob collectible: the release above removes only our deterministic
        // tags, and selective GC must preserve every other owner's reference.
        let mut remaining_tags = self.store().tags().list().await.map_err(|error| {
            StoreEvictionError::Sync(format!("listing tags before selective GC failed: {error}"))
        })?;
        while let Some(info) = remaining_tags.next().await {
            let info = info.map_err(|error| {
                StoreEvictionError::Sync(format!(
                    "reading tags before selective GC failed: {error}"
                ))
            })?;
            selected.remove(&info.hash);
        }
        self.evict_blobs(&selected)
            .await
            .map_err(|error| StoreEvictionError::Sync(error.to_string()))?;

        Ok(BackendGcRelease {
            id,
            authorized_hashes: hashes,
            released_owned_tags,
        })
    }

    /// Observe a durable eviction request against one coherent backend status
    /// sample. Completion is persisted only when every requested hash is
    /// `NotFound`; backend errors leave the queue unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error when the request is unknown or backend status/queue
    /// persistence fails.
    pub async fn observe_eviction_request(
        &self,
        queue: &mut EvictionQueue,
        id: EvictionRequestId,
        observed_at_ms: u64,
    ) -> Result<EvictionObservation> {
        let hashes = queue
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown eviction request {id:?}"))?
            .requested_hashes
            .clone();
        let mut states = Vec::with_capacity(hashes.len());
        for hash in hashes {
            let state = match self.store().blobs().status(hash).await? {
                BlobStatus::NotFound => ObservedBlobState::Absent,
                BlobStatus::Complete { size } => ObservedBlobState::Present {
                    resident_bytes: Some(size),
                },
                BlobStatus::Partial { size } => ObservedBlobState::Present {
                    resident_bytes: size,
                },
            };
            states.push((hash, state));
        }
        let observation = queue.record_observation(id, states, observed_at_ms)?;
        if matches!(
            observation.state,
            crate::eviction::EvictionRequestState::Completed { .. }
        ) {
            // GC only removes backend persistence. Drop any hot in-memory copy
            // before exposing completion to local readers.
            if let Some(request) = queue.get(id) {
                for hash in &request.requested_hashes {
                    self.cache.invalidate(hash);
                }
            }
        }
        Ok(observation)
    }
}

fn owned_blob_tag(hash: Hash, format: BlobFormat) -> String {
    let format = match format {
        BlobFormat::Raw => "raw",
        BlobFormat::HashSeq => "hash-seq",
    };
    format!("{OWNED_BLOB_TAG_PREFIX}{format}/{hash}")
}

/// Upgrade tags created by the prototype's default `add_bytes().await`
/// behavior. Promotion is ordered so a crash can only leave both tags, never
/// an unprotected blob: install the deterministic owned tag first, then remove
/// the legacy tag. The durable marker is written only after the entire scan.
/// Any corrupt marker or migration error aborts store startup.
async fn migrate_legacy_auto_tags(store: &FsStore, cache_dir: &Path) -> Result<()> {
    let marker = cache_dir.join(OWNED_TAG_MIGRATION_MARKER);
    match std::fs::read(&marker) {
        // A valid marker records the understood schema, but we still rescan.
        // That makes startup self-healing if an older binary wrote another
        // `auto-*` tag after the first migration.
        Ok(contents) if contents == OWNED_TAG_MIGRATION_MARKER_CONTENTS => {}
        Ok(_) => anyhow::bail!(
            "invalid owned blob tag migration marker at {}",
            marker.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    // Collect before mutating so the result does not depend on whether the
    // backend's range stream is a snapshot or a live cursor.
    let mut stream = store.tags().list_prefix("auto-").await?;
    let mut legacy = Vec::new();
    while let Some(info) = stream.next().await {
        legacy.push(info?);
    }
    drop(stream);

    let mut protected = Vec::with_capacity(legacy.len());
    for info in &legacy {
        let value = iroh_blobs::HashAndFormat {
            hash: info.hash,
            format: info.format,
        };
        let temporary = store.tags().temp_tag(value).await?;
        store
            .tags()
            .set(owned_blob_tag(info.hash, info.format), value)
            .await?;
        protected.push(temporary);
    }
    // Establish all owned tags durably before removing a single legacy tag.
    store.sync_db().await?;
    for info in legacy {
        store.tags().delete(info.name).await?;
    }
    store.sync_db().await?;
    drop(protected);

    atomic_file::write(&marker, OWNED_TAG_MIGRATION_MARKER_CONTENTS)?;
    Ok(())
}

fn eviction_gc(live: Arc<RwLock<Option<HashSet<Hash>>>>) -> GcConfig {
    GcConfig {
        interval: std::time::Duration::from_millis(50),
        add_protected: Some(Arc::new(move |protected| {
            let snapshot = live
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            match snapshot {
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

    async fn tag_names(store: &Store) -> Vec<Vec<u8>> {
        let mut tags = store.tags().list().await.unwrap();
        let mut names = Vec::new();
        while let Some(tag) = tags.next().await {
            names.push(tag.unwrap().name.as_ref().to_vec());
        }
        names
    }

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
    async fn ingest_and_fetch_use_only_deterministic_owned_tags() {
        let store = ContentStore::memory();
        let first = store.ingest(Bytes::from_static(b"ingested")).await.unwrap();
        let fetched_bytes = Bytes::from_static(b"fetched");
        let fetched = Hash::new(&fetched_bytes);
        store.store_fetched(fetched, fetched_bytes).await.unwrap();

        let mut names = tag_names(store.store()).await;
        names.sort();
        let mut expected = vec![
            owned_blob_tag(first, BlobFormat::Raw).into_bytes(),
            owned_blob_tag(fetched, BlobFormat::Raw).into_bytes(),
        ];
        expected.sort();
        assert_eq!(names, expected);
        assert!(
            store
                .store()
                .tags()
                .list_temp_tags()
                .await
                .unwrap()
                .next()
                .await
                .is_none(),
            "temporary protection must be released after owned-tag promotion"
        );
    }

    #[tokio::test]
    async fn fetched_hash_mismatch_does_not_persist_or_cache_bytes() {
        let store = ContentStore::memory();
        let claimed = Hash::from([9; 32]);
        assert!(
            store
                .store_fetched(claimed, Bytes::from_static(b"different"))
                .await
                .is_err()
        );
        assert!(tag_names(store.store()).await.is_empty());
        assert!(!store.has(claimed).await);
        assert!(!store.has(Hash::new(b"different")).await);
    }

    #[tokio::test]
    async fn persistent_store_migrates_auto_tags_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = Bytes::from_static(b"legacy persistent blob");
        let legacy_hash = {
            let legacy = FsStore::load(temp.path()).await.unwrap();
            let tag = legacy.blobs().add_bytes(bytes.clone()).await.unwrap();
            assert!(tag.name.as_ref().starts_with(b"auto-"));
            let hash = tag.hash;
            legacy.shutdown().await.unwrap();
            drop(legacy);
            hash
        };

        {
            let store = ContentStore::open(temp.path()).await.unwrap();
            assert_eq!(store.local(legacy_hash).await, Some(bytes.clone()));
            assert_eq!(
                tag_names(store.store()).await,
                vec![owned_blob_tag(legacy_hash, BlobFormat::Raw).into_bytes()]
            );
            assert_eq!(
                std::fs::read(temp.path().join(OWNED_TAG_MIGRATION_MARKER)).unwrap(),
                OWNED_TAG_MIGRATION_MARKER_CONTENTS
            );
            store.store().shutdown().await.unwrap();
        }

        // A valid marker is not permission to trust the tag set blindly. If an
        // older binary writes another auto tag, the next startup rescans and
        // converges it as well.
        let late_bytes = Bytes::from_static(b"written by an older binary");
        let late_hash = {
            let legacy = FsStore::load(temp.path()).await.unwrap();
            let tag = legacy.blobs().add_bytes(late_bytes.clone()).await.unwrap();
            assert!(tag.name.as_ref().starts_with(b"auto-"));
            legacy.shutdown().await.unwrap();
            tag.hash
        };

        let reopened = ContentStore::open(temp.path()).await.unwrap();
        assert_eq!(reopened.local(legacy_hash).await, Some(bytes));
        assert_eq!(reopened.local(late_hash).await, Some(late_bytes));
        let mut names = tag_names(reopened.store()).await;
        names.sort();
        let mut expected = vec![
            owned_blob_tag(legacy_hash, BlobFormat::Raw).into_bytes(),
            owned_blob_tag(late_hash, BlobFormat::Raw).into_bytes(),
        ];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[tokio::test]
    async fn corrupt_owned_tag_migration_marker_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        atomic_file::write(
            &temp.path().join(OWNED_TAG_MIGRATION_MARKER),
            b"unknown migration state\n",
        )
        .unwrap();

        let error = ContentStore::open(temp.path())
            .await
            .err()
            .expect("corrupt migration marker must reject startup");
        assert!(
            error
                .to_string()
                .contains("invalid owned blob tag migration marker")
        );
    }

    #[tokio::test]
    async fn version_inspection_uses_backend_residency_and_exact_manifest() {
        let store = ContentStore::memory();
        let data = vec![7u8; 100_000];
        let manifest_hash = store.write_content(&data).await.unwrap().unwrap();
        let payload = store
            .inspect_version_payload(
                collab_types::FileRecordId::new(),
                collab_types::FileVersionId::new(),
                manifest_hash,
            )
            .await
            .unwrap();
        assert_eq!(payload.manifest.hash, manifest_hash);
        assert!(!payload.chunks.is_empty());
        assert!(
            payload
                .chunks
                .iter()
                .all(|chunk| chunk.resident_bytes.is_some())
        );
        assert_eq!(
            payload
                .chunks
                .iter()
                .filter_map(|chunk| chunk.resident_bytes)
                .sum::<u64>(),
            data.len() as u64
        );
    }

    #[tokio::test]
    async fn durable_revision_rejects_a_complete_manifest_with_any_missing_chunk() {
        let store = ContentStore::memory();
        let missing = iroh_blobs::Hash::new(b"not persisted");
        let manifest = crate::content::Manifest {
            chunks: vec![crate::content::ChunkRef {
                hash: *missing.as_bytes(),
                len: 13,
            }],
        };
        let manifest_hash = store
            .ingest(Bytes::from(manifest.encode().unwrap()))
            .await
            .unwrap();
        let revision = collab_types::ContentRevisionV1 {
            schema_version: collab_types::CONTENT_REVISION_VERSION,
            record_id: collab_types::FileRecordId::new(),
            version_id: collab_types::FileVersionId::new(),
            content_hash: Some(collab_types::ContentHash(*manifest_hash.as_bytes())),
            byte_len: 13,
            metadata_hash: collab_types::ContentHash([1; 32]),
        };
        assert!(store.verify_durable_revision(&revision).await.is_err());
    }

    #[tokio::test]
    async fn unsupported_execution_never_claims_to_delete() {
        use crate::eviction::{EvictionMode, EvictionPlan};

        let store = ContentStore::memory();
        let hash = store.ingest(Bytes::from_static(b"keep me")).await.unwrap();
        let plan = EvictionPlan {
            record_id: collab_types::FileRecordId::new(),
            version_id: collab_types::FileVersionId::new(),
            mode: EvictionMode::DestructiveOverride,
            reclaimable: vec![BlobExtent {
                hash,
                locally_present: true,
                resident_bytes: Some(7),
            }],
            shared_and_protected: Vec::new(),
            already_absent: Vec::new(),
            reclaimable_bytes: 7,
            unaccounted_reclaimable_blobs: 0,
            protected_shared_bytes: 0,
            unaccounted_protected_shared_blobs: 0,
        };

        assert!(matches!(
            store.execute_eviction(&plan),
            Err(StoreEvictionError::ExplicitDeletionUnsupported)
        ));
        assert!(
            store.has(hash).await,
            "an unsupported action must not mutate storage"
        );
    }

    #[tokio::test]
    async fn eviction_observation_completes_only_for_backend_absence() {
        use crate::eviction::{EvictionMode, EvictionPlan, EvictionQueue, EvictionRequestState};

        let temp = tempfile::TempDir::new().unwrap();
        let store = ContentStore::memory();
        let present = store.ingest(Bytes::from_static(b"present")).await.unwrap();
        let absent = Hash::from([91; 32]);
        let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();

        let make_plan = |hash| EvictionPlan {
            record_id: collab_types::FileRecordId::new(),
            version_id: collab_types::FileVersionId::new(),
            mode: EvictionMode::Safe,
            reclaimable: vec![BlobExtent {
                hash,
                locally_present: true,
                resident_bytes: Some(7),
            }],
            shared_and_protected: Vec::new(),
            already_absent: Vec::new(),
            reclaimable_bytes: 7,
            unaccounted_reclaimable_blobs: 0,
            protected_shared_bytes: 0,
            unaccounted_protected_shared_blobs: 0,
        };

        let present_id = queue.enqueue(&make_plan(present), 1).unwrap().id;
        let present_observation = store
            .observe_eviction_request(&mut queue, present_id, 2)
            .await
            .unwrap();
        assert_eq!(
            present_observation.state,
            EvictionRequestState::AwaitingBackendGc
        );
        assert_eq!(present_observation.remaining_hashes, vec![present]);

        let absent_id = queue.enqueue(&make_plan(absent), 1).unwrap().id;
        let absent_observation = store
            .observe_eviction_request(&mut queue, absent_id, 3)
            .await
            .unwrap();
        assert_eq!(
            absent_observation.state,
            EvictionRequestState::Completed { observed_at_ms: 3 }
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Multi-phase backend eviction lifecycle scenario.
    async fn queued_eviction_releases_only_owned_target_tags_and_gc_completion_is_observed() {
        use crate::availability::{RecordAvailability, RecordKind, Residency};
        use crate::eviction::{
            EvictionCatalog, EvictionMode, EvictionQueue, EvictionRequestState, VersionPayload,
            plan_destructive_eviction,
        };

        let temp = tempfile::TempDir::new().unwrap();
        let store = ContentStore::open_with_gc_interval(
            temp.path().join("blobs"),
            Duration::from_secs(60 * 60),
        )
        .await
        .unwrap();
        // Larger than FsStore's inline threshold, so this also proves the
        // physical data file is reclaimed rather than only a metadata row.
        let target_bytes = Bytes::from(vec![0x5a; 64 * 1024]);
        let target_only = store.ingest(target_bytes.clone()).await.unwrap();
        let target_data_path = temp
            .path()
            .join("blobs/data")
            .join(format!("{}.data", target_only.to_hex()));
        assert!(target_data_path.exists());
        let shared = store.ingest(Bytes::from_static(b"shared")).await.unwrap();
        let other_only = store
            .ingest(Bytes::from_static(b"other only"))
            .await
            .unwrap();

        let target_id = collab_types::FileRecordId::new();
        let target_version = collab_types::FileVersionId::new();
        let other_id = collab_types::FileRecordId::new();
        let other_version = collab_types::FileVersionId::new();
        let mut registry =
            AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
        let mut target = RecordAvailability::new(target_id, RecordKind::File);
        target.current_version = Some(target_version);
        target.residency = Residency::Resident {
            bytes: target_bytes.len() as u64 + 6,
        };
        registry.put(target).unwrap();
        let mut other = RecordAvailability::new(other_id, RecordKind::File);
        other.current_version = Some(other_version);
        other.residency = Residency::Resident { bytes: 16 };
        registry.put(other).unwrap();
        let extent = |hash, bytes| BlobExtent {
            hash,
            locally_present: true,
            resident_bytes: Some(bytes),
        };
        let catalog = EvictionCatalog::new([
            VersionPayload {
                record_id: target_id,
                version_id: target_version,
                manifest: extent(target_only, target_bytes.len() as u64),
                chunks: vec![extent(shared, 6)],
            },
            VersionPayload {
                record_id: other_id,
                version_id: other_version,
                manifest: extent(other_only, 10),
                chunks: vec![extent(shared, 6)],
            },
        ])
        .unwrap();
        let plan = plan_destructive_eviction(
            &registry,
            &catalog,
            target_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        assert_eq!(plan.mode, EvictionMode::DestructiveOverride);
        assert_eq!(
            plan.reclaimable
                .iter()
                .map(|blob| blob.hash)
                .collect::<Vec<_>>(),
            vec![target_only]
        );
        assert_eq!(
            plan.shared_and_protected
                .iter()
                .map(|blob| blob.hash)
                .collect::<Vec<_>>(),
            vec![shared]
        );

        let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
        let id = queue.enqueue(&plan, 1).unwrap().id;
        let released = store
            .release_eviction_to_backend_gc(
                &queue,
                id,
                &registry,
                &catalog,
                2,
                EvictionRequirements::default(),
            )
            .await
            .unwrap();
        assert_eq!(released.authorized_hashes, vec![target_only]);
        assert_eq!(released.released_owned_tags, 1);

        // The tag release is a durable handoff, not process-local state. GC
        // may therefore finish it after a clean service restart.
        store.store().shutdown().await.unwrap();
        drop(store);
        let store = ContentStore::open_with_gc_interval(
            temp.path().join("blobs"),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        let mut completion = None;
        for observed_at_ms in 3..=203 {
            let observation = store
                .observe_eviction_request(&mut queue, id, observed_at_ms)
                .await
                .unwrap();
            if matches!(observation.state, EvictionRequestState::Completed { .. }) {
                completion = Some(observation);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            completion.is_some(),
            "configured GC did not sweep released blob"
        );
        assert!(!store.has(target_only).await);
        assert!(
            !target_data_path.exists(),
            "persistent GC must remove the external data file"
        );
        assert_eq!(
            store.local(shared).await,
            Some(Bytes::from_static(b"shared"))
        );
        assert_eq!(
            store.local(other_only).await,
            Some(Bytes::from_static(b"other only"))
        );
        assert!(matches!(
            EvictionQueue::open(temp.path().join("evictions.json"))
                .unwrap()
                .get(id)
                .unwrap()
                .state,
            EvictionRequestState::Completed { .. }
        ));
    }

    #[tokio::test]
    async fn backend_handoff_refuses_stale_shared_root_without_releasing_tag() {
        use crate::availability::{RecordAvailability, RecordKind, Residency};
        use crate::eviction::{
            EvictionCatalog, EvictionQueue, VersionPayload, plan_destructive_eviction,
        };

        let temp = tempfile::TempDir::new().unwrap();
        let store = ContentStore::memory_with_gc_interval(Duration::from_millis(10));
        let hash = store
            .ingest(Bytes::from_static(b"becomes shared"))
            .await
            .unwrap();
        let other_hash = store
            .ingest(Bytes::from_static(b"other root"))
            .await
            .unwrap();
        let target_id = collab_types::FileRecordId::new();
        let target_version = collab_types::FileVersionId::new();
        let other_id = collab_types::FileRecordId::new();
        let other_version = collab_types::FileVersionId::new();
        let mut registry =
            AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
        let mut target = RecordAvailability::new(target_id, RecordKind::File);
        target.current_version = Some(target_version);
        target.residency = Residency::Resident { bytes: 14 };
        registry.put(target).unwrap();
        let mut other = RecordAvailability::new(other_id, RecordKind::File);
        other.current_version = Some(other_version);
        other.residency = Residency::NotResident;
        registry.put(other.clone()).unwrap();
        let extent = |hash, bytes| BlobExtent {
            hash,
            locally_present: true,
            resident_bytes: Some(bytes),
        };
        let catalog = EvictionCatalog::new([
            VersionPayload {
                record_id: target_id,
                version_id: target_version,
                manifest: extent(hash, 14),
                chunks: Vec::new(),
            },
            VersionPayload {
                record_id: other_id,
                version_id: other_version,
                manifest: extent(other_hash, 10),
                chunks: vec![extent(hash, 14)],
            },
        ])
        .unwrap();
        let plan = plan_destructive_eviction(
            &registry,
            &catalog,
            target_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
        let id = queue.enqueue(&plan, 1).unwrap().id;

        other.residency = Residency::Resident { bytes: 24 };
        registry.put(other).unwrap();
        assert!(matches!(
            store
                .release_eviction_to_backend_gc(
                    &queue,
                    id,
                    &registry,
                    &catalog,
                    2,
                    EvictionRequirements::default(),
                )
                .await,
            Err(StoreEvictionError::Authorization(_))
        ));
        tokio::time::sleep(Duration::from_millis(35)).await;
        assert!(store.has(hash).await);
        assert!(
            store
                .store()
                .tags()
                .get(owned_blob_tag(hash, BlobFormat::Raw))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn eviction_never_releases_foreign_tags_or_claims_their_blob_was_deleted() {
        use crate::availability::{RecordAvailability, RecordKind, Residency};
        use crate::eviction::{
            EvictionCatalog, EvictionQueue, EvictionRequestState, VersionPayload,
            plan_destructive_eviction,
        };

        let temp = tempfile::TempDir::new().unwrap();
        let store = ContentStore::memory_with_gc_interval(Duration::from_millis(10));
        let hash = store
            .ingest(Bytes::from_static(b"externally protected"))
            .await
            .unwrap();
        store
            .store()
            .tags()
            .set(
                "another-application/keep",
                iroh_blobs::HashAndFormat {
                    hash,
                    format: BlobFormat::Raw,
                },
            )
            .await
            .unwrap();

        let record_id = collab_types::FileRecordId::new();
        let version_id = collab_types::FileVersionId::new();
        let mut registry =
            AvailabilityRegistry::open(temp.path().join("availability.json")).unwrap();
        let mut record = RecordAvailability::new(record_id, RecordKind::File);
        record.current_version = Some(version_id);
        record.residency = Residency::Resident { bytes: 20 };
        registry.put(record).unwrap();
        let catalog = EvictionCatalog::new([VersionPayload {
            record_id,
            version_id,
            manifest: BlobExtent {
                hash,
                locally_present: true,
                resident_bytes: Some(20),
            },
            chunks: Vec::new(),
        }])
        .unwrap();
        let plan = plan_destructive_eviction(
            &registry,
            &catalog,
            record_id,
            1,
            EvictionRequirements::default(),
        )
        .unwrap();
        let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();
        let id = queue.enqueue(&plan, 1).unwrap().id;
        let release = store
            .release_eviction_to_backend_gc(
                &queue,
                id,
                &registry,
                &catalog,
                2,
                EvictionRequirements::default(),
            )
            .await
            .unwrap();
        assert_eq!(release.released_owned_tags, 1);

        tokio::time::sleep(Duration::from_millis(35)).await;
        let observation = store
            .observe_eviction_request(&mut queue, id, 3)
            .await
            .unwrap();
        assert_eq!(observation.state, EvictionRequestState::AwaitingBackendGc);
        assert_eq!(observation.remaining_hashes, vec![hash]);
        assert!(store.has(hash).await);
        assert!(
            store
                .store()
                .tags()
                .get("another-application/keep")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .store()
                .tags()
                .get(owned_blob_tag(hash, BlobFormat::Raw))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Every reverse-lookup source is one matrix.
    async fn blob_reverse_lookup_covers_roots_chunks_thumbnails_and_live_records_only() {
        let store = ContentStore::memory();
        let shared_chunk = store.ingest(Bytes::from_static(b"shared")).await.unwrap();
        let first_only_chunk = store.ingest(Bytes::from_static(b"first")).await.unwrap();
        let second_only_chunk = store.ingest(Bytes::from_static(b"second")).await.unwrap();
        let manifest = |unique: Hash| crate::content::Manifest {
            chunks: vec![
                crate::content::ChunkRef {
                    hash: *shared_chunk.as_bytes(),
                    len: 6,
                },
                crate::content::ChunkRef {
                    hash: *unique.as_bytes(),
                    len: 6,
                },
            ],
        };
        let first_manifest = store
            .ingest(Bytes::from(manifest(first_only_chunk).encode().unwrap()))
            .await
            .unwrap();
        let second_manifest = store
            .ingest(Bytes::from(manifest(second_only_chunk).encode().unwrap()))
            .await
            .unwrap();
        let thumbnail = store
            .ingest(Bytes::from_static(b"thumbnail"))
            .await
            .unwrap();

        let mut namespace = Namespace::new("a").unwrap();
        let first = namespace
            .create_file(
                crate::namespace::ROOT_INO,
                "first",
                Some(&first_manifest),
                100_000,
                1,
            )
            .unwrap();
        let second = namespace
            .create_file(
                crate::namespace::ROOT_INO,
                "second",
                Some(&second_manifest),
                100_000,
                1,
            )
            .unwrap();
        let first_record = namespace.record_id_for(first).unwrap();
        let second_record = namespace.record_id_for(second).unwrap();
        assert!(namespace.set_thumb(first, &thumbnail));
        let limits = BlobRecordLookupLimits::default();

        assert_eq!(
            store
                .live_record_ids_for_blob(&namespace, first_manifest, limits)
                .await
                .unwrap(),
            vec![first_record]
        );
        assert_eq!(
            store
                .live_record_ids_for_blob(&namespace, shared_chunk, limits)
                .await
                .unwrap(),
            vec![first_record, second_record]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store
                .live_record_ids_for_blob(&namespace, second_only_chunk, limits)
                .await
                .unwrap(),
            vec![second_record]
        );
        assert_eq!(
            store
                .live_record_ids_for_blob(&namespace, thumbnail, limits)
                .await
                .unwrap(),
            vec![first_record]
        );
        assert!(
            store
                .live_record_ids_for_blob(&namespace, Hash::from([88; 32]), limits)
                .await
                .unwrap()
                .is_empty()
        );
        let mut tight_limits = limits;
        tight_limits.max_chunks_per_record = 1;
        assert!(matches!(
            store
                .live_record_ids_for_blob(&namespace, shared_chunk, tight_limits)
                .await,
            Err(BlobRecordLookupError::TooManyChunks { limit: 1, .. })
        ));

        assert!(namespace.remove(first));
        assert_eq!(
            store
                .live_record_ids_for_blob(&namespace, shared_chunk, limits)
                .await
                .unwrap(),
            vec![second_record]
        );
        assert_eq!(namespace.record_id_for(second), Some(second_record));
    }

    #[tokio::test]
    async fn blob_reverse_lookup_is_bounded_and_fails_closed_on_missing_manifest() {
        let store = ContentStore::memory();
        let missing = Hash::from([66; 32]);
        let mut namespace = Namespace::new("a").unwrap();
        let ino = namespace
            .create_file(
                crate::namespace::ROOT_INO,
                "projected",
                Some(&missing),
                1,
                1,
            )
            .unwrap();
        let record_id = namespace.record_id_for(ino).unwrap();
        assert!(matches!(
            store
                .live_record_ids_for_blob(
                    &namespace,
                    missing,
                    BlobRecordLookupLimits::default()
                )
                .await,
            Err(BlobRecordLookupError::ManifestUnavailable {
                record_id: found,
                manifest: found_manifest,
            }) if found == record_id && found_manifest == missing
        ));

        let limits = BlobRecordLookupLimits {
            max_live_records: 0,
            ..BlobRecordLookupLimits::default()
        };
        assert!(matches!(
            store
                .live_record_ids_for_blob(&namespace, missing, limits)
                .await,
            Err(BlobRecordLookupError::Namespace(
                LiveRecordBlobScanError::InvalidLimit
            ))
        ));
    }
}
