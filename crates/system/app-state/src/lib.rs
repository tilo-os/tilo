#![allow(
    clippy::collapsible_if,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Persistence for a Tilo app's Automerge documents, through one seam.
//!
//! Every document lives behind a [`durable_store::DurableStore`] key.
//! [`FsStore`] is the native implementation: one directory per document
//! (`<data home>/<package-id>/state.automerge` for a package's own document,
//! below it for rooms and records), atomic replacement, synced appends, an
//! actor sidecar for restart continuity and advisory-lock gates for single or
//! shared ownership. Identifiers are validated so an app can't address
//! another app's storage or the shared workspace FUSE root.
//!
//! [`DocOwner`] leases one key and decodes it; [`DocWriter`] persists the
//! live document without blocking, appending `save_after(sent_heads)`
//! (linear write volume, not quadratic) until compaction pays off, and
//! reporting what is actually durable. [`DocStore`] adds the load/save
//! conveniences and schema policy apps drive by hand, on the same owner.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use advisory_lock::{FileLock, LockMode, TryLockError};
use durable_store::{DurableStore, StorageKey, WriteOp};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, LoadOptions, OnPartialLoad, ROOT, ReadDoc};

mod doc_writer;
mod fs_store;
pub use doc_writer::{DEFAULT_DEBOUNCE, DocOwner, DocWriter, DurabilitySnapshot, StoredLoad};
pub use fs_store::{FsStore, INSTANCE_VIEWS_DIR};

pub const DOCUMENT_FILE: &str = "state.automerge";
pub const ACTOR_FILE: &str = "actor";
pub const CANONICAL_GATE: &str = "canonical.lock";
pub const LOCK_FILE: &str = "writer.lock";
/// Subdirectory holding one directory per record. See [`DocStore::open_room_record`].
pub const RECORDS_DIR: &str = "records";
/// Subdirectory holding one directory per **room** — the index of records
/// that room holds. See [`DocStore::open_room`].
pub const ROOMS_DIR: &str = "rooms";
/// Marker in a room record's directory naming the one record that adopted it
/// under a new identity.
const RECORD_SUCCESSOR_CLAIM: &str = "successor-claim";

/// Root-map key for the document's layout version. A document from before
/// this key existed reads as [`SCHEMA_VERSION`] rather than "unknown", so the
/// upgrade that added the check doesn't strand every existing document.
pub const SCHEMA_VERSION_KEY: &str = "tilo:schema_version";

/// The document layout this build writes and understands. Bump only when a
/// change makes a document unreadable to an older build in a way that loses
/// meaning — adding a key does not qualify, since an older build just
/// ignores it. Older-reading-newer has no safe default; this crate used to
/// start empty and overwrite, turning version skew into data loss. See
/// [`DocStore::load`].
pub const SCHEMA_VERSION: u32 = 1;

/// Everything that can go wrong opening, reading or writing a store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("another writer owns {path}")]
    WriterBusy { path: PathBuf },
    #[error("existing document is empty: {path}")]
    EmptyExistingDocument { path: PathBuf },
    #[error("storage source changed: {path}")]
    SourceChanged { path: PathBuf },
    #[error("invalid schema in {path}: {reason}")]
    InvalidSchema { path: PathBuf, reason: &'static str },
    #[error("invalid package id {0:?}: {1}")]
    PackageId(String, &'static str),
    /// The record key could not be turned into a directory name (distinct from
    /// [`StoreError::PackageId`] so the message names which input was wrong).
    #[error("invalid record key {0:?}: {1}")]
    RecordKey(String, &'static str),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    /// The stored bytes are not a loadable Automerge document. Boxed: unlike
    /// this crate's other variants, `AutomergeError` alone is well over 100
    /// bytes (nested enums carrying `PatchAction`, `String`s, etc.), which
    /// would make every `Result<_, StoreError>` in this crate's public API
    /// large regardless of which variant is actually returned.
    #[error("reading {path}: {source}")]
    Automerge {
        path: PathBuf,
        #[source]
        source: Box<automerge::AutomergeError>,
    },
    /// The document was written by a newer build than this one. Kept separate
    /// from [`StoreError::Automerge`] because the data is intact and only this
    /// build is wrong for it — saying "unreadable" would lose that distinction.
    #[error(
        "{path} has layout {found}, and this build understands {understood}. \
         It was written by a newer version of this app; nothing has been \
         changed."
    )]
    NewerSchema {
        path: PathBuf,
        found: u32,
        understood: u32,
    },
    /// An app-owned migration declined an older, otherwise readable layout.
    #[error("migrating {path} from layout {from} to {to}: {message}")]
    Migration {
        path: PathBuf,
        from: u32,
        to: u32,
        message: String,
    },
}

fn io_err(context: impl Into<String>, source: io::Error) -> StoreError {
    StoreError::Io {
        context: context.into(),
        source,
    }
}

/// The layout version a document declares. Absent, or present but not an
/// integer, both read as [`SCHEMA_VERSION`] (see [`SCHEMA_VERSION_KEY`]) —
/// this crate is the only writer of the key, so a non-integer is someone
/// else's key of the same name, not corruption.
pub fn schema_version_of(doc: &AutoCommit) -> u32 {
    doc.get(ROOT, SCHEMA_VERSION_KEY)
        .ok()
        .flatten()
        .and_then(|(v, _)| {
            v.as_u64()
                .or_else(|| v.as_i64().and_then(|i| u64::try_from(i).ok()))
                .and_then(|n| u32::try_from(n).ok())
        })
        .unwrap_or(SCHEMA_VERSION)
}

fn stamp_schema_version(doc: &mut AutoCommit, version: u32) {
    if let Err(e) = doc.put(ROOT, SCHEMA_VERSION_KEY, u64::from(version)) {
        tracing::warn!(error = %e, "could not stamp the document's schema version");
    }
}

/// Stamp the version only if the document doesn't already carry one, so a
/// pre-versioning document gets it on first save rather than staying unlabelled.
fn ensure_schema_version(doc: &mut AutoCommit, version: u32) {
    if matches!(doc.get(ROOT, SCHEMA_VERSION_KEY), Ok(None) | Err(_)) {
        stamp_schema_version(doc, version);
    }
}

/// Validate a package id as a single directory name — the containment
/// boundary. `TILO_PACKAGE_ID` is treated as untrusted (an app can set its
/// own environment), so anything that could escape its own directory is
/// rejected rather than sanitised.
fn validate_package_id(id: &str) -> Result<(), StoreError> {
    if id.is_empty() {
        return Err(StoreError::PackageId(id.to_string(), "must not be empty"));
    }
    if Path::new(id).is_absolute() {
        return Err(StoreError::PackageId(
            id.to_string(),
            "must not be an absolute path",
        ));
    }
    if id.contains('/') || id.contains('\\') {
        return Err(StoreError::PackageId(
            id.to_string(),
            "must not contain a path separator",
        ));
    }
    if id == "." || id == ".." {
        return Err(StoreError::PackageId(
            id.to_string(),
            "must not be a relative directory reference",
        ));
    }
    if id.contains('\0') {
        return Err(StoreError::PackageId(
            id.to_string(),
            "must not contain a NUL byte",
        ));
    }
    Ok(())
}

/// Validate a record key as a single directory name — same boundary as
/// [`validate_package_id`], but stricter matters here since a record's
/// identity can be a file path where `..` could enable traversal. Rejected
/// rather than sanitised, since silently rewriting a bad key could make two
/// records share one file.
fn validate_record_key(key: &str) -> Result<(), StoreError> {
    if key.is_empty() {
        return Err(StoreError::RecordKey(key.to_string(), "must not be empty"));
    }
    if Path::new(key).is_absolute() || key.contains('/') || key.contains('\\') {
        return Err(StoreError::RecordKey(
            key.to_string(),
            "must not contain a path separator",
        ));
    }
    if key == "." || key == ".." {
        return Err(StoreError::RecordKey(
            key.to_string(),
            "must not be a relative directory reference",
        ));
    }
    if key.contains('\0') {
        return Err(StoreError::RecordKey(
            key.to_string(),
            "must not contain a NUL byte",
        ));
    }
    Ok(())
}

/// Validate the identifiers of a room-scoped record without resolving any
/// path, so a caller can refuse a bad namespace before choosing a store.
///
/// # Errors
/// Rejects any identifier that is not a single, non-traversing name.
pub fn validate_record_identity(package: &str, room: &str, record: &str) -> Result<(), StoreError> {
    validate_package_id(package)?;
    validate_record_key(room)?;
    validate_record_key(record)
}

