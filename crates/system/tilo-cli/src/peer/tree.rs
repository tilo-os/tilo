//! A workspace's file tree on this computer: the Files app's record, held
//! through the device's session as every device of the workspace holds it.
//!
//! A workspace's file truth is one record, the tree as an Automerge document
//! ([`fs_protocol::tree`]), whose nodes name each file's content by hash; the
//! bytes travel beside it as the record's content
//! ([`super::content`]). Tilo OS publishes the tree from its filesystem daemon
//! on the desktop session ([`tilo_fs::tree_record`]); here the session is this
//! device's own session with the workspace's host, the one a browser tab
//! keeps, and the tree goes over it the way a tab's records do: opened by its
//! route, written with [`Outbox::write`], and read back from the session's
//! replica each time a write [`Event::Landed`] on it.
//!
//! The namespace this device mounts ([`tilo_fs::Namespace`]) and the record
//! converge both ways: a change the record brings merges into the namespace
//! (and what resolving it wrote goes back), and a change the namespace makes
//! (a file copied into the mount) goes out as the next increment.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::DocumentMutation;
use tilo_fs::namespace::Namespace;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;
use tilo_session_client::engine::RecordReading;
use tilo_session_client::{Cmd, DocumentRoute, Event, Link, Outbox as _};

use super::content::SessionContent;

/// The app whose record a workspace's tree is.
pub(crate) use fs_protocol::tree::FILES_APP;

/// How often the replica looks again when nothing woke it: the session's
/// events wake it at once, this only bounds a missed wake.
const LOOK_AGAIN: Duration = Duration::from_secs(1);
/// How often the namespace is kept on disk while it changes.
const KEEP_EVERY: Duration = Duration::from_secs(2);

/// The route of `workspace`'s tree.
pub(crate) fn route(workspace: WorkspaceId) -> DocumentRoute {
    DocumentRoute {
        app_id: FILES_APP.to_owned(),
        cluster_id: workspace.to_string(),
        record: collab_types::person_record_id(fs_protocol::tree::TREE_KIND),
    }
}

