//! The content of the workspaces' records, in the search index, without an
//! app of theirs running.
//!
//! A record is what the workspace keeps; a window is one reading of it. What
//! search finds is therefore the record's to answer: this reads every record
//! a change lands in, asks the app whose record it is what it holds
//! ([`RecordReader`], the app's own binary in its record mode, which links
//! the model types this session does not), and publishes the answer to the
//! same [`AppContentIndex`] a running window publishes to. A device that
//! never opened Chat still finds its messages, a conversation or an agent,
//! and one that deletes a message has it gone from search with it.
//!
//! What is read is what this device holds: [`RecordSource`] is the session's
//! owner, so a workspace this device does not keep, a sealed record whose
//! keys are not here, and a record it may not read are all nothing to index —
//! the device's appetite decides, not a list of apps.
//!
//! Changes arrive as applies ([`RecordChanges`]): an app's write committed
//! here, or a peer's change taken in. They are batched per record, and the
//! worker indexes each record once per pass on its own thread, so a burst of
//! changes to one conversation costs one reading of it and never holds up a
//! frame.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

use tilo_desktop_search::content_index::{AppContentIndex, SlotKey};
use tilo_lock::MutexRecover as _;
use ui::search::index_publication::{IndexEntry, IndexState, Publisher};

/// What the indexer reads: the workspaces this device holds, and the records within them, as the
/// owner last admitted them.
pub(crate) trait RecordSource: Send + Sync + 'static {
    /// The workspaces this device holds.
    fn workspaces(&self) -> Vec<String>;

    /// Every record `workspace` holds, by its app and id.
    ///
    /// # Errors
    /// The workspace is not held here, or its documents cannot be listed.
    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String>;

    /// What `workspace` holds of `app`'s `record` now, or `None` when there
    /// is nothing new to read or the device may not read it.
    ///
    /// # Errors
    /// The record cannot be read now.
    fn record(&self, workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String>;
}

impl RecordSource for crate::engine::Engine {
    fn workspaces(&self) -> Vec<String> {
        self.workspaces()
            .into_iter()
            .map(|workspace| workspace.to_string())
            .collect()
    }

    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        self.records(workspace)
    }

    fn record(&self, workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        self.record(workspace, app, record)
    }
}

/// What an app answers search about one of its records: the app's own model types, read from the
/// document, as its window reads them.
pub(crate) trait RecordReader: Send + Sync + 'static {
    /// What `app`'s record `doc` holds for search.
    ///
    /// # Errors
    /// The app cannot read the document now.
    fn entries(&self, app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, String>;
}

/// The reader this device asks: the app's own binary in its record mode, as every app of a
/// workspace starts, through the launcher.
pub(crate) struct AppReader {
    launcher: Option<std::path::PathBuf>,
}

impl AppReader {
    /// The reader of the apps installed on this device.
    #[must_use]
    pub(crate) fn of_device() -> Self {
        Self {
            launcher: tilo_app_launch::PlanContext::find_launcher(),
        }
    }
}

impl RecordReader for AppReader {
    fn entries(&self, app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, String> {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let launcher = self
            .launcher
            .as_deref()
            .ok_or("no tilo-launcher is installed, so no app's records are read")?;
        let installed = app_catalogue::apps::read_one_from_env(app)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("{app} is not installed here"))?;
        let mut child = Command::new(launcher)
            .arg("run")
            .arg("--manifest")
            .arg(&installed.manifest_path)
            .arg("--payload-root")
            .arg(&installed.payload_root)
            .arg("--app-id")
            .arg(&installed.package_id)
            .arg("--no-services")
            .arg("--")
            .arg(tilo_app::framework::record_index::INDEX_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{app} cannot read its record: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(doc)
                .map_err(|error| format!("{app} reads none of its record: {error}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|error| format!("{app} stops without reading its record: {error}"))?;
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "{app} reads none of its record ({}): {}",
                out.status,
                said.trim()
            ));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|error| format!("{app} answers no entries: {error}"))
    }
}

