//! A workspace's file tree on a device: the Files app's records, held in this device's own
//! workspace service as every device holding a member copy of the workspace holds them.
//!
//! The records of a workspace's tree ([`fs_protocol::tree`]) and the device's
//! [`tilo_fs::Namespace`] of it are kept converged by [`tilo_fs::tree_sync`], which knows the
//! order a tree goes out in, its pacing and what it does when the workspace refuses a change.
//! This module wires that sync to the member copy: its reads ([`ServiceDocuments`]), its writes
//! as this device ([`ServiceWrites`]), the serving device's word that it holds them
//! ([`tilo_peer_state::workspace_documents::DeviceWrites::served`]), and the writes the serving device sends the copy
//! ([`Workspaces::document_writes`]).
//!
//! Two answers decide what a sync may found, and both come from the copy alone: a record is
//! absent from the workspace only once the copy caught up with the device serving it
//! ([`Workspaces::member_caught_up`]) and still reads absent; and a record the copy holds is
//! held by the workspace only once the serving device said it holds every write of this
//! device's to it.
//!
//! The bytes a tree's files are made of travel beside its records, on the record's route
//! ([`crate::content`]): [`StoreShelf`] answers for them from a device's content store, and
//! [`WorkspaceSource`] asks the workspace for what a read lacks.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use fs_protocol::TreeChunk;
use iroh_blobs::Hash;
use tilo_fs::hydration::ContentSource;
use tilo_fs::tree_sync::{FILES_APP, Reading, TreeSync, Wake};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DeviceWrites as _, DocumentRead};
use tilo_server_protocol::{DocumentAnswer, WorkspaceId};
use tokio::sync::broadcast::error::RecvError;

use crate::content::{ContentScope, ContentShelf, ContentState, CrdtRoute, RouteContent};
use crate::device_documents::{ServiceDocuments, ServiceWrites};
use crate::workspaces::Workspaces;
use tilo_fs::tree_sync::Refusal;
use tilo_peer_state::owner::WriteRefusal;

pub mod access;
mod folders;
pub mod paths;
mod puts;
pub mod socket;

pub use folders::{FOLDERS_APP, Folder, FolderList, Folders};
pub use paths::{Entry, EntryKind, Paths};
pub use puts::Puts;

/// How long the serving device's word that it holds a write is waited for. It answers only the
/// writes it decides on: one it keeps while it waits for something it lacks gets no word, so a
/// wait that ends unanswered sends the record's changes again (a change it holds already merges
/// as nothing) rather than counting them held.
const SERVED_WITHIN: Duration = Duration::from_secs(20);

/// How often the workspace's holders' word on its content is read for a folder.
const HOLDERS_EVERY: Duration = Duration::from_millis(250);

/// The route a record of the tree is kept under.
fn route(
    workspace: WorkspaceId,
    chunk: TreeChunk,
) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace.to_string(),
        document: tilo_fs::tree_sync::document_for(chunk),
    }
}

/// Hold `workspace`'s tree in `workspaces`, this device's own service, kept in `kept_at` between
/// runs, as the device whose key is `device`; its file bytes are kept in `store` and asked of
/// the workspace through `content`, where this device keeps and asks for them.
///
/// # Errors
/// The kept namespace cannot be read.
pub async fn start(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    kept_at: &Path,
    (device, person): ([u8; 32], [u8; 32]),
    store: Option<Arc<tilo_fs::ContentStore>>,
    content: Option<Arc<RouteContent>>,
) -> std::io::Result<TreeSync> {
    // A workspace this device governs has no device serving it: its copy here is the workspace.
    let governed = workspaces.governed_copy(workspace).await.is_some();
    let backend = Backend::of(workspaces, workspace, content, governed);
    TreeSync::start(tilo_fs::tree_sync::Config {
        workspace: backend.copy.name.clone(),
        device,
        person,
        kept_at: kept_at.to_path_buf(),
        documents: Arc::new(backend.reads),
        writer: Box::new(backend.writer),
        peer: backend.copy,
        content: store,
        wakes: backend.wakes,
    })
    .await
}

