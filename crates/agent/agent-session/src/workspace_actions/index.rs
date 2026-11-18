//! What search answers from, kept as the workspaces' documents land.
//!
//! A find ranks what each document's app publishes to the search index
//! ([`ui::search::index_publication::IndexEntry`]). Reading that from the
//! replica opens the document, and a document is expensive to open: a
//! workspace of a hundred notes took seconds to read whole, which is what a
//! person waited for on their first find and again whenever a write had
//! landed. So the service reads each document's entries as the document lands
//! or as the workspace joins, and a find answers from what is kept without
//! opening anything.
//!
//! A notebook's entries cost seconds to read, and the thread that reads them
//! is the one every workspace of the service's share is served on: a request
//! that arrived while such a document was read waited for it, and a write
//! made a listing of another app take a second more than it does. So the
//! entries are read on a thread of their own ([`Warm`]), in a confined worker
//! of its own, and the serving thread only takes what was read between
//! requests and asks for the next. A workspace's folder is small enough to
//! read here, in the short pass [`Worker::warm_files`] takes.

use std::collections::HashSet;
use std::sync::mpsc;

use super::{Arc, Duration, Instant, Ordering, Worker, WorkspaceDocuments, WorkspaceId};
use tilo_peer_state::document_frame::DocumentRoute;
use ui::search::index_publication::IndexEntry;

/// How much of one pass over the folders keeps the thread from the requests queued behind it:
/// short enough that a request never waits on the pass, long enough that a workspace of a
/// hundred documents is indexed within a few seconds of joining.
pub(super) const WARM_SLICE: Duration = Duration::from_millis(15);

/// How often the serving thread looks for what the index's own thread read: it takes what it
/// read between requests, so a workspace of a hundred documents is indexed within a few
/// seconds of joining or of a write landing, and no request waits on the reading.
pub(super) const WARM_POLL: Duration = Duration::from_millis(15);

/// Documents the index's thread reads at once: enough that it keeps reading while a request is
/// served, and few enough that what it reads is taken soon after.
const WARM_IN_FLIGHT: usize = 4;

/// One document to read for the index: its route, the app that publishes its entries, and
/// which workspace it belongs to.
struct WarmJob {
    workspace: WorkspaceId,
    app: String,
    route: DocumentRoute,
    /// The replica the workspace's documents are read from, and how many writes had landed in
    /// the document when it was asked for: entries read of a document that moved since are
    /// dropped.
    engine: Arc<tilo_session_client::engine::Engine>,
    moved: u64,
}

/// What the index's thread read for one document.
struct WarmRead {
    workspace: WorkspaceId,
    route: DocumentRoute,
    moved: u64,
    /// The entries its app publishes at the revision they were read from, or why none were
    /// read.
    entries: Option<([u8; 32], Vec<IndexEntry>)>,
}

/// The thread that reads documents for the search index, off the serving thread.
pub(super) struct Warm {
    jobs: mpsc::Sender<WarmJob>,
    read: mpsc::Receiver<WarmRead>,
    /// The documents asked for and not yet read: each is asked for once.
    in_flight: HashSet<DocumentRoute>,
}

