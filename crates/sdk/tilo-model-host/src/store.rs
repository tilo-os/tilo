//! Where a record the document host keeps is stored: the platform's app
//! state, under the workspace the record belongs to.
//!
//! A record an app kept was written by the app's own process to
//! `<data home>/<package-id>/records/<workspace>/<record>/state.automerge`.
//! The document host keeps it at the same place and with the same key, so a
//! record a person already has opens unchanged — the ids in it, its history
//! and its actor sidecar are the ones it was written with (the data
//! invariants), and nothing is copied or rewritten to move ownership of the
//! keep.

/// The document store of one record, opened under the key a record is kept
/// at.
pub struct Kept {
    store: app_state::DocStore,
}

impl Kept {
    /// Open the store of the record `record` of `app`, in `workspace`.
    ///
    /// # Errors
    /// The store refuses the identifiers, or cannot lock the record.
    pub fn open(app: &str, workspace: &str, record: &str) -> Result<Self, String> {
        app_state::DocStore::open_workspace_record(app, workspace, record)
            .map(|store| Self { store })
            .map_err(|error| error.to_string())
    }

    /// What the store holds now, or `None` when it holds nothing yet.
    ///
    /// # Errors
    /// The store cannot read the document it holds.
    pub fn read(&mut self) -> Result<Option<tilo_resource::Doc>, String> {
        self.store.load().map_err(|error| error.to_string())
    }

    /// Keep `doc` as the record now.
    ///
    /// # Errors
    /// The store cannot write it (its disk is full, say).
    pub fn write(&mut self, doc: &mut tilo_resource::Doc) -> Result<(), String> {
        self.store.save(doc).map_err(|error| error.to_string())
    }
}
