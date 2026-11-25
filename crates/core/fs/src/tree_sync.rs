//! One workspace's file tree kept converged between a [`Namespace`] on this computer and the
//! workspace's records: the tree's own record and its chunks, each a record of its own.
//!
//! A workspace's file truth is its tree ([`fs_protocol::tree`]): the tree's own record and its
//! chunks, each a record of its own, whose nodes name each file's content by hash. This module
//! holds one such tree on this computer and keeps it in step with the peer the workspace is
//! reached through: a change a record brings merges into the namespace (and what resolving it
//! wrote goes back), and a change the namespace makes (a file copied into the mount) goes out as
//! the next change of the record that holds the file. A write costs what its chunk costs,
//! whatever the tree holds: one change of one record, kept on disk as that record alone.
//!
//! A document reaches the workspace before anything names it: a chunk this device starts goes
//! out, and the workspace's server says it holds it, before any other document of the tree goes
//! out again (the tree's own document lists the chunk, and another chunk's node may sit in a
//! folder of it). A tree whose own document names a chunk the workspace never got reads as
//! missing what that chunk holds, on every device; this order never makes one, whenever this
//! process ends.
//!
//! Every change the mount makes is on disk before the mount answers for it
//! ([`crate::kept_documents`]): a process killed in the middle of a burst starts again with every
//! file it answered for, and sends what the workspace does not hold yet. The heads each record
//! was last merged at are kept beside it, so a restart asks each record only what changed since.
//!
//! Writes that come in a burst (a checkout writing thousands of files) go out together: the
//! changes the namespace made within [`BATCH`] of the first one leave as one write per record
//! they changed. A change made whole at once ([`TreeSync::change`]: a put, a file manager's verb)
//! is its own burst, and goes out without that wait.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs_protocol::TreeChunk;
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::DocumentAnswer;

use crate::kept_documents::KeptDocuments;
use crate::namespace::{MountAuthor, Namespace};

mod change;
pub mod remote;
pub use change::{FileToPut, PlacedFile};

/// The app whose record a workspace's tree is.
pub const FILES_APP: &str = fs_protocol::tree::FILES_APP;

/// The route a record of `workspace`'s tree is opened by.
#[must_use]
pub fn document_for(chunk: TreeChunk) -> String {
    fs_protocol::tree::chunk_document_id(chunk)
}

/// How often the namespace is kept on disk while it changes, at most.
const KEEP_EVERY: Duration = Duration::from_secs(2);
/// How often the files and bytes the tree names are counted again while it changes, at most.
const COUNT_EVERY: Duration = Duration::from_secs(1);
/// How long a document of the tree that does not read here waits before it is asked of the
/// workspace again, the first time: each time after waits twice as long, up to
/// [`ASKED_AT_MOST_EVERY`].
const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(5);
const ASKED_AT_MOST_EVERY: Duration = Duration::from_secs(300);
/// How long changes made here may wait for a document of the tree that does not read here before
/// the folder says they wait, and why, rather than that they are going out.
const STUCK_AFTER: Duration = Duration::from_secs(120);
/// How often the bytes this computer holds of the files are counted again while some are not
/// here yet: content arrives without the tree changing.
const LOCAL_AGAIN: Duration = Duration::from_secs(5);
/// How long the changes after a first unsent one wait to go out with it: a
/// burst of writes becomes one write per record, and a single write waits no
/// longer than a person notices.
const BATCH: Duration = Duration::from_millis(200);
/// How often changes whose batch is due, but that wait for the workspace, are looked at again
/// when nothing woke the task sooner.
const WAITING_LOOK: Duration = Duration::from_millis(50);
/// How long a state of the tree that says its changes went out waits to be read again, before it
/// is taken for one written after the last change: longer than a pass that finds the batch not
/// due yet.
const SETTLED_AGAIN: Duration = Duration::from_millis(60);

/// How many bytes of changes one write of a record carries at most: well under the most a
/// document write may carry, so changes made while a record could not go out go out over
/// several writes, each building on the last, instead of one no host takes.
const WRITE_BUDGET: usize = collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES / 2;
/// How many times its own cost a step that walks or saves the whole tree waits before it runs
/// again: it then takes at most a twentieth of the replica's time. Run on every change, a count
/// of every node made writing a repository's twenty thousand files quadratic, and held the tree
/// (which every write into the folder waits for) for most of the time.
const PACE: u32 = 20;

/// A file the tree names: the hash of its content (`None` for a text document the tree holds
/// itself) and its size.
type FileBytes = (Option<iroh_blobs::Hash>, u64);

/// What a record of the tree read as, for [`Records`]. The shape of a device's own
/// `DeviceDocuments` read, without what a folder sync does not use: its revision names a
/// document, and the heads here already do.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Reading {
    /// The record whole, saved, at the heads it stands at.
    Whole {
        /// The document's heads.
        heads: Vec<[u8; 32]>,
        /// The document, saved.
        bytes: Vec<u8>,
    },
    /// Only the changes since the heads the reader holds, which they complete to `heads`.
    Changes {
        /// The heads the changes complete to.
        heads: Vec<[u8; 32]>,
        /// The changes, as raw change chunks.
        bytes: Vec<u8>,
    },
    /// The record stands where the reader last read it.
    Unchanged,
    /// The peer holds nothing of it.
    Absent,
    /// It is not here yet: a checkpoint of it is arriving, or the keys that open it are on
    /// their way, and it reads once they are.
    Pending,
}

/// What a tree sync reads of a workspace's records: the tree's own and its chunks, as the
/// device's own service (or the engine of a device that holds no service) reads them for it.
/// A device's own `DeviceDocuments` answers it as it is.
pub trait Records: collab_types::signer::MaybeSendSync {
    /// `app`'s `document` of `workspace` whole, or, for `since` heads the peer holds, only
    /// what changed after them.
    ///
    /// # Errors
    /// The peer is not open, holds no such workspace, the device may not read it, or it cannot
    /// be read now.
    fn read(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        since: Option<&[[u8; 32]]>,
    ) -> Result<Reading, String>;
}

/// What a tree sync reads of the peer beyond the workspace's records: what the records' host
/// answered, whether this device's writes to one are there, and how much it still keeps of
/// them. A device's own service answers these without a network hop.
pub trait TreePeer: collab_types::signer::MaybeSendSync {
    /// Ask the peer for record `chunk` of the tree, and to take part in its channel.
    fn open(&self, chunk: TreeChunk);

    /// What the peer's host answered about `chunk`: `None` while it owes an answer; once it
    /// came, the frames it sent — none when the workspace has none of the record — or that it
    /// is another person's own.
    fn answer(&self, chunk: TreeChunk) -> Option<DocumentAnswer>;

    /// Whether the peer holds anything of `chunk`.
    fn holds(&self, chunk: TreeChunk) -> bool;

    /// Whether this device's writes to `chunk` are in the peer, not read yet.
    fn holding(&self, chunk: TreeChunk) -> bool;

    /// How many of this device's writes the peer still keeps of anything.
    fn pending(&self) -> usize;

    /// Ask the peer again for file content whose answer stalled.
    fn reask_content(&self);

    /// How many blobs of the workspace's content the holder that keeps the most of it still
    /// lacks, as the peer last heard: `None` while no holder said.
    fn host_lacks(&self) -> Option<u32> {
        None
    }

    /// Whether this device holds the workspace the tree is part of, as the peer knows it:
    /// `Some(false)` while it does not, so the folder says it waits for the workspace to
    /// arrive. `None` when the peer cannot say.
    fn holds_workspace(&self) -> Option<bool> {
        None
    }
}

/// A write of one record under way, for the tree sync's own task.
pub type WriteFuture<'a> =
    Pin<Box<dyn std::future::Future<Output = Result<(), Refusal>> + Send + 'a>>;

/// The wait that a set of this device's writes is held by the serving side.
pub type Served = Pin<Box<dyn std::future::Future<Output = Result<(), Refusal>> + Send + 'static>>;

/// The records of a workspace's tree, written as this device itself: the tree's own record and
/// its chunks, one write per record.
pub trait TreeWriter {
    /// Write `changes` to record `chunk` of the tree: founding the record from them when the
    /// workspace holds none of it, a change of it otherwise. A record this device starts carries
    /// its canonical founding first, so every copy of it derives the one founding.
    fn write<'a>(&'a mut self, chunk: TreeChunk, changes: &'a [u8]) -> WriteFuture<'a>;