/// One Automerge document handled by hand: load it, save or append it, and
/// hand it to a [`DocWriter`] for debounced background writes. A thin policy
/// layer over a [`DocOwner`] on a [`DurableStore`]; it owns no file
/// handling of its own.
///
/// The launch-environment constructors ([`DocStore::open`],
/// [`DocStore::open_for`], [`DocStore::open_room`], ...) join the document's
/// shared writers on an [`FsStore`] rooted at the native data directory, so
/// two live instances of one app keep appending to one document, as they
/// always have. [`DocStore::open_canonical_at`] takes the exclusive owner.
pub struct DocStore {
    package_id: String,
    owner: Rc<DocOwner>,
    /// App/document-specific layout understood by this caller. Constructors
    /// default to the toolkit's legacy v1 contract; apps opt into a newer
    /// schema before loading via [`Self::with_schema_version`].
    schema_version: u32,
    /// Heads the store is known to hold; empty until the first load or save.
    persisted: Vec<ChangeHash>,
    /// Whether the store holds any bytes yet; learned on first need.
    has_base: Option<bool>,
    /// Why [`DocStore::load_or_new`] handed back an empty document instead of
    /// the stored one — set only for a refusal that must not be written over;
    /// e.g. [`DocStore::writer`] hands back a disabled writer when this is set.
    refusal: Option<Refusal>,
    directory: PathBuf,
}

/// The stored document, and why this build would not open it.
#[derive(Clone, Debug)]
struct Refusal {
    found: u32,
    message: String,
}

impl DocStore {
    /// Open the store belonging to the running package, via `TILO_PACKAGE_ID` —
    /// no argument by design, since an app doesn't choose where its state lives.
    ///
    /// # Errors
    ///
    /// Returns an error if `TILO_PACKAGE_ID` is not a valid single directory
    /// name (see `validate_package_id`), or if the package's state
    /// directory can't be created, inspected or locked.
    pub fn open() -> Result<Self, StoreError> {
        Self::open_for(&cli::package_id())
    }

    /// Open the store, falling back to `manifest_id` when `TILO_PACKAGE_ID` is
    /// unset. The env var always wins; the fallback only covers launcher-less
    /// runs (dev binary, tests) that would otherwise share one directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the resolved id is not a valid single directory
    /// name (see `validate_package_id`), or if the package's state
    /// directory can't be created, inspected or locked.
    pub fn open_with_manifest_id(manifest_id: &str) -> Result<Self, StoreError> {
        let id = std::env::var("TILO_PACKAGE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| manifest_id.to_string());
        Self::open_for(&id)
    }

    /// The store the native host keeps launch-environment documents in: an
    /// [`FsStore`] over the platform data directory (`$XDG_DATA_HOME`, else
    /// `~/.local/share`).
    pub fn native_store() -> Rc<dyn DurableStore> {
        Rc::new(FsStore::new(cli::data_home()))
    }

    /// Open the store for an explicit package id — for tests and migration
    /// tools; rejects any id that is not a single, non-traversing directory name.
    ///
    /// # Errors
    ///
    /// Returns an error if `package_id` fails `validate_package_id`, or if
    /// the package's state directory can't be created, inspected or locked.
    pub fn open_for(package_id: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        Self::shared(Self::native_store(), StorageKey::package(package_id))
    }

    /// Open one record in one collaboration room.  A record id is only
    /// unique *within* a room: a hostile or stale peer must not be able to
    /// make `uuid:...` in room B resolve to room A's change graph.
    ///
    /// # Errors
    ///
    /// Returns an error when an identifier is invalid or the room-scoped
    /// store cannot be created, locked, or loaded.
    pub fn open_room_record(
        package_id: &str,
        room_key: &str,
        record_key: &str,
    ) -> Result<Self, StoreError> {
        validate_record_identity(package_id, room_key, record_key)?;
        Self::shared(
            Self::native_store(),
            StorageKey::record(package_id, room_key, record_key),
        )
    }

    /// Open the index of **one room** — the list of which records that room
    /// holds. A sibling of the room's records, so a room id can never resolve
    /// to a record's document and merge the two.
    ///
    /// # Errors
    ///
    /// Returns an error if `package_id` fails `validate_package_id` or
    /// `room_key` fails `validate_record_key`, or if the room's state
    /// directory can't be created, inspected or locked.
    pub fn open_room(package_id: &str, room_key: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(room_key)?;
        Self::shared(
            Self::native_store(),
            StorageKey::room_index(package_id, room_key),
        )
    }

    /// Join `key`'s shared writers on `store`.
    ///
    /// # Errors
    /// An exclusive owner holds the key, or the store refuses it.
    pub fn shared(store: Rc<dyn DurableStore>, key: StorageKey) -> Result<Self, StoreError> {
        let package_id = key.package_id().unwrap_or_default().to_owned();
        Ok(Self::from_owner(
            package_id,
            DocOwner::acquire_shared(store, key)?,
        ))
    }

    /// Become `key`'s exclusive owner on `store`.
    ///
    /// # Errors
    /// Another owner holds the key, or the store refuses it.
    pub fn exclusive(store: Rc<dyn DurableStore>, key: StorageKey) -> Result<Self, StoreError> {
        let package_id = key.package_id().unwrap_or_default().to_owned();
        Ok(Self::from_owner(package_id, DocOwner::acquire(store, key)?))
    }

    fn from_owner(package_id: String, owner: DocOwner) -> Self {
        let directory = owner
            .label()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self {
            package_id,
            owner: Rc::new(owner),
            schema_version: SCHEMA_VERSION,
            persisted: Vec::new(),
            has_base: None,
            refusal: None,
            directory,
        }
    }

    /// A shared writer for the document kept directly in `dir`, for tests
    /// addressing a directory rather than a package.
    #[cfg(test)]
    fn open_in(package_id: &str, dir: &Path) -> Result<Self, StoreError> {
        let (store, key) = directory_key(dir)?;
        let mut opened = Self::shared(store, key)?;
        package_id.clone_into(&mut opened.package_id);
        Ok(opened)
    }

    /// The exclusive owner of the document kept directly in `directory`.
    /// Never adopts stale files. The ownership gate is cooperative: older
    /// binaries that do not acquire it can still write, so concurrent use of
    /// one profile with those binaries is unsupported.
    ///
    /// # Errors
    /// Rejects invalid paths, contested gates and every filesystem
    /// durability failure.
    pub fn open_canonical_at(package_id: &str, directory: &Path) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        let (store, key) = directory_key(directory)?;
        let mut opened = Self::exclusive(store, key)?;
        package_id.clone_into(&mut opened.package_id);
        Ok(opened)
    }

    /// The package-global document path under the standalone state root.
    ///
    /// # Errors
    ///
    /// Returns an error when `package_id` is invalid.
    pub fn legacy_package_document_path(package_id: &str) -> Result<PathBuf, StoreError> {
        validate_package_id(package_id)?;
        Ok(cli::standalone_state_dir(package_id).join(DOCUMENT_FILE))
    }

    /// The existing room-scoped package-primary directory, with explicit custody.
    ///
    /// # Errors
    /// Rejects relative roots and invalid package or room identifiers.
    pub fn room_primary_dir_at(
        profile_root: &Path,
        package: &str,
        room: &str,
    ) -> Result<PathBuf, StoreError> {
        validate_record_namespace(package, room)?;
        validate_absolute_directory(profile_root)?;
        Ok(profile_root.join(package).join(ROOMS_DIR).join(room))
    }

