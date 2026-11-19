//! A workspace's file tree on this computer: the Files app's records, held
//! through the device's session as every device of the workspace holds them.
//!
//! A workspace's file truth is its tree ([`fs_protocol::tree`]): the tree's
//! own record and its chunks, each a record of its own, whose nodes name each
//! file's content by hash; the bytes travel beside them as the records'
//! content ([`super::content`]). Tilo OS publishes the tree from its
//! filesystem daemon on the desktop session ([`tilo_fs::tree_document`]); here
//! the session is this device's own session with the workspace's host, the
//! one a browser tab keeps, and each record goes over it the way a tab's
//! records do: opened by its route, written with [`Outbox::write`], and read
//! back from the session's replica each time a write [`Event::Landed`] on it.
//!
//! The namespace this device mounts ([`tilo_fs::Namespace`]) and the records
//! converge both ways: a change a record brings merges into the namespace
//! (and what resolving it wrote goes back), and a change the namespace makes
//! (a file copied into the mount) goes out as the next change of the record
//! that holds the file. A write costs what its chunk costs, whatever the tree
//! holds: one change of one record, kept on disk as that record alone.
//!
//! A document reaches the workspace before anything names it: a chunk this device starts goes out,
//! and the workspace's server says it holds it, before any other document of the tree goes out
//! again (the tree's own document lists the chunk, and another chunk's node may sit in a folder
//! of it). A tree whose own document names a chunk the workspace never got reads as missing what
//! that chunk holds, on every device; this order never makes one, whenever this process ends.
//!
//! Every change the mount makes is on disk before the mount answers for it
//! ([`tilo_fs::kept_documents`]): a process killed in the middle of a burst starts again with
//! every file it answered for, and sends what the workspace does not hold yet.
//!
//! Writes that come in a burst (a checkout writing thousands of files) go out
//! together: the changes the namespace made within [`BATCH`] of the first one
//! leave as one write per record they changed. Each file's version still
//! names who wrote it; only how many writes carry them changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use fs_protocol::TreeChunk;
use tilo_fs::kept_documents::KeptDocuments;
use tilo_fs::namespace::Namespace;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::{DocumentAnswer, WorkspaceId};
use tilo_session_client::engine::ResourceReading;
use tilo_session_client::{Cmd, DocumentRoute, Event, Link, Outbox as _, Receipt, TryRecvError};

use super::content::SessionContent;

/// The app whose record a workspace's tree is.
pub(crate) use fs_protocol::tree::FILES_APP;

/// How often the replica looks again when nothing woke it: the session's
/// events wake it at once, this only bounds a missed wake.
const LOOK_AGAIN: Duration = Duration::from_secs(1);
/// How often the namespace is kept on disk while it changes, at most.
const KEEP_EVERY: Duration = Duration::from_secs(2);
/// How often the files and bytes the tree names are counted again while it changes, at most.
const COUNT_EVERY: Duration = Duration::from_secs(1);
/// How long the changes after a first unsent one wait to go out with it: a
/// burst of writes becomes one write per record, and a single write waits no
/// longer than a person notices.
const BATCH: Duration = Duration::from_millis(200);

/// How many bytes of changes one write of a record carries at most: well under the most a
/// document write may carry, so changes made while a record could not go out go out over
/// several writes, each building on the last, instead of one no host takes.
const WRITE_BUDGET: usize = collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES / 2;
/// How many times its own cost a step that walks or saves the whole tree waits before it runs
/// again: it then takes at most a twentieth of the replica's time. Run on every change, a count
/// of every node made writing a repository's twenty thousand files quadratic, and held the tree
/// (which every write into the folder waits for) for most of the time.
const PACE: u32 = 20;
/// Where the namespace's records are kept between runs, one file per record.
const KEPT: &str = "tree";

/// A file the tree names: the hash of its content (`None` for a text document the tree holds
/// itself) and its size.
pub(crate) type FileBytes = (Option<iroh_blobs::Hash>, u64);

/// When a step whose cost grows with the tree may run again.
#[derive(Debug)]
struct Paced {
    next: Instant,
    floor: Duration,
}

impl Paced {
    fn new(floor: Duration) -> Self {
        Self {
            next: Instant::now(),
            floor,
        }
    }

    fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// The step ran from `started` to `ended`: it waits its floor, or `PACE` times what it took.
    fn ran(&mut self, started: Instant, ended: Instant) {
        let took = ended.saturating_duration_since(started);
        let wait = self.floor.max(took.saturating_mul(PACE));
        self.next = ended.checked_add(wait).unwrap_or(ended);
    }
}

/// The route of record `chunk` of `workspace`'s tree: the tree's own record for 0.
pub(crate) fn route(workspace: WorkspaceId, chunk: TreeChunk) -> DocumentRoute {
    DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace.to_string(),
        document: fs_protocol::tree::chunk_document_id(chunk),
    }
}

/// Where the replica stands, as `tilo status` says it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a fact `tilo status` reports on its own"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TreeStatus {
    /// The workspace's host answered for the tree: what it holds is merged.
    pub(crate) opened: bool,
    /// Every change made here was handed to the session.
    pub(crate) sent: bool,
    /// The session holds nothing of this device's that its host has not
    /// answered for.
    pub(crate) durable: bool,
    /// Files the tree names.
    pub(crate) files: usize,
    /// Their bytes, all of them.
    pub(crate) bytes: u64,
    /// Not syncing, as the person asked.
    pub(crate) paused: bool,
}