    /// Resolves once the serving side answered every earlier write of this device's to
    /// `chunks`, or with the refusal that prevents it; `None` when the peer cannot answer (it
    /// is gone), so nothing waits for a word that will not come. A writer whose writes are kept
    /// where they are made owes no wait, and answers at once.
    fn served(&self, _chunks: &[TreeChunk]) -> Option<Served> {
        Some(Box::pin(std::future::ready(Ok(()))))
    }
}

/// What woke the sync: something the peer did, or said.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Wake {
    /// The record `chunk` of the tree was written: read it.
    Wrote(TreeChunk),
    /// Something in the peer changed that names no record: read every record of the tree again.
    Moved,
    /// The workspace's host takes part again: every record of the tree is opened again.
    HostReady,
    /// The host refused a write of this device's to `chunk`, for `reason`.
    Refused { chunk: TreeChunk, reason: Refusal },
    /// The host answered for `chunk` what it holds of it.
    Opened {
        chunk: TreeChunk,
        answer: DocumentAnswer,
    },
    /// The workspace's holders said anew what they keep of its content: the folder's state is
    /// said again, and no record is read for it.
    Holders,
}

/// Where the replica stands, as `tilo status` says it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a fact `tilo status` reports on its own"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// The workspace's host answered for the tree: what it holds is merged.
    pub opened: bool,
    /// This device does not hold the workspace yet: the folder waits for it to arrive, and
    /// starts by itself when it does.
    pub waiting_for_workspace: bool,
    /// Every change made here was handed to the peer.
    pub sent: bool,
    /// The peer holds nothing of this device's that its host has not answered for.
    pub durable: bool,
    /// Files the tree names.
    pub files: usize,
    /// Where this folder stands on this computer: the mount a person opens, and a file manager
    /// shows as the workspace's place. Empty while the tree is kept for its records alone.
    pub path: String,
    /// Their bytes, all of them.
    pub bytes: u64,
    /// Of those bytes, the ones this computer holds, as last counted.
    #[serde(skip)]
    pub local_bytes: u64,
    /// Not syncing, as the person asked.
    pub paused: bool,
    /// Why changes made here have waited for longer than [`STUCK_AFTER`] without going out,
    /// while they do, the one a person most needs to hear first; empty while nothing waits.
    pub stuck: Vec<Waiting>,
    /// How many blobs of the workspace's content the holder that keeps the most of it still
    /// lacks, by the holders' own word: `None` while none said.
    pub host_lacks: Option<u32>,
    /// What the folder is doing, from the facts above: the one thing a person is told of it.
    pub state: FolderState,
}

/// Why changes made in a folder wait, which whoever tells a person words in the person's language.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Waiting {
    /// The workspace has not said which files it holds yet, so nothing changed here goes out.
    Unopened,
    /// The workspace has not answered for some of its files yet.
    Unanswered,
    /// Some of the workspace's file list has not arrived on this computer; it is asked for again
    /// every few minutes.
    Unarrived,
    /// Some changes wait for others to reach the workspace first.
    Ordered,
    /// The workspace refused some changes, for the reason it gave; they are sent again after a
    /// wait.
    Refused(Refusal),
    /// Some changes have not reached the workspace yet and are sent again.
    Unsent,
    /// The folder names files whose details neither this computer nor the workspace holds.
    Lacking,
}

/// Why the workspace did not take a write of this device's to a folder's tree.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// The record's history at the workspace has no room for more changes until it is
    /// compacted.
    HistoryFull,
    /// The workspace's rules do not let this person make the change.
    NotPermitted,
    /// The workspace did not answer the write within the wait.
    Unanswered,
    /// Any other refusal, in the words of whoever refused it: for logs and `tilo folders`.
    Other(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HistoryFull => f.write_str("the record's history at the workspace is full"),
            Self::NotPermitted => f.write_str("the workspace's rules do not take the change"),
            Self::Unanswered => f.write_str("the workspace did not answer within the wait"),
            Self::Other(why) => f.write_str(why),
        }
    }
}

impl From<String> for Refusal {
    fn from(why: String) -> Self {
        Self::Other(why)
    }
}

/// What a folder is doing, as a person is told it: one state, the first that holds of these.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderState {
    /// Not syncing, as the person asked.
    Paused,
    /// The workspace has not arrived on this device; the folder starts by itself when it does.
    WaitingForWorkspace,
    /// The workspace has not said what its file list holds yet.
    #[default]
    Opening,
    /// Changes made here wait, for the reason [`Status::stuck`] gives.
    Stuck,
    /// Changes made here are going out to the workspace.
    Sending,
    /// Every change went out, and the workspace's holders still lack some of the content:
    /// files whose bytes may be on this device alone.
    ContentGoingOut,
    /// Every change went out, and a holder keeps all of the content.
    InSync,
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

/// How a tree sync is started: the workspace it holds, the peer it reaches, where it keeps its
/// documents, and the channel its wakes come on.
pub struct Config {
    /// The workspace, by its id.
    pub workspace: String,
    /// The certified device the tree's documents name as their writer.
    pub device: [u8; 32],
    /// The person the device belongs to.
    pub person: [u8; 32],
    /// Where the namespace's records are kept between runs, one file per record.
    pub kept_at: PathBuf,
    /// The workspace's records, as this device's peer reads them.
    pub documents: Arc<dyn Records>,
    /// The workspace's records, written as this device.
    pub writer: Box<dyn TreeWriter + Send>,
    /// What the peer says of the records beside their contents.
    pub peer: Arc<dyn TreePeer>,
    /// Where this workspace's file bytes are kept; `None` for a tree kept for its records
    /// alone, which counts none of them as here.
    pub content: Option<Arc<crate::ContentStore>>,
    /// The wakes the peer sends, from [`TreeSync::wake`]'s waits.
    pub wakes: tokio::sync::mpsc::UnboundedReceiver<Wake>,
}

/// Where a tree's sync stands, and its changes: what a status reader follows instead of asking
/// again and again.
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

    fn lock_recover(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock_recover()
    }

    /// Tell the followers where the sync stands now, when that changed.
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

