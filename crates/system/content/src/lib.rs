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
//! - [`ContentStore`] is a cheap handle an app creates at startup (`tilo_content_device::for_app`)
//!   and passes to whatever needs it; nothing here is process-global. Writes and reads stream
//!   ([`ContentStore::put_reader`], [`ContentStore::open`]) and every read is verified against the
//!   hash that named it.
//! - [`ContentBackend`] is what a store runs on. The device content store behind the filesystem
//!   daemon, reached through the capability the launcher hands the app
//!   (`tilo_content_device::DeviceBackend`), is what makes content reach an app's other devices
//!   and collaborators; [`DirBackend`] keeps content in a local directory for a process started
//!   without one. The device backend lives in its own crate so this one depends on no daemon
//!   protocol: the resource layer, and the low-level types it stores, can name content without
//!   sitting above the filesystem.
//! - [`ContentSource`] is the read side pure code takes (an exporter embedding images), and
//!   [`ContentInputs`] is what pure code hands back (an importer that found images) for the caller
//!   to persist. Neither does I/O of its own.
//!
//! Content a record references is sealed before it leaves the writer
//! ([`ContentStore::sealing`]): each item under a fresh key, stored and
//! carried under the address of its ciphertext, named by a reference that
//! carries the key ([`ContentRef`]). Only the record's readers open it;
//! whoever stores or serves it holds ciphertext alone.
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

#[cfg(feature = "dir")]
mod dir;
#[cfg(feature = "dir")]
mod files;
mod memory;
mod reference;
#[cfg(feature = "dir")]
mod workspaces;

#[cfg(feature = "dir")]
pub use dir::DirBackend;
#[cfg(feature = "dir")]
pub use files::FilesBackend;
pub use memory::MemoryBackend;
pub use reference::{ContentRef, content_address, is_sealed, valid_reference};
pub use tilo_content_types::{
    ContentMeta, DocumentFormat, ImageFormat, RECORDING, RECORDING_MAGIC, document_format,
    image_format, image_size, opened_as, opened_by, sniff, valid_hash,
};
#[cfg(feature = "dir")]
pub use workspaces::WorkspaceStores;

/// The app-files owner of a page's content store (see [`ContentStore::local_for_app`]).
#[cfg(feature = "dir")]
const PAGE_STORE_OWNER: &str = "so.tilo.content";
/// A page's largest content item: the largest a device keeps of a record's
/// content, which a page holds whole while it stores it.
#[cfg(feature = "dir")]
const PAGE_MAX_ITEM_BYTES: usize = 64 * 1024 * 1024;
/// Largest whole store a page keeps: the page holds its app files in memory.
#[cfg(feature = "dir")]
const PAGE_MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// The environment variable naming a content directory shared by every app of one session (the
/// shell sets it for the apps it hosts), so content one app stores another can open.
pub const SHARED_STORE_ENV: &str = "TILO_BLOB_STORE";

/// The media type sealed content is stored as: its bytes say nothing of what they hold.
const SEALED_MIME: &str = "application/octet-stream";

/// The address `reference` names, or `reference` itself when it is none (a backend refuses it).
fn address_of(reference: &str) -> String {
    content_address(reference).unwrap_or_else(|| reference.to_owned())
}

/// Hash `bytes` the way every content address in the system is derived: BLAKE3, lower-case hex.
pub fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Whether `bytes` are the content `reference` names: the bytes its address hashes, or for a
/// sealed reference the bytes whose sealing under its key that address hashes.
pub fn names(reference: &str, bytes: &[u8]) -> bool {
    let Some(parsed) = ContentRef::parse(reference) else {
        return false;
    };
    match parsed.key() {
        None => verify(parsed.address(), bytes),
        Some(key) => tilo_seal::content::seal(key, bytes)
            .is_ok_and(|sealed| verify(parsed.address(), &sealed)),
    }
}

/// True when `bytes` really are the content named by `hash`.
pub fn verify(hash: &str, bytes: &[u8]) -> bool {
    hash_bytes(bytes) == hash
}