/// What a tree's sync reads, writes and hears of one workspace in this device's service.
struct Backend {
    copy: Arc<Copy>,
    reads: Records,
    writer: Writer,
    wakes: tokio::sync::mpsc::UnboundedReceiver<Wake>,
}

impl Backend {
    /// `workspace`'s copy in `workspaces`, followed until the wakes are no longer taken.
    fn of(
        workspaces: &Workspaces,
        workspace: WorkspaceId,
        content: Option<Arc<RouteContent>>,
        governed: bool,
    ) -> Self {
        let copy = Arc::new(Copy {
            workspaces: workspaces.clone(),
            documents: ServiceDocuments::of(workspaces.clone()),
            writes: ServiceWrites::of(workspaces.clone()),
            workspace,
            governed,
            name: workspace.to_string(),
            answers: Mutex::new(BTreeMap::new()),
            unserved: Arc::new(AtomicUsize::new(0)),
            content,
            asked: tokio::sync::Notify::new(),
            lacks: Mutex::new(None),
        });
        let (wakes, wake_events) = tokio::sync::mpsc::unbounded_channel();
        // Follow the copy's news before the sync reads a resource, so a write the serving device
        // sends meanwhile is heard; what it tells waits in the channel until the sync takes it.
        tokio::spawn(follow(
            Arc::clone(&copy),
            workspaces.document_writes(),
            wakes.clone(),
        ));
        Self {
            reads: Records(Arc::clone(&copy)),
            writer: Writer {
                copy: Arc::clone(&copy),
                wakes,
            },
            copy,
            wakes: wake_events,
        }
    }
}

/// This device's copy of one workspace, a member copy or the one it governs, as its tree's sync
/// reads and writes it.
struct Copy {
    workspaces: Workspaces,
    documents: ServiceDocuments,
    writes: ServiceWrites,
    workspace: WorkspaceId,
    /// Whether this device governs the workspace: then what the copy holds is what the
    /// workspace holds, with no serving device to catch up with.
    governed: bool,
    name: String,
    /// The records of the tree the sync opened, each with what the copy answered of it: `None`
    /// until it can say.
    answers: Mutex<BTreeMap<TreeChunk, Option<DocumentAnswer>>>,
    /// This device's writes the serving device has not said it holds yet.
    unserved: Arc<AtomicUsize>,
    content: Option<Arc<RouteContent>>,
    /// Poked when the sync opens a record, so its answer comes now rather than at the next look.
    asked: tokio::sync::Notify,
    /// What the workspace's holders lack of its content, as [`Copy::read_host_lacks`] last read
    /// it.
    lacks: Mutex<Option<u32>>,
}

impl Copy {
    /// What the copy answers of `chunk` now, when it can say: the workspace holds none of it only
    /// once the copy caught up with the serving device and the record still reads absent; it
    /// holds it once the copy does and the serving device holds every write of this device's to
    /// it.
    fn answer_now(&self, chunk: TreeChunk) -> Option<DocumentAnswer> {
        let caught_up = self.governed
            || self
                .workspaces
                .member_caught_up(self.workspace)
                .is_some_and(|caught_up| *caught_up.borrow());
        if !caught_up {
            return None;
        }
        let document = tilo_fs::tree_sync::document_for(chunk);
        if !self.documents.holds(&self.name, FILES_APP, &document) {
            return match self.documents.read(&self.name, FILES_APP, &document, None) {
                Ok(DocumentRead::Absent) => Some(DocumentAnswer::Sent { frames: 0 }),
                Ok(DocumentRead::Pending) => None,
                Err(error) => {
                    tracing::warn!(workspace = %self.name, %document, %error, "a record of the tree does not read, so the folder waits for it");
                    None
                }
                Ok(_) => Some(DocumentAnswer::Sent { frames: 1 }),
            };
        }
        let held = match self.writes.served(&self.name, &[(FILES_APP, &document)]) {
            None => true,
            Some(served) => matches!(futures::FutureExt::now_or_never(served), Some(Ok(()))),
        };
        held.then_some(DocumentAnswer::Sent { frames: 1 })
    }