/// One workspace's tree, held while this value lives.
pub struct TreeSync {
    /// The tree, as the mount and the tests read it.
    pub namespace: Arc<Mutex<Namespace>>,
    kept: Arc<KeptDocuments>,
    status: Arc<Shown>,
    paused: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    /// Waited on by whatever notices the peer changed ([`Self::wake`]), and poked by
    /// [`Self::stop`] and [`Self::set_paused`].
    wake: Arc<tokio::sync::Notify>,
    /// How many passes the sync made, counted as each ends: what a read waits for a pass of.
    passes: Arc<std::sync::atomic::AtomicU64>,
    /// Set by a read that needs the tree as the workspace has it now: the next pass reads every
    /// record again.
    refresh: Arc<AtomicBool>,
    /// Set by [`Self::change`] once its change is made: a change made whole at once goes out at
    /// the next pass rather than waiting [`BATCH`] for more of a burst.
    whole: Arc<AtomicBool>,
    /// The task syncing this tree, until a stop waits for it.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for TreeSync {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// What the documents were read at when a namespace was opened
/// ([`Namespace::from_documents_read`]).
type ReadHeads = BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>;

/// The namespace `kept` holds for `author`, with each document's heads as read: its documents'
/// saves and journals, or the saves alone when a journal does not read (damaged on disk), or a
/// new one when nothing is kept.
fn open_namespace(
    mount: &MountAuthor,
    kept: &KeptDocuments,
    documents: Vec<(TreeChunk, Vec<u8>)>,
) -> std::io::Result<(Namespace, ReadHeads)> {
    let new = || {
        Namespace::new(mount.clone())
            .map(|namespace| (namespace, ReadHeads::new()))
            .map_err(std::io::Error::other)
    };
    if documents.is_empty() {
        return new();
    }
    match Namespace::from_documents_read(mount.clone(), documents) {
        Ok(opened) => Ok(opened),
        Err(error) => {
            tracing::warn!(%error, "the tree's journal does not read; it starts from the documents' last saves");
            let saved = kept.saved_only()?;
            if saved.is_empty() {
                return new();
            }
            Namespace::from_documents_read(mount.clone(), saved)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }
    }
}

impl TreeSync {
    /// Hold `config.workspace`'s tree over its peer, kept in `config.kept_at` between runs, as
    /// the device whose key is `config.device`; its file bytes come and go through
    /// `config.content`.
    ///
    /// # Errors
    /// The kept namespace cannot be read.
    pub async fn start(config: Config) -> std::io::Result<Self> {
        let Config {
            workspace,
            device,
            person,
            kept_at,
            documents: read,
            writer,
            peer,
            content,
            wakes,
        } = config;
        // The mount of the person's certified device holds this workspace's tree: every document
        // it publishes starts from the founding every copy of the record derives, and names the
        // person whose changes it carries.
        let mount = MountAuthor::person(
            hex::encode(device),
            tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(person)),
        );
        // Reading the kept chunks and indexing every node they name walks the whole tree. On
        // a runtime worker it would hold that worker — and the holder's clients, and the agents
        // it serves, wait for one — for as long as a repository's tree takes, so it goes to a
        // thread of the blocking pool instead.
        let (kept, namespace, records) = tokio::task::spawn_blocking(move || {
            let (kept, documents) = KeptDocuments::open(&kept_at)?;
            let (mut namespace, read) = open_namespace(&mount, &kept, documents)?;
            kept.opened(&mut namespace, read)?;
            // Where each record stood at the workspace when this device last merged it. A
            // record file that does not read is no record: the pass reads the record whole
            // again, which is what every start did before it was kept.
            let records = kept.records().unwrap_or_else(|error| {
                tracing::warn!(%error, "where the tree's records stood does not read; each is read whole again");
                BTreeMap::new()
            });
            Ok::<_, std::io::Error>((Arc::new(kept), namespace, records))
        })
        .await
        .map_err(std::io::Error::other)??;
        let namespace = Arc::new(Mutex::new(namespace));
        let status = Arc::new(Shown::new());
        let paused = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(tokio::sync::Notify::new());
        let passes = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let refresh = Arc::new(AtomicBool::new(false));
        let whole = Arc::new(AtomicBool::new(false));
        let sync = Sync {
            workspace,
            read,
            writer,
            peer,
            content,
            wakes,
            paused: Arc::clone(&paused),
            stopping: Arc::clone(&stopping),
            wake: Arc::clone(&wake),
            passes: Arc::clone(&passes),
            refresh: Arc::clone(&refresh),
            whole: Arc::clone(&whole),
            namespace: Arc::clone(&namespace),
            // Each record as the workspace held it when this device last merged it: the pass
            // asks only what changed since, and writes out only what the workspace lacks.
            // Heads the workspace does not know bring what it holds, never less.
            documents: records
                .into_iter()
                .map(|(chunk, heads)| {
                    let held = Held {
                        sent: heads.iter().copied().map(automerge::ChangeHash).collect(),
                        merged: Some(heads),
                        founded: true,
                        ..Held::default()
                    };
                    (chunk, held)
                })
                .collect(),
            records_changed: false,
            unread: BTreeSet::new(),
            unreadable: BTreeSet::new(),
            again: BTreeMap::new(),
            lacking: BTreeSet::new(),
            waiting: BTreeSet::new(),
            held_until: None,
            dirty_since: None,
            unsent: 0,
            kept: Arc::clone(&kept),
            ordered: BTreeSet::new(),
            ordered_for: BTreeSet::new(),
            pacing: Paced::new(KEEP_EVERY),
            counting: Paced::new(COUNT_EVERY),
            counted_heads: None,
            status: Arc::clone(&status),
            files: Arc::new(Vec::new()),
            local_counted: None,
        };
        let task = tokio::spawn(sync.run());
        Ok(Self {
            namespace,
            kept,
            status,
            paused,
            stopping,
            wake,
            passes,
            refresh,
            whole,
            task: Some(task),
        })
    }

    /// Stop syncing and keep the tree on disk, waiting at most `within` for it: a step still
    /// walking the tree cannot be interrupted, and a holder stopping must end inside the
    /// service's timeout whether or not one is.
    pub async fn stop(mut self, within: Duration) {
        self.stopping.store(true, Ordering::Relaxed);
        self.wake.notify_waiters();
        if let Some(task) = self.task.take()
            && tokio::time::timeout(within, task).await.is_err()
        {
            tracing::warn!("a workspace's tree did not keep in time; it is left as last kept");
        }
    }

    /// The wake wait: whatever notices the peer changed calls
    /// [`Notify::notify_waiters`] on it, which wakes the sync.
    #[must_use]
    pub fn wake(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.wake)
    }

    /// What the mount hands each change it makes to, before it answers the request that made
    /// it: the change goes into its document's journal on disk.
    pub fn journal(&self) -> crate::fs::ChangeJournal {
        let kept = Arc::clone(&self.kept);
        Arc::new(move |namespace: &Mutex<Namespace>| kept.journal(namespace))
    }

    /// Where the tree's parts are kept on disk, for a mount that saves them on its own
    /// schedule as well (on unmount, say).
    #[must_use]
    pub fn kept(&self) -> Arc<KeptDocuments> {
        Arc::clone(&self.kept)
    }

    /// What makes the journals [`Self::journal`] adds to durable, for the mount's barrier: it
    /// is called after the content they name is synced.
    pub fn sync_journal(&self) -> crate::fs::DurabilityHook {
        let kept = Arc::clone(&self.kept);
        Arc::new(move || kept.sync())
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.status.lock_recover().clone()
    }

    /// Where the sync stands each time that changes.
    #[must_use]
    pub fn status_changes(&self) -> tokio::sync::watch::Receiver<Status> {
        self.status.changed.subscribe()
    }

    /// Wait until every change this tree holds was handed to the peer, at most `within`: what a
    /// put into the tree answers once the workspace's records carry it.
    ///
    /// The state is read twice, a look apart, so a state written before the last change of this
    /// namespace was read again: the sync's own pass has seen the change by then.
    ///
    /// # Errors
    /// The changes did not all go out within `within`.
    pub async fn wait_sent(&self, within: Duration) -> Result<(), String> {
        let started = Instant::now();
        let mut seen = 0_u8;
        loop {
            if self.status.lock_recover().sent {
                seen = seen.saturating_add(1);
                if seen >= 2 {
                    return Ok(());
                }
                tokio::time::sleep(SETTLED_AGAIN).await;
            } else {
                seen = 0;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            if started.elapsed() >= within {
                return Err(format!(
                    "the workspace's tree has not taken the change within {}s",
                    within.as_secs()
                ));
            }
        }
    }

    /// Stop syncing (changes made here wait to go out, and none come in), or start again.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        // Said at once rather than at the next pass: the pass may wait for a step over the tree.
        let mut status = self.status.lock_recover();
        status.paused = paused;
        status.state = status.folder_state();
        drop(status);
        self.status.publish();
        self.wake.notify_waiters();
    }
}

/// Whether the host's `answer` about a record of the tree (the frames it sent of it, `None`
/// while it owes one) says the workspace has none of it.
fn holds_none(answer: Option<DocumentAnswer>) -> bool {
    answer == Some(DocumentAnswer::Sent { frames: 0 })
}

/// Whether the server's `answer` about a document of the tree says the workspace holds some of it.
fn holds_some(answer: Option<DocumentAnswer>) -> bool {
    matches!(answer, Some(DocumentAnswer::Sent { frames }) if frames > 0)
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

/// Read each document `due`: what the peer holds of it past the heads this namespace merged.
fn read_due(documents: &dyn Records, workspace: &str, due: Vec<DueRead>) -> Vec<DueReading> {
    due.into_iter()
        .map(|(chunk, merged)| {
            let reading = documents.read(
                workspace,
                FILES_APP,
                &document_for(chunk),
                merged.as_deref(),
            );
            (chunk, reading)
        })
        .collect()
}

/// A document of the tree read: its chunk, and the read.
type DueReading = (TreeChunk, Result<Reading, String>);

/// A document of the tree due a read: its chunk and the heads this namespace merged of it last.
type DueRead = (TreeChunk, Option<Vec<[u8; 32]>>);

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

/// A document of the tree that does not read here: since when, and when it is asked again.
#[derive(Debug)]
struct AskedAgain {
    since: Instant,
    next: Instant,
    asked: u32,
}

impl AskedAgain {
    fn new(now: Instant) -> Self {
        Self {
            since: now,
            next: now.checked_add(ASKED_AGAIN_AFTER).unwrap_or(now),
            asked: 0,
        }
    }

    /// Whether it is asked again at `now`, setting when it is asked next if it is.
    fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.asked = self.asked.saturating_add(1);
        let wait = ASKED_AGAIN_AFTER
            .saturating_mul(2_u32.saturating_pow(self.asked.min(16)))
            .min(ASKED_AT_MOST_EVERY);
        self.next = now.checked_add(wait).unwrap_or(now);
        true
    }
}

