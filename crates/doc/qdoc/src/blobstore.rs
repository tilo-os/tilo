//! Document binaries by content address, and the sniffer that derives a blob's
//! descriptor from its bytes.
//!
//! The bytes are deliberately not in the document: inline media turns every
//! Automerge replica and durable host log into a blob store, amplifies sync
//! memory, and makes ordinary edits wait behind large payloads. The document
//! holds `{hash, mime, len, w, h}` and the bytes live in the device's content
//! store, the same store that holds workspace files.
//!
//! An app reaches that store through the scoped capability the launcher hands
//! it; the SDK installs a [`CachedContentStore`] over it at startup, and
//! [`put`], [`get`], [`state`] and friends go through the installed store. The
//! capability — not this process — decides what the app may read: document
//! media of its room, including content a collaborator put there, which the
//! content store fetches from the collaborator's device. A
//! process with no capability (a unit test, a headless codec run) falls back
//! to a [`MemoryBlobStore`], whose content lives and dies with the process.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Hard limits for an in-process store or cache. They cover both payload and
/// metadata: a document that names millions of tiny blobs must not be able to
/// turn a bounded byte budget into an unbounded index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobStoreLimits {
    /// Largest accepted individual blob.
    pub max_blob_bytes: usize,
    /// Largest aggregate payload held in this process.
    pub max_total_bytes: usize,
    /// Largest number of blobs (and index entries) held in this process.
    pub max_entries: usize,
}

impl Default for BlobStoreLimits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 64 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_entries: 8192,
        }
    }
}

/// Why a store refused a write.
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
    #[error("content store unavailable: {0}")]
    Unavailable(String),
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

/// The answer to asking a store for content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    Ready(Vec<u8>),
    /// On its way: the content store is fetching it from a collaborator.
    Pending,
    /// Not readable here, now or later.
    Unavailable,
}

/// Where this process keeps and finds document binaries. Implementations must
/// treat the hash as authoritative and verify bytes against it — a store that
/// trusted the key would let one peer poison every replica that later reads it.
pub trait BlobStore: Send + Sync {
    /// Store `bytes`, returning their hash once they are retained.
    ///
    /// # Errors
    ///
    /// Returns [`BlobStoreError`] when a quota or the content store refuses.
    fn put(&self, bytes: &[u8], mime: &str) -> Result<String, BlobStoreError>;
    /// Bytes already held in this process; never asks anyone.
    fn cached(&self, hash: &str) -> Option<Vec<u8>>;
    /// Ask for bytes. May make one local round trip; never waits on a peer.
    fn fetch(&self, hash: &str) -> Fetch;
    /// Whether `hash` is readable now.
    fn has(&self, hash: &str) -> bool {
        self.cached(hash).is_some()
    }
    /// True when this store is backed by the device content store, so a miss
    /// can still resolve later and content outlives the process.
    fn shared(&self) -> bool {
        false
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

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

/// Process-local store: the fallback when no content store is reachable, and
/// the test double. It holds the only copy, so a full store refuses new
/// content rather than evicting content a document already references.
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
    fn put(&self, bytes: &[u8], _mime: &str) -> Result<String, BlobStoreError> {
        let hash = hash_bytes(bytes);
        let mut state = self
            .inner
            .write()
            .map_err(|_| BlobStoreError::Unavailable("memory store lock poisoned".into()))?;
        // Re-inserting content already retained consumes no more quota.
        if state.entries.contains_key(&hash) {
            return Ok(hash);
        }
        check_new_blob(&self.limits, state.bytes, state.entries.len(), bytes.len())?;
        state.bytes = state.bytes.saturating_add(bytes.len());
        state.entries.insert(hash.clone(), bytes.to_vec());
        drop(state);
        Ok(hash)
    }

    fn cached(&self, hash: &str) -> Option<Vec<u8>> {
        self.inner.read().ok()?.entries.get(hash).cloned()
    }

    fn fetch(&self, hash: &str) -> Fetch {
        self.cached(hash).map_or(Fetch::Unavailable, Fetch::Ready)
    }
}

/// The content-store capability as this crate sees it. The SDK implements it
/// over the launcher-provided connection; tests implement it directly.
pub trait ContentStoreClient: Send + Sync {
    /// Store `bytes` in the content store, returning their hash.
    ///
    /// # Errors
    ///
    /// Returns [`BlobStoreError`] when the content store refuses or is gone.
    fn put(&self, bytes: &[u8], mime: &str) -> Result<String, BlobStoreError>;
    /// Whole content of `hash`, `Pending` while a collaborator's copy is on
    /// its way, `Unavailable` when this app may not read it.
    fn read(&self, hash: &str) -> Fetch;
    fn has(&self, hash: &str) -> bool;
}

/// The content store behind a bounded in-process cache.
///
/// The content store holds the durable copy, so the cache evicts least
/// recently used entries to stay within its limits instead of refusing. A
/// single blob larger than [`BlobStoreLimits::max_blob_bytes`] is refused
/// before it is sent anywhere.
pub struct CachedContentStore {
    client: Arc<dyn ContentStoreClient>,
    limits: BlobStoreLimits,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    bytes: usize,
    tick: u64,
    entries: BTreeMap<String, (Vec<u8>, u64)>,
}

impl Cache {
    fn get(&mut self, hash: &str) -> Option<Vec<u8>> {
        self.tick = self.tick.wrapping_add(1);
        let tick = self.tick;
        let (bytes, used) = self.entries.get_mut(hash)?;
        *used = tick;
        Some(bytes.clone())
    }