    /// How many blobs of the workspace's content the holder that keeps the most of it still
    /// lacks, by the word of the holders this member copy reads: what a folder says is only
    /// here until it reaches a holder. `None` while no holder said.
    async fn read_host_lacks(&self) -> Option<u32> {
        let copies = self.workspaces.member_copies(self.workspace).await?;
        copies
            .holders
            .iter()
            .map(|holder| {
                copies
                    .shortfall
                    .iter()
                    .map(|short| match short {
                        tilo_peer_state::Shortfall::Holder {
                            device,
                            missing: tilo_peer_state::Missing::Content { blobs },
                        } if *device == holder.device => *blobs,
                        _ => 0,
                    })
                    .sum::<u32>()
            })
            .min()
    }

    /// Answer each record the sync opened that has no answer yet and can have one now.
    fn answer_opened(&self) -> Vec<Wake> {
        let asked: Vec<TreeChunk> = self
            .answers
            .lock_recover()
            .iter()
            .filter(|(_, answer)| answer.is_none())
            .map(|(chunk, _)| *chunk)
            .collect();
        let mut told = Vec::new();
        for chunk in asked {
            if let Some(answer) = self.answer_now(chunk) {
                self.answers.lock_recover().insert(chunk, Some(answer));
                told.push(Wake::Opened { chunk, answer });
            }
        }
        told
    }
}

/// The copy as the reader of the tree's records.
struct Records(Arc<Copy>);

impl tilo_fs::tree_sync::Records for Records {
    fn read(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<Reading, String> {
        self.0
            .documents
            .read(workspace, app, document, since)
            .map(|read| match read {
                DocumentRead::Whole { heads, bytes, .. } => Reading::Whole { heads, bytes },
                DocumentRead::Changes { heads, bytes, .. } => Reading::Changes { heads, bytes },
                DocumentRead::Unchanged(_) => Reading::Unchanged,
                DocumentRead::Absent => Reading::Absent,
                DocumentRead::Pending => Reading::Pending,
            })
    }
}

impl tilo_fs::tree_sync::TreePeer for Copy {
    /// The copy holds every record the serving device sends it; opening one asks only for the
    /// copy's answer of it, which [`follow`] gives once it can.
    fn open(&self, chunk: TreeChunk) {
        self.answers.lock_recover().insert(chunk, None);
        self.asked.notify_one();
    }

    fn answer(&self, chunk: TreeChunk) -> Option<DocumentAnswer> {
        self.answers.lock_recover().get(&chunk).copied().flatten()
    }

    fn holds(&self, chunk: TreeChunk) -> bool {
        self.documents.holds(
            &self.name,
            FILES_APP,
            &tilo_fs::tree_sync::document_for(chunk),
        )
    }

    /// A write of this device's is in the copy, and read back, the moment it is made.
    fn holding(&self, _chunk: TreeChunk) -> bool {
        false
    }

    fn pending(&self) -> usize {
        self.unserved.load(Ordering::Relaxed)
    }

    fn reask_content(&self) {
        if let Some(content) = &self.content {
            content.retry(tilo_clock::unix_now_ms());
        }
    }

    fn host_lacks(&self) -> Option<u32> {
        *self.lacks.lock_recover()
    }

