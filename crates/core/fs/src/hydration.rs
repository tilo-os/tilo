//! Content hydration: serve file bytes locally, fetching what is missing from the workspace.
//!
//! A `read()` resolves an inode to its content id through the namespace, then asks the
//! [`Hydrator`]. Small content is loose blobs; file-backed content is read from the store's area,
//! and the chunks its missing byte ranges overlap are fetched into it. Every byte this device
//! lacks comes from one place, the workspace's [`ContentSource`]: the bytes travel beside the
//! tree's records, and are checked against their hash before anything keeps them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Result, anyhow};
use bytes::Bytes;
use iroh_blobs::Hash;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};

use crate::content::{self, Manifest};
use crate::store::ContentStore;
use fs_protocol::SyncState;
use tilo_lock::{MutexRecover as _, RwLockRecover as _};

mod batch;
mod file_keys;
mod ranges;
pub use batch::{FetchReport, Want};
pub use file_keys::{FileKeys, KEYS_NOT_READY};
pub use ranges::Priority;

type FetchResult = std::result::Result<Bytes, String>;
type FetchCell = Arc<OnceCell<FetchResult>>;
type FetchMap = HashMap<Hash, FetchCell>;

/// Longest a blocked read waits for bytes a peer must supply, through FUSE and through native
/// placeholders alike. Past it the read fails with `EIO` and the fetch keeps going, so a retry finds
/// the bytes local. Well inside the kernel's two-minute hung-task report.
pub const BLOCKED_READ_BOUND: Duration = Duration::from_secs(30);

/// Waits at most `bound` for `read`, which runs on as a task of its own: a read that outlives the
/// bound still makes its bytes local for the next one.
pub async fn bounded_read<T, F>(bound: Duration, read: F) -> Result<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T>> + Send + 'static,
{
    match tokio::time::timeout(bound, tokio::spawn(read)).await {
        Ok(Ok(result)) => result,
        Ok(Err(stopped)) => Err(anyhow!("the read stopped: {stopped}")),
        Err(_) => Err(anyhow!("no peer delivered the bytes within {bound:?}")),
    }
}

/// Where the blobs this device lacks come from: the workspace, through the device's workspace
/// service.
pub trait ContentSource: Send + Sync {
    /// The bytes named `hash`, when the source can hand them over; they are checked against `hash`
    /// before anything keeps them.
    fn fetch(&self, hash: Hash) -> futures_util::future::BoxFuture<'static, Option<Bytes>>;
}

pub struct Hydrator {
    pub store: Arc<ContentStore>,
    /// Number of hydration fetches in flight for each blob, refcounted rather than a set: two
    /// overlapping guards for the same hash keep the file `downloading` until the final fetch ends.
    in_flight: Arc<Mutex<HashMap<Hash, usize>>>,
    /// Per-blob single-flight cells: concurrent reads and prefetches await the same ask
    /// instead of fetching identical bytes.
    fetches: Arc<AsyncMutex<FetchMap>>,
    /// The blobs this process received from the workspace: they are kept elsewhere too.
    received: Arc<RwLock<std::collections::HashSet<Hash>>>,
    /// Decoded manifests with their offset tables, bounded by total chunk count.
    manifests: moka::sync::Cache<Hash, Arc<Manifest>>,
    flights: ranges::Flights,
    scheduler: ranges::Scheduler,
    ranged_bytes: std::sync::atomic::AtomicU64,
    blob_bytes: std::sync::atomic::AtomicU64,
    pub runtime_handle: tokio::runtime::Handle,
    source: Option<Arc<dyn ContentSource>>,
    file_keys: Option<FileKeys>,
    scoped_file_keys: Option<file_keys::ScopedFileKeys>,
}

/// Chunk entries held by the manifest cache: roughly 40 bytes each, so about 20 MiB.
const MANIFEST_CHUNK_BUDGET: u64 = 512 * 1024;

/// Decrements a hash's in-flight count on drop; hold it for the duration of a hydration so
/// `sync_state` reports `downloading` until every overlapping fetch has ended.
pub struct FetchGuard {
    counts: Arc<Mutex<HashMap<Hash, usize>>>,
    hash: Hash,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        let mut counts = self.counts.lock_recover();
        if let Some(count) = counts.get_mut(&self.hash) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.hash);
            }
        }
    }
}

impl Hydrator {
    pub fn new(store: Arc<ContentStore>) -> Self {
        Self {
            store,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            fetches: Arc::new(AsyncMutex::new(HashMap::new())),
            received: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manifests: moka::sync::Cache::builder()
                .max_capacity(MANIFEST_CHUNK_BUDGET)
                .weigher(|_, manifest: &Arc<Manifest>| {
                    u32::try_from(manifest.chunks().len().max(1)).unwrap_or(u32::MAX)
                })
                .build(),
            flights: ranges::Flights::default(),
            scheduler: ranges::Scheduler::default(),
            ranged_bytes: std::sync::atomic::AtomicU64::new(0),
            blob_bytes: std::sync::atomic::AtomicU64::new(0),
            runtime_handle: tokio::runtime::Handle::current(),
            source: None,
            file_keys: None,
            scoped_file_keys: None,
        }
    }