    fn insert(&mut self, limits: &BlobStoreLimits, hash: &str, bytes: &[u8]) {
        if bytes.len() > limits.max_blob_bytes || bytes.len() > limits.max_total_bytes {
            return;
        }
        if let Some((old, _)) = self.entries.remove(hash) {
            self.bytes = self.bytes.saturating_sub(old.len());
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= limits.max_entries
                || self.bytes.saturating_add(bytes.len()) > limits.max_total_bytes)
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(hash, _)| hash.clone())
            else {
                break;
            };
            if let Some((evicted, _)) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(evicted.len());
            }
        }
        if limits.max_entries == 0 {
            return;
        }
        self.tick = self.tick.wrapping_add(1);
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.entries
            .insert(hash.to_owned(), (bytes.to_vec(), self.tick));
    }
}

impl CachedContentStore {
    pub fn new(client: Arc<dyn ContentStoreClient>, limits: BlobStoreLimits) -> Self {
        Self {
            client,
            limits,
            cache: Mutex::new(Cache::default()),
        }
    }

    pub fn limits(&self) -> BlobStoreLimits {
        self.limits
    }

    /// Bytes held in the cache right now.
    pub fn cached_bytes(&self) -> usize {
        self.cache.lock().map_or(0, |cache| cache.bytes)
    }

    fn remember(&self, hash: &str, bytes: &[u8]) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(&self.limits, hash, bytes);
        }
    }
}

impl BlobStore for CachedContentStore {
    fn put(&self, bytes: &[u8], mime: &str) -> Result<String, BlobStoreError> {
        if bytes.len() > self.limits.max_blob_bytes {
            return Err(BlobStoreError::ItemTooLarge {
                actual: bytes.len(),
                limit: self.limits.max_blob_bytes,
            });
        }
        let hash = hash_bytes(bytes);
        let stored = self.client.put(bytes, mime)?;
        if stored != hash {
            return Err(BlobStoreError::HashMismatch);
        }
        self.remember(&hash, bytes);
        Ok(hash)
    }

    fn cached(&self, hash: &str) -> Option<Vec<u8>> {
        self.cache.lock().ok()?.get(hash)
    }

    fn fetch(&self, hash: &str) -> Fetch {
        if let Some(bytes) = self.cached(hash) {
            return Fetch::Ready(bytes);
        }
        if !valid_hash(hash) {
            return Fetch::Unavailable;
        }
        match self.client.read(hash) {
            Fetch::Ready(bytes) if verify(hash, &bytes) => {
                self.remember(hash, &bytes);
                Fetch::Ready(bytes)
            }
            // Bytes that are not the content they claim to be are corruption,
            // and never reach a renderer.
            Fetch::Ready(_) | Fetch::Unavailable => Fetch::Unavailable,
            Fetch::Pending => Fetch::Pending,
        }
    }

