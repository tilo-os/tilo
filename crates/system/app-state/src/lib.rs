//! The canonical on-disk home for a qOS app's Automerge document.
//!
//! # Why one crate
//!
//! Every `ui` app keeps its whole user-visible state in one Automerge
//! document ([`ui::State::doc`]). Before this crate, what happened to that
//! document when the process exited was up to each app: most apps dropped it
//! on the floor (several while carrying a module comment promising the
//! opposite), and the ones that did persist had picked five mutually
//! incompatible conventions — raw Automerge bytes under half a dozen
//! different directory names, serde JSON, TOML, newline-delimited text, and
//! one app that rebuilt a brand-new `AutoCommit` on every save and thereby
//! threw away the history its own doc comment said it was keeping.
//!
//! This crate replaces all of that with a single store: one location per
//! installed package, one file format (raw Automerge, so any peer or tool
//! can merge against it), atomic writes, and a debounced background writer
//! so a keystroke never blocks on the disk.
//!
//! # Where the bytes go
//!
//! `$XDG_DATA_HOME/<package-id>/state.automerge`, i.e.
//! `~/.local/share/dev.qos.ebook/state.automerge` for the e-book reader.
//! The directory is derived from `QOS_PACKAGE_ID` — the launcher sets it for
//! every sandboxed launch and it is the only input. **An app cannot name its
//! own storage path, and it cannot name another app's**: [`DocStore::open`]
//! takes no path, and [`DocStore::open_for`] rejects any package id that is
//! absolute, contains a separator, or contains `..`.
//!
//! The resolver is [`cli::standalone_state_dir`] — the same function
//! `cli::resolve_workspace_path` uses for its own fallback, so there is one
//! implementation of "where does package X keep its data", not a third one.
//! `resolve_workspace_path` itself is deliberately *not* used: inside qOS it
//! resolves against the workspace FUSE root, which is shared by every app
//! and by the user's own file tree. App state is not a user document — it
//! would collide at the workspace root and show up in the file manager. The
//! workspace resolver stays the right call for documents the user browses
//! and shares; this store is for the app's own state.
//!
//! # Format and growth
//!
//! The file is a plain Automerge save followed by zero or more appended
//! change chunks — the format `AutoCommit::load` already understands and the
//! same shape the calendar and chat stores were already writing, so existing
//! files load unchanged.
//!
//! Saving a full `doc.save()` after every edit is quadratic in the number of
//! edits; [`Autosave`] therefore appends only `doc.save_after(persisted_heads)`
//! and rewrites the whole file only once the appended volume exceeds the
//! compacted base. That makes total write volume linear in document size,
//! amortised, the same way a growing `Vec` is.
//!
//! # Actor identity
//!
//! The store keeps the document's Automerge actor id in an `actor` sidecar
//! and restores it on load, so a restarted app continues the same actor's
//! change sequence instead of forking a new one on every launch. Two live
//! instances must never share an actor id — that would produce two different
//! changes claiming the same `(actor, seq)` — so [`DocStore::open`] takes an
//! exclusive `flock` on a `writer.lock` file and only adopts the sidecar
//! identity when it wins the lock. A second instance runs on a fresh random
//! actor, which is exactly what Automerge expects of a concurrent writer.
//!
//! What is *not* negotiable is history: the store round-trips the document's
//! own bytes. It never rebuilds a document by replaying an app's model into
//! a fresh `AutoCommit`, which is what made one app's saved file unmergeable
//! with any peer that had diverged before the save.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use automerge::{ActorId, AutoCommit, ChangeHash, LoadOptions, OnPartialLoad};

mod autosave;
pub use autosave::{Autosave, DEFAULT_DEBOUNCE};

/// Name of the Automerge document inside the package's state directory.
pub const DOCUMENT_FILE: &str = "state.automerge";
/// Name of the actor-identity sidecar.
pub const ACTOR_FILE: &str = "actor";
/// Name of the single-writer lock file.
pub const LOCK_FILE: &str = "writer.lock";
/// Directory, inside a package's state directory, holding one subdirectory per
/// record. See [`DocStore::open_record`].
pub const RECORDS_DIR: &str = "records";

