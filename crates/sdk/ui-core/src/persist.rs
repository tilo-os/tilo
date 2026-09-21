//! Making an app's state survive a restart.
//!
//! The toolkit stays away from the disk itself —
//! [`State::save`]/[`State::load`] only convert to and from bytes — so
//! without this, each app has to invent its own storage.
//!
//! This module closes that gap in three lines. Instead of
//!
//! ```ignore
//! ui::App::from_factory(ui::State::new(), my_app::app_root).run();
//! ```
//!
//! an app writes
//!
//! ```ignore
//! let (state, autosave) = ui::persist::open("so.tilo.my-app");
//! ui::App::from_factory(state, my_app::app_root)
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

// `app_state::StoreError` (144+ bytes) is what makes every `Result<_,
// StoreError>` in this module large. Shrinking it means editing the
// `app_state` crate; boxing it here instead means changing the return
// type of these `pub fn`s, a public-API change that ripples to every
// crate that calls into `ui::persist` (`shell` included). Neither is a
// same-crate lint fix, so the size is accepted rather than exempted
// function-by-function.
#![allow(clippy::result_large_err)]

use std::path::Path;
use std::time::Duration;

pub use app_state::{Autosave, DEFAULT_DEBOUNCE, DocStore, StoreError};

use crate::state::State;

/// Load this package's persisted document and start its background writer.
///
/// Never fails the caller: if the state directory is unusable (read-only
/// home, missing `HOME`, a corrupt document) the error is logged and the app
/// comes up on an empty document, exactly as it did before persistence
/// existed. An app that would rather know can call [`try_open`].
pub fn open(manifest_id: &str) -> (State, Autosave) {
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
pub fn open_room(manifest_id: &str, room_id: &str) -> (State, Autosave) {
    let package_id = std::env::var("TILO_PACKAGE_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| manifest_id.to_string());
    let attempt = || -> Result<(State, Autosave), StoreError> {
        let mut store = DocStore::open_room(&package_id, room_id)?;
        let legacy = DocStore::legacy_package_document_path(&package_id)?;
        if legacy.is_file() && DocStore::claim_legacy_package_migration(&package_id, room_id)? {
            match store.migrate_from(&legacy) {
                Ok(true) => tracing::info!(
                    room = room_id,
                    from = %legacy.display(),
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
        } else if legacy.is_file() {
            tracing::info!(
                room = room_id,
                from = %legacy.display(),
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
pub fn open_migrating(manifest_id: &str, legacy: &Path) -> (State, Autosave) {
    let attempt = || -> Result<(State, Autosave), StoreError> {
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
pub fn try_open(manifest_id: &str) -> Result<(State, Autosave), StoreError> {
    Ok(finish(DocStore::open_with_manifest_id(manifest_id)?))
}

fn finish(mut store: DocStore) -> (State, Autosave) {
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
        return (State::from_doc(doc), Autosave::disabled(reason));
    }
    let mut autosave = Autosave::spawn(store, DEFAULT_DEBOUNCE);
    autosave.seed(&mut doc);
    (State::from_doc(doc), autosave)
}

/// The degraded path: an empty document and a disabled writer.
/// [`Autosave::is_enabled`] returns `false` and
/// [`Autosave::last_error`] carries the reason, so nothing downstream can
/// mistake this for a working store.
fn in_memory_fallback() -> (State, Autosave) {
    (
        State::new(),
        Autosave::disabled("state directory unavailable"),
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
pub fn open_for(package_id: &str) -> Result<(State, Autosave), StoreError> {
    Ok(finish(DocStore::open_for(package_id)?))
}

/// The debounce window [`open`] uses. Re-exported so an app that wants a
/// different one can build its own [`Autosave`] from a [`DocStore`].
pub const DEBOUNCE: Duration = DEFAULT_DEBOUNCE;

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;
    use automerge::{ROOT, ReadDoc, ScalarValue, Value};

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
    #[serial_test::serial(process_env)]
    fn open_keeps_autosave_enabled_when_the_state_dir_already_exists() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let pkg = "so.tilo.persist-open-twice";

        // Directory present before the first open, as on every later launch.
        let dir = tmp.path().join(pkg);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: serialised on `process_env`; no other thread reads the var
        // between the set here and the removal at the end.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
        }

        // Run 1: write and flush a value.
        {
            let (mut state, mut autosave) = open_for(pkg).expect("run 1 open");
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
        let (state, autosave) = open(pkg);
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

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn legacy_record_index_is_imported_by_one_room_only() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let package = "so.tilo.legacy-room-index";
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
        }
        let id = crate::record::RecordId::minted();
        let kind = crate::record::RecordKind::new("note");
        let mut legacy = DocStore::open_for(package).expect("legacy store");
        let mut legacy_doc = legacy.load_or_new();
        crate::record::room::reference(&mut legacy_doc, &id, &kind);
        legacy.save(&mut legacy_doc).expect("write legacy index");
        let source = legacy.document_path().to_path_buf();

        let (a, _) = open_room(package, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert!(crate::record::room::holds(&a.doc, &id));
        let (b, _) = open_room(package, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
        assert!(
            !crate::record::room::holds(&b.doc, &id),
            "a second room must not re-import a package-global index"
        );
        assert!(source.is_file(), "legacy source is retained for recovery");
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    /// An older app layout wrote the document as a zero-byte *file* at the
    /// per-package state path. `open` must recover and come up with
    /// persistence enabled, not swallow the `EEXIST` and fall back to the
    /// disabled in-memory store.
    #[test]
    #[serial_test::serial(process_env)]
    fn open_recovers_when_the_state_path_is_a_stale_file() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let pkg = "so.tilo.stale-file-open";

        // Old layout's leftover: a file where the new layout wants a directory.
        let path = tmp.path().join(pkg);
        std::fs::write(&path, b"").unwrap();
        assert!(path.is_file(), "the leftover is a file, not a directory");
        // SAFETY: serialised on `process_env`.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
            std::env::remove_var("TILO_PACKAGE_ID");
        }

        // Run 1: `open` must recover to enabled persistence.
        {
            let (mut state, mut autosave) = open(pkg);
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
        let (state, autosave) = open(pkg);
        assert!(
            autosave.is_enabled(),
            "persistence stays enabled after recovery"
        );
        assert_eq!(
            note(&state).as_deref(),
            Some("recovered"),
            "state written after recovery must load on the next open"
        );

        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }
}
