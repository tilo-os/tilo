//! `tilo-content` — binary content by address.
//!
//! Apps keep their data as versioned resources: a log of the actions performed on them, replayed
//! into state. That is the default. Some bytes are not a resource, though: the photograph a photo
//! library was imported from, an image placed in a document, a mail attachment. Those are
//! *binary inputs*. A resource refers to one by its content hash, and the bytes live here, once,
//! however many documents and versions refer to them.
//!
//! This crate is that store, and the one supported way for an app to keep opaque bytes:
//!
//! - [`ContentStore`] is a cheap handle an app creates at startup ([`ContentStore::for_app`]) and
//!   passes to whatever needs it; nothing here is process-global. Writes and reads stream
//!   ([`ContentStore::put_reader`], [`ContentStore::open`]) and every read is verified against the
//!   hash that named it.
//! - [`ContentBackend`] is what a store runs on. The device content store behind the filesystem
//!   daemon, reached through the capability the launcher hands the app (`DeviceBackend`), is what
//!   makes content reach an app's other devices and collaborators; [`DirBackend`] keeps content
//!   in a local directory for a process started without one.
//! - [`ContentSource`] is the read side pure code takes (an exporter embedding images), and
//!   [`ContentInputs`] is what pure code hands back (an importer that found images) for the caller
//!   to persist. Neither does I/O of its own.
//!
//! Content is kept until someone who knows every reference collects the rest
//! ([`ContentStore::collect_unreferenced`]): a store never guesses what is garbage, so content a
//! version of a document still names is never lost.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
#[cfg(feature = "dir")]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(all(unix, feature = "device"))]
mod device;
#[cfg(feature = "dir")]
mod dir;
#[cfg(feature = "dir")]
mod files;
mod memory;
mod sniff;

#[cfg(all(unix, feature = "device"))]
pub use device::DeviceBackend;
#[cfg(feature = "dir")]
pub use dir::DirBackend;
#[cfg(feature = "dir")]
pub use files::FilesBackend;
pub use memory::MemoryBackend;
pub use sniff::sniff;

/// The app-files owner of a page's content store (see [`ContentStore::for_app`]).
#[cfg(feature = "dir")]
const PAGE_STORE_OWNER: &str = "so.tilo.content";
/// A page's largest content item and whole store: the page holds its app files in memory.
#[cfg(feature = "dir")]
const PAGE_MAX_ITEM_BYTES: usize = 32 * 1024 * 1024;
#[cfg(feature = "dir")]
const PAGE_MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// The environment variable naming a content directory shared by every app of one session (the
/// shell sets it for the apps it hosts), so content one app stores another can open.
pub const SHARED_STORE_ENV: &str = "TILO_BLOB_STORE";

/// Hash `bytes` the way every content address in the system is derived: BLAKE3, lower-case hex.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// True when `bytes` really are the content named by `hash`.
pub fn verify(hash: &str, bytes: &[u8]) -> bool {
    hash_bytes(bytes) == hash
}

/// Whether `hash` has the shape of a content address (64 lower- or upper-case hex digits).
/// Addresses arrive from documents and peers, so anything else is refused before it can name a path.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// What a document records about content *instead of* its bytes — enough to lay out a placeholder
/// occupying exactly the space the content will, so nothing reflows when the bytes arrive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentMeta {
    /// Media type, sniffed from the content (`image/png`, …). Empty if unknown.
    pub mime: String,
    pub len: u64,
    /// Intrinsic pixel width, 0 when not an image we can read a header from.
    pub w: u32,
    /// Intrinsic pixel height, 0 when unknown.
    pub h: u32,
}

/// Where the content behind an address has got to.
///
/// A renderer that only knows "the bytes are not here" draws the same grey box for a transfer in
/// flight and one that will never complete; the two must be distinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentState {
    /// The content is here; [`ContentStore::read`] returns it.
    Resolved,
    /// Not here yet, and the backend is fetching it from somewhere that has it.
    Pending,
    /// Not here, and nothing this store can reach is going to provide it.
    Unavailable,
}