/// What this device knows of one record of the tree at the workspace.
#[derive(Default)]
struct Held {
    /// The record's heads this namespace merged last; `None` before the first merge.
    merged: Option<Vec<[u8; 32]>>,
    /// The namespace's heads of this record the peer has: handed to it by this device, or
    /// merged from it, each head standing past every change it was built on. Every change after
    /// them is the next write.
    sent: Vec<automerge::ChangeHash>,
    /// The record exists at the workspace (merged from it, or founded here), so a write of it is
    /// a change.
    founded: bool,
    /// How many frames the host said it holds of the record when the session opened it; `None`
    /// until it answered. None at all: the workspace has no such record yet.
    answered: Option<DocumentAnswer>,
    /// The host refused a write of this record: none goes out before then, and the next one
    /// carries every change since what the host holds.
    refused: Refusals,
    /// The workspace's server holds the document: it said so when the document was opened, or
    /// every write of this device's to it is durable there. Until then nothing else of the tree
    /// goes out, since anything else may name it.
    durable: bool,
    /// Whether every write of this device's to the document is durable at the server, asked once
    /// this device wrote it while the server did not hold it.
    confirming: Option<Served>,
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
    /// When the last refusal came, and why the workspace refused.
    last: Option<(Instant, Refusal)>,
}

/// How long one write may take to reach the workspace before it counts as refused: a write
/// waits for the workspace's keys and for the device that serves it, and a link that stopped
/// answering would otherwise hold every pass, and with it every later write of the folder.
const WRITE_WITHIN: Duration = Duration::from_secs(60);

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

    /// When a write may go out, while that is after `now`.
    fn held_until(&self, now: Instant) -> Option<Instant> {
        self.until.filter(|until| *until > now)
    }
}

/// The task that keeps one namespace and its records converged.
struct Sync {
    workspace: String,
    read: Arc<dyn Records>,
    writer: Box<dyn TreeWriter + Send>,
    peer: Arc<dyn TreePeer>,
    content: Option<Arc<crate::ContentStore>>,
    wakes: tokio::sync::mpsc::UnboundedReceiver<Wake>,
    paused: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    passes: Arc<std::sync::atomic::AtomicU64>,
    refresh: Arc<AtomicBool>,
    whole: Arc<AtomicBool>,
    namespace: Arc<Mutex<Namespace>>,
    /// Every record of the tree this device opened, by chunk: the tree's own (0) first.
    documents: BTreeMap<TreeChunk, Held>,
    /// The records a write landed on since they were read: what the next pull reads, rather
    /// than every record of the tree.
    unread: BTreeSet<TreeChunk>,
    /// The records whose read failed, said once each.
    unreadable: BTreeSet<TreeChunk>,
    /// The documents the workspace holds (or has not answered for) that do not read here, and
    /// what changes of this device's wait for: each is asked of the workspace again, after a
    /// wait that doubles each time.
    again: BTreeMap<TreeChunk, AskedAgain>,
    /// The chunks the tree lists that neither this device nor the workspace holds, said once each.
    lacking: BTreeSet<TreeChunk>,
    /// The records whose changes waited to go out, said once each.
    waiting: BTreeSet<TreeChunk>,
    /// When the first record the host refused may write again, while one waits.
    held_until: Option<Instant>,
    /// When the oldest change not yet handed to the peer was made: what the next write waits
    /// [`BATCH`] after.
    dirty_since: Option<Instant>,
    /// The records the last pass wrote out without their changes reaching what this namespace
    /// holds: changes that did not go out, which [`Self::stuck`] names once they waited long.
    unsent: usize,
    /// Where each document is kept on disk, with its changes since.
    kept: Arc<KeptDocuments>,
    /// A record stood somewhere new at the workspace since its place was last kept on disk.
    records_changed: bool,
    /// The documents whose wait for another to reach the workspace was said, once each.
    ordered: BTreeSet<TreeChunk>,
    /// The chunks the documents of `ordered` wait for, as the last look found them.
    ordered_for: BTreeSet<TreeChunk>,
    /// How often the tree may be kept, since it costs what the tree costs.
    pacing: Paced,
    counting: Paced,
    /// The heads the files and bytes in `status` were counted at.
    counted_heads: Option<Vec<automerge::ChangeHash>>,
    status: Arc<Shown>,
    /// The files the tree names as last counted, each with the hash of its content and its size:
    /// what the bytes this computer holds are counted again from as content arrives, without
    /// walking the tree.
    files: Arc<Vec<FileBytes>>,
    /// When the bytes this computer holds were last counted.
    local_counted: Option<Instant>,
}

impl Sync {
    async fn run(mut self) {
        let wake = Arc::clone(&self.wake);
        let mut changes = self.namespace.lock_recover().subscribe_changes();
        let mut keep = tokio::time::interval(KEEP_EVERY);
        let held = self.namespace.lock_recover().records();
        for chunk in held {
            self.open(chunk);
        }
        // What the peer brought: on the first pass, and after it woke this task.
        let mut session_moved = true;
        loop {
            while let Ok(wake) = self.wakes.try_recv() {
                // The holders' word changes only what the folder says of itself.
                session_moved |= !matches!(wake, Wake::Holders);
                self.take(wake);
            }
            if self.stopping.load(Ordering::Relaxed) {
                // The holder is stopping: keep what this tree changed and end, rather than
                // carrying on into a peer that is going.
                break;
            }
            // A reader asked for the tree as the workspace has it now: every record the tree
            // lists is asked of the workspace again, which answers each that did not change in
            // a word.
            if self.refresh.swap(false, Ordering::Relaxed) {
                session_moved = true;
                let listed = self
                    .namespace_now()
                    .map(|namespace| namespace.listed_chunks());
                for chunk in listed.unwrap_or_default() {
                    self.unread.insert(chunk);
                }
            }
            if !self.paused.load(Ordering::Relaxed) {
                if session_moved || !self.unread.is_empty() {
                    self.pull_all().await;
                }
                self.ask_again();
                self.confirmations();
                self.push().await;
                self.confirm();
                self.peer.reask_content();
            }
            self.say_where_it_stands().await;
            // A change of this namespace's own goes out once its batch is due; the peer's news
            // reads what the records hold. A record the host refused waits its turn without the
            // task looking every moment. Changes whose batch is due but that wait for the
            // workspace (a chunk's word that it holds it, a refusal's wait) are looked at again
            // now and then: every pass takes the namespace, which every request of the mount
            // waits for. Nothing here reads a record on a clock: a write that changed one wakes
            // this task with the record it changed.
            self.passes.fetch_add(1, Ordering::Relaxed);
            let wait = self.dirty_since.map(|since| {
                let held = self.held_until.map_or(Duration::ZERO, |until| {
                    until.saturating_duration_since(Instant::now())
                });
                let batch = if self.whole.load(Ordering::Relaxed) {
                    Duration::ZERO
                } else {
                    BATCH.saturating_sub(since.elapsed())
                };
                let floor = if batch.is_zero() {
                    WAITING_LOOK
                } else {
                    Duration::from_millis(5)
                };
                batch.max(held).max(floor)
            });
            session_moved = false;
            let mut received: Option<Option<Wake>> = None;
            let mut keep_due = false;
            let mut wipe = false;
            tokio::select! {
                wake = self.wakes.recv() => received = Some(wake),
                moved = changes.changed() => if moved.is_err() { wipe = true },
                _ = keep.tick() => keep_due = true,
                () = wake.notified() => {}
                () = tokio::time::sleep(wait.unwrap_or(Duration::from_secs(86_400))) => {}
            }
            if keep_due && self.pacing.due(Instant::now()) {
                let started = Instant::now();
                self.keep().await;
                self.keep_records();
                self.pacing.ran(started, Instant::now());
            }
            if wipe {
                break;
            }
            match received {
                Some(Some(wake)) => {
                    session_moved = !matches!(wake, Wake::Holders);
                    self.take(wake);
                }
                // The peer is gone: nothing more will come.
                Some(None) => break,
                None => {}
            }
        }
        // What changed here is on disk before this task ends: the holder is going, and the
        // module that made it goes with the process.
        self.keep().await;
        self.keep_records();
    }