    fn has(&self, hash: &str) -> bool {
        self.cached(hash).is_some() || (valid_hash(hash) && self.client.has(hash))
    }

    fn shared(&self) -> bool {
        true
    }
}

static STORE: OnceLock<RwLock<Arc<dyn BlobStore>>> = OnceLock::new();

fn cell() -> &'static RwLock<Arc<dyn BlobStore>> {
    STORE.get_or_init(|| RwLock::new(Arc::new(MemoryBlobStore::default())))
}

std::thread_local! {
    static SCOPED: std::cell::RefCell<Option<Arc<dyn BlobStore>>> = const { std::cell::RefCell::new(None) };
}

/// Replaces the process-wide store on the thread that created it; dropping it restores what the
/// thread used before. See [`scope`].
#[must_use = "the scope ends when this is dropped"]
pub struct BlobScope {
    previous: Option<Arc<dyn BlobStore>>,
    /// Tied to the thread whose override it restores.
    _thread: std::marker::PhantomData<*const ()>,
}

impl Drop for BlobScope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SCOPED.with(|s| *s.borrow_mut() = previous);
        forget_outcomes();
    }
}

/// Use `store` instead of the process-wide one on this thread until the returned guard drops.
///
/// For code that must not see or disturb the process's store: a test that needs a store with its
/// own quota or contents installs it here and cannot leave it behind for whatever runs next, even
/// when it fails. Scopes nest; each restores the one it replaced.
pub fn scope(store: Arc<dyn BlobStore>) -> BlobScope {
    let previous = SCOPED.with(|s| s.borrow_mut().replace(store));
    // What was asked of the previous store says nothing about this one.
    forget_outcomes();
    BlobScope {
        previous,
        _thread: std::marker::PhantomData,
    }
}

/// This thread's scoped store, if a [`scope`] is active.
fn scoped() -> Option<Arc<dyn BlobStore>> {
    SCOPED.with(|s| s.borrow().clone())
}

/// Install the process-wide store. A global rather than a handle threaded through `DocMut`,
/// because otherwise every codec and every caller of `put_blob` needs the parameter, and the
/// store is a property of the process rather than of a document.
pub fn set_store(store: Arc<dyn BlobStore>) {
    if let Ok(mut s) = cell().write() {
        *s = store;
    }
    forget_outcomes();
}

/// The process-wide store: the content store the SDK installed, or an
/// in-memory store when none is reachable. A [`scope`] on the calling thread takes precedence.
pub fn store() -> Arc<dyn BlobStore> {
    if let Some(scoped) = scoped() {
        return scoped;
    }
    match cell().read() {
        Ok(s) => s.clone(),
        Err(_) => Arc::new(MemoryBlobStore::default()),
    }
}

/// How long to wait before asking again for the same content. A renderer calls [`get`] every
/// frame, so without this one missing image would put a request on every frame.
const ASK_AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// Where a reference's content has got to.
///
/// A renderer that only knows "the bytes are not here" draws the same grey box for a transfer in
/// flight and one that will never complete; the two must be distinguishable before anything can be drawn honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobState {
    /// The content is here; [`get`] returns it.
    Resolved,
    /// Not here, and expected: the content store is fetching it, and the
    /// answer has had less than [`GIVE_UP_AFTER`] to arrive.
    Pending,
    /// Not here, and not coming: the store says this app cannot read it, no
    /// store beyond this process exists, or nobody answered in time.
    Unavailable,
}

/// How long a missing blob stays [`BlobState::Pending`] before this process stops calling it
/// "arriving". Sized against the content store's peer fetch, which tries each known collaborator
/// in turn — sooner would call an in-flight image lost, much later leaves a reader watching a box
/// that says "arriving" long after it stopped.
pub const GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(45);

#[derive(Clone, Copy)]
struct Outcome {
    first_wanted: std::time::Instant,
    last_asked: std::time::Instant,
    unavailable: bool,
}