/// Everything that can go wrong opening, reading or writing a store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The package id could not be turned into a directory name.
    #[error("invalid package id {0:?}: {1}")]
    PackageId(String, &'static str),
    /// A filesystem operation failed. The path is in the message.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    /// The stored bytes are not a loadable Automerge document.
    #[error("reading {path}: {source}")]
    Automerge {
        path: PathBuf,
        #[source]
        source: automerge::AutomergeError,
    },
}

fn io_err(context: impl Into<String>, source: io::Error) -> StoreError {
    StoreError::Io {
        context: context.into(),
        source,
    }
}

/// Validate a package id as a single directory name.
///
/// This is the containment boundary. `QOS_PACKAGE_ID` normally arrives from
/// the launcher, but an app runs in its own process and can set its own
/// environment, so the value is treated as untrusted input: anything that
/// could address a directory other than the app's own is rejected rather
/// than sanitised.
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

/// Validate a record key as a single directory name.
///
/// Same boundary as [`validate_package_id`], and it matters more here: a
/// record's identity can be a *file path*, so the key derived from it is the
/// one place where `..` could turn a document identity into a traversal.
/// Rejected rather than sanitised — a key we would have to rewrite is a key the
/// caller derived wrongly, and silently redirecting it would make two records
/// share one file.
fn validate_record_key(key: &str) -> Result<(), StoreError> {
    if key.is_empty() {
        return Err(StoreError::RecordKey(
            key.to_string(),
            "must not be empty",
        ));
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

/// The persistent store for one package's Automerge document.
///
/// Construct it with [`DocStore::open`] (which reads `QOS_PACKAGE_ID`) and
/// either drive it by hand ([`DocStore::load_or_new`] + [`DocStore::save`])
/// or hand it to [`DocStore::autosave`] for debounced background writes.
pub struct DocStore {
    package_id: String,
    dir: PathBuf,
    document: PathBuf,
    actor: ActorId,
    /// Held for as long as the store lives; dropping the file releases the
    /// `flock`. `None` when another instance already owns the lock.
    lock: Option<File>,
    /// Heads the file on disk is known to contain. Empty until the first
    /// load or save.
    persisted: Vec<ChangeHash>,
}

impl DocStore {
    /// Open the store belonging to the running package.
    ///
    /// The package id comes from `QOS_PACKAGE_ID` (via [`cli::package_id`]),
    /// which the launcher sets on every sandboxed launch. There is no
    /// argument by design: an app does not get to choose where its state
    /// lives.
    pub fn open() -> Result<Self, StoreError> {
        Self::open_for(&cli::package_id())
    }

    /// Open the store, using `manifest_id` when the launcher has not set
    /// `QOS_PACKAGE_ID`.
    ///
    /// The env var always wins, so this cannot be used to escape the
    /// launcher's decision — it only supplies an answer for a run that has
    /// no launcher at all (a developer running the binary straight out of
    /// `target/debug`, or a test harness). Without it every such run would
    /// land in the shared `qos-standalone` directory and 44 apps would
    /// scribble over one document.
    ///
    /// `manifest_id` is the app's own `[package] id` from its
    /// `qos-app.toml`.
    pub fn open_with_manifest_id(manifest_id: &str) -> Result<Self, StoreError> {
        let id = std::env::var("QOS_PACKAGE_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| manifest_id.to_string());
        Self::open_for(&id)
    }

    /// Open the store for an explicit package id.
    ///
    /// For tests and for migration tools that legitimately need to address a
    /// specific package. Rejects any id that is not a single, non-traversing
    /// directory name.
    pub fn open_for(package_id: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        let dir = cli::standalone_state_dir(package_id);
        Self::open_in(package_id, dir)
    }

    /// Open the store for **one record** of a package.
    ///
    /// A record is the unit of collaboration: one document, one identity, one
    /// change graph. An automerge change graph cannot be served in part — a
    /// change names its dependencies by hash, so a filtered stream leaves the
    /// receiver holding changes whose parents never arrive — so "share this
    /// document and not that one" has to mean *separate documents*, which
    /// means separate files.
    ///
    /// `record_key` is a filesystem-safe key derived from the record's
    /// identity by the caller (`ui::record::RecordId::storage_key`), and it is
    /// validated exactly as strictly as a package id: a record whose identity
    /// is a *path* must not be able to turn that path into a directory
    /// traversal.
    ///
    /// The bytes land in `<package dir>/records/<record_key>/state.automerge`,
    /// beside — not inside — the package's own `state.automerge`, which stays
    /// the room index (which records this room holds).
    pub fn open_record(package_id: &str, record_key: &str) -> Result<Self, StoreError> {
        validate_package_id(package_id)?;
        validate_record_key(record_key)?;
        let dir = cli::standalone_state_dir(package_id)
            .join(RECORDS_DIR)
            .join(record_key);
        Self::open_in(package_id, dir)
    }

    fn open_in(package_id: &str, dir: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir_all(&dir)
            .map_err(|e| io_err(format!("creating state directory {}", dir.display()), e))?;

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
        })
    }

    /// The package id this store belongs to.
    pub fn package_id(&self) -> &str {
        &self.package_id
    }

    /// The directory holding the document, its actor sidecar and its lock.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The document file itself.
    pub fn document_path(&self) -> &Path {
        &self.document
    }

    /// The Automerge actor this store writes as.
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    /// Whether this process won the single-writer lock. A `false` here means
    /// another instance is live; writes still succeed (last writer wins, and
    /// Automerge merges the two histories on the next load) but the actor id
    /// is a fresh one rather than the sidecar's.
    pub fn is_exclusive_writer(&self) -> bool {
        self.lock.is_some()
    }

    /// Load the persisted document, or `Ok(None)` on a first launch.
    ///
    /// A trailing partially-written change chunk (a crash during an append)
    /// is dropped rather than failing the load, so the user loses the last
    /// unflushed edit instead of the whole document.
    pub fn load(&mut self) -> Result<Option<AutoCommit>, StoreError> {
        let bytes = match fs::read(&self.document) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(io_err(
                    format!("reading {}", self.document.display()),
                    e,
                ));
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
            source,
        })?;
        doc.set_actor(self.actor.clone());
        self.persisted = doc.get_heads();
        Ok(Some(doc))
    }

    /// Load the persisted document, falling back to an empty one.
    ///
    /// An unreadable file is logged and treated as empty rather than
    /// crashing the app on launch — the same policy the calendar and chat
    /// stores already had.
    pub fn load_or_new(&mut self) -> AutoCommit {
        match self.load() {
            Ok(Some(doc)) => doc,
            Ok(None) => self.new_document(),
            Err(e) => {
                tracing::error!(error = %e, "app state unreadable; starting empty");
                self.new_document()
            }
        }
    }

    /// An empty document already carrying this store's actor id.
    pub fn new_document(&mut self) -> AutoCommit {
        self.persisted = Vec::new();
        AutoCommit::new().with_actor(self.actor.clone())
    }

    /// Rewrite the whole document atomically: temp file, `fsync`, rename.
    ///
    /// A crash at any point leaves either the previous complete file or the
    /// new complete file, never a half-written one.
    pub fn save(&mut self, doc: &mut AutoCommit) -> Result<(), StoreError> {
        let bytes = doc.save();
        self.write_full(&bytes)?;
        self.persisted = doc.get_heads();
        Ok(())
    }

    /// Append only the changes `doc` has that the file does not.
    ///
    /// Returns the number of bytes appended — `0` when the document has not
    /// moved since the last save, which is the common idle case.
    pub fn append(&mut self, doc: &mut AutoCommit) -> Result<usize, StoreError> {
        let heads = doc.get_heads();
        if heads == self.persisted {
            return Ok(0);
        }
        // No base file yet: an append of raw change chunks with no document
        // chunk in front is still loadable, but a full save compresses far
        // better, so seed the file properly.
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

    /// Adopt a document an older release of this app wrote somewhere else.
    ///
    /// Merges `legacy` into the store's document rather than replacing it,
    /// so running a migration twice, or migrating on top of state written
    /// since, cannot lose data. Returns `true` when something was imported.
    /// The legacy file is left alone; removing it is the caller's call.
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
            source,
        })?;

        let mut current = match self.load()? {
            Some(doc) => doc,
            None => self.new_document(),
        };
        current
            .merge(&mut incoming)
            .map_err(|source| StoreError::Automerge {
                path: legacy.to_path_buf(),
                source,
            })?;
        self.save(&mut current)?;
        Ok(true)
    }

    /// Hand the store to a background thread that coalesces a burst of edits
    /// into one write. See [`Autosave`].
    pub fn autosave(self, debounce: std::time::Duration) -> Autosave {
        Autosave::spawn(self, debounce)
    }

    /// Extend the document file with raw change chunks. The caller is
    /// responsible for those chunks being ones the file does not already
    /// contain; [`DocStore::append`] and [`Autosave`] both derive them from
    /// `save_after`.
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