    /// The folder waits for the workspace until this device takes it in as a member copy:
    /// the same link, kept up or not, whose answers [`Self::answer_now`] reads. A workspace this
    /// device governs is here from the start.
    fn holds_workspace(&self) -> Option<bool> {
        Some(self.governed || self.workspaces.member_caught_up(self.workspace).is_some())
    }
}

/// The copy as the writer of the tree: a record goes out as this device's own write, sealed
/// with the keys of the tree's group where the workspace seals it.
struct Writer {
    copy: Arc<Copy>,
    wakes: tokio::sync::mpsc::UnboundedSender<Wake>,
}

impl tilo_fs::tree_sync::TreeWriter for Writer {
    fn write<'a>(
        &'a mut self,
        chunk: TreeChunk,
        changes: &'a [u8],
        founding: bool,
        snapshot: bool,
    ) -> tilo_fs::tree_sync::WriteFuture<'a> {
        Box::pin(async move {
            let written = write_as_device(&self.copy, chunk, changes, (founding, snapshot)).await;
            match &written {
                Ok(()) => {
                    count_until_served(&self.copy, &tilo_fs::tree_sync::document_for(chunk));
                }
                // A write the copy does not take waits as one the workspace refused would, rather
                // than going again at every look.
                Err(why) => {
                    let _ = self.wakes.send(Wake::Refused {
                        chunk,
                        reason: why.clone(),
                    });
                }
            }
            written
        })
    }

    fn served(&self, chunks: &[TreeChunk]) -> Option<tilo_fs::tree_sync::Served> {
        served(&self.copy, chunks)
    }
}

/// Resolves once the serving device holds every earlier write of this device's to `chunks` in
/// `copy`, or with why not. A workspace this device governs kept them in its own store as they
/// were made: that answers at once, so the records that name them go out.
fn served(copy: &Copy, chunks: &[TreeChunk]) -> Option<tilo_fs::tree_sync::Served> {
    if copy.governed {
        return Some(Box::pin(std::future::ready(Ok(()))));
    }
    let documents: Vec<String> = chunks
        .iter()
        .map(|chunk| tilo_fs::tree_sync::document_for(*chunk))
        .collect();
    let named: Vec<(&str, &str)> = documents
        .iter()
        .map(|document| (FILES_APP, document.as_str()))
        .collect();
    let served = copy.writes.served(&copy.name, &named)?;
    Some(Box::pin(async move {
        match tokio::time::timeout(SERVED_WITHIN, served).await {
            Ok(served) => served.map_err(Refusal::Other),
            Err(_) => Err(Refusal::Unanswered),
        }
    }))
}

/// Write `changes` to record `chunk` of the tree in `copy` as this device: its founding write
/// and the whole record as `founding` and `snapshot` say, sealed with the keys of the tree's
/// group where the workspace seals it.
async fn write_as_device(
    copy: &Copy,
    chunk: TreeChunk,
    changes: &[u8],
    (founding, snapshot): (bool, bool),
) -> Result<(), Refusal> {
    let route = route(copy.workspace, chunk);
    let keys = match crate::timing::said_if_long(
        "the keys of the workspace's files",
        copy.workspaces.own_route_keys(&route, founding),
    )
    .await
    {
        Some(Err(why)) => {
            return Err(Refusal::Other(format!(
                "the keys of the workspace's files: {why}"
            )));
        }
        keys => keys.and_then(Result::ok),
    };
    copy.writes
        .write(
            &copy.name,
            changes,
            tilo_peer_state::workspace_documents::DocumentWriting {
                app: FILES_APP,
                document: &route.document,
                readers: None,
                founding,
                snapshot,
                sealed: keys.as_ref(),
            },
        )
        .await
        .map(drop)
        .map_err(|refusal| match refusal {
            WriteRefusal::HistoryFull { .. } => Refusal::HistoryFull,
            WriteRefusal::ChangeRefused(_) => Refusal::NotPermitted,
            refusal => Refusal::Other(refusal.to_string()),
        })
}