    /// Whether content this device lacks has somewhere to come from.
    #[must_use]
    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    /// Where the content this device lacks is asked for.
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn ContentSource>) -> Self {
        self.source = Some(source);
        self
    }

    /// Require governed content keys for new file versions.
    #[must_use]
    pub fn with_file_keys(mut self, keys: FileKeys) -> Self {
        self.file_keys = Some(keys);
        self
    }

    /// Make `hashes` local from the content source, as loose blobs; the ones it did not hand over, or
    /// handed over as other bytes, are returned.
    pub(crate) async fn fetch_from_source(&self, hashes: Vec<Hash>) -> Vec<Hash> {
        let Some(source) = self.source.clone() else {
            return hashes;
        };
        let asks = hashes.into_iter().map(|hash| {
            let source = Arc::clone(&source);
            async move {
                let _guard = self.mark_fetching(hash);
                let Some(bytes) = source.fetch(hash).await else {
                    return Err(hash);
                };
                let length = bytes.len() as u64;
                match self.store.store_fetched(hash, bytes).await {
                    Ok(()) => {
                        self.count_blob_bytes(length);
                        self.received.write_recover().insert(hash);
                        Ok(())
                    }
                    Err(error) => {
                        tracing::debug!(%hash, %error, "the content source's bytes are not kept");
                        Err(hash)
                    }
                }
            }
        });
        let results: Vec<Result<(), Hash>> = futures_util::StreamExt::collect(
            futures_util::StreamExt::buffer_unordered(futures_util::stream::iter(asks), 8),
        )
        .await;
        results.into_iter().filter_map(Result::err).collect()
    }

    /// Whether a fetch of blob `hash` is in flight right now.
    pub fn is_fetching(&self, hash: Hash) -> bool {
        self.in_flight
            .lock_recover()
            .get(&hash)
            .is_some_and(|count| *count > 0)
    }

    /// Mark `hash` as being fetched; the returned guard releases one reference on drop, so
    /// overlapping fetches cannot clear each other's state.
    pub fn mark_fetching(&self, hash: Hash) -> FetchGuard {
        let mut counts = self.in_flight.lock_recover();
        let count = counts.entry(hash).or_insert(0);
        *count = count.saturating_add(1);
        drop(counts);
        FetchGuard {
            counts: self.in_flight.clone(),
            hash,
        }
    }

    /// The hydration state of the file whose content is `manifest_hash`: `downloading` if a fetch is
    /// in flight, `projected` if the manifest or a chunk is still missing, `local` only when all are
    /// present.
    pub async fn sync_state(&self, manifest_hash: Hash) -> SyncState {
        if self.is_fetching(manifest_hash) {
            return SyncState::Downloading;
        }
        let Some(mbytes) = self.store.local(manifest_hash).await else {
            return SyncState::Projected;
        };
        let Ok(manifest) = Manifest::decode(&mbytes) else {
            return SyncState::Projected;
        };
        if content::is_file_backed(&manifest) {
            return if self.store.is_complete(manifest_hash, &manifest).await {
                SyncState::Local
            } else {
                SyncState::Projected
            };
        }
        let mut any_missing = false;
        for c in manifest.chunks() {
            let h = Hash::from(c.hash);
            if self.is_fetching(h) {
                return SyncState::Downloading;
            }
            if !self.store.has(h).await {
                any_missing = true;
            }
        }
        if any_missing {
            SyncState::Projected
        } else {
            SyncState::Local
        }
    }

    /// Local residency and the exact physical blobs that make up one file. `remotely_durable` is
    /// strict: every present component must have been received from the workspace in this process.
    pub async fn residency(&self, manifest_hash: Hash) -> ContentResidency {
        let mut result = ContentResidency {
            remote_only: true,
            in_flight: self.is_fetching(manifest_hash),
            ..ContentResidency::default()
        };
        let Some(bytes) = self.store.local(manifest_hash).await else {
            return result;
        };
        let Ok(manifest) = Manifest::decode(&bytes) else {
            return result;
        };
        result.remote_only = false;
        result.hydrated_bytes = bytes.len() as u64;
        result.blobs.push(manifest_hash);
        let received = self.received.read_recover().clone();
        result.remotely_durable = received.contains(&manifest_hash);
        if content::is_file_backed(&manifest) {
            // Fetched ranges fill the area chunk by chunk, so file-backed content never counts
            // as remotely durable here; receipts are the durability proof.
            result.remotely_durable = false;
            if let Some(entry) = self.store.files().entry(manifest_hash.as_bytes()) {
                result.hydrated_bytes = result.hydrated_bytes.saturating_add(entry.resident);
            }
            return result;
        }
        for chunk in manifest.chunks().iter().copied() {
            let hash = Hash::from(chunk.hash);
            result.in_flight |= self.is_fetching(hash);
            if self.store.has(hash).await {
                result.hydrated_bytes = result.hydrated_bytes.saturating_add(u64::from(chunk.len));
                result.blobs.push(hash);
                result.remotely_durable &= received.contains(&hash);
            } else {
                result.remotely_durable = false;
            }
        }
        result
    }

    pub async fn read_blob(&self, hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        if let Some(bytes) = self.store.local(hash).await {
            return Ok(slice(&bytes, offset, size));
        }

        let cell = {
            let mut fetches = self.fetches.lock().await;
            fetches
                .entry(hash)
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        let fetched = cell
            .get_or_init(|| async {
                if self.fetch_from_source(vec![hash]).await.is_empty() {
                    self.store
                        .local(hash)
                        .await
                        .ok_or_else(|| format!("{hash} was evicted as it arrived"))
                } else {
                    Err(format!("the workspace did not hand over {hash}"))
                }
            })
            .await
            .clone();

        // Remove only our completed cell. A later caller may already have
        // installed a new retry cell after a failed request.
        {
            let mut pending = self.fetches.lock().await;
            if pending
                .get(&hash)
                .is_some_and(|current| Arc::ptr_eq(current, &cell))
            {
                pending.remove(&hash);
            }
        }

        fetched
            .map(|bytes| slice(&bytes, offset, size))
            .map_err(|message| anyhow!(message))
    }

    /// One chunk of `manifest_hash`, from the store or fetched with its manifest named: a serving peer
    /// authorises a chunk only through a manifest in the same request, so a bare [`Self::read_blob`]
    /// of a chunk is refused.
    pub async fn read_chunk(&self, hash: Hash, len: u64, manifest_hash: Hash) -> Result<Bytes> {
        if let Some(bytes) = self.store.local(hash).await {
            return Ok(bytes);
        }
        self.fetch_many(vec![Want::chunk(hash, len, manifest_hash)])
            .await;
        self.store.local(hash).await.ok_or_else(|| {
            anyhow!("the workspace did not hand over chunk {hash} of {manifest_hash}")
        })
    }

    /// Read a byte range of a file given its content id: fetch the manifest, then only what
    /// `[offset, offset+size)` needs.
    pub async fn read(&self, manifest_hash: Hash, offset: u64, size: u64) -> Result<Vec<u8>> {
        self.read_with(manifest_hash, offset, size, Priority::Foreground)
            .await
    }

    /// [`Self::read`] at a given priority.
    pub async fn read_with(
        &self,
        manifest_hash: Hash,
        offset: u64,
        size: u64,
        priority: Priority,
    ) -> Result<Vec<u8>> {
        self.read_in(manifest_hash, offset, size, priority, None)
            .await
    }

    pub(crate) async fn read_scoped(
        &self,
        manifest_hash: Hash,
        offset: u64,
        size: u64,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> Result<Vec<u8>> {
        self.read_in(manifest_hash, offset, size, Priority::Foreground, workspace)
            .await
    }

    async fn read_in(
        &self,
        manifest_hash: Hash,
        offset: u64,
        size: u64,
        priority: Priority,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> Result<Vec<u8>> {
        self.authorize_scoped_read(workspace)?;
        let tracing_read = tracing_reads();
        let began = std::time::Instant::now();
        let manifest = self.manifest_index(manifest_hash).await?;
        if tracing_read {
            eprintln!(
                "read of {manifest_hash}: its manifest in {:?}",
                began.elapsed()
            );
        }
        let end = offset.saturating_add(size).min(manifest.total_len());
        let start = offset.min(end);
        if content::is_file_backed(&manifest) {
            let ranged = std::time::Instant::now();
            self.ensure_range(manifest_hash, &manifest, start..end, priority)
                .await?;
            if tracing_read {
                eprintln!(
                    "read of {manifest_hash}: its range in {:?}",
                    ranged.elapsed()
                );
            }
            return self
                .store
                .read_local(manifest_hash, &manifest, start..end)
                .await
                .ok_or_else(|| anyhow!("{manifest_hash} was evicted while it was read"));
        }
        let decoder = self.decoder(&manifest, workspace).await?;
        let wants: Vec<Want> = manifest
            .overlapping(start..end)
            .map(|(_, _, chunk)| {
                Want::chunk(
                    Hash::from(chunk.hash),
                    manifest.stored_len(chunk),
                    manifest_hash,
                )
            })
            .collect();
        if wants.len() > 1 {
            self.fetch_many(wants).await;
        }
        let mut out = Vec::with_capacity(usize::try_from(end.saturating_sub(start)).unwrap_or(0));
        for (_, chunk_start, chunk) in manifest.overlapping(start..end) {
            let hash = Hash::from(chunk.hash);
            let bytes = match self.store.local(hash).await {
                Some(bytes) => bytes,
                None => {
                    self.read_chunk(hash, manifest.stored_len(chunk), manifest_hash)
                        .await?
                }
            };
            let bytes = match &decoder {
                Some(decoder) => decoder.open(&bytes, chunk.len)?,
                None => bytes,
            };
            crate::store::copy_part(&mut out, &bytes, chunk_start, start, end);
        }
        Ok(out)
    }

    /// Makes `[offset, offset+size)` of `manifest_hash` local at `priority` without returning it. For
    /// file-backed content the bytes are then in the store's area, where [`ContentStore::files`] can
    /// clone them.
    pub async fn hydrate_range(
        &self,
        manifest_hash: Hash,
        offset: u64,
        size: u64,
        priority: Priority,
    ) -> Result<Arc<Manifest>> {
        let manifest = self.manifest_index(manifest_hash).await?;
        let end = offset.saturating_add(size).min(manifest.total_len());
        let start = offset.min(end);
        if content::is_file_backed(&manifest) {
            self.ensure_range(manifest_hash, &manifest, start..end, priority)
                .await?;
        } else if manifest.sealed().is_some() {
            for (_, _, chunk) in manifest.overlapping(start..end) {
                self.read_chunk(
                    Hash::from(chunk.hash),
                    manifest.stored_len(chunk),
                    manifest_hash,
                )
                .await?;
            }
        } else {
            self.read_with(manifest_hash, start, end.saturating_sub(start), priority)
                .await?;
        }
        Ok(manifest)
    }

    /// [`read`](Self::read); kept for callers that name a fetch parallelism, which the transfer engine
    /// chooses itself.
    pub async fn read_parallel(
        &self,
        manifest_hash: Hash,
        offset: u64,
        size: u64,
        _parallel: usize,
    ) -> Result<Vec<u8>> {
        self.read(manifest_hash, offset, size).await
    }

    /// [`read`](Self::read) served only from local content: `None` when the manifest or any part of the
    /// range would have to be fetched. Safe on a latency-critical thread.
    pub async fn read_local(&self, manifest_hash: Hash, offset: u64, size: u64) -> Option<Vec<u8>> {
        self.read_local_scoped(manifest_hash, offset, size, None)
            .await
    }

    pub(crate) async fn read_local_scoped(
        &self,
        manifest_hash: Hash,
        offset: u64,
        size: u64,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> Option<Vec<u8>> {
        self.authorize_scoped_read(workspace).ok()?;
        let manifest = self.local_manifest_index(manifest_hash).await?;
        let end = offset.saturating_add(size).min(manifest.total_len());
        let start = offset.min(end);
        let Some(decoder) = self.decoder(&manifest, workspace).await.ok()? else {
            return self
                .store
                .read_local(manifest_hash, &manifest, start..end)
                .await;
        };
        let mut out = Vec::new();
        for (_, chunk_start, chunk) in manifest.overlapping(start..end) {
            let sealed = self.store.local(Hash::from(chunk.hash)).await?;
            let bytes = decoder.open(&sealed, chunk.len).ok()?;
            crate::store::copy_part(&mut out, &bytes, chunk_start, start, end);
        }
        Some(out)
    }

    /// The blobs `manifest_hash`'s content is made of, as a [`ContentSource`] hands them over: the
    /// manifest first, then each distinct chunk. `None` when the manifest is not local.
    pub async fn content_blobs(&self, manifest_hash: Hash) -> Option<Vec<Hash>> {
        let manifest = self.local_manifest_index(manifest_hash).await?;
        let mut seen = std::collections::HashSet::new();
        let mut blobs = vec![manifest_hash];
        for chunk in manifest.chunks() {
            let chunk = Hash::from(chunk.hash);
            if seen.insert(chunk) {
                blobs.push(chunk);
            }
        }
        Some(blobs)
    }

    /// One blob of `manifest_hash`'s content ([`Self::content_blobs`]) when it is local: a loose blob as
    /// kept, a chunk read from the store's area. Never touches the network.
    pub async fn local_content_blob(&self, manifest_hash: Hash, blob: Hash) -> Option<Bytes> {
        if let Some(bytes) = self.store.local(blob).await {
            return Some(bytes);
        }
        let manifest = self.local_manifest_index(manifest_hash).await?;
        let index = manifest
            .chunks()
            .iter()
            .position(|chunk| Hash::from(chunk.hash) == blob)?;
        let range = manifest.chunk_range(index)?;
        let bytes = self
            .store
            .read_local(manifest_hash, &manifest, range)
            .await?;
        (Hash::new(&bytes) == blob).then(|| Bytes::from(bytes))
    }

    /// The manifest if it is cached or stored locally; never fetched.
    pub async fn local_manifest_index(&self, manifest_hash: Hash) -> Option<Arc<Manifest>> {
        if let Some(manifest) = self.manifests.get(&manifest_hash) {
            return Some(manifest);
        }
        let manifest = Arc::new(self.store.manifest(manifest_hash).await?);
        self.manifests.insert(manifest_hash, Arc::clone(&manifest));
        Some(manifest)
    }

    /// The byte length of `manifest_hash` when every byte of it is local; `None` when anything would
    /// have to be fetched. Never touches the network.
    pub async fn local_len(&self, manifest_hash: Hash) -> Option<u64> {
        let manifest = self.local_manifest_index(manifest_hash).await?;
        self.store
            .is_complete(manifest_hash, &manifest)
            .await
            .then(|| manifest.total_len())
    }

    /// Run `fut` to completion on the daemon runtime from a synchronous caller: a FUSE worker, the
    /// native backend or a test. This is the one bridge from those threads into async code.
    ///
    /// A FUSE worker that waits here is unavailable to every other process using the mount, so callers
    /// on one keep `fut` local or explicitly bounded (a preview's few KiB, a thumbnail). Open-ended
    /// hydration belongs on the runtime: history restores and conflict resolutions hydrate through a
    /// history job.
    pub fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        // Waiting here blocks the thread: only a synchronous caller may. A task of a
        // current-thread runtime, or a thread that entered one, would stop every task that
        // runtime drives, and tokio's own refusal names no caller. Refuse first, in words that
        // name the mistake.
        let in_current_thread_runtime = tokio::runtime::Handle::try_current().is_ok_and(|current| {
            current.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread
        });
        assert!(
            !in_current_thread_runtime && !tilo_rt::in_async_context(),
            "Hydrator::block_on was called from a task of a current-thread runtime, or a thread \
             that entered one. It is the bridge from a synchronous caller only (a FUSE worker, \
             the native backend, a test): await the async Hydrator API there, or run the work \
             that waits on a thread of its own that enters no runtime."
        );
        tokio::task::block_in_place(|| self.runtime_handle.block_on(fut))
    }

    /// Write the complete local content of `manifest_hash` to `dest` as a read-only file. `Ok(false)`,
    /// with nothing written, when any part is not local; this never fetches. File-backed content is
    /// cloned from the area where the filesystem shares extents.
    pub async fn export_local(&self, manifest_hash: Hash, dest: &std::path::Path) -> Result<bool> {
        use std::io::Write as _;

        let Some(manifest) = self.local_manifest_index(manifest_hash).await else {
            return Ok(false);
        };
        let dir = dest
            .parent()
            .ok_or_else(|| anyhow!("export destination has no parent directory"))?;
        std::fs::create_dir_all(dir)?;
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        if content::is_file_backed(&manifest) {
            if !self.store.is_complete(manifest_hash, &manifest).await {
                return Ok(false);
            }
            let files = Arc::clone(self.store.files());
            let key = *manifest_hash.as_bytes();
            let len = manifest.total_len();
            let target = file.as_file().try_clone()?;
            tokio::task::spawn_blocking(move || files.copy_to(&key, &(0..len), &target)).await??;
        } else {
            let decoder = self.decoder(&manifest, None).await?;
            for chunk in manifest.chunks() {
                let Some(bytes) = self.store.local(Hash::from(chunk.hash)).await else {
                    return Ok(false);
                };
                let bytes = match &decoder {
                    Some(decoder) => decoder.open(&bytes, chunk.len)?,
                    None => bytes,
                };
                file.write_all(&bytes)?;
            }
        }
        let mut read_only = file.as_file().metadata()?.permissions();
        read_only.set_readonly(true);
        file.as_file().set_permissions(read_only)?;
        file.persist(dest).map_err(|error| error.error)?;
        Ok(true)
    }

    /// The decoded manifest for `manifest_hash`, fetched once and then served from a bounded cache.
    pub async fn manifest_index(&self, manifest_hash: Hash) -> Result<Arc<Manifest>> {
        if let Some(manifest) = self.local_manifest_index(manifest_hash).await {
            return Ok(manifest);
        }
        let manifest = Arc::new(self.load_manifest(manifest_hash).await?);
        self.manifests.insert(manifest_hash, Arc::clone(&manifest));
        Ok(manifest)
    }

    /// Total byte length of a file from its manifest, fetching only the manifest.
    pub async fn content_len(&self, manifest_hash: Hash) -> Result<u64> {
        Ok(self.manifest_index(manifest_hash).await?.total_len())
    }

    /// Hydrate every byte of `manifest_hash` without assembling the file in memory: the manifest is
    /// fetched first, its declared size checked before any content is requested, then what is missing
    /// is fetched at predictive priority.
    ///
    /// `Ok(false)` when the file exceeds `max_bytes`, which is a policy refusal rather than a transport
    /// failure.
    pub async fn prefetch(&self, manifest_hash: Hash, max_bytes: u64) -> Result<bool> {
        self.prefetch_with_progress(manifest_hash, max_bytes, Priority::Predictive, |_, _| {})
            .await
    }

    /// Hydrate a complete file at `priority` while reporting verified file-byte progress. The callback
    /// is synchronous and cheap so callers can project progress without holding a namespace lock across
    /// network I/O.
    pub async fn prefetch_with_progress(
        &self,
        manifest_hash: Hash,
        max_bytes: u64,
        priority: Priority,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<bool> {
        let manifest = self.manifest_index(manifest_hash).await?;
        let total = manifest.total_len();
        if total > max_bytes {
            return Ok(false);
        }
        let mut completed = 0_u64;
        progress(completed, total);
        let window_bytes = batch::BATCH_BYTES.saturating_mul(batch::WINDOW_PER_PEER as u64);
        if content::is_file_backed(&manifest) {
            let mut at = 0_u64;
            while at < total {
                let end = at.saturating_add(window_bytes).min(total);
                self.ensure_range(manifest_hash, &manifest, at..end, priority)
                    .await?;
                completed = end;
                progress(completed, total);
                at = end;
            }
            if priority == Priority::Pinned {
                self.store.set_pinned(manifest_hash, true)?;
            }
            return Ok(true);
        }
        let chunks = manifest.chunks();
        let mut start = 0_usize;
        while start < chunks.len() {
            let mut end = start;
            let mut bytes = 0_u64;
            while let Some(chunk) = chunks.get(end) {
                if end > start && bytes >= window_bytes {
                    break;
                }
                bytes = bytes.saturating_add(u64::from(chunk.len));
                end = end.saturating_add(1);
            }
            let window = chunks.get(start..end).unwrap_or_default();
            self.fetch_many(
                window
                    .iter()
                    .map(|c| Want::chunk(Hash::from(c.hash), manifest.stored_len(c), manifest_hash))
                    .collect(),
            )
            .await;
            for chunk in window {
                let hash = Hash::from(chunk.hash);
                if !self.store.has(hash).await {
                    let _ = self
                        .read_chunk(hash, manifest.stored_len(chunk), manifest_hash)
                        .await?;
                }
            }
            completed = completed.saturating_add(bytes).min(total);
            progress(completed, total);
            start = end;
        }
        Ok(true)
    }

    fn count_blob_bytes(&self, bytes: u64) {
        self.blob_bytes
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn received_bytes(&self) -> (u64, u64) {
        (
            self.fetched_range_bytes(),
            self.blob_bytes.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    async fn load_manifest(&self, manifest_hash: Hash) -> Result<Manifest> {
        let bytes = self.read_blob(manifest_hash, 0, u64::MAX).await?;
        Ok(Manifest::decode(&bytes)?)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContentResidency {
    pub hydrated_bytes: u64,
    pub remote_only: bool,
    pub in_flight: bool,
    pub remotely_durable: bool,
    pub blobs: Vec<Hash>,
}

fn slice(bytes: &[u8], offset: u64, size: u64) -> Vec<u8> {
    // `offset`/`size` cross the FUSE boundary as untrusted request values, so
    // clamp rather than trust them to fit `usize` before bounding against `bytes.len()`.
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = start
        .saturating_add(usize::try_from(size).unwrap_or(usize::MAX))
        .min(bytes.len());
    bytes
        .get(start..end)
        .map(<[u8]>::to_vec)
        .unwrap_or_default()
}

/// Another content store on this device as the source: what a test reads one mount's bytes
/// from in another, as the workspace would hand them over.
#[doc(hidden)]
pub struct StoreSource(pub Arc<ContentStore>);

impl ContentSource for StoreSource {
    fn fetch(&self, hash: Hash) -> futures_util::future::BoxFuture<'static, Option<Bytes>> {
        let store = Arc::clone(&self.0);
        Box::pin(async move { store.local(hash).await })
    }
}

/// Whether reads and their fetches say where their time went (`TILO_FS_TRACE_READS=1`): read
/// once, so a read's hot path costs no environment lookup.
pub(crate) fn tracing_reads() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TILO_FS_TRACE_READS").is_some())
}

/// Whether a commit says where its time went (`TILO_FS_TRACE_WRITES=1`).
pub(crate) fn tracing_writes() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TILO_FS_TRACE_WRITES").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// The other device, serving content over its own hydrator.
    struct OtherDevice(Arc<Hydrator>, Hash);

    impl ContentSource for OtherDevice {
        fn fetch(&self, hash: Hash) -> futures_util::future::BoxFuture<'static, Option<Bytes>> {
            let (hydrator, content) = (Arc::clone(&self.0), self.1);
            Box::pin(async move { hydrator.local_content_blob(content, hash).await })
        }
    }

    #[tokio::test]
    async fn a_read_waits_its_bound_while_the_fetch_it_started_keeps_going() {
        let bound = Duration::from_millis(200);
        let started = Instant::now();
        let never = bounded_read(bound, std::future::pending::<Result<()>>()).await;
        let waited = started.elapsed();
        let error = never.expect_err("a read whose bytes never arrive fails");
        assert!(error.to_string().contains("within"), "{error}");
        assert!(waited >= bound, "failed before its bound: {waited:?}");
        assert!(
            waited < bound.saturating_mul(10),
            "held far past its bound: {waited:?}"
        );

        let (done, landed) = tokio::sync::oneshot::channel();
        let slow = bounded_read(bound, async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = done.send(());
            Ok(())
        })
        .await;
        assert!(slow.is_err(), "a read past its bound fails");
        tokio::time::timeout(Duration::from_secs(5), landed)
            .await
            .expect("the fetch kept going after its reader failed")
            .expect("the fetch finished");

        let quick = bounded_read(bound, async { Ok(7) }).await.unwrap();
        assert_eq!(quick, 7, "a read inside its bound returns its bytes");
    }

    /// Bytes this device holds are read here; anything else arrives from the content source,
    /// whole or as a range, once however many readers ask at once.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each section keeps one former test's body verbatim"
    )]
    async fn content_this_device_lacks_arrives_from_the_source_whole_or_ranged() {
        {
            let store = Arc::new(ContentStore::memory());
            let hash = store.write_content(b"hello local").await.unwrap().unwrap();
            let hydrator = Hydrator::new(store);
            let got = hydrator.read(hash, 0, 11).await.unwrap();
            assert_eq!(&got, b"hello local");
            assert_eq!(&hydrator.read(hash, 6, 5).await.unwrap(), b"local");

            let data: Vec<u8> = (0..200_000_u32)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect();
            let store = Arc::new(ContentStore::memory());
            let hash = store.write_content(&data).await.unwrap().unwrap();
            let hydrator = Hydrator::new(store);
            assert_eq!(
                hydrator.read_local(hash, 70_000, 50_000).await.unwrap(),
                data[70_000..120_000]
            );
            assert!(
                hydrator
                    .read_local(Hash::from([9; 32]), 0, 10)
                    .await
                    .is_none(),
                "a manifest that is not local is not read"
            );
        }
        {
            for length in [11_usize, 300_000, 3 * 1024 * 1024] {
                let written: Vec<u8> = (0..length)
                    .map(|i| {
                        (u32::try_from(i).unwrap().wrapping_mul(2_654_435_761) >> 13).to_le_bytes()
                            [0]
                    })
                    .collect();
                let there = Arc::new(ContentStore::memory());
                let content = there.write_content(&written).await.unwrap().unwrap();
                let there = Arc::new(Hydrator::new(there));
                let blobs = there.content_blobs(content).await.unwrap();
                assert_eq!(blobs[0], content, "the manifest comes first");
                for blob in &blobs {
                    assert!(there.local_content_blob(content, *blob).await.is_some());
                }

                let here = Hydrator::new(Arc::new(ContentStore::memory()))
                    .with_source(Arc::new(OtherDevice(Arc::clone(&there), content)));
                assert_eq!(here.content_len(content).await.unwrap(), length as u64);
                assert_eq!(here.read(content, 0, length as u64).await.unwrap(), written);
                for (off, len) in [
                    (0usize, 100usize),
                    (8000, 20000),
                    (length / 2, 1),
                    (length.saturating_sub(2000), 2000),
                ] {
                    if off >= length {
                        continue;
                    }
                    let end = (off + len).min(length);
                    assert_eq!(
                        here.read(content, off as u64, (end - off) as u64)
                            .await
                            .unwrap(),
                        &written[off..end],
                        "range {off}+{len} of {length}"
                    );
                }
            }

            let nowhere =
                Hydrator::new(Arc::new(ContentStore::memory())).with_source(Arc::new(OtherDevice(
                    Arc::new(Hydrator::new(Arc::new(ContentStore::memory()))),
                    Hash::from([3; 32]),
                )));
            assert!(
                nowhere.read(Hash::from([3; 32]), 0, 10).await.is_err(),
                "content the source lacks does not read"
            );
        }
        {
            let there = Arc::new(ContentStore::memory());
            let payload = Bytes::from_static(b"collaborative filesystem payload");
            let hash = there
                .write_content(payload.as_ref())
                .await
                .unwrap()
                .unwrap();
            let here = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(
                Hydrator::new(Arc::clone(&here)).with_source(Arc::new(StoreSource(there))),
            );
            assert!(!here.has(hash).await, "precondition: nothing here yet");
            let readers: Vec<_> = (0..8)
                .map(|_| {
                    let hydrator = Arc::clone(&hydrator);
                    let len = payload.len() as u64;
                    tokio::spawn(async move { hydrator.read(hash, 0, len).await.unwrap() })
                })
                .collect();
            for reader in readers {
                assert_eq!(reader.await.unwrap().as_slice(), payload.as_ref());
            }
            assert!(here.has(hash).await, "the fetched blob is kept here");
            assert!(
                hydrator.residency(hash).await.remotely_durable,
                "what the workspace handed over is kept elsewhere too"
            );
        }
    }

    /// What the mount shows a reader before and during a fetch, and what prefetch takes.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each section keeps one former test's body verbatim"
    )]
    async fn sync_state_and_prefetch_report_what_a_read_will_cost() {
        {
            let store = Arc::new(ContentStore::memory());
            let data: Vec<u8> = (0..250_000u32)
                .map(|i| u8::try_from((i.wrapping_mul(2_654_435_761) >> 11) & 0xff).unwrap())
                .collect();
            let mh = store.write_content(&data).await.unwrap().unwrap();
            let hyd = Hydrator::new(store);
            assert_eq!(
                hyd.sync_state(mh).await,
                SyncState::Local,
                "fully-ingested file is local"
            );

            let guard = hyd.mark_fetching(mh);
            assert!(hyd.is_fetching(mh));
            assert_eq!(
                hyd.sync_state(mh).await,
                SyncState::Downloading,
                "an in-flight fetch of the manifest reads as downloading",
            );
            drop(guard);
            assert!(!hyd.is_fetching(mh));
            assert_eq!(
                hyd.sync_state(mh).await,
                SyncState::Local,
                "cleared after the fetch"
            );
        }
        {
            let hyd = Hydrator::new(Arc::new(ContentStore::memory()));
            assert_eq!(
                hyd.sync_state(Hash::from([7u8; 32])).await,
                SyncState::Projected,
                "unknown manifest is projected"
            );
        }
        {
            let store = Arc::new(ContentStore::memory());
            let present = store
                .ingest(Bytes::from_static(b"chunk that exists"))
                .await
                .unwrap();
            let missing = Hash::from([9u8; 32]);
            let manifest = crate::content::Manifest::new(
                crate::content::ChunkTier::Small,
                vec![
                    crate::content::ChunkRef {
                        hash: *present.as_bytes(),
                        len: 17,
                    },
                    crate::content::ChunkRef {
                        hash: *missing.as_bytes(),
                        len: 42,
                    },
                ],
            );
            let mh = store.ingest(Bytes::from(manifest.encode())).await.unwrap();
            let hyd = Hydrator::new(store);
            assert_eq!(
                hyd.sync_state(mh).await,
                SyncState::Projected,
                "a locally-known manifest with a missing chunk is still projected",
            );
            assert!(
                hyd.read(mh, 20, 20).await.is_err(),
                "a range only a missing chunk holds is not read"
            );
        }
        {
            let store = Arc::new(ContentStore::memory());
            let mh = store
                .write_content(b"already local")
                .await
                .unwrap()
                .unwrap();
            let hyd = Hydrator::new(store);
            let first = hyd.mark_fetching(mh);
            let second = hyd.mark_fetching(mh);
            let other = hyd.mark_fetching(Hash::from([0xB2; 32]));
            drop(first);
            drop(other);
            assert!(
                hyd.is_fetching(mh),
                "dropping one of two guards must not clear the shared state"
            );
            assert!(
                !hyd.is_fetching(Hash::from([0xB2; 32])),
                "another content's only guard was released"
            );
            drop(second);
            assert!(!hyd.is_fetching(mh));
        }
        {
            let server_store = Arc::new(ContentStore::memory());
            let payload: Vec<u8> = (0..400_000u32)
                .map(|i| (i.wrapping_mul(2_654_435_761) >> 11).to_le_bytes()[0])
                .collect();
            let manifest_hash = server_store.write_content(&payload).await.unwrap().unwrap();
            let hydrator = Hydrator::new(Arc::new(ContentStore::memory()))
                .with_source(Arc::new(StoreSource(server_store)));
            assert_eq!(
                hydrator.sync_state(manifest_hash).await,
                SyncState::Projected
            );
            assert!(
                hydrator
                    .prefetch(manifest_hash, payload.len() as u64)
                    .await
                    .unwrap(),
                "the bounded file is accepted for prefetch"
            );
            assert_eq!(
                hydrator.sync_state(manifest_hash).await,
                SyncState::Local,
                "manifest and every content chunk are local afterwards"
            );
            assert_eq!(
                hydrator
                    .read(manifest_hash, 0, payload.len() as u64)
                    .await
                    .unwrap(),
                payload
            );

            let store = Arc::new(ContentStore::memory());
            let payload = vec![7u8; 1024];
            let manifest_hash = store.write_content(&payload).await.unwrap().unwrap();
            let hydrator = Hydrator::new(store);
            assert!(
                !hydrator.prefetch(manifest_hash, 512).await.unwrap(),
                "a file over the policy's limit is not prefetched"
            );
        }
    }

    /// The synchronous bridge waits on the calling thread. A task of the process's
    /// current-thread runtime drives every task on that one thread, so waiting there is a
    /// mistake, and the refusal says so rather than tokio's own message naming no caller.
    #[test]
    fn the_bridge_refuses_to_wait_on_a_task_of_the_processes_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            tilo_rt::app()
                .block_on(ContentStore::open(dir.path().join("content")))
                .unwrap()
                .unwrap(),
        );
        let (told, heard) = std::sync::mpsc::channel();
        tilo_rt::app().spawn(async move {
            let hydrator = Hydrator::new(store);
            let mistake =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hydrator.block_on(async {})))
                    .expect_err("waiting on a task of the runtime is refused");
            let said = mistake
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    mistake
                        .downcast_ref::<&str>()
                        .map(|said| (*said).to_owned())
                })
                .unwrap_or_default();
            let _ = told.send(said);
        });
        let said = heard
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the task was answered");
        assert!(
            said.contains("Hydrator::block_on was called"),
            "the refusal names the bridge: {said}"
        );
        assert!(
            said.contains("thread of its own"),
            "the refusal says where the work belongs: {said}"
        );
    }
}
