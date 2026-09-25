//! Content-addressed storage for document binaries, and the sniffer that derives a blob's
//! descriptor from its bytes.
//!
//! The bytes are deliberately not in the document: inline media turns every
//! Automerge replica and durable host log into a blob store, amplifies sync
//! memory, and makes ordinary edits wait behind large payloads. The document
//! holds `{hash, mime, len, w, h}` and the bytes live here; dedup is free (the
//! key *is* the content hash), and history stops carrying megabytes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Hard limits for a blob store.  They deliberately cover both payload and
/// metadata: a document that names millions of tiny blobs must not be able to
/// turn a bounded byte budget into an unbounded index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobStoreLimits {
    /// Largest accepted individual blob.
    pub max_blob_bytes: usize,
    /// Largest aggregate payload size retained by this store.
    pub max_total_bytes: usize,
    /// Largest number of retained blobs (and index entries).
    pub max_entries: usize,
    /// Largest number of filesystem directory entries inspected while building
    /// the on-disk index.  If this is exceeded, new writes are rejected rather
    /// than performing an unbounded startup scan.
    pub max_scan_entries: usize,
}

impl Default for BlobStoreLimits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 32 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_entries: 8192,
            max_scan_entries: 16_384,
        }
    }
}

/// Why a store refused a write or could not inspect its durable index.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobStoreError {
    #[error("blob hash does not name these bytes")]
    HashMismatch,
    #[error("blob is {actual} bytes; per-item limit is {limit} bytes")]
    ItemTooLarge { actual: usize, limit: usize },
    #[error("blob store total would be {attempted} bytes; limit is {limit} bytes")]
    TotalQuotaExceeded { attempted: usize, limit: usize },
    #[error("blob store would contain more than {limit} entries")]
    EntryLimitExceeded { limit: usize },
    #[error("blob-store disk index exceeded its {limit}-entry scan bound")]
    ScanLimitExceeded { limit: usize },
    #[error("blob-store I/O: {0}")]
    Io(String),
}

/// Exact, deterministic result of an explicit reclamation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictionReport {
    /// Removed hashes in lexical order.
    pub hashes: Vec<String>,
    pub bytes: usize,
}

/// Metadata a document carries *instead of* a blob's bytes — enough for a renderer to lay out a
/// placeholder occupying exactly the space the image will, so nothing reflows when the bytes arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobMeta {
    /// Media type, sniffed from the content (`image/png`, …). Empty if unknown.
    pub mime: String,
    pub len: u64,
    /// Intrinsic pixel width, 0 when not an image we can read a header from.
    pub w: u32,
    /// Intrinsic pixel height, 0 when unknown.
    pub h: u32,
}

/// A content-addressed byte store keyed by blake3 hex. Implementations must treat the key as
/// authoritative and verify the bytes hash to it — a store that trusts the key would let one peer
/// poison every replica that later reads that hash.
pub trait BlobStore: Send + Sync {
    fn get(&self, hash: &str) -> Option<Vec<u8>>;
    fn put(&self, hash: &str, bytes: &[u8]) -> bool;
    /// Like [`Self::put`], but tells callers whether quota, integrity, or I/O
    /// caused a refusal.  The boolean method remains for existing lightweight
    /// implementations; quota-aware stores override this method.
    ///
    /// # Errors
    ///
    /// Returns [`BlobStoreError`] when the content fails verification, exceeds
    /// a quota, or the durable backend cannot complete the operation.
    fn put_checked(&self, hash: &str, bytes: &[u8]) -> Result<(), BlobStoreError> {
        self.put(hash, bytes)
            .then_some(())
            .ok_or(BlobStoreError::HashMismatch)
    }
    /// Reclaim only hashes absent from a caller-provided *complete* live set.
    ///
    /// The qdoc store deliberately never invokes this automatically: it does
    /// not know every open or persisted document that can reference a hash.
    /// Hosts which do have that reachability information may reclaim safely.
    ///
    /// # Errors
    ///
    /// Returns [`BlobStoreError`] if the store cannot enumerate or remove a
    /// candidate; prior removals, if any, are reflected in neither a report nor
    /// the error, so callers should retry with the same complete live set.
    fn evict_unreferenced(
        &self,
        live_hashes: &BTreeSet<String>,
    ) -> Result<EvictionReport, BlobStoreError>;
    fn has(&self, hash: &str) -> bool {
        self.get(hash).is_some()
    }
}

/// Hash `bytes` the way every blob key in this system is derived.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// True when `bytes` really are the content named by `hash`.
pub fn verify(hash: &str, bytes: &[u8]) -> bool {
    hash_bytes(bytes) == hash
}

/// In-process store: the default, so a unit test or a headless codec run works
/// with no filesystem at all.
pub struct MemoryBlobStore {
    limits: BlobStoreLimits,
    inner: RwLock<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    bytes: usize,
    entries: BTreeMap<String, Vec<u8>>,
}

impl Default for MemoryBlobStore {
    fn default() -> Self {
        Self::new(BlobStoreLimits::default())
    }
}

impl MemoryBlobStore {
    pub fn new(limits: BlobStoreLimits) -> Self {
        Self {
            limits,
            inner: RwLock::new(MemoryState::default()),
        }
    }

    pub fn limits(&self) -> BlobStoreLimits {
        self.limits
    }
}