/// Where the replica stands, as `tilo status` says it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a fact `tilo status` reports on its own"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Standing {
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
    standing: Arc<Mutex<Standing>>,
    paused: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TreeReplica {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TreeReplica {
    /// Hold `workspace`'s tree over `link`, kept in `state` between runs, as
    /// the device whose key is `device`; its file bytes come and go through
    /// `content`. Must be called on a tokio runtime.
    ///
    /// # Errors
    /// The kept namespace cannot be read.
    pub(crate) fn start(
        link: Link,
        workspace: WorkspaceId,
        state: &Path,
        device: [u8; 32],
        content: Arc<SessionContent>,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(state)?;
        let kept = state.join("tree.automerge");
        let author = hex::encode(device);
        let namespace = match std::fs::read(&kept) {
            Ok(bytes) => Namespace::from_bytes(author, &bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Namespace::new(author).map_err(std::io::Error::other)?
            }
            Err(error) => return Err(error),
        };
        let namespace = Arc::new(Mutex::new(namespace));
        let standing = Arc::new(Mutex::new(Standing::default()));
        let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake = Arc::new(tokio::sync::Notify::new());
        let sync = Sync {
            paused: Arc::clone(&paused),
            wake: Arc::clone(&wake),
            link,
            workspace,
            route: route(workspace),
            namespace: Arc::clone(&namespace),
            merged: None,
            sent: Vec::new(),
            founded: false,
            kept,
            kept_heads: Vec::new(),
            standing: Arc::clone(&standing),
            content,
        };
        let task = tokio::spawn(sync.run());
        Ok(Self {
            namespace,
            standing,
            paused,
            wake,
            task,
        })
    }

    pub(crate) fn standing(&self) -> Standing {
        self.standing.lock_recover().clone()
    }

    /// Stop syncing (changes made here wait to go out, and none come in), or
    /// start again.
    pub(crate) fn set_paused(&self, paused: bool) {
        self.paused
            .store(paused, std::sync::atomic::Ordering::Relaxed);
        self.wake.notify_one();
    }
}

/// The task that keeps one namespace and its record converged.
struct Sync {
    paused: Arc<std::sync::atomic::AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
    link: Link,
    workspace: WorkspaceId,
    route: DocumentRoute,
    namespace: Arc<Mutex<Namespace>>,
    /// The record's heads this namespace merged last; `None` before the first merge.
    merged: Option<Vec<[u8; 32]>>,
    /// The namespace's heads handed to the session last: every change after
    /// them is the next increment. Empty until the tree went out or came in.
    sent: Vec<automerge::ChangeHash>,
    /// Whether the record exists at the workspace (a whole tree went out or
    /// came in), so the next write is an increment.
    founded: bool,
    kept: PathBuf,
    kept_heads: Vec<automerge::ChangeHash>,
    standing: Arc<Mutex<Standing>>,
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
                    | Event::RecordOpened { .. }
                    | Event::Refused { .. }
                    | Event::RecordContent { .. }
                    | Event::HostReady { .. }
            )
        });
        let mut changes = self.namespace.lock_recover().subscribe_changes();
        let mut keep = tokio::time::interval(KEEP_EVERY);
        self.open();
        loop {
            for event in events.drain() {
                self.take(event);
            }
            if !self.paused.load(std::sync::atomic::Ordering::Relaxed) {
                self.pull();
                self.push();
                self.content.reask(&self.link);
            }
            self.say_where_it_stands();
            tokio::select! {
                () = wake.notified() => {}
                moved = changes.changed() => if moved.is_err() { return },
                _ = keep.tick() => self.keep(),
                () = tokio::time::sleep(LOOK_AGAIN) => {}
            }
        }
    }

    /// Ask the host for the tree, and to take part in its channel.
    fn open(&self) {
        let _ = self
            .link
            .send(Cmd::OpenRecordAt(self.workspace, self.route.clone()));
        let _ = self.link.send(Cmd::OpenRoute(self.route.clone()));
        // And on the workspace's plane, whose host answers what it holds.
        let _ = self.link.engine().open_record(
            &self.workspace.to_string(),
            &self.route.app_id,
            &self.route.record,
        );
    }

    fn take(&self, event: Event) {
        match event {
            Event::HostReady { ws, .. } if ws == self.workspace => self.open(),
            Event::Refused { route, reason, .. } if route == self.route => {
                tracing::warn!(%reason, workspace = %self.workspace, "the workspace refused a change to its files");
            }
            Event::RecordContent { ws, frame, .. } if ws == self.workspace => {
                self.content.piece(&frame);
            }
            // The host sent all it holds of the tree, none when the workspace has none yet:
            // this device's first change founds it.
            Event::RecordOpened {
                route: Some(route), ..
            } if route == self.route => {
                self.standing.lock_recover().opened = true;
            }
            _ => {}
        }
    }

    /// Merge what the record holds that this namespace has not merged yet.
    fn pull(&mut self) {
        let engine = self.link.engine();
        let workspace = self.workspace.to_string();
        let reading = engine.record_update(
            &workspace,
            &self.route.app_id,
            &self.route.record,
            self.merged.as_deref(),
        );
        let (mutation, heads) = match reading {
            Ok(RecordReading::Captured { heads, bytes, .. }) => {
                (DocumentMutation::Snapshot(bytes), heads)
            }
            Ok(RecordReading::Changes { heads, bytes, .. }) => {
                (DocumentMutation::Incremental(bytes), heads)
            }
            Ok(RecordReading::Unchanged(_)) => return,
            Ok(RecordReading::Pending) | Err(_) => {
                // Nothing here yet: once the host answered with none, the
                // workspace has no tree, and this device's first change
                // founds it.
                if engine
                    .record_answer(&workspace, &self.route.app_id, &self.route.record)
                    .is_some()
                {
                    self.standing.lock_recover().opened = true;
                }
                return;
            }
        };
        let resolution = {
            let mut namespace = self.namespace.lock_recover();
            let merged = match &mutation {
                DocumentMutation::Snapshot(bytes) => {
                    namespace.adopt_or_merge_remote_and_resolve(bytes)
                }
                DocumentMutation::Incremental(bytes) => namespace.apply_remote_and_resolve(bytes),
            };
            match merged {
                Ok(resolution) => resolution,
                Err(error) => {
                    tracing::warn!(%error, "the workspace's tree does not merge here");
                    return;
                }
            }
        };
        let shared = record_heads(&heads);
        self.merged = Some(heads);
        self.founded = true;
        self.standing.lock_recover().opened = true;
        if !resolution.is_empty() {
            let _ = self.link.write(
                self.route.clone(),
                &DocumentMutation::Incremental(resolution),
            );
        }
        // What the record holds is not this device's to send again; what only
        // this namespace holds is. A namespace makes changes of its own before
        // it ever merged the record (the tree it starts as, a file copied in
        // while the host had not answered), and a record whose document holds
        // no tree yet is not merged into it at all: counted as sent, those
        // changes never reach the host, which then leaves out every later
        // write of this device as building on changes it never got.
        if self.sent.is_empty() {
            self.sent = shared;
        }
    }

    /// Hand the session what the namespace changed since it last did.
    fn push(&mut self) {
        let standing_opened = self.standing.lock_recover().opened;
        if !standing_opened {
            // The host has not said what it holds: a tree written now could
            // found a second one beside it.
            return;
        }
        let mut namespace = self.namespace.lock_recover();
        if namespace.heads() == self.sent {
            return;
        }
        let mutation = if self.founded {
            let bytes = namespace.changes_since(&self.sent);
            if bytes.is_empty() {
                self.sent = namespace.heads();
                return;
            }
            DocumentMutation::Incremental(bytes)
        } else {
            if !namespace.holds_tree() {
                return;
            }
            DocumentMutation::Snapshot(namespace.save())
        };
        drop(namespace);
        if self.link.write(self.route.clone(), &mutation).is_ok() {
            self.founded = true;
            self.sent = self.namespace.lock_recover().heads();
        }
    }

    fn say_where_it_stands(&self) {
        let (files, bytes) = {
            let namespace = self.namespace.lock_recover();
            let mut files = 0_usize;
            let mut bytes = 0_u64;
            for id in namespace.live_node_ids() {
                // A text document (a `.md`, an `.odt`) is a file whose content
                // the tree holds itself, by no hash: a file all the same.
                if let Some(entry) = namespace.entry_for_node(&id)
                    && entry.kind == tilo_fs::namespace::NodeKind::File
                {
                    files = files.saturating_add(1);
                    bytes = bytes.saturating_add(entry.size);
                }
            }
            (files, bytes)
        };
        let sent = self.sent == self.namespace.lock_recover().heads();
        let durable = !self
            .link
            .engine()
            .held_writes()
            .lock()
            .contains_key(&self.route)
            && self.link.snapshot().outgoing_pending == 0;
        let mut standing = self.standing.lock_recover();
        standing.files = files;
        standing.bytes = bytes;
        standing.paused = self.paused.load(std::sync::atomic::Ordering::Relaxed);
        standing.sent = sent;
        standing.durable = durable;
    }

    /// Keep the namespace on disk, so a restart starts where this one was.
    fn keep(&mut self) {
        let (heads, bytes) = {
            let mut namespace = self.namespace.lock_recover();
            let heads = namespace.heads();
            if heads == self.kept_heads {
                return;
            }
            (heads, namespace.save())
        };
        match tilo_files::native::atomic::write_with_mode(&self.kept, &bytes, 0o600) {
            Ok(()) => self.kept_heads = heads,
            Err(error) => tracing::warn!(%error, "the workspace's tree is not kept on disk"),
        }
    }
}