    /// Resolve one record under an explicit profile root without consulting the environment.
    ///
    /// # Errors
    /// Rejects relative roots and invalid package, room or record identifiers.
    pub fn room_record_dir_at(
        profile_root: &Path,
        package: &str,
        room: &str,
        record: &str,
    ) -> Result<PathBuf, StoreError> {
        validate_package_id(package)?;
        validate_record_key(room)?;
        validate_record_key(record)?;
        validate_absolute_directory(profile_root)?;
        Ok(profile_root
            .join(package)
            .join(ROOMS_DIR)
            .join(room)
            .join(RECORDS_DIR)
            .join(record))
    }

    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Select the app-owned CRDT layout this store reads and writes. Call
    /// this before `load`, `load_or_new`, or creating a writer.
    #[must_use]
    pub fn with_schema_version(mut self, version: u32) -> Self {
        self.schema_version = version;
        self
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The directory holding the document, its actor sidecar and its locks,
    /// where the store keeps one.
    pub fn dir(&self) -> &Path {
        &self.directory
    }

    /// See [`DocOwner::label`].
    pub fn document_path(&self) -> &Path {
        self.owner.label()
    }

    /// The Automerge actor this store writes as.
    pub fn actor(&self) -> &ActorId {
        self.owner.actor()
    }

    /// Whether this process continues the stored writer identity. `false`
    /// means another instance is live: writes still succeed (merged on next
    /// load) but the actor id is fresh rather than the stored one.
    pub fn is_exclusive_writer(&self) -> bool {
        self.owner.is_primary()
    }

    /// Load the persisted document, or `Ok(None)` on a first launch. A trailing
    /// partially-written chunk (crash during append) is dropped rather than
    /// failing the load, losing only the last unflushed edit.
    ///
    /// # Errors
    ///
    /// Returns an error if the document exists but can't be read, if its
    /// bytes aren't a loadable Automerge document, or if it declares a schema
    /// layout newer than this build understands ([`StoreError::NewerSchema`]).
    pub fn load(&mut self) -> Result<Option<AutoCommit>, StoreError> {
        self.load_with_completeness(false)
    }

    fn load_with_completeness(&mut self, complete: bool) -> Result<Option<AutoCommit>, StoreError> {
        let Some(stored) = self.owner.stored()? else {
            self.has_base = Some(false);
            return Ok(None);
        };
        let bytes = stored.bytes;
        self.has_base = Some(!bytes.is_empty());
        if bytes.is_empty() {
            return if complete {
                Err(StoreError::EmptyExistingDocument {
                    path: self.document_path().to_path_buf(),
                })
            } else {
                Ok(None)
            };
        }
        let mut doc = AutoCommit::load_with_options(
            &bytes,
            LoadOptions::new().on_partial_load(if complete {
                OnPartialLoad::Error
            } else {
                OnPartialLoad::Ignore
            }),
        )
        .map_err(|source| StoreError::Automerge {
            path: self.document_path().to_path_buf(),
            source: Box::new(source),
        })?;
        // Check before reading any value: a newer layout may reuse a key with a
        // different meaning, and the writer above saves the whole document back.
        let found = schema_version_of(&doc);
        if found > self.schema_version {
            return Err(StoreError::NewerSchema {
                path: self.document_path().to_path_buf(),
                found,
                understood: self.schema_version,
            });
        }
        doc.set_actor(self.actor().clone());
        self.persisted = doc.get_heads();
        Ok(Some(doc))
    }

    /// Load a complete existing change graph for checked record admission.
    ///
    /// # Errors
    /// Rejects incomplete, empty, unreadable, or unsupported stored documents.
    pub fn load_complete(&mut self) -> Result<Option<AutoCommit>, StoreError> {
        self.load_with_completeness(true)
    }

    /// Load and, when necessary, atomically migrate an older app-owned CRDT
    /// layout to [`Self::schema_version`]. The callback must be deterministic
    /// and idempotent because another replica may independently migrate the
    /// same logical document before their changes merge.
    ///
    /// A successful migration is written before returning. This keeps the
    /// autosave frontier aligned with what is actually durable; merely
    /// mutating the returned document and then seeding autosave would
    /// otherwise make those migration heads look already persisted.
    ///
    /// # Errors
    ///
    /// Returns load/write errors unchanged, or [`StoreError::Migration`] when
    /// the app callback rejects the older layout.
    pub fn load_migrating<F>(&mut self, migrate: F) -> Result<Option<AutoCommit>, StoreError>
    where
        F: FnOnce(&mut AutoCommit, u32, u32) -> Result<(), String>,
    {
        let Some(mut doc) = self.load()? else {
            return Ok(None);
        };
        let found = schema_version_of(&doc);
        if found >= self.schema_version {
            return Ok(Some(doc));
        }
        migrate(&mut doc, found, self.schema_version).map_err(|message| StoreError::Migration {
            path: self.document_path().to_path_buf(),
            from: found,
            to: self.schema_version,
            message,
        })?;
        stamp_schema_version(&mut doc, self.schema_version);
        self.save(&mut doc)?;
        Ok(Some(doc))
    }

    /// Load the persisted document, falling back to an empty one. An unreadable
    /// file is logged and treated as empty rather than crashing on launch. A
    /// document from a **newer** layout is different: the refusal is recorded
    /// ([`DocStore::refusal`]) and every writer built on this store switches
    /// itself off, since starting empty and then saving over the file would be
    /// data loss rather than a survivable empty session.
    pub fn load_or_new(&mut self) -> AutoCommit {
        match self.load() {
            Ok(Some(doc)) => doc,
            Ok(None) => self.new_document(),
            Err(e @ StoreError::NewerSchema { found, .. }) => {
                tracing::error!(error = %e, "app state was written by a newer version; \
                                             starting empty and NOT writing back");
                let message = e.to_string();
                let doc = self.new_document();
                // After new_document, which clears it: the refusal describes the file, not
                // the fresh document, and must outlive this call for the write paths below.
                self.refusal = Some(Refusal { found, message });
                doc
            }
            Err(e) => {
                tracing::error!(error = %e, "app state unreadable; starting empty");
                self.new_document()
            }
        }
    }

    /// Why this store handed back an empty document, when it did — `Some` means
    /// the on-disk document must not be written over.
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_ref().map(|r| r.message.as_str())
    }

    /// An empty document already carrying this store's actor id, stamped with
    /// the current layout version at creation (not first save) so the version
    /// is never missing from a document written outside this path.
    pub fn new_document(&mut self) -> AutoCommit {
        self.persisted = Vec::new();
        self.refusal = None;
        let mut doc = AutoCommit::new().with_actor(self.actor().clone());
        stamp_schema_version(&mut doc, self.schema_version);
        doc
    }

    /// Replace the whole document and wait until the store has committed it
    /// — for [`FsStore`], a synced temporary file renamed into place, so a
    /// crash leaves either the previous or the new document complete.
    /// Refuses when [`DocStore::refusal`] is set, since the in-memory
    /// document is an empty stand-in and writing it out would delete the
    /// user's actual data.
    ///
    /// # Errors
    ///
    /// Returns an error if [`DocStore::refusal`] is set (writing would
    /// discard a newer-schema document already stored), or if the store
    /// fails to commit it.
    pub fn save(&mut self, doc: &mut AutoCommit) -> Result<(), StoreError> {
        self.refuse_to_write()?;
        ensure_schema_version(doc, self.schema_version);
        let bytes = doc.save();
        self.commit(WriteOp::Replace(bytes))?;
        self.has_base = Some(true);
        self.persisted = doc.get_heads();
        Ok(())
    }

    /// The guard both write paths share; returns the original refusal so a
    /// caller can log why, not just "write failed".
    fn refuse_to_write(&self) -> Result<(), StoreError> {
        match &self.refusal {
            None => Ok(()),
            Some(r) => Err(StoreError::NewerSchema {
                path: self.document_path().to_path_buf(),
                found: r.found,
                understood: self.schema_version,
            }),
        }
    }

    /// Append only the changes `doc` has that the store does not, and wait
    /// for the store to commit them. Returns bytes appended (`0` when idle).
    /// Refuses for the same data-loss reason as [`DocStore::save`].
    ///
    /// # Errors
    ///
    /// Returns an error if [`DocStore::refusal`] is set, or if the store
    /// fails to commit the changes.
    pub fn append(&mut self, doc: &mut AutoCommit) -> Result<usize, StoreError> {
        self.refuse_to_write()?;
        ensure_schema_version(doc, self.schema_version);
        let heads = doc.get_heads();
        if heads == self.persisted {
            return Ok(0);
        }
        let has_base = match self.has_base {
            Some(known) => known,
            None => self
                .owner
                .stored()?
                .is_some_and(|stored| !stored.bytes.is_empty()),
        };
        // Nothing stored yet: seed a full save (compresses far better than raw
        // change chunks with no document chunk in front).
        if !has_base {
            self.save(doc)?;
            return Ok(0);
        }
        let delta = doc.save_after(&self.persisted);
        if delta.is_empty() {
            self.persisted = heads;
            return Ok(0);
        }
        let len = delta.len();
        self.commit(WriteOp::Append(delta))?;
        self.persisted = heads;
        Ok(len)
    }

    /// Whether the store holds anything for this document, even bytes that
    /// would not load.
    ///
    /// # Errors
    /// A failure reading the store.
    pub fn has_stored(&self) -> Result<bool, StoreError> {
        Ok(self.owner.stored()?.is_some())
    }