/// One workspace's tree, held while this value lives.
pub(crate) struct TreeReplica {
    pub(crate) namespace: Arc<Mutex<Namespace>>,
    kept: Arc<KeptDocuments>,
    status: Arc<Mutex<TreeStatus>>,
    /// The files the tree names, each with the hash of its content (`None` for a text document
    /// the tree holds itself) and its size, as the last count found them: `tilo status` answers
    /// from here, never from the namespace, which a step over the whole tree holds for as long
    /// as it lasts.
    files: Arc<Mutex<Vec<FileBytes>>>,
    paused: Arc<std::sync::atomic::AtomicBool>,
    /// Set when the holder stops: the task keeps what changed and ends instead of syncing on
    /// into a session that is going.
    stopping: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    /// The task syncing this tree, until a stop waits for it.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for TreeReplica {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// What the documents were read at when a namespace was opened ([`Namespace::from_documents_read`]).
type ReadHeads = BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>;

/// The namespace `kept` holds for `author`, with each document's heads as read: its documents' saves
/// and journals, or the saves alone when a journal does not read (damaged on disk), or a new one
/// when nothing is kept.
fn open_namespace(
    author: &str,
    kept: &KeptDocuments,
    documents: Vec<(TreeChunk, Vec<u8>)>,
) -> std::io::Result<(Namespace, ReadHeads)> {
    let new = || {
        Namespace::new(author)
            .map(|namespace| (namespace, ReadHeads::new()))
            .map_err(std::io::Error::other)
    };
    if documents.is_empty() {
        return new();
    }
    match Namespace::from_documents_read(author, documents) {
        Ok(opened) => Ok(opened),
        Err(error) => {
            tracing::warn!(%error, "the tree's journal does not read; it starts from the documents' last saves");
            let saved = kept.saved_only()?;
            if saved.is_empty() {
                return new();
            }
            Namespace::from_documents_read(author, saved)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }
    }
}

impl TreeReplica {
    /// Hold `workspace`'s tree over `link`, kept in `state` between runs, as
    /// the device whose key is `device`; its file bytes come and go through
    /// `content`.
    ///
    /// # Errors
    /// The kept namespace cannot be read.
    pub(crate) async fn start(
        link: Link,
        workspace: WorkspaceId,
        state: &Path,
        device: [u8; 32],
        content: Arc<SessionContent>,
    ) -> std::io::Result<Self> {
        let kept_at = state.join(KEPT);
        let author = hex::encode(device);
        // Reading the kept chunks and indexing every node they name walks the whole tree. On
        // a runtime worker it would hold that worker — and the holder's clients, and the agents
        // it serves, wait for one — for as long as a repository's tree takes, so it goes to a
        // thread of the blocking pool instead.
        let (kept, namespace) = tokio::task::spawn_blocking(move || {
            let (kept, documents) = KeptDocuments::open(&kept_at)?;
            let (mut namespace, read) = open_namespace(&author, &kept, documents)?;
            kept.opened(&mut namespace, read)?;
            Ok::<_, std::io::Error>((Arc::new(kept), namespace))
        })
        .await
        .map_err(std::io::Error::other)??;
        let namespace = Arc::new(Mutex::new(namespace));
        let status = Arc::new(Mutex::new(TreeStatus::default()));
        let files = Arc::new(Mutex::new(Vec::new()));
        let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake = Arc::new(tokio::sync::Notify::new());
        let sync = Sync {
            paused: Arc::clone(&paused),
            stopping: Arc::clone(&stopping),
            wake: Arc::clone(&wake),
            link,
            workspace,
            namespace: Arc::clone(&namespace),
            documents: BTreeMap::new(),
            unread: BTreeSet::new(),
            unreadable: BTreeSet::new(),
            waiting: BTreeSet::new(),
            held_until: None,
            dirty_since: None,
            kept: Arc::clone(&kept),
            ordered: BTreeSet::new(),
            pacing: Paced::new(KEEP_EVERY),
            pulling: Paced::new(LOOK_AGAIN),
            counting: Paced::new(COUNT_EVERY),
            counted_heads: None,
            status: Arc::clone(&status),
            files: Arc::clone(&files),
            content,
        };
        let task = tokio::spawn(sync.run());
        Ok(Self {
            namespace,
            kept,
            status,
            files,
            paused,
            stopping,
            wake,
            task: Some(task),
        })
    }

    /// Stop syncing and keep the tree on disk, waiting at most `within` for it: a step still
    /// walking the tree cannot be interrupted, and a holder stopping must end inside the
    /// service's timeout whether or not one is.
    pub(crate) async fn stop(mut self, within: Duration) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.wake.notify_one();
        if let Some(task) = self.task.take()
            && tokio::time::timeout(within, task).await.is_err()
        {
            tracing::warn!("a workspace's tree did not keep in time; it is left as last kept");
        }
    }

    /// What the mount hands each change it makes to, before it answers the request that made
    /// it: the change goes into its document's journal on disk.
    pub(crate) fn journal(&self) -> tilo_fs::fs::ChangeJournal {
        let kept = Arc::clone(&self.kept);
        Arc::new(move |namespace: &mut Namespace| kept.journal(namespace))
    }

    /// The files the tree names as last counted: the hash of each one's content and its size.
    pub(crate) fn files(&self) -> Vec<FileBytes> {
        self.files.lock_recover().clone()
    }

    pub(crate) fn status(&self) -> TreeStatus {
        self.status.lock_recover().clone()
    }

    /// Stop syncing (changes made here wait to go out, and none come in), or
    /// start again.
    pub(crate) fn set_paused(&self, paused: bool) {
        self.paused
            .store(paused, std::sync::atomic::Ordering::Relaxed);
        // Said at once rather than at the next pass: the pass may wait for a step over the tree.
        self.status.lock_recover().paused = paused;
        self.wake.notify_one();
    }
}

/// What this device knows of one record of the tree at the workspace.
#[derive(Debug, Default)]
struct Held {
    /// The record's heads this namespace merged last; `None` before the first merge.
    merged: Option<Vec<[u8; 32]>>,
    /// The namespace's heads of this record the session has: handed to it by
    /// this device, or merged from it. Every change after them is the next write.
    sent: Vec<automerge::ChangeHash>,
    /// The record exists at the workspace (merged from it, or founded here), so
    /// a write of it is a change.
    founded: bool,
    /// How many frames the host said it holds of the record when this session opened it;
    /// `None` until it answered. None at all: the workspace has no such record yet.
    answered: Option<DocumentAnswer>,
    /// The host refused a write of this record: none goes out before then, and the next one
    /// carries every change since what the host holds.
    refused: Refusals,
    /// The workspace's server holds the document: it said so when the document was opened, or every
    /// write of this device's to it is durable there. Until then nothing else of the tree goes
    /// out, since anything else may name it.
    durable: bool,
    /// Whether every write of this device's to the document is durable at the server, asked once
    /// this device wrote it while the server did not hold it.
    confirming: Option<Receipt<Result<(), String>>>,
}