/// The records whose content search offers, read from the records themselves.
pub(crate) struct RecordIndex {
    source: Arc<dyn RecordSource>,
    reader: Arc<dyn RecordReader>,
    /// The index search reads; `None` on a session that keeps none.
    content: Option<Arc<AppContentIndex>>,
    /// The records to read, each once however often it changed.
    queued: Mutex<BTreeSet<(String, String, String)>>,
    /// Wakes the worker: a record is waiting.
    wake: std::sync::mpsc::Sender<()>,
    /// What a change of a record reaches besides the index: the roles that follow records, such as
    /// the person's mentions. The queue is where the session already learns of every change, so no
    /// role watches the store itself.
    touched: crate::platform::records::Touched,
}

impl RecordIndex {
    /// Index what `source` holds, reading each record through `reader` and publishing to `content`
    /// , and follow every later change.
    pub(crate) fn start(
        source: Arc<dyn RecordSource>,
        reader: Arc<dyn RecordReader>,
        content: Option<Arc<AppContentIndex>>,
        touched: crate::platform::records::Touched,
    ) -> Arc<Self> {
        let (wake, woken) = std::sync::mpsc::channel();
        let index = Arc::new(Self {
            source,
            reader,
            content,
            queued: Mutex::new(BTreeSet::new()),
            wake,
            touched,
        });
        // The worker reads the owner and the app's reader, neither of which may hold up the
        // session's frame: it wakes only when a record is waiting, and reads each one once.
        let worker = Arc::downgrade(&index);
        let started = std::thread::Builder::new()
            .name("tilo-record-index".into())
            .spawn(move || {
                while woken.recv().is_ok() {
                    let Some(index) = Weak::upgrade(&worker) else {
                        break;
                    };
                    index.pass();
                }
            });
        if let Err(error) = started {
            tracing::warn!(%error, "the records are not indexed");
        }
        index.sweep();
        index
    }

    /// Read what this device already holds, once: the records of every workspace it keeps, so
    /// search finds them before anything changes.
    pub(crate) fn sweep(&self) {
        for workspace in self.source.workspaces() {
            match self.source.records(&workspace) {
                Ok(records) => {
                    for (app, record) in records {
                        self.changed(&workspace, &app, &record);
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

    /// Queue `record` for the next pass, once, however often it changed, and tell the roles that
    /// follow records that it did.
    pub(crate) fn changed(&self, workspace: &str, app: &str, record: &str) {
        if record.is_empty() || app.is_empty() {
            return;
        }
        self.touched.note(workspace, app, record);
        self.queued.lock_recover().insert((
            workspace.to_owned(),
            app.to_owned(),
            record.to_owned(),
        ));
        let _ = self.wake.send(());
    }

    /// Read every record queued since the last pass, each once, and publish what it now holds.
    pub(crate) fn pass(&self) {
        let Some(content) = &self.content else {
            self.queued.lock_recover().clear();
            return;
        };
        let queued: Vec<(String, String, String)> =
            std::mem::take(&mut *self.queued.lock_recover())
                .into_iter()
                .collect();
        for (workspace, app, record) in queued {
            let doc = match self.source.record(&workspace, &app, &record) {
                Ok(Some(doc)) => doc,
                // Unchanged since it was last read, or nothing this device holds: what search keeps
                // of it is what it holds.
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, %workspace, %app, %record, "a record is not indexed");
                    continue;
                }
            };
            let entries = match self.reader.entries(&app, &doc) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(%error, %app, %record, "a record's app reads none of it");
                    continue;
                }
            };
            // The record is the whole of what its app publishes from it: what it no longer holds,
            // deleted messages included, goes with it.
            let mut publisher = Publisher::default();
            let deltas = publisher.publish(
                IndexState {
                    generation: 0,
                    secrets_unlocked: None,
                },
                entries,
            );
            let slot = SlotKey::new(&app, &workspace, &record);
            for delta in deltas {
                content.apply(&slot, delta);
            }
        }
    }

    /// The records waiting for the next pass: what a test reads to see that a change that reads
    /// nothing new waits for none.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.queued.lock_recover().len()
    }
}

impl tilo_broker_core::services::RecordIndexChanges for RecordIndex {
    fn changed(&self, workspace: &str, app: &str, record: &str) {
        Self::changed(self, workspace, app, record);
    }
}

#[cfg(test)]
mod tests;
