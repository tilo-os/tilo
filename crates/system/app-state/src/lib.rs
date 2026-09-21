//! Canonical on-disk home for a Tilo app's Automerge document: one file per
//! package at `$XDG_DATA_HOME/<package-id>/state.automerge`, atomic writes,
//! a debounced background writer. The path derives only from
//! `TILO_PACKAGE_ID`, validated so an app can't address another app's
//! storage or the shared workspace FUSE root.
//!
//! [`Autosave`] appends `save_after(persisted_heads)` (linear write volume,
//! not quadratic) and round-trips the document's own bytes rather than a
//! rebuilt `AutoCommit`, so saves stay mergeable with a diverged peer. The
//! actor id lives in a sidecar for restart continuity; [`DocStore::open`]
//! takes an exclusive `flock` so two live instances never share one.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, LoadOptions, OnPartialLoad, ROOT, ReadDoc};

mod autosave;
pub use autosave::{Autosave, DEFAULT_DEBOUNCE, DurabilitySnapshot};

pub const DOCUMENT_FILE: &str = "state.automerge";
pub const ACTOR_FILE: &str = "actor";
const CANONICAL_GATE: &str = "canonical.lock";
pub const LOCK_FILE: &str = "writer.lock";
/// Subdirectory holding one directory per record. See [`DocStore::open_record`].
pub const RECORDS_DIR: &str = "records";
/// Subdirectory holding one directory per **room** — the index of records
/// that room holds. See [`DocStore::open_room`].
pub const ROOMS_DIR: &str = "rooms";
/// Durable claim files prevent a pre-room document from being silently
/// imported into every room that happens to open after an upgrade.
const LEGACY_PACKAGE_ROOM_CLAIM: &str = ".tilo-legacy-room-index-claim";
const LEGACY_RECORD_ROOM_CLAIM: &str = ".tilo-legacy-room-record-claim";

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
    match doc.get(ROOT, SCHEMA_VERSION_KEY) {
        Ok(Some((v, _))) => v
            .to_u64()
            .or_else(|| v.to_i64().and_then(|i| u64::try_from(i).ok()))
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(SCHEMA_VERSION),
        _ => SCHEMA_VERSION,
    }
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

/// The persistent store for one package's Automerge document. Construct
/// with [`DocStore::open`], then drive by hand ([`DocStore::load_or_new`] +
/// [`DocStore::save`]) or hand to [`DocStore::autosave`] for debounced writes.
pub struct DocStore {
    package_id: String,
    dir: PathBuf,
    document: PathBuf,
    actor: ActorId,
    /// App/document-specific layout understood by this caller. Constructors
    /// default to the toolkit's legacy v1 contract; apps opt into a newer
    /// schema before loading via [`Self::with_schema_version`].
    schema_version: u32,
    /// Held for as long as the store lives; `None` when another instance already owns the lock.
    lock: Option<File>,
    /// Heads the file on disk is known to contain; empty until the first load or save.
    persisted: Vec<ChangeHash>,
    /// Why [`DocStore::load_or_new`] handed back an empty document instead of
    /// the one on disk — set only for a refusal that must not be written over;
    /// e.g. [`Autosave`] switches itself off when this is set.
    refusal: Option<Refusal>,
    // Shared for legacy stores, exclusive for canonical owners; acquired before the actor.
    gate: File,
    canonical: Option<CanonicalState>,
    writer_claimed: AtomicBool,
    #[cfg(test)]
    faults: std::sync::Arc<TestFaults>,
}

#[cfg(test)]
#[derive(Default)]
struct TestFaults {
    spawn_failures: std::sync::atomic::AtomicUsize,
    active_workers: std::sync::atomic::AtomicUsize,
    max_workers: std::sync::atomic::AtomicUsize,
    append_prefix: std::sync::Mutex<Option<usize>>,
    sync_failure: std::sync::Mutex<Option<PathBuf>>,
    sync_visits: std::sync::Mutex<Vec<PathBuf>>,
    completion:
        std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}

struct CanonicalState {
    directory: File,
    identity: std::sync::Arc<()>,
    accepted: bool,
}

/// A validated decoding candidate. No recovered bytes are published until acceptance.
pub struct CanonicalLoad {
    doc: AutoCommit,
    source_bytes: Vec<u8>,
    repair_bytes: Option<Vec<u8>>,
    identity: std::sync::Arc<()>,
}

impl CanonicalLoad {
    pub fn document(&self) -> &AutoCommit {
        &self.doc
    }
}