impl BlobStore for MemoryBlobStore {
    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        self.inner.read().ok()?.entries.get(hash).cloned()
    }
    fn put(&self, hash: &str, bytes: &[u8]) -> bool {
        self.put_checked(hash, bytes).is_ok()
    }
    fn put_checked(&self, hash: &str, bytes: &[u8]) -> Result<(), BlobStoreError> {
        if !verify(hash, bytes) {
            return Err(BlobStoreError::HashMismatch);
        }
        let mut state = self
            .inner
            .write()
            .map_err(|_| BlobStoreError::Io("memory store lock poisoned".into()))?;
        // Re-inserting content already retained consumes no more quota.
        if state.entries.contains_key(hash) {
            return Ok(());
        }
        check_new_blob(&self.limits, state.bytes, state.entries.len(), bytes.len())?;
        state.bytes = state.bytes.saturating_add(bytes.len());
        state.entries.insert(hash.to_string(), bytes.to_vec());
        drop(state);
        Ok(())
    }
    fn evict_unreferenced(
        &self,
        live_hashes: &BTreeSet<String>,
    ) -> Result<EvictionReport, BlobStoreError> {
        let mut state = self
            .inner
            .write()
            .map_err(|_| BlobStoreError::Io("memory store lock poisoned".into()))?;
        let stale_hashes: Vec<String> = state
            .entries
            .keys()
            .filter(|hash| !live_hashes.contains(*hash))
            .cloned()
            .collect();
        let mut report = EvictionReport::default();
        for hash in stale_hashes {
            if let Some(bytes) = state.entries.remove(&hash) {
                state.bytes = state.bytes.saturating_sub(bytes.len());
                report.bytes = report.bytes.saturating_add(bytes.len());
                report.hashes.push(hash);
            }
        }
        drop(state);
        Ok(report)
    }
}

/// Filesystem store, sharded one level on the first two hex characters so a
/// directory does not accumulate every blob the machine has ever seen.
pub struct FsBlobStore {
    root: PathBuf,
    limits: BlobStoreLimits,
    state: Mutex<FsState>,
}

#[derive(Default)]
struct FsState {
    bytes: usize,
    entries: BTreeMap<String, usize>,
}

/// Held around every filesystem mutation and its preceding quota scan.  The
/// lock is separate from `FsState`: two `FsBlobStore`s (or two processes) can
/// point at one configured root and must not each admit a write from a stale
/// local total.
struct RootWriteLock(std::fs::File);

impl Drop for RootWriteLock {
    fn drop(&mut self) {
        // The OS also releases this lock when a process dies. std selects
        // flock/LockFileEx for the host while retaining the same quota boundary.
        let _ = self.0.unlock();
    }
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_limits(root, BlobStoreLimits::default())
    }

    pub fn with_limits(root: impl Into<PathBuf>, limits: BlobStoreLimits) -> Self {
        Self {
            root: root.into(),
            limits,
            state: Mutex::new(FsState::default()),
        }
    }

    pub fn limits(&self) -> BlobStoreLimits {
        self.limits
    }

    fn lock_root_for_write(&self) -> Result<RootWriteLock, BlobStoreError> {
        std::fs::create_dir_all(&self.root)
            .map_err(|error| BlobStoreError::Io(error.to_string()))?;
        let path = self.root.join(".qdoc-blob-store.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| BlobStoreError::Io(error.to_string()))?;
        file.lock()
            .map_err(|error| BlobStoreError::Io(error.to_string()))?;
        Ok(RootWriteLock(file))
    }

    /// The store this machine uses unless told otherwise. `TILO_BLOB_STORE` overrides it, letting two
    /// sessions on one host be genuinely separate peers rather than one store they both hit.
    pub fn default_root() -> PathBuf {
        Self::default_root_with(|key| std::env::var_os(key))
    }

    fn default_root_with(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
        if let Some(v) = env("TILO_BLOB_STORE")
            && !v.is_empty()
        {
            return PathBuf::from(v);
        }
        let native_base = || {
            #[cfg(windows)]
            {
                env("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .or_else(|| {
                        env("USERPROFILE")
                            .map(PathBuf::from)
                            .filter(|path| path.is_absolute())
                            .map(|path| path.join("AppData/Local"))
                    })
            }
            #[cfg(not(windows))]
            {
                None
            }
        };
        let base = env("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(native_base)
            .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        base.join("tilo").join("blobs")
    }

    fn path_for(&self, hash: &str) -> Option<PathBuf> {
        // The hash reaches this function from the wire, so anything that is not
        // plain hex is refused: a key with a separator in it would otherwise
        // address a file outside the store.
        if !valid_hash(hash) {
            return None;
        }
        // Every byte was just checked ASCII-hex above, so byte offset 2 is a char boundary.
        let prefix = hash.get(..2)?;
        Some(self.root.join(prefix).join(hash))
    }

    fn load_index(&self, state: &mut FsState) -> Result<(), BlobStoreError> {
        let root = match std::fs::read_dir(&self.root) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => return Err(BlobStoreError::Io(error.to_string())),
        };
        let mut scanned = 0usize;
        let mut entries = BTreeMap::new();
        let mut total = 0usize;
        for shard in root {
            let shard = shard.map_err(|error| BlobStoreError::Io(error.to_string()))?;
            scanned = scanned.saturating_add(1);
            if scanned > self.limits.max_scan_entries {
                return Err(BlobStoreError::ScanLimitExceeded {
                    limit: self.limits.max_scan_entries,
                });
            }
            let name = shard.file_name();
            let Some(prefix) = name.to_str() else {
                continue;
            };
            if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(kind) = shard.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let dir = std::fs::read_dir(shard.path())
                .map_err(|error| BlobStoreError::Io(error.to_string()))?;
            for blob in dir {
                let blob = blob.map_err(|error| BlobStoreError::Io(error.to_string()))?;
                scanned = scanned.saturating_add(1);
                if scanned > self.limits.max_scan_entries {
                    return Err(BlobStoreError::ScanLimitExceeded {
                        limit: self.limits.max_scan_entries,
                    });
                }
                let name = blob.file_name();
                let Some(hash) = name.to_str().filter(|hash| valid_hash(hash)) else {
                    continue;
                };
                // Canonical lower-case names prevent aliases and make the
                // BTree index/reclamation order deterministic.
                if hash.bytes().any(|byte| byte.is_ascii_uppercase()) {
                    continue;
                }
                let Ok(kind) = blob.file_type() else { continue };
                if !kind.is_file() {
                    continue;
                }
                let len = blob
                    .metadata()
                    .map_err(|error| BlobStoreError::Io(error.to_string()))?
                    .len();
                let len = usize::try_from(len).map_err(|_| BlobStoreError::TotalQuotaExceeded {
                    attempted: usize::MAX,
                    limit: self.limits.max_total_bytes,
                })?;
                total = total.saturating_add(len);
                entries.insert(hash.to_string(), len);
            }
        }
        state.bytes = total;
        state.entries = entries;
        Ok(())
    }
}