/// Hard limits for a store. They cover both payload and metadata: a document that names millions
/// of tiny items must not be able to turn a bounded byte budget into an unbounded index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentLimits {
    /// Largest accepted individual item.
    pub max_item_bytes: usize,
    /// Largest aggregate payload retained.
    pub max_total_bytes: usize,
    /// Largest number of retained items.
    pub max_entries: usize,
    /// Largest number of directory entries inspected while building an on-disk index; past it,
    /// writes are refused rather than scanning without bound.
    pub max_scan_entries: usize,
}

impl Default for ContentLimits {
    fn default() -> Self {
        Self {
            max_item_bytes: 64 * 1024 * 1024,
            max_total_bytes: 1024 * 1024 * 1024,
            max_entries: 65_536,
            max_scan_entries: 131_072,
        }
    }
}

/// Why a store refused a write or could not complete a read or collection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContentError {
    #[error("content does not match the address it was stored under")]
    HashMismatch,
    #[error("{0:?} is not a content address")]
    InvalidHash(String),
    #[error("content is {actual} bytes; per-item limit is {limit} bytes")]
    ItemTooLarge { actual: usize, limit: usize },
    #[error("store total would be {attempted} bytes; limit is {limit} bytes")]
    TotalQuotaExceeded { attempted: usize, limit: usize },
    #[error("store would contain more than {limit} items")]
    EntryLimitExceeded { limit: usize },
    #[error("store index exceeded its {limit}-entry scan bound")]
    ScanLimitExceeded { limit: usize },
    #[error("this store cannot collect unreferenced content")]
    CollectionUnsupported,
    #[error("content store I/O: {0}")]
    Io(String),
}

impl From<std::io::Error> for ContentError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Content stored under its address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub hash: String,
    pub len: u64,
}

/// What a collection pass removed, in lexical order of address.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    pub hashes: Vec<String>,
    pub bytes: u64,
}

/// A streaming read of stored content. It fails with [`std::io::ErrorKind::InvalidData`] at the
/// end if the bytes turn out not to match their address, so a consumer that reads to the end has
/// read verified content.
pub type ContentReader = Box<dyn Read + Send>;

/// What a [`ContentStore`] runs on. Implementations treat the address as authoritative and verify
/// content against it; a store that trusts the key would let one peer poison every replica that
/// later reads that address.
pub trait ContentBackend: Send + Sync {
    /// Store everything `reader` yields, returning its address. Storing content already present
    /// is a no-op that succeeds. `mime` is advisory (a backend may index or serve by it).
    ///
    /// # Errors
    /// Quota, integrity or I/O failures. Nothing is stored on error.
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError>;

    /// A streaming, verifying read of the content at `hash`, or `None` when it is not here.
    ///
    /// # Errors
    /// I/O failures other than absence, or a malformed address.
    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError>;

    /// Whether the content at `hash` is here and intact.
    fn has(&self, hash: &str) -> bool;

    /// Where the content at `hash` has got to. A backend that can fetch from elsewhere reports
    /// [`ContentState::Pending`] while it does, and may start fetching when asked.
    fn state(&self, hash: &str) -> ContentState {
        if self.has(hash) {
            ContentState::Resolved
        } else {
            ContentState::Unavailable
        }
    }

    /// Remove every item whose address is not in `live`, which must be complete: the caller
    /// asserts nothing outside it is referenced by anything it is responsible for.
    ///
    /// # Errors
    /// [`ContentError::CollectionUnsupported`] for backends whose retention is governed elsewhere
    /// (a shared device store), or an I/O failure part-way (retry with the same live set).
    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        let _ = live;
        Err(ContentError::CollectionUnsupported)
    }

    /// Whether content stored here reaches beyond this device's own copy: the device content
    /// store serves it to the app's other devices and collaborators. A document may then refer
    /// to content by address alone; with a store that is not shared, a collaborator would be left
    /// holding an address nothing can resolve.
    fn shared(&self) -> bool {
        false
    }
}

/// An app's handle to binary content: cheap to clone, passed to whatever needs it.
#[derive(Clone)]
pub struct ContentStore {
    backend: Arc<dyn ContentBackend>,
}

impl std::fmt::Debug for ContentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentStore").finish_non_exhaustive()
    }
}

impl ContentStore {
    /// A store over `backend`.
    pub fn new(backend: impl ContentBackend + 'static) -> Self {
        Self {
            backend: Arc::new(backend),
        }
    }

