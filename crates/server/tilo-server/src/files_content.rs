//! The files of the workspaces this host governs, kept so that a member which
//! comes up later reads them from this host alone.
//!
//! A workspace's file truth is its Files app record: the tree as several
//! Automerge documents — the tree's own record, which names its chunks, and
//! the chunks, which hold the nodes ([`fs_protocol::tree`]) — whose nodes name
//! each file's content by hash and never carry the bytes
//! ([`fs::tree_document`]), and the bytes themselves, which travel beside the
//! records as their content (the broker's document content routes) between
//! the members the host carries them for. A member keeps the files it wrote
//! and answers whoever asks; a member that leaves — a VM host that was
//! suspended, evicted or replaced, a laptop that went offline — takes the only
//! copy with it, and nothing that joins afterwards has any to hydrate from.
//!
//! So the host keeps them. It reads each governed workspace's tree record by
//! record, the records it already holds and opens, sealed or not
//! ([`Workspaces::resource_document`]), and each as it changes: a write names
//! one record of the tree, and only that one is read again, so a folder of
//! twenty thousand files costs the chunk a write touched and not the folder.
//! For every file the chunks it read name it asks
//! the workspace's members for the content it does not keep yet, on its own
//! presence, as a member asks ([`Workspaces::ask_members`]). A file's content
//! is its manifest and the chunks that manifest lists, so it asks for the
//! manifest first — which is how a file over a megabyte, kept as chunks in the
//! store's file-backed area, arrives as well — and for whatever the manifest
//! lists once it is here. Each blob is kept only when its bytes hash to the
//! hash it was asked by ([`ContentWants`]).
//!
//! A tree whose record the host cannot open yet — a workspace's channels hand
//! a device their keys after it joins them, and a record founded on one is
//! sealed from its first write — is read again in a moment: quickly at first,
//! and further apart the longer it stays shut. A host that kept only the files
//! of the trees it happened to read at the right moment would keep none of a
//! workspace's files. Once the host holds the channel's keys there is nothing
//! to wait for: a tree nobody wrote yet is read when its first write lands, so
//! a workspace whose members never touched its files costs nothing while it
//! sits there, however many of them the host governs.
//!
//! A change to one workspace's governance reads that workspace's tree alone:
//! making a person's next workspace costs the same whether they have one or
//! hundreds.
//!
//! What it keeps goes into the workspace's content store on this host
//! ([`crate::content`], beside the workspace's uploads, what its apps made and
//! what its bots fetched), from where the host answers members asking their
//! workspace for content ([`Workspaces::content_answer`]): a member that joins
//! after the writer left hydrates the same bytes from this host.
//!
//! # What it keeps
//!
//! Everything the governed workspaces' current trees reference: each file's
//! manifest and its chunks, however much that is. What bounds it is the disk
//! the store is on: while that runs low the store refuses new content and
//! says why ([`tilo_content::ContentError::DiskLow`]), and the host asks
//! again later, so a file it could not keep then stays with the members that
//! wrote it until there is workspace. A blob of a file dropped from a tree stays
//! in the store until nothing in the workspace references it any more.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs::content::Manifest;
use fs_protocol::files::{FileNodeKind, TreeChunk};
use fs_protocol::tree::{self, TRASH_ID, TreeChunks};
use neighbour_wire::content::ContentWants;
use tilo_lock::MutexRecover as _;

use crate::workspaces::{ContentKeeper, ResourceWrite, Workspaces};
use tilo_content::WorkspaceStores;

/// The app whose record a workspace's tree is.
const FILES_APP: &str = fs_protocol::tree::FILES_APP;
/// How often the records of workspaces written to since the last pass are
/// read: a workspace takes many writes a second, and one pass sees their files.
const READ_EVERY: Duration = Duration::from_secs(2);
/// How long the host waits before it asks again for content nobody answered.
const ASK_AGAIN: Duration = Duration::from_secs(90);
/// How often the content being gathered is looked at for the pieces a lost
/// one left missing, so a member's answer arrives whole without waiting for
/// the next write to the tree that named it.
const MISSING_EVERY: Duration = Duration::from_millis(100);
/// How long a tree that could not be read waits before it is read again, at
/// first: each failed read waits twice as long, up to [`RETRY_LONGEST`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
/// The longest wait between two reads of a tree that does not open.
const RETRY_LONGEST: Duration = Duration::from_secs(60);

