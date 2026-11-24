//! The content store: one content-addressed store with one hash space.
//!
//! Everything lives in the [`FileArea`]: small blobs (manifests, the chunks of small files,
//! thumbnails) inline in its index database, a published file's content as one sparse file per
//! content id with a chunk index so a chunk is served and deduplicated where it lives, and a whole
//! blob too large to inline as an entry keyed by its own hash with its BAO outboard persisted beside
//! it. On a Tier 1 store volume the area sits next to the workspaces, so publishing a file clones its
//! extents instead of copying bytes. iroh-blobs is only the wire protocol and its client: what peers
//! fetch is served out of this area.
//!
//! The area is also the daemon's bounded cache: resident bytes of file-backed content are kept under
//! a byte budget, least recently used first, and pinned or local-only content is never evicted. Inline
//! blobs are inside the index database and outside the budget. A `moka` cache in front of the inline
//! blobs bounds hot reads by bytes ([`BLOB_CACHE_BYTES`]).

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use bao_tree::BaoTree;
use bao_tree::io::mixed::{EncodedItem, traverse_ranges_validated};
use bao_tree::io::outboard::{PostOrderMemOutboard, PreOrderMemOutboard};
use bytes::Bytes;
use futures_util::StreamExt;
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::protocol::ChunkRangesExt as _;
use iroh_blobs::provider::events::EventSender;
use iroh_blobs::store::IROH_BLOCK_SIZE;
use iroh_blobs::{BlobsProtocol, Hash};
use moka::sync::Cache;

#[cfg(feature = "filesystem")]
use crate::availability::{AvailabilityRegistry, EvictionRequirements};
#[cfg(feature = "filesystem")]
use crate::eviction::{
    BlobExtent, ContentPayload, EvictionAuthorizationError, EvictionCatalog, EvictionObservation,
    EvictionQueue, EvictionRequestId, ObservedBlobState,
};
#[cfg(feature = "filesystem")]
use crate::namespace::{LiveDocumentBlobRoots, LiveDocumentBlobScanError, Namespace};
use tilo_lock::MutexRecover as _;

pub mod area;
/// Whether `files` holds all of `key`: inline, as a whole blob, or as a resident chunk.
fn area_holds(files: &FileArea, key: &[u8; 32]) -> bool {
    files.has_inline(key)
        || (files.is_blob(key) && files.entry(key).is_some_and(|entry| entry.complete()))
        || files.find_chunk(key).is_some()
}

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

/// Largest blob stored inline, in the area's index database, rather than as a file of its own. It
/// covers every manifest and every chunk of a small file; a larger whole blob is a file entry with a
/// persisted outboard, and a manifest at the same key as a file-backed content id always stays inline
/// so the two never collide.
pub const INLINE_MAX: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "filesystem")]
pub struct BlobDocumentLookupLimits {
    pub max_live_documents: usize,
    pub max_manifest_bytes: usize,
    pub max_chunks_per_document: usize,
}

