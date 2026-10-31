//! A record's save state: its on-disk copy in a room this device governs,
//! and whether what the record holds has reached it.
//!
//! The copy is the store's (`app_state`): one document per record,
//! keyed by package, room and record, written by its [`DocWriter`]. The
//! toolkit's record store (`ui::record`) opens and reports records through
//! these; a process without the toolkit does the same.

use std::rc::Rc;

use app_state::{DocStore, DocWriter, StoreError};
use automerge::AutoCommit;
use automerge::transaction::Transactable as _;
use durable_store::{DurableStore, StorageKey};

use crate::portable_storage::{self, PortableStorage};

/// Whether a record's changes are stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordSaveStatus {
    Saved,
    Saving,
    Failed(String),
    MemoryOnly,
}

impl RecordSaveStatus {
    /// The save state of `doc` against the writer of its on-disk copy,
    /// without closing a typing transaction or scheduling work.
    #[must_use]
    pub fn of_writer(doc: &AutoCommit, writer: &DocWriter) -> Self {
        let snapshot = writer.durability_snapshot();
        if let Some(error) = snapshot.error {
            return Self::Failed(error);
        }
        // A record that was never written holds nothing durable; while
        // it has no changes either (a pristine record) there is nothing
        // to save, so it reads as saved rather than saving.
        let durable = snapshot.durable_heads.unwrap_or_default();
        if doc.pending_ops() == 0 && durable == doc.base_heads() {
            Self::Saved
        } else {
            Self::Saving
        }
    }

    /// The save state of `doc`, kept under `key` in a portable cache (a
    /// browser's, acknowledged by its host).
    #[must_use]
    pub fn of_portable(
        cache: &PortableStorage,
        key: &portable_storage::StorageKey,
        doc: &AutoCommit,
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
        if heads != doc.base_heads() {
            return Self::Saving;
        }
        match cache.flush(key) {
            Ok(()) => Self::Saved,
            Err(message) if message == "persistence pending host acknowledgement" => Self::Saving,
            Err(message) => Self::Failed(message),
        }
    }
}

/// The on-disk copy of the record `record_key` of `package` in the room
/// `home`, as a shared writer in `documents`.
///
/// # Errors
/// Refuses a package, room or record name that is not a valid identity, and
/// a store that cannot open the document.
pub fn record_document(
    documents: &Rc<dyn DurableStore>,
    package: &str,
    home: &str,
    record_key: &str,
) -> Result<DocStore, StoreError> {
    app_state::validate_record_identity(package, home, record_key)?;
    DocStore::shared(
        Rc::clone(documents),
        StorageKey::record(package, home, record_key),
    )
}

/// The on-disk copy of the room document of `package` in the room `room`:
/// what an app that keeps one record for the room ([`StorageKey::room_index`])
/// holds there, as a shared writer in `documents`.
///
/// # Errors
/// Refuses a package or room name that is not a valid identity, and a store
/// that cannot open the document.
pub fn room_document(
    documents: &Rc<dyn DurableStore>,
    package: &str,
    room: &str,
) -> Result<DocStore, StoreError> {
    app_state::validate_room_identity(package, room)?;
    DocStore::shared(Rc::clone(documents), StorageKey::room_index(package, room))
}

/// Open the document `store` holds, as a room this device governs reads it:
/// from its on-disk copy, with a writer that keeps it there. A document that
/// left the room ([`super::retirement`]) starts over.
pub fn open_stored(mut store: DocStore) -> (AutoCommit, DocWriter) {
    let mut doc = store.load_or_new();
    // The app's document moved to another workspace, which now holds it
    // with its history; this room's app starts over.
    if super::retirement::is_retired(&doc) {
        doc = store.new_document();
        if let Err(error) = store.save(&mut doc) {
            tracing::error!(%error, "could not start a new app document after a move");
        }
    }
    let mut autosave = store.writer();
    autosave.seed(&mut doc);
    (doc, autosave)
}

/// Open the document `store` holds, as a room another host governs reads it:
/// the broker's committed read of the room (`read`) merged into the on-disk
/// copy, which keeps the merged graph from then on.
///
/// # Errors
/// The copy on disk cannot be read or written, or the read is not a
/// snapshot that loads.
pub fn open_with_committed_read(
    mut store: DocStore,
    read: &crate::CommittedDocument,
) -> Result<(AutoCommit, DocWriter), String> {
    use tilo_automerge::ApplyIncoming as _;
    let stored = store.load_complete().map_err(|e| e.to_string())?;
    let had_stored = stored.is_some();
    let mut doc = stored.unwrap_or_else(|| store.new_document());
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
        let mut actual = AutoCommit::load(bytes).map_err(|e| e.to_string())?;
        doc.merge_incoming(&mut actual).map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    })??;
    writer.request(&mut doc);
    read.applied();
    Ok((doc, writer))
}
