//! Canonical on-disk home for a qOS app's Automerge document: one file per
//! package at `$XDG_DATA_HOME/<package-id>/state.automerge`, atomic writes,
//! a debounced background writer. The path derives only from
//! `QOS_PACKAGE_ID`, validated so an app can't address another app's
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
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, LoadOptions, OnPartialLoad, ROOT, ReadDoc};

mod autosave;
pub use autosave::{Autosave, DEFAULT_DEBOUNCE};

pub const DOCUMENT_FILE: &str = "state.automerge";
pub const ACTOR_FILE: &str = "actor";
pub const LOCK_FILE: &str = "writer.lock";
/// Subdirectory holding one directory per record. See [`DocStore::open_record`].
pub const RECORDS_DIR: &str = "records";
/// Subdirectory holding one directory per **room** — the index of records
/// that room holds. See [`DocStore::open_room`].
pub const ROOMS_DIR: &str = "rooms";

/// Root-map key for the document's layout version. A document from before
/// this key existed reads as [`SCHEMA_VERSION`] rather than "unknown", so the
/// upgrade that added the check doesn't strand every existing document.
pub const SCHEMA_VERSION_KEY: &str = "qos:schema_version";

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

fn stamp_schema_version(doc: &mut AutoCommit) {
    if let Err(e) = doc.put(ROOT, SCHEMA_VERSION_KEY, u64::from(SCHEMA_VERSION)) {
        tracing::warn!(error = %e, "could not stamp the document's schema version");
    }
}

/// Stamp the version only if the document doesn't already carry one, so a
/// pre-versioning document gets it on first save rather than staying unlabelled.
fn ensure_schema_version(doc: &mut AutoCommit) {
    if matches!(doc.get(ROOT, SCHEMA_VERSION_KEY), Ok(None) | Err(_)) {
        stamp_schema_version(doc);
    }
}

/// Validate a package id as a single directory name — the containment
/// boundary. `QOS_PACKAGE_ID` is treated as untrusted (an app can set its
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
    /// Held for as long as the store lives; `None` when another instance already owns the lock.
    lock: Option<File>,
    /// Heads the file on disk is known to contain; empty until the first load or save.
    persisted: Vec<ChangeHash>,
    /// Why [`DocStore::load_or_new`] handed back an empty document instead of
    /// the one on disk — set only for a refusal that must not be written over;
    /// e.g. [`Autosave`] switches itself off when this is set.
    refusal: Option<Refusal>,
}

/// The document on disk, and why this build would not open it.
#[derive(Clone, Debug)]
struct Refusal {
    found: u32,
    message: String,
}

impl DocStore {
    /// Open the store belonging to the running package, via `QOS_PACKAGE_ID` —
    /// no argument by design, since an app doesn't choose where its state lives.
    ///
    /// # Errors
    ///
    /// Returns an error if `QOS_PACKAGE_ID` is not a valid single directory
    /// name (see [`validate_package_id`]), or if the package's state
    /// directory can't be created, inspected or locked.
    pub fn open() -> Result<Self, StoreError> {
        Self::open_for(&cli::package_id())
    }