/// When a tree the host could not read is read again, and how long it will
/// wait after the next read that does not open either.
struct Retry {
    at: Instant,
    wait: Duration,
}

/// A workspace's tree as the host read it, and the records to read next.
///
/// The tree is read record by record ([`TreeChunks`]): the tree's own record
/// says which chunks hold its nodes, and a chunk holds a thousand of them. A
/// write to one file changes one record, so the host reads that record and
/// not the folder's twenty thousand files.
#[derive(Default)]
struct Kept {
    /// Each record of the tree read so far, as it read last.
    chunks: TreeChunks,
    /// Records to read next, by chunk: the tree's own record (0) and the chunks
    /// a write named or a read did not get, beside the chunks its own record
    /// lists that were never read ([`Self::due`]).
    due: BTreeSet<TreeChunk>,
    /// A member's write named one of the tree's records since the last pass,
    /// so they are read now rather than when the retry comes due.
    written: bool,
}

impl Kept {
    /// Note a write to `chunk` of the tree, making them read on the next pass.
    fn wrote(&mut self, chunk: TreeChunk) {
        self.due.insert(chunk);
        self.written = true;
    }

    /// The records to read now, in order, taken: the tree's own record when a
    /// write named it or a read did not get it, the chunks a write named or a
    /// read did not get, and the chunks the tree's own record lists that were
    /// never read. A write that lands while they are read is read on the next
    /// pass.
    fn due(&mut self) -> Vec<TreeChunk> {
        let mut due = std::mem::take(&mut self.due);
        due.extend(
            self.chunks
                .listed()
                .into_iter()
                .filter(|chunk| !self.chunks.holds(*chunk)),
        );
        due.into_iter().collect()
    }

    /// Note that `chunk` did not read: it is read again when the workspace's
    /// retry comes due, and when a write to the tree lands.
    fn failed(&mut self, chunk: TreeChunk) {
        self.due.insert(chunk);
    }
}

/// What reading a record of a tree came to.
enum Reading {
    /// It was read, and the tree now holds what it says.
    Read,
    /// It did not read: the host lacks its keys, or the write that makes it
    /// has not reached here yet.
    Failed,
    /// There is nothing to read: nobody wrote the tree yet.
    Absent,
}

/// What the host holds of the content it asked for.
#[derive(Default)]
struct State {
    /// Content asked for and not whole yet, named by the workspace whose tree
    /// references it.
    wants: ContentWants<String>,
    /// Each workspace's tree as the host read it, with the records to read next.
    trees: BTreeMap<String, Kept>,
    /// Workspaces whose content is to be asked for over their whole tree again:
    /// content arrived that is itself made of more content (a manifest), or
    /// what the tree names was asked for a while ago and never answered.
    again: BTreeSet<String>,
    /// Workspaces the host lacks content of and has not been answered about:
    /// read again in a while, so content nobody holds now is asked for again.
    unanswered: BTreeSet<String>,
    /// Workspaces with a record that did not open, and when to read it again:
    /// the keys of a record a workspace seals arrive after its host joins the
    /// channel that seals it.
    reopened: BTreeMap<String, Retry>,
}