    /// Submit one write through the owner and wait for its outcome.
    fn commit(&self, op: WriteOp) -> Result<(), StoreError> {
        let store = self.owner.store();
        let key = self.owner.key();
        let writer = self.owner.unstored_writer();
        let revision = store.write(key, op, writer.as_deref())?;
        match store.flush(Some(key)) {
            Ok(()) | Err(durable_store::StoreError::Pending(_)) => {}
            Err(error) => return Err(error.into()),
        }
        if let durable_store::Durability::Failed(message) = store.durability(key, revision) {
            return Err(io_err(
                format!("writing {}", self.document_path().display()),
                io::Error::other(message),
            ));
        }
        if writer.is_some() {
            self.owner.note_writer_stored();
        }
        Ok(())
    }

    /// A [`DocWriter`] continuing this document in the background: a burst
    /// of edits becomes one write. Seed it with the loaded document (see
    /// [`DocWriter::seed`]). A refused store hands back a disabled writer,
    /// so the stand-in document never reaches storage.
    pub fn writer(self) -> DocWriter {
        if let Some(refusal) = &self.refusal {
            return DocWriter::disabled(refusal.message.clone());
        }
        if !self.persisted.is_empty() {
            let base = self
                .owner
                .stored()
                .ok()
                .flatten()
                .map_or(0, |stored| stored.bytes.len());
            self.owner.adopt(self.persisted.clone(), base);
        }
        DocWriter::try_start(&self.owner)
            .unwrap_or_else(|error| DocWriter::disabled(error.to_string()))
    }
}

/// The store and key addressing the document kept directly in `directory`.
fn directory_key(directory: &Path) -> Result<(Rc<dyn DurableStore>, StorageKey), StoreError> {
    validate_absolute_directory(directory)?;
    let (Some(root), Some(name)) = (
        directory.parent(),
        directory.file_name().and_then(|name| name.to_str()),
    ) else {
        return Err(io_err(
            "a document directory needs a named parent",
            io::Error::from(io::ErrorKind::InvalidInput),
        ));
    };
    Ok((
        Rc::new(FsStore::new(root)) as Rc<dyn DurableStore>,
        StorageKey::package(name),
    ))
}

fn validate_absolute_directory(path: &Path) -> Result<(), StoreError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(io_err(
            "absolute directory without parent traversal required",
            io::Error::from(io::ErrorKind::InvalidInput),
        ));
    }
    Ok(())
}

/// Decode stored canonical bytes without publishing anything. With
/// `recover_partial`, a torn trailing chunk (a crash mid-append) is dropped
/// and the complete prefix is returned together with the bytes that would
/// replace the torn original once the caller accepts it; the original is
/// never rewritten here. `label` names the source in errors.
///
/// # Errors
/// Refuses empty, undecodable, incomplete (without recovery), malformed or
/// newer-schema documents.
pub fn decode_canonical(
    bytes: &[u8],
    recover_partial: bool,
    label: &Path,
) -> Result<(AutoCommit, Option<Vec<u8>>), StoreError> {
    if bytes.is_empty() {
        return Err(StoreError::EmptyExistingDocument {
            path: label.to_path_buf(),
        });
    }
    let automerge_error = |source| StoreError::Automerge {
        path: label.to_path_buf(),
        source: Box::new(source),
    };
    let (mut doc, repair) = match AutoCommit::load_with_options(
        bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Error),
    ) {
        Ok(doc) => (doc, false),
        Err(error) if !recover_partial => return Err(automerge_error(error)),
        Err(_) => (
            AutoCommit::load_with_options(
                bytes,
                LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
            )
            .map_err(automerge_error)?,
            true,
        ),
    };
    validate_canonical_schema_of(&doc, label)?;
    let repair = repair.then(|| doc.save());
    Ok((doc, repair))
}

/// Validate a canonical package/workspace identity independently of its storage adapter.
///
/// # Errors
/// Rejects an invalid package id or workspace key.
pub fn validate_record_namespace(package: &str, workspace: &str) -> Result<(), StoreError> {
    validate_package_id(package)?;
    validate_record_key(workspace)
}

/// Validate portable or filesystem document schema assignments without
/// mutating bytes; the portable-record name for [`validate_canonical_schema_of`].
///
/// # Errors
/// Rejects malformed, conflicting and newer schemas.
pub fn validate_document_schema(doc: &AutoCommit, path: &Path) -> Result<(), StoreError> {
    validate_canonical_schema_of(doc, path)
}

/// Validate every live schema assignment of a canonical document without
/// coercion or mutation. `label` names the source in errors.
///
/// # Errors
/// Rejects malformed, conflicting and newer schemas.
pub fn validate_canonical_schema_of(doc: &AutoCommit, label: &Path) -> Result<(), StoreError> {
    let invalid = |reason| StoreError::InvalidSchema {
        path: label.to_path_buf(),
        reason,
    };
    let automerge_error = |source| StoreError::Automerge {
        path: label.to_path_buf(),
        source: Box::new(source),
    };
    let mut versions = Vec::new();
    for (value, _) in doc
        .get_all(ROOT, SCHEMA_VERSION_KEY)
        .map_err(automerge_error)?
    {
        let raw = match value {
            automerge::Value::Scalar(value) => match value.as_ref() {
                automerge::ScalarValue::Uint(v) => *v,
                automerge::ScalarValue::Int(v) => {
                    u64::try_from(*v).map_err(|_| invalid("negative schema version"))?
                }
                _ => return Err(invalid("schema version must be an integer")),
            },
            automerge::Value::Object(_) => {
                return Err(invalid("schema version must be a scalar"));
            }
        };
        versions.push(u32::try_from(raw).map_err(|_| invalid("schema version exceeds u32"))?);
    }
    if let Some(found) = versions
        .iter()
        .copied()
        .max()
        .filter(|v| *v > SCHEMA_VERSION)
    {
        return Err(StoreError::NewerSchema {
            path: label.to_path_buf(),
            found,
            understood: SCHEMA_VERSION,
        });
    }
    if versions.iter().any(|v| Some(v) != versions.first()) {
        return Err(invalid("conflicting schema versions"));
    }
    Ok(())
}

/// `fsync` a directory and every ancestor, so a newly created or renamed
/// entry survives a crash however deep it was created. `visit` runs before
/// each directory and may refuse it.
fn sync_directory_ancestors(
    dir: &Path,
    mut visit: impl FnMut(&Path) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let physical = fs::canonicalize(dir).map_err(|e| io_err("resolving canonical directory", e))?;
    // The home directory is provisioned before the app runs. Our durability
    // obligation includes that root and every child directory, including
    // ancestors left by an interrupted earlier open, but not the directories
    // above it, which the app never creates and may not even open: the
    // OS-owned C:\Users or volume root on Windows (queried as the known
    // folder, not an environment variable), the app sandbox on Android
    // (`ui::android_main!` makes it `$HOME`), and the room directory the
    // Linux launcher binds at `/run/tilo/app` beneath a Landlock boundary.
    let profile_root = tilo_paths::home().and_then(|path| fs::canonicalize(path).ok());
    for path in physical.ancestors() {
        visit(path)?;
        open_directory(path, true)
            .and_then(|f| f.sync_all())
            .map_err(|e| io_err(format!("syncing directory {}", path.display()), e))?;
        if profile_root.as_deref() == Some(path) {
            break;
        }
    }
    Ok(())
}

fn open_directory(path: &Path, writable: bool) -> io::Result<File> {
    #[cfg(not(windows))]
    let _ = writable;
    let mut options = OpenOptions::new();
    options.read(true);
    // Windows directory handles require BACKUP_SEMANTICS, and
    // FlushFileBuffers requires a handle opened for writing.
    #[cfg(windows)]
    options.write(writable).custom_flags(0x0200_0000);
    options.open(path)
}

#[cfg(unix)]
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(windows)]
fn same_file(file: &File, path: &Path) -> bool {
    let opened = file.try_clone().and_then(same_file::Handle::from_file);
    match (opened, same_file::Handle::from_path(path)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(not(any(unix, windows)))]
fn same_file(_file: &File, _path: &Path) -> bool {
    false
}

fn acquire_gate(path: &Path, exclusive: bool) -> Result<FileLock, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io_err(format!("opening gate {}", path.display()), e))?;
    let mode = if exclusive {
        LockMode::Exclusive
    } else {
        LockMode::Shared
    };
    FileLock::try_lock(file, mode).map_err(|error| match error {
        TryLockError::WouldBlock => StoreError::WriterBusy {
            path: path.to_path_buf(),
        },
        TryLockError::Error(error) => io_err("acquiring canonical gate", error),
    })
}