    /// Open the store, falling back to `manifest_id` when `QOS_PACKAGE_ID` is
    /// unset. The env var always wins; the fallback only covers launcher-less
    /// runs (dev binary, tests) that would otherwise share one directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the resolved id is not a valid single directory
    /// name (see [`validate_package_id`]), or if the package's state
    /// directory can't be created, inspected or locked.
    pub fn open_with_manifest_id(manifest_id: &str) -> Result<Self, StoreError> {
        let id = std::env::var("QOS_PACKAGE_ID")
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
            lock,
            persisted: Vec::new(),
            refusal: None,
        })
    }

    pub fn package_id(&self) -> &str {
        &self.package_id
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
        self.lock.is_some()
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
        if found > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema {
                path: self.document.clone(),
                found,
                understood: SCHEMA_VERSION,
            });
        }
        doc.set_actor(self.actor.clone());
        self.persisted = doc.get_heads();
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
        stamp_schema_version(&mut doc);
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
        ensure_schema_version(doc);
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
                understood: SCHEMA_VERSION,
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
        ensure_schema_version(doc);
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
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.document)
            .map_err(|e| io_err(format!("appending to {}", self.document.display()), e))?;
        f.write_all(delta)
            .map_err(|e| io_err(format!("appending to {}", self.document.display()), e))?;
        f.sync_all()
            .map_err(|e| io_err(format!("syncing {}", self.document.display()), e))?;
        Ok(())
    }

    pub(crate) fn write_full(&self, bytes: &[u8]) -> Result<(), StoreError> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| io_err(format!("creating {}", self.dir.display()), e))?;
        atomic_file::write(&self.document, bytes)
            .map_err(|e| io_err(format!("writing {}", self.document.display()), e))
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
    // (`dev.qos.maps`), so `with_extension` would eat the last id segment.
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
            std::env::remove_var("QOS_PACKAGE_ID");
        }
        tmp
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
        let store = DocStore::open_for("dev.qos.ebook").unwrap();
        assert_eq!(store.dir(), tmp.path().join("dev.qos.ebook"));
        assert_eq!(
            store.document_path(),
            tmp.path().join("dev.qos.ebook").join(DOCUMENT_FILE)
        );
    }

    #[test]
    #[serial_test::serial]
    fn open_reads_the_launcher_package_id() {
        let tmp = scratch_home();
        unsafe { std::env::set_var("QOS_PACKAGE_ID", "dev.qos.monitor") };
        let store = DocStore::open().unwrap();
        assert_eq!(store.package_id(), "dev.qos.monitor");
        assert_eq!(store.dir(), tmp.path().join("dev.qos.monitor"));
        unsafe { std::env::remove_var("QOS_PACKAGE_ID") };
    }

    /// The containment boundary: an app that lies about its own package id must
    /// not be able to address another app's directory or escape the data root.
    #[test]
    #[serial_test::serial]
    fn package_id_cannot_escape_the_data_root() {
        let _tmp = scratch_home();
        for bad in [
            "../dev.qos.mail",
            "../../etc",
            "/etc/shadow",
            "dev.qos.mail/../dev.qos.chat",
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
        let mut store = DocStore::open_for("dev.qos.roundtrip").unwrap();
        assert!(store.load().unwrap().is_none(), "nothing saved yet");

        let mut doc = store.new_document();
        doc.put(&ROOT, "title", ScalarValue::Str("Moby Dick".into()))
            .unwrap();
        let notes = doc.put_object(&ROOT, "notes", ObjType::Text).unwrap();
        doc.splice_text(&notes, 0, 0, "call me ishmael").unwrap();
        store.save(&mut doc).unwrap();
        drop((store, doc));

        let mut reopened = DocStore::open_for("dev.qos.roundtrip").unwrap();
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
        let mut store = DocStore::open_for("dev.qos.merge").unwrap();

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

        let mut reopened = DocStore::open_for("dev.qos.merge").unwrap();
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
        let mut store = DocStore::open_for("dev.qos.append").unwrap();
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

        let mut reopened = DocStore::open_for("dev.qos.append").unwrap();
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
        let mut store = DocStore::open_for("dev.qos.torn").unwrap();
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

        let mut reopened = DocStore::open_for("dev.qos.torn").unwrap();
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
        let mut store = DocStore::open_for("dev.qos.atomic").unwrap();
        let mut doc = store.new_document();
        doc.put(&ROOT, "v", ScalarValue::Str("first".into()))
            .unwrap();
        store.save(&mut doc).unwrap();

        // The write half of an interrupted save: temp file exists, rename never happened.
        let tmp_path = store.document_path().with_extension("automerge.tmp");
        fs::write(&tmp_path, b"\x85\x6f\x4a\x83truncated garbage").unwrap();
        drop((store, doc));

        let mut reopened = DocStore::open_for("dev.qos.atomic").unwrap();
        let loaded = reopened.load().unwrap().expect("previous document");
        assert_eq!(text_of(&loaded, "v").as_deref(), Some("first"));
    }

    #[test]
    #[serial_test::serial]
    fn a_second_instance_gets_a_distinct_actor() {
        let _tmp = scratch_home();
        let first = DocStore::open_for("dev.qos.twice").unwrap();
        assert!(first.is_exclusive_writer());
        let second = DocStore::open_for("dev.qos.twice").unwrap();
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
        let mut store = DocStore::open_for("dev.qos.migrate").unwrap();
        let mut doc = store.new_document();
        doc.put(&ROOT, "from_new", ScalarValue::Str("yes".into()))
            .unwrap();
        store.save(&mut doc).unwrap();

        assert!(store.migrate_from(&legacy).unwrap());
        let mut reopened = DocStore::open_for("dev.qos.migrate").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        assert_eq!(text_of(&loaded, "from_legacy").as_deref(), Some("yes"));
        assert_eq!(text_of(&loaded, "from_new").as_deref(), Some("yes"));

        // Idempotent: a second run changes nothing and loses nothing.
        assert!(store.migrate_from(&legacy).unwrap());
        let mut again = DocStore::open_for("dev.qos.migrate").unwrap();
        let loaded = again.load().unwrap().unwrap();
        assert_eq!(text_of(&loaded, "from_legacy").as_deref(), Some("yes"));
        assert_eq!(text_of(&loaded, "from_new").as_deref(), Some("yes"));
    }

    #[test]
    #[serial_test::serial]
    fn migration_of_a_missing_file_is_a_no_op() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.nomigrate").unwrap();
        assert!(!store.migrate_from(Path::new("/nonexistent/nope")).unwrap());
    }

    /// Every document this crate creates says which layout it is, from the
    /// moment it exists, so a document created and never edited is still
    /// distinguishable from one written before versioning.
    #[test]
    #[serial_test::serial]
    fn a_new_document_carries_the_schema_version() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.stamped").unwrap();
        let mut doc = store.new_document();
        assert_eq!(schema_version_of(&doc), SCHEMA_VERSION);
        store.save(&mut doc).unwrap();

        let mut reopened = DocStore::open_for("dev.qos.stamped").unwrap();
        let loaded = reopened.load().unwrap().expect("saved document");
        assert!(
            loaded.get(ROOT, SCHEMA_VERSION_KEY).unwrap().is_some(),
            "the version has to survive the round trip, not just live in memory"
        );
        assert_eq!(schema_version_of(&loaded), SCHEMA_VERSION);
    }

    /// A document from before versioning existed opens, and gains its version
    /// on next write. Reading absence as "unknown" would refuse every existing
    /// user's documents on the very upgrade meant to prevent that loss.
    #[test]
    #[serial_test::serial]
    fn a_document_written_before_versioning_opens_and_is_stamped_on_save() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.unversioned").unwrap();

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

        let mut reopened = DocStore::open_for("dev.qos.unversioned").unwrap();
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
        let mut store = DocStore::open_for("dev.qos.future").unwrap();

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
        let pkg = "dev.qos.persist-twice";

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
        let pkg = "dev.qos.stale-empty";
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
        let pkg = "dev.qos.stale-doc";
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
            std::env::remove_var("QOS_PACKAGE_ID");
        }

        let result = DocStore::open_for("dev.qos.blocked");
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
