//! `tilo-content` — binary content by address.
//!
//! Apps keep their data as versioned resources: a log of the actions performed on them, replayed
//! into state. That is the default. Some bytes are not a resource, though: the photograph a photo
//! library was imported from, an image placed in a document, a mail attachment. Those are
//! *binary inputs*. A resource refers to one by its content hash, and the bytes live here, once,
//! however many documents and versions refer to them.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};
#[cfg(feature = "dir")]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "dir")]
mod dir;
#[cfg(feature = "dir")]
mod files;
mod list;
mod memory;
mod reference;
#[cfg(feature = "dir")]
mod workspaces;

#[cfg(feature = "dir")]
pub use dir::DirBackend;
#[cfg(feature = "dir")]
pub use files::FilesBackend;
pub use list::PART_BYTES;
pub use memory::MemoryBackend;
pub use reference::{ContentRef, content_address, is_sealed, valid_reference};
pub use tilo_content_types::{
    BLOB_SCHEME, ContentMeta, DocumentFormat, FACTS_SCAN_BYTES, ImageFormat, MediaFacts, RECORDING,
    RECORDING_MAGIC, document_format, facts, has_facts, image_format, image_size, opened_as,
    opened_by, sniff, valid_hash,
};
#[cfg(feature = "dir")]
pub use workspaces::WorkspaceStores;

#[cfg(feature = "dir")]
const PAGE_STORE_OWNER: &str = "so.tilo.content";
/// A page's largest content item: the largest a device keeps of a record's
/// content, which a page holds whole while it stores it.
#[cfg(feature = "dir")]
const PAGE_MAX_ITEM_BYTES: usize = 64 * 1024 * 1024;
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

/// The hash the members' items are keyed by: BLAKE3, keyed by a key derived from the content
/// secret ([`ContentBackend::content_secret`]).
fn convergent_hasher(secret: &tilo_seal::SymmetricKey) -> blake3::Hasher {
    blake3::Hasher::new_keyed(&blake3::derive_key(
        "tilo record content key v1",
        secret.expose_secret(),
    ))
}

/// The key `bytes` are sealed under by whoever holds the members' content `secret`: the same
/// bytes take the same key, so they are one item under one reference however often they are kept
/// ([`ContentBackend::content_secret`]).
#[must_use]
pub fn part_key(secret: &tilo_seal::SymmetricKey, bytes: &[u8]) -> tilo_seal::SymmetricKey {
    let mut hasher = convergent_hasher(secret);
    hasher.update(bytes);
    tilo_seal::SymmetricKey::from_bytes(*hasher.finalize().as_bytes())
}

const KEEP_PLAINTEXT: usize = 1024 * 1024;

/// A file a sealing write keeps the plaintext it hashes in once it no longer fits memory: one
/// the store makes beside its own content ([`ContentBackend::spill`]), removed when it is
/// dropped.
pub trait SpillFile: Read + Write + Seek + Send {}

impl<T: Read + Write + Seek + Send> SpillFile for T {}

/// The plaintext a sealing store hashes before it seals it: in memory while it is small, and
/// past [`KEEP_PLAINTEXT`] bytes in the store's spill file, so a large item costs disk while it
/// is sealed rather than the memory to hold it whole. A store with no directory of its own
/// ([`ContentBackend::spill`]) keeps all of it in memory.
struct Gathered {
    kept: Vec<u8>,
    spill: Option<Box<dyn SpillFile>>,
    /// Whether the store was asked for a spill file and had none.
    unspillable: bool,
}

impl Gathered {
    const fn new() -> Self {
        Self {
            kept: Vec::new(),
            spill: None,
            unspillable: false,
        }
    }

    /// Keep `chunk`, moving what is kept so far and the rest to `backend`'s spill file once the
    /// plaintext no longer fits memory.
    fn keep(&mut self, chunk: &[u8], backend: &dyn ContentBackend) -> Result<(), ContentError> {
        if self.spill.is_none()
            && !self.unspillable
            && self.kept.len().saturating_add(chunk.len()) > KEEP_PLAINTEXT
        {
            match backend.spill()? {
                Some(mut file) => {
                    file.write_all(&self.kept)?;
                    self.spill = Some(file);
                    self.kept = Vec::new();
                }
                None => self.unspillable = true,
            }
        }
        if let Some(spill) = &mut self.spill {
            spill.write_all(chunk)?;
            return Ok(());
        }
        self.kept.extend_from_slice(chunk);
        Ok(())
    }

