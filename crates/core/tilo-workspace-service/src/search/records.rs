//! The content of the workspaces' records, in the device's search index, without an app of theirs
//! running.
//!
//! A resource is what the workspace keeps; a window is one reading of it. What search finds is
//! therefore the resource's to answer: this reads every record a write lands in, asks the app whose
//! record it is what it holds ([`RecordReader`], the app's own binary in its record mode, which
//! links the model types the daemon does not), and publishes the answer to the same
//! [`AppContentIndex`] a running app publishes to. A device that never opened Chat still finds its
//! messages, a conversation or an agent, and one that deletes a message has it gone from search
//! with it.
//!
//! What is read is what this device holds ([`DocumentSource`]): a workspace it does not keep, a
//! sealed record whose keys are not here, and a record it may not read are all nothing to index —
//! the device's appetite decides, not a list of apps.
//!
//! Writes are batched per record, and the worker reads each record once per pass on its own
//! thread, so a burst of changes to one conversation costs one reading of it.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead};
use tilo_resource::search_index::{IndexEntry, IndexState, Publisher};

use super::{AppContentIndex, IndexKey};

/// The argument an app's binary is started with to read one of its records for search: it reads
/// the record on its input and answers its entries on its output.
pub const INDEX_FLAG: &str = "--index";

/// What the indexer reads: the workspaces this device holds, and the documents within them.
pub trait DocumentSource: Send + Sync + 'static {
    /// The workspaces this device holds.
    fn workspaces(&self) -> Vec<String>;

    /// Every document `workspace` holds, by its app and route.
    ///
    /// # Errors
    /// The workspace is not held here, or its documents cannot be listed.
    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String>;

    /// What `workspace` holds of `app`'s `record` now, or `None` while the keys that open it
    /// are not here.
    ///
    /// # Errors
    /// The record cannot be read now.
    fn record(&self, workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String>;
}

/// The documents of the workspaces this device holds, as its own roles read them.
impl<T: DeviceDocuments + 'static> DocumentSource for T {
    fn workspaces(&self) -> Vec<String> {
        DeviceDocuments::workspaces(self)
    }

    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        self.listing(workspace)
    }

    fn record(&self, workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        match self.read(workspace, app, record, None)? {
            DocumentRead::Whole { bytes, .. } => Ok(Some(bytes)),
            _ => Ok(None),
        }
    }
}

/// What an app answers search about one of its records: the app's own model types, read from the
/// document, as its window reads them.
pub trait RecordReader: Send + Sync + 'static {
    /// What `app`'s record `doc` holds for search.
    ///
    /// # Errors
    /// [`Unread::Stopped`] for a reader that was stopped before it answered, and
    /// [`Unread::Refused`] for a record the app cannot read.
    fn entries(&self, app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, Unread>;
}

/// Why an app's reader did not say what a record of its holds.
#[derive(Debug)]
pub enum Unread {
    /// Whoever read it was stopped before it answered (the daemon stopping, the app closed for
    /// memory), so nothing is known of the record's content: not a record that holds nothing.
    Stopped(String),
    /// The app cannot read the record: it says why.
    Refused(String),
}

/// A record by its workspace, app and route.
type Record = (String, String, String);

/// The records whose content search offers, read from the records themselves.
pub struct RecordIndex {
    source: Arc<dyn DocumentSource>,
    reader: Arc<dyn RecordReader>,
    content: Arc<AppContentIndex>,
    /// The records to read, each once however often it changed.
    queued: Mutex<BTreeSet<Record>>,
    /// The records whose keys were not here when they were read: read again on the next write
    /// to their workspace, which may be the keys arriving.
    sealed: Mutex<BTreeSet<Record>>,
    /// The workspaces this device held when they were last read whole ([`RecordIndex::sweep`]).
    swept: Mutex<BTreeSet<String>>,
    /// Wakes the worker: a record is waiting.
    wake: std::sync::mpsc::Sender<()>,
}