/// Atomically assign a source to one claimant.  A marker is deliberately
/// retained forever: deleting it after a successful adoption would let a
/// later claimant adopt the still-preserved source again, while deleting it
/// after a crash would make recovery non-deterministic.
fn claim_marker(marker: &Path, claimant: &str) -> Result<bool, StoreError> {
    match read_claim(marker)? {
        ClaimState::Owner(owner) => return Ok(owner == claimant),
        ClaimState::Malformed => {
            // Never overwrite a malformed old marker: a separate recovery
            // name lets one contender claim atomically without a stale reader
            // deleting another contender's valid replacement.
            let recovered = marker.with_extension("recovered");
            return publish_claim(&recovered, claimant);
        }
        ClaimState::Absent => {}
    }
    publish_claim(marker, claimant)
}

enum ClaimState {
    Absent,
    Malformed,
    Owner(String),
}

fn read_claim(marker: &Path) -> Result<ClaimState, StoreError> {
    let bytes = match fs::read(marker) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ClaimState::Absent),
        Err(error) => return Err(io_err(format!("reading {}", marker.display()), error)),
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(ClaimState::Malformed);
    };
    let Some(owner) = text.strip_suffix('\n') else {
        return Ok(ClaimState::Malformed);
    };
    if owner.is_empty() || owner.contains('\n') || owner.contains('\r') {
        return Ok(ClaimState::Malformed);
    }
    if validate_record_key(owner).is_err() {
        return Ok(ClaimState::Malformed);
    }
    Ok(ClaimState::Owner(owner.to_string()))
}

/// Publish a fully durable temp file with a no-replace hard link.  `link(2)`
/// gives the needed compare-and-create property: even two recovery callers
/// can never both become owner.
fn publish_claim(marker: &Path, room_key: &str) -> Result<bool, StoreError> {
    static CLAIM_NONCE: AtomicU64 = AtomicU64::new(0);
    let parent = marker.parent().ok_or_else(|| StoreError::Io {
        context: format!("finding parent of {}", marker.display()),
        source: io::Error::new(io::ErrorKind::InvalidInput, "claim has no parent"),
    })?;
    let nonce = CLAIM_NONCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".tilo-claim-{}-{}-{}",
        std::process::id(),
        nonce,
        room_key
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| io_err(format!("creating {}", temp.display()), error))?;
    file.write_all(format!("{room_key}\n").as_bytes())
        .map_err(|error| io_err(format!("writing {}", temp.display()), error))?;
    file.sync_all()
        .map_err(|error| io_err(format!("syncing {}", temp.display()), error))?;
    drop(file);
    match fs::hard_link(&temp, marker) {
        Ok(()) => {
            let _ = fs::remove_file(&temp);
            open_directory(parent, true)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| io_err(format!("syncing {}", parent.display()), error))?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&temp);
            match read_claim(marker)? {
                ClaimState::Owner(owner) => Ok(owner == room_key),
                // A new claim is never partial (it is linked only after its
                // temp file is synced).  Do not guess at an invalid recovery
                // marker: failing closed keeps two rooms from winning.
                ClaimState::Malformed => Err(StoreError::Io {
                    context: format!("reading complete claim {}", marker.display()),
                    source: io::Error::new(io::ErrorKind::InvalidData, "malformed claim"),
                }),
                ClaimState::Absent => publish_claim(marker, room_key),
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(io_err(format!("claiming {}", marker.display()), error))
        }
    }
}

/// Make `dir` an existing, writable directory.
fn ensure_state_dir(dir: &Path) -> Result<(), StoreError> {
    fs::create_dir_all(dir)
        .map_err(|e| io_err(format!("creating state directory {}", dir.display()), e))
}

