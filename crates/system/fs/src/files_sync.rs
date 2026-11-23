//! A namespace kept in step with the workspace's store: what a folder holds is the index's page
//! over the nodes nested in it, and every change made here is the verb it is, written through
//! the store ([`crate::files_sync::Peer`], the service on a device or the in-process store on a
//! host that keeps no socket).
//!
//! The store is the tree. This device's namespace is the copy the mount serves; it is made by
//! paging the store and kept in step when the store says something changed, and a change made
//! here is applied to the copy and handed out as its verb. There is no record of the tree on
//! either side: a node is a resource of the kind `files.node`, nested in the node that holds it.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs_protocol::FileAction;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;

use crate::namespace::{MountAuthor, Namespace};

pub mod remote;

/// The app whose verbs write a workspace's tree.
pub const FILES_APP: &str = fs_protocol::tree::FILES_APP;

/// The socket below a session's services the tree is served on.
pub const SOCKET: &str = "files-tree";

/// How long a burst of changes made here waits for the next, so a copy of many files is one
/// step of history.
const BATCH: Duration = Duration::from_millis(200);

/// How many nodes one page of a folder shows.
pub const PAGE: usize = 1000;

/// One node of a folder's page: what the store holds of it, as the tree names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRow {
    /// Its key: the identity a rename or a move never changes.
    pub key: String,
    /// Where it stands: a folder's key, [`fs_protocol::tree::ROOT_ID`] or
    /// [`fs_protocol::tree::TRASH_ID`].
    pub parent: String,
    /// Its name as people read it.
    pub name: String,
    /// Its name's exact bytes, when it is not UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_name: Option<Vec<u8>>,
    pub folder: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub size: u64,
    pub mtime: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_from: Option<String>,
}

/// One page of a folder: its nodes in their order, and where the next page starts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodePage {
    pub rows: Vec<NodeRow>,
    /// Where the next page starts; `None` when this one is the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Vec<u8>>,
}

/// What a sync reads and writes of a workspace's store.
pub trait Peer: collab_types::signer::MaybeSendSync {
    /// One page of the nodes directly under `parent`.
    ///
    /// # Errors
    /// The workspace is not held here, or the service does not answer.
    fn list(&self, parent: &str, after: Option<Vec<u8>>, limit: usize) -> Result<NodePage, String>;

    /// How many nodes are directly under `parent`.
    ///
    /// # Errors
    /// The workspace is not held here, or the service does not answer.
    fn count(&self, parent: &str) -> Result<usize, String>;

    /// Write `actions` as one burst named `burst`, as this device.
    ///
    /// # Errors
    /// The store refused a verb.
    fn apply(&self, burst: &str, actions: &[FileAction]) -> Result<(), String>;

    /// Whether this device holds the workspace, as the peer last said; `None` while it cannot.
    fn held(&self) -> Option<bool> {
        None
    }

    /// How many blobs of the workspace's content its holders lack, as the peer last said.
    fn host_lacks(&self) -> Option<u32> {
        None
    }

    /// Ask the workspace again for file content whose answer stalled.
    fn reask_content(&self) {}
}

/// What woke the sync.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Wake {
    /// A record of the tree was written. Retired: the store's pages are read instead.
    RetiredWrote,
    /// A change that named no record. Retired.
    RetiredMoved,
    /// The workspace's host took part again. Retired.
    RetiredHostReady,
    /// The host refused a write. Retired.
    RetiredRefused,
    /// The host answered for a record. Retired.
    RetiredOpened,
    /// The holders said what they keep. Retired.
    RetiredHolders,
    /// A node of the tree changed in the store: read the tree again.
    Changed,
}