    /// A store over a shared backend.
    pub fn from_backend(backend: Arc<dyn ContentBackend>) -> Self {
        Self { backend }
    }

    /// Content that lives and dies with this handle and its clones: tests, headless tools.
    pub fn memory() -> Self {
        Self::new(MemoryBackend::default())
    }

    /// Content in the directory `root`, created on first write.
    #[cfg(feature = "dir")]
    pub fn in_dir(root: impl Into<PathBuf>) -> Self {
        Self::new(DirBackend::new(root))
    }

    /// The store an app uses unless it is handed another: the device content store when the
    /// launcher handed this process its capability ([`ContentStore::device`]), otherwise the
    /// session's shared directory when the host names one ([`SHARED_STORE_ENV`]), otherwise, where
    /// files have no native path (a browser page), the profile's app files ([`FilesBackend`]),
    /// otherwise `tilo/content` in the user's data directory. Content an earlier version kept in the cache
    /// directory (`tilo/blobs`) still reads. With no usable directory at all, content is kept in
    /// memory.
    #[cfg(feature = "dir")]
    pub fn for_app() -> Self {
        #[cfg(all(unix, feature = "device"))]
        if let Some(device) = Self::device() {
            return device;
        }
        Self::for_app_with(&|name| std::env::var_os(name))
    }

    #[cfg(feature = "dir")]
    fn for_app_with(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        if let Some(shared) = env(SHARED_STORE_ENV).filter(|value| !value.is_empty()) {
            return Self::in_dir(PathBuf::from(shared));
        }
        if !matches!(
            tilo_files::availability(tilo_files::Feature::NativePaths),
            tilo_files::Availability::Available
        ) {
            return Self::in_page_files();
        }
        let Some(data) = xdg_paths::data_dir() else {
            tracing::warn!("no data directory; binary content is kept in memory only");
            return Self::memory();
        };
        let mut backend = DirBackend::new(data.join("tilo").join("content"));
        if let Some(cache) = xdg_paths::cache_dir() {
            backend = backend.reading_also(cache.join("tilo").join("blobs"));
        }
        Self::new(backend)
    }

    /// Where files have no native path (a browser page): content in the profile's app files,
    /// which outlive the page. Every app of the profile shares the one store, as native apps
    /// share `tilo/content`. The page holds those files in memory, so the store is kept
    /// smaller than a native one.
    #[cfg(feature = "dir")]
    fn in_page_files() -> Self {
        let Ok(content) = tilo_files::VPath::new("content") else {
            return Self::memory();
        };
        let location = tilo_paths::AppPaths::for_app(PAGE_STORE_OWNER)
            .data()
            .join(&content);
        Self::new(FilesBackend::new(
            location,
            ContentLimits {
                max_item_bytes: PAGE_MAX_ITEM_BYTES,
                max_total_bytes: PAGE_MAX_TOTAL_BYTES,
                ..ContentLimits::default()
            },
        ))
    }

    /// Store `bytes`, returning their address. The media type is sniffed from the content.
    ///
    /// # Errors
    /// See [`ContentBackend::put`].
    pub fn put(&self, bytes: &[u8]) -> Result<String, ContentError> {
        let mime = sniff(bytes).mime;
        let mut reader = bytes;
        self.backend
            .put(&mut reader, &mime)
            .map(|stored| stored.hash)
    }

    /// Store everything `reader` yields without holding it in memory.
    ///
    /// # Errors
    /// See [`ContentBackend::put`].
    pub fn put_reader(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        self.backend.put(reader, mime)
    }

    /// Store every input `inputs` collected (an import's images, say).
    ///
    /// # Errors
    /// The first refusal; inputs stored before it stay stored.
    pub fn put_inputs(&self, inputs: &ContentInputs) -> Result<(), ContentError> {
        for (hash, bytes) in inputs.iter() {
            let mut reader = bytes;
            let stored = self.backend.put(&mut reader, &sniff(bytes).mime)?;
            if stored.hash != hash {
                return Err(ContentError::HashMismatch);
            }
        }
        Ok(())
    }

    /// The verified content at `hash`, or `None` when it is not here (or not intact).
    pub fn read(&self, hash: &str) -> Option<Vec<u8>> {
        let mut reader = self.backend.open(hash).ok()??;
        let mut bytes = Vec::new();
        match reader.read_to_end(&mut bytes) {
            Ok(_) => Some(bytes),
            Err(error) => {
                tracing::warn!(%hash, %error, "content store: unreadable content");
                None
            }
        }
    }

