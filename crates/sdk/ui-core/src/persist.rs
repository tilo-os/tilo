//! Making an app's state survive a restart.
//!
//! The toolkit stays away from the disk itself —
//! [`State::save`]/[`State::load`] only convert to and from bytes — so
//! without this, each app has to invent its own storage.
//!
//! This module closes that gap in three lines. Instead of
//!
//! ```ignore
//! tilo_ui::App::from_factory(tilo_ui::State::new(), my_app::app_root).run();
//! ```
//!
//! an app writes
//!
//! ```ignore
//! let (state, autosave) = tilo_ui::persist::open("so.tilo.my-app");
//! tilo_ui::App::from_factory(state, my_app::app_root)
//!     .with_autosave(autosave)
//!     .run();
//! ```
//!
//! and gets the previous session's document loaded before the first
//! `sync`, every change written back atomically and debounced, and a
//! flush on exit. Where the bytes live is documented on [`app_state`].
//!
//! The id string is a fallback: `TILO_PACKAGE_ID`, set by the launcher on
//! every sandboxed launch, always wins — without it a binary run from
//! `target/debug` would share one document with every other app.
//!
//! Apps carrying state an older release wrote elsewhere call
//! [`open_migrating`] once with that legacy path; the old document is
//! merged in, so the migration is safe to run repeatedly.
//!
//! Every function here resolves storage from the native process
//! environment (`TILO_PACKAGE_ID`, the XDG data directory) and the local
//! filesystem. A host with an explicit profile opens records through
//! [`crate::record::RecordOwners`] instead, which persists through any
//! [`durable_store::DurableStore`].

// `app_state::StoreError` (144+ bytes) is what makes every `Result<_,
// StoreError>` in this module large. Shrinking it means editing the
// `app_state` crate; boxing it here instead means changing the return
// type of these `pub fn`s, a public-API change that ripples to every
// crate that calls into `tilo_ui::persist` (`shell` included). Neither is a
// same-crate lint fix, so the size is accepted rather than exempted
// function-by-function.
#![allow(clippy::result_large_err)]

use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use durable_store::{DurableStore, StorageKey};

pub use app_state::{DEFAULT_DEBOUNCE, DocStore, DocWriter, StoreError};

use crate::state::State;

