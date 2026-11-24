//! The device's one search index, kept by its daemon.
//!
//! An app publishes what it holds on its connection here ([`Publications`]), and the records the
//! device's workspaces hold are read by their apps in record mode as writes land in them
//! ([`RecordIndex`]); both go into one persistent index under the daemon's state. Every caller
//! the daemon answers (the person's shell, `tilo do`, an agent's `find`) searches that index
//! ([`IndexSearch`]), and is answered only what it may read: each result's record is decided by
//! the broker as every read of it is.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::apps::collab_broker::AppActionBroker;
use tilo_workspace_service::apps::services::SearchIndexServices;
use tilo_workspace_service::device_documents::ServiceDocuments;
use tilo_workspace_service::search::records::{RecordIndex, RecordReader};
use tilo_workspace_service::search::{AppContentIndex, IndexSearch};
use tilo_workspace_service::workspaces::Workspaces;

/// Where under the daemon's profile the index is kept.
const INDEX_DIR: &str = "state/search/app-content";

/// The device's search index, the reader of its records into it, and the search of it once the
/// broker that decides who reads what serves.
pub(crate) struct DeviceSearch {
    index: Arc<AppContentIndex>,
    search: OnceLock<Arc<IndexSearch>>,
    _records: Arc<RecordIndex>,
}

impl DeviceSearch {
    /// The index kept under `root`, filled with what `copies` hold as `reader` reads it, and with
    /// every write they take from now on.
    pub(crate) fn open(
        root: &Path,
        copies: &Workspaces,
        reader: Arc<dyn RecordReader>,
    ) -> Arc<Self> {
        let index = AppContentIndex::persistent(root.join(INDEX_DIR));
        let records = RecordIndex::start(
            Arc::new(ServiceDocuments::of(copies.clone())),
            reader,
            Arc::clone(&index),
        );
        let mut writes = copies.document_writes();
        let following = Arc::downgrade(&records);
        tilo_rt::app().handle().spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write) => match following.upgrade() {
                        Some(records) => records.changed(&write.workspace, &write.app, &write.record),
                        None => return,
                    },
                    // A write missed is read with the record's next one.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::debug!(missed, "search missed writes it reads with the next ones");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        Arc::new(Self {
            index,
            search: OnceLock::new(),
            _records: records,
        })
    }

    /// What an app's publications go into.
    pub(crate) fn publications(&self) -> Arc<dyn SearchIndexServices> {
        Arc::new(Publications(Arc::clone(&self.index)))
    }

    /// Search the index from now on, each result decided by `broker` for the caller asking.
    pub(crate) fn serve(&self, broker: &AppActionBroker) {
        let state = Arc::downgrade(broker.state());
        let _ = self.search.set(Arc::new(IndexSearch::new(
            Arc::clone(&self.index),
            Arc::new(move |caller, key| {
                state.upgrade().is_some_and(|state| {
                    state.may_read_record(caller, &key.workspace, &key.app, &key.record)
                })
            }),
        )));
    }

    /// The search of the index, once the broker serves.
    pub(crate) fn search(&self) -> Option<Arc<IndexSearch>> {
        self.search.get().cloned()
    }
}

/// An app's search entries, published on its connection to the daemon, into the device's index.
struct Publications(Arc<AppContentIndex>);

impl SearchIndexServices for Publications {
    fn publish(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        delta: &[u8],
    ) -> Result<(), String> {
        self.0.publish(&workspace.to_string(), app, record, delta)
    }
}