/// Where the content behind an address has got to.
///
/// A renderer that only knows "the bytes are not here" draws the same grey box for a transfer in
/// flight and one that will never complete; the two must be distinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContentState {
    /// The content is here; [`ContentStore::read`] returns it.
    Resolved,
    /// Not here yet, and the backend is fetching it from somewhere that has it.
    Pending,
    /// Not here, and none of the devices that could hand it over is connected right now. It is
    /// fetched the moment one connects; nothing waits on a timer.
    Waiting,
    /// Not here, and nothing this store can reach is going to provide it: every device that could
    /// hold it said it does not.
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
    #[error(
        "the disk holding this content has {free} bytes free, under the {keep_free} it keeps \
         for the host's own state: nothing new is kept until there is room"
    )]
    DiskLow { free: u64, keep_free: u64 },
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

    /// Remove the item `hash`, when the one who keeps it here knows nothing it is responsible
    /// for references it any longer (a quota letting the oldest item go). Content kept for
    /// another reason under the same address goes with it, so a caller sharing a store removes
    /// only what it alone keeps.
    ///
    /// # Errors
    /// [`ContentError::CollectionUnsupported`] for backends whose retention is governed elsewhere,
    /// or an I/O failure.
    fn release(&self, hash: &str) -> Result<Collected, ContentError> {
        let _ = hash;
        Err(ContentError::CollectionUnsupported)
    }

    /// Whether content stored here reaches beyond this device's own copy: the device content
    /// store serves it to the app's other devices and collaborators. A document may then refer
    /// to content by address alone; with a store that is not shared, a collaborator would be left
    /// holding an address nothing can resolve.
    fn shared(&self) -> bool {
        false
    }

    /// Whether this store answers only for content readable in one workspace: the device content
    /// store reads through the scope the launcher handed the window, and serves an address only
    /// when that workspace references it. A caller's content reference is resolved only in such
    /// a store, so naming a hash never reaches content from elsewhere on this machine.
    fn workspace_scoped(&self) -> bool {
        false
    }
}

/// A store a host keeps for one workspace alone
/// ([`ContentStore::for_one_workspace`]).
struct OneWorkspace(Arc<dyn ContentBackend>);

impl ContentBackend for OneWorkspace {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        self.0.put(reader, mime)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        self.0.open(hash)
    }

    fn has(&self, hash: &str) -> bool {
        self.0.has(hash)
    }

    fn state(&self, hash: &str) -> ContentState {
        self.0.state(hash)
    }

    fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        self.0.collect_unreferenced(live)
    }

    fn release(&self, hash: &str) -> Result<Collected, ContentError> {
        self.0.release(hash)
    }

    fn shared(&self) -> bool {
        self.0.shared()
    }

    fn workspace_scoped(&self) -> bool {
        true
    }
}

/// An app's handle to binary content: cheap to clone, passed to whatever needs it.
///
/// Everything here takes a reference ([`ContentRef`]) where it names content: a plain address, or a
/// sealed reference, which reads through its key. What a store hands back for what it stored is the
/// reference to keep: sealed when the store seals ([`Self::sealing`]).
#[derive(Clone)]
pub struct ContentStore {
    backend: Arc<dyn ContentBackend>,
    seals: bool,
}

impl std::fmt::Debug for ContentStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentStore").finish_non_exhaustive()
    }
}

impl ContentStore {
    /// A store over `backend`.
    pub fn new(backend: impl ContentBackend + 'static) -> Self {
        Self::from_backend(Arc::new(backend))
    }

    /// A store over a shared backend.
    pub fn from_backend(backend: Arc<dyn ContentBackend>) -> Self {
        Self {
            backend,
            seals: false,
        }
    }

    /// This store, sealing what it stores: each item under a fresh key, kept under the address of
    /// its ciphertext, and named by a reference that carries the key. The store a record's content
    /// travels through seals, so the record's readers alone open it.
    #[must_use]
    pub fn sealing(self) -> Self {
        Self {
            seals: true,
            ..self
        }
    }

    /// This store, keeping what it stores under its own address, unsealed: for code that must name
    /// content alike wherever it runs (an action applying on every replica). Sealed references
    /// still read.
    #[must_use]
    pub fn unsealed(self) -> Self {
        Self {
            seals: false,
            ..self
        }
    }