#[cfg(feature = "filesystem")]
impl Default for BlobDocumentLookupLimits {
    fn default() -> Self {
        Self {
            max_live_documents: 16_384,
            max_manifest_bytes: 2 * 1024 * 1024,
            max_chunks_per_document: 50_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[cfg(feature = "filesystem")]
pub enum BlobDocumentLookupError {
    #[error("invalid bounded blob-record lookup limits")]
    InvalidLimits,
    #[error(transparent)]
    Namespace(#[from] LiveDocumentBlobScanError),
    #[error("live record {document_id:?} manifest {manifest} is not locally available")]
    ManifestUnavailable {
        document_id: fs_protocol::FileDocumentId,
        manifest: Hash,
    },
    #[error("live record {document_id:?} manifest exceeds {limit} encoded bytes")]
    ManifestTooLarge {
        document_id: fs_protocol::FileDocumentId,
        limit: usize,
    },
    #[error("live record {document_id:?} has a malformed manifest")]
    MalformedManifest {
        document_id: fs_protocol::FileDocumentId,
    },
    #[error("live record {document_id:?} manifest exceeds {limit} chunks")]
    TooManyChunks {
        document_id: fs_protocol::FileDocumentId,
        limit: usize,
    },
}

/// Explicit reclamation errors.
#[derive(Debug, thiserror::Error)]
#[cfg(feature = "filesystem")]
pub enum StoreEvictionError {
    #[error(transparent)]
    Authorization(#[from] EvictionAuthorizationError),
    #[error("removing evicted blobs failed: {0}")]
    Remove(String),
}

/// A file stored as a version: its content id and length (`None` for an empty file), and whether
/// the file itself became the stored entry.
#[cfg(feature = "filesystem")]
pub(crate) type Published = (Option<(iroh_blobs::Hash, u64)>, bool);

pub struct ContentStore {
    cache: Cache<Hash, Bytes>,
    /// Serializes publication with eviction, so a blob being written is not removed out from under
    /// its writer.
    mutation: tokio::sync::Mutex<()>,
    /// Open deferred-durability groups, and whether a write inside one still waits for its database
    /// sync; see [`ContentStore::defer_sync`].
    deferred: std::sync::Mutex<(usize, bool)>,
    write_syncs: std::sync::atomic::AtomicU64,
    /// Whether a write since the last sync has not reached the disk.
    pending: std::sync::atomic::AtomicBool,
    /// Whether this store syncs on a window ([`Self::set_sync_writes`]`(false)`) instead of on
    /// every write, which a deferred group's end must not undo.
    batched: std::sync::atomic::AtomicBool,
    read_bytes: std::sync::atomic::AtomicU64,
    files: Arc<FileArea>,
    _scratch: Option<tempfile::TempDir>,
}

/// Heap the hot cache in front of inline blobs may hold, counting each entry's bookkeeping. For what
/// is read again and again and is small: manifests, the chunks of small files, thumbnails.
pub const BLOB_CACHE_BYTES: u64 = 32 * 1024 * 1024;

/// Largest blob the hot cache admits. Anything larger has a file of its own and is read by range, and
/// one such blob would take a large share of the budget for a single read.
pub const MAX_CACHED_BLOB: usize = 1024 * 1024;

/// What a cache entry costs beyond its bytes: the key, the `Bytes` handle and the cache's node.
const CACHE_ENTRY_OVERHEAD: u32 = 256;

/// An insert at least this large settles the cache at once. moka applies writes in batches of 64, so
/// a run of large inserts could otherwise sit above the budget until the next batch.
const CACHE_SETTLE_AT: usize = 64 * 1024;

/// The hot cache in front of inline blobs, bounded by bytes. Its admission weighs a newcomer's access
/// frequency against that of every entry it would evict, so a run of cold large reads does not flush
/// small hot entries.
/// A blob's bytes: inline in the index database, or a whole blob's file with its outboard.
enum Either {
    Inline(Vec<u8>),
    /// A complete whole blob of `size` bytes asked for no bytes of it: only its length answers,
    /// so neither its file nor its outboard is read.
    Length(u64),
    Whole {
        size: u64,
        file: File,
        outboard: Vec<u8>,
    },
}

/// Copies the bytes `start..end` out of the verified leaves of a BAO stream as each arrives, so a
/// range read holds the range once rather than its leaves and then their copy.
struct Collector {
    start: u64,
    end: u64,
    out: Vec<u8>,
}

impl Collector {
    fn new(start: u64, end: u64) -> Self {
        let len = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
        Self {
            start,
            end,
            out: Vec::with_capacity(len),
        }
    }
}

impl bao_tree::io::mixed::Sender for Collector {
    type Error = std::convert::Infallible;

    async fn send(&mut self, item: EncodedItem) -> Result<(), Self::Error> {
        if let EncodedItem::Leaf(leaf) = item {
            content::copy_part(&mut self.out, &leaf.data, leaf.offset, self.start, self.end);
        }
        Ok(())
    }
}

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

fn files_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("files")
}

/// Budget for cached file-backed content when the caller sets none: a quarter of the filesystem that
/// holds it.
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

/// The BAO outboard of a blob of `len` bytes, read from `source`, in pre order without a length
/// suffix, as the area persists it. The tree's root is the blob's own hash, so a peer verifies what
/// it receives against the hash it asked for.
fn outboard_of_reader(source: impl io::Read, len: u64) -> io::Result<Vec<u8>> {
    let tree = BaoTree::new(len, IROH_BLOCK_SIZE);
    let mut data =
        Vec::with_capacity(usize::try_from(tree.outboard_size()).map_err(io::Error::other)?);
    let root = bao_tree::io::sync::outboard_post_order(source.take(len), tree, &mut data)?;
    let post = PostOrderMemOutboard { root, tree, data };
    Ok(post.flip().data)
}

impl ContentStore {
    /// Open the persistent store rooted at `cache_dir` (created if missing).
    pub async fn open(cache_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = cache_dir.as_ref();
        Self::open_with_files(dir, &files_dir(dir)).await
    }

    /// [`Self::open`] with the file-backed content under `files_dir`, for example on the store volume
    /// next to the workspaces.
    pub async fn open_with_files(cache_dir: impl AsRef<Path>, files_dir: &Path) -> Result<Self> {
        let dir = cache_dir.as_ref().to_owned();
        let files_dir = files_dir.to_owned();
        let files = tokio::task::spawn_blocking(move || -> std::io::Result<Arc<FileArea>> {
            std::fs::create_dir_all(&dir)?;
            let budget = default_budget(&files_dir);
            std::fs::create_dir_all(&files_dir)?;
            Ok(Arc::new(FileArea::open(&files_dir, budget)?))
        })
        .await??;
        Ok(Self {
            cache: blob_cache(),
            mutation: tokio::sync::Mutex::new(()),
            deferred: std::sync::Mutex::new((0, false)),
            write_syncs: std::sync::atomic::AtomicU64::new(0),
            pending: std::sync::atomic::AtomicBool::new(false),
            batched: std::sync::atomic::AtomicBool::new(false),
            read_bytes: std::sync::atomic::AtomicU64::new(0),
            files,
            _scratch: None,
        })
    }

    /// Whether `error` from [`Self::open`] is another process still holding the store's database
    /// open. It is redb's, and redb says a database it cannot lock with this sentence, wherever the
    /// open that met it ended up.
    pub fn held_elsewhere(error: &anyhow::Error) -> bool {
        format!("{error:#}").contains("Database already open")
    }

    /// The whole of file version `content` from the persistent store at `cache_dir`, read without
    /// opening it: for another process beside the one that has it open, which must not take its
    /// database locks. Only file-backed versions live where such a reader can find them; any other is
    /// `Ok(None)`, as is one longer than `max_bytes`.
    ///
    /// Nothing is verified and the version may be only partly resident: the caller checks the bytes
    /// against a hash it trusts.
    pub fn read_file_version_unlocked(
        cache_dir: &Path,
        content: [u8; 32],
        max_bytes: u64,
    ) -> std::io::Result<Option<Vec<u8>>> {
        area::read_entry_unlocked(&files_dir(cache_dir), &content, max_bytes)
    }

    /// An ephemeral store (tests, and as a fallback): everything in memory and in a temporary
    /// directory removed with the store.
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
        Self {
            cache: blob_cache(),
            mutation: tokio::sync::Mutex::new(()),
            deferred: std::sync::Mutex::new((0, false)),
            write_syncs: std::sync::atomic::AtomicU64::new(0),
            pending: std::sync::atomic::AtomicBool::new(false),
            batched: std::sync::atomic::AtomicBool::new(false),
            read_bytes: std::sync::atomic::AtomicU64::new(0),
            files,
            _scratch: scratch,
        }
    }

    /// Shuts the store down and releases its database, so the same directory can be opened again in
    /// this process while handles to this store remain; they fail from now on.
    pub async fn shutdown(&self) -> Result<()> {
        self.flush().await?;
        self.files.close();
        Ok(())
    }

    /// Flush and close the store so another process may open the same directory, and end it.
    pub async fn close(self) -> Result<()> {
        let files = Arc::clone(&self.files);
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            files.flush()?;
            files.close();
            Ok(())
        })
        .await??;
        Ok(())
    }

    pub const fn files(&self) -> &Arc<FileArea> {
        &self.files
    }

    /// Opens a deferred-durability group: until the last open group ends
    /// ([`Self::end_deferred_sync`]), writes skip their database sync and the group's end syncs once,
    /// so a changeset of many small files costs one sync instead of one per file. Callers end the group
    /// before anything depending on the writes is made durable or announced.
    pub fn defer_sync(&self) {
        {
            let mut deferred = self.deferred.lock_recover();
            deferred.0 = deferred.0.saturating_add(1);
        }
        self.files.set_sync_writes(false);
    }

    /// Make every write this store accepted so far durable with one sync, when anything was
    /// written since the last one; a mount that syncs on a short window calls this from its
    /// flusher and from `fsync`. Its writes are committed without waiting for the disk and kept in
    /// the area's log of such commits, so until this returns they survive the process being
    /// killed but not a power loss. The entry files those writes created are fsynced first, so a
    /// barrier that makes the index durable names only bytes that are.
    ///
    /// # Errors
    /// The database could not be synced; the next call tries again.
    pub fn sync_pending(&self) -> std::io::Result<()> {
        let pending = self
            .pending
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        if !pending && !self.files.has_unsynced() && !self.files.has_unsynced_data() {
            return Ok(());
        }
        // The bytes a window wrote, then the index that names them: a name this barrier makes
        // durable never points at bytes that are not. `sync_written` makes the index durable
        // itself when it had bytes to sync; otherwise the database still needs its sync.
        let durable_index = match self.files.sync_written() {
            Ok(durable) => durable,
            Err(error) => {
                self.pending
                    .store(true, std::sync::atomic::Ordering::Release);
                return Err(error);
            }
        };
        if durable_index {
            self.write_syncs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        match self.files.sync_db() {
            Ok(()) => {
                self.write_syncs
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.pending
                    .store(true, std::sync::atomic::Ordering::Release);
                Err(error)
            }
        }
    }

    /// Whether each content write waits for the disk (the default), or a barrier syncs a window of
    /// them ([`Self::sync_pending`]): a mount serving many small files turns the per-write sync off
    /// and syncs on a short window and on `fsync`.
    pub fn set_sync_writes(&self, sync: bool) {
        self.batched
            .store(!sync, std::sync::atomic::Ordering::Release);
        self.files.set_sync_writes(sync);
    }

    /// How many database syncs writes to this store have made: one per write outside a
    /// deferred-durability group, one per group that wrote. Each is an fsync, so a caller can check
    /// that it pays for one rather than one per blob.
    pub fn write_syncs(&self) -> u64 {
        self.write_syncs.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many bytes reads of this store have taken: whole blobs from [`Self::local`], the verified
    /// leaves of [`Self::read_blob_range`], and file-backed content. A ranged read pays for its range
    /// only.
    pub fn read_bytes(&self) -> u64 {
        self.read_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_add(self.files.read_bytes())
    }

    fn note_read(&self, bytes: usize) {
        self.read_bytes
            .fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// Closes a group opened by [`Self::defer_sync`]; the last one syncs the database if a write inside
    /// the groups skipped it. The writes stay pending for the next sync when the sync fails.
    pub async fn end_deferred_sync(&self) -> Result<()> {
        let (sync, outermost) = {
            let mut deferred = self.deferred.lock_recover();
            deferred.0 = deferred.0.saturating_sub(1);
            let sync = deferred.0 == 0 && deferred.1;
            if sync {
                deferred.1 = false;
            }
            (sync, deferred.0 == 0)
        };
        if outermost && !self.batched.load(std::sync::atomic::Ordering::Acquire) {
            // Every write after the last group is durable on its own again, unless this store
            // syncs on a window.
            self.files.set_sync_writes(true);
        }
        if sync {
            self.write_syncs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let files = Arc::clone(&self.files);
            let synced = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                // The group's content bytes, then the index that names them.
                if !files.sync_written()? {
                    files.sync_db()?;
                }
                Ok(())
            })
            .await?;
            if let Err(error) = synced {
                self.files.set_sync_writes(false);
                self.deferred.lock_recover().1 = true;
                return Err(error.into());
            }
        }
        Ok(())
    }

    /// Make everything written so far durable: the index and the files it names.
    pub async fn flush(&self) -> Result<()> {
        let files = Arc::clone(&self.files);
        tokio::task::spawn_blocking(move || files.flush()).await??;
        Ok(())
    }

    /// Build a `BlobsProtocol` handler backed by this store, serving inline blobs, whole blobs and
    /// file-backed chunks alike.
    pub fn blobs_protocol(&self, events: Option<EventSender>) -> BlobsProtocol {
        let store = serve::serving_store(&self.files);
        BlobsProtocol::new(&store, events)
    }

    pub async fn ingest(&self, data: impl Into<Bytes>) -> Result<Hash> {
        let _mutation = self.mutation.lock().await;
        self.persist_owned(data.into()).await
    }

    /// Locally available bytes of blob `hash` (cache, inline storage, a whole blob's file, or a
    /// file-backed chunk, each entry file checked against its hash), or `None`.
    pub async fn local(&self, hash: Hash) -> Option<Bytes> {
        if let Some(b) = self.cache.get(&hash) {
            self.note_read(b.len());
            return Some(b);
        }
        let files = Arc::clone(&self.files);
        let whole = tokio::task::spawn_blocking(move || whole_blob(&files, hash))
            .await
            .ok()
            .flatten();
        if let Some(bytes) = self.took_whole(hash, whole) {
            return Some(bytes);
        }
        self.local_chunk(hash).await
    }

    /// [`Self::local`], read on the calling thread: for a caller that answers from a thread of
    /// its own, where a read handed to another thread and waited for costs more than the read.
    pub fn local_now(&self, hash: Hash) -> Option<Bytes> {
        if let Some(b) = self.cache.get(&hash) {
            self.note_read(b.len());
            return Some(b);
        }
        if let Some(bytes) = self.took_whole(hash, whole_blob(&self.files, hash)) {
            return Some(bytes);
        }
        file_backed_chunk(&self.files, hash)
    }

    /// The whole blob `hash` read from where it lives, counted and cached.
    fn took_whole(&self, hash: Hash, whole: Option<(Bytes, bool)>) -> Option<Bytes> {
        let (bytes, inline) = whole?;
        // The area counts what it reads of a whole blob's file itself; an inline blob is counted
        // here, once.
        if inline {
            self.note_read(bytes.len());
        }
        self.cache_insert(hash, bytes.clone());
        Some(bytes)
    }

    /// Read-only bytes of a whole blob `hash`: inline, or a complete whole-blob entry's file. Partial
    /// file-backed versions are not blobs and never answer here.
    #[cfg(all(test, feature = "filesystem"))]
    async fn blob_of_bytes(&self, hash: Hash) -> Option<Vec<u8>> {
        let files = Arc::clone(&self.files);
        let key = *hash.as_bytes();
        tokio::task::spawn_blocking(move || {
            if let Some(bytes) = files.get_inline(&key) {
                return Some(bytes);
            }
            if !files.is_blob(&key) {
                return None;
            }
            let entry = files.entry(&key)?;
            entry
                .complete()
                .then(|| files.read_at(&key, 0, entry.size).ok())
                .flatten()
        })
        .await
        .ok()
        .flatten()
    }

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

    /// Import content that arrives as a stream of pieces, hashing and writing each as it comes: the
    /// import is staged to a file, never held whole in memory, and becomes an inline blob or a whole
    /// blob's file entry with its outboard once its hash is known. Returns the content's hash and
    /// length.
    ///
    /// The stream is consumed before any lock is taken, so a slow producer holds up nothing else. A
    /// piece that is an error abandons the import: nothing is stored and the staging file is removed.
    pub async fn ingest_stream<S>(&self, pieces: S) -> Result<(Hash, u64)>
    where
        S: futures_util::Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static,
    {
        let mut pieces = Box::pin(pieces);
        let (path, mut file) = self.files.staging_file()?;
        let mut hasher = blake3::Hasher::new();
        let mut len = 0_u64;
        let abandon = |path: &Path| {
            let _ = std::fs::remove_file(path);
        };
        while let Some(piece) = pieces.next().await {
            let piece = match piece {
                Ok(piece) => piece,
                Err(error) => {
                    abandon(&path);
                    return Err(error.into());
                }
            };
            hasher.update(&piece);
            if let Err(error) = file.write_all(&piece) {
                abandon(&path);
                return Err(error.into());
            }
            len = len.saturating_add(piece.len() as u64);
        }
        drop(file);
        let hash = Hash::from(*hasher.finalize().as_bytes());
        let _mutation = self.mutation.lock().await;
        let files = Arc::clone(&self.files);
        let staged = path.clone();
        let stored = tokio::task::spawn_blocking(move || -> io::Result<()> {
            if len <= INLINE_MAX as u64 {
                let bytes = std::fs::read(&staged)?;
                files.put_inline([(*hash.as_bytes(), Bytes::from(bytes))])?;
                std::fs::remove_file(&staged)
            } else {
                let mut source = File::open(&staged)?;
                let outboard = outboard_of_reader(&mut source, len)?;
                files.install_staged_blob(hash.as_bytes(), &staged, len, &outboard)
            }
        })
        .await;
        match stored {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                abandon(&path);
                return Err(error.into());
            }
            Err(error) => {
                abandon(&path);
                return Err(error.into());
            }
        }
        self.note_write_durability();
        Ok((hash, len))
    }

    /// Read a clipped range and the whole length of a locally complete blob. Inline bytes and whole
    /// blobs are read from where they live; a whole blob's range is BAO-verified against its stored
    /// outboard, and file-backed chunks are bounded and hash-checked. Never fills the blob cache.
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
        let key = *hash.as_bytes();
        let files = Arc::clone(&self.files);
        let wanted = range.clone();
        let read = tokio::task::spawn_blocking(move || {
            if let Some(bytes) = files.get_inline(&key) {
                return Some(Either::Inline(bytes));
            }
            if files.is_blob(&key) {
                let entry = files.entry(&key)?;
                if !entry.complete() {
                    return None;
                }
                if wanted.start.min(entry.size) >= wanted.end.min(entry.size) {
                    return Some(Either::Length(entry.size));
                }
                let outboard = files.outboard(&key)?;
                let file = files.file(&key).ok()?.as_ref().try_clone().ok()?;
                return Some(Either::Whole {
                    size: entry.size,
                    file,
                    outboard,
                });
            }
            None
        })
        .await
        .ok()
        .flatten();
        match read {
            Some(Either::Inline(bytes)) => {
                let mut out = Vec::new();
                content::copy_part(&mut out, &bytes, 0, range.start, range.end);
                self.note_read(out.len());
                Some((out, bytes.len() as u64))
            }
            Some(Either::Length(size)) => Some((Vec::new(), size)),
            Some(Either::Whole {
                size,
                file,
                outboard,
            }) => {
                let start = range.start.min(size);
                let end = range.end.min(size);
                let tree = BaoTree::new(size, IROH_BLOCK_SIZE);
                if outboard.len() as u64 != tree.outboard_size() {
                    return None;
                }
                let mut out = Vec::new();
                if start < end {
                    let ranges = iroh_blobs::protocol::ChunkRanges::bytes(start..end);
                    let outboard = PreOrderMemOutboard {
                        root: bao_tree::blake3::Hash::from(key),
                        tree,
                        data: outboard,
                    };
                    let mut collected = Collector::new(start, end);
                    traverse_ranges_validated(file, outboard, &ranges, &mut collected)
                        .await
                        .ok()?;
                    out = collected.out;
                    if out.len() as u64 != end.saturating_sub(start) {
                        return None;
                    }
                    self.note_read(out.len());
                }
                Some((out, size))
            }
            None => {
                let bytes = self.local_chunk(hash).await?;
                let mut out = Vec::new();
                content::copy_part(&mut out, &bytes, 0, range.start, range.end);
                Some((out, bytes.len() as u64))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_cached(&self, hash: Hash) -> bool {
        self.cache.contains_key(&hash)
    }

    #[cfg(test)]
    pub(crate) fn forget_cached(&self, hash: Hash) {
        self.cache.invalidate(&hash);
    }

    #[cfg(test)]
    pub(crate) fn cached_bytes(&self) -> u64 {
        self.cache.run_pending_tasks();
        self.cache.weighted_size()
    }

    pub async fn store_fetched(&self, hash: Hash, data: Bytes) -> Result<()> {
        let actual = Hash::new(&data);
        anyhow::ensure!(
            actual == hash,
            "fetched blob hash mismatch: expected {hash}, got {actual}"
        );
        let actual = self.persist_owned(data).await?;
        debug_assert_eq!(actual, hash);
        Ok(())
    }

    /// [`Self::store_fetched`] for many blobs with one durability barrier, so a batch of small files
    /// costs one database sync. Every blob is checked against its hash before anything is stored; a
    /// mismatch rejects the whole batch.
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

    /// Remove exactly `selected` from the store: an inline blob, a whole blob's file entry, or the
    /// entry a chunk lives in. Returns how many of the selected blobs were present.
    pub async fn evict_blobs(&self, selected: &HashSet<Hash>) -> Result<u64> {
        if selected.is_empty() {
            return Ok(0);
        }
        let _mutation = self.mutation.lock().await;
        let hashes: Vec<Hash> = selected.iter().copied().collect();
        let files = Arc::clone(&self.files);
        let inside = hashes.clone();
        let removed = tokio::task::spawn_blocking(move || -> io::Result<u64> {
            let mut removed = 0_u64;
            for hash in inside {
                let key = *hash.as_bytes();
                // File-backed content goes with any of its hashes: its content id or one of its
                // chunks.
                let mut gone = files.remove_inline(&key)?;
                if !gone {
                    if files.entry(&key).is_some() {
                        files.remove(&key)?;
                        gone = true;
                    } else if let Some(location) = files.find_chunk(&key) {
                        files.remove(&location.key)?;
                        gone = true;
                    }
                }
                if gone {
                    removed = removed.saturating_add(1);
                }
            }
            files.sync_db()?;
            Ok(removed)
        })
        .await??;
        for hash in hashes {
            self.cache.invalidate(&hash);
        }
        Ok(removed)
    }

    /// Note the writes just made durable, or that a deferred-durability group owes their one
    /// sync at its end. Outside a group every write committed its database transaction durably
    /// already ([`crate::store::area`]), so nothing synced here: an empty transaction synced
    /// after each of them cost a checkout a whole extra fsync per file.
    fn note_write_durability(&self) {
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
        }
        self.pending
            .store(true, std::sync::atomic::Ordering::Release);
    }

    async fn persist_owned(&self, data: Bytes) -> Result<Hash> {
        self.persist_owned_batch(vec![data])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("owned blob batch unexpectedly returned no hash"))
    }

    async fn persist_owned_batch(&self, data: Vec<Bytes>) -> Result<Vec<Hash>> {
        let files = Arc::clone(&self.files);
        let stored = tokio::task::spawn_blocking(move || -> io::Result<Vec<(Hash, Bytes)>> {
            let mut inline = Vec::new();
            let mut stored = Vec::with_capacity(data.len());
            for bytes in data {
                let hash = Hash::new(&bytes);
                let key = *hash.as_bytes();
                // A manifest shares its content id with the file-backed entry of the same content, so
                // it stays inline even beyond the inline size rather than replacing that entry.
                if bytes.len() <= INLINE_MAX || files.entry(&key).is_some() {
                    inline.push((key, bytes.clone()));
                } else {
                    let outboard = outboard_of_reader(bytes.as_ref(), bytes.len() as u64)?;
                    files.adopt_blob_bytes(&key, &bytes, &outboard)?;
                }
                stored.push((hash, bytes));
            }
            files.put_inline(inline)?;
            Ok(stored)
        })
        .await??;
        self.note_write_durability();
        for (hash, bytes) in &stored {
            self.cache_insert(*hash, bytes.clone());
        }
        Ok(stored.into_iter().map(|(hash, _)| hash).collect())
    }

    pub async fn has(&self, hash: Hash) -> bool {
        if self.cache.contains_key(&hash) {
            return true;
        }
        let key = *hash.as_bytes();
        let files = Arc::clone(&self.files);
        tokio::task::spawn_blocking(move || area_holds(&files, &key))
            .await
            .unwrap_or(false)
    }

    /// Whether the store holds all of `hash`, asked from a thread that may wait on the disk: a
    /// count over many files asks this for each from one blocking step, rather than a step of
    /// the blocking pool per file.
    #[must_use]
    pub fn holds(&self, hash: Hash) -> bool {
        self.cache.contains_key(&hash) || area_holds(&self.files, hash.as_bytes())
    }

    /// The status of blob `hash` in the one store: inline bytes and a whole blob's complete file
    /// are complete, a chunk resident in a file-backed entry is complete at its chunk length.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot answer.
    pub async fn blob_status(&self, hash: Hash) -> Result<BlobStatus> {
        let files = Arc::clone(&self.files);
        let key = *hash.as_bytes();
        Ok(tokio::task::spawn_blocking(move || {
            if let Some(bytes) = files.get_inline(&key) {
                return BlobStatus::Complete {
                    size: bytes.len() as u64,
                };
            }
            if files.is_blob(&key)
                && let Some(entry) = files.entry(&key)
            {
                return if entry.complete() {
                    BlobStatus::Complete { size: entry.size }
                } else {
                    BlobStatus::Partial {
                        size: Some(entry.resident),
                    }
                };
            }
            files
                .find_chunk(&key)
                .map_or(BlobStatus::NotFound, |location| BlobStatus::Complete {
                    size: u64::from(location.len),
                })
        })
        .await
        .unwrap_or(BlobStatus::NotFound))
    }

    /// The blob `tag` names, if any: a document media reference or an import journal entry. The
    /// store never scans or evicts by these.
    pub async fn reference(&self, tag: &str) -> Option<Hash> {
        let files = Arc::clone(&self.files);
        let tag = tag.to_owned();
        tokio::task::spawn_blocking(move || files.reference(&tag).map(Hash::from))
            .await
            .ok()
            .flatten()
    }

    /// Makes `tag` a name for blob `hash`.
    pub async fn set_reference(&self, tag: &str, hash: Hash) -> Result<()> {
        let files = Arc::clone(&self.files);
        let tag = tag.to_owned();
        tokio::task::spawn_blocking(move || files.set_reference(&tag, hash.as_bytes())).await??;
        Ok(())
    }

    /// Resolve a blob to every exact live stable record that references it as a manifest root, manifest
    /// chunk, or thumbnail.
    ///
    /// The lookup is bounded and fail-closed: it never returns paths or other records' metadata, and an
    /// unavailable or malformed manifest prevents a partial authorization result.
    ///
    /// Its roots are read here, before the future is made: a namespace counts its nodes in a
    /// `Cell`, so it is not `Sync`, and a future holding it could not be sent between threads.
    #[cfg(feature = "filesystem")]
    pub fn live_document_ids_for_blob<'a>(
        &'a self,
        namespace: &Namespace,
        requested: Hash,
        limits: BlobDocumentLookupLimits,
    ) -> impl std::future::Future<
        Output = std::result::Result<Vec<fs_protocol::FileDocumentId>, BlobDocumentLookupError>,
    > + Send
    + 'a {
        let roots = if limits.max_manifest_bytes == 0
            || limits.max_manifest_bytes > MAX_BLOB_LOOKUP_MANIFEST_BYTES
            || limits.max_chunks_per_document == 0
            || limits.max_chunks_per_document > MAX_BLOB_LOOKUP_CHUNKS_PER_RECORD
        {
            Err(BlobDocumentLookupError::InvalidLimits)
        } else {
            namespace
                .live_document_blob_roots(limits.max_live_documents)
                .map_err(BlobDocumentLookupError::from)
        };
        async move {
            self.live_document_ids_for_blob_roots(roots?, requested, limits)
                .await
        }
    }

    #[cfg(feature = "filesystem")]
    pub(crate) async fn live_document_ids_for_blob_roots(
        &self,
        roots: Vec<LiveDocumentBlobRoots>,
        requested: Hash,
        limits: BlobDocumentLookupLimits,
    ) -> std::result::Result<Vec<fs_protocol::FileDocumentId>, BlobDocumentLookupError> {
        if limits.max_manifest_bytes == 0
            || limits.max_manifest_bytes > MAX_BLOB_LOOKUP_MANIFEST_BYTES
            || limits.max_chunks_per_document == 0
            || limits.max_chunks_per_document > MAX_BLOB_LOOKUP_CHUNKS_PER_RECORD
        {
            return Err(BlobDocumentLookupError::InvalidLimits);
        }
        let mut records = std::collections::BTreeSet::new();
        for roots in roots {
            if roots.manifest == Some(requested) || roots.thumbnail == Some(requested) {
                records.insert(roots.document_id);
            }
            let Some(manifest_hash) = roots.manifest else {
                continue;
            };
            let bytes = self.local(manifest_hash).await.ok_or(
                BlobDocumentLookupError::ManifestUnavailable {
                    document_id: roots.document_id,
                    manifest: manifest_hash,
                },
            )?;
            if bytes.len() > limits.max_manifest_bytes {
                return Err(BlobDocumentLookupError::ManifestTooLarge {
                    document_id: roots.document_id,
                    limit: limits.max_manifest_bytes,
                });
            }
            let manifest = crate::content::Manifest::decode(&bytes).map_err(|_| {
                BlobDocumentLookupError::MalformedManifest {
                    document_id: roots.document_id,
                }
            })?;
            if manifest.chunks().len() > limits.max_chunks_per_document {
                return Err(BlobDocumentLookupError::TooManyChunks {
                    document_id: roots.document_id,
                    limit: limits.max_chunks_per_document,
                });
            }
            if manifest
                .chunks()
                .iter()
                .any(|chunk| Hash::from(chunk.hash) == requested)
            {
                records.insert(roots.document_id);
            }
        }
        Ok(records.into_iter().collect())
    }

    /// Inspect the exact locally resident graph for a content manifest. The manifest must itself be
    /// complete and decodable; chunk accounting is read from the area rather than inferred from
    /// namespace metadata.
    #[cfg(feature = "filesystem")]
    pub async fn inspect_content_payload(&self, manifest_hash: Hash) -> Result<ContentPayload> {
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
        Ok(ContentPayload {
            manifest: BlobExtent {
                hash: manifest_hash,
                locally_present: true,
                resident_bytes: Some(u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX)),
            },
            chunks,
        })
    }

    /// Prove that the content `manifest_hash` names, `byte_len` bytes long, is complete in the
    /// persistent store: its manifest and every chunk it declares. Hot cache entries and partial
    /// blobs never satisfy this boundary.
    #[cfg(all(test, feature = "filesystem"))]
    pub async fn verify_durable_content(&self, manifest_hash: Hash, byte_len: u64) -> Result<()> {
        let manifest_bytes = self
            .blob_of_bytes(manifest_hash)
            .await
            .ok_or_else(|| anyhow::anyhow!("version manifest is not durably complete"))?;
        let manifest = crate::content::Manifest::decode(&manifest_bytes)?;
        anyhow::ensure!(
            manifest.total_len() == byte_len,
            "manifest length does not match the content's length"
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
            match self.blob_status(hash).await? {
                BlobStatus::Complete { size } if size == manifest.stored_len(chunk) => {}
                BlobStatus::Complete { .. } => anyhow::bail!("durable chunk length mismatch"),
                BlobStatus::Partial { .. } | BlobStatus::NotFound => {
                    anyhow::bail!("version chunk is not durably complete")
                }
            }
        }
        Ok(())
    }

    /// Freshly authorize a queued request and remove exactly its approved blobs from the store.
    ///
    /// The caller must keep `registry` and `catalog` stable for the complete call. A successful result
    /// proves the bytes are gone; [`Self::observe_eviction_request`] records the completion.
    #[cfg(feature = "filesystem")]
    pub async fn evict_request(
        &self,
        queue: &EvictionQueue,
        id: EvictionRequestId,
        registry: &AvailabilityRegistry,
        catalog: &EvictionCatalog,
        now_ms: u64,
        requirements: EvictionRequirements,
    ) -> std::result::Result<(), StoreEvictionError> {
        let hashes = queue.authorize_eviction(id, registry, catalog, now_ms, requirements)?;
        let selected: HashSet<Hash> = hashes.into_iter().collect();
        self.evict_blobs(&selected)
            .await
            .map_err(|error| StoreEvictionError::Remove(error.to_string()))?;
        Ok(())
    }

    /// Observe a durable eviction request against one coherent store status sample. Completion is
    /// persisted only when every requested hash is `NotFound`; a failed read leaves the queue
    /// unchanged.
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
            // Drop any hot in-memory copy before exposing completion to local readers.
            if let Some(request) = queue.get(id) {
                for hash in &request.requested_hashes {
                    self.cache.invalidate(hash);
                }
            }
        }
        Ok(observation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blobs_ingest_and_fetch_round_trip_through_one_store() {
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
            let first = Bytes::from_static(b"ingested");
            let first_hash = store.ingest(first.clone()).await.unwrap();
            let fetched_bytes = Bytes::from_static(b"fetched");
            let fetched = Hash::new(&fetched_bytes);
            store.store_fetched(fetched, fetched_bytes).await.unwrap();
            assert_eq!(store.local(first_hash).await, Some(first));

            let claimed = Hash::from([9; 32]);
            assert!(
                store
                    .store_fetched(claimed, Bytes::from_static(b"different"))
                    .await
                    .is_err()
            );
            assert!(!store.has(claimed).await);
            assert!(!store.has(Hash::new(b"different")).await);
        }
    }

    #[tokio::test]
    async fn a_whole_blob_and_its_reference_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..3 * 1024 * 1024 + 123_u32)
            .map(|n| u8::try_from(n % 251).unwrap())
            .collect();
        let hash = Hash::new(&bytes);
        {
            let store = ContentStore::open(dir.path()).await.unwrap();
            let (stored, len) = store
                .ingest_stream(futures_util::stream::iter([Ok(Bytes::from(bytes.clone()))]))
                .await
                .unwrap();
            assert_eq!((stored, len), (hash, bytes.len() as u64));
            assert!(store.files().is_blob(hash.as_bytes()));
            let created = PreOrderMemOutboard::create(&bytes, IROH_BLOCK_SIZE);
            assert_eq!(
                store.files().outboard(hash.as_bytes()),
                Some(created.data.clone()),
                "the stored outboard is the one the tree builds"
            );
            store
                .set_reference("tilo/media/v1/a/b", hash)
                .await
                .unwrap();
            store.shutdown().await.unwrap();
        }
        let store = ContentStore::open(dir.path()).await.unwrap();
        assert_eq!(
            store.reference("tilo/media/v1/a/b").await,
            Some(hash),
            "the reference survives reopening"
        );
        assert!(store.files().is_blob(hash.as_bytes()));
        assert!(
            store.files().outboard(hash.as_bytes()).is_some(),
            "the outboard survives reopening"
        );
        assert_eq!(
            store.files().entry(hash.as_bytes()).map(|e| e.complete()),
            Some(true)
        );
        assert_eq!(
            store.read_blob_range(hash, 0..0).await,
            Some((Vec::new(), bytes.len() as u64))
        );
        assert_eq!(
            store.read_blob_range(hash, 100..5000).await,
            Some((bytes[100..5000].to_vec(), bytes.len() as u64))
        );
        assert_eq!(
            store.read_blob_range(hash, 0..u64::MAX).await,
            Some((bytes.clone(), bytes.len() as u64))
        );
        let key = *hash.as_bytes();
        store.files().write_at(&key, 4096, &[0xff]).unwrap();
        assert!(
            store.read_blob_range(hash, 4096..4097).await.is_none(),
            "a damaged range fails its BAO check"
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn persistent_stores_evict_selectively_and_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
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
            store.shutdown().await.unwrap();
            (kept, selected)
        };
        let reopened = ContentStore::open(directory.path()).await.unwrap();
        assert_eq!(reopened.local(kept).await, Some(kept_bytes));
        assert!(reopened.local(selected).await.is_none());
        reopened.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[cfg(feature = "filesystem")]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn residency_and_manifests_decide_what_is_durable_or_evicted() {
        {
            let store = ContentStore::memory();
            let data = vec![7u8; 100_000];
            let manifest_hash = store.write_content(&data).await.unwrap().unwrap();
            let payload = store.inspect_content_payload(manifest_hash).await.unwrap();
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
            assert!(
                store
                    .verify_durable_content(manifest_hash, 13)
                    .await
                    .is_err()
            );
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
                document_id: fs_protocol::FileDocumentId::new(),
                content_hash: hash,
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
                EvictionRequestState::AwaitingEviction
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
            let first_document = namespace.document_id_for(first).unwrap();
            let second_document = namespace.document_id_for(second).unwrap();
            assert!(namespace.set_thumb(first, &thumbnail));
            let limits = BlobDocumentLookupLimits::default();

            assert_eq!(
                store
                    .live_document_ids_for_blob(&namespace, first_manifest, limits)
                    .await
                    .unwrap(),
                vec![first_document]
            );
            assert_eq!(
                store
                    .live_document_ids_for_blob(&namespace, shared_chunk, limits)
                    .await
                    .unwrap(),
                vec![first_document, second_document]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                store
                    .live_document_ids_for_blob(&namespace, second_only_chunk, limits)
                    .await
                    .unwrap(),
                vec![second_document]
            );
            assert_eq!(
                store
                    .live_document_ids_for_blob(&namespace, thumbnail, limits)
                    .await
                    .unwrap(),
                vec![first_document]
            );
            assert!(
                store
                    .live_document_ids_for_blob(&namespace, Hash::from([88; 32]), limits)
                    .await
                    .unwrap()
                    .is_empty()
            );
            let mut tight_limits = limits;
            tight_limits.max_chunks_per_document = 1;
            assert!(matches!(
                store
                    .live_document_ids_for_blob(&namespace, shared_chunk, tight_limits)
                    .await,
                Err(BlobDocumentLookupError::TooManyChunks { limit: 1, .. })
            ));

            assert!(namespace.remove(first));
            assert_eq!(
                store
                    .live_document_ids_for_blob(&namespace, shared_chunk, limits)
                    .await
                    .unwrap(),
                vec![second_document]
            );
            assert_eq!(namespace.document_id_for(second), Some(second_document));
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
            let document_id = namespace.document_id_for(ino).unwrap();
            assert!(matches!(
                store
                    .live_document_ids_for_blob(
                        &namespace,
                        missing,
                        BlobDocumentLookupLimits::default()
                    )
                    .await,
                Err(BlobDocumentLookupError::ManifestUnavailable {
                    document_id: found,
                    manifest: found_manifest,
                }) if found == document_id && found_manifest == missing
            ));

            let limits = BlobDocumentLookupLimits {
                max_live_documents: 0,
                ..BlobDocumentLookupLimits::default()
            };
            assert!(matches!(
                store
                    .live_document_ids_for_blob(&namespace, missing, limits)
                    .await,
                Err(BlobDocumentLookupError::Namespace(
                    LiveDocumentBlobScanError::InvalidLimit
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

        let read = |content, max_bytes| {
            ContentStore::read_file_version_unlocked(directory.path(), content, max_bytes).unwrap()
        };
        let len = u64::try_from(large.len()).unwrap();
        assert_eq!(read(manifest, u64::MAX), Some(large.clone()));
        assert_eq!(read(manifest, len), Some(large.clone()));
        assert_eq!(read(manifest, len - 1), None, "over the bound");
        assert_eq!(read(small, u64::MAX), None, "not file-backed");
        assert_eq!(read([7; 32], u64::MAX), None, "unknown");

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

/// The whole blob `hash` from `files`: inline, or a complete whole blob's file whose bytes hash to
/// its name, with whether it was inline.
fn whole_blob(files: &FileArea, hash: Hash) -> Option<(Bytes, bool)> {
    let key = *hash.as_bytes();
    if let Some(bytes) = files.get_inline(&key) {
        return Some((Bytes::from(bytes), true));
    }
    if !files.is_blob(&key) {
        return None;
    }
    let entry = files.entry(&key)?;
    entry
        .complete()
        .then(|| files.read_at(&key, 0, entry.size).ok())
        .flatten()
        // A window's record names its bytes before the barrier syncs them, so a power loss can
        // leave a file short of its name. Answering only bytes that hash to the name keeps that
        // loss a miss to hydrate from a peer, never content served as something it is not.
        .filter(|bytes| blake3::hash(bytes).as_bytes() == &key)
        .map(|bytes| (Bytes::from(bytes), false))
}

/// A file-backed chunk's bytes from `files`, checked against its hash.
fn file_backed_chunk(files: &FileArea, hash: Hash) -> Option<Bytes> {
    let location = files.find_chunk(hash.as_bytes())?;
    let bytes = files
        .read_at(&location.key, location.offset, u64::from(location.len))
        .ok()?;
    (blake3::hash(&bytes).as_bytes() == hash.as_bytes()).then(|| Bytes::from(bytes))
}