/// A governed workspace's files, kept by the host that governs it.
pub struct FilesContent {
    workspaces: Workspaces,
    /// This host's device: what its requests name, and what the answers to
    /// them are addressed to.
    host: [u8; 32],
    store: Arc<WorkspaceStores>,
    state: Mutex<State>,
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

impl FilesContent {
    /// A host that keeps the files of `workspaces`, whose device is `host`,
    /// in each workspace's store of `store`.
    #[must_use]
    pub fn new(workspaces: Workspaces, host: [u8; 32], store: Arc<WorkspaceStores>) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            host,
            store,
            state: Mutex::default(),
        })
    }

    /// Keep the files of the workspaces this host governs from now on: what
    /// the trees hold already, then each tree as it changes, until the
    /// workspaces are no longer governed here.
    pub async fn run(self: Arc<Self>) {
        let keeper: Arc<dyn ContentKeeper> = self.clone();
        self.workspaces.keep_content(&keeper);
        // Subscribed before the first read, so a write that lands while the
        // trees are read is read too.
        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
        let mut reached = self.workspaces.holders_reached();
        self.read_all().await;
        let mut read = ticker(READ_EVERY);
        let mut again = ticker(READ_EVERY);
        let mut unanswered = ticker(ASK_AGAIN);
        let mut reopen = ticker(RETRY_FIRST);
        let mut missing = ticker(MISSING_EVERY);
        loop {
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => self.written(&write),
                    // Missed writes are read from the trees themselves.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                // A workspace that appeared, or whose membership moved: its
                // tree is read, and a record this host cannot open yet is
                // asked about before its members write to it. The others did
                // not move.
                change = governance.recv() => match change {
                    Ok(workspace) => self.keep_tree(&workspace, true).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                reached = reached.recv() => match reached {
                    Ok(workspace) => self.holder_reached(&workspace).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.state.lock_recover().wants.forget_unanswered();
                        self.read_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = read.tick() => {
                    let trees = self.written_since();
                    for (workspace, records) in trees {
                        self.keep_records(&workspace, records, false).await;
                    }
                }
                _ = again.tick() => {
                    let trees = std::mem::take(&mut self.state.lock_recover().again);
                    for workspace in trees {
                        self.keep_tree(&workspace, true).await;
                    }
                }
                _ = unanswered.tick() => {
                    let trees = std::mem::take(&mut self.state.lock_recover().unanswered);
                    for workspace in trees {
                        self.keep_tree(&workspace, true).await;
                    }
                }
                _ = reopen.tick() => {
                    for workspace in self.reopening(Instant::now()) {
                        self.keep_tree(&workspace, true).await;
                    }
                }
                _ = missing.tick() => self.ask_again_for_missing().await,
            }
        }
    }

    /// Note that `write` reached `record` of a workspace's tree, which that
    /// record is read for once the writes it takes settle.
    fn written(&self, write: &ResourceWrite) {
        if write.app != FILES_APP {
            return;
        }
        let Some(chunk) = tree::chunk_of_record(&write.record) else {
            return;
        };
        self.state
            .lock_recover()
            .trees
            .entry(write.workspace.clone())
            // A tree the host heard of through a chunk alone: its own record
            // names the chunks, and is read first.
            .or_insert_with(|| Kept {
                due: BTreeSet::from([0]),
                ..Kept::default()
            })
            .wrote(chunk);
    }

    /// The records written to since the last pass, by workspace, taken.
    fn written_since(&self) -> Vec<(String, Vec<TreeChunk>)> {
        let mut state = self.state.lock_recover();
        state
            .trees
            .iter_mut()
            .filter_map(|(workspace, kept)| {
                if !std::mem::take(&mut kept.written) {
                    return None;
                }
                Some((workspace.clone(), kept.due()))
            })
            .collect()
    }

    /// Ask the workspaces again for the content being gathered that a lost
    /// piece left short: an answer carries only so much of a file at a time,
    /// and a piece a session could not deliver is not sent again by itself,
    /// so the host asks within [`neighbour_wire::content::PIECE_PATIENCE_MS`]
    /// of the last piece it got, naming what it holds.
    async fn ask_again_for_missing(&self) {
        let asks = {
            let mut state = self.state.lock_recover();
            state.wants.reask(&self.host, now_ms())
        };
        for (workspace, ask) in asks {
            if let Err(error) = self.workspaces.ask_members(&workspace, ask).await {
                tracing::debug!(%workspace, %error, "could not ask a workspace for the content it is missing");
            }
        }
    }

    /// Read the tree of every workspace governed here.
    async fn read_all(&self) {
        for summary in self.workspaces.list().await {
            self.keep_tree(&summary.workspace, true).await;
        }
    }

    /// The workspaces whose tree is due to be read again, `now`.
    fn reopening(&self, now: Instant) -> Vec<String> {
        self.state
            .lock_recover()
            .reopened
            .iter()
            .filter(|(_, retry)| retry.at <= now)
            .map(|(workspace, _)| workspace.clone())
            .collect()
    }

    /// Read the records of `workspace`'s tree that are to be read, and ask for
    /// the content they name.
    ///
    /// `whole` asks for the content of every chunk the host holds, which a pass
    /// that is not the answer to a write does: a manifest that just arrived
    /// names the chunks asked for next, and content nobody answered is asked
    /// for again over the tree that names it.
    async fn keep_tree(&self, workspace: &str, whole: bool) {
        let due = self.due(workspace);
        self.keep_records(workspace, due, whole).await;
    }

    /// Read `records` of `workspace`'s tree, then the chunks its own record
    /// lists and the host was never sent, and ask for the content of what was
    /// read, or of the whole tree when `whole` (see [`Self::keep_tree`]).
    async fn keep_records(&self, workspace: &str, records: Vec<TreeChunk>, whole: bool) {
        let mut read = BTreeSet::new();
        let mut failed = false;
        let mut queue: VecDeque<TreeChunk> = records.into();
        let mut attempted = BTreeSet::new();
        while let Some(chunk) = queue.pop_front() {
            if !attempted.insert(chunk) {
                continue;
            }
            match self.keep_record(workspace, chunk).await {
                Reading::Read => {
                    read.insert(chunk);
                }
                Reading::Failed => failed = true,
                Reading::Absent => {}
            }
            // The tree's own record names the chunks: one just read here may
            // be a record the host never read before.
            let more: Vec<TreeChunk> = self
                .unread_chunks(workspace)
                .into_iter()
                .filter(|next| !attempted.contains(next) && !queue.contains(next))
                .collect();
            queue.extend(more);
        }
        if !failed {
            // Everything that was to be read is: the next retry is a write's.
            self.state.lock_recover().reopened.remove(workspace);
        }
        if read.is_empty() && !whole {
            // Nothing new names content.
            return;
        }
        let only = (!whole).then_some(&read);
        let (asks, missing) = self.asks(workspace, only);
        for ask in asks {
            if let Err(error) = self.workspaces.ask_members(workspace, ask).await {
                tracing::debug!(workspace, %error, "could not ask a workspace for its files");
            }
        }
        if whole {
            // Read over the whole tree: what it lacks is all it lacks.
            let blobs = u32::try_from(missing).unwrap_or(u32::MAX);
            self.workspaces.lacks_content(workspace, blobs).await;
        }
    }

    /// Ask again at once for all `workspace` lacks: its copy turned current
    /// with another holder, which may keep what nobody answered before.
    async fn holder_reached(&self, workspace: &str) {
        self.state.lock_recover().wants.forget_unanswered();
        self.keep_tree(workspace, true).await;
    }

    /// The records of `workspace`'s tree to read now, taken; the tree's own
    /// record first for one the host never read.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the records are read from what the guard holds"
    )]
    fn due(&self, workspace: &str) -> Vec<TreeChunk> {
        let mut state = self.state.lock_recover();
        let kept = state
            .trees
            .entry(workspace.to_owned())
            .or_insert_with(|| Kept {
                // A tree the host holds nothing of yet: its own record names the rest.
                due: BTreeSet::from([0]),
                ..Kept::default()
            });
        kept.due()
    }

    /// The chunks `workspace`'s tree record lists that the host never read.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the chunks are read from what the guard holds"
    )]
    fn unread_chunks(&self, workspace: &str) -> Vec<TreeChunk> {
        let state = self.state.lock_recover();
        let Some(kept) = state.trees.get(workspace) else {
            return Vec::new();
        };
        kept.chunks
            .listed()
            .into_iter()
            .filter(|chunk| !kept.chunks.holds(*chunk))
            .collect()
    }

    /// Read record `chunk` of `workspace`'s tree into what the host keeps of it.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the record is read into what the guard holds"
    )]
    async fn keep_record(&self, workspace: &str, chunk: TreeChunk) -> Reading {
        let record = tree::chunk_document_id(chunk);
        let Some(bytes) = self.record(workspace, &record).await else {
            let waiting = self
                .workspaces
                .waits_for_keys(workspace, FILES_APP, &record)
                .await;
            if chunk == 0 && !waiting {
                // Nobody wrote the tree yet, and its first write reaches this
                // host readable: it is read when that write lands.
                return Reading::Absent;
            }
            if waiting {
                self.workspaces
                    .ask_to_open(workspace, FILES_APP, &record)
                    .await;
            }
            self.failed(workspace, chunk);
            self.reopen(workspace);
            return Reading::Failed;
        };
        let mut state = self.state.lock_recover();
        let Some(kept) = state.trees.get_mut(workspace) else {
            return Reading::Absent;
        };
        match kept.chunks.read_resource(&record, &bytes) {
            Ok(true) => Reading::Read,
            Ok(false) => Reading::Absent,
            Err(error) => {
                tracing::debug!(workspace, %record, %error, "a record of a workspace's tree does not read as one");
                Reading::Absent
            }
        }
    }

    /// Note that `chunk` of `workspace`'s tree did not read.
    fn failed(&self, workspace: &str, chunk: TreeChunk) {
        if let Some(kept) = self.state.lock_recover().trees.get_mut(workspace) {
            kept.failed(chunk);
        }
    }

    /// Read `workspace`'s tree again shortly, but further apart each time it
    /// does not open.
    fn reopen(&self, workspace: &str) {
        let mut state = self.state.lock_recover();
        let wait = state.reopened.get(workspace).map_or(RETRY_FIRST, |retry| {
            retry.wait.saturating_mul(2).min(RETRY_LONGEST)
        });
        state.reopened.insert(
            workspace.to_owned(),
            Retry {
                at: Instant::now()
                    .checked_add(wait)
                    .unwrap_or_else(Instant::now),
                wait,
            },
        );
    }

    /// The governor's copy of the record `record` of `workspace`'s tree.
    async fn record(&self, workspace: &str, record: &str) -> Option<Vec<u8>> {
        match self
            .workspaces
            .resource_document(workspace, FILES_APP, record)
            .await
        {
            Ok(record) => record,
            Err(error) => {
                tracing::debug!(
                    workspace,
                    %record,
                    %error,
                    "a record of a governed workspace's tree is not readable yet"
                );
                None
            }
        }
    }

    /// The requests the host sends for the content of `workspace`'s files it
    /// lacks and has not asked for lately: the files of every chunk the host
    /// holds, or of `only` among them.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the files are read from what the guard holds"
    )]
    fn asks(&self, workspace: &str, only: Option<&BTreeSet<TreeChunk>>) -> (Vec<Vec<u8>>, usize) {
        let files = {
            let state = self.state.lock_recover();
            let Some(kept) = state.trees.get(workspace) else {
                return (Vec::new(), 0);
            };
            referenced(&kept.chunks, only)
        };
        let mut state = self.state.lock_recover();
        let mut asks = Vec::new();
        let mut missing = 0_usize;
        for file in files {
            for hash in self.missing(workspace, &file) {
                missing = missing.saturating_add(1);
                if let Some(ask) = state.wants.ask(
                    &self.host,
                    workspace.to_owned(),
                    (FILES_APP, workspace),
                    &hash,
                    now_ms(),
                ) {
                    asks.push(ask);
                }
            }
        }
        if missing > 0 {
            state.unanswered.insert(workspace.to_owned());
        }
        drop(state);
        (asks, missing)
    }

    /// The blobs of the file whose content is `manifest` that this host does
    /// not keep: the manifest itself, or, when it keeps that, the chunks the
    /// manifest lists.
    fn missing(&self, workspace: &str, manifest: &str) -> Vec<String> {
        let Ok(store) = self.store.get(workspace) else {
            return Vec::new();
        };
        let Some(bytes) = store.read(manifest) else {
            return vec![manifest.to_owned()];
        };
        let Ok(decoded) = Manifest::decode(&bytes) else {
            // A manifest that does not read names no chunk to ask for; the
            // file stays with the members that wrote it.
            tracing::debug!(%manifest, "a kept manifest does not read as one");
            return Vec::new();
        };
        decoded
            .chunks()
            .iter()
            .map(|chunk| hex::encode(chunk.hash))
            .filter(|chunk| !store.has(chunk))
            .collect()
    }
}