/// Count a write of this device's to `document` as unserved until the serving device answers
/// for it, or the wait for its word ends: what the tree's status says is still going out.
fn count_until_served(copy: &Copy, document: &str) {
    let Some(served) = copy.writes.served(&copy.name, &[(FILES_APP, document)]) else {
        return;
    };
    let unserved = Arc::clone(&copy.unserved);
    unserved.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(async move {
        let _ = tokio::time::timeout(SERVED_WITHIN, served).await;
        unserved.fetch_sub(1, Ordering::Relaxed);
    });
}

/// Carry the copy's news to the sync as wakes: a write to a record of the tree, the copy catching
/// up with the serving device again, a write of this device's answered, and the answers the opened
/// records can have now. Ends once the sync is gone.
async fn follow(
    copy: Arc<Copy>,
    mut writes: tokio::sync::broadcast::Receiver<crate::workspaces::ResourceWrite>,
    wakes: tokio::sync::mpsc::UnboundedSender<Wake>,
) {
    let mut answered = copy.workspaces.member_writes_answered();
    let mut caught_up: Option<tokio::sync::watch::Receiver<bool>> = None;
    let mut was_caught_up = copy.governed;
    if copy.governed && wakes.send(Wake::HostReady).is_err() {
        return;
    }
    // The holders' word arrives in the workspace's own state, which says nothing when it moves:
    // it is read now and then, and the sync told only when it changed.
    let mut look = tokio::time::interval(HOLDERS_EVERY);
    loop {
        let mut told = Vec::new();
        tokio::select! {
            _ = look.tick() => {
                let lacks = copy.read_host_lacks().await;
                if std::mem::replace(&mut *copy.lacks.lock_recover(), lacks) == lacks {
                    continue;
                }
                if wakes.send(Wake::Holders).is_err() {
                    return;
                }
                continue;
            }
            written = writes.recv() => match written {
                Ok(written) => {
                    if written.workspace == copy.name
                        && written.app == FILES_APP
                        && let Some(chunk) = fs_protocol::tree::chunk_of_record(&written.record)
                    {
                        told.push(Wake::Wrote(chunk));
                    }
                }
                // Writes went by unheard: any record may have changed.
                Err(RecvError::Lagged(_)) => told.push(Wake::Moved),
                Err(RecvError::Closed) => return,
            },
            () = changed(&mut caught_up) => {}
            () = copy.asked.notified() => {}
            _ = answered.changed() => {}
        }
        if caught_up.is_none() {
            // Held as a member copy only once the device took the workspace in.
            caught_up = copy.workspaces.member_caught_up(copy.workspace);
        }
        let now_caught_up = copy.governed
            || caught_up
                .as_ref()
                .is_some_and(|caught_up| *caught_up.borrow());
        if now_caught_up && !was_caught_up {
            // Caught up again after its link was made anew: what was answered before it may be
            // older than what the copy holds now, so every record is opened and answered again.
            told.push(Wake::HostReady);
        }
        was_caught_up = now_caught_up;
        let answering = Arc::clone(&copy);
        // An answer may read a record, which waits for the service: off the runtime's workers.
        if let Ok(answers) = tokio::task::spawn_blocking(move || answering.answer_opened()).await {
            told.extend(answers);
        }
        for wake in told {
            if wakes.send(wake).is_err() {
                return;
            }
        }
        if wakes.is_closed() {
            return;
        }
    }
}

/// Once `caught_up` changes; never while there is none to follow, or once it ended.
async fn changed(caught_up: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match caught_up {
        Some(receiver) => {
            if receiver.changed().await.is_err() {
                *caught_up = None;
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending().await,
    }
}

/// A device's content store as the service's shelf: the bytes of the files of the
/// workspaces it keeps folders of, and nothing of any other app's records.
pub struct StoreShelf(pub Arc<tilo_fs::ContentStore>);

impl StoreShelf {
    /// Run `work` on the store from a thread of its own: the service asks from its own tasks,
    /// and the store is never read by blocking a runtime worker on another runtime.
    fn on_store<T: Send>(
        &self,
        work: impl FnOnce(Arc<tilo_fs::ContentStore>) -> futures::future::BoxFuture<'static, T> + Send,
    ) -> Option<T> {
        let store = Arc::clone(&self.0);
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .ok()?;
                    Some(runtime.block_on(work(store)))
                })
                .join()
                .ok()
                .flatten()
        })
    }
}