    /// Keep where each record of the tree stood at the workspace when this device last merged
    /// it, beside the documents: a restarted holder asks each record only what changed since,
    /// rather than reading everything the workspace holds of it again.
    fn keep_records(&mut self) {
        if !self.records_changed {
            return;
        }
        let records: BTreeMap<TreeChunk, Vec<[u8; 32]>> = self
            .documents
            .iter()
            .filter_map(|(chunk, held)| held.merged.as_ref().map(|heads| (*chunk, heads.clone())))
            .collect();
        match self.kept.keep_records(&records) {
            Ok(()) => self.records_changed = false,
            Err(error) => tracing::warn!(
                %error,
                "where the tree's records stand is not kept on disk; a restart reads them whole"
            ),
        }
    }

    /// Ask the workspace again for each document of the tree that does not read here and whose
    /// wait is over: a document the workspace sent this device but whose changes did not all
    /// arrive, or one it never answered for, would otherwise wait for a restart.
    fn ask_again(&mut self) {
        let now = Instant::now();
        let due: Vec<TreeChunk> = self
            .again
            .iter_mut()
            .filter_map(|(chunk, again)| again.due(now).then_some((*chunk, again.asked)))
            .map(|(chunk, asked)| {
                tracing::info!(chunk, asked, workspace = %self.workspace, "a document of the workspace's tree that does not read here is asked of the workspace again");
                chunk
            })
            .collect();
        for chunk in due {
            self.open(chunk);
        }
    }

    /// Why changes made here wait, once they waited for longer than [`STUCK_AFTER`]: for the
    /// workspace's word of whether it holds a part of the tree, for a part that does not read
    /// here, or for the parts a part names to reach the workspace first; none while they go out
    /// or have not waited that long.
    fn stuck(&self, opened: bool) -> Vec<Waiting> {
        let Some(since) = self.dirty_since else {
            return Vec::new();
        };
        let long = |since: Instant| since.elapsed() >= STUCK_AFTER;
        // A part waits long either for the workspace's first word of whether it holds it, or,
        // once it said it does, for the part itself to arrive here.
        let long_waits: Vec<&TreeChunk> = self
            .again
            .iter()
            .filter(|(_, again)| long(again.since))
            .map(|(chunk, _)| chunk)
            .collect();
        let unanswered = long_waits
            .iter()
            .filter(|chunk| {
                self.documents
                    .get(**chunk)
                    .is_some_and(|held| held.answered.is_none())
            })
            .count();
        let waiting = long_waits.len().saturating_sub(unanswered);
        let lacking = self.lacking.len();
        let ordered = if long(since) { self.ordered.len() } else { 0 };
        // A change the workspace keeps refusing goes again after each wait, and says why.
        let refused: Vec<&Refusal> = if long(since) {
            self.documents
                .values()
                .filter_map(|held| held.refused.last.as_ref())
                .filter(|(at, _)| at.elapsed() <= MOST_REFUSED_WAIT.saturating_mul(2))
                .map(|(_, reason)| reason)
                .collect()
        } else {
            Vec::new()
        };
        let unsent = if long(since) { self.unsent } else { 0 };
        let mut why = Vec::new();
        if !opened {
            why.push(Waiting::Unopened);
        }
        if unanswered > 0 {
            why.push(Waiting::Unanswered);
        }
        if waiting > 0 {
            why.push(Waiting::Unarrived);
        }
        if ordered > 0 {
            why.push(Waiting::Ordered);
        }
        if let Some(reason) = refused.first() {
            why.push(Waiting::Refused((*reason).clone()));
        }
        if unsent > 0 {
            why.push(Waiting::Unsent);
        }
        if lacking > 0 {
            why.push(Waiting::Lacking);
        }
        why
    }

    /// Ask the peer for record `chunk` of the tree, and to take part in its channel.
    fn open(&mut self, chunk: TreeChunk) {
        self.documents.entry(chunk).or_default();
        self.unread.insert(chunk);
        self.peer.open(chunk);
    }

    fn take(&mut self, wake: Wake) {
        match wake {
            // A write that named its record: that record alone may have changed.
            Wake::Wrote(chunk) => {
                if self.documents.contains_key(&chunk) {
                    self.unread.insert(chunk);
                } else {
                    // A record this device has not opened yet is one the tree now names: it is
                    // asked for, as the tree's own record naming a new chunk is.
                    self.open(chunk);
                }
            }
            // A change whose record was not named (the follower fell behind its writes): any
            // record may have changed, so each already opened is read.
            Wake::Moved => {
                self.unread.extend(self.documents.keys().copied());
            }
            Wake::HostReady => {
                let opened: Vec<TreeChunk> = self.documents.keys().copied().collect();
                for chunk in opened {
                    self.open(chunk);
                }
            }
            // The host did not take a write: its changes are still this namespace's, and go out
            // again, with all made since, once the record's wait is over. What the host holds
            // is what was last merged from it.
            Wake::Refused { chunk, reason } => self.refused(chunk, &reason),
            // The host's answer to this session's open of a record of the tree: whether the
            // workspace holds any of it decides whether this device founds it.
            Wake::Opened { chunk, answer } => {
                let held = self.documents.entry(chunk).or_default();
                held.answered = Some(answer);
                held.durable |= holds_some(Some(answer));
                // A host that sent some of the tree holds one, which this device reads
                // before it writes: a tree it founded beside that one would go out whole.
                if chunk == 0 && holds_none(Some(answer)) {
                    self.status.lock_recover().opened = true;
                }
            }
            Wake::Holders => {}
        }
    }

    /// The host did not take this device's writes to `chunk`, for `reason`: their changes are
    /// still this namespace's, and go out again, with all made since, once the record's wait is
    /// over. What the host holds is what was last merged from it.
    fn refused(&mut self, chunk: TreeChunk, reason: &Refusal) {
        let held = self.documents.entry(chunk).or_default();
        held.sent = held
            .merged
            .iter()
            .flatten()
            .map(|hash| automerge::ChangeHash(*hash))
            .collect();
        let now = Instant::now();
        held.refused.last = Some((now, reason.clone()));
        if held.refused.refused(now) {
            tracing::warn!(%reason, workspace = %self.workspace, chunk, "the workspace refused a change to its files; it goes again after a wait");
        }
    }

    /// Write `changes` to record `chunk` through the writer. A write that did not end within
    /// [`WRITE_WITHIN`] is refused as one the workspace refused: it goes again after the
    /// record's wait, and the folder says why its changes wait.
    async fn write(&mut self, chunk: TreeChunk, changes: &[u8]) -> Result<(), Refusal> {
        let writing = self.writer.write(chunk, changes);
        if let Ok(written) = tokio::time::timeout(WRITE_WITHIN, writing).await {
            return written;
        }
        self.refused(chunk, &Refusal::Unanswered);
        Err(Refusal::Unanswered)
    }