impl BlobStore for FsBlobStore {
    fn get(&self, hash: &str) -> Option<Vec<u8>> {
        let p = self.path_for(hash)?;
        // Avoid allocating an arbitrary file merely because a hostile peer
        // named its hash. Existing oversized files remain on disk but are not
        // admitted into this bounded store.
        let bytes = read_bounded_file(&p, self.limits.max_blob_bytes)?;
        // A blob that does not match its own name is corruption, not content.
        if verify(hash, &bytes) {
            Some(bytes)
        } else {
            None
        }
    }

    fn put(&self, hash: &str, bytes: &[u8]) -> bool {
        self.put_checked(hash, bytes).is_ok()
    }

    fn put_checked(&self, hash: &str, bytes: &[u8]) -> Result<(), BlobStoreError> {
        if !verify(hash, bytes) {
            return Err(BlobStoreError::HashMismatch);
        }
        let Some(p) = self.path_for(hash) else {
            return Err(BlobStoreError::HashMismatch);
        };
        // A valid pre-existing file is an idempotent write even if a later
        // configuration tightened the limits.
        if self.get(hash).is_some() {
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| BlobStoreError::Io("filesystem store lock poisoned".into()))?;
        let root_lock = self.lock_root_for_write()?;
        // Always rescan while holding the per-root lock.  `FsState` is a
        // cache for reads, not authority for admission: another process may
        // have written to this root since this instance's previous put.
        self.load_index(&mut state)?;
        // Another writer might have completed after the optimistic check.
        if let Some(old_len) = state.entries.get(hash).copied() {
            if self.get(hash).is_some() {
                return Ok(());
            }
            // A file whose bytes fail verification is not valid retained
            // content.  Do not let it permanently consume the same hash's
            // quota; the atomic rename below replaces it only after a complete
            // verified payload has been written.
            state.entries.remove(hash);
            state.bytes = state.bytes.saturating_sub(old_len);
        }
        check_new_blob(&self.limits, state.bytes, state.entries.len(), bytes.len())?;
        let Some(dir) = p.parent() else {
            return Err(BlobStoreError::Io(
                "blob path has no parent directory".into(),
            ));
        };
        if let Err(error) = std::fs::create_dir_all(dir) {
            return Err(BlobStoreError::Io(error.to_string()));
        }
        // Write beside the target and rename, so a reader never observes a
        // partially written blob under a name that promises whole content.
        // The unique temporary name also prevents two writers from truncating
        // one another's partial file.
        let tmp = unique_partial_path(dir, hash);
        let write_result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&tmp);
            return Err(BlobStoreError::Io(error.to_string()));
        }
        if let Err(error) = std::fs::rename(&tmp, &p) {
            let _ = std::fs::remove_file(&tmp);
            return Err(BlobStoreError::Io(error.to_string()));
        }
        state.bytes = state.bytes.saturating_add(bytes.len());
        state.entries.insert(hash.to_string(), bytes.len());
        drop(root_lock);
        drop(state);
        Ok(())
    }

    fn evict_unreferenced(
        &self,
        live_hashes: &BTreeSet<String>,
    ) -> Result<EvictionReport, BlobStoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| BlobStoreError::Io("filesystem store lock poisoned".into()))?;
        let root_lock = self.lock_root_for_write()?;
        self.load_index(&mut state)?;
        let stale_hashes: Vec<(String, usize)> = state
            .entries
            .iter()
            .filter(|(hash, _)| !live_hashes.contains(*hash))
            .map(|(hash, len)| (hash.clone(), *len))
            .collect();
        let mut report = EvictionReport::default();
        for (hash, len) in stale_hashes {
            let Some(path) = self.path_for(&hash) else {
                continue;
            };
            match std::fs::remove_file(path) {
                Ok(()) => {
                    state.entries.remove(&hash);
                    state.bytes = state.bytes.saturating_sub(len);
                    report.bytes = report.bytes.saturating_add(len);
                    report.hashes.push(hash);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    state.entries.remove(&hash);
                    state.bytes = state.bytes.saturating_sub(len);
                    report.bytes = report.bytes.saturating_add(len);
                    report.hashes.push(hash);
                }
                Err(error) => return Err(BlobStoreError::Io(error.to_string())),
            }
        }
        drop(root_lock);
        drop(state);
        Ok(report)
    }

    fn has(&self, hash: &str) -> bool {
        self.get(hash).is_some()
    }
}

