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