impl ContentShelf for StoreShelf {
    fn read(&self, scope: &ContentScope, hash: &str) -> Option<Vec<u8>> {
        if scope.app != FILES_APP {
            return None;
        }
        let hash: Hash = hash.parse().ok()?;
        // Asked once for each blob a holder lacks: a read of a few kilobytes, done here rather
        // than on a thread and runtime of its own.
        self.0.local_now(hash).map(|bytes| bytes.to_vec())
    }

    fn keep_file(
        &self,
        scope: &ContentScope,
        hash: &str,
        path: &std::path::Path,
        size: u64,
    ) -> Result<(), String> {
        if scope.app != FILES_APP {
            return Err("this device keeps only the bytes of the workspaces' files".to_owned());
        }
        let hash: Hash = hash
            .parse()
            .map_err(|_| "not a content address".to_owned())?;
        let owned = path.to_owned();
        let kept = self
            .on_store(move |store| {
                Box::pin(async move {
                    store
                        .ingest_file(&owned)
                        .await
                        .map_err(|error| error.to_string())
                })
            })
            .ok_or_else(|| "the content store could not be reached".to_owned())??;
        if kept == hash && std::fs::metadata(path).is_ok_and(|file| file.len() == size) {
            Ok(())
        } else {
            Err("the store kept other content than the file's address names".to_owned())
        }
    }

    fn keep(&self, scope: &ContentScope, hash: &str, bytes: &[u8]) -> Result<(), String> {
        if scope.app != FILES_APP {
            return Err("this device keeps only the bytes of the workspaces' files".to_owned());
        }
        let hash: Hash = hash
            .parse()
            .map_err(|_| "not a content address".to_owned())?;
        let bytes = Bytes::copy_from_slice(bytes);
        self.on_store(move |store| {
            Box::pin(async move {
                store
                    .store_fetched(hash, bytes)
                    .await
                    .map_err(|error| error.to_string())
            })
        })
        .unwrap_or_else(|| Err("the content store could not be reached".to_owned()))
    }
}

/// One workspace's files as the source of the bytes this device lacks: asked of the device
/// serving the workspace, through the service's record content.
pub struct WorkspaceSource {
    content: Arc<RouteContent>,
    route: CrdtRoute,
    /// How long a read waits for content the workspace is asked for.
    wait: Duration,
}

impl WorkspaceSource {
    #[must_use]
    pub fn new(content: Arc<RouteContent>, workspace: &str, wait: Duration) -> Self {
        Self {
            content,
            route: CrdtRoute::app(FILES_APP, workspace),
            wait,
        }
    }
}

impl ContentSource for WorkspaceSource {
    fn fetch(&self, hash: Hash) -> futures::future::BoxFuture<'static, Option<Bytes>> {
        let (content, route, wait) = (Arc::clone(&self.content), self.route.clone(), self.wait);
        Box::pin(async move {
            let hash = hash.to_hex();
            // Subscribed before the want goes out: an answer that comes at once is heard.
            let mut news = content.subscribe(&route);
            {
                let (content, route, hash) = (Arc::clone(&content), route.clone(), hash.clone());
                tokio::task::spawn_blocking(move || content.want(&route, &hash))
                    .await
                    .ok()?;
            }
            tokio::time::timeout(wait, async {
                while let Some(news) = news.recv().await {
                    if news.hash != hash {
                        continue;
                    }
                    match news.state {
                        ContentState::Resolved => return Some(Bytes::from(news.bytes)),
                        ContentState::Unavailable => return None,
                        _ => {}
                    }
                }
                None
            })
            .await
            .ok()
            .flatten()
        })
    }
}