fn check_new_blob(
    limits: &BlobStoreLimits,
    current_bytes: usize,
    current_entries: usize,
    incoming_bytes: usize,
) -> Result<(), BlobStoreError> {
    if incoming_bytes > limits.max_blob_bytes {
        return Err(BlobStoreError::ItemTooLarge {
            actual: incoming_bytes,
            limit: limits.max_blob_bytes,
        });
    }
    if current_entries >= limits.max_entries {
        return Err(BlobStoreError::EntryLimitExceeded {
            limit: limits.max_entries,
        });
    }
    let attempted = current_bytes.saturating_add(incoming_bytes);
    if attempted > limits.max_total_bytes {
        return Err(BlobStoreError::TotalQuotaExceeded {
            attempted,
            limit: limits.max_total_bytes,
        });
    }
    Ok(())
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_partial_path(dir: &Path, hash: &str) -> PathBuf {
    let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{hash}.partial-{}-{serial}", std::process::id()))
}

/// Read at most `limit` bytes plus one sentinel byte.  The `take` cap closes
/// the metadata/read race: a file which grows after its initial stat still
/// cannot make `get` allocate beyond the configured per-item limit.
fn read_bounded_file(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let declared = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if declared > limit {
        return None;
    }
    let sentinel = u64::try_from(limit).ok()?.saturating_add(1);
    let mut bytes = Vec::with_capacity(declared);
    file.take(sentinel).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

static STORE: OnceLock<RwLock<Arc<dyn BlobStore>>> = OnceLock::new();

#[cfg(test)]
mod runtime_store_tests {
    #[test]
    fn explicit_runtime_store_carries_bytes_between_processes() {
        const CONTENT: &[u8] = b"source bytes from another application";
        if let Ok(stage) = std::env::var("QOS_BLOB_PROOF_STAGE") {
            if stage == "write" {
                assert!(super::put(CONTENT).is_some());
            } else {
                assert_eq!(
                    super::get(&super::hash_bytes(CONTENT)).as_deref(),
                    Some(CONTENT)
                );
            }
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for stage in ["write", "read"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "blobstore::runtime_store_tests::explicit_runtime_store_carries_bytes_between_processes", "--nocapture"])
                .env("TILO_BLOB_STORE", directory.path())
                .env("QOS_BLOB_PROOF_STAGE", stage)
                .status().unwrap();
            assert!(status.success(), "{stage} process failed");
        }
    }
}

fn cell() -> &'static RwLock<Arc<dyn BlobStore>> {
    STORE.get_or_init(|| {
        let store: Arc<dyn BlobStore> =
            match std::env::var_os("TILO_BLOB_STORE").filter(|path| !path.is_empty()) {
                Some(path) => Arc::new(FsBlobStore::new(PathBuf::from(path))),
                None => Arc::new(MemoryBlobStore::default()),
            };
        RwLock::new(store)
    })
}

/// A store and transport that replace the process-wide ones on one thread, for as long as the
/// [`BlobScope`] that [`scope`] returned lives.
#[derive(Clone)]
struct Scoped {
    store: Arc<dyn BlobStore>,
    transport: Option<Arc<dyn BlobTransport>>,
}

std::thread_local! {
    static SCOPED: std::cell::RefCell<Option<Scoped>> = const { std::cell::RefCell::new(None) };
}

/// Replaces the process-wide store and transport on the thread that created it; dropping it
/// restores what the thread used before. See [`scope`].
#[must_use = "the scope ends when this is dropped"]
pub struct BlobScope {
    previous: Option<Scoped>,
    /// Tied to the thread whose override it restores.
    _thread: std::marker::PhantomData<*const ()>,
}

impl Drop for BlobScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SCOPED.with(|s| *s.borrow_mut() = previous);
    }
}

/// Use `store` and `transport` instead of the process-wide ones on this thread until the returned
/// guard drops.
///
/// For code that must not see or disturb the process's store: a test that needs a store with its
/// own quota or contents, or a transport of its own, installs it here and cannot leave it behind for
/// whatever runs next, even when it fails. Scopes nest; each restores the one it replaced.
pub fn scope(store: Arc<dyn BlobStore>, transport: Option<Arc<dyn BlobTransport>>) -> BlobScope {
    let previous = SCOPED.with(|s| s.borrow_mut().replace(Scoped { store, transport }));
    BlobScope {
        previous,
        _thread: std::marker::PhantomData,
    }
}

/// This thread's scoped store and transport, if a [`scope`] is active.
fn scoped() -> Option<Scoped> {
    SCOPED.with(|s| s.borrow().clone())
}

/// Install the process-wide store. A global rather than a handle threaded through `DocMut`,
/// because otherwise every codec and every caller of `put_blob` needs the parameter, and the store is a property of the machine rather than of a document.
pub fn set_store(store: Arc<dyn BlobStore>) {
    if let Ok(mut s) = cell().write() {
        *s = store;
    }
}

/// The process-wide store: the explicitly configured `TILO_BLOB_STORE`, or
/// in-memory when absent. [`set_store`] overrides either choice. A trusted
/// local runtime can supply one shared directory; sandboxed hosts must still
/// provide their authorized content transport rather than an ambient path.
/// A [`scope`] on the calling thread takes precedence over all of these.
pub fn store() -> Arc<dyn BlobStore> {
    if let Some(scoped) = scoped() {
        return scoped.store;
    }
    match cell().read() {
        Ok(s) => s.clone(),
        Err(_) => Arc::new(MemoryBlobStore::default()),
    }
}