/// Where the copy stands, as `tilo status` says it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a fact `tilo status` reports on its own"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The store answered the tree: what is here is what it holds.
    pub opened: bool,
    /// This device does not hold the workspace yet.
    pub waiting_for_workspace: bool,
    /// Every change made here was handed to the store.
    pub sent: bool,
    /// The store holds every change made here.
    pub durable: bool,
    /// Files and folders the tree holds.
    pub files: usize,
    /// Their bytes, all of them.
    pub bytes: u64,
    /// Of those bytes, the ones this computer holds, as last counted.
    #[serde(skip)]
    pub local_bytes: u64,
    /// Not syncing, as the person asked.
    pub paused: bool,
    /// Why changes made here waited, while they do.
    pub stuck: Vec<Waiting>,
    /// How many blobs of the workspace's content the holder that keeps the most of it still
    /// lacks, by the holders' own word.
    pub host_lacks: Option<u32>,
    /// What the folder is doing, from the facts above.
    pub state: FolderState,
}

impl Status {
    /// The state these facts say.
    #[must_use]
    pub fn folder_state(&self) -> FolderState {
        if self.paused {
            FolderState::Paused
        } else if self.waiting_for_workspace {
            FolderState::WaitingForWorkspace
        } else if !self.opened {
            FolderState::Opening
        } else if !self.stuck.is_empty() {
            FolderState::Stuck
        } else if !(self.sent && self.durable) {
            FolderState::Sending
        } else if self.host_lacks.is_some_and(|blobs| blobs > 0) {
            FolderState::ContentGoingOut
        } else {
            FolderState::InSync
        }
    }
}

/// What a folder is doing, as a person is told it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderState {
    Paused,
    WaitingForWorkspace,
    #[default]
    Opening,
    Stuck,
    Sending,
    ContentGoingOut,
    InSync,
}

/// Why a change made here waits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Waiting {
    /// The store has not answered the tree yet.
    Unopened,
    /// Retired: the store's pages answer what arrived.
    RetiredUnanswered,
    /// Retired.
    RetiredUnarrived,
    /// Retired.
    RetiredOrdered,
    /// The workspace refused the change, for the reason it gave.
    Refused(Refusal),
    /// Retired.
    RetiredUnsent,
    /// Retired.
    RetiredLacking,
}

/// Why the store did not take a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// Retired: the store's own history takes every change.
    RetiredHistoryFull,
    /// The workspace's rules do not let this person make the change.
    NotPermitted,
    /// The store did not answer within the wait.
    Unanswered,
    /// Any other refusal, in the words of whoever refused it.
    Other(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RetiredHistoryFull => f.write_str("the workspace's history was full"),
            Self::NotPermitted => f.write_str("the workspace's rules do not take the change"),
            Self::Unanswered => f.write_str("the store did not answer within the wait"),
            Self::Other(why) => f.write_str(why),
        }
    }
}

impl From<String> for Refusal {
    fn from(why: String) -> Self {
        Self::Other(why)
    }
}

/// How a sync is started.
pub struct Config {
    /// The workspace, by its id.
    pub workspace: String,
    /// Whose copy this is: who its changes are made as.
    pub mount: MountAuthor,
    /// The workspace's store.
    pub peer: Arc<dyn Peer>,
    /// Where this workspace's file bytes are kept; `None` for a copy that counts none as here.
    pub content: Option<Arc<crate::ContentStore>>,
    /// The wakes the peer sends.
    pub wakes: tokio::sync::mpsc::UnboundedReceiver<Wake>,
}

/// Where the copy stands, and its followers.
struct Shown {
    status: Mutex<Status>,
    changed: tokio::sync::watch::Sender<Status>,
}

impl Shown {
    fn new() -> Self {
        Self {
            status: Mutex::new(Status::default()),
            changed: tokio::sync::watch::channel(Status::default()).0,
        }
    }

    fn publish(&self) {
        let now = self.status.lock_recover().clone();
        self.changed.send_if_modified(|shown| {
            let changed = *shown != now;
            if changed {
                *shown = now;
            }
            changed
        });
    }
}