    fn source(&mut self) -> Result<Box<dyn Read + '_>, ContentError> {
        if let Some(spill) = &mut self.spill {
            spill.rewind()?;
            return Ok(Box::new(spill));
        }
        Ok(Box::new(self.kept.as_slice()))
    }
}

/// A reader whose first read fails as reading the content did.
struct Failed(Option<std::io::Error>);

impl Read for Failed {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        self.0.take().map_or(Ok(0), Err)
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
    Resolved,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    pub hash: String,
    pub len: u64,
}

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

    /// Store many items, answering each address in order: what [`Self::put`]
    /// does for each, done together where a backend can (one index update, one
    /// flush), so a folder of small files costs its bytes rather than its
    /// calls. A failing item leaves the others' answers in place.
    fn put_many(&self, items: &[(&[u8], &str)]) -> Vec<Result<Stored, ContentError>> {
        items
            .iter()
            .map(|(bytes, mime)| self.put(&mut &bytes[..], mime))
            .collect()
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError>;

    /// The content at `hash` read from `offset` on: a backend that keeps it in a file seeks
    /// there, so serving one piece of a large item reads only that piece. Bytes read so are not
    /// checked against the address (that takes the whole item); sealed content is still
    /// checked chunk by chunk as it opens.
    ///
    /// # Errors
    /// What [`Self::open`] answers.
    fn open_at(&self, hash: &str, offset: u64) -> Result<Option<ContentReader>, ContentError> {
        let Some(mut reader) = self.open(hash)? else {
            return Ok(None);
        };
        std::io::copy(&mut reader.by_ref().take(offset), &mut std::io::sink())?;
        Ok(Some(reader))
    }

    fn has(&self, hash: &str) -> bool;

    /// The length of the content at `hash`, when the store knows it without
    /// reading the bytes: what a reader serving one piece of a large file
    /// asks before it seeks there.
    fn size(&self, hash: &str) -> Option<u64> {
        let _ = hash;
        None
    }

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
    /// [`ContentError::CollectionUnsupported`] for backends whose retention another store
    /// decides (a shared device store), or an I/O failure part-way (retry with the same live set).
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
    /// [`ContentError::CollectionUnsupported`] for backends whose retention another store
    /// decides, or an I/O failure.
    fn release(&self, hash: &str) -> Result<Collected, ContentError> {
        let _ = hash;
        Err(ContentError::CollectionUnsupported)
    }

    /// Whether content stored here reaches beyond this device's own copy: the device content
    /// store serves it to the app's other devices and members. A document may then refer
    /// to content by address alone; with a store that is not shared, a member would be left
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

    /// The secret the members who read what this store keeps hold, when it is at hand: a
    /// sealing store ([`ContentStore::sealing`]) then seals each item under a key derived from
    /// it and the item's bytes, rather than a fresh one, so the same bytes stored twice are the
    /// same ciphertext under the same reference, kept and carried once.
    ///
    /// Whoever holds the secret learns which items are the same bytes, and can tell whether a
    /// given item is among them; the members can tell that anyway, by opening both. Whoever does
    /// not hold it (the hosts that keep and carry the ciphertext, a device removed before the
    /// secret was derived) learns nothing: without the secret no key, and no address, follows
    /// from any bytes. The key never leaves the reference, which travels inside the sealed
    /// record, so a reader who joins later opens the item as before, and a reference sealed
    /// under a fresh key opens as it always did.
    fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        None
    }

    /// Seal and keep what `reader` yields, when the store behind this backend seals what it is
    /// handed itself, under the members' secret it holds and this backend does not (a session's
    /// store, whose device's daemon holds the workspace's keys): the sealed reference it answers
    /// is what a sealing store hands back. `None`, reading nothing, when the backend keeps only
    /// what is sealed before it reaches it.
    fn put_sealed(
        &self,
        reader: &mut dyn Read,
        mime: &str,
    ) -> Option<Result<Stored, ContentError>> {
        let _ = (reader, mime);
        None
    }

    /// The largest item this store accepts: a write that hashes before it seals refuses a larger
    /// one as it reads it, rather than holding it whole first ([`ContentStore::put_reader`]).
    fn max_item_bytes(&self) -> usize {
        ContentLimits::default().max_item_bytes
    }

    /// A new file beside the content this store keeps, where a sealing write keeps the plaintext
    /// it hashes rather than in memory, removed when it is dropped. `None` for a store with no
    /// directory of its own: one that keeps content on a host, or holds it in memory.
    ///
    /// # Errors
    /// The file could not be made.
    fn spill(&self) -> std::io::Result<Option<Box<dyn SpillFile>>> {
        Ok(None)
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

    fn open_at(&self, hash: &str, offset: u64) -> Result<Option<ContentReader>, ContentError> {
        self.0.open_at(hash, offset)
    }

    fn has(&self, hash: &str) -> bool {
        self.0.has(hash)
    }

    fn size(&self, hash: &str) -> Option<u64> {
        self.0.size(hash)
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

    fn content_secret(&self) -> Option<tilo_seal::SymmetricKey> {
        self.0.content_secret()
    }

    fn max_item_bytes(&self) -> usize {
        self.0.max_item_bytes()
    }

    fn spill(&self) -> std::io::Result<Option<Box<dyn SpillFile>>> {
        self.0.spill()
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
    pub fn new(backend: impl ContentBackend + 'static) -> Self {
        Self::from_backend(Arc::new(backend))
    }

    pub fn from_backend(backend: Arc<dyn ContentBackend>) -> Self {
        Self {
            backend,
            seals: false,
        }
    }

    /// This store, sealing what it stores: each item under a key of its own, kept under the
    /// address of its ciphertext, and named by a reference that carries the key. The key derives
    /// from the members' content secret and the bytes when the backend holds one
    /// ([`ContentBackend::content_secret`]), and is fresh otherwise. The store a record's content
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

    pub fn memory() -> Self {
        Self::new(MemoryBackend::default())
    }

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

    /// Whether this store seals what it keeps, so its writes go one at a time.
    #[must_use]
    pub const fn seals(&self) -> bool {
        self.seals
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

    /// Keep many items, answering each one's address in order: [`Self::put_public`]
    /// for each, one store write where the backend can make it.
    ///
    /// # Errors
    /// Each item its own, as [`Self::put_public`] refuses it.
    pub fn put_public_many(&self, items: &[&[u8]]) -> Vec<Result<String, ContentError>> {
        let mimes: Vec<String> = items.iter().map(|bytes| sniff(bytes).mime).collect();
        let with_mime: Vec<(&[u8], &str)> = items
            .iter()
            .zip(&mimes)
            .map(|(bytes, mime)| (*bytes, mime.as_str()))
            .collect();
        self.backend
            .put_many(&with_mime)
            .into_iter()
            .map(|stored| stored.map(|stored| stored.hash))
            .collect()
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
    /// A sealing store with the members' secret hashes the plaintext in a first pass and seals it
    /// in a second, and refuses it once it is past [`ContentBackend::max_item_bytes`]: a large
    /// item waits in a file beside the store's own content
    /// ([`ContentBackend::scratch_dir`]) rather than whole in memory, and an oversized one is
    /// refused without being held at all.
    ///
    /// # Errors
    /// See [`ContentBackend::put`], and [`ContentError::ItemTooLarge`] for an item past the
    /// store's limit.
    pub fn put_reader(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        if !self.seals {
            return self.backend.put(reader, mime);
        }
        if let Some(stored) = self.backend.put_sealed(reader, mime) {
            return stored;
        }
        // With the members' secret the key is the bytes' keyed hash, so they are read once to
        // hash them, kept in memory while they are small and in a file beside the store's own
        // content past that, then read again to seal them. Such a key seals the one plaintext it
        // was derived from, as a fresh one does, so the nonces sealing derives from a chunk's
        // position never repeat under it.
        let mut gathered = None;
        let key = match self.backend.content_secret() {
            Some(secret) => {
                let mut hasher = convergent_hasher(&secret);
                gathered = Some(self.gather(reader, &mut hasher)?);
                tilo_seal::SymmetricKey::from_bytes(*hasher.finalize().as_bytes())
            }
            None => tilo_seal::SymmetricKey::generate()
                .map_err(|error| ContentError::Io(error.to_string()))?,
        };
        let source: Box<dyn Read + '_> = match &mut gathered {
            Some(gathered) => gathered.source()?,
            None => Box::new(reader),
        };
        let mut sealed = tilo_seal::content::SealReader::new(&key, source)
            .map_err(|error| ContentError::Io(error.to_string()))?;
        let stored = self.backend.put(&mut sealed, SEALED_MIME)?;
        Ok(Stored {
            hash: ContentRef::sealed(&stored.hash, key).to_string(),
            len: stored.len,
        })
    }

    /// Read `reader` to its end, hashing what it yields into `hasher`, and refuse it the moment
    /// it is past what the store accepts: what it yields waits in memory while it is small, and
    /// past that in a file beside the store's own content.
    fn gather(
        &self,
        reader: &mut dyn Read,
        hasher: &mut blake3::Hasher,
    ) -> Result<Gathered, ContentError> {
        let limit = self.backend.max_item_bytes();
        let mut gathered = Gathered::new();
        let mut buf = vec![0_u8; 64 * 1024];
        let mut read = 0_usize;
        loop {
            // Never ask for more than the byte past the limit that refuses the item.
            let want = limit.saturating_add(1).saturating_sub(read).min(buf.len());
            let n = match reader.read(buf.get_mut(..want).unwrap_or_default()) {
                Ok(0) => return Ok(gathered),
                Ok(n) => n.min(want),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            read = read.saturating_add(n);
            if read > limit {
                return Err(ContentError::ItemTooLarge {
                    actual: read,
                    limit,
                });
            }
            let chunk = buf.get(..n).unwrap_or_default();
            hasher.update(chunk);
            gathered.keep(chunk, &*self.backend)?;
        }
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
    /// sealed; see [`ContentReader`]. Content kept in parts ([`Self::put_parts`]) reads as the
    /// content its parts make, and is `None` until every part is here: each part not here is
    /// asked for at once.
    pub fn open(&self, reference: &str) -> Option<ContentReader> {
        let mut item = self.open_item(reference)?;
        match list::List::read(&mut item) {
            Ok(Ok(list)) => {
                // Every missing part is asked for, not only the first.
                let mut missing = false;
                for (_, part) in &list.parts {
                    if !self.has(part) {
                        // Opening what is not here asks for it.
                        let _ = self.backend.open(&address_of(part));
                        missing = true;
                    }
                }
                if missing {
                    return None;
                }
                let mut left: Vec<String> = list.parts.into_iter().map(|(_, part)| part).collect();
                left.reverse();
                Some(Box::new(list::Parts {
                    store: self.clone(),
                    left,
                    current: None,
                }))
            }
            Ok(Err(read)) => Some(Box::new(std::io::Cursor::new(read).chain(item))),
            // What failed reading the item's start (a check that does not
            // hold on a short item) fails the reader, as it would have read on.
            Err(error) => Some(Box::new(Failed(Some(error)))),
        }
    }

    /// The one item `reference` names, opened when sealed: a list of parts as the list.
    fn open_item(&self, reference: &str) -> Option<ContentReader> {
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

    /// The one item `reference` names, read whole: a list of parts as the list.
    #[cfg(test)]
    fn read_item_whole(&self, reference: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Some(mut item) = self.open_item(reference) {
            let _ = item.read_to_end(&mut bytes);
        }
        bytes
    }

    /// The parts `reference` is kept in, when it is a list of them.
    fn list(&self, reference: &str) -> Option<list::List> {
        let mut item = self.open_item(reference)?;
        list::List::read(&mut item).ok()?.ok()
    }

    /// Store everything `reader` yields a part at a time ([`list::PART_BYTES`]), never holding
    /// more than one part: content of any size, kept as items no larger than a part. Content
    /// that fits in one part is that one item; larger content is its parts and a list naming
    /// them, whose reference reads as the whole ([`Self::open`]). Each is sealed when this store
    /// seals.
    ///
    /// # Errors
    /// What storing a part or the list refused, or reading `reader` failed with.
    pub fn put_parts(&self, reader: &mut dyn Read) -> Result<Stored, ContentError> {
        let mut parts = Vec::new();
        let mut part = Vec::with_capacity(list::PART_BYTES);
        loop {
            part.clear();
            (&mut *reader)
                .take(list::PART_BYTES as u64)
                .read_to_end(&mut part)?;
            if part.is_empty() && !parts.is_empty() {
                break;
            }
            let len = part.len() as u64;
            parts.push((len, self.put(&part)?));
            if part.len() < list::PART_BYTES {
                break;
            }
        }
        if let [(len, only)] = parts.as_slice() {
            return Ok(Stored {
                hash: only.clone(),
                len: *len,
            });
        }
        let list = list::List { parts };
        let len = list.total();
        Ok(Stored {
            hash: self.put(&list.encode())?,
            len,
        })
    }

    /// The length of the content `reference` names, when the store knows it
    /// without reading it: for sealed content, the plaintext its sealed
    /// bytes hold; for content kept in parts, what its parts make.
    pub fn size(&self, reference: &str) -> Option<u64> {
        if let Some(list) = self.list(reference) {
            return Some(list.total());
        }
        self.item_size(reference)
    }

    fn item_size(&self, reference: &str) -> Option<u64> {
        let parsed = ContentRef::parse(reference);
        let address = parsed.as_ref().map_or(reference, ContentRef::address);
        let stored = self.backend.size(address)?;
        match parsed.as_ref().and_then(ContentRef::key) {
            None => Some(stored),
            Some(_) => tilo_seal::content::plain_len(stored),
        }
    }

    /// `length` bytes of the content `reference` names, from `offset`: the
    /// bytes read, and the content's length. Reads only what it answers, so
    /// serving one piece of a large file does not read the file: sealed
    /// content opens from the chunk the piece begins in, and content kept in
    /// parts reads only the parts the piece is in.
    ///
    /// # Errors
    /// The content is not here, or cannot be read.
    pub fn read_range(
        &self,
        reference: &str,
        offset: u64,
        length: u64,
    ) -> Result<(u64, Vec<u8>), ContentError> {
        let Some(list) = self.list(reference) else {
            return self.read_item_range(reference, offset, length);
        };
        let size = list.total();
        let end = offset.saturating_add(length).min(size);
        let mut bytes = Vec::new();
        let mut start = 0_u64;
        for (len, part) in &list.parts {
            let stop = start.saturating_add(*len);
            if stop > offset && start < end {
                let from = offset.max(start).saturating_sub(start);
                let to = end.min(stop).saturating_sub(start);
                let (_, piece) = self.read_item_range(part, from, to.saturating_sub(from))?;
                bytes.extend_from_slice(&piece);
            }
            start = stop;
        }
        Ok((size, bytes))
    }

    fn read_item_range(
        &self,
        reference: &str,
        offset: u64,
        length: u64,
    ) -> Result<(u64, Vec<u8>), ContentError> {
        let missing = || ContentError::InvalidHash(reference.to_owned());
        let size = self.item_size(reference).ok_or_else(missing)?;
        let offset = offset.min(size);
        let length = length.min(size.saturating_sub(offset));
        let parsed = ContentRef::parse(reference);
        let address = parsed.as_ref().map_or(reference, ContentRef::address);
        let (mut reader, skip): (ContentReader, u64) =
            match parsed.as_ref().and_then(ContentRef::key) {
                None => (
                    self.backend.open_at(address, offset)?.ok_or_else(missing)?,
                    0,
                ),
                Some(key) => {
                    let chunk = tilo_seal::content::CHUNK as u64;
                    let index = offset.checked_div(chunk).unwrap_or(0);
                    let sealed = self
                        .backend
                        .open_at(address, tilo_seal::content::sealed_offset(index))?
                        .ok_or_else(missing)?;
                    let opened = tilo_seal::content::OpenReader::at_chunk(key, sealed, index)
                        .map_err(|error| ContentError::Io(error.to_string()))?;
                    (
                        Box::new(opened),
                        offset.saturating_sub(index.saturating_mul(chunk)),
                    )
                }
            };
        let unreadable = |error: std::io::Error| ContentError::Io(error.to_string());
        std::io::copy(&mut reader.by_ref().take(skip), &mut std::io::sink()).map_err(unreadable)?;
        let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
        reader
            .take(length)
            .read_to_end(&mut bytes)
            .map_err(unreadable)?;
        if (bytes.len() as u64) < length {
            return Err(ContentError::Io(format!(
                "{reference} ended {} bytes short of the piece asked for",
                length.saturating_sub(bytes.len() as u64)
            )));
        }
        Ok((size, bytes))
    }

    pub fn has(&self, reference: &str) -> bool {
        self.backend.has(&address_of(reference))
    }

    pub fn state(&self, reference: &str) -> ContentState {
        self.backend.state(&address_of(reference))
    }

    /// Whether content stored here reaches the app's other devices and members; see
    /// [`ContentBackend::shared`].
    pub fn shared(&self) -> bool {
        self.backend.shared()
    }

    /// Whether this store answers only for content readable in one workspace;
    /// see [`ContentBackend::workspace_scoped`].
    pub fn workspace_scoped(&self) -> bool {
        self.backend.workspace_scoped()
    }

    /// A new file beside the content this store keeps, where a sealing write keeps the plaintext
    /// it hashes; see [`ContentBackend::spill`].
    ///
    /// # Errors
    /// The file could not be made.
    pub fn spill(&self) -> std::io::Result<Option<Box<dyn SpillFile>>> {
        self.backend.spill()
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

impl<A: ContentSource, B: ContentSource> ContentSource for (A, B) {
    fn content(&self, hash: &str) -> Option<Cow<'_, [u8]>> {
        self.0.content(hash).or_else(|| self.1.content(hash))
    }
}

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

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.entries
            .iter()
            .map(|(hash, bytes)| (hash.as_str(), bytes.as_slice()))
    }

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