static OUTCOMES: OnceLock<Mutex<BTreeMap<String, Outcome>>> = OnceLock::new();

fn outcomes() -> &'static Mutex<BTreeMap<String, Outcome>> {
    OUTCOMES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn forget_outcomes() {
    if let Ok(mut map) = outcomes().lock() {
        map.clear();
    }
}

/// Whether `get` may ask the store about `hash` now, recording the ask.
fn may_ask(hash: &str) -> bool {
    let now = std::time::Instant::now();
    let Ok(mut map) = outcomes().lock() else {
        return false;
    };
    if let Some(outcome) = map.get_mut(hash) {
        if now.duration_since(outcome.last_asked) < ASK_AGAIN_AFTER {
            return false;
        }
        outcome.last_asked = now;
        return true;
    }
    // Bounded: a document referencing thousands of blobs must not turn this into a leak.
    // Evicting oldest-first costs at most one extra request for whatever was evicted.
    if map.len() >= 512
        && let Some(oldest) = map
            .iter()
            .min_by_key(|(_, outcome)| outcome.last_asked)
            .map(|(hash, _)| hash.clone())
    {
        map.remove(&oldest);
    }
    map.insert(
        hash.to_owned(),
        Outcome {
            first_wanted: now,
            last_asked: now,
            unavailable: false,
        },
    );
    true
}

fn record(hash: &str, fetch: &Fetch) {
    let Ok(mut map) = outcomes().lock() else {
        return;
    };
    match fetch {
        // Forget that `hash` was ever waited on, so a re-reference after it
        // landed starts its clock fresh rather than inheriting an expired one.
        Fetch::Ready(_) => {
            map.remove(hash);
        }
        Fetch::Pending => {
            if let Some(outcome) = map.get_mut(hash) {
                outcome.unavailable = false;
            }
        }
        Fetch::Unavailable => {
            if let Some(outcome) = map.get_mut(hash) {
                outcome.unavailable = true;
            }
        }
    }
}

/// Whether the content behind `hash` is here, on its way, or not coming. Reports; does not ask —
/// [`get`] is what asks, and a renderer already calls that every frame.
pub fn state(hash: &str) -> BlobState {
    let store = store();
    if store.cached(hash).is_some() {
        return BlobState::Resolved;
    }
    let outcome = outcomes()
        .lock()
        .ok()
        .and_then(|map| map.get(hash).copied());
    match outcome {
        Some(outcome) if outcome.unavailable => BlobState::Unavailable,
        Some(outcome) if outcome.first_wanted.elapsed() >= GIVE_UP_AFTER => BlobState::Unavailable,
        Some(_) => BlobState::Pending,
        // Nothing has asked yet. With no store beyond this process a miss is
        // permanent, and "arriving" would be a lie that never resolves.
        None if store.shared() => BlobState::Pending,
        None => BlobState::Unavailable,
    }
}

/// Read a blob's bytes. Content already in this process returns at once; otherwise the store is
/// asked (at most once per [`ASK_AGAIN_AFTER`]) and a miss returns `None`: the caller renders its
/// placeholder now and a later frame draws the image once the content store has fetched it — a
/// blocking fetch here would stall a compositor frame on a peer.
pub fn get(hash: &str) -> Option<Vec<u8>> {
    let store = store();
    if let Some(bytes) = store.cached(hash) {
        record(hash, &Fetch::Ready(Vec::new()));
        return Some(bytes);
    }
    if !may_ask(hash) {
        return None;
    }
    let fetch = store.fetch(hash);
    record(hash, &fetch);
    match fetch {
        Fetch::Ready(bytes) => Some(bytes),
        Fetch::Pending | Fetch::Unavailable => None,
    }
}

/// Store bytes, returning their hash once the store has retained them. A refusal is
/// deliberately observable so callers do not create a document reference to content this
/// device failed to keep.
pub fn put(bytes: &[u8]) -> Option<String> {
    let mime = sniff(bytes).mime;
    match store().put(bytes, &mime) {
        Ok(hash) => {
            record(&hash, &Fetch::Ready(Vec::new()));
            Some(hash)
        }
        Err(error) => {
            tracing::error!(
                bytes = bytes.len(), %error,
                "blobstore: refused content; no document reference was created",
            );
            None
        }
    }
}