/// Load this package's persisted document and start its background writer.
///
/// Never fails the caller: if the state directory is unusable (read-only
/// home, missing `HOME`, a corrupt document) the error is logged and the app
/// comes up on an empty document, exactly as it did before persistence
/// existed. An app that would rather know can call [`try_open`].
pub fn open(manifest_id: &str) -> (State, DocWriter) {
    try_open(manifest_id).unwrap_or_else(|e| {
        tracing::error!(error = %e, "app state unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// Open the index for one collaboration room.  Record-aware applications use
/// this instead of [`open`]: their package-global document used to be the
/// room index, which let two rooms hydrate and overwrite each other's record
/// references after a restart.
///
/// On the first scoped open the legacy package document is merged, never
/// moved or deleted.  The next autosave lands in the room directory, so a
/// crash during migration still leaves the legacy source intact.
///
/// The host's package id, when set, wins over `manifest_id`.
pub fn open_room(manifest_id: &str, room_id: &str) -> (State, DocWriter) {
    let package_id = crate::host::environment()
        .identity
        .package_id
        .clone()
        .unwrap_or_else(|| manifest_id.to_string());
    open_room_in(DocStore::native_store(), &package_id, room_id)
}

/// [`open_room`] over an explicit store and package, consulting no
/// environment.
pub fn open_room_in(
    documents: Rc<dyn DurableStore>,
    package_id: &str,
    room_id: &str,
) -> (State, DocWriter) {
    let attempt = || -> Result<(State, DocWriter), StoreError> {
        app_state::validate_record_identity(package_id, room_id, "index")?;
        let legacy = StorageKey::package(package_id);
        let has_legacy = documents
            .read(&legacy)?
            .is_some_and(|stored| !stored.bytes.is_empty());
        let mut store = DocStore::shared(
            Rc::clone(&documents),
            StorageKey::room_index(package_id, room_id),
        )?;
        if has_legacy && documents.claim(&legacy, room_id)? {
            match store.import_from(&legacy) {
                Ok(true) => tracing::info!(
                    room = room_id,
                    from = %legacy,
                    to = %store.document_path().display(),
                    "migrated package-global record index into its room"
                ),
                Ok(false) => {}
                Err(error) => tracing::error!(
                    room = room_id,
                    error = %error,
                    "legacy package record index could not be imported"
                ),
            }
        } else if has_legacy {
            tracing::info!(
                room = room_id,
                from = %legacy,
                "legacy package record index belongs to another room; not importing"
            );
        }
        Ok(finish(store))
    };
    attempt().unwrap_or_else(|error| {
        tracing::error!(room = room_id, error = %error, "room index unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// [`open`], plus a one-time import of a document an older release of this
/// app left at `legacy`.
///
/// The legacy document is *merged*, so state written since the upgrade is
/// preserved and running the migration twice is a no-op. The old file is
/// left on disk; deleting it is a decision for whoever knows whether
/// anything else still reads it.
pub fn open_migrating(manifest_id: &str, legacy: &Path) -> (State, DocWriter) {
    let attempt = || -> Result<(State, DocWriter), StoreError> {
        let mut store = DocStore::open_with_manifest_id(manifest_id)?;
        match store.migrate_from(legacy) {
            Ok(true) => tracing::info!(
                from = %legacy.display(),
                to = %store.document_path().display(),
                "imported app state from its previous location"
            ),
            Ok(false) => {}
            Err(e) => tracing::error!(error = %e, "legacy app state could not be imported"),
        }
        Ok(finish(store))
    };
    attempt().unwrap_or_else(|e| {
        tracing::error!(error = %e, "app state unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// [`open`] without the swallow-and-log fallback.
///
/// # Errors
///
/// Returns [`StoreError`] when the state directory can't be opened (a
/// read-only home, a missing `HOME`, or a document a newer build wrote in
/// an incompatible layout).
pub fn try_open(manifest_id: &str) -> Result<(State, DocWriter), StoreError> {
    Ok(finish(DocStore::open_with_manifest_id(manifest_id)?))
}

fn finish(mut store: DocStore) -> (State, DocWriter) {
    let mut doc = store.load_or_new();
    // A document written by a newer build is not opened, and the empty
    // stand-in must never reach disk — with autosave on, the first edit
    // (or the exit flush) would save it over the existing document. The store
    // also refuses the write itself; the disabled writer is what stops
    // the app logging an error on every keystroke to earn that guarantee.
    if let Some(reason) = store.refusal() {
        let reason = reason.to_string();
        tracing::error!(
            reason = %reason,
            "app state was written by a newer version of this app; running \
             without persistence so the file on disk is left intact"
        );
        eprintln!("tilo: {reason}");
        eprintln!("tilo: this session starts empty and will NOT be saved.");
        return (State::from_doc(doc), DocWriter::disabled(reason));
    }
    let mut autosave = store.writer();
    autosave.seed(&mut doc);
    (State::from_doc(doc), autosave)
}

/// The degraded path: an empty document and a disabled writer.
/// [`DocWriter::is_enabled`] returns `false` and
/// [`DocWriter::last_error`] carries the reason, so nothing downstream can
/// mistake this for a working store.
fn in_memory_fallback() -> (State, DocWriter) {
    (
        State::new(),
        DocWriter::disabled("state directory unavailable"),
    )
}

/// Convenience for tests and tools: same as [`open`] but for an explicit
/// package id rather than `TILO_PACKAGE_ID`.
///
/// # Errors
///
/// Returns [`StoreError`] when the state directory can't be opened (a
/// read-only home, a missing `HOME`, or a document a newer build wrote in
/// an incompatible layout).
pub fn open_for(package_id: &str) -> Result<(State, DocWriter), StoreError> {
    Ok(finish(DocStore::open_for(package_id)?))
}

/// [`open_for`] over an explicit store, consulting no environment.
///
/// # Errors
///
/// Returns [`StoreError`] when the package id is invalid or the store
/// refuses the document.
pub fn open_in(
    documents: Rc<dyn DurableStore>,
    package_id: &str,
) -> Result<(State, DocWriter), StoreError> {
    Ok(finish(DocStore::shared(
        documents,
        StorageKey::package(package_id),
    )?))
}

/// The debounce window the native store applies to [`open`]'s writes.
pub const DEBOUNCE: Duration = DEFAULT_DEBOUNCE;

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

    fn native(root: &std::path::Path) -> Rc<dyn DurableStore> {
        Rc::new(app_state::FsStore::new(root))
    }

    fn open_at(root: &std::path::Path, package: &str) -> (State, DocWriter) {
        open_in(native(root), package).expect("open")
    }

    fn note(state: &crate::state::State) -> Option<String> {
        match state.doc.get(&ROOT, "note") {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Str(v) => Some(v.to_string()),
                other => Some(other.to_string()),
            },
            _ => None,
        }
    }

    /// Opening an app for the SECOND time must keep persistence ON. If the
    /// store treated an already-existing state directory as an error,
    /// [`open`] would silently fall back to a disabled, in-memory
    /// [`in_memory_fallback`] on every run but the first — a case
    /// fresh-XDG CI never exercises, so this test creates the directory
    /// up front and drives two opens against it.
    #[test]
    fn open_keeps_autosave_enabled_when_the_state_dir_already_exists() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let pkg = "so.tilo.persist-open-twice";

        // Directory present before the first open, as on every later launch.
        let dir = tmp.path().join(pkg);
        std::fs::create_dir_all(&dir).unwrap();

        // Run 1: write and flush a value.
        {
            let (mut state, mut autosave) = open_in(native(tmp.path()), pkg).expect("run 1 open");
            assert!(autosave.is_enabled(), "run 1 must persist, not fall back");
            state
                .doc
                .put(&ROOT, "note", ScalarValue::Str("survives".into()))
                .unwrap();
            autosave.request(&mut state.doc);
            autosave.flush();
            assert_eq!(autosave.last_error(), None, "no write may have failed");
        }

        // Run 2: the directory still exists; persistence must stay enabled
        // and run 1's value must load.
        let (state, autosave) = open_at(tmp.path(), pkg);
        assert!(
            autosave.is_enabled(),
            "the SECOND open against an existing state dir must keep persistence \
             enabled, not drop to the in-memory fallback"
        );
        assert_eq!(
            note(&state).as_deref(),
            Some("survives"),
            "state written on the first run must load on the second open"
        );
    }

    #[test]
    fn legacy_record_index_is_imported_by_one_room_only() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let package = "so.tilo.legacy-room-index";
        let id = crate::record::RecordId::minted();
        let kind = crate::record::RecordKind::new("note");
        let mut legacy = DocStore::shared(native(tmp.path()), StorageKey::package(package))
            .expect("legacy store");
        let mut legacy_doc = legacy.load_or_new();
        crate::record::room::reference(&mut legacy_doc, &id, &kind);
        legacy.save(&mut legacy_doc).expect("write legacy index");
        let source = legacy.document_path().to_path_buf();
        drop(legacy);

        let (a, _) = open_room_in(
            native(tmp.path()),
            package,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        );
        assert!(crate::record::room::holds(&a.doc, &id));
        let (b, _) = open_room_in(
            native(tmp.path()),
            package,
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
        );
        assert!(
            !crate::record::room::holds(&b.doc, &id),
            "a second room must not re-import a package-global index"
        );
        assert!(source.is_file(), "legacy source is retained for recovery");
    }

    /// An older app layout wrote the document as a zero-byte *file* at the
    /// per-package state path. `open` must recover and come up with
    /// persistence enabled, not swallow the `EEXIST` and fall back to the
    /// disabled in-memory store.
    #[test]
    fn open_recovers_when_the_state_path_is_a_stale_file() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let pkg = "so.tilo.stale-file-open";

        // Old layout's leftover: a file where the new layout wants a directory.
        let path = tmp.path().join(pkg);
        std::fs::write(&path, b"").unwrap();
        assert!(path.is_file(), "the leftover is a file, not a directory");

        // Run 1: `open` must recover to enabled persistence.
        {
            let (mut state, mut autosave) = open_at(tmp.path(), pkg);
            assert!(
                autosave.is_enabled(),
                "opening over a stale file must recover to enabled persistence, \
                 not drop to the in-memory fallback"
            );
            state
                .doc
                .put(&ROOT, "note", ScalarValue::Str("recovered".into()))
                .unwrap();
            autosave.request(&mut state.doc);
            autosave.flush();
            assert_eq!(autosave.last_error(), None, "no write may have failed");
        }

        // Run 2: the path is a directory now, and the value survives.
        let (state, autosave) = open_at(tmp.path(), pkg);
        assert!(
            autosave.is_enabled(),
            "persistence stays enabled after recovery"
        );
        assert_eq!(
            note(&state).as_deref(),
            Some("recovered"),
            "state written after recovery must load on the next open"
        );
    }
}
