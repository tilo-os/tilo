//! The content plane: one content store with one hash space and two
//! backings (`docs/architecture/storage.md`, §7.4).
//!
//! - **Loose** blobs live in an iroh-blobs store (`FsStore` rooted at
//!   `--cache-dir`, `MemStore` in tests): manifests, the chunks of small
//!   files, legacy content, thumbnails, package payloads.
//! - **File-backed** content ([`FileArea`]) is everything larger than
//!   [`crate::content::FILE_BACKED_MIN`]: one sparse file per content id at
//!   the file's own offsets, filled range by range, with a chunk index so a
//!   chunk is served and deduplicated from where it lives. On a Tier 1
//!   store volume the area sits next to the rooms, so publishing a file
//!   clones its extents instead of copying bytes.
//!
//! The same area is the daemon's bounded cache: resident bytes of cached
//! entries are kept under a byte budget, least recently used first, and
//! pinned or local-only content is never evicted. A `moka` cache sits in
//! front of loose blobs for hot reads. The provider serves both backings
//! ([`ContentStore::blobs_protocol`]).

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::Result;
use bao_tree::io::mixed::EncodedItem;
use bytes::Bytes;
use futures_util::StreamExt;
use iroh_blobs::api::Store;
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt as _};
use iroh_blobs::provider::events::EventSender;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::mem::MemStore;
use iroh_blobs::store::{GcConfig, ProtectCb, ProtectOutcome};
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use moka::sync::Cache;

#[cfg(feature = "filesystem")]
use crate::availability::{AvailabilityRegistry, EvictionRequirements};
#[cfg(feature = "filesystem")]
use crate::eviction::{
    BackendGcAuthorizationError, BlobExtent, EvictionCatalog, EvictionObservation, EvictionPlan,
    EvictionQueue, EvictionRequestId, ObservedBlobState, VersionPayload,
};
#[cfg(feature = "filesystem")]
use crate::namespace::{LiveRecordBlobRoots, LiveRecordBlobScanError, Namespace};

pub mod area;
mod content;
mod files;
mod serve;

pub use area::{FileArea, Retention};
#[cfg(feature = "filesystem")]
pub(crate) use content::copy_part;

#[cfg(feature = "filesystem")]
pub const MAX_BLOB_LOOKUP_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
#[cfg(feature = "filesystem")]
pub const MAX_BLOB_LOOKUP_CHUNKS_PER_RECORD: usize = 100_000;

const OWNED_BLOB_TAG_PREFIX: &str = "tilo/blob/v1/";

/// Persistent stores sweep unreferenced blobs periodically. Explicit eviction
/// does not wait for this interval; callers observe the queue to learn when a
/// sweep has removed every requested hash.
pub const DEFAULT_BLOB_GC_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "filesystem")]
pub struct BlobRecordLookupLimits {
    pub max_live_records: usize,
    pub max_manifest_bytes: usize,
    pub max_chunks_per_record: usize,
}

#[cfg(feature = "filesystem")]
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
#[cfg(feature = "filesystem")]
pub enum BlobRecordLookupError {
    #[error("invalid bounded blob-record lookup limits")]
    InvalidLimits,
    #[error(transparent)]
    Namespace(#[from] LiveRecordBlobScanError),
    #[error("live record {record_id:?} manifest {manifest} is not locally available")]
    ManifestUnavailable {
        record_id: fs_protocol::FileRecordId,
        manifest: Hash,
    },
    #[error("live record {record_id:?} manifest exceeds {limit} encoded bytes")]
    ManifestTooLarge {
        record_id: fs_protocol::FileRecordId,
        limit: usize,
    },
    #[error("live record {record_id:?} has a malformed manifest")]
    MalformedManifest {
        record_id: fs_protocol::FileRecordId,
    },
    #[error("live record {record_id:?} manifest exceeds {limit} chunks")]
    TooManyChunks {
        record_id: fs_protocol::FileRecordId,
        limit: usize,
    },
}

/// Explicit reclamation errors. iroh-blobs intentionally exposes deletion
/// only through GC, so an unqueued plan remains insufficient authority.
#[derive(Debug, thiserror::Error)]
#[cfg(feature = "filesystem")]
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
#[cfg(feature = "filesystem")]
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
    gc: GcGate,
    /// Open deferred-durability groups, and whether a write inside one
    /// still waits for its database sync; see [`ContentStore::defer_sync`].
    deferred: std::sync::Mutex<(usize, bool)>,
    /// File-backed content and the daemon's bounded cache.
    files: Arc<FileArea>,
    /// The directory of an ephemeral store's area, removed with it.
    _scratch: Option<tempfile::TempDir>,
    /// The runtime the persistent store's actors run on (declared last, so
    /// it outlives the store handle).
    runtime: Option<StoreRuntime>,
}

