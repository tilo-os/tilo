//! The files of the workspaces this host governs, kept so that a member which
//! comes up later reads them from this host alone.
//!
//! A workspace's file truth is its Files app record: the tree as one Automerge
//! document, whose nodes name each file's content by hash and never carry the
//! bytes ([`fs::tree_record`]), and the bytes themselves, which travel beside
//! the record as its content ([`tilo_action_broker::record_content`]) between
//! the members the host carries them for. A member keeps the files it wrote
//! and answers whoever asks; a member that leaves — a VM host that was
//! suspended, evicted or replaced, a laptop that went offline — takes the only
//! copy with it, and nothing that joins afterwards has any to hydrate from.
//!
//! So the host keeps them. It reads each governed workspace's tree, the record
//! it already holds and opens, sealed or not
//! ([`Workspaces::record_document`]); for every file the tree names it asks
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
//! workspace's files.
//!
//! What it keeps goes into the host's one blob store
//! ([`crate::blobs::FsBlobs`], beside what its bots fetched and the files its
//! members posted), from where the host already answers members asking their
//! workspace for content ([`Workspaces::content_answer`]): a member that joins
//! after the writer left hydrates the same bytes from this host.
//!
//! # What it keeps, and how much
//!
//! Everything the governed workspaces' current trees reference: each file's
//! manifest and its chunks. A file dropped from a tree is no longer asked for,
//! but the blobs of it stay in the store — nothing collects them yet, and the
//! same bytes may still serve another workspace, a bot or a member's post —
//! which is the one way this grows.
//!
//! To keep that from growing without bound, what a workspace's files take up
//! here is bounded by [`FilesLimits::workspace_bytes`]. A pass counts the
//! files whose content is wholly here, and past the bound it asks for no more:
//! the files it reaches for first are the ones the tree names first, and a
//! file past the bound stays with the members that wrote it, as it was before
//! this host kept anything. The host says so in the log when a workspace
//! starts and stops being over its bound, rather than silently keeping less.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs::content::Manifest;
use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{TREE_KIND, Tree};
use neighbour_wire::content::ContentWants;
use tilo_lock::MutexRecover as _;

use crate::blobs::FsBlobs;
use crate::workspaces::{ContentKeeper, RecordWrite, WorkspaceContent, Workspaces};

/// The app whose record a workspace's tree is.
const FILES_APP: &str = fs::tree_record::FILES_APP;
/// How often the trees of workspaces written to since the last pass are read:
/// a workspace takes many writes a second, and one pass sees their files.
const READ_EVERY: Duration = Duration::from_secs(2);
/// How long the host waits before it asks again for content nobody answered.
const ASK_AGAIN: Duration = Duration::from_secs(90);
/// How long a tree that could not be read waits before it is read again, at
/// first: each failed read waits twice as long, up to [`RETRY_LONGEST`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
/// The longest wait between two reads of a tree that does not open.
const RETRY_LONGEST: Duration = Duration::from_secs(60);

/// How much of a workspace's files this host keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilesLimits {
    /// What the files it keeps whole for one workspace may take together.
    /// Past it, it asks for no more of that workspace's files, and the rest
    /// stay with their members.
    pub workspace_bytes: u64,
}