impl RecordIndex {
    /// Index what `source` holds, reading each record through `reader` and publishing to
    /// `content`, from a worker of its own: what this device holds is read at once, and each
    /// record [`Self::changed`] names after.
    pub fn start(
        source: Arc<dyn DocumentSource>,
        reader: Arc<dyn RecordReader>,
        content: Arc<AppContentIndex>,
    ) -> Arc<Self> {
        let (wake, woken) = std::sync::mpsc::channel();
        let index = Arc::new(Self {
            source,
            reader,
            content,
            queued: Mutex::new(BTreeSet::new()),
            sealed: Mutex::new(BTreeSet::new()),
            swept: Mutex::new(BTreeSet::new()),
            wake,
        });
        // The worker reads the documents and the app's reader, neither of which may hold up the
        // daemon's runtime: it wakes only when a record is waiting, and reads each one once.
        let worker = Arc::downgrade(&index);
        let started = std::thread::Builder::new()
            .name("tilo-record-index".into())
            .spawn(move || {
                while woken.recv().is_ok() {
                    let Some(index) = Weak::upgrade(&worker) else {
                        break;
                    };
                    index.sweep();
                    index.pass();
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "the records are not indexed");
        }
        let _ = index.wake.send(());
        index
    }

    /// Queue every record of each workspace this device holds that was not read whole yet, so
    /// search finds it before anything changes.
    fn sweep(&self) {
        for workspace in self.source.workspaces() {
            if self.swept.lock_recover().contains(&workspace) {
                continue;
            }
            match self.source.records(&workspace) {
                Ok(records) => {
                    self.swept.lock_recover().insert(workspace.clone());
                    let mut queued = self.queued.lock_recover();
                    for (app, record) in records.into_iter().filter(|(_, r)| !r.is_empty()) {
                        queued.insert((workspace.clone(), app, record));
                    }
                }
                Err(error) => tracing::warn!(
                    %error,
                    %workspace,
                    "the records search holds are not listed"
                ),
            }
        }
    }

    /// `record` of `app` changed in `workspace`: read it on the next pass, once however often it
    /// changed, and what of the workspace had no keys here again with it. An app's own document of
    /// the workspace (an empty route, a tilolang app's) publishes from its windows, and is read for
    /// nothing here.
    pub fn changed(&self, workspace: &str, app: &str, record: &str) {
        let mut queued = self.queued.lock_recover();
        let mut sealed = self.sealed.lock_recover();
        let unsealed: Vec<Record> = sealed
            .iter()
            .filter(|(held, _, _)| held == workspace)
            .cloned()
            .collect();
        for record in unsealed {
            sealed.remove(&record);
            queued.insert(record);
        }
        if !record.is_empty() {
            queued.insert((workspace.to_owned(), app.to_owned(), record.to_owned()));
        }
        drop((queued, sealed));
        let _ = self.wake.send(());
    }

    /// Read every record queued since the last pass, each once, and publish what it now holds.
    fn pass(&self) {
        let queued: Vec<Record> = std::mem::take(&mut *self.queued.lock_recover())
            .into_iter()
            .collect();
        for (workspace, app, record) in queued {
            let doc = match self.source.record(&workspace, &app, &record) {
                Ok(Some(doc)) => doc,
                // Its keys are not here yet: it is read again with the next write there.
                Ok(None) => {
                    self.sealed.lock_recover().insert((workspace, app, record));
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, %workspace, %app, %record, "a record is not indexed");
                    continue;
                }
            };
            let entries = match self.reader.entries(&app, &doc) {
                Ok(entries) => entries,
                // Stopped before it answered: nothing is known of what the record holds, so it is
                // read again on its next change rather than kept as a record holding nothing.
                Err(Unread::Stopped(reason)) => {
                    tracing::debug!(%reason, %app, %record, "a record's reading was stopped");
                    continue;
                }
                Err(Unread::Refused(error)) => {
                    tracing::warn!(%error, %app, %record, "a record's app reads none of it");
                    continue;
                }
            };
            // The record is the whole of what its app publishes from it: what it no longer holds,
            // deleted messages included, goes with it.
            let deltas = Publisher::default().publish(
                IndexState {
                    generation: 0,
                    secrets_unlocked: None,
                },
                entries,
            );
            let slot = IndexKey::new(&app, &workspace, &record);
            for delta in deltas {
                self.content.apply(&slot, delta);
            }
        }
    }
}

impl tilo_broker_core::services::DocumentIndexChanges for RecordIndex {
    fn changed(&self, workspace: &str, app: &str, record: &str) {
        Self::changed(self, workspace, app, record);
    }
}