    /// A streaming, verifying read of the content at `hash`; see [`ContentReader`].
    pub fn open(&self, hash: &str) -> Option<ContentReader> {
        self.backend.open(hash).ok().flatten()
    }

    /// Whether the content at `hash` is here.
    pub fn has(&self, hash: &str) -> bool {
        self.backend.has(hash)
    }

    /// Where the content at `hash` has got to.
    pub fn state(&self, hash: &str) -> ContentState {
        self.backend.state(hash)
    }

    /// Whether content stored here reaches the app's other devices and collaborators; see
    /// [`ContentBackend::shared`].
    pub fn shared(&self) -> bool {
        self.backend.shared()
    }

    /// Remove every item not named in `live`; see [`ContentBackend::collect_unreferenced`].
    ///
    /// # Errors
    /// See [`ContentBackend::collect_unreferenced`].
    pub fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        self.backend.collect_unreferenced(live)
    }
}

/// Read access to content by address, for code that turns documents into something else (an
/// exporter embedding images, a renderer) without doing I/O of its own.
pub trait ContentSource {
    /// The bytes at `hash`, if this source has them.
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>>;
}

impl ContentSource for ContentStore {
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>> {
        self.read(hash).map(Cow::Owned)
    }
}

impl ContentSource for BTreeMap<String, Vec<u8>> {
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>> {
        self.get(hash).map(|bytes| Cow::Borrowed(bytes.as_slice()))
    }
}

impl<T: ContentSource + ?Sized> ContentSource for &T {
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>> {
        (**self).content(hash)
    }
}

/// The first source that has the content wins.
impl<A: ContentSource, B: ContentSource> ContentSource for (A, B) {
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>> {
        self.0.content(hash).or_else(|| self.1.content(hash))
    }
}

/// A source with no content at all, for documents known to reference none.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoContent;

impl ContentSource for NoContent {
    fn content(&self, _hash: &str) -> Option<Cow<'_, [u8]>> {
        None
    }
}

/// Binary inputs found by pure code — the images an importer met — keyed by address, for the
/// caller to persist ([`ContentStore::put_inputs`]) before the document naming them is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentInputs {
    entries: BTreeMap<String, Vec<u8>>,
}

impl ContentInputs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take `bytes` as an input, returning their address. The same bytes added twice are kept once.
    pub fn add(&mut self, bytes: impl Into<Vec<u8>>) -> String {
        let bytes = bytes.into();
        let hash = hash_bytes(&bytes);
        self.entries.entry(hash.clone()).or_insert(bytes);
        hash
    }

    pub fn get(&self, hash: &str) -> Option<&[u8]> {
        self.entries.get(hash).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every input, in address order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries
            .iter()
            .map(|(hash, bytes)| (hash.as_str(), bytes.as_slice()))
    }

    /// Move every input of `other` into this set.
    pub fn append(&mut self, other: &mut Self) {
        self.entries.append(&mut other.entries);
    }
}

impl IntoIterator for ContentInputs {
    type Item = (String, Vec<u8>);
    type IntoIter = std::collections::btree_map::IntoIter<String, Vec<u8>>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl ContentSource for ContentInputs {
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>> {
        self.get(hash).map(Cow::Borrowed)
    }
}

/// A reader that hashes what passes through and fails at the end if it was not `expected`.
#[cfg(feature = "dir")]
pub(crate) struct Verifying<R> {
    inner: R,
    hasher: blake3::Hasher,
    expected: String,
    done: bool,
}

#[cfg(feature = "dir")]
impl<R> Verifying<R> {
    pub(crate) fn new(inner: R, expected: &str) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            expected: expected.to_ascii_lowercase(),
            done: false,
        }
    }
}

#[cfg(feature = "dir")]
impl<R: Read> Read for Verifying<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.done = true;
            if self.hasher.finalize().to_hex().as_str() != self.expected {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "content does not match its address",
                ));
            }
            return Ok(0);
        }
        self.hasher.update(buf.get(..n).unwrap_or_default());
        Ok(n)
    }
}

#[cfg(all(test, feature = "dir"))]
mod tests;