/// The document on disk, and why this build would not open it.
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
    /// name (see [`validate_package_id`]), or if the package's state
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
    /// name (see [`validate_package_id`]), or if the package's state
    /// directory can't be created, inspected or locked.
    pub fn open_with_manifest_id(manifest_id: &str) -> Result<Self, StoreError> {
        let id = std::env::var("TILO_PACKAGE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| manifest_id.to_string());
        Self::open_for(&id)
    }

    /// Open the store for an explicit package id — for tests and migration
    /// tools; rejects any id that is not a single, non-traversing directory name.
    ///
    /// # Errors
    ///
    /// Returns an error if `package_id` fails [`validate_package_id`], or if
    /// the package's state directory can't be created, inspected or locked.
    pub fn open_for(package_id: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        let dir = cli::standalone_state_dir(package_id);
        Self::open_in(package_id, dir)
    }

    /// Open the store for **one record** of a package. A record is the unit of
    /// collaboration: one document, one identity, one change graph — Automerge
    /// changes name dependencies by hash, so a change graph can't be served in
    /// part, meaning separate files per record. `record_key` is validated as
    /// strictly as a package id. Lands in
    /// `<package dir>/records/<record_key>/state.automerge`, beside the
    /// package's own `state.automerge`.
    ///
    /// # Errors
    ///
    /// Returns an error if `package_id` fails [`validate_package_id`] or
    /// `record_key` fails [`validate_record_key`], or if the record's state
    /// directory can't be created, inspected or locked.
    pub fn open_record(package_id: &str, record_key: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(record_key)?;
        let dir = cli::standalone_state_dir(package_id)
            .join(RECORDS_DIR)
            .join(record_key);
        Self::open_in(package_id, dir)
    }

    /// Open one record in one collaboration room.  A record id is only
    /// unique *within* a room: a hostile or stale peer must not be able to
    /// make `uuid:...` in room B resolve to room A's on-disk change graph.
    ///
    /// This deliberately does not remove the pre-room
    /// `<package>/records/<record>` location.  Callers may merge that legacy
    /// document on first open, then persist the merged result here; retaining
    /// the source makes an interrupted migration recoverable.
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
        validate_package_id(package_id)?;
        validate_record_key(room_key)?;
        validate_record_key(record_key)?;
        let dir = cli::standalone_state_dir(package_id)
            .join(ROOMS_DIR)
            .join(room_key)
            .join(RECORDS_DIR)
            .join(record_key);
        Self::open_in(package_id, dir)
    }

    /// The old package-global record document path.  Migration code uses this
    /// only as a read source; it is never deleted or renamed automatically.
    ///
    /// # Errors
    ///
    /// Returns an error when either identifier is invalid.
    pub fn legacy_record_document_path(
        package_id: &str,
        record_key: &str,
    ) -> Result<PathBuf, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(record_key)?;
        Ok(cli::standalone_state_dir(package_id)
            .join(RECORDS_DIR)
            .join(record_key)
            .join(DOCUMENT_FILE))
    }

    /// The old package-global document path, retained as a safe migration
    /// source when a package document becomes a room index.
    ///
    /// # Errors
    ///
    /// Returns an error when `package_id` is invalid.
    pub fn legacy_package_document_path(package_id: &str) -> Result<PathBuf, StoreError> {
        validate_package_id(package_id)?;
        Ok(cli::standalone_state_dir(package_id).join(DOCUMENT_FILE))
    }

    /// Claim the legacy package-global document for `room_key`.  The claim is
    /// an atomic create-next-to-source operation: exactly one room can import
    /// it, while that same room can resume after a crash before its first
    /// scoped write. `false` means another room already owns the migration.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers, malformed durable claim
    /// state, or an I/O failure while publishing the claim.
    pub fn claim_legacy_package_migration(
        package_id: &str,
        room_key: &str,
    ) -> Result<bool, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(room_key)?;
        claim_legacy_room_migration(
            &cli::standalone_state_dir(package_id).join(LEGACY_PACKAGE_ROOM_CLAIM),
            room_key,
        )
    }

    /// The record-level twin of [`Self::claim_legacy_package_migration`].
    /// Keeping the marker beside the retained legacy record lets recovery
    /// reason about one source document without any global registry.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identifiers, malformed durable claim
    /// state, or an I/O failure while publishing the claim.
    pub fn claim_legacy_record_migration(
        package_id: &str,
        room_key: &str,
        record_key: &str,
    ) -> Result<bool, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(room_key)?;
        validate_record_key(record_key)?;
        claim_legacy_room_migration(
            &cli::standalone_state_dir(package_id)
                .join(RECORDS_DIR)
                .join(record_key)
                .join(LEGACY_RECORD_ROOM_CLAIM),
            room_key,
        )
    }

    /// Open the index of **one room** — the list of which records that room
    /// holds. A sibling directory, not the record itself, so a room id can
    /// never resolve to a record's document and merge the two. `room_key` is
    /// validated as strictly as a record key. Lands in
    /// `<package dir>/rooms/<room_key>/state.automerge`.
    ///
    /// # Errors
    ///
    /// Returns an error if `package_id` fails [`validate_package_id`] or
    /// `room_key` fails [`validate_record_key`], or if the room's state
    /// directory can't be created, inspected or locked.
    pub fn open_room(package_id: &str, room_key: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(room_key)?;
        let dir = cli::standalone_state_dir(package_id)
            .join(ROOMS_DIR)
            .join(room_key);
        Self::open_in(package_id, dir)
    }

    fn open_in(package_id: &str, dir: PathBuf) -> Result<Self, StoreError> {
        // Recovers a pre-existing dir or an older layout's stale file; see `ensure_state_dir`.
        ensure_state_dir(&dir)?;

        let gate = acquire_gate(&dir.join(CANONICAL_GATE), false)?;
        let lock = acquire_writer_lock(&dir.join(LOCK_FILE));
        let actor = if lock.is_some() {
            load_or_create_actor(&dir.join(ACTOR_FILE))?
        } else {
            tracing::warn!(
                package = package_id,
                "another instance holds the state lock; running on a fresh automerge actor"
            );
            ActorId::random()
        };

        Ok(Self {
            package_id: package_id.to_string(),
            document: dir.join(DOCUMENT_FILE),
            dir,
            actor,
            schema_version: SCHEMA_VERSION,
            lock,
            persisted: Vec::new(),
            refusal: None,
            gate,
            canonical: None,
            writer_claimed: AtomicBool::new(false),
            #[cfg(test)]
            faults: std::sync::Arc::default(),
        })
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

    /// Open an exclusive canonical owner. Never adopts stale files or falls back to memory.
    /// The ownership gate is cooperative: older binaries that do not acquire it can still write,
    /// so concurrent use of one profile with those binaries is unsupported.
    ///
    /// # Errors
    /// Rejects invalid paths, contested gates and every filesystem durability failure.
    pub fn open_canonical_at(package_id: &str, directory: &Path) -> Result<Self, StoreError> {
        Self::open_canonical_with(package_id, directory, |_| {})
    }

    fn open_canonical_with(
        package_id: &str,
        directory: &Path,
        configure: impl FnOnce(&mut Self),
    ) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        validate_absolute_directory(directory)?;
        fs::create_dir_all(directory).map_err(|e| io_err("creating canonical directory", e))?;
        let opened = open_directory(directory, false)
            .map_err(|e| io_err("opening canonical directory", e))?;
        let gate = acquire_gate(&directory.join(CANONICAL_GATE), true)?;
        let actor = load_or_create_actor(&directory.join(ACTOR_FILE))?;
        let mut store = Self {
            package_id: package_id.to_owned(),
            dir: directory.to_path_buf(),
            document: directory.join(DOCUMENT_FILE),
            actor,
            schema_version: SCHEMA_VERSION,
            lock: None,
            persisted: Vec::new(),
            refusal: None,
            gate,
            canonical: Some(CanonicalState {
                directory: opened,
                identity: std::sync::Arc::new(()),
                accepted: false,
            }),
            writer_claimed: AtomicBool::new(false),
            #[cfg(test)]
            faults: std::sync::Arc::default(),
        };
        configure(&mut store);
        store.check_canonical_source()?;
        store.sync_directory_chain()?;
        Ok(store)
    }

    fn canonical_state(&self) -> Result<&CanonicalState, StoreError> {
        self.canonical.as_ref().ok_or_else(|| {
            io_err(
                "canonical mode required",
                io::Error::from(io::ErrorKind::InvalidInput),
            )
        })
    }

    /// Build a private current-schema candidate without publishing bytes or resetting heads.
    ///
    /// # Errors
    /// Rejects legacy mode and propagates schema-stamp errors.
    pub fn new_canonical_document(&self) -> Result<AutoCommit, StoreError> {
        self.canonical_state()?;
        let mut doc = AutoCommit::new().with_actor(self.actor.clone());
        doc.put(ROOT, SCHEMA_VERSION_KEY, u64::from(SCHEMA_VERSION))
            .map_err(|e| self.automerge_error(e))?;
        Ok(doc)
    }

    fn automerge_error(&self, source: automerge::AutomergeError) -> StoreError {
        StoreError::Automerge {
            path: self.document.clone(),
            source: Box::new(source),
        }
    }

    /// Validate every live schema assignment without coercion or mutation.
    ///
    /// # Errors
    /// Rejects legacy mode, malformed/conflicting schemas and newer schemas.
    pub fn validate_canonical_schema(&self, doc: &AutoCommit) -> Result<(), StoreError> {
        self.canonical_state()?;
        let invalid = |reason| StoreError::InvalidSchema {
            path: self.document.clone(),
            reason,
        };
        let mut versions = Vec::new();
        for (value, _) in doc
            .get_all(ROOT, SCHEMA_VERSION_KEY)
            .map_err(|e| self.automerge_error(e))?
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
                path: self.document.clone(),
                found,
                understood: SCHEMA_VERSION,
            });
        }
        if versions.iter().any(|v| Some(v) != versions.first()) {
            return Err(invalid("conflicting schema versions"));
        }
        Ok(())
    }

    /// Decode an existing file, retaining any repair privately for identity validation.
    ///
    /// # Errors
    /// Refuses empty, corrupt, changed or unsupported documents without publishing a repair.
    pub fn load_canonical(&mut self) -> Result<Option<CanonicalLoad>, StoreError> {
        self.check_canonical_source()?;
        let source_bytes = match fs::read(&self.document) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err("reading canonical document", e)),
        };
        if source_bytes.is_empty() {
            return Err(StoreError::EmptyExistingDocument {
                path: self.document.clone(),
            });
        }
        let (mut doc, repair) = match AutoCommit::load_with_options(
            &source_bytes,
            LoadOptions::new().on_partial_load(OnPartialLoad::Error),
        ) {
            Ok(doc) => (doc, false),
            Err(_) => (
                AutoCommit::load_with_options(
                    &source_bytes,
                    LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
                )
                .map_err(|e| self.automerge_error(e))?,
                true,
            ),
        };
        self.validate_canonical_schema(&doc)?;
        let repair_bytes = repair.then(|| doc.save());
        Ok(Some(CanonicalLoad {
            doc,
            source_bytes,
            repair_bytes,
            identity: self.canonical_state()?.identity.clone(),
        }))
    }

    /// Accept this owner's exact validated candidate and acknowledge its durable on-disk heads.
    ///
    /// # Errors
    /// Refuses a changed source/owner or any publication/file/directory synchronization failure.
    pub fn accept_canonical(
        &mut self,
        mut loaded: CanonicalLoad,
    ) -> Result<AutoCommit, StoreError> {
        self.check_canonical_source()?;
        if !std::sync::Arc::ptr_eq(&loaded.identity, &self.canonical_state()?.identity)
            || fs::read(&self.document).ok().as_ref() != Some(&loaded.source_bytes)
        {
            return Err(StoreError::SourceChanged {
                path: self.document.clone(),
            });
        }
        self.validate_canonical_schema(&loaded.doc)?;
        if let Some(bytes) = loaded.repair_bytes {
            self.write_full(&bytes)?;
        } else {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            options.write(true);
            options
                .open(&self.document)
                .and_then(|f| f.sync_all())
                .map_err(|e| io_err("syncing accepted document", e))?;
            self.sync_directory_chain()?;
        }
        loaded.doc.set_actor(self.actor.clone());
        self.persisted = loaded.doc.get_heads();
        if let Some(state) = self.canonical.as_mut() {
            state.accepted = true;
        }
        Ok(loaded.doc)
    }

    fn check_canonical_source(&self) -> Result<(), StoreError> {
        let state = self.canonical_state()?;
        for (opened, path) in [
            (&state.directory, self.dir.clone()),
            (&self.gate, self.dir.join(CANONICAL_GATE)),
        ] {
            if !same_file(opened, &path) {
                return Err(StoreError::SourceChanged { path });
            }
        }
        Ok(())
    }

    fn sync_directory_chain(&self) -> Result<(), StoreError> {
        self.check_canonical_source()?;
        let physical =
            fs::canonicalize(&self.dir).map_err(|e| io_err("resolving canonical directory", e))?;
        // Windows owns and provisions the profile root before launching an
        // app. Our durability obligation includes that root and every child
        // directory, including ancestors left by an interrupted earlier open.
        // It does not include creating the OS-owned C:\Users or volume root.
        // Query the OS known folder, not USERPROFILE supplied by an environment.
        #[cfg(windows)]
        let profile_root = dirs::home_dir().and_then(|path| fs::canonicalize(path).ok());
        for path in physical.ancestors() {
            #[cfg(test)]
            {
                self.faults
                    .sync_visits
                    .lock()
                    .unwrap()
                    .push(path.to_path_buf());
                if self.faults.sync_failure.lock().unwrap().as_deref() == Some(path) {
                    return Err(io_err(
                        "injected directory sync failure",
                        io::Error::from(io::ErrorKind::Unsupported),
                    ));
                }
            }
            open_directory(path, true)
                .and_then(|f| f.sync_all())
                .map_err(|e| io_err(format!("syncing directory {}", path.display()), e))?;
            #[cfg(windows)]
            if profile_root.as_deref() == Some(path) {
                break;
            }
        }
        self.check_canonical_source()
    }

    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Select the app-owned CRDT layout this store reads and writes. Call
    /// this before `load`, `load_or_new`, or creating an autosave handle.
    #[must_use]
    pub fn with_schema_version(mut self, version: u32) -> Self {
        self.schema_version = version;
        self
    }

    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The directory holding the document, its actor sidecar and its lock.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn document_path(&self) -> &Path {
        &self.document
    }

    /// The Automerge actor this store writes as.
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    /// Whether this process won the single-writer lock. `false` means another
    /// instance is live: writes still succeed (last writer wins, merged on next
    /// load) but the actor id is fresh rather than the sidecar's.
    pub fn is_exclusive_writer(&self) -> bool {
        self.canonical.is_some() || self.lock.is_some()
    }

    /// Load the persisted document, or `Ok(None)` on a first launch. A trailing
    /// partially-written chunk (crash during append) is dropped rather than
    /// failing the load, losing only the last unflushed edit.
    ///
    /// # Errors
    ///
    /// Returns an error if the document file exists but can't be read, if its
    /// bytes aren't a loadable Automerge document, or if it declares a schema
    /// layout newer than this build understands ([`StoreError::NewerSchema`]).
    pub fn load(&mut self) -> Result<Option<AutoCommit>, StoreError> {
        let bytes = match fs::read(&self.document) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(io_err(format!("reading {}", self.document.display()), e));
            }
        };
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut doc = AutoCommit::load_with_options(
            &bytes,
            LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
        )
        .map_err(|source| StoreError::Automerge {
            path: self.document.clone(),
            source: Box::new(source),
        })?;
        // Check before reading any value: a newer layout may reuse a key with a
        // different meaning, and the writer above saves the whole document back.
        let found = schema_version_of(&doc);
        if found > self.schema_version {
            return Err(StoreError::NewerSchema {
                path: self.document.clone(),
                found,
                understood: self.schema_version,
            });
        }
        doc.set_actor(self.actor.clone());
        self.persisted = doc.get_heads();
        Ok(Some(doc))
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
            path: self.document.clone(),
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
        let mut doc = AutoCommit::new().with_actor(self.actor.clone());
        stamp_schema_version(&mut doc, self.schema_version);
        doc
    }

    /// Rewrite the whole document atomically: temp file, `fsync`, rename — a
    /// crash at any point leaves either the previous or the new file complete,
    /// never half-written. Refuses when [`DocStore::refusal`] is set, since the
    /// in-memory document is an empty stand-in and writing it out would delete
    /// the user's actual data.
    ///
    /// # Errors
    ///
    /// Returns an error if [`DocStore::refusal`] is set (writing would
    /// discard a newer-schema document already on disk), or if the file
    /// can't be written.
    pub fn save(&mut self, doc: &mut AutoCommit) -> Result<(), StoreError> {
        self.refuse_to_write()?;
        ensure_schema_version(doc, self.schema_version);
        let bytes = doc.save();
        self.write_full(&bytes)?;
        self.persisted = doc.get_heads();
        Ok(())
    }

    /// The guard both write paths share; returns the original refusal so a
    /// caller can log why, not just "write failed".
    fn refuse_to_write(&self) -> Result<(), StoreError> {
        match &self.refusal {
            None => Ok(()),
            Some(r) => Err(StoreError::NewerSchema {
                path: self.document.clone(),
                found: r.found,
                understood: self.schema_version,
            }),
        }
    }

    /// Append only the changes `doc` has that the file does not. Returns bytes
    /// appended (`0` when idle). Refuses for the same data-loss reason as
    /// [`DocStore::save`].
    ///
    /// # Errors
    ///
    /// Returns an error if [`DocStore::refusal`] is set, or if the file
    /// can't be written.
    pub fn append(&mut self, doc: &mut AutoCommit) -> Result<usize, StoreError> {
        self.refuse_to_write()?;
        ensure_schema_version(doc, self.schema_version);
        let heads = doc.get_heads();
        if heads == self.persisted {
            return Ok(0);
        }
        // No base file yet: seed a full save (compresses far better than raw change
        // chunks with no document chunk in front).
        if !self.document.exists() {
            self.save(doc)?;
            return Ok(0);
        }
        let delta = doc.save_after(&self.persisted);
        if delta.is_empty() {
            self.persisted = heads;
            return Ok(0);
        }
        self.append_bytes(&delta)?;
        self.persisted = heads;
        Ok(delta.len())
    }

    /// Adopt a document an older release wrote elsewhere, merging into the
    /// store's document (not replacing) so a repeated or later migration can't
    /// lose data. Returns `true` if anything was imported; leaves the legacy
    /// file alone.
    ///
    /// # Errors
    ///
    /// Returns an error if `legacy` exists but can't be read, its bytes
    /// aren't a loadable Automerge document, the merge fails, or the
    /// resulting save fails.
    pub fn migrate_from(&mut self, legacy: &Path) -> Result<bool, StoreError> {
        let bytes = match fs::read(legacy) {
            Ok(b) if !b.is_empty() => b,
            Ok(_) => return Ok(false),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io_err(format!("reading {}", legacy.display()), e)),
        };
        let mut incoming = AutoCommit::load_with_options(
            &bytes,
            LoadOptions::new().on_partial_load(OnPartialLoad::Ignore),
        )
        .map_err(|source| StoreError::Automerge {
            path: legacy.to_path_buf(),
            source: Box::new(source),
        })?;

        let mut current = match self.load()? {
            Some(doc) => doc,
            None => self.new_document(),
        };
        current
            .merge(&mut incoming)
            .map_err(|source| StoreError::Automerge {
                path: legacy.to_path_buf(),
                source: Box::new(source),
            })?;
        self.save(&mut current)?;
        Ok(true)
    }

    /// Hand the store to a background thread that coalesces a burst of edits
    /// into one write. See [`Autosave`].
    pub fn autosave(self, debounce: std::time::Duration) -> Autosave {
        Autosave::spawn(self, debounce)
    }

    /// Extend the document file with raw change chunks; caller must ensure the
    /// chunks aren't already in the file (both [`DocStore::append`] and
    /// [`Autosave`] derive them from `save_after`).
    pub(crate) fn append_bytes(&self, delta: &[u8]) -> Result<(), StoreError> {
        if delta.is_empty() {
            return Ok(());
        }
        if self.canonical.is_some() {
            self.check_canonical_source()?;
        }
        let mut f = OpenOptions::new()
            .create(self.canonical.is_none())
            .append(true)
            .open(&self.document)
            .map_err(|e| io_err(format!("appending to {}", self.document.display()), e))?;
        #[cfg(test)]
        let injected_prefix = self.faults.append_prefix.lock().unwrap().take();
        #[cfg(test)]
        if let Some(prefix) = injected_prefix {
            f.write_all(&delta[..prefix.min(delta.len())]).unwrap();
            f.sync_all().unwrap();
            return Err(io_err(
                "injected append failure after prefix",
                io::Error::from(io::ErrorKind::WriteZero),
            ));
        }
        f.write_all(delta)
            .map_err(|e| io_err(format!("appending to {}", self.document.display()), e))?;
        f.sync_all()
            .map_err(|e| io_err(format!("syncing {}", self.document.display()), e))?;
        Ok(())
    }

    pub(crate) fn write_full(&self, bytes: &[u8]) -> Result<(), StoreError> {
        if self.canonical.is_some() {
            self.check_canonical_source()?;
        } else {
            fs::create_dir_all(&self.dir)
                .map_err(|e| io_err(format!("creating {}", self.dir.display()), e))?;
        }
        atomic_file::write(&self.document, bytes)
            .map_err(|e| io_err(format!("writing {}", self.document.display()), e))?;
        if self.canonical.is_some() {
            self.sync_directory_chain()?;
        }
        #[cfg(test)]
        let completion = self.faults.completion.lock().unwrap().take();
        #[cfg(test)]
        if let Some((reached, resume)) = completion {
            reached.send(()).unwrap();
            resume.recv().unwrap();
        }
        Ok(())
    }
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