/// How long writes of a record the host refused wait before the next goes out. A refusal says
/// the host does not take the changes as they were (the workspace is full, or a grant is
/// missing): waiting coalesces what is made meanwhile into one write, and each refusal in a row
/// doubles the wait, up to [`MOST_REFUSED_WAIT`]; a write that lands ends the run.
#[derive(Debug, Default)]
struct Refusals {
    /// The wait the last refusal set; zero when the last write landed.
    wait: Duration,
    /// When the next write may go out.
    until: Option<Instant>,
}

/// The first wait after a refusal.
const FIRST_REFUSED_WAIT: Duration = Duration::from_secs(1);
/// The longest wait after refusals in a row.
const MOST_REFUSED_WAIT: Duration = Duration::from_secs(30);

impl Refusals {
    /// A write was refused at `now`: the next waits twice as long as the last. Whether it is
    /// the first of a run.
    fn refused(&mut self, now: Instant) -> bool {
        let first = self.wait.is_zero();
        self.wait = if first {
            FIRST_REFUSED_WAIT
        } else {
            self.wait.saturating_mul(2).min(MOST_REFUSED_WAIT)
        };
        self.until = now.checked_add(self.wait);
        first
    }

    /// A write of the record landed: the run of refusals is over.
    const fn landed(&mut self) {
        self.wait = Duration::ZERO;
    }

    /// When a write may go out, while that is after `now`.
    fn held_until(&self, now: Instant) -> Option<Instant> {
        self.until.filter(|until| *until > now)
    }
}

/// The task that keeps one namespace and its records converged.
struct Sync {
    paused: Arc<std::sync::atomic::AtomicBool>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    link: Link,
    workspace: WorkspaceId,
    namespace: Arc<Mutex<Namespace>>,
    /// Every record of the tree this device opened, by chunk: the tree's own (0) first.
    documents: BTreeMap<TreeChunk, Held>,
    /// The records a write landed on since they were read: what the next pull reads, rather
    /// than every record of the tree.
    unread: BTreeSet<TreeChunk>,
    /// The records whose read failed, said once each.
    unreadable: BTreeSet<TreeChunk>,
    /// The records whose changes waited to go out, said once each.
    waiting: BTreeSet<TreeChunk>,
    /// When the first record the host refused may write again, while one waits.
    held_until: Option<Instant>,
    /// When the oldest change not yet handed to the session was made: what
    /// the next write waits [`BATCH`] after.
    dirty_since: Option<Instant>,
    /// Where each document is kept on disk, with its changes since.
    kept: Arc<KeptDocuments>,
    /// The documents whose wait for another to reach the workspace was said, once each.
    ordered: BTreeSet<TreeChunk>,
    /// How often the tree may be kept, since it costs what the tree costs.
    pacing: Paced,
    counting: Paced,
    pulling: Paced,
    /// The heads the files and bytes in `status` were counted at.
    counted_heads: Option<Vec<automerge::ChangeHash>>,
    status: Arc<Mutex<TreeStatus>>,
    /// The files the tree names as last counted, for `tilo status`.
    files: Arc<Mutex<Vec<FileBytes>>>,
    content: Arc<SessionContent>,
}

impl Sync {
    async fn run(mut self) {
        let wake = Arc::clone(&self.wake);
        {
            let wake = Arc::clone(&wake);
            self.link.on_change(Arc::new(move || wake.notify_one()));
        }
        let events = self.link.events_where(|event| {
            matches!(
                event,
                Event::Landed { .. }
                    | Event::DocumentOpened { .. }
                    | Event::Refused { .. }
                    | Event::DocumentContent { .. }
                    | Event::HostReady { .. }
            )
        });
        let mut changes = self.namespace.lock_recover().subscribe_changes();
        let mut keep = tokio::time::interval(KEEP_EVERY);
        let held = self.namespace.lock_recover().records();
        for chunk in held {
            self.open(chunk);
        }
        // What the session brought: on the first pass, and after it woke this task.
        let mut session_moved = true;
        let mut sweep = Paced::new(LOOK_AGAIN);
        loop {
            for event in events.drain() {
                self.take(event);
            }
            if self.stopping.load(std::sync::atomic::Ordering::Relaxed) {
                // The holder is stopping: keep what this tree changed and end, rather than
                // carrying on into a session that is going.
                break;
            }
            if !self.paused.load(std::sync::atomic::Ordering::Relaxed) {
                if sweep.due(Instant::now()) {
                    // Now and then every record, for a landing whose word was missed.
                    self.unread.extend(self.documents.keys().copied());
                    sweep.ran(Instant::now(), Instant::now());
                }
                if session_moved || !self.unread.is_empty() {
                    self.pull_all().await;
                }
                self.confirmations();
                self.push().await;
                self.confirm();
                self.content.reask(&self.link);
            }
            self.say_where_it_stands().await;
            // A change of this namespace's own goes out once its batch is due; the session's
            // news, and a look now and then, read what the records hold.
            // A record the host refused waits its turn without the task looking every moment.
            let wait = self.dirty_since.map_or(LOOK_AGAIN, |since| {
                let held = self.held_until.map_or(Duration::ZERO, |until| {
                    until.saturating_duration_since(Instant::now())
                });
                BATCH
                    .saturating_sub(since.elapsed())
                    .max(held)
                    .max(Duration::from_millis(5))
            });
            session_moved = false;
            tokio::select! {
                () = wake.notified() => session_moved = true,
                moved = changes.changed() => if moved.is_err() { break },
                _ = keep.tick() => if self.pacing.due(Instant::now()) {
                    let started = Instant::now();
                    self.keep().await;
                    self.pacing.ran(started, Instant::now());
                },
                () = tokio::time::sleep(wait) => session_moved = self.dirty_since.is_none(),
            }
        }
        // What changed here is on disk before this task ends: the holder is going, and the
        // module that made it goes with the process.
        self.keep().await;
    }