/// Take an exclusive, non-blocking lock on `path`. `None` when another
/// process holds it, or (conservatively) when the lock file can't even be created.
fn acquire_writer_lock(path: &Path) -> Option<FileLock> {
    FileLock::try_open(path, LockMode::Exclusive).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue};
    use tempfile::TempDir;
    use tilo_automerge::ApplyIncoming;

    /// A scratch data home that `standalone_state_dir` resolves to while it
    /// lives. The environment is restored before the tree is deleted.
    struct ScratchHome {
        _env: tilo_test_env::ScopedEnv,
        dir: TempDir,
    }

    impl ScratchHome {
        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    /// Point `standalone_state_dir` at a scratch tree, for the tests whose
    /// subject is the environment-resolving constructors.
    fn scratch_home() -> ScratchHome {
        let dir = TempDir::new().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", dir.path())
            .remove("TILO_PACKAGE_ID");
        ScratchHome { _env: env, dir }
    }

    #[test]
    #[serial_test::serial]
    fn directory_sync_stops_at_the_home_directory() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let nested = home.join("state").join("docs");
        fs::create_dir_all(&nested).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &home);
        let mut visited = Vec::new();
        sync_directory_ancestors(&nested, |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        drop(env);
        let home = fs::canonicalize(&home).unwrap();
        assert_eq!(visited.last(), Some(&home), "{visited:?}");
        assert_eq!(visited.len(), 3, "{visited:?}");
    }

    #[test]
    #[serial_test::serial]
    fn claim_markers_grant_exactly_one_claimant() {
        // Phase `a_marker_is_claimed_once_and_resume_is_stable`.
        {
            let tmp = TempDir::new().unwrap();
            let marker = tmp.path().join("claim");
            assert!(claim_marker(&marker, "room-a").unwrap());
            assert!(
                !claim_marker(&marker, "room-b").unwrap(),
                "a second claimant cannot adopt the same source"
            );
            assert!(
                claim_marker(&marker, "room-a").unwrap(),
                "the original claimant can resume after an interruption"
            );
        }
        // Phase `concurrent_claims_have_one_winner`.
        {
            let tmp = TempDir::new().unwrap();
            let marker = std::sync::Arc::new(tmp.path().join("claim"));
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let mut threads = Vec::new();
            for claimant in ["room-a", "room-b"] {
                let barrier = std::sync::Arc::clone(&barrier);
                let marker = std::sync::Arc::clone(&marker);
                threads.push(std::thread::spawn(move || {
                    barrier.wait();
                    claim_marker(&marker, claimant).unwrap()
                }));
            }
            let wins = threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|won| *won)
                .count();
            assert_eq!(wins, 1, "atomic create gives one claimant the source");
        }
        // Phase `a_torn_claim_recovers_without_granting_two_claimants`.
        {
            let tmp = TempDir::new().unwrap();
            let marker = tmp.path().join("claim");
            // Shape left by a create-then-write that died between those
            // syscalls.  It has no valid owner, so recovery uses a separate
            // atomic claim rather than replacing this file in place.
            fs::write(&marker, b"room-a").unwrap();
            assert!(claim_marker(&marker, "room-a").unwrap());
            assert!(!claim_marker(&marker, "room-b").unwrap());
            assert!(claim_marker(&marker, "room-a").unwrap());
        }
    }

    #[test]
    fn canonical_and_complete_loads_refuse_newer_invalid_or_truncated_documents() {
        // Phase `canonical_schema_checks_all_live_values_without_mutation`.
        {
            assert_malformed_schemas_are_rejected();
            assert_supported_schemas_are_nonmutating();
            assert_schema_conflicts_are_checked();
        }
        // Phase `complete_load_refuses_truncated_increment_without_accepting_or_rewriting_it`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut store = DocStore::open_in("dev.tilo.complete-load", tmp.path()).unwrap();
            let mut doc = store.new_document();
            doc.put(ROOT, "body", "first").unwrap();
            let mut bytes = doc.save();
            doc.put(ROOT, "body", "later version").unwrap();
            let increment = doc.save_incremental();
            assert!(increment.len() > 8);
            bytes.extend_from_slice(&increment[..increment.len() - 3]);
            fs::write(store.document_path(), &bytes).unwrap();
            let before = store.persisted.clone();
            assert!(
                store.load_complete().is_err(),
                "checked admission cannot accept a valid prefix with a truncated appended change"
            );
            assert_eq!(
                store.persisted, before,
                "refused content cannot become the admitted persisted frontier"
            );
            assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
            let legacy = store.load().unwrap().unwrap();
            assert_eq!(
                legacy.get(ROOT, "body").unwrap().unwrap().0.as_str(),
                Some("first"),
                "ordinary legacy recovery retains its prior partial-load behavior"
            );
        }
        // Phase `complete_load_accepts_full_history_and_distinguishes_missing_from_empty`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let mut store = DocStore::open_in("dev.tilo.complete-load", tmp.path()).unwrap();
            assert!(store.load_complete().unwrap().is_none());
            fs::write(store.document_path(), []).unwrap();
            assert!(matches!(
                store.load_complete(),
                Err(StoreError::EmptyExistingDocument { .. })
            ));
            let mut doc = store.new_document();
            doc.put(ROOT, "body", "first").unwrap();
            let mut bytes = doc.save();
            doc.put(ROOT, "body", "second").unwrap();
            bytes.extend_from_slice(&doc.save_incremental());
            let heads = doc.get_heads();
            fs::write(store.document_path(), &bytes).unwrap();
            let mut loaded = store.load_complete().unwrap().unwrap();
            assert_eq!(loaded.get_heads(), heads);
            assert_eq!(store.persisted, heads);
            assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
        }
    }

    #[test]
    #[serial_test::serial]
    fn store_paths_follow_the_contained_package_id() {
        // Phase `path_is_derived_from_the_package_id`.
        {
            let tmp = scratch_home();
            let store = DocStore::open_for("so.tilo.ebook").unwrap();
            assert_eq!(store.dir(), tmp.path().join("so.tilo.ebook"));
            assert_eq!(
                store.document_path(),
                tmp.path().join("so.tilo.ebook").join(DOCUMENT_FILE)
            );
        }
        // Phase `open_reads_the_launcher_package_id`.
        {
            let tmp = scratch_home();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_PACKAGE_ID", "so.tilo.monitor");
            let store = DocStore::open().unwrap();
            assert_eq!(store.package_id(), "so.tilo.monitor");
            assert_eq!(store.dir(), tmp.path().join("so.tilo.monitor"));
        }
        // Phase `package_id_cannot_escape_the_data_root`.
        // The containment boundary: an app that lies about its own package id must
        // not be able to address another app's directory or escape the data root.
        {
            let _tmp = scratch_home();
            for bad in [
                "../so.tilo.mail",
                "../../etc",
                "/etc/shadow",
                "so.tilo.mail/../so.tilo.chat",
                "a/b",
                "..",
                ".",
                "",
            ] {
                let err = DocStore::open_for(bad)
                    .err()
                    .unwrap_or_else(|| panic!("package id {bad:?} must be rejected"));
                assert!(
                    matches!(err, StoreError::PackageId(..)),
                    "package id {bad:?} rejected for the wrong reason: {err}"
                );
            }
        }
    }

    fn with_partial_tail(doc: &mut AutoCommit) -> Vec<u8> {
        let heads = doc.get_heads();
        let mut bytes = doc.save();
        doc.put(ROOT, "incomplete_tail", true).unwrap();
        let tail = doc.save_after(&heads);
        bytes.extend_from_slice(&tail[..tail.len() / 2]);
        bytes
    }

    fn assert_malformed_schemas_are_rejected() {
        for value in [
            ScalarValue::Int(-1),
            ScalarValue::Int(i64::MAX),
            ScalarValue::Uint(u64::MAX),
            ScalarValue::F64(1.0),
            ScalarValue::Str("1".into()),
            ScalarValue::Boolean(true),
            ScalarValue::Null,
            ScalarValue::Timestamp(1),
            ScalarValue::Counter(1.into()),
            ScalarValue::Bytes(vec![1]),
        ] {
            for torn in [false, true] {
                let mut doc = AutoCommit::new();
                doc.put(ROOT, SCHEMA_VERSION_KEY, value.clone()).unwrap();
                assert_eq!(
                    schema_version_of(&doc),
                    SCHEMA_VERSION,
                    "legacy remains tolerant"
                );
                let bytes = if torn {
                    with_partial_tail(&mut doc)
                } else {
                    doc.save()
                };
                assert!(matches!(
                    decode_canonical(&bytes, true, Path::new("doc")),
                    Err(StoreError::InvalidSchema { .. })
                ));
            }
        }
        let mut object = AutoCommit::new();
        object
            .put_object(ROOT, SCHEMA_VERSION_KEY, ObjType::Map)
            .unwrap();
        assert!(matches!(
            validate_canonical_schema_of(&object, Path::new("doc")),
            Err(StoreError::InvalidSchema { .. })
        ));
    }

    fn assert_supported_schemas_are_nonmutating() {
        for value in [
            None,
            Some(ScalarValue::Int(0)),
            Some(ScalarValue::Int(1)),
            Some(ScalarValue::Uint(1)),
            Some(ScalarValue::Uint(2)),
        ] {
            for torn in [false, true] {
                let mut doc = AutoCommit::new();
                if let Some(value) = value.clone() {
                    doc.put(ROOT, SCHEMA_VERSION_KEY, value).unwrap();
                }
                let newer = schema_version_of(&doc) > SCHEMA_VERSION;
                let bytes = if torn {
                    with_partial_tail(&mut doc)
                } else {
                    doc.save()
                };
                if newer {
                    assert!(matches!(
                        decode_canonical(&bytes, true, Path::new("doc")),
                        Err(StoreError::NewerSchema { .. })
                    ));
                } else {
                    let loaded = decode_canonical(&bytes, true, Path::new("doc")).unwrap();
                    if value.is_none() {
                        assert!(loaded.0.get(ROOT, SCHEMA_VERSION_KEY).unwrap().is_none());
                    }
                }
            }
        }
    }

    fn assert_schema_conflicts_are_checked() {
        for (left, right, accepted) in [
            (ScalarValue::Int(1), ScalarValue::Uint(1), true),
            (ScalarValue::Uint(0), ScalarValue::Uint(1), false),
        ] {
            let mut a = AutoCommit::new();
            let mut b = a.fork();
            a.put(ROOT, SCHEMA_VERSION_KEY, left).unwrap();
            b.put(ROOT, SCHEMA_VERSION_KEY, right).unwrap();
            a.merge_incoming(&mut b).unwrap();
            let bytes = a.save();
            if accepted {
                let loaded = decode_canonical(&bytes, true, Path::new("doc")).unwrap();
                let restored = loaded.0;
                assert_eq!(restored.get_all(ROOT, SCHEMA_VERSION_KEY).unwrap().len(), 2);
            } else {
                assert!(matches!(
                    decode_canonical(&bytes, true, Path::new("doc")),
                    Err(StoreError::InvalidSchema {
                        reason: "conflicting schema versions",
                        ..
                    })
                ));
            }
        }
        let mut supported_winner_seen = false;
        for swap in [false, true] {
            let mut a = AutoCommit::new().with_actor(ActorId::from(vec![1]));
            let mut b = AutoCommit::new().with_actor(ActorId::from(vec![2]));
            a.put(ROOT, SCHEMA_VERSION_KEY, if swap { 1_u64 } else { 2 })
                .unwrap();
            b.put(ROOT, SCHEMA_VERSION_KEY, if swap { 2_u64 } else { 1 })
                .unwrap();
            a.merge_incoming(&mut b).unwrap();
            supported_winner_seen |= schema_version_of(&a) == 1;
            let bytes = a.save();
            assert!(matches!(
                decode_canonical(&bytes, true, Path::new("doc")),
                Err(StoreError::NewerSchema { found: 2, .. })
            ));
        }
        assert!(
            supported_winner_seen,
            "must exercise unsupported losing assignment"
        );
    }

    #[test]
    fn store_gates_locks_and_directories_fail_closed() {
        // Phase `canonical_gate_blocks_legacy_before_actor_creation`.
        {
            let tmp = TempDir::new().unwrap();
            // The exclusive owner holds this gate before touching actor or document.
            let gate = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
            let result = DocStore::open_in("so.tilo.test", tmp.path());
            assert!(
                result.is_err(),
                "a legacy opener must respect the canonical gate"
            );
            assert!(!tmp.path().join(ACTOR_FILE).exists());
            assert!(!tmp.path().join(DOCUMENT_FILE).exists());
            drop(gate);
            assert!(DocStore::open_in("so.tilo.test", tmp.path()).is_ok());
        }
        // Phase `writer_lock_is_exclusive_and_released_with_its_handle`.
        {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("writer.lock");
            let first = acquire_writer_lock(&path).expect("first writer lock");
            assert!(
                acquire_writer_lock(&path).is_none(),
                "a second live handle must not acquire the writer lock"
            );
            drop(first);
            assert!(
                acquire_writer_lock(&path).is_some(),
                "dropping the owner must release the writer lock"
            );
        }
        // Phase `an_uncreatable_state_dir_is_still_an_error`.
        // A state directory that genuinely cannot be created must still surface as
        // an error — idempotent create must not paper over a real filesystem failure.
        {
            // A file where a parent directory needs to be fails ENOTDIR for any
            // uid — unlike a read-only directory, which root would sail past.
            let tmp = TempDir::new().unwrap();
            let blocker = tmp.path().join("blocker");
            fs::write(&blocker, b"not a directory").unwrap();

            let result = DocStore::shared(
                Rc::new(FsStore::new(blocker.join("sub"))),
                StorageKey::package("so.tilo.blocked"),
            );

            match result {
                Err(StoreError::Io { .. }) => {}
                Err(other) => {
                    panic!(
                        "expected an Io error from an uncreatable state directory, got {other:?}"
                    )
                }
                Ok(_) => panic!(
                    "an uncreatable state directory must surface as an error, not be \
                     masked by the idempotent create"
                ),
            }
        }
    }

    fn text_of(doc: &AutoCommit, key: &str) -> Option<String> {
        str_at(doc, &ROOT, key)
    }

    fn str_at(doc: &AutoCommit, obj: &automerge::ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((automerge::Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                other => Some(other.to_string()),
            },
            _ => None,
        }
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn saves_and_appends_round_trip_and_survive_interruption() {
        // Phase `round_trip_preserves_content`.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.roundtrip").unwrap();
            assert!(store.load().unwrap().is_none(), "nothing saved yet");

            let mut doc = store.new_document();
            doc.put(&ROOT, "title", ScalarValue::Str("Moby Dick".into()))
                .unwrap();
            let notes = doc.put_object(&ROOT, "notes", ObjType::Text).unwrap();
            doc.splice_text(&notes, 0, 0, "call me ishmael").unwrap();
            store.save(&mut doc).unwrap();
            drop((store, doc));

            let mut reopened = DocStore::open_for("so.tilo.roundtrip").unwrap();
            let loaded = reopened.load().unwrap().expect("document is on disk");
            assert_eq!(text_of(&loaded, "title").as_deref(), Some("Moby Dick"));
            let notes = loaded.get(&ROOT, "notes").unwrap().unwrap().1;
            assert_eq!(loaded.text(&notes).unwrap(), "call me ishmael");
        }
        // Phase `round_trip_merges_with_a_replica_that_diverged_before_the_save`.
        // The anti-`contacts` test: `apps/contacts` used to save by replaying its
        // model into a brand-new `AutoCommit`, producing a file with no shared
        // history with any peer, so merges duplicated records instead of
        // converging. A document through this store must still merge cleanly with
        // a replica that diverged before the save.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.merge").unwrap();

            let mut doc = store.new_document();
            let list = doc.put_object(&ROOT, "contacts", ObjType::Map).unwrap();
            doc.put(&list, "ada", ScalarValue::Str("Ada Lovelace".into()))
                .unwrap();
            let original_actor = doc.get_actor().clone();

            let mut peer = doc.fork().with_actor(ActorId::random());
            let peer_list = peer.get(&ROOT, "contacts").unwrap().unwrap().1;
            peer.put(&peer_list, "grace", ScalarValue::Str("Grace Hopper".into()))
                .unwrap();

            doc.put(&list, "alan", ScalarValue::Str("Alan Turing".into()))
                .unwrap();
            store.save(&mut doc).unwrap();
            drop((store, doc));

            let mut reopened = DocStore::open_for("so.tilo.merge").unwrap();
            let mut restored = reopened.load().unwrap().expect("saved document");

            // Actor identity survives the restart: same automerge actor, not a fresh one per launch.
            assert_eq!(
                restored.get_actor(),
                &original_actor,
                "a reopened store must keep the document's actor identity"
            );

            restored
                .merge_incoming(&mut peer)
                .expect("merge with diverged peer");
            let list = restored.get(&ROOT, "contacts").unwrap().unwrap().1;
            assert_eq!(
                str_at(&restored, &list, "ada").as_deref(),
                Some("Ada Lovelace"),
                "the shared ancestor is still there exactly once"
            );
            assert_eq!(
                str_at(&restored, &list, "alan").as_deref(),
                Some("Alan Turing"),
                "the saved local edit survives the merge"
            );
            assert_eq!(
                str_at(&restored, &list, "grace").as_deref(),
                Some("Grace Hopper"),
                "the peer's concurrent edit merges in"
            );
            // A rebuilt-from-scratch document would have made `contacts` a different
            // object on each side, so the merge would leave two conflicting values.
            // Exactly one is the proof it didn't.
            assert_eq!(
                restored.get_all(&ROOT, "contacts").unwrap().len(),
                1,
                "history was preserved, so both sides edited the same object"
            );
        }
        // Phase `append_is_incremental_and_loads_back`.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.append").unwrap();
            let mut doc = store.new_document();
            let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            doc.splice_text(&body, 0, 0, "hello").unwrap();
            store.save(&mut doc).unwrap();
            let base = fs::metadata(store.document_path()).unwrap().len();

            doc.splice_text(&body, 5, 0, " world").unwrap();
            let appended = store.append(&mut doc).unwrap();
            assert!(appended > 0, "a real edit must produce a delta");
            let grown = fs::metadata(store.document_path()).unwrap().len();
            assert_eq!(
                grown,
                base + appended as u64,
                "append must extend the file, not rewrite it"
            );

            assert_eq!(store.append(&mut doc).unwrap(), 0);
            drop((store, doc));

            let mut reopened = DocStore::open_for("so.tilo.append").unwrap();
            let loaded = reopened.load().unwrap().unwrap();
            let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
            assert_eq!(loaded.text(&body).unwrap(), "hello world");
        }
        // Phase `a_torn_append_loses_only_the_torn_change`.
        // A crash during an append leaves a truncated trailing chunk; everything
        // written before it must still load.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.torn").unwrap();
            let mut doc = store.new_document();
            let body = doc.put_object(&ROOT, "body", ObjType::Text).unwrap();
            doc.splice_text(&body, 0, 0, "committed").unwrap();
            store.save(&mut doc).unwrap();

            doc.splice_text(&body, 9, 0, " and lost").unwrap();
            store.append(&mut doc).unwrap();

            // Simulate the interrupted write: chop the tail of the last chunk.
            let bytes = fs::read(store.document_path()).unwrap();
            fs::write(store.document_path(), &bytes[..bytes.len() - 3]).unwrap();
            drop((store, doc));

            let mut reopened = DocStore::open_for("so.tilo.torn").unwrap();
            let loaded = reopened
                .load()
                .expect("a torn tail must not fail the load")
                .expect("document still present");
            let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
            assert_eq!(
                loaded.text(&body).unwrap(),
                "committed",
                "everything committed before the torn append survives"
            );
        }
        // Phase `an_interrupted_full_save_leaves_the_previous_document_intact`.
        // A full save is a temp-file rename, so an interrupted one can't be
        // observed: the previous document is intact, and the partial bytes sit in
        // a `.tmp` sibling `load` never looks at.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.atomic").unwrap();
            let mut doc = store.new_document();
            doc.put(&ROOT, "v", ScalarValue::Str("first".into()))
                .unwrap();
            store.save(&mut doc).unwrap();

            // The write half of an interrupted save: temp file exists, rename never happened.
            let tmp_path = store.document_path().with_extension("automerge.tmp");
            fs::write(&tmp_path, b"\x85\x6f\x4a\x83truncated garbage").unwrap();
            drop((store, doc));

            let mut reopened = DocStore::open_for("so.tilo.atomic").unwrap();
            let loaded = reopened.load().unwrap().expect("previous document");
            assert_eq!(text_of(&loaded, "v").as_deref(), Some("first"));
        }
        // Phase `a_second_instance_gets_a_distinct_actor`.
        {
            let _tmp = scratch_home();
            let first = DocStore::open_for("so.tilo.twice").unwrap();
            assert!(first.is_exclusive_writer());
            let second = DocStore::open_for("so.tilo.twice").unwrap();
            assert!(
                !second.is_exclusive_writer(),
                "the second live store must not claim the writer lock"
            );
            assert_ne!(
                first.actor(),
                second.actor(),
                "two live writers sharing an actor id would corrupt the change sequence"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn schema_versions_are_stamped_migrated_once_and_newer_documents_refused() {
        // Phase `app_owned_schema_migration_is_durable_and_runs_once`.
        fn app_owned_schema_migration_is_durable_and_runs_once() {
            let _tmp = scratch_home();
            let package = "so.tilo.schema-migration";
            let mut old_store = DocStore::open_for(package).unwrap();
            let mut old = old_store.new_document();
            old.put(ROOT, "legacy", "value").unwrap();
            old_store.save(&mut old).unwrap();
            drop(old_store);

            let mut migrated_store = DocStore::open_for(package).unwrap().with_schema_version(2);
            let migrated = migrated_store
                .load_migrating(|doc, from, to| {
                    assert_eq!((from, to), (1, 2));
                    doc.put(ROOT, "migrated", true)
                        .map_err(|error| error.to_string())?;
                    Ok(())
                })
                .unwrap()
                .unwrap();
            assert_eq!(schema_version_of(&migrated), 2);
            assert!(migrated.get(ROOT, "migrated").unwrap().is_some());
            drop(migrated_store);

            let mut reopened = DocStore::open_for(package).unwrap().with_schema_version(2);
            let loaded = reopened
                .load_migrating(|_, _, _| Err("migration ran twice".into()))
                .unwrap()
                .unwrap();
            assert_eq!(schema_version_of(&loaded), 2);
            assert!(loaded.get(ROOT, "legacy").unwrap().is_some());
            assert!(loaded.get(ROOT, "migrated").unwrap().is_some());
        }
        // Phase `a_new_document_carries_the_schema_version`.
        // Every document this crate creates says which layout it is, from the
        // moment it exists, so a document created and never edited is still
        // distinguishable from one written before versioning.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.stamped").unwrap();
            let mut doc = store.new_document();
            assert_eq!(schema_version_of(&doc), SCHEMA_VERSION);
            store.save(&mut doc).unwrap();

            let mut reopened = DocStore::open_for("so.tilo.stamped").unwrap();
            let loaded = reopened.load().unwrap().expect("saved document");
            assert!(
                loaded.get(ROOT, SCHEMA_VERSION_KEY).unwrap().is_some(),
                "the version has to survive the round trip, not just live in memory"
            );
            assert_eq!(schema_version_of(&loaded), SCHEMA_VERSION);
        }
        // Phase `app_owned_schema_migration_is_durable_and_runs_once` (defined above).
        app_owned_schema_migration_is_durable_and_runs_once();
        // Phase `app_owned_schema_refuses_a_newer_layout`.
        {
            let _tmp = scratch_home();
            let package = "so.tilo.schema-refusal";
            let mut newer = DocStore::open_for(package).unwrap().with_schema_version(3);
            let mut doc = newer.new_document();
            newer.save(&mut doc).unwrap();
            drop(newer);

            let mut older = DocStore::open_for(package).unwrap().with_schema_version(2);
            assert!(matches!(
                older.load(),
                Err(StoreError::NewerSchema {
                    found: 3,
                    understood: 2,
                    ..
                })
            ));
        }
        // Phase `a_document_written_before_versioning_opens_and_is_stamped_on_save`.
        // A document from before versioning existed opens, and gains its version
        // on next write. Reading absence as "unknown" would refuse every existing
        // user's documents on the very upgrade meant to prevent that loss.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.unversioned").unwrap();

            // Written the way the crate used to: a bare AutoCommit, no stamp.
            let mut old = AutoCommit::new();
            old.put(&ROOT, "note", ScalarValue::Str("kept".into()))
                .unwrap();
            assert!(
                old.get(ROOT, SCHEMA_VERSION_KEY).unwrap().is_none(),
                "the fixture must actually be unversioned"
            );
            fs::create_dir_all(store.dir()).unwrap();
            fs::write(store.document_path(), old.save()).unwrap();

            let mut doc = store.load_or_new();
            assert!(
                store.refusal().is_none(),
                "an unversioned document is not refused"
            );
            assert_eq!(
                text_of(&doc, "note").as_deref(),
                Some("kept"),
                "its contents must load"
            );
            store.save(&mut doc).unwrap();

            let mut reopened = DocStore::open_for("so.tilo.unversioned").unwrap();
            let loaded = reopened.load().unwrap().unwrap();
            assert!(
                loaded.get(ROOT, SCHEMA_VERSION_KEY).unwrap().is_some(),
                "saving must stamp a document that had no version, so a later build \
                 is not left guessing"
            );
            assert_eq!(text_of(&loaded, "note").as_deref(), Some("kept"));
        }
        // Phase `a_newer_document_is_refused_and_never_written_over`.
        // The one this exists for: an older build meets a document a newer one
        // wrote. It must refuse and keep the bytes — writing over them is what
        // turns a survivable empty session into data loss.
        {
            let _tmp = scratch_home();
            let mut store = DocStore::open_for("so.tilo.future").unwrap();

            let mut future = AutoCommit::new();
            future
                .put(
                    &ROOT,
                    "irreplaceable",
                    ScalarValue::Str("please keep".into()),
                )
                .unwrap();
            future
                .put(&ROOT, SCHEMA_VERSION_KEY, u64::from(SCHEMA_VERSION) + 1)
                .unwrap();
            fs::create_dir_all(store.dir()).unwrap();
            let on_disk = future.save();
            fs::write(store.document_path(), &on_disk).unwrap();

            // load() names the situation rather than calling it corruption.
            match store.load() {
                Err(StoreError::NewerSchema {
                    found, understood, ..
                }) => {
                    assert_eq!(found, SCHEMA_VERSION + 1);
                    assert_eq!(understood, SCHEMA_VERSION);
                }
                other => panic!("expected NewerSchema, got {other:?}"),
            }

            // load_or_new comes up empty and records why.
            let mut doc = store.load_or_new();
            assert!(
                text_of(&doc, "irreplaceable").is_none(),
                "a refused document must not be loaded"
            );
            let reason = store
                .refusal()
                .expect("the refusal has to be visible, or nothing downstream can act on it");
            assert!(
                reason.contains(&(SCHEMA_VERSION + 1).to_string()),
                "the reason must name the layout found, got: {reason}"
            );

            // And every write path refuses, so the bytes survive an app that carries on regardless.
            doc.put(&ROOT, "scribble", ScalarValue::Str("no".into()))
                .unwrap();
            assert!(
                matches!(store.save(&mut doc), Err(StoreError::NewerSchema { .. })),
                "save over a refused document must fail, not succeed quietly"
            );
            assert!(
                matches!(store.append(&mut doc), Err(StoreError::NewerSchema { .. })),
                "append over a refused document must fail too — it writes to the \
                 same file"
            );
            assert_eq!(
                fs::read(store.document_path()).unwrap(),
                on_disk,
                "the document on disk must be byte-for-byte what it was"
            );
        }
    }

    #[test]
    #[serial_test::serial]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn opening_over_existing_state_keeps_persistence() {
        // Phase `opening_twice_against_an_existing_dir_keeps_persistence_enabled`.
        // If the directory create were not idempotent, the second open would fail
        // EEXIST and swallow into an in-memory fallback, silently losing state
        // after the first launch. Pre-creates the directory before the *first*
        // open too, since fresh-XDG CI never sees an existing one otherwise.
        {
            let _tmp = scratch_home();
            let pkg = "so.tilo.persist-twice";

            let dir = cli::standalone_state_dir(pkg);
            fs::create_dir_all(&dir).unwrap();
            assert!(
                dir.is_dir(),
                "the state directory pre-exists for both opens"
            );

            // `seed` runs on the freshly loaded document, exactly as
            // `ui::persist::open` sequences it, so the later edit reads as a change.
            let mut run1 =
                DocStore::open_for(pkg).expect("first open must succeed when the dir exists");
            let mut doc = run1.load_or_new();
            assert!(run1.refusal().is_none(), "a fresh store is not refused");
            let mut autosave1 = run1.writer();
            assert!(autosave1.is_enabled(), "run 1 must persist, not fall back");
            autosave1.seed(&mut doc);
            doc.put(&ROOT, "note", ScalarValue::Str("survives".into()))
                .unwrap();
            autosave1.request(&mut doc);
            autosave1.flush();
            assert_eq!(autosave1.last_error(), None, "no write may have failed");
            drop(autosave1);

            let run2 = DocStore::open_for(pkg);
            assert!(
                run2.is_ok(),
                "second open against the existing dir must succeed, got {:?}",
                run2.err()
            );
            let mut run2 = run2.unwrap();
            let mut doc2 = run2.load_or_new();
            assert_eq!(
                text_of(&doc2, "note").as_deref(),
                Some("survives"),
                "state written in run 1 must round-trip across the re-open"
            );
            let autosave2 = run2.writer();
            assert!(
                autosave2.is_enabled(),
                "the SECOND run must persist too — this is the bug: a non-idempotent \
                 directory create disabled persistence for every launch after the first"
            );
            doc2.put(&ROOT, "note", ScalarValue::Str("again".into()))
                .unwrap();
            autosave2.flush();
        }
    }
}