#[cfg(unix)]
fn acquire_gate(path: &Path, exclusive: bool) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io_err(format!("opening gate {}", path.display()), e))?;
    let mode = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    // SAFETY: the owned file descriptor remains live for this call and the returned lease.
    if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Err(StoreError::WriterBusy {
                path: path.to_path_buf(),
            });
        }
        return Err(io_err("acquiring canonical gate", error));
    }
    Ok(file)
}

#[cfg(windows)]
fn acquire_gate(path: &Path, exclusive: bool) -> Result<File, StoreError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io_err(format!("opening gate {}", path.display()), e))?;
    let result = if exclusive {
        file.try_lock()
    } else {
        file.try_lock_shared()
    };
    match result {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(StoreError::WriterBusy {
            path: path.to_path_buf(),
        }),
        Err(std::fs::TryLockError::Error(error)) => Err(io_err("acquiring canonical gate", error)),
    }
}

#[cfg(not(any(unix, windows)))]
fn acquire_gate(path: &Path, exclusive: bool) -> Result<File, StoreError> {
    if exclusive {
        return Err(io_err(
            "canonical gate unsupported",
            io::Error::from(io::ErrorKind::Unsupported),
        ));
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io_err("opening gate", e))
}

/// Atomically assign a legacy source to one room.  A marker is deliberately
/// retained forever: deleting it after a successful import would let a later
/// room re-import the still-preserved source, while deleting it after a crash
/// would make recovery non-deterministic.
fn claim_legacy_room_migration(marker: &Path, room_key: &str) -> Result<bool, StoreError> {
    match read_claim(marker)? {
        ClaimState::Owner(owner) => return Ok(owner == room_key),
        ClaimState::Malformed => {
            // Never overwrite a malformed old marker: a separate recovery
            // name lets one contender claim atomically without a stale reader
            // deleting another contender's valid replacement.
            let recovered = marker.with_extension("recovered");
            return publish_claim(&recovered, room_key);
        }
        ClaimState::Absent => {}
    }
    publish_claim(marker, room_key)
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

/// Make `dir` an existing, writable directory, recovering from a state path
/// a previous layout left as a *file*. `create_dir_all` fails EEXIST there,
/// which the caller swallows into a disabled in-memory session — so without
/// recovery, an app that ever ran the old layout would lose persistence on
/// every launch. A zero-byte leftover is removed; a non-empty one (possibly
/// an old-format document) is moved aside to `<dir>.stale` — never deleted —
/// and best-effort seeded as the new document so it migrates in. A genuine
/// filesystem failure still surfaces as an error.
fn ensure_state_dir(dir: &Path) -> Result<(), StoreError> {
    match fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => return Ok(()),
        // Exists, but not as a directory — the old-layout leftover.
        Ok(meta) => {
            if meta.len() == 0 {
                fs::remove_file(dir).map_err(|e| {
                    io_err(format!("removing stale state file {}", dir.display()), e)
                })?;
            } else {
                let aside = stale_sibling(dir);
                fs::rename(dir, &aside).map_err(|e| {
                    io_err(
                        format!(
                            "moving aside stale state file {} to {}",
                            dir.display(),
                            aside.display()
                        ),
                        e,
                    )
                })?;
                fs::create_dir_all(dir).map_err(|e| {
                    io_err(format!("creating state directory {}", dir.display()), e)
                })?;
                // Best effort: a genuine old-format document migrates in on the next load;
                // anything else leaves the store empty while the bytes stay safe at `<dir>.stale`.
                let _ = fs::copy(&aside, dir.join(DOCUMENT_FILE));
                return Ok(());
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        // A real failure inspecting the path (e.g. a non-directory parent).
        Err(e) => {
            return Err(io_err(
                format!("inspecting state path {}", dir.display()),
                e,
            ));
        }
    }
    fs::create_dir_all(dir)
        .map_err(|e| io_err(format!("creating state directory {}", dir.display()), e))
}

/// A sibling path `<dir>.stale`, `.stale.1`, `.stale.2`, … — the first that
/// doesn't exist yet, so preserving one leftover never clobbers another.
fn stale_sibling(dir: &Path) -> PathBuf {
    // Append rather than replace the extension: package ids contain dots
    // (`so.tilo.maps`), so `with_extension` would eat the last id segment.
    let mut base = dir.as_os_str().to_owned();
    base.push(".stale");
    let mut candidate = PathBuf::from(&base);
    let mut n = 0u32;
    while candidate.exists() {
        n = n.saturating_add(1);
        let mut next = base.clone();
        next.push(format!(".{n}"));
        candidate = PathBuf::from(next);
    }
    candidate
}

/// Take an exclusive, non-blocking `flock` on `path`. `None` when another
/// process holds it, or (conservatively) when the lock file can't even be created.
#[cfg(unix)]
fn acquire_writer_lock(path: &Path) -> Option<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    // SAFETY: `file` owns a live fd for the duration of the call.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (rc == 0).then_some(file)
}

/// Windows file sharing is fixed when the handle is opened. Denying every
/// share mode gives this live `File` the same process-lifetime exclusivity as
/// `flock`; closing it releases the lock even after a crash.
#[cfg(windows)]
fn acquire_writer_lock(path: &Path) -> Option<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .ok()
}