/// The record's heads as the namespace's change hashes: the same changes,
/// since the namespace and the record are one Automerge document.
fn record_heads(heads: &[[u8; 32]]) -> Vec<automerge::ChangeHash> {
    heads
        .iter()
        .map(|head| automerge::ChangeHash(*head))
        .collect()
}

impl Drop for Sync {
    fn drop(&mut self) {
        self.keep();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A namespace that changed before it merged the record (its own start, a
    /// file copied in while the host had not answered) sends those changes
    /// with its next write: the record then holds every file, with nothing a
    /// write of this device builds on missing.
    #[test]
    fn what_a_namespace_made_before_the_record_arrived_goes_out() {
        // The record as a page that opened Files founded it: no tree yet.
        let mut founding = automerge::AutoCommit::new();
        let _ = founding.commit();
        let founded = founding.save();
        let heads: Vec<[u8; 32]> = founding.get_heads().iter().map(|head| head.0).collect();

        let mut namespace = Namespace::new("laptop").unwrap();
        namespace
            .adopt_or_merge_remote_and_resolve(&founded)
            .unwrap();
        let sent = record_heads(&heads);
        let hash = iroh_blobs::Hash::new(b"net 30");
        namespace
            .create_file(
                tilo_fs::namespace::ROOT_INO,
                "contract.txt",
                Some(&hash),
                6,
                1,
            )
            .unwrap();

        // What the host keeps: the record as founded, then this device's write.
        let mut record =
            automerge::AutoCommit::load(&[founded, namespace.changes_since(&sent)].concat())
                .unwrap();
        assert!(
            record.get_missing_deps(&[]).is_empty(),
            "every change the record took builds on changes it holds"
        );
        let tree = fs_protocol::tree::Tree::of_record(&record.save()).unwrap();
        assert!(
            tree.nodes
                .values()
                .any(|node| node.name.as_bytes() == b"contract.txt" && node.hash.is_some()),
            "{tree:?}"
        );
    }
}