    /// Whether this store seals what it stores ([`Self::sealing`]).
    pub const fn seals(&self) -> bool {
        self.seals
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

    /// The store an app keeps on this machine when the launcher handed it no device content
    /// store (`tilo_content_device::for_app` prefers that one): the session's shared directory
    /// when the host names one ([`SHARED_STORE_ENV`]), otherwise, where files have no native path
    /// (a browser page), the profile's app files ([`FilesBackend`]), otherwise `tilo/content` in
    /// the user's data directory. Content an earlier version kept in the cache directory
    /// (`tilo/blobs`) still reads. With no usable directory at all, content is kept in memory.
    #[cfg(feature = "dir")]
    pub fn local_for_app() -> Self {
        Self::for_app_with(&|name| std::env::var_os(name))
    }

    #[cfg(feature = "dir")]
    fn for_app_with(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        if let Some(shared) = env(SHARED_STORE_ENV).filter(|value| !value.is_empty()) {
            return Self::in_dir(PathBuf::from(shared));
        }
        if !tilo_files::available(tilo_files::Feature::NativePaths) {
            return Self::page_files();
        }
        let Some(data) = tilo_paths::data_dir() else {
            tracing::warn!("no data directory; binary content is kept in memory only");
            return Self::memory();
        };
        let mut backend = DirBackend::new(data.join("tilo").join("content"));
        if let Some(cache) = tilo_paths::cache_dir() {
            backend = backend.reading_also(cache.join("tilo").join("blobs"));
        }
        Self::new(backend)
    }

    /// Where files have no native path (a browser page): content in the profile's app files,
    /// which outlive the page, so an upload a page stored is still here after a reload. Every
    /// app of the profile shares the one store, as native apps share `tilo/content`. The page
    /// holds those files in memory, so the store is bounded: an item or store past the bound is
    /// refused, never dropped, and nothing collects what the page stored on its own. A page whose
    /// host loaded no app files keeps content in memory, under the same bounds, as a tab alone can.
    ///
    /// This is the store a page host lends each window it opens, as the framework's native
    /// process host lends `tilo_content_device::for_app`'s: a window keeps content in the store
    /// its host lent it (`cx.content()`), never in one it opens itself.
    #[cfg(feature = "dir")]
    #[must_use]
    pub fn page_files() -> Self {
        let limits = ContentLimits {
            max_item_bytes: PAGE_MAX_ITEM_BYTES,
            max_total_bytes: PAGE_MAX_TOTAL_BYTES,
            ..ContentLimits::default()
        };
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        let mirrored = tilo_files::web::is_mirrored(&tilo_paths::default_profile());
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        let mirrored = true;
        match tilo_files::VPath::new("content") {
            Ok(content) if mirrored => Self::new(FilesBackend::new(
                tilo_paths::AppPaths::for_app(PAGE_STORE_OWNER)
                    .data()
                    .join(&content),
                limits,
            )),
            _ => Self::new(MemoryBackend::new(limits)),
        }
    }

    /// Store `bytes`, returning the reference to keep: sealed when this store seals. The media
    /// type is sniffed from the content.
    ///
    /// # Errors
    /// See [`ContentBackend::put`].
    pub fn put(&self, bytes: &[u8]) -> Result<String, ContentError> {
        let mime = sniff(bytes).mime;
        let mut reader = bytes;
        self.put_reader(&mut reader, &mime)
            .map(|stored| stored.hash)
    }

    /// Store `bytes` as they are, under their own address, even in a store that seals: content
    /// that is public by nature (an app's surface, which whoever decides its changes reads).
    ///
    /// # Errors
    /// See [`ContentBackend::put`].
    pub fn put_public(&self, bytes: &[u8]) -> Result<String, ContentError> {
        let mut reader = bytes;
        self.backend
            .put(&mut reader, &sniff(bytes).mime)
            .map(|stored| stored.hash)
    }

    /// Keep `bytes` as the content `reference` names: sealed under its key when it carries one, so
    /// a device that holds the bytes keeps exactly what the writer stored. Refused when `bytes`
    /// are not that content.
    ///
    /// # Errors
    /// [`ContentError::HashMismatch`] for other bytes, else see [`ContentBackend::put`].
    pub fn put_as(&self, reference: &str, bytes: &[u8]) -> Result<(), ContentError> {
        let parsed = ContentRef::parse(reference)
            .ok_or_else(|| ContentError::InvalidHash(reference.to_owned()))?;
        let stored = match parsed.key() {
            None => self.put_public(bytes)?,
            Some(key) => {
                let sealed = tilo_seal::content::seal(key, bytes)
                    .map_err(|error| ContentError::Io(error.to_string()))?;
                let mut reader = sealed.as_slice();
                self.backend.put(&mut reader, SEALED_MIME)?.hash
            }
        };
        if stored == parsed.address() {
            Ok(())
        } else {
            Err(ContentError::HashMismatch)
        }
    }

    /// Store everything `reader` yields without holding it in memory: the device store sends it
    /// to the daemon one `runtime_fs::blobs::MAX_PUT_CHUNK` at a time. [`Stored::hash`] is the
    /// reference to keep, sealed when this store seals.
    ///
    /// # Errors
    /// See [`ContentBackend::put`].
    pub fn put_reader(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        if !self.seals {
            return self.backend.put(reader, mime);
        }
        let key = tilo_seal::SymmetricKey::generate()
            .map_err(|error| ContentError::Io(error.to_string()))?;
        let mut sealed = tilo_seal::content::SealReader::new(&key, reader)
            .map_err(|error| ContentError::Io(error.to_string()))?;
        let stored = self.backend.put(&mut sealed, SEALED_MIME)?;
        Ok(Stored {
            hash: ContentRef::sealed(&stored.hash, key).to_string(),
            len: stored.len,
        })
    }

    /// Store every input `inputs` collected (an import's images, say), answering the reference to
    /// keep for each input's address: the address itself, or a sealed reference when this store
    /// seals, which the document naming the input names instead.
    ///
    /// # Errors
    /// The first refusal; inputs stored before it stay stored.
    pub fn put_inputs(
        &self,
        inputs: &ContentInputs,
    ) -> Result<BTreeMap<String, String>, ContentError> {
        let mut references = BTreeMap::new();
        for (hash, bytes) in inputs.iter() {
            let reference = self.put(bytes)?;
            if content_address(&reference).as_deref() != Some(hash) && !self.seals {
                return Err(ContentError::HashMismatch);
            }
            references.insert(hash.to_owned(), reference);
        }
        Ok(references)
    }

    /// Store again, sealed, the plain content `reference` names, when this store seals and it is
    /// here: the sealed reference a record keeps in its place. `None` for a reference already
    /// sealed, a store that does not seal, or content not here yet. The plain copy stays until
    /// nothing references it ([`Self::collect_unreferenced`]), so a reader still holding the old
    /// reference reads it meanwhile.
    pub fn reseal(&self, reference: &str) -> Option<String> {
        if !self.seals || is_sealed(reference) {
            return None;
        }
        let bytes = self.read(reference)?;
        self.put(&bytes)
            .inspect_err(|error| tracing::warn!(%error, "content could not be sealed"))
            .ok()
    }

    /// The verified content `reference` names, opened when sealed, or `None` when it is not here
    /// (or not intact).
    pub fn read(&self, reference: &str) -> Option<Vec<u8>> {
        let mut reader = self.open(reference)?;
        let mut bytes = Vec::new();
        match reader.read_to_end(&mut bytes) {
            Ok(_) => Some(bytes),
            Err(error) => {
                tracing::warn!(%error, "content store: unreadable content");
                None
            }
        }
    }

    /// A streaming, verifying read of the content `reference` names, opened as it streams when
    /// sealed; see [`ContentReader`].
    pub fn open(&self, reference: &str) -> Option<ContentReader> {
        let parsed = ContentRef::parse(reference);
        let address = parsed.as_ref().map_or(reference, ContentRef::address);
        let reader = self.backend.open(address).ok().flatten()?;
        match parsed.as_ref().and_then(ContentRef::key) {
            None => Some(reader),
            Some(key) => tilo_seal::content::OpenReader::new(key, reader)
                .ok()
                .map(|reader| Box::new(reader) as ContentReader),
        }
    }

    /// Whether the content `reference` names is here.
    pub fn has(&self, reference: &str) -> bool {
        self.backend.has(&address_of(reference))
    }

    /// Where the content `reference` names has got to.
    pub fn state(&self, reference: &str) -> ContentState {
        self.backend.state(&address_of(reference))
    }

    /// Whether content stored here reaches the app's other devices and collaborators; see
    /// [`ContentBackend::shared`].
    pub fn shared(&self) -> bool {
        self.backend.shared()
    }

    /// Whether this store answers only for content readable in one workspace;
    /// see [`ContentBackend::workspace_scoped`].
    pub fn workspace_scoped(&self) -> bool {
        self.backend.workspace_scoped()
    }

    /// This store, declared as one workspace's ([`Self::workspace_scoped`]): for a host that
    /// keeps a store for one workspace alone, so everything in it is that workspace's (a
    /// test's profile, one workspace's headless drive).
    #[must_use]
    pub fn for_one_workspace(self) -> Self {
        Self {
            backend: Arc::new(OneWorkspace(self.backend)),
            seals: self.seals,
        }
    }

    /// Content in memory, as one workspace's store
    /// ([`Self::for_one_workspace`]): for tests of what a
    /// caller may name by reference.
    pub fn memory_in_workspace() -> Self {
        Self::memory().for_one_workspace()
    }

    /// Remove every item not named in `live`, references or addresses; see
    /// [`ContentBackend::collect_unreferenced`].
    ///
    /// # Errors
    /// See [`ContentBackend::collect_unreferenced`].
    pub fn collect_unreferenced(&self, live: &BTreeSet<String>) -> Result<Collected, ContentError> {
        let addresses = live.iter().map(|reference| address_of(reference)).collect();
        self.backend.collect_unreferenced(&addresses)
    }

    /// Remove the item `reference` names ([`ContentBackend::release`]).
    ///
    /// # Errors
    /// As [`ContentBackend::release`].
    pub fn release(&self, reference: &str) -> Result<Collected, ContentError> {
        self.backend.release(&address_of(reference))
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

/// A reader that hashes what passes through and fails at the end if it was not `expected`: what
/// a backend wraps a read from storage it does not trust in, so it returns a [`ContentReader`].
#[cfg(feature = "dir")]
pub struct Verifying<R> {
    inner: R,
    hasher: blake3::Hasher,
    expected: String,
    done: bool,
}

#[cfg(feature = "dir")]
impl<R> Verifying<R> {
    /// Verify what `inner` yields against the address `expected`.
    pub fn new(inner: R, expected: &str) -> Self {
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

#[cfg(test)]
#[cfg(feature = "dir")]
mod tests;