    /// Ask the host for record `chunk` of the tree, and to take part in its channel.
    fn open(&mut self, chunk: TreeChunk) {
        self.documents.entry(chunk).or_default();
        self.unread.insert(chunk);
        let route = route(self.workspace, chunk);
        let _ = self
            .link
            .send(Cmd::OpenDocumentAt(self.workspace, route.clone()));
        let _ = self.link.send(Cmd::OpenRoute(route.clone()));
        // And on the workspace's live session, whose host answers what it holds.
        let _ = self.link.engine().open_resource(
            &self.workspace.to_string(),
            &route.app_id,
            &route.document,
        );
    }

    fn take(&mut self, event: Event) {
        match event {
            Event::HostReady { ws, .. } if ws == self.workspace => {
                let opened: Vec<TreeChunk> = self.documents.keys().copied().collect();
                for chunk in opened {
                    self.open(chunk);
                }
            }
            // The host did not take a write: its changes are still this namespace's, and go out
            // again, with all made since, once the record's wait is over. What the host holds
            // is what was last merged from it.
            Event::Refused { route, refusal, .. }
                if route.cluster_id == self.workspace.to_string() && route.app_id == FILES_APP =>
            {
                if let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.document) {
                    let held = self.documents.entry(chunk).or_default();
                    held.sent = held
                        .merged
                        .iter()
                        .flatten()
                        .map(|hash| automerge::ChangeHash(*hash))
                        .collect();
                    if held.refused.refused(Instant::now()) {
                        tracing::warn!(%refusal, workspace = %self.workspace, record = %route.document, "the workspace refused a change to its files; it goes again after a wait");
                    }
                }
            }
            Event::Landed { route, .. }
                if route.cluster_id == self.workspace.to_string() && route.app_id == FILES_APP =>
            {
                if let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.document) {
                    self.unread.insert(chunk);
                    self.documents.entry(chunk).or_default().refused.landed();
                }
            }
            Event::DocumentContent { ws, frame, .. } if ws == self.workspace => {
                self.content.piece(&frame);
            }
            // The host sent all it holds of the tree, none when the workspace has none yet:
            // this device's first change founds it.
            // The host's answer to this session's open of a record of the tree: whether the
            // workspace holds any of it decides whether this device founds it.
            Event::DocumentOpened {
                route: Some(route),
                answer,
                ..
            } if route.cluster_id == self.workspace.to_string() && route.app_id == FILES_APP => {
                if let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.document) {
                    let held = self.documents.entry(chunk).or_default();
                    held.answered = Some(answer);
                    held.durable |= holds_some(Some(answer));
                    // A host that sent some of the tree holds one, which this device reads
                    // before it writes: a tree it founded beside that one would go out whole.
                    if chunk == 0 && holds_none(Some(answer)) {
                        self.status.lock_recover().opened = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Whether the session holds writes of this device's to record `chunk` that its replica
    /// has not taken in: a read then lays them over the whole record, and all it can bring is
    /// another device's change, which waits a moment.
    fn holding(&self, chunk: TreeChunk) -> bool {
        self.link
            .engine()
            .held_writes()
            .lock()
            .contains_key(&route(self.workspace, chunk))
    }

    /// Merge what each opened record holds that this namespace has not merged yet, and open
    /// the chunks the tree's own record lists that this device has not opened.
    async fn pull_all(&mut self) {
        let link = self.link.clone();
        let engine = link.engine();
        let workspace = self.workspace.to_string();
        let now = Instant::now();
        let paced = self.pulling.due(now);
        let mut readings = Vec::new();
        let chunks = std::mem::take(&mut self.unread);
        for chunk in chunks {
            if self.holding(chunk) && !paced {
                // Read it with the next paced pass.
                self.unread.insert(chunk);
                continue;
            }
            let route = route(self.workspace, chunk);
            let merged = self
                .documents
                .get(&chunk)
                .and_then(|held| held.merged.clone());
            let reading = engine.record_update(
                &workspace,
                &route.app_id,
                &route.document,
                merged.as_deref(),
            );
            match reading {
                Ok(
                    ResourceReading::Captured { heads, bytes, .. }
                    | ResourceReading::Changes { heads, bytes, .. },
                ) => readings.push((chunk, heads, bytes)),
                Ok(ResourceReading::Unchanged(_)) => {}
                Ok(ResourceReading::Pending) | Err(_) => {
                    if let Err(error) = &reading
                        && self.unreadable.insert(chunk)
                    {
                        // Said once per record: the read is tried again on every pass.
                        tracing::warn!(%error, chunk, workspace = %self.workspace, "a record of the workspace's tree cannot be read here");
                    }
                    // Nothing here yet: once the host answered with none, the workspace has
                    // no such record, and this device's first change founds it.
                    if chunk == 0
                        && holds_none(engine.resource_answer(
                            &workspace,
                            &route.app_id,
                            &route.document,
                        ))
                    {
                        self.status.lock_recover().opened = true;
                    }
                }
            }
        }
        if paced {
            self.pulling.ran(now, Instant::now());
        }
        if readings.is_empty() {
            return;
        }
        let records: Vec<(TreeChunk, Vec<u8>)> = readings
            .iter()
            .map(|(chunk, _, bytes)| (*chunk, bytes.clone()))
            .collect();
        // The merge runs away from the runtime's workers: a tree's first merge walks every
        // node it brings, and a worker inside it stops answering agents, mounts and stops for
        // as long as it takes.
        let namespace = Arc::clone(&self.namespace);
        let merged = tokio::task::spawn_blocking(move || {
            let mut namespace = namespace.lock_recover();
            namespace
                .take_documents(records)
                .map(|()| namespace.listed_records())
                .map_err(|error| error.to_string())
        })
        .await;
        let listed = match merged {
            Ok(Ok(listed)) => listed,
            Ok(Err(error)) => {
                tracing::warn!(%error, "the workspace's tree does not merge here");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "the workspace's tree merge could not run");
                return;
            }
        };
        for (chunk, heads, _) in readings {
            if chunk == 0 {
                self.status.lock_recover().opened = true;
            }
            let held = self.documents.entry(chunk).or_default();
            // What the record holds is not this device's to send again; what only this
            // namespace holds is. A namespace makes changes of its own before it ever merged the
            // record (the tree it starts as, a file copied in while the host had not answered):
            // counted as sent, those changes never reach the host, which then leaves out every
            // later write of this device as building on changes it never got.
            for merged in &heads {
                let hash = automerge::ChangeHash(*merged);
                if !held.sent.contains(&hash) {
                    held.sent.push(hash);
                }
            }
            held.merged = Some(heads);
            held.founded = true;
        }
        for chunk in listed {
            if !self.documents.contains_key(&chunk) {
                self.open(chunk);
            }
        }
    }

    /// The namespace, only when it can be had now. A worker that waited on a step over the
    /// whole tree (a merge, a count, a keep) would answer no client meanwhile, and a step says
    /// when it is done through the change watch this task follows, so a pass that cannot read
    /// the tree now leaves it to the next one.
    fn namespace_now(&self) -> Option<std::sync::MutexGuard<'_, Namespace>> {
        match self.namespace.try_lock() {
            Ok(namespace) => Some(namespace),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// Hand the session what the namespace changed since it last did, once the batch the
    /// first of those changes started is due: one write per record that changed.
    async fn push(&mut self) {
        if !self.status.lock_recover().opened {
            // The host has not said what it holds: a tree written now could found a second
            // one beside it.
            return;
        }
        let heads = {
            let Some(mut namespace) = self.namespace_now() else {
                return;
            };
            namespace.document_heads()
        };
        let dirty: Vec<TreeChunk> = heads
            .iter()
            .filter(|(chunk, heads)| {
                self.documents.get(chunk).map(|held| &held.sent) != Some(*heads)
            })
            .map(|(chunk, _)| *chunk)
            .collect();
        if dirty.is_empty() {
            self.dirty_since = None;
            return;
        }
        let since = *self.dirty_since.get_or_insert_with(Instant::now);
        if since.elapsed() < BATCH {
            return;
        }
        let (due, mut waiting) = self.due(dirty, heads.keys().copied().collect());
        if due.is_empty() {
            self.dirty_since = waiting.then_some(since);
            return;
        }
        // Reading the changes a chunk holds beyond what the session has walks that chunk's
        // document, and a folder written into in bulk has many chunks to walk: it goes to the
        // blocking pool, so the workers answering the holder's clients and the agents stay free.
        let namespace = Arc::clone(&self.namespace);
        let writes = tokio::task::spawn_blocking(move || {
            let mut namespace = namespace.lock_recover();
            due.into_iter()
                .filter_map(|(chunk, sent)| namespace.document_write(chunk, &sent, WRITE_BUDGET))
                .collect::<Vec<_>>()
        })
        .await;
        let writes = match writes {
            Ok(writes) => writes,
            Err(error) => {
                tracing::warn!(%error, "the workspace's tree cannot be read out");
                return;
            }
        };
        for write in writes {
            let route = route(self.workspace, write.chunk);
            if !write.changes.is_empty()
                && self
                    .link
                    .write(route, &DocumentMutation::Incremental(write.changes))
                    .is_err()
            {
                waiting = true;
                continue;
            }
            // What the write carries, not what the namespace holds now: the mount keeps
            // changing it while the write goes out, and a change made meanwhile counted as
            // sent would never reach the host, which then leaves out every later write of
            // this device.
            if let Some(held) = self.documents.get_mut(&write.chunk) {
                held.sent = write.heads;
            }
        }
        self.dirty_since = waiting.then_some(since);
    }

    /// The chunks of `dirty` that can go out now, with the heads each was last handed to the
    /// session at, and whether any waits instead: whether it is founded here or waits for the
    /// workspace's answer.
    fn due(
        &mut self,
        dirty: Vec<TreeChunk>,
        held_now: Vec<TreeChunk>,
    ) -> (Vec<(TreeChunk, Vec<automerge::ChangeHash>)>, bool) {
        let link = self.link.clone();
        let engine = link.engine();
        let workspace = self.workspace.to_string();
        let mut waiting = false;
        let now = Instant::now();
        self.held_until = None;
        let mut due = Vec::new();
        // The chunks the workspace's server does not hold yet go out first, and alone: every
        // other document of the tree waits until the server holds them, since it may name them.
        let unheld = unheld_chunks(&self.documents, held_now);
        for chunk in dirty {
            if !self.documents.contains_key(&chunk) {
                self.open(chunk);
            }
            if !may_go_out(chunk, &unheld) {
                if self.ordered.insert(chunk) {
                    tracing::info!(chunk, waits_for = ?unheld, workspace = %self.workspace, "a document of the workspace's tree waits for the chunks it may name to reach the workspace");
                }
                waiting = true;
                continue;
            }
            self.ordered.remove(&chunk);
            if let Some(until) = self
                .documents
                .get(&chunk)
                .and_then(|held| held.refused.held_until(now))
            {
                self.held_until = Some(self.held_until.map_or(until, |held| held.min(until)));
                waiting = true;
                continue;
            }
            let route = route(self.workspace, chunk);
            let founded = self.documents.get(&chunk).is_some_and(|held| held.founded);
            if !founded {
                // A record this device starts: the host says first whether the workspace has
                // it. One it has is merged before this device's changes of it go out; one it
                // has not is founded here, with the empty start every device makes alike.
                // Its changes are saved only once they can go out: saving walks the record.
                // The session's own answer to its open, as a reader of the record hears it: a
                // record the host holds frames of is merged before this device's changes of it
                // go out, one it holds none of is founded here.
                let answered = self.documents.get(&chunk).and_then(|held| held.answered);
                if !holds_none(answered)
                    || engine.holds_resource(&workspace, &route.app_id, &route.document)
                {
                    // Said once per record: a write that waits here for long is one the
                    // folder says it is still sending.
                    if self.waiting.insert(chunk) {
                        tracing::info!(chunk, ?answered, workspace = %self.workspace, "a record of the workspace's tree waits before this device's changes of it go out");
                    }
                    waiting = true;
                    continue;
                }
                let genesis = automerge::AutoCommit::new().save();
                if self
                    .link
                    .write(route.clone(), &DocumentMutation::Snapshot(genesis))
                    .is_err()
                {
                    waiting = true;
                    continue;
                }
                if let Some(held) = self.documents.get_mut(&chunk) {
                    held.founded = true;
                }
            }
            let sent = self
                .documents
                .get(&chunk)
                .map(|held| held.sent.clone())
                .unwrap_or_default();
            due.push((chunk, sent));
        }
        (due, waiting)
    }

    /// Ask whether each document this device wrote that the server did not hold is durable there
    /// now: once it is, the documents that may name it go out.
    fn confirm(&mut self) {
        let asks: Vec<TreeChunk> = self
            .documents
            .iter()
            .filter(|(chunk, held)| {
                **chunk != 0
                    && !held.durable
                    && held.confirming.is_none()
                    && held.founded
                    && !held.sent.is_empty()
            })
            .map(|(chunk, _)| *chunk)
            .collect();
        for chunk in asks {
            if let Ok(receipt) = self.link.barrier_for(&[route(self.workspace, chunk)])
                && let Some(held) = self.documents.get_mut(&chunk)
            {
                held.confirming = Some(receipt);
            }
        }
    }

    /// Take the server's answers to [`Self::confirm`]'s asks: a document all of whose writes are
    /// durable there is held by the workspace; one the server refused is asked again once its
    /// next write went out.
    fn confirmations(&mut self) {
        for (chunk, held) in &mut self.documents {
            let Some(receipt) = &held.confirming else {
                continue;
            };
            match receipt.try_recv() {
                Ok(Ok(())) => {
                    held.durable = true;
                    held.confirming = None;
                }
                Ok(Err(refusal)) => {
                    tracing::warn!(%refusal, chunk, workspace = %self.workspace, "a chunk of the workspace's tree did not reach it; it goes again");
                    held.confirming = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => held.confirming = None,
            }
        }
    }

    async fn say_where_it_stands(&mut self) {
        let (heads, records) = {
            let Some(mut namespace) = self.namespace_now() else {
                return;
            };
            (namespace.heads(), namespace.document_heads())
        };
        let counting =
            self.counted_heads.as_ref() != Some(&heads) && self.counting.due(Instant::now());
        let counted = if counting {
            // Counting the files walks every node of the tree: off the runtime's workers, like
            // a merge and a keep, so a client's status is answered meanwhile.
            let namespace = Arc::clone(&self.namespace);
            let files = Arc::clone(&self.files);
            let started = Instant::now();
            match tokio::task::spawn_blocking(move || {
                let namespace = namespace.lock_recover();
                let named = files_of(&namespace);
                drop(namespace);
                let counts = (
                    named.len(),
                    named
                        .iter()
                        .fold(0_u64, |bytes, (_, size)| bytes.saturating_add(*size)),
                );
                *files.lock_recover() = named;
                counts
            })
            .await
            {
                Ok(counted) => {
                    self.counting.ran(started, Instant::now());
                    self.counted_heads = Some(heads);
                    Some(counted)
                }
                Err(error) => {
                    tracing::warn!(%error, "counting the workspace's files could not be done");
                    None
                }
            }
        } else {
            None
        };
        let sent = records.iter().all(|(chunk, heads)| {
            self.documents
                .get(chunk)
                .is_some_and(|held| held.sent == *heads)
        });
        let durable = !self.documents.keys().any(|chunk| self.holding(*chunk))
            && self.link.outgoing_pending() == 0;
        let mut status = self.status.lock_recover();
        if let Some((files, bytes)) = counted {
            status.files = files;
            status.bytes = bytes;
        }
        status.paused = self.paused.load(std::sync::atomic::Ordering::Relaxed);
        status.sent = sent;
        status.durable = durable;
    }

    /// Save the documents that changed on disk, each in a file of its own, so a restart reads
    /// each document's save rather than a long journal of its changes: what a write costs on
    /// disk is its document, not the tree. Saving a document walks it, so it is done off the
    /// runtime's workers, as a merge is.
    fn keep(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let namespace = Arc::clone(&self.namespace);
        let kept = Arc::clone(&self.kept);
        async move {
            match tokio::task::spawn_blocking(move || kept.keep(&namespace)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "the workspace's tree is not saved on disk; its journal keeps its changes");
                }
                Err(error) => {
                    tracing::warn!(%error, "keeping the workspace's tree could not be done");
                }
            }
        }
    }
}

/// The files `namespace` names, each with the hash of its content (`None` for a text document
/// whose content the tree holds itself) and its size: a walk of every node.
fn files_of(namespace: &Namespace) -> Vec<FileBytes> {
    let mut files = Vec::new();
    for id in namespace.live_node_ids() {
        // A text document (a `.md`, an `.odt`) is a file whose content the tree holds itself,
        // by no hash: a file all the same.
        if let Some(entry) = namespace.entry_for_node(&id)
            && entry.kind == tilo_fs::namespace::NodeKind::File
        {
            files.push((entry.hash, entry.size));
        }
    }
    files
}

impl Drop for Sync {
    /// The backstop for what the task cannot do itself: a tree whose task ended without
    /// keeping (or was let go of) still writes what changed on disk.
    fn drop(&mut self) {
        if let Err(error) = self.kept.keep(&self.namespace) {
            tracing::warn!(%error, "the workspace's tree is not saved on disk; its journal keeps its changes");
        }
    }
}

/// Whether the host's `answer` about a record of the tree (the frames it sent of it, `None`
/// while it owes one) says the workspace has none of it.
fn holds_none(answer: Option<DocumentAnswer>) -> bool {
    answer == Some(DocumentAnswer::Sent { frames: 0 })
}

/// The chunks of `chunks` the workspace's server does not hold yet, as `documents` know them: the
/// tree's own document is never one, since every device opens it first.
fn unheld_chunks(
    documents: &BTreeMap<TreeChunk, Held>,
    chunks: impl IntoIterator<Item = TreeChunk>,
) -> BTreeSet<TreeChunk> {
    chunks
        .into_iter()
        .filter(|chunk| *chunk != 0 && !documents.get(chunk).is_some_and(|held| held.durable))
        .collect()
}

/// Whether a write of document `chunk` may go out while the workspace's server does not hold the
/// chunks `unheld`: those chunks go out first and alone, since any other document may name them.
fn may_go_out(chunk: TreeChunk, unheld: &BTreeSet<TreeChunk>) -> bool {
    unheld.is_empty() || unheld.contains(&chunk)
}

/// Whether the server's `answer` about a document of the tree says the workspace holds some of it.
fn holds_some(answer: Option<DocumentAnswer>) -> bool {
    matches!(answer, Some(DocumentAnswer::Sent { frames }) if frames > 0)
}

#[cfg(test)]
mod tests {
    use tilo_server_protocol::DocumentAnswer;