/// A runtime of the content store's own. The store's actors batch their
/// database transactions on timers; on the daemon's runtime they stalled
/// whenever a worker blocked on the namespace lock while the content pruner
/// held that lock and waited for the store, and with them every timer and
/// socket of the daemon.
struct StoreRuntime(Option<tokio::runtime::Runtime>);

impl StoreRuntime {
    fn new() -> std::io::Result<Self> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("fs-content-store")
            .enable_all()
            .build()
            .map(|runtime| Self(Some(runtime)))
    }

    fn handle(&self) -> Option<&tokio::runtime::Handle> {
        self.0.as_ref().map(tokio::runtime::Runtime::handle)
    }
}

impl Drop for StoreRuntime {
    fn drop(&mut self) {
        // Dropping a runtime blocks, which an async caller must not do.
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// Budget for cached file-backed content when the caller sets none: a
/// quarter of the filesystem that holds it.
fn default_budget(dir: &Path) -> u64 {
    const FALLBACK: u64 = 8 * 1024 * 1024 * 1024;
    #[cfg(all(target_os = "linux", feature = "filesystem"))]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
            return FALLBACK;
        };
        // SAFETY: an all-zero statvfs is valid for the kernel to fill.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `st` is writable.
        if unsafe { libc::statvfs(path.as_ptr(), &raw mut st) } != 0 {
            return FALLBACK;
        }
        #[allow(
            clippy::useless_conversion,
            reason = "the statvfs field types differ between targets"
        )]
        let (blocks, fragment) = (u64::from(st.f_blocks), u64::from(st.f_frsize));
        blocks
            .saturating_mul(fragment)
            .checked_div(4)
            .filter(|b| *b > 0)
            .unwrap_or(FALLBACK)
    }
    #[cfg(not(all(target_os = "linux", feature = "filesystem")))]
    {
        let _ = dir;
        FALLBACK
    }
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

    /// [`Self::open`] with the file-backed content under `files_dir`, for
    /// example on the store volume next to the rooms.
    ///
    /// # Errors
    ///
    /// Returns an error if either directory or the stores cannot be opened.
    pub async fn open_with_files(cache_dir: impl AsRef<Path>, files_dir: &Path) -> Result<Self> {
        Self::open_inner(cache_dir.as_ref(), DEFAULT_BLOB_GC_INTERVAL, files_dir).await
    }

    /// Open a persistent store with background garbage collection enabled.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, directory creation or store
    /// loading fails.
    pub async fn open_with_gc_interval(
        cache_dir: impl AsRef<Path>,
        gc_interval: Duration,
    ) -> Result<Self> {
        let dir = cache_dir.as_ref();
        Self::open_inner(dir, gc_interval, &dir.join("files")).await
    }

    async fn open_inner(dir: &Path, gc_interval: Duration, files_dir: &Path) -> Result<Self> {
        anyhow::ensure!(!gc_interval.is_zero(), "blob GC interval must be non-zero");
        std::fs::create_dir_all(dir)?;
        std::fs::create_dir_all(files_dir)?;
        let files = Arc::new(FileArea::open(files_dir, default_budget(files_dir))?);
        let gc_ready = Arc::new(AtomicBool::new(false));
        let gc = GcGate::default();
        let mut options = iroh_blobs::store::fs::options::Options::new(dir);
        // Selective eviction is an explicit user action and must not wait
        // for the ordinary maintenance cadence. Unscoped passes wait in the
        // callback, so the short interval only separates the sweeps of one
        // authorized eviction and cannot collect anything until an exact
        // live set is installed.
        options.gc = Some(gc.config(
            gc_interval.min(Duration::from_millis(50)),
            Some(Arc::clone(&gc_ready)),
        ));
        let runtime = StoreRuntime::new()?;
        let root = dir.to_path_buf();
        let store = runtime
            .handle()
            .ok_or_else(|| anyhow::anyhow!("content store runtime is gone"))?
            .spawn(async move { FsStore::load_with_opts(root.join("blobs.db"), options).await })
            .await
            .map_err(|error| anyhow::anyhow!("opening the content store failed: {error}"))??;
        gc_ready.store(true, Ordering::Release);
        Ok(Self {
            backend: Backend::Fs(store),
            cache: Cache::new(2048),
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc,
            deferred: std::sync::Mutex::new((0, false)),
            files,
            _scratch: None,
            runtime: Some(runtime),
        })
    }

    /// An ephemeral store (tests, and as a fallback): loose blobs in memory,
    /// file-backed content in a temporary directory removed with the store.
    ///
    /// # Panics
    ///
    /// Panics when no temporary directory can be created.
    #[allow(
        clippy::expect_used,
        reason = "an ephemeral store without a temporary directory cannot hold content at all"
    )]
    pub fn memory() -> Self {
        let scratch = tempfile::tempdir().expect("a temporary directory for the content store");
        let files = Arc::new(
            FileArea::open_ephemeral(scratch.path(), u64::MAX)
                .expect("a content area in a fresh directory"),
        );
        Self::memory_with_files(files, Some(scratch))
    }

    fn memory_with_files(files: Arc<FileArea>, scratch: Option<tempfile::TempDir>) -> Self {
        let gc = GcGate::default();
        let options = iroh_blobs::store::mem::Options {
            gc_config: Some(gc.config(Duration::from_millis(50), None)),
        };
        Self {
            backend: Backend::Mem(MemStore::new_with_opts(options)),
            cache: Cache::new(2048),
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc,
            deferred: std::sync::Mutex::new((0, false)),
            files,
            _scratch: scratch,
            runtime: None,
        }
    }

    /// Shuts the store down and releases its databases, so the same
    /// directory can be opened again in this process while handles to this
    /// store remain; they fail from now on.
    ///
    /// # Errors
    ///
    /// Returns an error when the blob store does not shut down cleanly.
    pub async fn shutdown(&self) -> Result<()> {
        let stopped = self.store().shutdown().await;
        self.files.close();
        stopped.map_err(Into::into)
    }

    /// The file-backed content and cache.
    pub const fn files(&self) -> &Arc<FileArea> {
        &self.files
    }

    #[cfg(test)]
    #[cfg(feature = "filesystem")]
    #[allow(clippy::unwrap_used, reason = "test fixture")]
    fn memory_with_gc_interval(gc_interval: Duration) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let files = Arc::new(FileArea::open_ephemeral(scratch.path(), u64::MAX).unwrap());
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
            gc: GcGate::default(),
            deferred: std::sync::Mutex::new((0, false)),
            files,
            _scratch: Some(scratch),
            runtime: None,
        }
    }

    /// The runtime the persistent store runs on, for a caller that must wait
    /// on the store from a thread that holds a lock the daemon's own runtime
    /// may be blocked on. `None` for the in-memory store, which runs on the
    /// caller's runtime.
    pub fn runtime(&self) -> Option<tokio::runtime::Handle> {
        self.runtime
            .as_ref()
            .and_then(StoreRuntime::handle)
            .cloned()
    }

    pub fn store(&self) -> &Store {
        match &self.backend {
            Backend::Mem(m) => m,
            Backend::Fs(f) => f,
        }
    }

    /// Opens a deferred-durability group: until the last open group ends
    /// ([`Self::end_deferred_sync`]), writes skip their database sync and
    /// the group's end syncs once. A changeset of many small files then
    /// costs one sync instead of one per file. Callers end the group before
    /// anything depending on the writes is made durable or announced.
    pub fn defer_sync(&self) {
        let mut deferred = self
            .deferred
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        deferred.0 = deferred.0.saturating_add(1);
    }

    /// Closes a group opened by [`Self::defer_sync`]; the last one syncs the
    /// database if a write inside the groups skipped it.
    ///
    /// # Errors
    ///
    /// Returns an error when the database sync fails; the writes stay
    /// pending for the next sync.
    pub async fn end_deferred_sync(&self) -> Result<()> {
        let sync = {
            let mut deferred = self
                .deferred
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            deferred.0 = deferred.0.saturating_sub(1);
            let sync = deferred.0 == 0 && deferred.1;
            if sync {
                deferred.1 = false;
            }
            sync
        };
        if sync && let Err(error) = self.store().sync_db().await {
            self.deferred
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .1 = true;
            return Err(error.into());
        }
        Ok(())
    }

    /// Build a `BlobsProtocol` handler backed by this store (for the
    /// router). It serves loose blobs and file-backed chunks alike.
    pub fn blobs_protocol(&self, events: Option<EventSender>) -> BlobsProtocol {
        let store = serve::serving_store(self.store().clone(), &self.files);
        BlobsProtocol::new(&store, events)
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

    /// Locally available bytes of blob `hash` (cache, loose store, or a
    /// file-backed chunk, which is checked against its hash), or `None`.
    pub async fn local(&self, hash: Hash) -> Option<Bytes> {
        if let Some(b) = self.cache.get(&hash) {
            return Some(b);
        }
        match self.store().blobs().get_bytes(hash).await {
            Ok(b) => {
                self.cache.insert(hash, b.clone());
                Some(b)
            }
            Err(_) => self.local_chunk(hash).await,
        }
    }

    /// Read a clipped range and the whole length of a locally complete blob.
    /// Loose data is BAO-verified over the requested chunk groups only; an
    /// empty range needs just its status. Existing cached bytes and bounded,
    /// hash-checked file chunks remain readable. Never fills the blob cache.
    pub async fn read_blob_range(
        &self,
        hash: Hash,
        range: std::ops::Range<u64>,
    ) -> Option<(Vec<u8>, u64)> {
        if let Some(bytes) = self.cache.get(&hash) {
            let mut out = Vec::new();
            content::copy_part(&mut out, &bytes, 0, range.start, range.end);
            return Some((out, bytes.len() as u64));
        }
        let Ok(BlobStatus::Complete { size }) = self.store().blobs().status(hash).await else {
            let bytes = self.local_chunk(hash).await?;
            let mut out = Vec::new();
            content::copy_part(&mut out, &bytes, 0, range.start, range.end);
            return Some((out, bytes.len() as u64));
        };
        let end = range.end.min(size);
        let start = range.start.min(end);
        if start == end {
            return Some((Vec::new(), size));
        }
        let requested = end.checked_sub(start)?;
        let mut out = Vec::with_capacity(usize::try_from(requested).ok()?);
        let mut items = self
            .store()
            .blobs()
            .export_bao(hash, ChunkRanges::bytes(start..end))
            .stream();
        while let Some(item) = items.next().await {
            match item {
                EncodedItem::Leaf(leaf) => {
                    content::copy_part(&mut out, &leaf.data, leaf.offset, start, end);
                }
                EncodedItem::Size(actual) if actual != size => return None,
                EncodedItem::Parent(_) | EncodedItem::Size(_) => {}
                EncodedItem::Done => {
                    return (out.len() as u64 == requested).then_some((out, size));
                }
                EncodedItem::Error(_) => return None,
            }
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn is_cached(&self, hash: Hash) -> bool {
        self.cache.contains_key(&hash)
    }

    #[cfg(test)]
    pub(crate) fn forget_cached(&self, hash: Hash) {
        self.cache.invalidate(&hash);
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

    /// [`Self::store_fetched`] for many blobs with one durability barrier:
    /// a batch of small files costs one database sync instead of one per
    /// blob. Every blob is checked against its hash before anything is
    /// stored; a mismatch rejects the whole batch.
    ///
    /// # Errors
    ///
    /// Returns an error on a hash mismatch or when the blob store fails.
    pub async fn store_fetched_batch(&self, blobs: Vec<(Hash, Bytes)>) -> Result<()> {
        if blobs.is_empty() {
            return Ok(());
        }
        let mut seen = HashSet::with_capacity(blobs.len());
        let mut unique = Vec::with_capacity(blobs.len());
        for (hash, data) in blobs {
            let actual = Hash::new(&data);
            anyhow::ensure!(
                actual == hash,
                "fetched blob hash mismatch: expected {hash}, got {actual}"
            );
            if seen.insert(hash) {
                unique.push(data);
            }
        }
        self.persist_owned_batch(unique).await?;
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
        // File-backed content goes with any of its hashes: its content id
        // or one of its chunks.
        for hash in selected {
            let key = if self.files.entry(hash.as_bytes()).is_some() {
                Some(*hash.as_bytes())
            } else {
                self.files.find_chunk(hash.as_bytes()).map(|c| c.key)
            };
            if let Some(key) = key {
                self.files.remove(&key)?;
            }
        }
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
        self.gc.authorize(live);
        let deadline = tokio::time::Instant::now()
            .checked_add(EVICTION_HANG_BOUND)
            .unwrap_or_else(tokio::time::Instant::now);
        loop {
            let mut remaining = false;
            for hash in selected {
                let remaining_time =
                    deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining_time.is_zero() {
                    self.gc.revoke();
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
                self.gc.revoke();
                let after_count = self.store().blobs().list().hashes().await?.len();
                return Ok(before_count.saturating_sub(after_count) as u64);
            }
            if tokio::time::Instant::now() >= deadline {
                self.gc.revoke();
                anyhow::bail!("selected blobs remained after eviction deadline");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Make the writes just made durable now or, inside a deferred-durability
    /// group ([`Self::defer_sync`]), leave the one sync to the group's end.
    async fn sync_unless_deferred(&self) -> Result<()> {
        let deferred = {
            let mut deferred = self
                .deferred
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if deferred.0 > 0 {
                deferred.1 = true;
            }
            deferred.0 > 0
        };
        if !deferred {
            self.store().sync_db().await?;
        }
        Ok(())
    }

    async fn persist_owned(&self, data: Bytes) -> Result<Hash> {
        self.persist_owned_batch(vec![data])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("owned blob batch unexpectedly returned no hash"))
    }

    async fn persist_owned_batch(&self, data: Vec<Bytes>) -> Result<Vec<Hash>> {
        /// Blob-store requests in flight at once: the store's actor serves
        /// them in parallel, so a batch of small chunks is not a round trip
        /// per chunk.
        const IN_FLIGHT: usize = 64;

        let _owned_tags = self.owned_tags.lock().await;
        let adds = data.into_iter().map(|bytes| async move {
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
            Ok::<_, anyhow::Error>((temporary, bytes))
        });
        let protected: Vec<_> = futures_util::TryStreamExt::try_collect(
            futures_util::StreamExt::buffered(futures_util::stream::iter(adds), IN_FLIGHT),
        )
        .await?;
        // `FsStore` explicitly does not promise that dropping it flushes recent
        // writes. Make the named protection durable before releasing the
        // temporary protection. This is a no-op-compatible boundary for the
        // in-memory backend. Inside a deferred-durability group the group's
        // end syncs once instead; the owned tags are already installed, so
        // nothing collects the blobs meanwhile.
        self.sync_unless_deferred().await?;
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
            || self.files.find_chunk(hash.as_bytes()).is_some()
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
    #[cfg(feature = "filesystem")]
    pub async fn live_record_ids_for_blob(
        &self,
        namespace: &Namespace,
        requested: Hash,
        limits: BlobRecordLookupLimits,
    ) -> std::result::Result<Vec<fs_protocol::FileRecordId>, BlobRecordLookupError> {
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
    #[cfg(feature = "filesystem")]
    pub(crate) async fn live_record_ids_for_blob_roots(
        &self,
        roots: Vec<LiveRecordBlobRoots>,
        requested: Hash,
        limits: BlobRecordLookupLimits,
    ) -> std::result::Result<Vec<fs_protocol::FileRecordId>, BlobRecordLookupError> {
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
            if manifest.chunks().len() > limits.max_chunks_per_record {
                return Err(BlobRecordLookupError::TooManyChunks {
                    record_id: roots.record_id,
                    limit: limits.max_chunks_per_record,
                });
            }
            if manifest
                .chunks()
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
    #[cfg(feature = "filesystem")]
    pub async fn inspect_version_payload(
        &self,
        record_id: fs_protocol::FileRecordId,
        version_id: fs_protocol::FileVersionId,
        manifest_hash: Hash,
    ) -> Result<VersionPayload> {
        let manifest_bytes = self
            .local(manifest_hash)
            .await
            .ok_or_else(|| anyhow::anyhow!("version manifest {manifest_hash} is not resident"))?;
        let manifest = crate::content::Manifest::decode(&manifest_bytes)?;
        let mut chunks = Vec::with_capacity(manifest.chunks().len());
        for chunk in manifest.chunks() {
            let hash = Hash::from(chunk.hash);
            let (locally_present, resident_bytes) = match self.blob_status(hash).await? {
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
    #[cfg(feature = "filesystem")]
    pub async fn verify_durable_revision(
        &self,
        revision: &fs_protocol::ContentRevisionV1,
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
        if crate::content::is_file_backed(&manifest) {
            anyhow::ensure!(
                self.is_complete(manifest_hash, &manifest).await,
                "version content is not durably complete"
            );
            return Ok(());
        }
        for chunk in manifest.chunks() {
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
    #[cfg(feature = "filesystem")]
    pub fn execute_eviction(
        &self,
        _plan: &EvictionPlan,
    ) -> std::result::Result<(), StoreEvictionError> {
        Err(StoreEvictionError::ExplicitDeletionUnsupported)
    }

    /// Freshly authorize a queued request and make exactly its approved blobs
    /// collectible by releasing Tilo-owned named tags.
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
    #[cfg(feature = "filesystem")]
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
    #[cfg(feature = "filesystem")]
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
            let state = match self.blob_status(hash).await? {
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

/// How long an eviction waits for the blob store's GC to delete the selected
/// blobs. The wait ends as soon as the next pass removes them; a pass over a
/// large store on a loaded disk can take seconds, so this bound only catches
/// a GC that has stopped.
const EVICTION_HANG_BOUND: Duration = Duration::from_secs(60);

/// The blob store's background GC runs only while an authorized selective
/// sweep holds an exact live set.
///
/// iroh-blobs polls its protect callback on a fixed interval and exposes no
/// way to trigger a pass. Answering `Abort` to every idle poll costs a wakeup
/// and an INFO log line per interval, forever; instead the callback's future
/// stays pending until [`Self::authorize`] wakes it, so an idle store costs
/// nothing.
#[derive(Clone, Default)]
struct GcGate {
    live: Arc<RwLock<Option<HashSet<Hash>>>>,
    wake: Arc<tokio::sync::Notify>,
}

impl GcGate {
    /// Let the next sweeps collect everything outside `live`.
    fn authorize(&self, live: HashSet<Hash>) {
        *self
            .live
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(live);
        self.wake.notify_one();
    }

    /// Park the GC again once the sweep this live set was installed for is over.
    fn revoke(&self) {
        *self
            .live
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    fn snapshot(&self) -> Option<HashSet<Hash>> {
        self.live
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A GC configuration whose passes wait for [`Self::authorize`] and, when
    /// `ready` is given, for it to be set.
    fn config(&self, interval: Duration, ready: Option<Arc<AtomicBool>>) -> GcConfig {
        let gate = self.clone();
        let protect: ProtectCb =
            Arc::new(move |protected| gate.clone().protect(ready.clone(), protected));
        GcConfig {
            interval,
            add_protected: Some(protect),
        }
    }

    fn protect(
        self,
        ready: Option<Arc<AtomicBool>>,
        protected: &mut HashSet<Hash>,
    ) -> Pin<Box<dyn Future<Output = ProtectOutcome> + Send + Sync + '_>> {
        Box::pin(async move {
            loop {
                let authorized = self.wake.notified();
                let snapshot = ready
                    .as_ref()
                    .is_none_or(|ready| ready.load(Ordering::Acquire))
                    .then(|| self.snapshot())
                    .flatten();
                if let Some(snapshot) = snapshot {
                    protected.extend(snapshot);
                    return ProtectOutcome::Continue;
                }
                authorized.await;
            }
        })
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
    async fn idle_gc_waits_for_an_authorized_live_set_instead_of_polling() {
        let gate = GcGate::default();
        let ready = Arc::new(AtomicBool::new(false));
        let config = gate.config(Duration::from_millis(50), Some(Arc::clone(&ready)));
        let protect = config.add_protected.expect("a protect callback");
        let kept = Hash::new(b"kept");

        let mut protected = HashSet::new();
        let pass = tokio::time::timeout(Duration::from_millis(100), protect(&mut protected)).await;
        assert!(pass.is_err(), "an unauthorized pass must stay parked");

        // A live set before the store is ready still does not release a pass.
        gate.authorize(HashSet::from([kept]));
        let pass = tokio::time::timeout(Duration::from_millis(100), protect(&mut protected)).await;
        assert!(
            pass.is_err(),
            "a pass must not run before the store is ready"
        );

        ready.store(true, Ordering::Release);
        gate.revoke();
        let mut protected = HashSet::new();
        let parked = protect(&mut protected);
        let authorize = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            gate.authorize(HashSet::from([kept]));
        };
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(parked, authorize)
        })
        .await
        .expect("authorizing wakes the parked pass");
        assert!(matches!(outcome, ProtectOutcome::Continue));
        assert_eq!(protected, HashSet::from([kept]));
    }

    #[tokio::test]
    async fn blobs_ingest_fetch_and_share_hashes_with_owned_tags() {
        // Phase `ingest_roundtrips_locally`.
        {
            let store = ContentStore::memory();
            let data = Bytes::from_static(b"the quick brown fox");
            let hash = store.ingest(data.clone()).await.unwrap();

            assert!(store.has(hash).await);
            let got = store.local(hash).await.expect("bytes present locally");
            assert_eq!(got, data);
        }
        // Phase `missing_hash_is_none`.
        {
            let store = ContentStore::memory();
            let absent = Hash::from([42u8; 32]);
            assert!(!store.has(absent).await);
            assert!(store.local(absent).await.is_none());
        }
        // Phase `identical_bytes_share_a_hash`.
        {
            let store = ContentStore::memory();
            let a = store.ingest(Bytes::from_static(b"dedup me")).await.unwrap();
            let b = store.ingest(Bytes::from_static(b"dedup me")).await.unwrap();
            assert_eq!(a, b, "content addressing: same bytes -> same hash");
        }
        // Phase `ingest_and_fetch_use_only_deterministic_owned_tags`.
        {
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
        // Phase `fetched_hash_mismatch_does_not_persist_or_cache_bytes`.
        {
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
    }

    #[tokio::test]
    async fn persistent_stores_migrate_tags_evict_selectively_and_survive_restart() {
        // Phase `persistent_selective_eviction_preserves_other_content_after_restart`.
        {
            let directory = tempfile::tempdir().unwrap();
            // Exceed the backend's inline threshold so this also exercises native
            // blob files, not just rows inside its metadata database.
            let kept_bytes = Bytes::from(vec![0x61; 1_048_576]);
            let (kept, selected) = {
                let store = ContentStore::open(directory.path()).await.unwrap();
                let kept = store.ingest(kept_bytes.clone()).await.unwrap();
                let selected = store
                    .ingest(Bytes::from(vec![0x62; 1_048_576]))
                    .await
                    .unwrap();
                assert_eq!(
                    store.evict_blobs(&HashSet::from([selected])).await.unwrap(),
                    1
                );
                assert_eq!(store.local(kept).await, Some(kept_bytes.clone()));
                assert!(store.local(selected).await.is_none());
                store.store().shutdown().await.unwrap();
                (kept, selected)
            };
            let reopened = ContentStore::open(directory.path()).await.unwrap();
            assert_eq!(reopened.local(kept).await, Some(kept_bytes));
            assert!(reopened.local(selected).await.is_none());
            reopened.store().shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    #[cfg(feature = "filesystem")]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn backend_residency_and_manifests_decide_what_is_durable_or_evicted() {
        // Phase `version_inspection_uses_backend_residency_and_exact_manifest`.
        {
            let store = ContentStore::memory();
            let data = vec![7u8; 100_000];
            let manifest_hash = store.write_content(&data).await.unwrap().unwrap();
            let payload = store
                .inspect_version_payload(
                    fs_protocol::FileRecordId::new(),
                    fs_protocol::FileVersionId::new(),
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
        // Phase `durable_revision_rejects_a_complete_manifest_with_any_missing_chunk`.
        {
            let store = ContentStore::memory();
            let missing = iroh_blobs::Hash::new(b"not persisted");
            let manifest = crate::content::Manifest::new(
                crate::content::ChunkTier::Small,
                vec![crate::content::ChunkRef {
                    hash: *missing.as_bytes(),
                    len: 13,
                }],
            );
            let manifest_hash = store.ingest(Bytes::from(manifest.encode())).await.unwrap();
            let revision = fs_protocol::ContentRevisionV1 {
                schema_version: fs_protocol::CONTENT_REVISION_VERSION,
                record_id: fs_protocol::FileRecordId::new(),
                version_id: fs_protocol::FileVersionId::new(),
                content_hash: Some(fs_protocol::ContentHash(*manifest_hash.as_bytes())),
                byte_len: 13,
                metadata_hash: fs_protocol::ContentHash([1; 32]),
            };
            assert!(store.verify_durable_revision(&revision).await.is_err());
        }
        // Phase `unsupported_execution_never_claims_to_delete`.
        {
            use crate::eviction::{EvictionMode, EvictionPlan};

            let store = ContentStore::memory();
            let hash = store.ingest(Bytes::from_static(b"keep me")).await.unwrap();
            let plan = EvictionPlan {
                record_id: fs_protocol::FileRecordId::new(),
                version_id: fs_protocol::FileVersionId::new(),
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
        // Phase `eviction_observation_completes_only_for_backend_absence`.
        {
            use crate::eviction::{
                EvictionMode, EvictionPlan, EvictionQueue, EvictionRequestState,
            };

            let temp = tempfile::TempDir::new().unwrap();
            let store = ContentStore::memory();
            let present = store.ingest(Bytes::from_static(b"present")).await.unwrap();
            let absent = Hash::from([91; 32]);
            let mut queue = EvictionQueue::open(temp.path().join("evictions.json")).unwrap();

            let make_plan = |hash| EvictionPlan {
                record_id: fs_protocol::FileRecordId::new(),
                version_id: fs_protocol::FileVersionId::new(),
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
    }

    #[tokio::test]
    #[cfg(feature = "filesystem")]
    #[allow(
        clippy::too_many_lines,
        reason = "multi-phase backend eviction lifecycle scenario"
    )]
    async fn backend_eviction_releases_only_owned_tags_for_fresh_roots() {
        // Phase `queued_eviction_releases_only_owned_target_tags_and_gc_completion_is_observed`.
        {
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

            let target_id = fs_protocol::FileRecordId::new();
            let target_version = fs_protocol::FileVersionId::new();
            let other_id = fs_protocol::FileRecordId::new();
            let other_version = fs_protocol::FileVersionId::new();
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
        // Phase `backend_handoff_refuses_stale_shared_root_without_releasing_tag`.
        {
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
            let target_id = fs_protocol::FileRecordId::new();
            let target_version = fs_protocol::FileVersionId::new();
            let other_id = fs_protocol::FileRecordId::new();
            let other_version = fs_protocol::FileVersionId::new();
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
        // Phase `eviction_never_releases_foreign_tags_or_claims_their_blob_was_deleted`.
        {
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

            let record_id = fs_protocol::FileRecordId::new();
            let version_id = fs_protocol::FileVersionId::new();
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
    }

    #[tokio::test]
    #[cfg(feature = "filesystem")]
    #[allow(
        clippy::too_many_lines,
        reason = "every reverse-lookup source is one matrix"
    )]
    async fn blob_reverse_lookup_is_complete_bounded_and_fail_closed() {
        // Phase `blob_reverse_lookup_covers_roots_chunks_thumbnails_and_live_records_only`.
        {
            let store = ContentStore::memory();
            let shared_chunk = store.ingest(Bytes::from_static(b"shared")).await.unwrap();
            let first_only_chunk = store.ingest(Bytes::from_static(b"first")).await.unwrap();
            let second_only_chunk = store.ingest(Bytes::from_static(b"second")).await.unwrap();
            let manifest = |unique: Hash| {
                crate::content::Manifest::new(
                    crate::content::ChunkTier::Small,
                    vec![
                        crate::content::ChunkRef {
                            hash: *shared_chunk.as_bytes(),
                            len: 6,
                        },
                        crate::content::ChunkRef {
                            hash: *unique.as_bytes(),
                            len: 6,
                        },
                    ],
                )
            };
            let first_manifest = store
                .ingest(Bytes::from(manifest(first_only_chunk).encode()))
                .await
                .unwrap();
            let second_manifest = store
                .ingest(Bytes::from(manifest(second_only_chunk).encode()))
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
        // Phase `blob_reverse_lookup_is_bounded_and_fails_closed_on_missing_manifest`.
        {
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
}