/// What an import of a private legacy store did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyImport {
    /// Blobs now in the content store.
    pub imported: usize,
    /// Files that were not intact blobs (wrong name, corrupt, oversized).
    pub skipped: usize,
    /// The directory had already been imported, or held no store.
    pub already_done: bool,
}

const LEGACY_IMPORT_MARKER: &str = ".imported-into-content-store";
const MAX_LEGACY_SCAN: usize = 65_536;

/// Move the blobs of an app's private legacy store (`<root>/<2 hex>/<64 hex>`,
/// the layout apps used before document media moved into the content store)
/// into the installed store, so nothing a document references is lost.
///
/// Idempotent: a marker records a complete import and later calls return at
/// once; an interrupted import simply runs again, since putting content the
/// store already holds is a no-op. The files are left where they are. Nothing
/// is imported unless the installed store is shared, because importing into a
/// process-local fallback and marking the directory done would forget the
/// content at exit.
///
/// # Errors
///
/// Returns [`BlobStoreError::Unavailable`] when the directory cannot be read
/// or the marker cannot be written, or the store's refusal of a blob.
pub fn import_legacy_directory(root: &Path) -> Result<LegacyImport, BlobStoreError> {
    import_legacy_into(&*store(), root)
}

/// [`import_legacy_directory`] into an explicit store.
///
/// # Errors
///
/// As [`import_legacy_directory`].
pub fn import_legacy_into(
    store: &dyn BlobStore,
    root: &Path,
) -> Result<LegacyImport, BlobStoreError> {
    let io = |error: std::io::Error| BlobStoreError::Unavailable(error.to_string());
    if !store.shared() {
        return Err(BlobStoreError::Unavailable(
            "no content store to import into".into(),
        ));
    }
    let marker = root.join(LEGACY_IMPORT_MARKER);
    if marker.exists() {
        return Ok(LegacyImport {
            already_done: true,
            ..LegacyImport::default()
        });
    }
    let shards = match std::fs::read_dir(root) {
        Ok(shards) => shards,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LegacyImport {
                already_done: true,
                ..LegacyImport::default()
            });
        }
        Err(error) => return Err(io(error)),
    };
    let mut report = LegacyImport::default();
    let mut scanned = 0_usize;
    let mut complete = true;
    'shards: for shard in shards {
        let shard = shard.map_err(io)?;
        let name = shard.file_name();
        let Some(prefix) = name.to_str() else {
            continue;
        };
        if prefix.len() != 2
            || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !shard.file_type().map_err(io)?.is_dir()
        {
            continue;
        }
        for entry in std::fs::read_dir(shard.path()).map_err(io)? {
            scanned = scanned.saturating_add(1);
            if scanned > MAX_LEGACY_SCAN {
                // Leave the marker unwritten: the next run continues.
                complete = false;
                break 'shards;
            }
            let entry = entry.map_err(io)?;
            let name = entry.file_name();
            let Some(hash) = name.to_str().filter(|hash| valid_hash(hash)) else {
                continue;
            };
            if !hash.starts_with(prefix) || !entry.file_type().map_err(io)?.is_file() {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            };
            if !verify(hash, &bytes) {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
            match store.put(&bytes, &sniff(&bytes).mime) {
                Ok(_) => report.imported = report.imported.saturating_add(1),
                Err(BlobStoreError::ItemTooLarge { .. }) => {
                    report.skipped = report.skipped.saturating_add(1);
                }
                Err(error) => return Err(error),
            }
        }
    }
    if complete {
        std::fs::write(
            &marker,
            b"document media moved to the device content store\n",
        )
        .map_err(io)?;
    }
    Ok(report)
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

    #[test]
    fn unknown_content_still_reports_its_length() {
        let m = sniff(b"not an image at all");
        assert_eq!(m.mime, "");
        assert_eq!((m.w, m.h), (0, 0));
        assert_eq!(m.len, 19, "length is knowable even when the format is not");
    }

    fn limits(item: usize, total: usize, entries: usize) -> BlobStoreLimits {
        BlobStoreLimits {
            max_blob_bytes: item,
            max_total_bytes: total,
            max_entries: entries,
        }
    }

    #[test]
    fn memory_quota_rejects_without_evicting_referenced_content() {
        let s = MemoryBlobStore::new(limits(4, 6, 2));
        let a = s.put(b"abc", "").unwrap();
        assert_eq!(
            s.put(b"defg", ""),
            Err(BlobStoreError::TotalQuotaExceeded {
                attempted: 7,
                limit: 6,
            })
        );
        let c = s.put(b"de", "").unwrap();
        assert_eq!(s.cached(&a).as_deref(), Some(b"abc".as_slice()));
        assert_eq!(s.fetch(&c), Fetch::Ready(b"de".to_vec()));
        // Re-putting retained content costs nothing.
        assert_eq!(s.put(b"abc", ""), Ok(a));
        assert_eq!(
            s.put(b"x", ""),
            Err(BlobStoreError::EntryLimitExceeded { limit: 2 })
        );
        assert_eq!(
            s.put(b"12345", ""),
            Err(BlobStoreError::ItemTooLarge {
                actual: 5,
                limit: 4,
            })
        );
        assert_eq!(s.fetch(&hash_bytes(b"never stored")), Fetch::Unavailable);
    }

    /// A content store that answers `Pending` a set number of times before
    /// serving what was put, and can be told to corrupt its answers.
    #[derive(Default)]
    struct FakeContentStore {
        stored: Mutex<BTreeMap<String, Vec<u8>>>,
        pending_reads: Mutex<usize>,
        reads: Mutex<usize>,
        corrupt: std::sync::atomic::AtomicBool,
    }

    impl ContentStoreClient for FakeContentStore {
        fn put(&self, bytes: &[u8], _mime: &str) -> Result<String, BlobStoreError> {
            let hash = hash_bytes(bytes);
            self.stored
                .lock()
                .unwrap()
                .insert(hash.clone(), bytes.to_vec());
            Ok(hash)
        }

        fn read(&self, hash: &str) -> Fetch {
            {
                let mut reads = self.reads.lock().unwrap();
                *reads = reads.saturating_add(1);
            }
            let mut pending = self.pending_reads.lock().unwrap();
            if let Some(remaining) = pending.checked_sub(1) {
                *pending = remaining;
                return Fetch::Pending;
            }
            drop(pending);
            match self.stored.lock().unwrap().get(hash) {
                Some(_) if self.corrupt.load(std::sync::atomic::Ordering::Relaxed) => {
                    Fetch::Ready(b"tampered".to_vec())
                }
                Some(bytes) => Fetch::Ready(bytes.clone()),
                None => Fetch::Unavailable,
            }
        }

        fn has(&self, hash: &str) -> bool {
            self.stored.lock().unwrap().contains_key(hash)
        }
    }

    #[test]
    fn the_cache_evicts_least_recently_used_content_the_store_still_holds() {
        let content = Arc::new(FakeContentStore::default());
        let cached = CachedContentStore::new(
            Arc::clone(&content) as Arc<dyn ContentStoreClient>,
            limits(4, 6, 8),
        );
        let a = cached.put(b"aaa", "").unwrap();
        let b = cached.put(b"bbb", "").unwrap();
        // Touch `a` so `b` is the least recently used.
        assert!(cached.cached(&a).is_some());
        let c = cached.put(b"cc", "").unwrap();
        assert!(
            cached.cached(&b).is_none(),
            "the cache stayed within 6 bytes"
        );
        assert!(cached.cached(&a).is_some() && cached.cached(&c).is_some());
        assert!(cached.cached_bytes() <= 6);
        // Evicted from the cache is not lost: the content store answers.
        assert_eq!(cached.fetch(&b), Fetch::Ready(b"bbb".to_vec()));
        assert!(cached.has(&b));
        // A blob over the per-item limit never reaches the content store.
        assert_eq!(
            cached.put(b"12345", ""),
            Err(BlobStoreError::ItemTooLarge {
                actual: 5,
                limit: 4,
            })
        );
        assert!(!content.has(&hash_bytes(b"12345")));
    }

    #[test]
    fn content_from_the_store_is_verified_before_anyone_draws_it() {
        let content = Arc::new(FakeContentStore::default());
        let hash = content.put(b"honest", "").unwrap();
        content
            .corrupt
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let cached = CachedContentStore::new(
            Arc::clone(&content) as Arc<dyn ContentStoreClient>,
            BlobStoreLimits::default(),
        );
        assert_eq!(cached.fetch(&hash), Fetch::Unavailable);
        assert_eq!(cached.fetch("../../etc/passwd"), Fetch::Unavailable);
        assert!(cached.cached(&hash).is_none());
    }

    #[test]
    fn a_collaborators_image_is_pending_until_the_store_has_it() {
        let content = Arc::new(FakeContentStore::default());
        let hash = content.put(TINY_PNG, "image/png").unwrap();
        *content.pending_reads.lock().unwrap() = 1;
        let cached = CachedContentStore::new(
            Arc::clone(&content) as Arc<dyn ContentStoreClient>,
            BlobStoreLimits::default(),
        );
        assert_eq!(cached.fetch(&hash), Fetch::Pending);
        assert_eq!(cached.fetch(&hash), Fetch::Ready(TINY_PNG.to_vec()));
        // Now cached: no further round trip.
        let reads = *content.reads.lock().unwrap();
        assert_eq!(cached.fetch(&hash), Fetch::Ready(TINY_PNG.to_vec()));
        assert_eq!(*content.reads.lock().unwrap(), reads);
    }

    fn legacy_store(root: &Path, blobs: &[&[u8]]) -> Vec<String> {
        blobs
            .iter()
            .map(|bytes| {
                let hash = hash_bytes(bytes);
                let shard = root.join(&hash[..2]);
                std::fs::create_dir_all(&shard).unwrap();
                std::fs::write(shard.join(&hash), bytes).unwrap();
                hash
            })
            .collect()
    }

    #[test]
    fn legacy_private_stores_import_once_and_skip_what_is_not_intact() {
        let dir = tempfile::tempdir().unwrap();
        let hashes = legacy_store(dir.path(), &[TINY_PNG, b"an attachment"]);
        // Crash leftovers and corruption are not content.
        let first_shard_dir = dir.path().join(&hashes[0][..2]);
        std::fs::write(
            first_shard_dir.join(format!(".{}.partial-1-0", hashes[0])),
            b"half",
        )
        .unwrap();
        let corrupt = hash_bytes(b"what the name promises");
        std::fs::create_dir_all(dir.path().join(&corrupt[..2])).unwrap();
        std::fs::write(dir.path().join(&corrupt[..2]).join(&corrupt), b"other").unwrap();

        let memory = MemoryBlobStore::default();
        assert!(
            import_legacy_into(&memory, dir.path()).is_err(),
            "a process-local store must not claim the import"
        );
        assert!(!dir.path().join(LEGACY_IMPORT_MARKER).exists());

        let content = Arc::new(FakeContentStore::default());
        let shared = CachedContentStore::new(
            Arc::clone(&content) as Arc<dyn ContentStoreClient>,
            BlobStoreLimits::default(),
        );
        let report = import_legacy_into(&shared, dir.path()).unwrap();
        assert_eq!(report.imported, 2);
        assert_eq!(report.skipped, 1);
        assert!(!report.already_done);
        for hash in &hashes {
            assert!(content.has(hash), "legacy blob {hash} reached the store");
        }
        assert!(!content.has(&corrupt));

        let again = import_legacy_into(&shared, dir.path()).unwrap();
        assert!(again.already_done);
        assert_eq!(again.imported, 0);
        assert!(
            dir.path().join(&hashes[0][..2]).join(&hashes[0]).exists(),
            "the original files stay in place"
        );
        let missing = import_legacy_into(&shared, &dir.path().join("never-created")).unwrap();
        assert!(missing.already_done);
    }
}