impl ContentKeeper for FilesContent {
    fn piece(&self, workspace: &str, _from: [u8; 32], frame: &[u8]) {
        let mut state = self.state.lock_recover();
        let Some(arrived) = state.wants.piece(&self.host, frame, now_ms()) else {
            return;
        };
        let kept = self
            .store
            .get(workspace)
            .and_then(|store| store.put(&arrived.bytes).map_err(|error| error.to_string()));
        if let Err(error) = kept {
            tracing::warn!(hash = %arrived.hash, %error, "a file's content could not be kept");
            return;
        }
        // A manifest says what the file is made of: the chunks it lists are
        // asked for on the tree's next pass.
        state.again.insert(arrived.tag);
    }

    fn expects(&self, _workspace: &str, frame: &[u8]) -> bool {
        self.state.lock_recover().wants.expects(&self.host, frame)
    }
}

/// The content of the files the chunks of `chunks` hold, in the order the tree
/// names them: each file's manifest, lowercase BLAKE3 hex, for every node that
/// is a file with content, unless it is in the trash. Only the nodes of `only`
/// when it names chunks; every chunk the host read otherwise.
fn referenced(chunks: &TreeChunks, only: Option<&BTreeSet<TreeChunk>>) -> Vec<String> {
    let numbers: Vec<TreeChunk> = match only {
        Some(only) => only.iter().copied().collect(),
        None => chunks.listed(),
    };
    numbers
        .into_iter()
        .filter_map(|chunk| chunks.chunk(chunk))
        .flat_map(|nodes| nodes.iter())
        .filter(|(_, node)| node.kind == FileNodeKind::File)
        .filter(|(_, node)| node.parent.as_deref() != Some(TRASH_ID))
        .filter_map(|(_, node)| Some(node.hash.as_ref()?.to_ascii_lowercase()))
        .collect()
}