    /// Merge what each opened record holds that this namespace has not merged yet, and open
    /// the chunks the tree's own record lists that this device has not opened.
    async fn pull_all(&mut self) {
        // Opening a sealed history takes as long as it is: named while it runs, so a folder
        // that waits for it says so in a stall report.
        let _doing = tilo_rt::stall::doing("a folder's tree coming in");
        let documents = Arc::clone(&self.read);
        let workspace = self.workspace.clone();
        let due = self.due_reads();
        if due.is_empty() {
            return;
        }
        // The reads happen away from the runtime's workers: opening a sealed history takes as long
        // as it is, and a worker inside it leaves the tasks it would take up next (the session's) waiting.
        let read =
            tokio::task::spawn_blocking(move || read_due(&*documents, &workspace, due)).await;
        let Ok(read) = read else {
            tracing::warn!(workspace = %self.workspace, "the workspace's tree could not be read");
            return;
        };
        let mut readings = Vec::new();
        for (chunk, reading) in read {
            match reading {
                Ok(Reading::Whole { heads, bytes } | Reading::Changes { heads, bytes }) => {
                    if self.unreadable.remove(&chunk) {
                        tracing::info!(chunk, workspace = %self.workspace, "a document of the workspace's tree reads here now");
                    }
                    self.again.remove(&chunk);
                    readings.push((chunk, heads, bytes));
                }
                Ok(Reading::Unchanged) => {
                    // The record stands at what this device holds of it, and the workspace holds
                    // it: nothing is merged, and it is open all the same. A tree that only ever
                    // reads unchanged (the one this computer wrote before it stopped) is open,
                    // and its changes go out — nothing else would ever mark it.
                    let held = self.documents.entry(chunk).or_default();
                    held.founded = true;
                    if chunk == 0 {
                        self.status.lock_recover().opened = true;
                    }
                    self.again.remove(&chunk);
                }
                Ok(Reading::Absent | Reading::Pending) => self.not_read(chunk, None),
                Err(error) => self.not_read(chunk, Some(&error)),
            }
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
            //
            // The heads the workspace's copy answered are what it holds of this record now, and
            // a head says it holds every change that head was built on. A head this device last
            // had from the copy, or kept from an earlier run, may be one of those changes now
            // rather than a head: it goes, or the folder says it is still sending changes the
            // copy has, forever, since a write has nothing to carry and so never answers new
            // heads.
            held.sent = heads.iter().copied().map(automerge::ChangeHash).collect();
            if held.merged.as_ref() != Some(&heads) {
                held.merged = Some(heads);
                self.records_changed = true;
            }
            held.founded = true;
        }
        for chunk in listed {
            if !self.documents.contains_key(&chunk) {
                self.open(chunk);
            }
        }
    }

    /// The documents of the tree to read now, each with the heads this namespace merged of it
    /// last: those a write was told of since they were read.
    fn due_reads(&mut self) -> Vec<DueRead> {
        std::mem::take(&mut self.unread)
            .into_iter()
            .map(|chunk| {
                let held = self.documents.get(&chunk);
                (chunk, held.and_then(|held| held.merged.clone()))
            })
            .collect()
    }

