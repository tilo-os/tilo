//! The content of the workspaces' records, in the search index, without an
//! app of theirs running.
//!
//! A resource is what the workspace keeps; a window is one reading of it. What
//! search finds is therefore the resource's to answer: this reads every one
//! a change lands in, asks the app whose resource it is what it holds
//! ([`RecordReader`], the app's own binary in its record mode, which links
//! the model types this session does not), and publishes the answer to the
//! same [`AppContentIndex`] a running window publishes to. A device that
//! never opened Chat still finds its messages, a conversation or an agent,
//! and one that deletes a message has it gone from search with it.
//!
//! What is read is what this device holds: [`DocumentSource`] is the session's
//! owner, so a workspace this device does not keep, a sealed record whose
//! keys are not here, and a record it may not read are all nothing to index —
//! the device's appetite decides, not a list of apps.
//!
//! Changes arrive as applies ([`ResourceChanges`]): an app's write committed
//! here, or a peer's change taken in. They are batched per record, and the
//! worker indexes each record once per pass on its own thread, so a burst of
//! changes to one conversation costs one reading of it and never holds up a
//! frame.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, Weak};

use tilo_desktop_search::content_index::{AppContentIndex, IndexKey};
use tilo_lock::MutexRecover as _;
use ui::search::index_publication::{IndexEntry, IndexState, Publisher};

/// What the indexer reads: the workspaces this device's service holds, and the documents within
/// them, as the service last took them in.
pub(crate) trait DocumentSource: Send + Sync + 'static {
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

/// [`DocumentSource`] over the documents of the workspaces this device's service holds: what
/// search reads, with no app of theirs running.
pub(crate) struct ServiceSource(
    pub(crate) Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
);