    #[test]
    fn a_refused_record_waits_longer_each_time_until_a_write_lands() {
        use super::{FIRST_REFUSED_WAIT, MOST_REFUSED_WAIT, Refusals};
        let start = std::time::Instant::now();
        let mut refusals = Refusals::default();
        assert_eq!(
            refusals.held_until(start),
            None,
            "nothing refused: writes go out"
        );
        assert!(
            refusals.refused(start),
            "the first refusal of a run is said"
        );
        assert_eq!(refusals.held_until(start), Some(start + FIRST_REFUSED_WAIT));
        assert!(
            !refusals.refused(start),
            "the rest of the run is not said again"
        );
        assert_eq!(
            refusals.held_until(start),
            Some(start + FIRST_REFUSED_WAIT * 2)
        );
        for _ in 0..10 {
            refusals.refused(start);
        }
        assert_eq!(refusals.held_until(start), Some(start + MOST_REFUSED_WAIT));
        assert_eq!(
            refusals.held_until(start + MOST_REFUSED_WAIT),
            None,
            "the wait ends"
        );
        refusals.landed();
        assert!(refusals.refused(start), "a write that landed ends the run");
        assert_eq!(refusals.held_until(start), Some(start + FIRST_REFUSED_WAIT));
    }

    #[test]
    fn a_tree_the_host_sent_but_that_does_not_read_here_is_not_founded_again() {
        // The host owes its answer still: nothing is known.
        assert!(!super::holds_none(None));
        // It sent frames of the tree: the workspace holds one, though it may not read here
        // yet (a write still on its way), and a tree founded now would go out whole beside it.
        assert!(!super::holds_none(Some(DocumentAnswer::Sent { frames: 3 })));
        // It sent none: the workspace has no tree, and this device's first change founds it.
        assert!(super::holds_none(Some(DocumentAnswer::Sent { frames: 0 })));
        // It is another person's own: nothing of it is this device's to found.
        assert!(!super::holds_none(Some(DocumentAnswer::NotYours)));
    }