/// How this process reaches the rest of the system for content.
///
/// A content-addressed store makes a blob's *identity* portable; it does not move the bytes. An
/// app holds its blobs in its own sandbox (the launcher gives every app a private `HOME`/
/// `XDG_CACHE_HOME`), so there is no directory an app and its shell can both read — without this
/// door an app could reference an image the shell then had no way to send to the room.
///
/// Whatever owns this process's link to the rest of the system installs one; then [`put`] announces and a missed [`get`] asks.
pub trait BlobTransport: Send + Sync {
    /// This process now holds `hash`. Anyone who might need it should hear.
    fn offer(&self, hash: &str, bytes: &[u8]);
    /// This process needs `hash` and does not have it. Called on a miss,
    /// rate-limited by [`ASK_AGAIN_AFTER`].
    fn want(&self, hash: &str);
}

static TRANSPORT: OnceLock<RwLock<Option<Arc<dyn BlobTransport>>>> = OnceLock::new();

fn transport_cell() -> &'static RwLock<Option<Arc<dyn BlobTransport>>> {
    TRANSPORT.get_or_init(|| RwLock::new(None))
}

/// Content stored before a transport existed, waiting to be offered.  Keep
/// only hashes here: retaining another copy of every image would bypass the
/// memory-store quota.  The bytes are read from the store when a transport is
/// installed; a late peer can still request any entry that was not queued.
type PendingOffer = String;

static PENDING_OFFERS: OnceLock<RwLock<Vec<PendingOffer>>> = OnceLock::new();

const MAX_PENDING_OFFERS: usize = 8;

fn pending_offers() -> &'static RwLock<Vec<PendingOffer>> {
    PENDING_OFFERS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Install the process-wide transport, and hand it anything stored before it
/// existed.
pub fn set_transport(t: &Arc<dyn BlobTransport>) {
    if let Ok(mut s) = transport_cell().write() {
        *s = Some(Arc::clone(t));
    }
    let queued = match pending_offers().write() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    };
    for hash in queued {
        if let Some(bytes) = store().get(&hash) {
            t.offer(&hash, &bytes);
        }
    }
}

/// Take the transport away again (collaboration socket closed, or the app left the room). Content
/// already here still reads; content that is not stops claiming to be on its way, since nothing can fetch it any more (see [`state`]).
pub fn clear_transport() {
    if let Ok(mut s) = transport_cell().write() {
        *s = None;
    }
}

/// The process-wide transport, if one has been installed; within a [`scope`], the scope's.
pub fn transport() -> Option<Arc<dyn BlobTransport>> {
    if let Some(scoped) = scoped() {
        return scoped.transport;
    }
    transport_cell().read().ok()?.clone()
}

/// How long to wait before asking again for the same content. A renderer calls [`get`] every frame, so without this one missing image would flood the room's topic.
const ASK_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

static ASKED: OnceLock<RwLock<BTreeMap<String, std::time::Instant>>> = OnceLock::new();

fn may_ask(hash: &str) -> bool {
    let cell = ASKED.get_or_init(|| RwLock::new(BTreeMap::new()));
    let now = std::time::Instant::now();
    let Ok(mut m) = cell.write() else {
        return false;
    };
    match m.get(hash) {
        Some(t) if now.duration_since(*t) < ASK_AGAIN_AFTER => false,
        _ => {
            // Bounded: a document referencing thousands of blobs must not turn this into a leak. Evicting
            // oldest-first costs at most one extra request for whatever was evicted.
            if m.len() > 512
                && let Some(k) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())
            {
                m.remove(&k);
            }
            m.insert(hash.to_string(), now);
            true
        }
    }
}

/// Where a reference's content has got to.
///
/// A renderer that only knows "the bytes are not here" draws the same grey box for a transfer in
/// flight and one that will never complete; the two must be distinguishable before anything can be drawn honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobState {
    /// The content is here; [`get`] returns it.
    Resolved,
    /// Not here, and expected: this process has asked and the answer has had
    /// less than [`GIVE_UP_AFTER`] to arrive.
    Pending,
    /// Not here, and not coming: either nothing in this process can fetch it (no transport, so a miss
    /// is permanent by construction), or it has been asked for long enough that nobody has it.
    Unavailable,
}

/// How long a missing blob stays [`BlobState::Pending`] before this process stops calling it
/// "arriving". Sized against the fetching layer: the shell asks the room and retries a handful of
/// times, taking well under a minute — sooner would call an in-flight image lost, much later leaves
/// a reader watching a box that says "arriving" long after it stopped.
pub const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(45);

static FIRST_WANTED: OnceLock<RwLock<BTreeMap<String, std::time::Instant>>> = OnceLock::new();

/// When this process first went looking for `hash`, recorded once. The [`BlobState::Pending`]
/// clock starts here rather than when the reference arrived, since a document can hold a reference long before anything draws it.
fn first_wanted(hash: &str) -> std::time::Instant {
    let cell = FIRST_WANTED.get_or_init(|| RwLock::new(BTreeMap::new()));
    let now = std::time::Instant::now();
    let Ok(mut m) = cell.write() else { return now };
    if m.len() > 512
        && let Some(k) = m.iter().min_by_key(|(_, t)| **t).map(|(k, _)| k.clone())
    {
        m.remove(&k);
    }
    *m.entry(hash.to_string()).or_insert(now)
}

/// Forget that `hash` was ever waited on, so a re-reference after it landed
/// starts its clock fresh rather than inheriting an expired one.
fn clear_wait(hash: &str) {
    if let Some(cell) = FIRST_WANTED.get()
        && let Ok(mut m) = cell.write()
    {
        m.remove(hash);
    }
}