/// Take an exclusive, non-blocking `flock` on `path`. `None` when another
/// process already holds it (or the lock file cannot be created at all, in
/// which case we conservatively behave as the non-exclusive writer).
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

    /// The containment boundary: an app that lies about its own package id
    /// must not be able to address another app's directory or escape the
    /// data root at all.
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
        drop((store, doc)); // the process exits here

        let mut reopened = DocStore::open_for("dev.qos.roundtrip").unwrap();
        let loaded = reopened.load().unwrap().expect("document is on disk");
        assert_eq!(text_of(&loaded, "title").as_deref(), Some("Moby Dick"));
        let notes = loaded.get(&ROOT, "notes").unwrap().unwrap().1;
        assert_eq!(loaded.text(&notes).unwrap(), "call me ishmael");
    }

    /// The anti-`contacts` test. `apps/contacts` used to save by replaying
    /// its model into a brand-new `AutoCommit`, which produced a file whose
    /// ops shared no history with any peer — a merge would duplicate every
    /// record instead of converging. A document that goes through this store
    /// must still merge cleanly with a replica that diverged *before* the
    /// save.
    #[test]
    #[serial_test::serial]
    fn round_trip_merges_with_a_replica_that_diverged_before_the_save() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.merge").unwrap();

        // Shared ancestor.
        let mut doc = store.new_document();
        let list = doc.put_object(&ROOT, "contacts", ObjType::Map).unwrap();
        doc.put(&list, "ada", ScalarValue::Str("Ada Lovelace".into()))
            .unwrap();
        let original_actor = doc.get_actor().clone();

        // A peer forks here and edits independently.
        let mut peer = doc.fork().with_actor(ActorId::random());
        let peer_list = peer.get(&ROOT, "contacts").unwrap().unwrap().1;
        peer.put(&peer_list, "grace", ScalarValue::Str("Grace Hopper".into()))
            .unwrap();

        // Meanwhile the app edits and saves.
        doc.put(&list, "alan", ScalarValue::Str("Alan Turing".into()))
            .unwrap();
        store.save(&mut doc).unwrap();
        drop((store, doc)); // the process exits here

        let mut reopened = DocStore::open_for("dev.qos.merge").unwrap();
        let mut restored = reopened.load().unwrap().expect("saved document");

        // Actor identity survives the restart: the store keeps writing as
        // the same automerge actor rather than forking a new one per launch.
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
        // A rebuilt-from-scratch document would have made `contacts` a
        // *different* object on each side, so the merge would leave two
        // conflicting values at the key. Exactly one is the proof it didn't.
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

        // Idle: no further bytes.
        assert_eq!(store.append(&mut doc).unwrap(), 0);
        drop((store, doc));

        let mut reopened = DocStore::open_for("dev.qos.append").unwrap();
        let loaded = reopened.load().unwrap().unwrap();
        let body = loaded.get(&ROOT, "body").unwrap().unwrap().1;
        assert_eq!(loaded.text(&body).unwrap(), "hello world");
    }

    /// A crash during an append leaves a truncated trailing chunk. Everything
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

    /// A full save is a temp-file rename, so an interrupted one cannot be
    /// observed: the previous document is intact and the partial bytes are
    /// parked in a `.tmp` sibling that `load` never looks at.
    #[test]
    #[serial_test::serial]
    fn an_interrupted_full_save_leaves_the_previous_document_intact() {
        let _tmp = scratch_home();
        let mut store = DocStore::open_for("dev.qos.atomic").unwrap();
        let mut doc = store.new_document();
        doc.put(&ROOT, "v", ScalarValue::Str("first".into())).unwrap();
        store.save(&mut doc).unwrap();

        // The write half of an interrupted save: temp file exists, rename
        // never happened.
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
}