impl Warm {
    /// Whether the index is reading a document now.
    pub(super) fn in_flight(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Start the index's thread: it reads one document at a time in a confined worker of its
    /// own, which no thread serving requests waits for.
    pub(super) fn start() -> Self {
        let (jobs, asked) = mpsc::channel::<WarmJob>();
        let (read, reading) = mpsc::channel::<WarmRead>();
        let spawned = std::thread::Builder::new()
            .name("search-warm".into())
            .spawn(move || warm(asked, &read));
        if let Err(error) = spawned {
            tracing::warn!(%error, "workspace actions: the search index has no thread to read on");
        }
        Self {
            jobs,
            read: reading,
            in_flight: HashSet::new(),
        }
    }
}

/// Read the documents asked for, one at a time, and hand back what each publishes: the
/// index's thread's own loop.
fn warm(jobs: mpsc::Receiver<WarmJob>, read: &mpsc::Sender<WarmRead>) {
    let mut worker: Option<crate::worker::Process> = None;
    for job in jobs {
        let entries = read_entries(&mut worker, &job);
        if read
            .send(WarmRead {
                workspace: job.workspace,
                route: job.route,
                moved: job.moved,
                entries,
            })
            .is_err()
        {
            return;
        }
    }
}

/// What `job`'s app publishes to the search index, read from the replica's saved copy in
/// `worker`: `None` when the document cannot be read now (its keys have not arrived), or the
/// app cannot be asked.
fn read_entries(
    worker: &mut Option<crate::worker::Process>,
    job: &WarmJob,
) -> Option<([u8; 32], Vec<IndexEntry>)> {
    let read = Instant::now();
    let Ok(Some((bytes, revision))) =
        job.engine
            .record_since(&job.route.cluster_id, &job.app, &job.route.document, None)
    else {
        return None;
    };
    let process = match worker.as_mut() {
        Some(process) => process,
        None => match crate::worker::Process::start(crate::worker::proto::Mode::Views) {
            Ok(started) => worker.insert(started),
            Err(reason) => {
                tracing::debug!(%reason, "workspace actions: the index's worker did not start");
                return None;
            }
        },
    };
    let request = crate::worker::proto::Request::Entries {
        app_id: job.app.clone(),
        doc: bytes,
    };
    let entries = match process.call(&request) {
        Ok(crate::worker::proto::Reply::Entries(entries)) => {
            serde_json::from_str::<Vec<IndexEntry>>(&entries)
                .ok()
                .map(|entries| {
                    entries
                        .into_iter()
                        .filter_map(IndexEntry::bounded)
                        .collect::<Vec<_>>()
                })
        }
        Ok(crate::worker::proto::Reply::Failed(reason)) => {
            tracing::debug!(%reason, document = %job.route.document, "workspace actions: a document is not read for search");
            None
        }
        // A worker that answered out of turn is started again next time.
        _ => {
            *worker = None;
            None
        }
    };
    tracing::debug!(
        target: "tilo_agent_session::timing",
        "TIMING search.entries {} app={} document={}",
        read.elapsed().as_micros(),
        job.app,
        job.route.document
    );
    entries.map(|entries| (revision, entries))
}

impl Worker {
    /// Take what the index's thread read, and ask it for what is next: every document search
    /// keeps no entries for, each asked for once while it is in flight. `true` when the index
    /// has nothing more to read now.
    pub(super) fn warm_entries(&mut self) -> bool {
        while let Ok(read) = self.warm.read.try_recv() {
            self.warm.in_flight.remove(&read.route);
            self.shared.search.reading.fetch_sub(1, Ordering::Relaxed);
            let Some(kept) = self
                .workspaces
                .get_mut(&read.workspace)
                .and_then(|workspace| workspace.documents.as_mut())
            else {
                continue;
            };
            match read.entries {
                Some((revision, entries)) => {
                    if kept.warmed(&read.route, (read.moved, revision), entries) {
                        self.shared.search.warmed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // The document could not be read now: it is asked for again once a write
                // lands in it, never in a pass that would ask the same thing for ever.
                None => kept.unreadable(&read.route),
            }
        }
        self.ask_what_is_unread();
        self.warm.in_flight.is_empty() && !self.unindexed_any()
    }

    /// Ask the index's thread for the next documents search keeps no entries for, as many as
    /// it reads at once.
    fn ask_what_is_unread(&mut self) {
        let ids: Vec<WorkspaceId> = self.workspaces.keys().copied().collect();
        for id in ids {
            if self.warm.in_flight.len() >= WARM_IN_FLIGHT {
                return;
            }
            let Some(kept) = self
                .workspaces
                .get_mut(&id)
                .and_then(|workspace| workspace.documents.as_mut())
            else {
                continue;
            };
            let waiting = kept.unindexed(16);
            for route in waiting {
                if self.warm.in_flight.len() >= WARM_IN_FLIGHT {
                    return;
                }
                if self.warm.in_flight.contains(&route) {
                    continue;
                }
                let job = WarmJob {
                    workspace: id,
                    app: route.app_id.clone(),
                    route: route.clone(),
                    engine: kept.engine(),
                    moved: kept.moved(&route),
                };
                if self.warm.jobs.send(job).is_ok() {
                    self.warm.in_flight.insert(route);
                    self.shared.search.reading.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Whether any workspace holds a document search keeps no entries for: what a pass of
    /// [`Self::warm_entries`] reads next.
    pub(super) fn unindexed_any(&mut self) -> bool {
        self.workspaces
            .values_mut()
            .filter_map(|workspace| workspace.documents.as_mut())
            .any(WorkspaceDocuments::has_unindexed)
    }

    /// Read the files of the workspaces' folders that the index has not read yet, for `budget`
    /// at most: each tree document as it landed or as the workspace joined, so a find answers from
    /// what is kept without reading a chunk or a file while the person waits. `true` when no
    /// workspace's folder waits on this thread, or none of what waits could be read now, which
    /// the next request looks at again.
    pub(super) fn warm_files(&mut self, budget: Duration) -> bool {
        let began = Instant::now();
        let ids: Vec<WorkspaceId> = self.workspaces.keys().copied().collect();
        let mut all = true;
        let mut read_any = false;
        'workspaces: for id in ids {
            let Some(engine) = self
                .workspaces
                .get(&id)
                .map(|workspace| Arc::clone(workspace.peer.engine()))
            else {
                continue;
            };
            let Some(cluster) = self
                .workspaces
                .get(&id)
                .and_then(|workspace| workspace.cluster_id(id))
            else {
                continue;
            };
            let content = self.workspace_content(id);
            let counted = &self.shared.search;
            let Some(workspace) = self.workspaces.get_mut(&id) else {
                break 'workspaces;
            };
            while let Some(chunk) = workspace.files.next() {
                let known = workspace.files.known(chunk);
                let chunk_id = fs_protocol::tree::chunk_document_id(chunk);
                let read =
                    engine.record_since(&cluster, fs_protocol::tree::FILES_APP, &chunk_id, known);
                match read {
                    Ok(Some((bytes, revision))) => {
                        let files = workspace
                            .files
                            .read(chunk, &bytes, revision, content.as_ref());
                        counted
                            .files_warmed
                            .fetch_add(files as u64, Ordering::Relaxed);
                        read_any = true;
                    }
                    Ok(None) if known.is_some() => workspace.files.unchanged(chunk),
                    Ok(None) | Err(_) => workspace.files.pending(chunk),
                }
                if began.elapsed() >= budget {
                    all = false;
                    break 'workspaces;
                }
            }
        }
        all || !read_any
    }

    /// Whether any workspace's folder has documents the index has not read yet: what a pass of
    /// [`Self::warm_files`] reads next.
    pub(super) fn files_unwarmed_any(&self) -> bool {
        self.workspaces
            .values()
            .any(|workspace| !workspace.files.settled())
    }
}

impl WorkspaceDocuments {
    /// Entries read off the serving thread for the document at `route` (`at`: how many writes
    /// had landed in it, and the revision it was read from): kept when it still stands there,
    /// and dropped when a write landed in it since, which reads it again.
    pub(crate) fn warmed(
        &mut self,
        route: &DocumentRoute,
        at: (u64, [u8; 32]),
        entries: Vec<IndexEntry>,
    ) -> bool {
        let (moved, revision) = at;
        if moved != self.moved(route) {
            return false;
        }
        self.keep_entries(route, revision, entries.into());
        true
    }
}