/// What the sync and the mount's journal share.
struct Inner {
    peer: Arc<dyn Peer>,
    shown: Arc<Shown>,
    /// One burst reaches the store at a time.
    applying: Mutex<()>,
    /// Verbs handed to the store and not answered yet.
    inflight: AtomicU64,
    /// How many bursts were made here: the name each is told by.
    burst: AtomicU64,
    refused: Mutex<Option<String>>,
}

impl Inner {
    /// Hand the verbs `namespace` holds to the store as one burst. On a refusal they are put
    /// back, so nothing made here is lost to it.
    fn flush(&self, namespace: &mut Namespace) -> Result<(), String> {
        let actions = namespace.take_actions();
        if actions.is_empty() {
            return Ok(());
        }
        let _burst = self.applying.lock_recover();
        self.inflight.store(
            u64::try_from(actions.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        let name = format!("files-{}", self.burst.fetch_add(1, Ordering::Relaxed));
        let applied = self.peer.apply(&name, &actions);
        self.inflight.store(0, Ordering::Relaxed);
        match applied {
            Ok(()) => {
                *self.refused.lock_recover() = None;
                Ok(())
            }
            Err(why) => {
                tracing::warn!(%why, "the workspace did not take a change of its files");
                *self.refused.lock_recover() = Some(why.clone());
                namespace.restore_actions(actions);
                Err(why)
            }
        }
    }
}

/// One workspace's tree, held while this value lives.
pub struct StoreSync {
    /// The tree, as the mount serves it.
    pub namespace: Arc<Mutex<Namespace>>,
    inner: Arc<Inner>,
    paused: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    refresh: Arc<AtomicBool>,
    passes: Arc<AtomicU64>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for StoreSync {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl StoreSync {
    /// Open `config.workspace`'s tree for `config.mount`, kept in step with `config.peer`.
    ///
    /// # Errors
    /// The namespace cannot be made.
    pub fn start(config: Config) -> Result<Self, automerge::AutomergeError> {
        let Config {
            workspace: _,
            mount,
            peer,
            content,
            wakes,
        } = config;
        let namespace = Arc::new(Mutex::new(Namespace::new(mount)?));
        let shown = Arc::new(Shown::new());
        let inner = Arc::new(Inner {
            peer,
            shown: Arc::clone(&shown),
            applying: Mutex::default(),
            inflight: AtomicU64::new(0),
            burst: AtomicU64::new(0),
            refused: Mutex::new(None),
        });
        let paused = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(tokio::sync::Notify::new());
        let refresh = Arc::new(AtomicBool::new(false));
        let passes = Arc::new(AtomicU64::new(0));
        let running = Arc::new(Running {
            namespace: Arc::clone(&namespace),
            content,
            inner: Arc::clone(&inner),
            shown,
            paused: Arc::clone(&paused),
            stopping: Arc::clone(&stopping),
            wake: Arc::clone(&wake),
            refresh: Arc::clone(&refresh),
            passes: Arc::clone(&passes),
        });
        let task = tilo_rt::app().spawn(async move { running.run(wakes).await });
        Ok(Self {
            namespace,
            inner,
            paused,
            stopping,
            wake,
            refresh,
            passes,
            task: Some(task),
        })
    }

    /// What the mount hands each change it makes to, before it answers the request that made
    /// it: the change's verbs go to the store, and a refusal is the request's failure.
    #[must_use]
    pub fn journal(&self) -> crate::fs::ChangeJournal {
        let inner = Arc::clone(&self.inner);
        Arc::new(move |namespace: &mut Namespace| {
            inner
                .flush(namespace)
                .map_err(|why| std::io::Error::other(why))
        })
    }

    /// Nothing to make durable beyond what [`Self::journal`] already wrote.
    #[must_use]
    pub fn sync_journal(&self) -> crate::fs::DurabilityHook {
        Arc::new(|| Ok(()))
    }

    /// Where the copy stands.
    #[must_use]
    pub fn status(&self) -> Status {
        self.inner.shown.status.lock_recover().clone()
    }

    /// Where the copy stands each time that changed.
    #[must_use]
    pub fn status_changes(&self) -> tokio::sync::watch::Receiver<Status> {
        self.inner.shown.changed.subscribe()
    }

    /// The wake wait: whatever notices the store changed calls
    /// [`Notify::notify_waiters`] on it.
    #[must_use]
    pub fn wake(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.wake)
    }

    /// Read this tree as the store has it now: the pages are read again first.
    pub async fn read<R>(&self, read: impl FnOnce(&mut Namespace) -> R) -> R {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let before = self.passes.load(Ordering::Relaxed);
                self.refresh.store(true, Ordering::Relaxed);
                self.wake.notify_waiters();
                while self.passes.load(Ordering::Relaxed) == before {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if !self.refresh.load(Ordering::Relaxed) {
                    break;
                }
            }
        })
        .await;
        let mut namespace = self.namespace.lock_recover();
        read(&mut namespace)
    }

    /// Change this tree with `change`, as `acting`: the verbs it makes are written into the
    /// copy and handed to the store before this answers.
    ///
    /// # Errors
    /// What `change` refused, or the store's refusal.
    pub async fn change<R>(
        &self,
        acting: Option<tilo_actions::ActionContext>,
        change: impl FnOnce(&mut Namespace) -> Result<R, String>,
    ) -> Result<R, String> {
        let changed = {
            let mut namespace = self.namespace.lock_recover();
            let _acting = crate::namespace::acting_as(acting);
            let changed = change(&mut namespace)?;
            self.inner.flush(&mut namespace)?;
            changed
        };
        self.inner.shown.publish();
        Ok(changed)
    }

    /// Wait until every change this copy made reached the store, at most `within`.
    ///
    /// # Errors
    /// The changes did not all go out within `within`.
    pub async fn wait_sent(&self, within: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            {
                let mut namespace = self.namespace.lock_recover();
                if self.inner.flush(&mut namespace).is_err() {
                    return Err("the workspace refused a change of its files".to_owned());
                }
            }
            if self.inner.inflight.load(Ordering::Relaxed) == 0
                && !self.namespace.lock_recover().has_pending()
            {
                return Ok(());
            }
            if started.elapsed() >= within {
                return Err(format!(
                    "the changes did not reach the store within {}s",
                    within.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Stop syncing and end the task, at most `within`.
    pub async fn stop(mut self, within: Duration) {
        self.stopping.store(true, Ordering::Relaxed);
        self.wake.notify_waiters();
        if let Some(task) = self.task.take()
            && tokio::time::timeout(within, task).await.is_err()
        {
            tracing::warn!("a workspace's tree did not finish in time");
        }
    }

    /// Pause or resume syncing, as the person asked.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        let mut status = self.inner.shown.status.lock_recover();
        status.paused = paused;
        status.state = status.folder_state();
        drop(status);
        self.inner.shown.publish();
        self.wake.notify_waiters();
    }
}

/// What the sync task holds.
struct Running {
    namespace: Arc<Mutex<Namespace>>,
    /// Where this workspace's file bytes are kept, for counting what this computer holds.
    content: Option<Arc<crate::ContentStore>>,
    inner: Arc<Inner>,
    shown: Arc<Shown>,
    paused: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    refresh: Arc<AtomicBool>,
    passes: Arc<AtomicU64>,
}

impl Running {
    async fn run(self: Arc<Self>, mut wakes: tokio::sync::mpsc::UnboundedReceiver<Wake>) {
        let mut changes = self.namespace.lock_recover().subscribe_changes();
        self.pull();
        loop {
            if self.stopping.load(Ordering::Relaxed) {
                break;
            }
            if !self.paused.load(Ordering::Relaxed) {
                if self.refresh.swap(false, Ordering::Relaxed) {
                    self.pull();
                }
                let mut namespace = self.namespace.lock_recover();
                let _ = self.inner.flush(&mut namespace);
                drop(namespace);
            }
            self.passes.fetch_add(1, Ordering::Relaxed);
            self.say_where_it_stands();
            tokio::select! {
                wake = wakes.recv() => {
                    match wake {
                        Some(Wake::Changed) => self.pull(),
                        Some(_retired) => {}
                        None => break,
                    }
                }
                changed = changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    tokio::time::sleep(BATCH).await;
                }
                () = self.wake.notified() => {}
            }
        }
    }

    /// Read every folder's page and make the copy hold exactly what the store says.
    fn pull(&self) {
        let mut stack = vec![
            fs_protocol::tree::ROOT_ID.to_owned(),
            fs_protocol::tree::TRASH_ID.to_owned(),
        ];
        let mut present = HashSet::new();
        let mut pages = Vec::new();
        let mut complete = true;
        while let Some(parent) = stack.pop() {
            match self.list_folder(&parent) {
                Ok(rows) => {
                    for row in &rows {
                        present.insert(row.key.clone());
                        if row.folder {
                            stack.push(row.key.clone());
                        }
                    }
                    pages.push(rows);
                }
                Err(why) => {
                    tracing::warn!(%why, parent, "a folder of the workspace's tree did not read");
                    complete = false;
                }
            }
        }
        let mut namespace = self.namespace.lock_recover();
        namespace.set_collecting(false);
        for rows in &pages {
            for row in rows {
                namespace.apply_store_row(row);
            }
        }
        if complete {
            for id in namespace.all_node_ids() {
                if id != fs_protocol::tree::ROOT_ID && !present.contains(&id) {
                    namespace.forget_store_node(&id);
                }
            }
        }
        namespace.set_collecting(true);
        let (files, bytes) = namespace.live_totals();
        // What this computer holds, by the tree's own hashes: a text document the tree holds
        // itself is here whenever the tree is.
        let mut local = 0_u64;
        for id in namespace.live_node_ids() {
            let Some(entry) = namespace.entry_for_node(&id) else {
                continue;
            };
            if entry.kind != crate::namespace::NodeKind::File {
                continue;
            }
            if entry
                .hash
                .is_none_or(|hash| self.content.as_ref().is_some_and(|store| store.holds(hash)))
            {
                local = local.saturating_add(entry.size);
            }
        }
        drop(namespace);
        {
            let mut status = self.shown.status.lock_recover();
            status.opened |= complete;
            status.files = files;
            status.bytes = bytes;
            status.local_bytes = local;
            status.waiting_for_workspace = self.inner.peer.held() == Some(false);
            status.host_lacks = self.inner.peer.host_lacks();
        }
        self.inner.peer.reask_content();
    }

    /// Every row of `parent`, a page at a time.
    fn list_folder(&self, parent: &str) -> Result<Vec<NodeRow>, String> {
        let mut rows = Vec::new();
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page = self.inner.peer.list(parent, after.clone(), PAGE)?;
            rows.extend(page.rows);
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(rows),
            }
        }
    }

    fn say_where_it_stands(&self) {
        let pending = self
            .namespace
            .lock_recover()
            .has_pending()
            .then_some(1)
            .unwrap_or(0);
        let inflight = self.inner.inflight.load(Ordering::Relaxed);
        let refused = self.inner.refused.lock_recover().clone();
        let mut status = self.shown.status.lock_recover();
        status.sent = pending == 0;
        status.durable = pending == 0 && inflight == 0 && refused.is_none();
        status.stuck = if !status.opened {
            vec![Waiting::Unopened]
        } else if let Some(why) = refused {
            vec![Waiting::Refused(Refusal::Other(why))]
        } else {
            Vec::new()
        };
        status.state = status.folder_state();
        drop(status);
        self.shown.publish();
    }
}