/// Whether the content behind `hash` is here, on its way, or not coming. Reports; does not ask —
/// [`get`] is what asks, and a renderer already calls that every frame, so a state query triggering another request would just be the same flood by another route.
pub fn state(hash: &str) -> BlobState {
    if store().has(hash) {
        clear_wait(hash);
        return BlobState::Resolved;
    }
    // No door out of this process, so a miss is permanent however long anyone
    // waits and "arriving" would be a lie that never resolves.
    if transport().is_none() {
        return BlobState::Unavailable;
    }
    if first_wanted(hash).elapsed() < GIVE_UP_AFTER {
        BlobState::Pending
    } else {
        BlobState::Unavailable
    }
}

/// Read a blob's bytes from the process-wide store. A miss asks the transport and still returns
/// `None`: the caller renders its placeholder now and the next frame draws the image once bytes
/// arrive out of band — a blocking fetch here would stall a compositor frame on a peer.
pub fn get(hash: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = store().get(hash) {
        clear_wait(hash);
        return Some(bytes);
    }
    if let Some(t) = transport() {
        // Start the clock on the first miss, so `state` can tell a transfer in
        // flight from one nobody is going to answer.
        let _ = first_wanted(hash);
        if may_ask(hash) {
            t.want(hash);
        }
    }
    None
}

/// Write bytes to the process-wide store, returning their hash on success.
/// Also *offers* them to the transport, which is what carries an image out of
/// the process that produced it — the document only ever gains the reference.
/// A quota refusal is deliberately observable so callers do not create a
/// local reference to content this machine failed to retain.
pub fn put(bytes: &[u8]) -> Option<String> {
    let hash = hash_bytes(bytes);
    if let Err(error) = store().put_checked(&hash, bytes) {
        tracing::error!(
            hash = %hash, bytes = bytes.len(), %error,
            "blobstore: refused content; no local document reference was created",
        );
        return None;
    }
    match transport() {
        Some(t) => t.offer(&hash, bytes),
        // A document's images are stored while the app is still starting, so having nowhere to offer them
        // *yet* is the common case, not the odd one; `set_transport` flushes these. Content stored in
        // a scope stays there: the process's transport must not offer it later.
        None if scoped().is_some() => {}
        None => {
            if let Ok(mut q) = pending_offers().write()
                && q.len() < MAX_PENDING_OFFERS
                && !q.iter().any(|h| h == &hash)
            {
                q.push(hash.clone());
            }
        }
    }
    Some(hash)
}

/// Derive a descriptor from content: media type plus intrinsic dimensions, read from the container
/// header rather than a full decode (the point is only to know how much space to reserve). An
/// unrecognised format still gets a correct `len` and reserves no particular size.
pub fn sniff(bytes: &[u8]) -> BlobMeta {
    let mut m = BlobMeta {
        len: bytes.len() as u64,
        ..Default::default()
    };
    if let Some((mime, w, h)) = sniff_image(bytes) {
        m.mime = mime.to_string();
        m.w = w;
        m.h = h;
    }
    m
}

/// Reads a big-endian `u32` at byte offset `at`, or `None` if `b` is too short.
fn read_u32be(b: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_be_bytes(b.get(at..end)?.try_into().ok()?))
}

/// Reads a little-endian `u16` at byte offset `at`, or `None` if `b` is too short.
fn read_u16le(b: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(b.get(at..end)?.try_into().ok()?))
}

/// Reads a big-endian `u16` at byte offset `at`, or `None` if `b` is too short.
fn read_u16be(b: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_be_bytes(b.get(at..end)?.try_into().ok()?))
}

