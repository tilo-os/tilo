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

use crate::time::Duration;
use std::path::Path;
use std::rc::Rc;
use tilo_automerge::ApplyIncoming;

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
    if let Some(result) = open_native_initial(manifest_id, None) {
        return native_initial_result(result);
    }
    try_open(manifest_id).unwrap_or_else(|e| {
        tracing::error!(error = %e, "app state unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// Open the index for one collaboration room.  Record-aware applications use
/// this instead of [`open()`], so two rooms never hydrate or overwrite each
/// other's record references.
///
/// The host's package id, when set, wins over `manifest_id`.
pub fn open_room(manifest_id: &str, room_id: &str) -> (State, DocWriter) {
    if let Some(result) = open_native_initial(manifest_id, Some(room_id)) {
        return native_initial_result(result);
    }

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
        let store = DocStore::shared(documents, StorageKey::room_index(package_id, room_id))?;
        Ok(finish(store))
    };
    attempt().unwrap_or_else(|error| {
        tracing::error!(room = room_id, error = %error, "room index unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// [`open()`] without the swallow-and-log fallback.
///
/// # Errors
///
/// Returns [`StoreError`] when the state directory can't be opened (a
/// read-only home, a missing `HOME`, or a document a newer build wrote in
/// an incompatible layout).
pub fn try_open(manifest_id: &str) -> Result<(State, DocWriter), StoreError> {
    if let Some(carried) = open_carried(manifest_id) {
        return carried;
    }
    Ok(finish(DocStore::open_with_manifest_id(manifest_id)?))
}

/// A window carried in from another workspace
/// ([`crate::record::transfer`]) opens the record its instance's saved
/// presentation names, rather than the app's own document in this room.
/// `None` for every other launch, which opens the app's document as ever.
fn open_carried(manifest_id: &str) -> Option<Result<(State, DocWriter), StoreError>> {
    let (profile, instance) = crate::application::NativeProfile::of_launch()?;
    let view = match profile.saved_view(instance) {
        Ok(view) => view?,
        Err(error) => {
            tracing::warn!(%error, "saved presentation unreadable; opening the app document");
            return None;
        }
    };
    let package = crate::host::environment()
        .identity
        .package_id
        .clone()
        .unwrap_or_else(|| manifest_id.to_string());
    let id = crate::record::RecordId::parse(&view.record_id)
        .filter(|id| view.app_id == package && !id.is_package_primary())?;
    let key = StorageKey::record(&package, profile.workspace_id(), id.storage_key());
    let mut store = match DocStore::shared(Rc::clone(profile.store()), key) {
        Ok(store) => store,
        Err(error) => return Some(Err(error)),
    };
    match store.load() {
        Ok(Some(mut doc)) if !crate::record::transfer::is_retired(&doc) => {
            let mut autosave = store.writer();
            autosave.seed(&mut doc);
            Some(Ok((State::from_doc(doc), autosave)))
        }
        // Moved on again, or never arrived: this room's own document.
        Ok(_) => None,
        Err(error) => Some(Err(error)),
    }
}

/// Open an explicitly selected canonical profile through the same private
/// native initial-read context when one is installed. The path selects local
/// storage only; it never supplies a document authority or changes read mode.
pub fn open_canonical_at(app: &str, directory: &Path) -> (State, DocWriter) {
    if let Some(capture) = crate::collaboration::native_initial::primary(app, None) {
        return native_initial_result((|| {
            let (_, read) = capture?;
            let store =
                DocStore::open_canonical_at(app, directory).map_err(|error| error.to_string())?;
            let (doc, writer) = finish_canonical_initial(store, read, |_| Ok(()))?;
            Ok((State::from_doc(doc), writer))
        })());
    }
    match DocStore::open_canonical_at(app, directory) {
        Ok(store) => finish(store),
        Err(error) => (State::new(), DocWriter::disabled(error.to_string())),
    }
}

/// Merge an admitted native canonical snapshot into this local canonical
/// cache and continue it with a background writer. Only the previously
/// stored heads are durable; the merged graph stays a pending write.
pub(crate) fn finish_canonical_initial(
    store: DocStore,
    read: crate::collaboration::CommittedDocument,
    validate: impl Fn(&automerge::AutoCommit) -> Result<(), String>,
) -> Result<(automerge::AutoCommit, DocWriter), String> {
    finish_canonical_read(store, read, &validate, &validate)
}

/// The exceptional first graph is admitted only by its live creation capture.
/// Existing local bytes always pass the ordinary identity validator.
pub(crate) fn finish_canonical_created(
    store: DocStore,
    read: crate::collaboration::CommittedDocument,
    validate: impl Fn(&automerge::AutoCommit) -> Result<(), String>,
    validate_created: impl Fn(&automerge::AutoCommit) -> Result<(), String>,
) -> Result<(automerge::AutoCommit, DocWriter), String> {
    finish_canonical_read(store, read, &validate, &validate_created)
}

fn finish_canonical_read(
    mut store: DocStore,
    read: crate::collaboration::CommittedDocument,
    validate: &impl Fn(&automerge::AutoCommit) -> Result<(), String>,
    validate_created: &impl Fn(&automerge::AutoCommit) -> Result<(), String>,
) -> Result<(automerge::AutoCommit, DocWriter), String> {
    let incoming_validator = if read.creation_workspace().is_some() {
        validate_created as &dyn Fn(&automerge::AutoCommit) -> Result<(), String>
    } else {
        validate as &dyn Fn(&automerge::AutoCommit) -> Result<(), String>
    };
    let mut incoming = read.with_mutation(|mutation| {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return Err("native canonical snapshot missing".to_string());
        };
        let doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
        incoming_validator(&doc)
            .map_err(|error| format!("incoming canonical history refused: {error}"))?;
        Ok::<_, String>(doc)
    })??;
    let label = store.document_path().to_path_buf();
    app_state::validate_canonical_schema_of(&incoming, &label)
        .map_err(|error| error.to_string())?;
    let loaded = store.load_complete().map_err(|error| error.to_string())?;
    let mut doc = if let Some(mut existing) = loaded {
        validate(&existing)
            .map_err(|error| format!("existing canonical cache refused: {error}"))?;
        existing
            .merge_incoming(&mut incoming)
            .map_err(|error| error.to_string())?;
        validate(&existing).map_err(|error| format!("merged canonical cache refused: {error}"))?;
        existing
    } else {
        // The absence is solely in this local cache. Its content comes from
        // the already admitted remote Existing read, not a creation grant.
        incoming.set_actor(store.actor().clone());
        incoming
    };
    incoming_validator(&doc)
        .map_err(|error| format!("final canonical history refused: {error}"))?;
    app_state::validate_canonical_schema_of(&doc, &label).map_err(|error| error.to_string())?;
    if let Some(reason) = store.refusal() {
        return Err(reason.to_string());
    }
    let mut writer = store.writer();
    if !writer.is_enabled() {
        return Err(writer
            .last_error()
            .unwrap_or_else(|| "canonical writer unavailable".to_string()));
    }
    writer.request(&mut doc);
    read.applied();
    Ok((doc, writer))
}

fn native_initial_result(result: Result<(State, DocWriter), String>) -> (State, DocWriter) {
    result.unwrap_or_else(|error| {
        crate::collaboration::native_initial::fail(&error);
        (State::new(), DocWriter::disabled(error))
    })
}

fn open_native_initial(
    app: &str,
    room: Option<&str>,
) -> Option<Result<(State, DocWriter), String>> {
    crate::collaboration::native_initial::primary(app, room).map(|capture| {
        let (room, read) = capture?;
        let store = DocStore::open_room(app, &room).map_err(|e| e.to_string())?;
        finish_initial(store, read)
    })
}

fn finish_initial(
    mut store: DocStore,
    read: crate::collaboration::CommittedDocument,
) -> Result<(State, DocWriter), String> {
    let stored = store.load_complete().map_err(|e| e.to_string())?;
    let had_stored = stored.is_some();
    let mut doc = stored.unwrap_or_else(|| store.new_document());
    if let Some(reason) = store.refusal() {
        return Err(reason.to_string());
    }
    let mut writer = store.writer();
    // Only the previously stored heads are durable. The captured graph must
    // remain a pending local-cache write even without a subsequent user edit.
    if had_stored {
        writer.seed(&mut doc);
    }
    read.with_mutation(|mutation| {
        let collab_types::DocumentMutation::Snapshot(bytes) = mutation else {
            return Err("native initial snapshot missing".to_string());
        };
        let mut actual = automerge::AutoCommit::load(bytes).map_err(|e| e.to_string())?;
        doc.merge_incoming(&mut actual).map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    })??;
    writer.request(&mut doc);
    let state = State::from_doc(doc);
    read.applied();
    Ok((state, writer))
}

fn finish(mut store: DocStore) -> (State, DocWriter) {
    let mut doc = store.load_or_new();
    // The app's document moved to another workspace, which now holds it
    // with its history; this room's app starts over.
    if crate::record::transfer::is_retired(&doc) {
        doc = store.new_document();
        if let Err(error) = store.save(&mut doc) {
            tracing::error!(%error, "could not start a new app document after a move");
        }
    }
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

/// Convenience for tests and tools: same as [`open()`] but for an explicit
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

/// The debounce window the native store applies to [`open()`]'s writes.
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

    #[test]
    fn persistence_opens_twice() {
        // Case `open_keeps_autosave_enabled_when_the_state_dir_already_exists`:
        // Opening an app for the SECOND time must keep persistence ON. If the
        // store treated an already-existing state directory as an error,
        // [`open`] would silently fall back to a disabled, in-memory
        // [`in_memory_fallback`] on every run but the first — a case
        // fresh-XDG CI never exercises, so this test creates the directory
        // up front and drives two opens against it.
        {
            let tmp = tempfile::tempdir().expect("temp dir");
            let pkg = "so.tilo.persist-open-twice";

            // Directory present before the first open, as on every later launch.
            let dir = tmp.path().join(pkg);
            std::fs::create_dir_all(&dir).unwrap();

            // Run 1: write and flush a value.
            {
                let (mut state, mut autosave) =
                    open_in(native(tmp.path()), pkg).expect("run 1 open");
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
    }

    #[test]
    fn a_carried_window_opens_its_record_and_a_moved_app_document_starts_over() {
        use crate::record::transfer::{self, Origin, Retirement};
        const PACKAGE: &str = "so.tilo.persist-carry";
        const HOME: &str = "3f2b1c00-0000-4000-8000-0000000000d1";
        const ELSEWHERE: &str = "3f2b1c00-0000-4000-8000-0000000000d2";
        const INSTANCE: u64 = 9;
        let temp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", temp.path())
            .set("TILO_PACKAGE_ID", PACKAGE)
            .set("TILO_WORKSPACE_ID", HOME)
            .remove("TILO_CONTINUITY_INSTANCE");
        {
            let (mut state, mut autosave) = try_open(PACKAGE).unwrap();
            state
                .doc
                .put(&ROOT, "note", ScalarValue::Str("from home".into()))
                .unwrap();
            autosave.request(&mut state.doc);
            autosave.flush();
        }

        // What the shell does when the window moves.
        let store = native(temp.path());
        let origin = Origin::AppDocument(crate::record::RecordKind::new("tilo.app-document"));
        let snapshot = transfer::read(&store, PACKAGE, HOME, &origin).unwrap();
        let mut arrived = transfer::rehome(
            &snapshot,
            &origin,
            HOME,
            ELSEWHERE,
            &crate::record::RecordId::minted(),
        )
        .unwrap();
        let carried = transfer::install(&store, PACKAGE, ELSEWHERE, &mut arrived).unwrap();
        let there = crate::application::NativeProfile::open(temp.path(), Some(ELSEWHERE)).unwrap();
        let view = crate::application_view::SavedInstanceView {
            version: 2,
            app_id: PACKAGE.into(),
            device_id: there.device_id().into(),
            workspace_id: there.workspace_id().into(),
            instance_id: INSTANCE,
            record_id: carried.id.as_str().into(),
            record_kind: carried.kind.as_str().into(),
            view: crate::view::View::default(),
            application: None,
        };
        there.adopt_view(&view, INSTANCE, &carried).unwrap();
        transfer::retire(&store, PACKAGE, HOME, &origin, &Retirement::Moved(None)).unwrap();

        env.set("TILO_WORKSPACE_ID", ELSEWHERE)
            .set("TILO_CONTINUITY_INSTANCE", INSTANCE.to_string());
        {
            let (state, autosave) = try_open(PACKAGE).unwrap();
            assert!(autosave.is_enabled());
            assert_eq!(
                note(&state).as_deref(),
                Some("from home"),
                "the carried window opens what it brought"
            );
        }
        env.set("TILO_CONTINUITY_INSTANCE", (INSTANCE + 1).to_string());
        {
            let (state, _) = try_open(PACKAGE).unwrap();
            assert_eq!(
                note(&state),
                None,
                "a moved app document is never reopened; the app starts over"
            );
        }
    }
}
