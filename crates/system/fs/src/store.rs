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
//! front of loose blobs for hot reads, bounded by bytes
//! ([`BLOB_CACHE_BYTES`]) and holding only small blobs
//! ([`MAX_CACHED_BLOB`]). The provider serves both backings
//! ([`ContentStore::blobs_protocol`]).

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
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
    BackendGcAuthorizationError, BlobExtent, EvictionCatalog, EvictionObservation, EvictionQueue,
    EvictionRequestId, ObservedBlobState, VersionPayload,
};
#[cfg(feature = "filesystem")]
use crate::namespace::{LiveRecordBlobRoots, LiveRecordBlobScanError, Namespace};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

pub mod area;
#[cfg(test)]
mod cache_tests;
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
    /// Database syncs writes have asked for; see [`ContentStore::write_syncs`].
    write_syncs: std::sync::atomic::AtomicU64,
    /// Bytes read so far; see [`ContentStore::read_bytes`].
    read_bytes: std::sync::atomic::AtomicU64,
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

/// Heap the hot cache in front of loose blobs may hold, counting each
/// entry's bookkeeping. It is for what is read again and again and is small:
/// manifests, the chunks of small files, thumbnails.
pub const BLOB_CACHE_BYTES: u64 = 32 * 1024 * 1024;

/// Largest blob the hot cache admits. Anything larger is file-backed content
/// ([`crate::content::FILE_BACKED_MIN`] is the same size) or document media,
/// both read by range, and one such blob would take a large share of the
/// budget for a single read.
pub const MAX_CACHED_BLOB: usize = 1024 * 1024;

/// What a cache entry costs beyond its bytes: the key, the `Bytes` handle
/// and the cache's own node.
const CACHE_ENTRY_OVERHEAD: u32 = 256;

/// An insert at least this large settles the cache at once. moka applies
/// writes in batches of 64, so without this a run of large inserts could sit
/// above the budget until the next batch; with it, what waits is at most 64
/// inserts smaller than this.
const CACHE_SETTLE_AT: usize = 64 * 1024;

/// The hot cache in front of loose blobs, bounded by bytes. Its admission
/// weighs a newcomer's access frequency against that of every entry it would
/// evict, so a run of cold large reads does not flush small hot entries.
fn blob_cache() -> Cache<Hash, Bytes> {
    Cache::builder()
        .max_capacity(BLOB_CACHE_BYTES)
        .weigher(|_, bytes: &Bytes| {
            u32::try_from(bytes.len())
                .unwrap_or(u32::MAX)
                .saturating_add(CACHE_ENTRY_OVERHEAD)
        })
        .build()
}