/// The content of the files every chunk of `tree` holds now, as this host keeps it for them
/// ([`referenced`]): what a branch of the workspace must hold whole
/// ([`crate::content::branch`]).
pub(crate) fn files_named(tree: &TreeChunks) -> Vec<String> {
    referenced(tree, None)
}

/// A timer that keeps no backlog: a pass that ran late is not run twice.
fn ticker(every: Duration) -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT};
    use fs_protocol::tree::{
        HASH_KEY, KIND_KEY, NAME_KEY, NODES_KEY, PARENT_KEY, ROOT_ID, SIZE_KEY, TRASH_ID,
    };

    use super::*;

    /// A chunk record holding `files`, each `(name, content hash, bytes, parent)`.
    fn chunk_of(files: &[(&str, &str, u64, &str)]) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        let nodes = doc.put_object(ROOT, NODES_KEY, ObjType::Map).unwrap();
        for (index, (name, hash, bytes, parent)) in files.iter().enumerate() {
            let node = doc
                .put_object(&nodes, format!("device/{index}"), ObjType::Map)
                .unwrap();
            doc.put(&node, PARENT_KEY, *parent).unwrap();
            doc.put(&node, NAME_KEY, *name).unwrap();
            doc.put(&node, KIND_KEY, "file").unwrap();
            doc.put(&node, HASH_KEY, *hash).unwrap();
            doc.put(&node, SIZE_KEY, *bytes).unwrap();
        }
        doc.save()
    }

    /// The tree's own record, listing the chunks `chunks`.
    fn index_of(chunks: &[TreeChunk]) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        for chunk in chunks {
            tree::list_chunk(&mut doc, *chunk).unwrap();
        }
        doc.save()
    }

    /// What the host kept of a tree whose own record is `index` and whose
    /// chunks are `chunks`.
    fn kept(index: &[u8], chunks: &[(TreeChunk, Vec<u8>)]) -> Kept {
        let mut kept = Kept::default();
        kept.chunks
            .read_resource(&tree::chunk_document_id(0), index)
            .unwrap();
        for (chunk, doc) in chunks {
            kept.chunks
                .read_resource(&tree::chunk_document_id(*chunk), doc)
                .unwrap();
        }
        kept
    }

    tilo_test_env::scenario! {
        fn a_tree_references_only_the_content_of_its_live_files;

        phase a_tree_names_the_content_of_the_files_it_holds;
        {
            let report = "b1".repeat(32);
            let build = "c2".repeat(32);
            let chunk = chunk_of(&[
                ("report.txt", &report, 300_000, ROOT_ID),
                ("build.bin", &build, 3_145_728, ROOT_ID),
            ]);
            let read = kept(&index_of(&[1]), &[(1, chunk)]);
            assert_eq!(referenced(&read.chunks, None), [report, build]);
        }

        phase a_trashed_file_is_no_content_to_keep;
        {
            let deleted = "d3".repeat(32);
            let chunk = chunk_of(&[("gone.txt", &deleted, 12, TRASH_ID)]);
            let read = kept(&index_of(&[1]), &[(1, chunk)]);
            assert!(referenced(&read.chunks, None).is_empty());
        }

        phase a_tree_no_mount_wrote_references_nothing;
        {
            let mut doc = AutoCommit::new();
            doc.put_object(ROOT, NODES_KEY, ObjType::Map).unwrap();
            assert!(referenced(&kept(&doc.save(), &[]).chunks, None).is_empty());
        }

        phase only_the_chunks_asked_for_are_read_for_files;
        {
            let first = "e4".repeat(32);
            let second = "f5".repeat(32);
            let read = kept(
                &index_of(&[1, 2]),
                &[
                    (1, chunk_of(&[("first.txt", &first, 1, ROOT_ID)])),
                    (2, chunk_of(&[("second.txt", &second, 1, ROOT_ID)])),
                ],
            );
            assert_eq!(
                referenced(&read.chunks, Some(&BTreeSet::from([2]))),
                [second],
                "a write to one chunk asks for the content of that chunk's files"
            );
        }
    }

    #[test]
    fn a_write_to_one_chunk_reads_that_chunk_alone() {
        let mut read = kept(
            &index_of(&[1, 2]),
            &[
                (1, chunk_of(&[("first.txt", &"e4".repeat(32), 1, ROOT_ID)])),
                (2, chunk_of(&[("second.txt", &"f5".repeat(32), 1, ROOT_ID)])),
            ],
        );
        assert!(
            read.due().is_empty(),
            "a tree just read has nothing to read"
        );
        read.wrote(2);
        assert_eq!(read.due(), [2], "the chunk written to is read, and only it");
        assert!(read.due().is_empty(), "and is not read twice");
    }

    #[test]
    fn a_write_to_the_trees_own_record_reads_it_for_the_chunks_it_lists() {
        let mut read = kept(&index_of(&[1]), &[(1, chunk_of(&[]))]);
        read.wrote(0);
        assert_eq!(
            read.due(),
            [0],
            "the tree's own record is read on a write to it"
        );
        // Read again, it lists chunk 2, which the host never read.
        read.chunks
            .read_resource(&tree::chunk_document_id(0), &index_of(&[1, 2]))
            .unwrap();
        assert_eq!(read.due(), [2], "a chunk newly listed is read once");
        read.chunks
            .read_resource(&tree::chunk_document_id(2), &chunk_of(&[]))
            .unwrap();
        assert!(read.due().is_empty());
    }
}
