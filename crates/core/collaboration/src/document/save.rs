//! A record's save state: its on-disk copy in a workspace this device governs,
//! and whether what the record holds has reached it.
//!
//! The copy is the store's (`app_state`): one document per record,
//! keyed by package, workspace and record, written by its [`DocWriter`]. The
//! toolkit's record store (`ui::resource`) opens and reports records through
//! these; a process without the toolkit does the same.

use std::rc::Rc;

use app_state::{DocStore, DocWriter, StoreError};
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use durable_store::{DurableStore, StorageKey};

use crate::history_source::authors::DeviceActors;
use crate::portable_storage::{self, PortableStorage};
use crate::state::WritingActors;

/// Whether a record's changes are stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceSaveStatus {
    Saved,
    Saving,
    Failed(String),
    MemoryOnly,
}

/// Whether `stored` holds every change the person made here: nothing `doc`
/// holds beyond it was written by the document's own writer, one of this
/// replica's writers (`writers`), or a handle of this device. Content the
/// workspace sent is durable where it came from, so this device still writing
/// its copy of it is not the person's unsaved work.
///
/// The genesis of a record is this device's too: it is the record's creation,
/// made here when the person made the record.
///
/// `writers` is `None` for a process that does not track which changes it
/// wrote: then every change beyond `stored` is taken to be the person's.
///
/// A `stored` baseline the document no longer holds (a withdrawal rebuilt it)
/// covers nothing: what it names is not here to judge.
fn stored_covers_local_changes(
    doc: &AutoCommit,
    stored: &[automerge::ChangeHash],
    writers: Option<&WritingActors>,
) -> bool {
    if stored == doc.base_heads() {
        return true;
    }
    if !stored.iter().all(|hash| doc.has_change(hash)) {
        return false;
    }
    let Some(writers) = writers else {
        return false;
    };
    let actor = doc.get_actor().clone();
    let device = DeviceActors::of(&actor);
    let genesis = crate::resource::genesis_actor(doc);
    !doc.committed_document()
        .get_changes_meta(stored)
        .iter()
        .any(|change| {
            let writer = change.actor.as_ref();
            writer == &actor
                || genesis.as_ref() == Some(writer)
                || writers.contains(writer)
                || device.wrote(&writer.to_hex_string())
        })
}

impl ResourceSaveStatus {
    /// The save state of `doc` against the writer of its on-disk copy,
    /// without closing a typing transaction or scheduling work. `writers` are
    /// the writers this replica has written the record as besides the
    /// document's own actor ([`Self::of_portable`] for a process without them).
    #[must_use]
    pub fn of_writer(
        doc: &AutoCommit,
        writer: &DocWriter,
        writers: Option<&WritingActors>,
    ) -> Self {
        let snapshot = writer.durability_snapshot();
        if let Some(error) = snapshot.error {
            return Self::Failed(error);
        }
        // A record that was never written holds nothing durable; while
        // it has no changes either (a pristine record) there is nothing
        // to save, so it reads as saved rather than saving.
        let durable = snapshot.durable_heads.unwrap_or_default();
        if doc.pending_ops() == 0 && stored_covers_local_changes(doc, &durable, writers) {
            Self::Saved
        } else {
            Self::Saving
        }
    }

    /// The save state of `doc`, kept under `key` in a portable cache (a
    /// browser's, acknowledged by its host). `writers` as for [`Self::of_writer`].
    #[must_use]
    pub fn of_portable(
        cache: &PortableStorage,
        key: &portable_storage::StorageKey,
        doc: &AutoCommit,
        writers: Option<&WritingActors>,
    ) -> Self {
        if doc.pending_ops() != 0 {
            return Self::Saving;
        }
        let Some(heads) = cache.document_heads(key) else {
            return Self::Saving;
        };
        let Ok(heads) = heads else {
            return Self::Failed("cached document is corrupt".into());
        };
        if !stored_covers_local_changes(doc, &heads, writers) {
            return Self::Saving;
        }
        match cache.flush(key) {
            Ok(()) => Self::Saved,
            Err(message) if message == portable_storage::PENDING_ACKNOWLEDGEMENT => Self::Saving,
            Err(message) => Self::Failed(message),
        }
    }
}

/// The on-disk copy of the record `resource_key` of `package` in the workspace
/// `home`, as a shared writer in `documents`.
pub fn resource_document(
    documents: &Rc<dyn DurableStore>,
    package: &str,
    home: &str,
    resource_key: &str,
) -> Result<DocStore, StoreError> {
    app_state::validate_document_identity(package, home, resource_key)?;
    DocStore::shared(
        Rc::clone(documents),
        StorageKey::document(package, home, resource_key),
    )
}

/// The on-disk copy of the workspace document of `package` in the workspace `workspace`:
/// what an app that keeps one record for the workspace ([`StorageKey::workspace_index`])
/// holds there, as a shared writer in `documents`.
pub fn workspace_document(
    documents: &Rc<dyn DurableStore>,
    package: &str,
    workspace: &str,
) -> Result<DocStore, StoreError> {
    app_state::validate_workspace_identity(package, workspace)?;
    DocStore::shared(
        Rc::clone(documents),
        StorageKey::workspace_index(package, workspace),
    )
}

/// Open the document `store` holds, as a workspace this device governs reads it:
/// from its on-disk copy, with a writer that keeps it there. A document that
/// left the workspace ([`super::retirement`]) starts over.
pub fn open_stored(mut store: DocStore) -> (AutoCommit, DocWriter) {
    let mut doc = store.load_or_new();
    // The app's document moved to another workspace, which now holds it
    // with its history; this workspace's app starts over.
    if super::retirement::is_retired(&doc) {
        doc = store.new_document();
        if let Err(error) = store.save(&mut doc) {
            tracing::error!(%error, "could not start a new app document after a move");
        }
    }
    let mut autosave = store.writer();
    autosave.seed(&doc);
    (doc, autosave)
}