/// Removes what imports left in the blob store's spill directory. A stream
/// import spills to a file there and the store removes it only when the
/// import succeeds, so an upload abandoned halfway leaves its part behind.
/// Called right after the store loads, before anything imports, so every
/// file there is from an earlier run.
fn remove_import_spills(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Where a persistent store at `cache_dir` keeps its file-backed content,
/// unless opened with [`ContentStore::open_with_files`].
fn files_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("files")
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
    pub async fn open_with_gc_interval(
        cache_dir: impl AsRef<Path>,
        gc_interval: Duration,
    ) -> Result<Self> {
        let dir = cache_dir.as_ref();
        Self::open_inner(dir, gc_interval, &files_dir(dir)).await
    }

    /// The whole of file version `content` (its manifest hash) from the
    /// persistent store at `cache_dir` ([`Self::open`]), read without
    /// opening the store: for another process beside the one that has it
    /// open, which must not take its database locks. Only file-backed
    /// versions ([`crate::content::FILE_BACKED_MIN`]) live where such a
    /// reader can find them; any other is `Ok(None)`, as is one longer than
    /// `max_bytes`.
    ///
    /// Nothing is verified and the version may be only partly resident: the
    /// caller checks the bytes against a hash it trusts.
    ///
    /// # Errors
    ///
    /// Returns an error when the version's file exists but cannot be read.
    pub fn read_file_version_unlocked(
        cache_dir: &Path,
        content: [u8; 32],
        max_bytes: u64,
    ) -> std::io::Result<Option<Vec<u8>>> {
        area::read_entry_unlocked(&files_dir(cache_dir), &content, max_bytes)
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
        remove_import_spills(&dir.join("temp"));
        Ok(Self {
            backend: Backend::Fs(store),
            cache: blob_cache(),
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc,
            deferred: std::sync::Mutex::new((0, false)),
            write_syncs: std::sync::atomic::AtomicU64::new(0),
            read_bytes: std::sync::atomic::AtomicU64::new(0),
            files,
            _scratch: None,
            runtime: Some(runtime),
        })
    }

    /// An ephemeral store (tests, and as a fallback): loose blobs in memory,
    /// file-backed content in a temporary directory removed with the store.
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
            cache: blob_cache(),
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc,
            deferred: std::sync::Mutex::new((0, false)),
            write_syncs: std::sync::atomic::AtomicU64::new(0),
            read_bytes: std::sync::atomic::AtomicU64::new(0),
            files,
            _scratch: scratch,
            runtime: None,
        }
    }

    /// Shuts the store down and releases its databases, so the same
    /// directory can be opened again in this process while handles to this
    /// store remain; they fail from now on.
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
            cache: blob_cache(),
            owned_tags: tokio::sync::Mutex::new(()),
            mutation: tokio::sync::Mutex::new(()),
            gc: GcGate::default(),
            deferred: std::sync::Mutex::new((0, false)),
            write_syncs: std::sync::atomic::AtomicU64::new(0),
            read_bytes: std::sync::atomic::AtomicU64::new(0),
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
        let mut deferred = self.deferred.lock_recover();
        deferred.0 = deferred.0.saturating_add(1);
    }

    /// How many database syncs the writes to this store have made so far: one
    /// per write outside a deferred-durability group, one per group that
    /// wrote. Each is an fsync, so a caller writing many blobs can check that
    /// it pays for one rather than one per blob.
    pub fn write_syncs(&self) -> u64 {
        self.write_syncs.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many bytes reads of this store have taken so far: whole blobs
    /// [`Self::local`] returned (cached or not), the verified leaves of
    /// [`Self::read_blob_range`], and file-backed content. A ranged read pays
    /// for its range, so a caller can check that it never loads whole blobs.
    pub fn read_bytes(&self) -> u64 {
        self.read_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_add(self.files.read_bytes())
    }

    fn note_read(&self, bytes: usize) {
        self.read_bytes
            .fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
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
            let mut deferred = self.deferred.lock_recover();
            deferred.0 = deferred.0.saturating_sub(1);
            let sync = deferred.0 == 0 && deferred.1;
            if sync {
                deferred.1 = false;
            }
            sync
        };
        if sync {
            self.write_syncs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if sync && let Err(error) = self.store().sync_db().await {
            self.deferred.lock_recover().1 = true;
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
    pub async fn ingest(&self, data: impl Into<Bytes>) -> Result<Hash> {
        let _mutation = self.mutation.lock().await;
        let data = data.into();
        self.persist_owned(data).await
    }

    /// Locally available bytes of blob `hash` (cache, loose store, or a
    /// file-backed chunk, which is checked against its hash), or `None`.
    pub async fn local(&self, hash: Hash) -> Option<Bytes> {
        if let Some(b) = self.cache.get(&hash) {
            self.note_read(b.len());
            return Some(b);
        }
        match self.store().blobs().get_bytes(hash).await {
            Ok(b) => {
                self.note_read(b.len());
                self.cache_insert(hash, b.clone());
                Some(b)
            }
            Err(_) => self.local_chunk(hash).await,
        }
    }

    /// Keep `bytes` hot if they are small enough to be worth their share of
    /// [`BLOB_CACHE_BYTES`].
    fn cache_insert(&self, hash: Hash, bytes: Bytes) {
        if bytes.len() > MAX_CACHED_BLOB {
            return;
        }
        let settle = bytes.len() >= CACHE_SETTLE_AT;
        self.cache.insert(hash, bytes);
        if settle {
            self.cache.run_pending_tasks();
        }
    }

    /// Import content that arrives as a stream of pieces, hashing and
    /// writing each as it comes: a persistent store spills it to a file
    /// after the first 16 KiB and builds the BAO outboard from that file, so
    /// it never holds the whole content. Returns the content's hash and
    /// length. Never fills the hot cache.
    ///
    /// The stream is consumed before any lock is taken, so a slow producer
    /// (an app uploading) holds up nothing else; the import is protected
    /// from collection by its temporary tag until the owned tag is in
    /// place, as for [`Self::ingest`]. A piece that is an error abandons the
    /// import and stores nothing.
    pub async fn ingest_stream<S>(&self, pieces: S) -> Result<(Hash, u64)>
    where
        S: futures_util::Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static,
    {
        // iroh-blobs' `add_stream` stops forwarding at an error but keeps
        // the import open, waiting for an end that never comes. So an error
        // is never handed to it: it abandons the import here instead, and
        // dropping the import ends the store's side of it.
        let (abandon, abandoned) = tokio::sync::oneshot::channel::<std::io::Error>();
        let mut abandon = Some(abandon);
        let mut pieces = Box::pin(pieces);
        let forwarded =
            futures_util::stream::poll_fn(move |cx| match pieces.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Err(error))) => {
                    if let Some(abandon) = abandon.take() {
                        let _ = abandon.send(error);
                    }
                    std::task::Poll::Pending
                }
                other => other,
            });
        let import = async {
            self.store()
                .blobs()
                .add_stream(forwarded)
                .await
                .temp_tag()
                .await
        };
        let temporary = tokio::select! {
            imported = import => imported?,
            Ok(error) = abandoned => {
                return Err(anyhow::Error::new(error).context("the streamed import was abandoned"));
            }
        };
        let hash = temporary.hash();
        let BlobStatus::Complete { size } = self.store().blobs().status(hash).await? else {
            anyhow::bail!("streamed blob {hash} is not complete after its import");
        };
        let _mutation = self.mutation.lock().await;
        let _owned_tags = self.owned_tags.lock().await;
        self.store()
            .tags()
            .set(
                owned_blob_tag(hash, temporary.format()),
                temporary.hash_and_format(),
            )
            .await?;
        self.sync_unless_deferred().await?;
        drop(temporary);
        Ok((hash, size))
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
                    self.note_read(leaf.data.len());
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

    /// Bytes the hot cache holds once its pending writes are applied.
    #[cfg(test)]
    pub(crate) fn cached_bytes(&self) -> u64 {
        self.cache.run_pending_tasks();
        self.cache.weighted_size()
    }

    /// Persist + cache bytes that were fetched and verified from a peer.
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
            let mut deferred = self.deferred.lock_recover();
            if deferred.0 > 0 {
                deferred.1 = true;
            }
            deferred.0 > 0
        };
        if !deferred {
            self.write_syncs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            self.cache_insert(temporary.hash(), bytes);
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

    /// Freshly authorize a queued request and make exactly its approved blobs
    /// collectible by releasing Tilo-owned named tags.
    ///
    /// The caller must keep `registry` and `catalog` stable for this complete
    /// call, as required by [`EvictionQueue::authorize_backend_gc`]. Other
    /// applications' tags and temporary transfer tags are never touched. A
    /// successful result proves only a durable GC handoff; use
    /// [`Self::observe_eviction_request`] for exact completion.
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
        *self.live.write_recover() = Some(live);
        self.wake.notify_one();
    }

    /// Park the GC again once the sweep this live set was installed for is over.
    fn revoke(&self) {
        *self.live.write_recover() = None;
    }

    fn snapshot(&self) -> Option<HashSet<Hash>> {
        self.live.read_recover().clone()
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

        // The callback decides before its first wait, so one poll shows
        // whether it parked.
        let parks = |pass: Pin<Box<dyn Future<Output = ProtectOutcome> + Send + Sync + '_>>| {
            let mut pass = pass;
            pass.as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        };
        let mut protected = HashSet::new();
        assert!(
            parks(protect(&mut protected)),
            "an unauthorized pass must stay parked"
        );

        // A live set before the store is ready still does not release a pass.
        gate.authorize(HashSet::from([kept]));
        assert!(
            parks(protect(&mut protected)),
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
        let (outcome, ()) = tilo_test_env::within("authorizing to wake the parked pass", async {
            tokio::join!(parked, authorize)
        })
        .await;
        assert!(matches!(outcome, ProtectOutcome::Continue));
        assert_eq!(protected, HashSet::from([kept]));
    }

    #[tokio::test]
    async fn blobs_ingest_fetch_and_share_hashes_with_owned_tags() {
        {
            let store = ContentStore::memory();
            let data = Bytes::from_static(b"the quick brown fox");
            let hash = store.ingest(data.clone()).await.unwrap();

            assert!(store.has(hash).await);
            let got = store.local(hash).await.expect("bytes present locally");
            assert_eq!(got, data);
        }
        {
            let store = ContentStore::memory();
            let absent = Hash::from([42u8; 32]);
            assert!(!store.has(absent).await);
            assert!(store.local(absent).await.is_none());
        }
        {
            let store = ContentStore::memory();
            let a = store.ingest(Bytes::from_static(b"dedup me")).await.unwrap();
            let b = store.ingest(Bytes::from_static(b"dedup me")).await.unwrap();
            assert_eq!(a, b, "content addressing: same bytes -> same hash");
        }
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
        {
            use crate::availability::{RecordAvailability, RecordKind};
            use crate::eviction::{
                EvictionCatalog, EvictionMode, EvictionQueue, EvictionRequestState, VersionPayload,
                plan_destructive_eviction,
            };
            use fs_protocol::Residency;

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
        {
            use crate::availability::{RecordAvailability, RecordKind};
            use crate::eviction::{
                EvictionCatalog, EvictionQueue, VersionPayload, plan_destructive_eviction,
            };
            use fs_protocol::Residency;

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
            // The GC sweeps only an authorized live set: none, none deleted.
            assert!(
                store.gc.snapshot().is_none(),
                "a refused eviction authorized a sweep"
            );
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
        {
            use crate::availability::{RecordAvailability, RecordKind};
            use crate::eviction::{
                EvictionCatalog, EvictionQueue, EvictionRequestState, VersionPayload,
                plan_destructive_eviction,
            };
            use fs_protocol::Residency;

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

    #[tokio::test]
    async fn another_process_reads_file_backed_versions_while_the_store_is_open() {
        let directory = tempfile::tempdir().unwrap();
        let store = ContentStore::open(directory.path()).await.unwrap();
        let large: Vec<u8> = (0..3 * 1024 * 1024_u32)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let manifest = *store
            .write_content(&large)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        let small = *store
            .write_content(b"a small file kept as loose blobs")
            .await
            .unwrap()
            .unwrap()
            .as_bytes();

        // The store stays open, holding its databases, while it is read.
        let read = |content, max_bytes| {
            ContentStore::read_file_version_unlocked(directory.path(), content, max_bytes).unwrap()
        };
        let len = u64::try_from(large.len()).unwrap();
        assert_eq!(read(manifest, u64::MAX), Some(large.clone()));
        assert_eq!(read(manifest, len), Some(large.clone()));
        assert_eq!(read(manifest, len - 1), None, "over the bound");
        assert_eq!(read(small, u64::MAX), None, "not file-backed");
        assert_eq!(read([7; 32], u64::MAX), None, "unknown");

        // A link where an entry's file belongs is not followed.
        let elsewhere = directory.path().join("elsewhere");
        std::fs::write(&elsewhere, &large).unwrap();
        let linked = files_dir(directory.path())
            .join("data")
            .join(hex::encode([8; 32]));
        std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
        assert_eq!(read([8; 32], u64::MAX), None);
        drop(store);
    }
}