impl DocumentSource for ServiceSource {
    fn workspaces(&self) -> Vec<String> {
        self.0.workspaces()
    }

    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        self.0.listing(workspace)
    }

    fn record(&self, workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        use tilo_peer_state::workspace_documents::DocumentRead;
        match self.0.read(workspace, app, record, None) {
            Ok(DocumentRead::Whole { bytes, .. }) => Ok(Some(bytes)),
            Ok(_) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The indexer of the documents the workspaces this device's service holds keep, reading content
/// into `content` and taking what `changes` notes: what this device holds is found before
/// anything changes, and every change after it. The stream notes only those workspaces: a change
/// of one the service does not hold is no news to any role that follows it, and is taken when the
/// service holds the workspace.
pub(crate) fn start(
    reads: &Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    content: Arc<tilo_desktop_search::content_index::AppContentIndex>,
    changes: crate::platform::records::Changes,
) -> Arc<RecordIndex> {
    changes.holding(Arc::new({
        let reads = Arc::clone(reads);
        move |workspace: &str| reads.workspaces().iter().any(|held| held == workspace)
    }));
    RecordIndex::start(
        Arc::new(ServiceSource(Arc::clone(reads))) as Arc<dyn DocumentSource>,
        Arc::new(AppReader::of_device()),
        Some(content),
        changes,
    )
}

/// What an app answers search about one of its records: the app's own model types, read from the
/// document, as its window reads them.
pub(crate) trait RecordReader: Send + Sync + 'static {
    /// What `app`'s record `doc` holds for search.
    ///
    /// # Errors
    /// [`Unread::Stopped`] for a reader that was stopped before it answered, and
    /// [`Unread::Refused`] for a record the app cannot read.
    fn entries(&self, app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, Unread>;
}

/// Why an app's reader did not say what a record of its holds.
#[derive(Debug)]
pub(crate) enum Unread {
    /// Whoever read it was stopped before it answered — a session ending, the app closed for
    /// memory — so nothing is known of the record's content: not a record that holds nothing.
    Stopped(String),
    /// The app cannot read the record: it says why.
    Refused(String),
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
    fn entries(&self, app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, Unread> {
        use std::io::Write as _;
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::{Command, Stdio};

        let launcher = self.launcher.as_deref().ok_or_else(|| {
            Unread::Refused("no tilo-launcher is installed, so no app's records are read".into())
        })?;
        let installed = app_catalogue::apps::read_one_from_env(app)
            .map_err(|error| Unread::Refused(error.to_string()))?
            .ok_or_else(|| Unread::Refused(format!("{app} is not installed here")))?;
        let mut child = Command::new(launcher)
            .arg("run")
            .arg("--manifest")
            .arg(&installed.manifest_path)
            .arg("--payload-root")
            .arg(&installed.payload_root)
            .arg("--app-id")
            .arg(installed.app_id.as_str())
            .arg("--no-services")
            .arg("--")
            .arg(tilo_app::framework::resource_index::INDEX_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Unread::Refused(format!("{app} cannot read its record: {error}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            // A reader stopped before it read any of the record leaves the pipe it writes to
            // closed: nothing read, rather than anything about the record.
            let read = stdin.write_all(doc).and_then(|()| stdin.flush());
            if let Err(error) = read {
                if error.kind() == std::io::ErrorKind::BrokenPipe {
                    return Err(Unread::Stopped(format!(
                        "{app} was stopped before it read its record"
                    )));
                }
                return Err(Unread::Refused(format!(
                    "{app} reads none of its record: {error}"
                )));
            }
        }
        let out = child.wait_with_output().map_err(|error| {
            Unread::Refused(format!("{app} stops without reading its record: {error}"))
        })?;
        if !out.status.success() {
            let said = String::from_utf8_lossy(&out.stderr);
            let said = format!(
                "{app} reads none of its record ({}): {}",
                out.status,
                said.trim()
            );
            // What ends a reader from outside — the session going away, an app closed for
            // memory — says nothing about the record it was reading.
            return Err(match out.status.signal() {
                Some(libc::SIGTERM | libc::SIGINT | libc::SIGHUP | libc::SIGKILL) => {
                    Unread::Stopped(said)
                }
                _ => Unread::Refused(said),
            });
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|error| Unread::Refused(format!("{app} answers no entries: {error}")))
    }
}

/// The records whose content search offers, read from the records themselves.
pub(crate) struct RecordIndex {
    source: Arc<dyn DocumentSource>,
    reader: Arc<dyn RecordReader>,
    /// The index search reads; `None` on a session that keeps none.
    content: Option<Arc<AppContentIndex>>,
    /// The records to read, each once however often it changed.
    queued: Mutex<BTreeSet<(String, String, String)>>,
    /// The records whose keys were not here when they were read: read again once the channels
    /// settled.
    sealed: Mutex<BTreeSet<(String, String, String)>>,
    /// The workspaces this device held when they were last read whole ([`RecordIndex::sweep`]).
    swept: Mutex<BTreeSet<String>>,
    /// Wakes the worker: a record is waiting.
    wake: std::sync::mpsc::Sender<()>,
    /// What a change of a record reached besides the index: the one stream the roles that follow
    /// records read, such as the person's mentions. The queue is where the session already learns
    /// of every change an app or a peer made, so no role watches the store itself.
    changes: crate::platform::records::Changes,
}

impl RecordIndex {
    /// Index what `source` holds, reading each record through `reader` and publishing to `content`
    /// , and follow every later change.
    pub(crate) fn start(
        source: Arc<dyn DocumentSource>,
        reader: Arc<dyn RecordReader>,
        content: Option<Arc<AppContentIndex>>,
        changes: crate::platform::records::Changes,
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
            changes,
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
        index
    }

    /// Read what this device already holds, once the key service that opens its sealed documents
    /// runs: everything every workspace it keeps holds, so search finds it before anything
    /// changes. What it holds is not news to the roles following the stream: these reached them
    /// with the workspace's own replay. A workspace read before is not read again.
    pub(crate) fn sweep(&self) {
        for workspace in self.source.workspaces() {
            if self.swept.lock_recover().contains(&workspace) {
                continue;
            }
            match self.source.records(&workspace) {
                Ok(records) => {
                    self.swept.lock_recover().insert(workspace.clone());
                    for (app, record) in records {
                        self.reached(&workspace, &app, &record, true);
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

    /// The keys settled: the workspaces this device holds now are read, what had no keys here is
    /// read again, and what still has none waits for the next settling.
    pub(crate) fn settled(&self) {
        self.changes.settling();
        self.sweep();
        let sealed = std::mem::take(&mut *self.sealed.lock_recover());
        if sealed.is_empty() {
            return;
        }
        self.queued.lock_recover().extend(sealed);
        let _ = self.wake.send(());
    }

    /// Queue `record` for the next pass, once, however often it changed, and note the change in
    /// the stream the roles following records read.
    pub(crate) fn changed(&self, workspace: &str, app: &str, record: &str) {
        self.reached(workspace, app, record, false);
    }

    /// Note that `record` of `app` changed in `workspace`, as `replayed` where it is what the
    /// device already held, and queue it for the next pass. Nothing of a workspace this device
    /// does not hold is queued: it is read by [`Self::sweep`] when the workspace becomes held.
    fn reached(&self, workspace: &str, app: &str, record: &str, replayed: bool) {
        // An app's own document of the workspace (an empty route: a tilolang app's, which its
        // windows write) is news to whoever follows it, the session's own copy of it among them;
        // search reads what has a route of its own, and queues it for none.
        if !self.changes.note(workspace, app, record, replayed) {
            return;
        }
        if record.is_empty() {
            return;
        }
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
                // Its keys are not here yet: it is read once the channels settled.
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
                // read again on the next change or settling rather than kept as a record holding
                // nothing.
                Err(Unread::Stopped(reason)) => {
                    tilo_failure::ignored!(
                        "nobody answered yet, and the record waits to be read",
                        tracing::debug!(%reason, %app, %record, "a record's reading was stopped")
                    );
                    self.queued.lock_recover().insert((workspace, app, record));
                    continue;
                }
                Err(Unread::Refused(error)) => {
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
            let slot = IndexKey::new(&app, &workspace, &record);
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

impl tilo_broker_core::services::DocumentIndexChanges for RecordIndex {
    fn changed(&self, workspace: &str, app: &str, record: &str) {
        Self::changed(self, workspace, app, record);
    }
}

#[cfg(test)]
mod tests;

/// Guard results already published before a workspace's document access changed.
pub(crate) fn install_read_policy(
    engine: &Arc<crate::engine::Engine>,
    content: &Arc<AppContentIndex>,
) {
    let reader = Arc::downgrade(engine);
    content.set_read_policy(
        move |key| {
            let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&key.workspace) else {
                return true;
            };
            reader
                .upgrade()
                .is_some_and(|engine| engine.document_read_policy(workspace)(&key.app, &key.record))
        },
        tilo_identity::policy_change::epoch,
    );
}