    /// Document `chunk` of the tree did not read here, with `error` when its read failed.
    fn not_read(&mut self, chunk: TreeChunk, error: Option<&str>) {
        // A document the workspace has not answered for yet is one still on its way (a device
        // starting reads before its keys and the documents arrive): only one the workspace said
        // it holds some of and that still does not read is said, once.
        let answered = self.documents.get(&chunk).and_then(|held| held.answered);
        if let Some(error) = error
            && holds_some(answered)
        {
            self.again
                .entry(chunk)
                .or_insert_with(|| AskedAgain::new(Instant::now()));
            if self.unreadable.insert(chunk) {
                tracing::warn!(%error, chunk, workspace = %self.workspace, "a document of the workspace's tree cannot be read here; it is asked of the workspace again");
            }
        }
        // Nothing here yet: once the host answered with none, the workspace has no such
        // document, and this device's first change founds it.
        if chunk == 0 && holds_none(self.peer.answer(chunk)) {
            self.status.lock_recover().opened = true;
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

    /// Hand the peer what the namespace changed since it last did, once the batch the first of
    /// those changes started is due: one write per record that changed.
    async fn push(&mut self) {
        // A write of a folder's tree that takes seconds names itself, with what else holds
        // the process, for as long as it takes.
        let _doing = tilo_rt::stall::doing("a folder's tree going out");
        if !self.status.lock_recover().opened {
            // The host has not said what it holds: a tree written now could found a second
            // one beside it. What changed here waits for that answer, and [`Self::stuck`] says
            // so rather than the folder going quiet with its changes held.
            self.dirty_since.get_or_insert_with(Instant::now);
            return;
        }
        let (heads, listed) = {
            let Some(mut namespace) = self.namespace_now() else {
                return;
            };
            (namespace.document_heads(), namespace.listed_chunks())
        };
        let dirty: Vec<TreeChunk> = heads
            .iter()
            .filter(|(chunk, heads)| {
                self.documents.get(chunk).map(|held| &held.sent) != Some(*heads)
            })
            .map(|(chunk, _)| *chunk)
            .collect();
        // A change made whole at once ([`TreeSync::change`]) is the whole burst already.
        let whole = self.whole.swap(false, Ordering::Relaxed);
        if dirty.is_empty() {
            self.dirty_since = None;
            self.unsent = 0;
            return;
        }
        let mut since = *self.dirty_since.get_or_insert_with(Instant::now);
        if whole && let Some(due) = Instant::now().checked_sub(BATCH) {
            // Its batch is due now, and stays due while its records wait for the workspace.
            since = since.min(due);
            self.dirty_since = Some(since);
        }
        if since.elapsed() < BATCH {
            return;
        }
        // Every chunk the tree's own document lists, whether or not this namespace holds a
        // document of it: one listed but held nowhere here (its nodes were never journaled, or
        // it came from another device) is one the tree must not go out naming until the
        // workspace says it holds it.
        let held_here: BTreeSet<TreeChunk> = heads.keys().copied().collect();
        let mut held_now = held_here.clone();
        held_now.extend(listed);
        let (due, mut waiting) = self.due(dirty, held_now, &held_here);
        if due.is_empty() {
            self.dirty_since = waiting.then_some(since);
            self.unsent = 0;
            return;
        }
        // Reading the changes a chunk holds beyond what the peer has walks that chunk's
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
        let (mut more, mut unsent) = (false, 0_usize);
        for write in writes {
            // What the write carries, not what the namespace holds now: the mount keeps
            // changing it while the write goes out, and a change made meanwhile counted as
            // sent would never reach the host, which then leaves out every later write of
            // this device.
            if !write.changes.is_empty() && self.write(write.chunk, &write.changes).await.is_err() {
                waiting = true;
                continue;
            }
            // A record that changed more than one write carries goes on with the next pass; one
            // whose write carried nothing and still stands apart from the namespace did not go
            // out, and the folder says so rather than counting it sent.
            if heads.get(&write.chunk) != Some(&write.heads) {
                more = true;
                if write.changes.is_empty() {
                    unsent = unsent.saturating_add(1);
                }
            }
            if let Some(held) = self.documents.get_mut(&write.chunk) {
                // A write that carried changes founded the record where the workspace had none.
                held.founded |= !write.changes.is_empty();
                held.sent = write.heads;
            }
        }
        self.unsent = unsent;
        self.dirty_since = (waiting || more).then_some(since);
    }

    /// The chunks of `dirty` that can go out now, with the heads each was last handed to the
    /// peer at, and whether any waits instead: whether it is founded here or waits for the
    /// workspace's answer.
    fn due(
        &mut self,
        dirty: Vec<TreeChunk>,
        held_now: BTreeSet<TreeChunk>,
        held_here: &BTreeSet<TreeChunk>,
    ) -> (Vec<(TreeChunk, Vec<automerge::ChangeHash>)>, bool) {
        let mut waiting = false;
        let now = Instant::now();
        self.held_until = None;
        let mut due = Vec::new();
        // The chunks the workspace's server does not hold yet go out first, and alone: every
        // other document of the tree waits until the server holds them, since it may name them.
        let unheld = unheld_chunks(&self.documents, held_now);
        self.ask_after(&unheld, held_here);
        self.ordered_for.clone_from(&unheld);
        self.ordered.retain(|chunk| dirty.contains(chunk));
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
            let founded = self.documents.get(&chunk).is_some_and(|held| held.founded);
            if !founded {
                // A record this device starts: the host says first whether the workspace has
                // it. One it has is merged before this device's changes of it go out; one it has
                // not is founded here, by the changes that carry its canonical founding first.
                // Its changes are saved only once they can go out: saving walks the record.
                let answered = self.documents.get(&chunk).and_then(|held| held.answered);
                if !holds_none(answered) || self.peer.holds(chunk) {
                    // Said once per record: a write that waits here for long is one the
                    // folder says it is still sending.
                    if self.waiting.insert(chunk) {
                        tracing::info!(chunk, ?answered, workspace = %self.workspace, "a record of the workspace's tree waits before this device's changes of it go out");
                    }
                    self.again
                        .entry(chunk)
                        .or_insert_with(|| AskedAgain::new(now));
                    waiting = true;
                    continue;
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

    /// Open each chunk of `unheld` that nothing asked the peer about yet, so its answer comes;
    /// and say once of each chunk the tree lists that neither this device (none of `held_here`)
    /// nor the workspace holds: the tree's changes wait for it rather than go out naming a chunk
    /// the workspace lacks.
    fn ask_after(&mut self, unheld: &BTreeSet<TreeChunk>, held_here: &BTreeSet<TreeChunk>) {
        for chunk in unheld {
            if !self.documents.contains_key(chunk) {
                self.open(*chunk);
            }
            let answered = self.documents.get(chunk).and_then(|held| held.answered);
            if holds_none(answered) && !held_here.contains(chunk) && self.lacking.insert(*chunk) {
                tracing::error!(
                    chunk,
                    workspace = %self.workspace,
                    "the workspace's tree lists a chunk that neither this device nor the \
                     workspace holds; its changes wait rather than go out naming it"
                );
            }
        }
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
            if let Some(served) = self.writer.served(&[chunk])
                && let Some(held) = self.documents.get_mut(&chunk)
            {
                held.confirming = Some(served);
            }
        }
    }

    /// Take the server's answers to [`Self::confirm`]'s asks: a document all of whose writes are
    /// durable there is held by the workspace. One the server refused, or did not answer in
    /// time, is not: its changes go out again after the record's wait, as a refused write's do,
    /// and it is asked again once they went. Until then nothing that may name it goes out.
    fn confirmations(&mut self) {
        let mut refused = Vec::new();
        for (chunk, held) in &mut self.documents {
            let answer = held.confirming.as_mut().and_then(answered);
            match answer {
                Some(Ok(())) => {
                    held.durable = true;
                    held.confirming = None;
                }
                Some(Err(refusal)) => {
                    held.confirming = None;
                    refused.push((*chunk, refusal));
                }
                None => {}
            }
        }
        for (chunk, refusal) in refused {
            self.refused(chunk, &refusal);
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
            // Counting the files walks every node of the tree, and the bytes this computer holds
            // are asked of the content store a file at a time: off the runtime's workers, like a
            // merge and a keep, so a client's status is answered meanwhile.
            let namespace = Arc::clone(&self.namespace);
            let store = self.content.clone();
            let started = Instant::now();
            match tokio::task::spawn_blocking(move || {
                let namespace = namespace.lock_recover();
                let named = files_of(&namespace);
                drop(namespace);
                let local = local_bytes(store.as_deref(), &named);
                (Arc::new(named), local)
            })
            .await
            {
                Ok((named, local)) => {
                    self.counting.ran(started, Instant::now());
                    self.counted_heads = Some(heads);
                    self.local_counted = Some(Instant::now());
                    self.files = named;
                    Some((self.files.len(), total_bytes(&self.files), local))
                }
                Err(error) => {
                    tracing::warn!(%error, "counting the workspace's files could not be done");
                    None
                }
            }
        } else {
            self.count_local_again().await
        };
        let sent = records.iter().all(|(chunk, heads)| {
            self.documents
                .get(chunk)
                .is_some_and(|held| held.sent == *heads)
        });
        let durable = !self.documents.keys().any(|chunk| self.peer.holding(*chunk))
            && self.peer.pending() == 0;
        let mut status = self.status.lock_recover();
        let opened = status.opened;
        if let Some((files, bytes, local)) = counted {
            status.files = files;
            status.bytes = bytes;
            status.local_bytes = local;
        }
        status.paused = self.paused.load(Ordering::Relaxed);
        status.sent = sent;
        status.durable = durable;
        status.waiting_for_workspace = self.peer.holds_workspace() == Some(false);
        status.stuck = self.stuck(opened);
        status.host_lacks = self.peer.host_lacks();
        status.state = status.folder_state();
        drop(status);
        self.status.publish();
    }

    /// The files, bytes and local bytes as last counted, with the local bytes counted again when
    /// some of the files' content was not here at the last count and [`LOCAL_AGAIN`] went by.
    async fn count_local_again(&mut self) -> Option<(usize, u64, u64)> {
        let (bytes, local) = {
            let status = self.status.lock_recover();
            (status.bytes, status.local_bytes)
        };
        if local >= bytes
            || self
                .local_counted
                .is_some_and(|counted| counted.elapsed() < LOCAL_AGAIN)
        {
            return None;
        }
        let (store, files) = (self.content.clone(), Arc::clone(&self.files));
        let local = tokio::task::spawn_blocking(move || local_bytes(store.as_deref(), &files))
            .await
            .ok()?;
        self.local_counted = Some(Instant::now());
        Some((self.files.len(), bytes, local))
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

/// What a [`Served`] said when it answered, or nothing while it has not.
fn answered(served: &mut Served) -> Option<Result<(), Refusal>> {
    futures_util::FutureExt::now_or_never(served.as_mut())
}

/// The files `namespace` names, each with the hash of its content (`None` for a text document
/// whose content the tree holds itself) and its size: a walk of every node.
fn files_of(namespace: &Namespace) -> Vec<FileBytes> {
    let mut files = Vec::new();
    for id in namespace.live_node_ids() {
        // A text document (a `.md`, an `.odt`) is a file whose content the tree holds itself,
        // by no hash: a file all the same.
        if let Some(entry) = namespace.entry_for_node(&id)
            && entry.kind == crate::namespace::NodeKind::File
        {
            files.push((entry.hash, entry.size));
        }
    }
    files
}

/// The bytes of `files` the content store holds: a text document the tree holds itself is here
/// whenever the tree is.
fn local_bytes(store: Option<&crate::ContentStore>, files: &[FileBytes]) -> u64 {
    files
        .iter()
        .filter(|(hash, _)| hash.is_none_or(|hash| store.is_some_and(|store| store.holds(hash))))
        .fold(0_u64, |bytes, (_, size)| bytes.saturating_add(*size))
}

fn total_bytes(files: &[FileBytes]) -> u64 {
    files
        .iter()
        .fold(0_u64, |bytes, (_, size)| bytes.saturating_add(*size))
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

#[cfg(test)]
mod tests {

    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use tilo_server_protocol::DocumentAnswer;

    use super::*;
    use crate::tree_sync::FileToPut;

    /// A document of the tree that does not read here is asked of the workspace again after a
    /// short wait, then less and less often, but never less often than every few minutes: a
    /// document whose changes did not all arrive is fetched again rather than waited for until a
    /// restart.
    #[test]
    fn a_record_that_does_not_read_here_is_asked_for_again_ever_less_often() {
        let start = Instant::now();
        let mut again = AskedAgain::new(start);
        assert!(!again.due(start), "not at once");
        let mut at = start + ASKED_AGAIN_AFTER;
        let mut waits = Vec::new();
        for _ in 0..12 {
            assert!(again.due(at), "asked again once its wait is over");
            assert!(!again.due(at), "and not twice for the same wait");
            waits.push(again.next - at);
            at = again.next;
        }
        assert_eq!(waits[0], ASKED_AGAIN_AFTER * 2);
        assert!(waits.windows(2).all(|pair| pair[0] <= pair[1]), "{waits:?}");
        assert_eq!(*waits.last().unwrap(), ASKED_AT_MOST_EVERY);
    }

    #[test]
    fn a_refused_record_waits_longer_each_time() {
        let start = Instant::now();
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
    }

    #[test]
    fn a_tree_the_host_sent_but_that_does_not_read_here_is_not_founded_again() {
        // The host owes its answer still: nothing is known.
        assert!(!holds_none(None));
        // It sent frames of the tree: the workspace holds one, though it may not read here
        // yet (a write still on its way), and a tree founded now would go out whole beside it.
        assert!(!holds_none(Some(DocumentAnswer::Sent { frames: 3 })));
        // It sent none: the workspace has no tree, and this device's first change founds it.
        assert!(holds_none(Some(DocumentAnswer::Sent { frames: 0 })));
        // It is another person's own: nothing of it is this device's to found.
        assert!(!holds_none(Some(DocumentAnswer::NotYours)));
    }

    #[test]
    fn a_step_that_walks_the_tree_waits_twenty_times_its_cost_and_at_least_its_floor() {
        let mut paced = Paced::new(Duration::from_secs(1));
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
            .create_file(crate::namespace::ROOT_INO, "handbook.md.txt", None, 1, 1)
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
                crate::namespace::ROOT_INO,
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
                host.lookup(crate::namespace::ROOT_INO, name).is_some(),
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
            .create_file(crate::namespace::ROOT_INO, "a.txt", None, 1, 1)
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
        let (mut read, _) =
            open_namespace(&MountAuthor::device("laptop"), &kept, documents).unwrap();
        assert!(read.lookup(crate::namespace::ROOT_INO, "a.txt").is_some());
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

    /// A workspace with nothing yet, which answers each record the sync opens as one it holds
    /// none of.
    struct Empty(tokio::sync::mpsc::UnboundedSender<Wake>);

    impl Records for Empty {
        fn read(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[[u8; 32]]>,
        ) -> Result<Reading, String> {
            Ok(Reading::Absent)
        }
    }

    impl TreePeer for Empty {
        fn open(&self, chunk: TreeChunk) {
            let answer = DocumentAnswer::Sent { frames: 0 };
            let _ = self.0.send(Wake::Opened { chunk, answer });
        }
        fn answer(&self, _: TreeChunk) -> Option<DocumentAnswer> {
            Some(DocumentAnswer::Sent { frames: 0 })
        }
        fn holds(&self, _: TreeChunk) -> bool {
            false
        }
        fn holding(&self, _: TreeChunk) -> bool {
            false
        }
        fn pending(&self) -> usize {
            0
        }
        fn reask_content(&self) {}
    }

    /// A writer whose first write never ends, as one over a link that stopped answering, and
    /// whose later writes are taken.
    struct Unanswered(Arc<std::sync::atomic::AtomicUsize>);

    impl TreeWriter for Unanswered {
        fn write<'a>(&'a mut self, _: TreeChunk, _: &'a [u8]) -> WriteFuture<'a> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Box::pin(std::future::pending())
            } else {
                Box::pin(std::future::ready(Ok(())))
            }
        }
    }

    /// A peer that never answers an open: the workspace's tree never reads here.
    struct Silent;

    impl Records for Silent {
        fn read(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&[[u8; 32]]>,
        ) -> Result<Reading, String> {
            Ok(Reading::Pending)
        }
    }

    impl TreePeer for Silent {
        fn open(&self, _: TreeChunk) {}
        fn answer(&self, _: TreeChunk) -> Option<DocumentAnswer> {
            None
        }
        fn holds(&self, _: TreeChunk) -> bool {
            false
        }
        fn holding(&self, _: TreeChunk) -> bool {
            false
        }
        fn pending(&self) -> usize {
            0
        }
        fn reask_content(&self) {}
    }

    /// A put waits for the workspace's tree to be read here, and changes nothing until it is: a
    /// change made first would land on a tree founded here beside the workspace's own.
    #[tokio::test(start_paused = true)]
    async fn a_put_waits_for_the_workspace_tree_and_changes_nothing_before_it() {
        let dir = tempfile::tempdir().unwrap();
        let (_tell, wakes) = tokio::sync::mpsc::unbounded_channel();
        let workspace = Arc::new(Silent);
        let sync = TreeSync::start(Config {
            workspace: "studio".to_owned(),
            device: [1; 32],
            person: [2; 32],
            kept_at: dir.path().join("tree"),
            documents: Arc::clone(&workspace) as Arc<dyn Records>,
            writer: Box::new(Unanswered(Arc::new(std::sync::atomic::AtomicUsize::new(0)))),
            peer: workspace,
            content: None,
            wakes,
        })
        .await
        .unwrap();
        let hash = hex::encode([3; 32]);
        let files = [FileToPut {
            to: "",
            name: "a.txt",
            hash: &hash,
            size: 1,
            mtime: 1,
        }];
        let Err(said) = sync.change(None, |tree| tree.put_files(&files)).await else {
            panic!("the put was made before the workspace's tree was read");
        };
        assert!(said.contains("has not been read here"), "{said}");
        assert!(
            sync.namespace.lock_recover().entry_at("a.txt").is_none(),
            "nothing was changed before the workspace's tree was read"
        );
    }

    /// A write that never ends holds the folder no longer than its bound: it counts as refused,
    /// and the change goes again after the refusal's wait and is taken.
    #[tokio::test(start_paused = true)]
    async fn a_write_that_never_ends_is_refused_and_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let (tell, wakes) = tokio::sync::mpsc::unbounded_channel();
        let workspace = Arc::new(Empty(tell));
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sync = TreeSync::start(Config {
            workspace: "studio".to_owned(),
            device: [1; 32],
            person: [2; 32],
            kept_at: dir.path().join("tree"),
            documents: Arc::clone(&workspace) as Arc<dyn Records>,
            writer: Box::new(Unanswered(Arc::clone(&writes))),
            peer: workspace,
            content: Some(Arc::new(
                crate::ContentStore::open(dir.path().join("blobs"))
                    .await
                    .unwrap(),
            )),
            wakes,
        })
        .await
        .unwrap();
        sync.namespace
            .lock_recover()
            .create_file(crate::namespace::ROOT_INO, "a.txt", None, 1, 1)
            .unwrap();
        // The refusal's wait is read on the wall clock, the write's bound on the runtime's.
        let began = std::time::Instant::now();
        while writes.load(Ordering::SeqCst) < 2 {
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "the folder never wrote again after a write that did not end: {:?}",
                sync.status()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A record this device sent all of, then merged as the workspace held it before (a holder
    /// restarted after a kill reads the records it kept), counts the workspace's older heads as
    /// sent beside its own: the next pass finds nothing more to send and counts the record sent,
    /// rather than holding it as unsent for good with every change in place.
    #[test]
    fn a_record_merged_as_the_workspace_held_it_before_counts_as_sent() {
        let mut laptop = Namespace::new("laptop").unwrap();
        laptop
            .create_file(crate::namespace::ROOT_INO, "a.txt", None, 1, 1)
            .unwrap();
        // What a write reads out is what the namespace stands at.
        laptop.document_writes(&BTreeMap::new());
        let older = laptop.document_heads();
        laptop
            .create_file(crate::namespace::ROOT_INO, "b.txt", None, 1, 1)
            .unwrap();
        laptop.document_writes(&BTreeMap::new());
        let now = laptop.document_heads();
        assert!(
            now.iter()
                .any(|(chunk, heads)| older.get(chunk) != Some(heads)),
            "a record moved on"
        );
        let mut documents: BTreeMap<TreeChunk, Held> = BTreeMap::new();
        for (chunk, heads) in &now {
            let held = documents.entry(*chunk).or_default();
            held.sent.clone_from(heads);
            held.founded = true;
            held.durable = true;
            // The merge adds the heads the record stood at to what counts as sent.
            for merged in older.get(chunk).into_iter().flatten() {
                if !held.sent.contains(merged) {
                    held.sent.push(*merged);
                }
            }
        }
        let (mut server, mut confirmed) = (Server::new(), Vec::new());
        pass(&mut laptop, &mut documents, &mut server, &mut confirmed);
        for (chunk, heads) in &now {
            assert_eq!(
                documents.get(chunk).map(|held| &held.sent),
                Some(heads),
                "record {chunk} still counts as not sent"
            );
        }
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
                        crate::namespace::ROOT_INO,
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
            .create_file(crate::namespace::ROOT_INO, "a.txt", None, 1, 1)
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
    fn a_server_that_sent_frames_of_a_document_holds_it() {
        assert!(!holds_some(None));
        assert!(!holds_some(Some(DocumentAnswer::Sent { frames: 0 })));
        assert!(holds_some(Some(DocumentAnswer::Sent { frames: 2 })));
        assert!(!holds_some(Some(DocumentAnswer::NotYours)));
    }

    /// The files the tree names, each with the hash of its content and its size, are what
    /// `tilo status` counts: a text document the tree holds itself has no hash.
    #[test]
    fn the_files_the_tree_names_are_counted_with_their_sizes() {
        let mut namespace = Namespace::new("laptop").unwrap();
        namespace
            .create_file(crate::namespace::ROOT_INO, "a.txt", None, 7, 1)
            .unwrap();
        namespace
            .create_dir(crate::namespace::ROOT_INO, "folder", 1)
            .unwrap();
        assert_eq!(files_of(&namespace), vec![(None, 7)]);
    }
}