#[cfg(not(any(unix, windows)))]
fn acquire_writer_lock(_path: &Path) -> Option<File> {
    None
}

/// Read the actor sidecar, creating a fresh identity the first time.
fn load_or_create_actor(path: &Path) -> Result<ActorId, StoreError> {
    match fs::read_to_string(path) {
        Ok(s) => {
            if let Ok(actor) = ActorId::try_from(s.trim()) {
                return Ok(actor);
            }
            tracing::warn!(
                path = %path.display(),
                "actor sidecar is unreadable; minting a new automerge actor"
            );
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_err(format!("reading {}", path.display()), e)),
    }
    let actor = ActorId::random();
    // Atomic so a crash here can't leave a truncated hex string that would
    // silently downgrade every future launch to a new identity.
    atomic_file::write(path, actor.to_hex_string().as_bytes())
        .map_err(|e| io_err(format!("writing {}", path.display()), e))?;
    Ok(actor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ObjType, ROOT, ReadDoc, ScalarValue};
    use tempfile::TempDir;

    /// Point `standalone_state_dir` at a scratch tree. Tests that use this
    /// are `#[serial]` because it mutates process-global environment.
    fn scratch_home() -> TempDir {
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
        }
        tmp
    }

    #[test]
    fn canonical_lock_child() {
        let Some(path) = std::env::var_os("TILO_CANONICAL_TEST_DIR") else {
            return;
        };
        let path = PathBuf::from(path);
        let canonical = std::env::var_os("TILO_CANONICAL_TEST_MODE").is_some();
        let mut store = if canonical {
            DocStore::open_canonical_at("so.tilo.test", &path)
        } else {
            DocStore::open_in("so.tilo.test", path)
        }
        .unwrap();
        let mut doc = store.new_document();
        doc.put(ROOT, "held", true).unwrap();
        store.save(&mut doc).unwrap();
        println!("CANONICAL_GATE_READY");
        std::io::stdout().flush().unwrap();
        let mut release = String::new();
        std::io::stdin().read_line(&mut release).unwrap();
        drop(store);
    }

    #[test]
    fn canonical_cross_process_gate_matrix_and_release() {
        use std::io::BufRead;
        for first_canonical in [false, true] {
            for second_canonical in [false, true] {
                for terminate in [false, true] {
                    let tmp = TempDir::new().unwrap();
                    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                    command
                        .args(["--exact", "tests::canonical_lock_child", "--nocapture"])
                        .env("TILO_CANONICAL_TEST_DIR", tmp.path())
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped());
                    if first_canonical {
                        command.env("TILO_CANONICAL_TEST_MODE", "canonical");
                    }
                    let mut child = command.spawn().unwrap();
                    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
                    loop {
                        let mut line = String::new();
                        assert!(
                            output.read_line(&mut line).unwrap() > 0,
                            "child must acquire gate before opener test"
                        );
                        if line.contains("CANONICAL_GATE_READY") {
                            break;
                        }
                    }
                    let actor = fs::read(tmp.path().join(ACTOR_FILE)).unwrap();
                    let document = fs::read(tmp.path().join(DOCUMENT_FILE)).unwrap();
                    let open = || {
                        if second_canonical {
                            DocStore::open_canonical_at("so.tilo.test", tmp.path())
                        } else {
                            DocStore::open_in("so.tilo.test", tmp.path().to_path_buf())
                        }
                    };
                    let result = open();
                    assert_eq!(result.is_ok(), !first_canonical && !second_canonical);
                    if first_canonical || second_canonical {
                        assert!(matches!(result, Err(StoreError::WriterBusy { .. })));
                    }
                    assert_eq!(fs::read(tmp.path().join(ACTOR_FILE)).unwrap(), actor);
                    assert_eq!(fs::read(tmp.path().join(DOCUMENT_FILE)).unwrap(), document);
                    if terminate {
                        child.kill().unwrap();
                    } else {
                        child.stdin.take().unwrap().write_all(b"release\n").unwrap();
                    }
                    child.wait().unwrap();
                    assert!(open().is_ok());
                }
            }
        }
    }

    #[test]
    fn canonical_directory_sync_failure_is_retried_even_when_ancestors_exist() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("profile/package/rooms/room/records/record");
        let obligation = fs::canonicalize(tmp.path()).unwrap().join("profile");
        let faults = std::sync::Arc::new(TestFaults::default());
        *faults.sync_failure.lock().unwrap() = Some(obligation.clone());
        let result = DocStore::open_canonical_with("so.tilo.test", &path, |store| {
            store.faults = faults.clone();
        });
        assert!(result.is_err());
        assert!(path.is_dir());
        assert!(faults.sync_visits.lock().unwrap().contains(&obligation));
        *faults.sync_failure.lock().unwrap() = None;
        faults.sync_visits.lock().unwrap().clear();
        let store = DocStore::open_canonical_with("so.tilo.test", &path, |store| {
            store.faults = faults.clone();
        })
        .unwrap();
        assert!(faults.sync_visits.lock().unwrap().contains(&obligation));
        let physical = fs::canonicalize(&path).unwrap();
        let expected: Vec<_> = physical.ancestors().map(Path::to_path_buf).collect();
        #[cfg(windows)]
        let expected: Vec<_> = {
            let profile = fs::canonicalize(dirs::home_dir().unwrap()).unwrap();
            assert!(physical.starts_with(&profile));
            expected
                .into_iter()
                .filter(|path| path.starts_with(&profile))
                .collect()
        };
        assert_eq!(*faults.sync_visits.lock().unwrap(), expected);
        let mut doc = store.new_canonical_document().unwrap();
        doc.put(ROOT, "body", "visible").unwrap();
        *faults.sync_failure.lock().unwrap() = Some(physical);
        assert!(store.write_full(&doc.save()).is_err());
        assert_eq!(
            text_of(
                &AutoCommit::load(&fs::read(store.document_path()).unwrap()).unwrap(),
                "body"
            )
            .as_deref(),
            Some("visible")
        );
        drop(store);
        let mut reopen = DocStore::open_canonical_at("so.tilo.test", &path).unwrap();
        let loaded = reopen.load_canonical().unwrap().unwrap();
        reopen.accept_canonical(loaded).unwrap();
    }

    #[test]
    fn canonical_missing_or_replaced_directory_and_gate_never_recreated() {
        for replace_gate in [false, true] {
            let tmp = TempDir::new().unwrap();
            let path = tmp.path().join("record");
            let store = DocStore::open_canonical_at("so.tilo.test", &path).unwrap();
            if replace_gate {
                fs::remove_file(path.join(CANONICAL_GATE)).unwrap();
                fs::write(path.join(CANONICAL_GATE), b"replacement").unwrap();
            } else {
                #[cfg(windows)]
                {
                    // Windows prevents moving a directory with an open child
                    // handle. Verify that protection and its lifetime, rather
                    // than assuming the Unix rename operation can succeed.
                    let moved = tmp.path().join("moved");
                    let error = fs::rename(&path, &moved).unwrap_err();
                    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                    assert!(path.is_dir());
                    assert!(!moved.exists());
                    store.write_full(b"still owned").unwrap();
                    assert_eq!(fs::read(path.join(DOCUMENT_FILE)).unwrap(), b"still owned");
                    drop(store);
                    fs::rename(&path, &moved).unwrap();
                    assert!(!path.exists());
                    assert_eq!(fs::read(moved.join(DOCUMENT_FILE)).unwrap(), b"still owned");
                    continue;
                }
                #[cfg(not(windows))]
                fs::rename(&path, tmp.path().join("moved")).unwrap();
            }
            assert!(matches!(
                store.write_full(b"forbidden"),
                Err(StoreError::SourceChanged { .. })
            ));
            assert!(matches!(
                store.append_bytes(b"forbidden"),
                Err(StoreError::SourceChanged { .. })
            ));
            assert_eq!(path.exists(), replace_gate);
        }
    }

    #[test]
    fn canonical_empty_corrupt_and_changed_sources_are_never_published() {
        let tmp = TempDir::new().unwrap();
        let mut store = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
        assert!(store.load_canonical().unwrap().is_none());
        for bytes in [b"".as_slice(), b"not automerge".as_slice()] {
            fs::write(store.document_path(), bytes).unwrap();
            assert!(store.load_canonical().is_err());
            assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
        }
        let mut doc = store.new_canonical_document().unwrap();
        doc.put(ROOT, "body", "original").unwrap();
        let bytes = doc.save();
        fs::write(store.document_path(), &bytes).unwrap();
        let loaded = store.load_canonical().unwrap().unwrap();
        fs::write(store.document_path(), b"replacement").unwrap();
        assert!(matches!(
            store.accept_canonical(loaded),
            Err(StoreError::SourceChanged { .. })
        ));
        assert_eq!(fs::read(store.document_path()).unwrap(), b"replacement");
    }

    fn with_partial_tail(doc: &mut AutoCommit) -> Vec<u8> {
        let heads = doc.get_heads();
        let mut bytes = doc.save();
        doc.put(ROOT, "incomplete_tail", true).unwrap();
        let tail = doc.save_after(&heads);
        bytes.extend_from_slice(&tail[..tail.len() / 2]);
        bytes
    }

    fn assert_malformed_schemas_are_rejected(store: &mut DocStore) {
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
                fs::write(store.document_path(), &bytes).unwrap();
                assert!(matches!(
                    store.load_canonical(),
                    Err(StoreError::InvalidSchema { .. })
                ));
                assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
            }
        }
        let mut object = AutoCommit::new();
        object
            .put_object(ROOT, SCHEMA_VERSION_KEY, ObjType::Map)
            .unwrap();
        assert!(matches!(
            store.validate_canonical_schema(&object),
            Err(StoreError::InvalidSchema { .. })
        ));
    }

    fn assert_supported_schemas_are_nonmutating(store: &mut DocStore) {
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
                fs::write(store.document_path(), &bytes).unwrap();
                if newer {
                    assert!(matches!(
                        store.load_canonical(),
                        Err(StoreError::NewerSchema { .. })
                    ));
                } else {
                    let loaded = store.load_canonical().unwrap().unwrap();
                    if value.is_none() {
                        assert!(
                            loaded
                                .document()
                                .get(ROOT, SCHEMA_VERSION_KEY)
                                .unwrap()
                                .is_none()
                        );
                    }
                }
                assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
            }
        }
    }

    fn assert_schema_conflicts_are_checked(store: &mut DocStore) {
        for (left, right, accepted) in [
            (ScalarValue::Int(1), ScalarValue::Uint(1), true),
            (ScalarValue::Uint(0), ScalarValue::Uint(1), false),
        ] {
            let mut a = AutoCommit::new();
            let mut b = a.fork();
            a.put(ROOT, SCHEMA_VERSION_KEY, left).unwrap();
            b.put(ROOT, SCHEMA_VERSION_KEY, right).unwrap();
            a.merge(&mut b).unwrap();
            let bytes = a.save();
            fs::write(store.document_path(), &bytes).unwrap();
            if accepted {
                let loaded = store.load_canonical().unwrap().unwrap();
                let restored = store.accept_canonical(loaded).unwrap();
                assert_eq!(restored.get_all(ROOT, SCHEMA_VERSION_KEY).unwrap().len(), 2);
            } else {
                assert!(matches!(
                    store.load_canonical(),
                    Err(StoreError::InvalidSchema {
                        reason: "conflicting schema versions",
                        ..
                    })
                ));
            }
            assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
        }
        let mut supported_winner_seen = false;
        for swap in [false, true] {
            let mut a = AutoCommit::new().with_actor(ActorId::from(vec![1]));
            let mut b = AutoCommit::new().with_actor(ActorId::from(vec![2]));
            a.put(ROOT, SCHEMA_VERSION_KEY, if swap { 1_u64 } else { 2 })
                .unwrap();
            b.put(ROOT, SCHEMA_VERSION_KEY, if swap { 2_u64 } else { 1 })
                .unwrap();
            a.merge(&mut b).unwrap();
            supported_winner_seen |= schema_version_of(&a) == 1;
            let bytes = a.save();
            fs::write(store.document_path(), &bytes).unwrap();
            assert!(matches!(
                store.load_canonical(),
                Err(StoreError::NewerSchema { found: 2, .. })
            ));
            assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
        }
        assert!(
            supported_winner_seen,
            "must exercise unsupported losing assignment"
        );
    }

    #[test]
    fn canonical_schema_checks_all_live_values_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let mut store = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
        assert_malformed_schemas_are_rejected(&mut store);
        assert_supported_schemas_are_nonmutating(&mut store);
        assert_schema_conflicts_are_checked(&mut store);
    }

    #[test]
    fn canonical_prefix_repair_sync_failure_leaves_visible_bytes_unacknowledged() {
        let tmp = TempDir::new().unwrap();
        let mut store = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
        let mut doc = store.new_canonical_document().unwrap();
        doc.put(ROOT, "body", "prefix").unwrap();
        let torn = with_partial_tail(&mut doc);
        fs::write(store.document_path(), &torn).unwrap();
        let loaded = store.load_canonical().unwrap().unwrap();
        *store.faults.sync_failure.lock().unwrap() = Some(fs::canonicalize(tmp.path()).unwrap());
        assert!(store.accept_canonical(loaded).is_err());
        assert!(store.persisted.is_empty());
        assert!(!store.canonical_state().unwrap().accepted);
        assert!(AutoCommit::load(&fs::read(store.document_path()).unwrap()).is_ok());
        *store.faults.sync_failure.lock().unwrap() = None;
        let loaded = store.load_canonical().unwrap().unwrap();
        store.accept_canonical(loaded).unwrap();
        assert!(store.canonical_state().unwrap().accepted);
    }

    #[test]
    fn canonical_recovery_waits_for_acceptance_and_is_bound_to_its_store() {
        let tmp = TempDir::new().unwrap();
        let mut store = DocStore::open_canonical_at("so.tilo.test", tmp.path()).unwrap();
        let mut doc = store.new_canonical_document().unwrap();
        doc.put(ROOT, "body", "prefix").unwrap();
        let heads = doc.get_heads();
        let mut bytes = doc.save();
        doc.put(ROOT, "body", "torn").unwrap();
        let tail = doc.save_after(&heads);
        bytes.extend_from_slice(&tail[..tail.len() / 2]);
        fs::write(store.document_path(), &bytes).unwrap();
        let loaded = store.load_canonical().unwrap().unwrap();
        assert_eq!(fs::read(store.document_path()).unwrap(), bytes);
        assert_eq!(
            text_of(loaded.document(), "body").as_deref(),
            Some("prefix")
        );
        let other = TempDir::new().unwrap();
        let mut wrong = DocStore::open_canonical_at("so.tilo.test", other.path()).unwrap();
        assert!(matches!(
            wrong.accept_canonical(loaded),
            Err(StoreError::SourceChanged { .. })
        ));
        let loaded = store.load_canonical().unwrap().unwrap();
        store.accept_canonical(loaded).unwrap();
        assert!(AutoCommit::load(&fs::read(store.document_path()).unwrap()).is_ok());
    }

    #[test]
    fn canonical_gate_blocks_legacy_before_actor_creation() {
        let tmp = TempDir::new().unwrap();
        // The canonical protocol reserves this gate before touching actor or document.
        let gate_path = tmp.path().join("canonical.lock");
        let gate = acquire_writer_lock(&gate_path).unwrap();
        let result = DocStore::open_in("so.tilo.test", tmp.path().to_path_buf());
        assert!(
            result.is_err(),
            "a legacy opener must respect the canonical gate"
        );
        assert!(!tmp.path().join(ACTOR_FILE).exists());
        assert!(!tmp.path().join(DOCUMENT_FILE).exists());
        drop(gate);
        assert!(DocStore::open_in("so.tilo.test", tmp.path().to_path_buf()).is_ok());
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
    fn path_is_derived_from_the_package_id() {
        let tmp = scratch_home();
        let store = DocStore::open_for("so.tilo.ebook").unwrap();
        assert_eq!(store.dir(), tmp.path().join("so.tilo.ebook"));
        assert_eq!(
            store.document_path(),
            tmp.path().join("so.tilo.ebook").join(DOCUMENT_FILE)
        );
    }

    #[test]
    #[serial_test::serial]
    fn open_reads_the_launcher_package_id() {
        let tmp = scratch_home();
        unsafe { std::env::set_var("TILO_PACKAGE_ID", "so.tilo.monitor") };
        let store = DocStore::open().unwrap();
        assert_eq!(store.package_id(), "so.tilo.monitor");
        assert_eq!(store.dir(), tmp.path().join("so.tilo.monitor"));
        unsafe { std::env::remove_var("TILO_PACKAGE_ID") };
    }

    #[test]
    #[serial_test::serial]
    fn legacy_package_migration_is_claimed_by_one_room_and_resume_is_stable() {
        let tmp = scratch_home();
        let package = "so.tilo.legacy-claim";
        let legacy = DocStore::legacy_package_document_path(package).unwrap();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"legacy source remains intact").unwrap();

        assert!(DocStore::claim_legacy_package_migration(package, "room-a").unwrap());
        assert!(
            !DocStore::claim_legacy_package_migration(package, "room-b").unwrap(),
            "a second room cannot import the same retained source"
        );
        assert!(
            DocStore::claim_legacy_package_migration(package, "room-a").unwrap(),
            "the original claimant can resume after an interrupted migration"
        );
        assert!(legacy.is_file(), "claims never remove the recovery source");
        drop(tmp);
    }

    #[test]
    #[serial_test::serial]
    fn concurrent_legacy_migration_claims_have_one_winner() {
        let _tmp = scratch_home();
        let package = "so.tilo.concurrent-claim";
        fs::create_dir_all(cli::standalone_state_dir(package)).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut threads = Vec::new();
        for room in ["room-a", "room-b"] {
            let barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                DocStore::claim_legacy_package_migration(package, room).unwrap()
            }));
        }
        let wins = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(wins, 1, "atomic create gives one room the source");
    }

    #[test]
    #[serial_test::serial]
    fn torn_legacy_claim_recovers_without_granting_two_rooms() {
        let _tmp = scratch_home();
        let package = "so.tilo.torn-claim";
        let root = cli::standalone_state_dir(package);
        fs::create_dir_all(&root).unwrap();
        // Shape left by the old create-then-write implementation if it died
        // between those syscalls.  It has no valid owner, so recovery uses a
        // separate atomic claim rather than replacing this file in place.
        fs::write(root.join(LEGACY_PACKAGE_ROOM_CLAIM), b"room-a").unwrap();
        assert!(DocStore::claim_legacy_package_migration(package, "room-a").unwrap());
        assert!(!DocStore::claim_legacy_package_migration(package, "room-b").unwrap());
        assert!(DocStore::claim_legacy_package_migration(package, "room-a").unwrap());
    }

    /// The containment boundary: an app that lies about its own package id must
    /// not be able to address another app's directory or escape the data root.
    #[test]
    #[serial_test::serial]
    fn package_id_cannot_escape_the_data_root() {
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

    #[test]
    #[serial_test::serial]
    fn round_trip_preserves_content() {
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

    /// The anti-`contacts` test: `apps/contacts` used to save by replaying its
    /// model into a brand-new `AutoCommit`, producing a file with no shared
    /// history with any peer, so merges duplicated records instead of
    /// converging. A document through this store must still merge cleanly with
    /// a replica that diverged before the save.
    #[test]
    #[serial_test::serial]
    fn round_trip_merges_with_a_replica_that_diverged_before_the_save() {
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

        restored.merge(&mut peer).expect("merge with diverged peer");
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

    #[test]
    #[serial_test::serial]
    fn append_is_incremental_and_loads_back() {
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

    /// A crash during an append leaves a truncated trailing chunk; everything
    /// written before it must still load.
    #[test]
    #[serial_test::serial]
    fn a_torn_append_loses_only_the_torn_change() {
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

    /// A full save is a temp-file rename, so an interrupted one can't be
    /// observed: the previous document is intact, and the partial bytes sit in
    /// a `.tmp` sibling `load` never looks at.
    #[test]
    #[serial_test::serial]
    fn an_interrupted_full_save_leaves_the_previous_document_intact() {
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

    #[test]
    #[serial_test::serial]
    fn a_second_instance_gets_a_distinct_actor() {
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

    #[test]
    fn writer_lock_is_exclusive_and_released_with_its_handle() {
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

    #[test]
    #[serial_test::serial]
    fn migration_merges_a_legacy_file_without_losing_new_state() {
        let _tmp = scratch_home();
        // State an old release left behind somewhere else.
        let legacy_dir = TempDir::new().unwrap();
        let legacy = legacy_dir.path().join("addressbook.automerge");
        let mut old = AutoCommit::new();
        old.put(&ROOT, "from_legacy", ScalarValue::Str("yes".into()))
            .unwrap();
        fs::write(&legacy, old.save()).unwrap();

        // State the new store already has.
        let mut store = DocStore::open_for("so.tilo.migrate").unwrap();
        let mut doc = store.new_document();
        doc.put(&ROOT, "from_new", ScalarValue::Str("yes".into()))
            .unwrap();
        store.save(&mut doc).unwrap();

        assert!(store.migrate_from(&legacy).unwrap());
        let mut reopened = DocStore::open_for("so.tilo.migrate").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        assert_eq!(text_of(&loaded, "from_legacy").as_deref(), Some("yes"));
        assert_eq!(text_of(&loaded, "from_new").as_deref(), Some("yes"));

        // Idempotent: a second run changes nothing and loses nothing.
        assert!(store.migrate_from(&legacy).unwrap());
        let mut again = DocStore::open_for("so.tilo.migrate").unwrap();
        let loaded = again.load().unwrap().unwrap();
        assert_eq!(text_of(&loaded, "from_legacy").as_deref(), Some("yes"));
        assert_eq!(text_of(&loaded, "from_new").as_deref(), Some("yes"));
    }

    #[test]
    #[serial_test::serial]
    fn migration_of_a_missing_file_is_a_no_op() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("so.tilo.nomigrate").unwrap();
        assert!(!store.migrate_from(Path::new("/nonexistent/nope")).unwrap());
    }

    /// Every document this crate creates says which layout it is, from the
    /// moment it exists, so a document created and never edited is still
    /// distinguishable from one written before versioning.
    #[test]
    #[serial_test::serial]
    fn a_new_document_carries_the_schema_version() {
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

    #[test]
    #[serial_test::serial]
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

    #[test]
    #[serial_test::serial]
    fn app_owned_schema_refuses_a_newer_layout() {
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

    /// A document from before versioning existed opens, and gains its version
    /// on next write. Reading absence as "unknown" would refuse every existing
    /// user's documents on the very upgrade meant to prevent that loss.
    #[test]
    #[serial_test::serial]
    fn a_document_written_before_versioning_opens_and_is_stamped_on_save() {
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

    /// The one this exists for: an older build meets a document a newer one
    /// wrote. It must refuse and keep the bytes — writing over them is what
    /// turns a survivable empty session into data loss.
    #[test]
    #[serial_test::serial]
    fn a_newer_document_is_refused_and_never_written_over() {
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

    /// If the directory create were not idempotent, the second open would fail
    /// EEXIST and swallow into an in-memory fallback, silently losing state
    /// after the first launch. Pre-creates the directory before the *first*
    /// open too, since fresh-XDG CI never sees an existing one otherwise.
    #[test]
    #[serial_test::serial]
    fn opening_twice_against_an_existing_dir_keeps_persistence_enabled() {
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
        let mut autosave1 = Autosave::spawn(run1, DEFAULT_DEBOUNCE);
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
        let autosave2 = Autosave::spawn(run2, DEFAULT_DEBOUNCE);
        assert!(
            autosave2.is_enabled(),
            "the SECOND run must persist too — this is the bug: a non-idempotent \
             directory create disabled persistence for every launch after the first"
        );
        doc2.put(&ROOT, "note", ScalarValue::Str("again".into()))
            .unwrap();
        autosave2.flush();
    }

    /// An older layout's zero-byte file at the directory's path used to fail
    /// `create_dir_all` with EEXIST on every launch; opening must recover and
    /// come up enabled.
    #[test]
    #[serial_test::serial]
    fn opening_when_the_state_path_is_a_stale_empty_file_recovers_persistence() {
        let _tmp = scratch_home();
        let pkg = "so.tilo.stale-empty";
        let path = cli::standalone_state_dir(pkg);

        fs::write(&path, b"").unwrap();
        assert!(path.is_file(), "the leftover is a file, not a directory");

        let mut run1 = DocStore::open_for(pkg).expect("open must recover from a stale file");
        assert!(path.is_dir(), "the stale file must have become a directory");
        let mut doc = run1.load_or_new();
        let mut autosave1 = Autosave::spawn(run1, DEFAULT_DEBOUNCE);
        assert!(
            autosave1.is_enabled(),
            "recovering from a stale file must enable persistence, not fall back"
        );
        autosave1.seed(&mut doc);
        doc.put(&ROOT, "note", ScalarValue::Str("recovered".into()))
            .unwrap();
        autosave1.request(&mut doc);
        autosave1.flush();
        assert_eq!(autosave1.last_error(), None, "no write may have failed");
        drop(autosave1);

        let mut run2 = DocStore::open_for(pkg).expect("second open after recovery");
        let doc2 = run2.load_or_new();
        assert_eq!(
            text_of(&doc2, "note").as_deref(),
            Some("recovered"),
            "state written after recovery must round-trip across the re-open"
        );
    }

    /// A *non-empty* file at the state path might be an old-format document, so
    /// recovery must not delete it: moved aside to a `.stale` sibling and, if a
    /// real document, migrated in — persistence comes up enabled either way.
    #[test]
    #[serial_test::serial]
    fn opening_when_the_state_path_is_a_nonempty_file_preserves_and_migrates_it() {
        let _tmp = scratch_home();
        let pkg = "so.tilo.stale-doc";
        let path = cli::standalone_state_dir(pkg);

        // The old layout wrote a real Automerge document straight to the path.
        let mut old = AutoCommit::new();
        old.put(&ROOT, "note", ScalarValue::Str("from-old-layout".into()))
            .unwrap();
        let original = old.save();
        fs::write(&path, &original).unwrap();

        let mut store = DocStore::open_for(pkg).expect("open must recover from a stale document");
        assert!(
            path.is_dir(),
            "the stale document must have become a directory"
        );

        // The old bytes are preserved beside the directory, never deleted.
        let aside = PathBuf::from({
            let mut s = path.as_os_str().to_owned();
            s.push(".stale");
            s
        });
        assert!(
            aside.exists(),
            "the old document must be preserved at {}",
            aside.display()
        );
        assert_eq!(
            fs::read(&aside).unwrap(),
            original,
            "preserved bytes must be intact"
        );

        // And, being a genuine document, it migrated in and loads.
        let doc = store.load_or_new();
        assert_eq!(
            text_of(&doc, "note").as_deref(),
            Some("from-old-layout"),
            "an old-format document must migrate into the recovered store"
        );
        let autosave = Autosave::spawn(store, DEFAULT_DEBOUNCE);
        assert!(
            autosave.is_enabled(),
            "persistence must be enabled after migration"
        );
    }

    /// A state directory that genuinely cannot be created must still surface as
    /// an error — idempotent create must not paper over a real filesystem failure.
    #[test]
    #[serial_test::serial]
    fn an_uncreatable_state_dir_is_still_an_error() {
        // A file where a parent directory needs to be fails ENOTDIR for any
        // uid — unlike a read-only directory, which root would sail past.
        let tmp = TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, b"not a directory").unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", blocker.join("sub"));
            std::env::remove_var("TILO_PACKAGE_ID");
        }

        let result = DocStore::open_for("so.tilo.blocked");
        unsafe { std::env::remove_var("XDG_DATA_HOME") };

        match result {
            Err(StoreError::Io { .. }) => {}
            Err(other) => {
                panic!("expected an Io error from an uncreatable state directory, got {other:?}")
            }
            Ok(_) => panic!(
                "an uncreatable state directory must surface as an error, not be \
                 masked by the idempotent create"
            ),
        }
    }
}
