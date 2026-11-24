//! The device's one search index, kept by its daemon.
//!
//! An app publishes what it holds on its connection here ([`Publications`]), and the records the
//! device's workspaces hold are read as writes land in them ([`RecordIndex`]): a Rust app's by the
//! app in its record mode, a tilolang app's resources from the workspace's resource store through
//! the app's installed package ([`InstalledPackages`]). Both go into one persistent index under the
//! daemon's state. Every caller
//! the daemon answers (the person's shell, `tilo do`, an agent's `find`) searches that index
//! ([`IndexSearch`]), and is answered only what it may read: each result's record is decided by
//! the device's one reader as every read of it is, and a result of a resource sealed to its
//! readers is answered to them alone.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use tilo_actions::remote::{ResourceAddress, ResourceReads as _};
use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::apps::services::SearchIndexServices;
use tilo_workspace_service::device_documents::ServiceDocuments;
use tilo_workspace_service::search::records::{RecordIndex, RecordReader, StoreReader};
use tilo_workspace_service::search::{AppContentIndex, IndexSearch};
use tilo_workspace_service::workspaces::{RecordReads, Workspaces};

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
    /// The index kept under `root`, filled with what `copies` hold as `reader` reads it (a
    /// tilolang app's resources as `person` reads them), and with every write they take from now
    /// on.
    pub(crate) fn open(
        root: &Path,
        copies: &Workspaces,
        reader: Arc<dyn RecordReader>,
        person: &str,
    ) -> Arc<Self> {
        let index = AppContentIndex::persistent(root.join(INDEX_DIR));
        let records = RecordIndex::start(
            Arc::new(ServiceDocuments::of(copies.clone())),
            reader,
            Arc::new(InstalledPackages::new(copies, person)),
            Arc::clone(&index),
        );
        let mut writes = copies.document_writes();
        let following = Arc::downgrade(&records);
        tilo_rt::app().handle().spawn(async move {
            loop {
                match writes.recv().await {
                    Ok(write) => match following.upgrade() {
                        Some(records) => {
                            records.changed(&write.workspace, &write.app, &write.record)
                        }
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

    /// Search the index from now on, each result's record decided by the device's one reader
    /// (`reads`) for the caller asking, as a read of that record is.
    pub(crate) fn serve(&self, reads: Arc<RecordReads>) {
        let _ = self.search.set(Arc::new(IndexSearch::new(
            Arc::clone(&self.index),
            Arc::new(move |caller, key| {
                let address = ResourceAddress {
                    workspace: key.workspace.clone(),
                    app: key.app.clone(),
                    record: key.record.clone(),
                };
                // The device's reader decides as it is asked; a hit it would ask about is left out.
                matches!(
                    reads
                        .may_read(caller, &address)
                        .wait_timeout(std::time::Duration::MAX),
                    Ok(Some(Ok(())))
                )
            }),
        )));
    }

    /// The search of the index, once the broker serves.
    pub(crate) fn search(&self) -> Option<Arc<IndexSearch>> {
        self.search.get().cloned()
    }
}

/// The tilolang apps' resources, read from the workspaces' stores through each app's package as
/// this device installed it, verified as a launch verifies it (else as this build carries it),
/// as the device's person reads them: the entries a window of the app publishes, with no window
/// open.
struct InstalledPackages {
    copies: Workspaces,
    person: String,
}

impl InstalledPackages {
    fn new(copies: &Workspaces, person: &str) -> Self {
        Self {
            copies: copies.clone(),
            person: person.to_owned(),
        }
    }
}

impl StoreReader for InstalledPackages {
    /// An installed tilolang app, or one built into this build that the daemon runs for a
    /// workspace with no package of it installed.
    fn reads(&self, app: &str) -> bool {
        super::package_runs::runs_in_process(app)
            || self
                .copies
                .tilolang()
                .is_some_and(|tilolang| tilolang.records.built_in(app))
    }

    fn entries(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<tilo_resource::search_index::IndexEntry>, String> {
        let installed = if super::package_runs::runs_in_process(app) {
            Some(super::package_runs::verified_installed(workspace, app)?.module)
        } else {
            None
        };
        self.copies
            .record_entries(workspace, app, installed.as_ref(), &self.person)
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