    use super::*;

    #[test]
    fn a_step_that_walks_the_tree_waits_twenty_times_its_cost_and_at_least_its_floor() {
        let mut paced = super::Paced::new(Duration::from_secs(1));
        let start = Instant::now();
        assert!(paced.due(start));
        // Cheap: the floor decides.
        paced.ran(start, start + Duration::from_millis(10));
        assert!(!paced.due(start + Duration::from_millis(900)));
        assert!(paced.due(start + Duration::from_millis(1010)));
        // Costly: twenty times its cost.
        paced.ran(start, start + Duration::from_millis(200));
        assert!(!paced.due(start + Duration::from_secs(4)));
        assert!(paced.due(start + Duration::from_millis(4200)));
    }

    /// A namespace that changed before it merged the records (its own start, a file copied in
    /// while the host had not answered) sends those changes with its next writes: each record
    /// then holds every change of it, with nothing a write of this device builds on missing.
    #[test]
    fn what_a_namespace_made_before_the_records_arrived_goes_out() {
        // The workspace's tree as another device wrote it: its own record and one chunk.
        let mut desktop = Namespace::new("desktop").unwrap();
        desktop
            .create_file(tilo_fs::namespace::ROOT_INO, "handbook.md.txt", None, 1, 1)
            .unwrap();
        let founded: BTreeMap<TreeChunk, (Vec<u8>, Vec<automerge::ChangeHash>)> = desktop
            .document_heads()
            .into_iter()
            .map(|(chunk, heads)| (chunk, (desktop.save_document(chunk).unwrap(), heads)))
            .collect();

        // This device copied a file in before the records arrived, then merged them.
        let mut laptop = Namespace::new("laptop").unwrap();
        let hash = iroh_blobs::Hash::new(b"net 30");
        laptop
            .create_file(
                tilo_fs::namespace::ROOT_INO,
                "contract.txt",
                Some(&hash),
                6,
                1,
            )
            .unwrap();
        laptop
            .take_documents(
                founded
                    .iter()
                    .map(|(chunk, (bytes, _))| (*chunk, bytes.clone()))
                    .collect(),
            )
            .unwrap();
        let known: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>> = founded
            .iter()
            .map(|(chunk, (_, heads))| (*chunk, heads.clone()))
            .collect();

        // What the host keeps: each record as founded, then this device's writes of it.
        let mut host = Namespace::from_records(
            "host",
            founded
                .into_iter()
                .map(|(chunk, (bytes, _))| (chunk, bytes)),
        )
        .unwrap();
        let writes = laptop.document_writes(&known);
        assert!(!writes.is_empty());
        host.take_documents(writes.into_iter().map(|w| (w.chunk, w.changes)).collect())
            .unwrap();
        for name in ["contract.txt", "handbook.md.txt"] {
            assert!(
                host.lookup(tilo_fs::namespace::ROOT_INO, name).is_some(),
                "{name} reached the host"
            );
        }
        assert_eq!(host.document_heads(), laptop.document_heads());
    }