fn sniff_image(b: &[u8]) -> Option<(&'static str, u32, u32)> {
    // PNG: 8-byte signature, then an IHDR chunk whose width/height are the two
    // big-endian u32s at offset 16.
    if b.len() >= 24 && b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(("image/png", read_u32be(b, 16)?, read_u32be(b, 20)?));
    }
    // GIF: logical screen descriptor at offset 6, little-endian u16s.
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        let w = u32::from(read_u16le(b, 6)?);
        let h = u32::from(read_u16le(b, 8)?);
        return Some(("image/gif", w, h));
    }
    // WebP: a RIFF container whose lossy/lossless/extended chunks each state the
    // canvas size in a different place.
    if b.len() >= 30 && b.starts_with(b"RIFF") && b.get(8..12).is_some_and(|s| s == b"WEBP") {
        match b.get(12..16) {
            Some(b"VP8X") => {
                let (b24, b25, b26) = (
                    u32::from(*b.get(24)?),
                    u32::from(*b.get(25)?),
                    u32::from(*b.get(26)?),
                );
                let (b27, b28, b29) = (
                    u32::from(*b.get(27)?),
                    u32::from(*b.get(28)?),
                    u32::from(*b.get(29)?),
                );
                let w = (b24 | (b25 << 8) | (b26 << 16)).saturating_add(1);
                let h = (b27 | (b28 << 8) | (b29 << 16)).saturating_add(1);
                return Some(("image/webp", w, h));
            }
            Some(b"VP8 ") => {
                let w = u32::from(read_u16le(b, 26)? & 0x3FFF);
                let h = u32::from(read_u16le(b, 28)? & 0x3FFF);
                return Some(("image/webp", w, h));
            }
            Some(b"VP8L") => {
                let end = 21usize.checked_add(4)?;
                let bits = u32::from_le_bytes(b.get(21..end)?.try_into().ok()?);
                return Some((
                    "image/webp",
                    (bits & 0x3FFF).saturating_add(1),
                    ((bits >> 14) & 0x3FFF).saturating_add(1),
                ));
            }
            _ => return Some(("image/webp", 0, 0)),
        }
    }
    // JPEG: walk the marker segments to the start-of-frame, honouring each segment's declared length
    // to skip the entropy-coded scan (which a scan for the next 0xFF would not).
    if b.len() >= 4 && b.first() == Some(&0xFF) && b.get(1) == Some(&0xD8) {
        let mut i = 2usize;
        while i.checked_add(3).is_some_and(|end| end < b.len()) {
            let Some(&byte_at_i) = b.get(i) else {
                break;
            };
            if byte_at_i != 0xFF {
                i = i.saturating_add(1);
                continue;
            }
            let Some(&marker) = b.get(i.saturating_add(1)) else {
                break;
            };
            // Standalone markers carry no payload.
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                i = i.saturating_add(2);
                continue;
            }
            if marker == 0xD9 || marker == 0xDA {
                break; // end of image, or the start of the scan data
            }
            if i.saturating_add(3) >= b.len() {
                break;
            }
            let Some(len) = read_u16be(b, i.saturating_add(2)).map(usize::from) else {
                break;
            };
            let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
            if is_sof && i.saturating_add(9) < b.len() {
                let h = u32::from(read_u16be(b, i.saturating_add(5))?);
                let w = u32::from(read_u16be(b, i.saturating_add(7))?);
                return Some(("image/jpeg", w, h));
            }
            if len < 2 {
                break;
            }
            i = i.saturating_add(2).saturating_add(len);
        }
        return Some(("image/jpeg", 0, 0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 PNG, the fixture the codec tests already use.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn png_dimensions_come_from_the_header() {
        let m = sniff(TINY_PNG);
        assert_eq!(m.mime, "image/png");
        assert_eq!((m.w, m.h), (1, 1));
        assert_eq!(m.len, TINY_PNG.len() as u64);
    }

    #[cfg(windows)]
    #[test]
    fn default_blob_root_uses_the_windows_profile_instead_of_temp() {
        let root = FsBlobStore::default_root_with(|key| match key {
            "LOCALAPPDATA" => Some("C:/Users/Photo User/AppData/Local".into()),
            "HOME" => Some("C:/old-unix-home".into()),
            _ => None,
        });
        assert_eq!(
            root,
            PathBuf::from("C:/Users/Photo User/AppData/Local/tilo/blobs")
        );
        let fallback = FsBlobStore::default_root_with(|key| match key {
            "USERPROFILE" => Some("C:/Users/Photo User".into()),
            _ => None,
        });
        assert_eq!(
            fallback,
            PathBuf::from("C:/Users/Photo User/AppData/Local/tilo/blobs")
        );
    }

    #[test]
    fn default_blob_root_preserves_explicit_peer_and_cache_locations() {
        let peer = FsBlobStore::default_root_with(|key| match key {
            "TILO_BLOB_STORE" => Some("peer-blobs".into()),
            "XDG_CACHE_HOME" => Some("cache".into()),
            _ => None,
        });
        assert_eq!(peer, PathBuf::from("peer-blobs"));
        let cache = FsBlobStore::default_root_with(|key| match key {
            "XDG_CACHE_HOME" => Some("cache".into()),
            _ => None,
        });
        assert_eq!(cache, PathBuf::from("cache/tilo/blobs"));
    }

    #[test]
    fn unknown_content_still_reports_its_length() {
        let m = sniff(b"not an image at all");
        assert_eq!(m.mime, "");
        assert_eq!((m.w, m.h), (0, 0));
        assert_eq!(m.len, 19, "length is knowable even when the format is not");
    }

    #[test]
    fn a_store_refuses_bytes_that_do_not_match_their_name() {
        // Accepting bytes that hash to something else would let one peer poison
        // every replica that later reads that hash.
        let s = MemoryBlobStore::default();
        assert!(!s.put(&hash_bytes(b"honest"), b"tampered"));
        assert!(s.get(&hash_bytes(b"honest")).is_none());
        assert!(s.put(&hash_bytes(b"honest"), b"honest"));
    }

    #[test]
    fn a_filesystem_store_round_trips_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let s = FsBlobStore::new(dir.path());
        let h = hash_bytes(TINY_PNG);
        assert!(s.put(&h, TINY_PNG));
        assert_eq!(s.get(&h).as_deref(), Some(TINY_PNG));
        assert!(s.has(&h));
        // The hash arrives from the wire, so a key that is not plain hex must
        // not be able to name a path outside the store.
        assert!(s.get("../../etc/passwd").is_none());
        assert!(!s.put("../escape", b"x"));
    }

    fn limits(item: usize, total: usize, entries: usize) -> BlobStoreLimits {
        BlobStoreLimits {
            max_blob_bytes: item,
            max_total_bytes: total,
            max_entries: entries,
            max_scan_entries: 64,
        }
    }

    #[test]
    fn memory_quota_rejects_without_evicting_referenced_content() {
        let s = MemoryBlobStore::new(limits(4, 6, 2));
        let a = hash_bytes(b"abc");
        let b = hash_bytes(b"defg");
        let c = hash_bytes(b"de");
        assert_eq!(s.put_checked(&a, b"abc"), Ok(()));
        assert_eq!(
            s.put_checked(&b, b"defg"),
            Err(BlobStoreError::TotalQuotaExceeded {
                attempted: 7,
                limit: 6,
            })
        );
        assert_eq!(s.put_checked(&c, b"de"), Ok(()));
        assert_eq!(s.get(&a).as_deref(), Some(b"abc".as_slice()));
        assert_eq!(s.get(&c).as_deref(), Some(b"de".as_slice()));
        assert_eq!(
            s.put_checked(&hash_bytes(b"x"), b"x"),
            Err(BlobStoreError::EntryLimitExceeded { limit: 2 })
        );
        assert_eq!(
            s.put_checked(&hash_bytes(b"12345"), b"12345"),
            Err(BlobStoreError::ItemTooLarge {
                actual: 5,
                limit: 4,
            })
        );
    }

    #[test]
    fn explicit_memory_reclamation_is_complete_and_lexical() {
        let s = MemoryBlobStore::new(limits(8, 24, 8));
        let hashes: Vec<String> = [b"c".as_slice(), b"a", b"b"]
            .iter()
            .map(|bytes| {
                let hash = hash_bytes(bytes);
                s.put_checked(&hash, bytes).unwrap();
                hash
            })
            .collect();
        let live = BTreeSet::from([hashes[0].clone()]);
        let report = s.evict_unreferenced(&live).unwrap();
        let mut expected = vec![hashes[1].clone(), hashes[2].clone()];
        expected.sort();
        assert_eq!(report.hashes, expected);
        assert_eq!(report.bytes, 2);
        assert_eq!(s.get(&hashes[0]).as_deref(), Some(b"c".as_slice()));
        assert!(s.get(&hashes[1]).is_none());
        assert!(s.get(&hashes[2]).is_none());
    }

    #[test]
    fn filesystem_quota_survives_restart_and_explicit_reclaim_reopens_space() {
        let dir = tempfile::tempdir().unwrap();
        let limits = limits(4, 6, 4);
        let a = hash_bytes(b"abc");
        let b = hash_bytes(b"de");
        let c = hash_bytes(b"fgh");
        {
            let s = FsBlobStore::with_limits(dir.path(), limits);
            assert_eq!(s.put_checked(&a, b"abc"), Ok(()));
            assert_eq!(s.put_checked(&b, b"de"), Ok(()));
        }
        let s = FsBlobStore::with_limits(dir.path(), limits);
        assert_eq!(s.get(&a).as_deref(), Some(b"abc".as_slice()));
        assert_eq!(
            s.put_checked(&c, b"fgh"),
            Err(BlobStoreError::TotalQuotaExceeded {
                attempted: 8,
                limit: 6,
            })
        );
        assert_eq!(
            s.evict_unreferenced(&BTreeSet::from([a.clone()]))
                .unwrap()
                .hashes,
            vec![b]
        );
        assert_eq!(s.put_checked(&c, b"fgh"), Ok(()));
        assert_eq!(s.get(&a).as_deref(), Some(b"abc".as_slice()));
        assert_eq!(s.get(&c).as_deref(), Some(b"fgh".as_slice()));
    }

    #[test]
    fn same_root_instances_cannot_race_past_the_disk_quota() {
        let dir = tempfile::tempdir().unwrap();
        let limits = limits(4, 6, 4);
        let first = std::sync::Arc::new(FsBlobStore::with_limits(dir.path(), limits));
        let second = std::sync::Arc::new(FsBlobStore::with_limits(dir.path(), limits));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let a = hash_bytes(b"abcd");
        let b = hash_bytes(b"efgh");
        let (one, two) = std::thread::scope(|scope| {
            let first_barrier = std::sync::Arc::clone(&barrier);
            let first_store = std::sync::Arc::clone(&first);
            let first_thread = scope.spawn(move || {
                first_barrier.wait();
                first_store.put_checked(&a, b"abcd")
            });
            let second_barrier = std::sync::Arc::clone(&barrier);
            let second_store = std::sync::Arc::clone(&second);
            let second_thread = scope.spawn(move || {
                second_barrier.wait();
                second_store.put_checked(&b, b"efgh")
            });
            (first_thread.join().unwrap(), second_thread.join().unwrap())
        });
        assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
        assert!(matches!(
            (one, two),
            (Ok(()), Err(BlobStoreError::TotalQuotaExceeded { .. }))
                | (Err(BlobStoreError::TotalQuotaExceeded { .. }), Ok(()))
        ));
    }

    #[test]
    fn filesystem_partial_and_corrupt_files_are_never_observable_as_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let limits = limits(8, 16, 4);
        let hash = hash_bytes(b"good");
        let shard = dir.path().join(&hash[..2]);
        std::fs::create_dir_all(&shard).unwrap();
        // A crash can leave a unique temporary file.  It is neither indexed nor
        // addressable under the content hash after restart.
        std::fs::write(shard.join(format!(".{hash}.partial-dead")), b"partial").unwrap();
        std::fs::write(shard.join(&hash), b"corrupt").unwrap();
        let s = FsBlobStore::with_limits(dir.path(), limits);
        assert!(s.get(&hash).is_none());
        assert!(!s.has(&hash));
        assert_eq!(s.put_checked(&hash, b"good"), Ok(()));
        assert_eq!(s.get(&hash).as_deref(), Some(b"good".as_slice()));
        assert!(shard.join(format!(".{hash}.partial-dead")).exists());
    }

    #[test]
    fn filesystem_refuses_an_unbounded_startup_scan() {
        let dir = tempfile::tempdir().unwrap();
        let limits = BlobStoreLimits {
            max_blob_bytes: 8,
            max_total_bytes: 16,
            max_entries: 4,
            max_scan_entries: 1,
        };
        for prefix in ["aa", "bb"] {
            std::fs::create_dir_all(dir.path().join(prefix)).unwrap();
        }
        let s = FsBlobStore::with_limits(dir.path(), limits);
        let hash = hash_bytes(b"x");
        assert_eq!(
            s.put_checked(&hash, b"x"),
            Err(BlobStoreError::ScanLimitExceeded { limit: 1 })
        );
    }
}