impl Default for FilesLimits {
    fn default() -> Self {
        Self {
            workspace_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// When a tree the host could not read is read again, and how long it will
/// wait after the next read that does not open either.
struct Retry {
    at: Instant,
    wait: Duration,
}

/// What the host holds of the content it asked for.
#[derive(Default)]
struct State {
    /// Content asked for and not whole yet, named by the workspace whose tree
    /// references it.
    wants: ContentWants<String>,
    /// Workspaces whose tree is to be read again: written to, or answered
    /// with content that is itself made of more content (a manifest).
    again: BTreeSet<String>,
    /// Workspaces the host lacks content of and has not been answered about:
    /// read again in a while, so content nobody holds now is asked for again.
    unanswered: BTreeSet<String>,
    /// Workspaces whose tree did not open, and when to read it again: the
    /// keys of a record a workspace seals arrive after its host joins the
    /// channel that seals it.
    reopened: BTreeMap<String, Retry>,
    /// Workspaces whose files are past what this host keeps of them, so its
    /// log names each one once.
    over: BTreeSet<String>,
}

/// A governed workspace's files, kept by the host that governs it.
pub struct FilesContent {
    workspaces: Workspaces,
    /// This host's device: what its requests name, and what the answers to
    /// them are addressed to.
    host: [u8; 32],
    store: Arc<FsBlobs>,
    limits: FilesLimits,
    state: Mutex<State>,
}

/// One file of a tree the host keeps the content of.
struct Referenced {
    /// The content id, lowercase BLAKE3 hex: the file's manifest.
    hash: String,
    /// How many bytes the tree says the file is.
    bytes: u64,
}

/// The record a workspace's tree is, as the device that publishes it names it.
fn tree_record() -> String {
    collab_types::person_record_id(TREE_KIND)
}

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

impl FilesContent {
    /// A host that keeps the files of `workspaces`, whose device is `host`,
    /// in `store`, within `limits`.
    #[must_use]
    pub fn new(
        workspaces: Workspaces,
        host: [u8; 32],
        store: Arc<FsBlobs>,
        limits: FilesLimits,
    ) -> Arc<Self> {
        Arc::new(Self {
            workspaces,
            host,
            store,
            limits,
            state: Mutex::default(),
        })
    }

    /// Keep the files of the workspaces this host governs from now on: what
    /// the trees hold already, then each tree as it changes, until the
    /// workspaces are no longer governed here.
    pub async fn run(self: Arc<Self>) {
        let store: Arc<dyn WorkspaceContent> = self.store.clone();
        self.workspaces.serve_content(store);
        self.workspaces.keep_content(self.clone());
        // Subscribed before the first read, so a write that lands while the
        // trees are read is read too.
        let mut writes = self.workspaces.record_writes();
        let mut governance = self.workspaces.governance_changes();
        self.read_all().await;
        let mut read = ticker(READ_EVERY);
        let mut again = ticker(ASK_AGAIN);
        let mut reopen = ticker(RETRY_FIRST);
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
                // asked about before its members write to it.
                change = governance.recv() => match change {
                    Ok(_) => self.read_all().await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        self.read_all().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = read.tick() => {
                    let trees = std::mem::take(&mut self.state.lock_recover().again);
                    for workspace in trees {
                        self.keep_tree(&workspace).await;
                    }
                }
                _ = again.tick() => {
                    let trees = std::mem::take(&mut self.state.lock_recover().unanswered);
                    for workspace in trees {
                        self.keep_tree(&workspace).await;
                    }
                }
                _ = reopen.tick() => {
                    for workspace in self.reopening(Instant::now()) {
                        self.keep_tree(&workspace).await;
                    }
                }
            }
        }
    }

    /// Note that `write` reached a workspace's tree, which is read again once
    /// the writes it takes settle.
    fn written(&self, write: &RecordWrite) {
        if write.app != FILES_APP {
            return;
        }
        self.state
            .lock_recover()
            .again
            .insert(write.workspace.clone());
    }

    /// Read the tree of every workspace governed here.
    async fn read_all(&self) {
        for summary in self.workspaces.list().await {
            self.keep_tree(&summary.workspace).await;
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

    /// Ask the workspace's members for the content of `workspace`'s files that
    /// this host does not keep yet.
    async fn keep_tree(&self, workspace: &str) {
        let Some(record) = self.record(workspace).await else {
            // A tree its members write sealed opens only once this host's
            // device is in the channel that seals it: ask, and read again.
            self.workspaces
                .ask_to_open(workspace, FILES_APP, &tree_record())
                .await;
            self.reopen(workspace);
            return;
        };
        self.state.lock_recover().reopened.remove(workspace);
        let Ok(tree) = Tree::of_record(&record) else {
            tracing::debug!(workspace, "a workspace's tree does not read as one");
            return;
        };
        for ask in self.asks(workspace, &tree) {
            if let Err(error) = self.workspaces.ask_members(workspace, ask).await {
                tracing::debug!(workspace, %error, "could not ask a workspace for its files");
            }
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

    /// The governor's copy of `workspace`'s tree.
    async fn record(&self, workspace: &str) -> Option<Vec<u8>> {
        match self
            .workspaces
            .record_document(workspace, FILES_APP, &tree_record())
            .await
        {
            Ok(record) => record,
            Err(error) => {
                tracing::debug!(
                    workspace,
                    %error,
                    "the tree of a governed workspace is not readable yet"
                );
                None
            }
        }
    }

    /// The requests the host sends for the content of `tree`'s files it lacks,
    /// within what a workspace may keep and not asked for lately.
    fn asks(&self, workspace: &str, tree: &Tree) -> Vec<Vec<u8>> {
        let files = referenced(tree);
        let mut state = self.state.lock_recover();
        let mut kept = 0_u64;
        let mut wanted = 0_u64;
        let mut asks = Vec::new();
        let mut bounded = false;
        let mut unanswered = false;
        for file in files {
            let missing = self.missing(&file.hash);
            if missing.is_empty() {
                kept = kept.saturating_add(file.bytes);
                continue;
            }
            if kept.saturating_add(wanted).saturating_add(file.bytes) > self.limits.workspace_bytes
            {
                bounded = true;
                unanswered = true;
                continue;
            }
            let mut asked = false;
            for hash in missing {
                if let Some(ask) = state.wants.ask(
                    &self.host,
                    workspace.to_owned(),
                    (FILES_APP, workspace),
                    &hash,
                    now_ms(),
                ) {
                    asks.push(ask);
                    asked = true;
                }
            }
            if asked {
                wanted = wanted.saturating_add(file.bytes);
            }
            unanswered = true;
        }
        if unanswered {
            state.unanswered.insert(workspace.to_owned());
        }
        if bounded && state.over.insert(workspace.to_owned()) {
            tracing::info!(
                workspace,
                limit = self.limits.workspace_bytes,
                "a workspace's files are past what this host keeps of them; the rest stay with its members"
            );
        } else if !bounded {
            state.over.remove(workspace);
        }
        drop(state);
        asks
    }

    /// The blobs of the file whose content is `manifest` that this host does
    /// not keep: the manifest itself, or, when it keeps that, the chunks the
    /// manifest lists.
    fn missing(&self, manifest: &str) -> Vec<String> {
        let Some(bytes) = self.store.read(manifest) else {
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
            .filter(|chunk| !self.store.contains(chunk))
            .collect()
    }
}

impl ContentKeeper for FilesContent {
    fn piece(&self, _workspace: &str, _from: [u8; 32], frame: &[u8]) {
        let mut state = self.state.lock_recover();
        let Some(arrived) = state.wants.piece(&self.host, frame) else {
            return;
        };
        if let Err(error) = self.store.keep(&arrived.bytes) {
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

/// The files `tree` holds the content of, in the order the tree names them:
/// every node that is a file with content, unless it is in the trash.
fn referenced(tree: &Tree) -> Vec<Referenced> {
    tree.nodes
        .iter()
        .filter(|(id, _)| !tree.trashed(id))
        .filter(|(_, node)| node.kind == FileNodeKind::File)
        .filter_map(|(_, node)| {
            Some(Referenced {
                hash: node.hash.as_ref()?.to_ascii_lowercase(),
                bytes: node.size,
            })
        })
        .collect()
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

    /// A tree record holding `files`, each `(name, content hash, bytes, parent)`.
    fn tree_of(files: &[(&str, &str, u64, &str)]) -> Vec<u8> {
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

    #[test]
    fn a_tree_names_the_content_of_the_files_it_holds() {
        let report = "b1".repeat(32);
        let build = "c2".repeat(32);
        let record = tree_of(&[
            ("report.txt", &report, 300_000, ROOT_ID),
            ("build.bin", &build, 3_145_728, ROOT_ID),
        ]);
        let files = referenced(&Tree::of_record(&record).unwrap());
        assert_eq!(files.len(), 2);
        assert_eq!(
            (files[0].hash.as_str(), files[0].bytes),
            (report.as_str(), 300_000)
        );
        assert_eq!(
            (files[1].hash.as_str(), files[1].bytes),
            (build.as_str(), 3_145_728)
        );
    }

    #[test]
    fn a_trashed_file_is_no_content_to_keep() {
        let deleted = "d3".repeat(32);
        let record = tree_of(&[("gone.txt", &deleted, 12, TRASH_ID)]);
        assert!(referenced(&Tree::of_record(&record).unwrap()).is_empty());
    }

    #[test]
    fn a_tree_no_mount_wrote_references_nothing() {
        let mut doc = AutoCommit::new();
        doc.put_object(ROOT, NODES_KEY, ObjType::Map).unwrap();
        assert!(referenced(&Tree::of_record(&doc.save()).unwrap()).is_empty());
    }
}