    #[test]
    fn each_record_is_kept_in_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let mut namespace = Namespace::new("laptop").unwrap();
        namespace
            .create_file(tilo_fs::namespace::ROOT_INO, "a.txt", None, 1, 1)
            .unwrap();
        for chunk in namespace.records() {
            std::fs::write(
                dir.path().join(format!("{chunk}.automerge")),
                namespace.save_document(chunk).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("stray"), b"x").unwrap();
        let (kept, documents) = KeptDocuments::open(dir.path()).unwrap();
        let (mut read, _) = open_namespace("laptop", &kept, documents).unwrap();
        assert!(read.lookup(tilo_fs::namespace::ROOT_INO, "a.txt").is_some());
        assert_eq!(read.document_heads(), namespace.document_heads());
    }

    /// What the server of the test below holds: each chunk's document.
    type Server = BTreeMap<TreeChunk, automerge::AutoCommit>;

    /// One pass of the sync as [`Sync::push`] orders it: every write that may go out does, the
    /// server takes each in turn, and its word that it holds a chunk comes a pass later. After
    /// each write the server takes (where a kill could end this device's process), the server's
    /// tree lists only chunks it holds. Answers whether anything went out.
    #[allow(
        clippy::disallowed_methods,
        reason = "the server here takes each write's changes as they come, as a server's store does"
    )]
    fn pass(
        laptop: &mut Namespace,
        documents: &mut BTreeMap<TreeChunk, Held>,
        server: &mut Server,
        confirmed_next: &mut Vec<TreeChunk>,
    ) -> bool {
        for chunk in confirmed_next.drain(..) {
            documents.entry(chunk).or_default().durable = true;
        }
        let heads = laptop.document_heads();
        let unheld = unheld_chunks(documents, heads.keys().copied());
        let mut going = Vec::new();
        for (chunk, now) in &heads {
            let held = documents.entry(*chunk).or_default();
            if held.sent == *now || !may_go_out(*chunk, &unheld) {
                continue;
            }
            let write = laptop
                .document_write(*chunk, &held.sent, usize::MAX)
                .unwrap();
            held.sent = write.heads;
            going.push((*chunk, write.changes));
            if unheld.contains(chunk) {
                confirmed_next.push(*chunk);
            }
        }
        let went = !going.is_empty() || !confirmed_next.is_empty();
        for (chunk, changes) in going {
            server
                .entry(chunk)
                .or_default()
                .load_incremental(&changes)
                .unwrap();
            let listed = server
                .get(&0)
                .map(fs_protocol::tree::listed_chunks)
                .unwrap_or_default();
            for listed in listed {
                assert!(
                    server
                        .get_mut(&listed)
                        .is_some_and(|doc| !doc.get_heads().is_empty()),
                    "the server's tree lists chunk {listed}, which it does not hold"
                );
            }
        }
        went
    }

    /// A repository's worth of files written into the folder in bursts, each burst going out as
    /// the sync orders it: wherever the process is killed, the server's tree lists only chunks
    /// it holds, and once the bursts end every change reaches the server.
    #[test]
    fn the_tree_never_lists_a_chunk_the_workspace_does_not_hold() {
        let mut laptop = Namespace::new("laptop").unwrap();
        let mut documents: BTreeMap<TreeChunk, Held> = BTreeMap::new();
        documents.entry(0).or_default().durable = true;
        let mut server = Server::new();
        let mut confirmed_next = Vec::new();
        for burst in 0..45 {
            for n in 0..70 {
                laptop
                    .create_file(
                        tilo_fs::namespace::ROOT_INO,
                        format!("object-{burst}-{n}"),
                        None,
                        1,
                        1,
                    )
                    .unwrap();
            }
            pass(
                &mut laptop,
                &mut documents,
                &mut server,
                &mut confirmed_next,
            );
        }
        while pass(
            &mut laptop,
            &mut documents,
            &mut server,
            &mut confirmed_next,
        ) {}
        let listed = fs_protocol::tree::listed_chunks(server.get(&0).unwrap());
        assert!(
            listed.len() > 2,
            "the files filled several chunks: {listed:?}"
        );
        for (chunk, heads) in laptop.document_heads() {
            assert_eq!(
                server.get_mut(&chunk).map(automerge::AutoCommit::get_heads),
                Some(heads),
                "document {chunk} reached the server whole"
            );
        }
    }

    /// Keeping saves each chunk that changed in a file of its own, and then nothing until a
    /// chunk changes again: a stop pays for the chunks that changed, not for the tree.
    #[test]
    fn keeping_writes_what_changed_and_then_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, _) = KeptDocuments::open(dir.path()).unwrap();
        let mut namespace = Namespace::new("laptop").unwrap();
        kept.opened(&mut namespace, ReadHeads::new()).unwrap();
        namespace
            .create_file(tilo_fs::namespace::ROOT_INO, "a.txt", None, 1, 1)
            .unwrap();
        let namespace = Mutex::new(namespace);
        let saved = |chunk: TreeChunk| {
            std::fs::metadata(dir.path().join(format!("{chunk}.automerge")))
                .and_then(|file| file.modified())
                .ok()
        };
        kept.keep(&namespace).unwrap();
        let chunks: Vec<TreeChunk> = namespace
            .lock_recover()
            .document_heads()
            .into_keys()
            .collect();
        let first: Vec<_> = chunks.iter().map(|chunk| saved(*chunk)).collect();
        assert!(
            first.iter().all(Option::is_some),
            "every chunk of the tree is saved"
        );
        // Nothing changed: nothing is written again.
        std::thread::sleep(Duration::from_millis(20));
        kept.keep(&namespace).unwrap();
        assert_eq!(
            chunks.iter().map(|chunk| saved(*chunk)).collect::<Vec<_>>(),
            first
        );
    }

    #[test]
    fn a_host_that_sent_frames_of_a_record_holds_it() {
        assert!(!super::holds_some(None));
        assert!(!super::holds_some(Some(DocumentAnswer::Sent { frames: 0 })));
        assert!(super::holds_some(Some(DocumentAnswer::Sent { frames: 2 })));
        assert!(!super::holds_some(Some(DocumentAnswer::NotYours)));
    }
    /// The files the tree names, each with the hash of its content and its size, are what
    /// `tilo status` counts: a text document the tree holds itself has no hash.
    #[test]
    fn the_files_the_tree_names_are_counted_with_their_sizes() {
        let mut namespace = Namespace::new("laptop").unwrap();
        namespace
            .create_file(tilo_fs::namespace::ROOT_INO, "a.txt", None, 7, 1)
            .unwrap();
        namespace
            .create_dir(tilo_fs::namespace::ROOT_INO, "folder", 1)
            .unwrap();
        assert_eq!(files_of(&namespace), vec![(None, 7)]);
    }
}
