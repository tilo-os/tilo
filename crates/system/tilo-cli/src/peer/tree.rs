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
use std::time::{Duration, Instant};

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
/// How many bytes of changes one write of the tree carries at most: well under the most a
/// document write may carry, so changes made while the tree could not go out (a repository
/// checked out into the folder) go out over several writes instead of one no host takes.
const WRITE_BUDGET: usize = collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES / 2;
/// How often the namespace is kept on disk while it changes, at most.
const KEEP_EVERY: Duration = Duration::from_secs(2);
/// How often the files and bytes the tree names are counted again while it changes, at most.
const COUNT_EVERY: Duration = Duration::from_secs(1);
/// How many times its own cost a step that walks or saves the whole tree waits before it runs
/// again: it then takes at most a twentieth of the replica's time. Run on every change, a count
/// of every node made writing a repository's twenty thousand files quadratic, and held the tree
/// (which every write into the folder waits for) for most of the time.
const PACE: u32 = 20;

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
            keeping: Paced::new(KEEP_EVERY),
            pulling: Paced::new(LOOK_AGAIN),
            pushing: Paced::new(LOOK_AGAIN),
            unsent: None,
            counting: Paced::new(COUNT_EVERY),
            counted_heads: None,
            unread: None,
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
    keeping: Paced,
    counting: Paced,
    pulling: Paced,
    /// When a write the session turned down may be tried again: building one walks every change
    /// since the last write that went out, so a write that keeps failing is not rebuilt each pass.
    pushing: Paced,
    /// Why the last write of the tree failed, said once until one goes out or fails otherwise.
    unsent: Option<String>,
    /// The heads the files and bytes in `standing` were counted at.
    counted_heads: Option<Vec<automerge::ChangeHash>>,
    /// Why the last read of the record failed, said once until it reads or fails otherwise.
    unread: Option<String>,
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
                self.pull_paced();
                self.push();
                self.content.reask(&self.link);
            }
            self.say_where_it_stands();
            tokio::select! {
                () = wake.notified() => {}
                moved = changes.changed() => if moved.is_err() { return },
                _ = keep.tick() => if self.keeping.due(Instant::now()) {
                    let started = Instant::now();
                    self.keep();
                    self.keeping.ran(started, Instant::now());
                },
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
            Event::Refused { route, refusal, .. } if route == self.route => {
                tracing::warn!(%refusal, workspace = %self.workspace, "the workspace refused a change to its files");
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

    /// [`Self::pull`], paced while the session holds writes of this device's that its replica
    /// has not taken in: a read then overlays them on the whole record, so it costs the whole
    /// tree each time, and all it can bring is another device's change, which waits a moment.
    fn pull_paced(&mut self) {
        let holding = self
            .link
            .engine()
            .held_writes()
            .lock()
            .contains_key(&self.route);
        if !holding {
            self.pull();
            return;
        }
        let now = Instant::now();
        if self.pulling.due(now) {
            self.pull();
            self.pulling.ran(now, Instant::now());
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
            Ok(RecordReading::Unchanged(_)) => {
                self.unread = None;
                return;
            }
            Err(error) => {
                if self.unread.as_ref() != Some(&error) {
                    tracing::warn!(%error, workspace = %self.workspace, "the workspace's tree does not read here");
                    self.unread = Some(error);
                }
                self.note_answered(&workspace);
                return;
            }
            Ok(RecordReading::Pending) => {
                self.note_answered(&workspace);
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
        self.unread = None;
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

    /// Nothing of the tree reads here yet: once the host answered with none, the workspace has
    /// no tree, and this device's first change founds it. A host that sent some of it holds a
    /// tree, which this device then waits to read: a tree it founded beside that one would go
    /// out whole, and the workspace refuses it for good.
    fn note_answered(&self, workspace: &str) {
        let answer =
            self.link
                .engine()
                .record_answer(workspace, &self.route.app_id, &self.route.record);
        if holds_none(answer) {
            self.standing.lock_recover().opened = true;
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
        let started = Instant::now();
        if !self.pushing.due(started) {
            return;
        }
        let outgoing = outgoing(&mut self.namespace.lock_recover(), &self.sent, self.founded);
        let (mutation, covered) = match outgoing {
            Outgoing::Nothing => return,
            Outgoing::Covered(heads) => {
                self.sent = heads;
                return;
            }
            Outgoing::Write(mutation, heads) => (mutation, heads),
        };
        if let Err(error) = self.link.write(self.route.clone(), &mutation) {
            if self.unsent.as_ref() != Some(&error) {
                tracing::warn!(%error, workspace = %self.workspace, "the workspace's tree did not go out; it is tried again");
                self.unsent = Some(error);
            }
            self.pushing.ran(started, Instant::now());
            return;
        }
        self.unsent = None;
        self.founded = true;
        // What the write carries, not what the namespace holds now: the
        // mount keeps changing it while the write goes out, and a change
        // made meanwhile counted as sent would never reach the host, which
        // then leaves out every later write of this device.
        self.sent = covered;
    }

    fn say_where_it_stands(&mut self) {
        let heads = self.namespace.lock_recover().heads();
        let counted =
            if self.counted_heads.as_ref() != Some(&heads) && self.counting.due(Instant::now()) {
                let started = Instant::now();
                let counted = self.count();
                self.counting.ran(started, Instant::now());
                self.counted_heads = Some(heads.clone());
                Some(counted)
            } else {
                None
            };
        let sent = self.sent == heads;
        let durable = !self
            .link
            .engine()
            .held_writes()
            .lock()
            .contains_key(&self.route)
            && self.link.outgoing_pending() == 0;
        let mut standing = self.standing.lock_recover();
        if let Some((files, bytes)) = counted {
            standing.files = files;
            standing.bytes = bytes;
        }
        standing.paused = self.paused.load(std::sync::atomic::Ordering::Relaxed);
        standing.sent = sent;
        standing.durable = durable;
    }

    /// The files the tree names and their bytes: a walk of every node.
    fn count(&self) -> (usize, u64) {
        {
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
        }
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

/// Whether the host's `answer` about the tree (`None` while it owes one) says the workspace has
/// none of it.
fn holds_none(answer: Option<tilo_relay_protocol::RecordAnswer>) -> bool {
    answer == Some(tilo_relay_protocol::RecordAnswer::Sent { frames: 0 })
}

/// What a namespace has to send after `sent`.
enum Outgoing {
    /// Nothing, or nothing it may send yet.
    Nothing,
    /// Nothing new: everything up to these heads is out.
    Covered(Vec<automerge::ChangeHash>),
    /// This write, which carries the namespace up to these heads.
    Write(DocumentMutation, Vec<automerge::ChangeHash>),
}

/// What `namespace` has to send after `sent`, read under one lock with the heads it reaches.
/// Until the record is `founded`, the whole tree goes out once it holds one.
fn outgoing(namespace: &mut Namespace, sent: &[automerge::ChangeHash], founded: bool) -> Outgoing {
    let heads = namespace.heads();
    if heads == sent {
        return Outgoing::Nothing;
    }
    if founded {
        let (bytes, reached) = namespace.changes_since_within(sent, WRITE_BUDGET);
        if bytes.is_empty() {
            return Outgoing::Covered(heads);
        }
        return Outgoing::Write(DocumentMutation::Incremental(bytes), reached);
    }
    if !namespace.holds_tree() {
        return Outgoing::Nothing;
    }
    Outgoing::Write(DocumentMutation::Snapshot(namespace.save()), heads)
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

    #[test]
    fn a_tree_the_host_sent_but_that_does_not_read_here_is_not_founded_again() {
        // The host owes its answer still: nothing is known.
        assert!(!super::holds_none(None));
        // It sent frames of the tree: the workspace holds one, though it may not read here
        // yet (a write still on its way), and a tree founded now would go out whole beside it.
        assert!(!super::holds_none(Some(
            tilo_relay_protocol::RecordAnswer::Sent { frames: 3 }
        )));
        // It sent none: the workspace has no tree, and this device's first change founds it.
        assert!(super::holds_none(Some(
            tilo_relay_protocol::RecordAnswer::Sent { frames: 0 }
        )));
    }

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
    #[test]
    fn changes_larger_than_one_write_go_out_over_several() {
        let mut founding = automerge::AutoCommit::new();
        let _ = founding.commit();
        let founded = founding.save();
        let heads: Vec<[u8; 32]> = founding.get_heads().iter().map(|head| head.0).collect();
        let mut namespace = Namespace::new("laptop").unwrap();
        namespace
            .adopt_or_merge_remote_and_resolve(&founded)
            .unwrap();
        let mut sent = record_heads(&heads);
        // A checkout while the tree could not go out: many files, many changes.
        for n in 0..200_u32 {
            let hash = iroh_blobs::Hash::new(n.to_le_bytes());
            namespace
                .create_file(
                    tilo_fs::namespace::ROOT_INO,
                    format!("file-{n}.rs"),
                    Some(&hash),
                    4,
                    1,
                )
                .unwrap();
            let _ = namespace.heads();
        }
        let whole = namespace.changes_since(&sent).len();
        let budget = whole / 5;
        let mut record = founded;
        let mut writes = 0;
        while namespace.heads() != sent {
            let (bytes, reached) = namespace.changes_since_within(&sent, budget);
            assert!(
                !bytes.is_empty(),
                "a write carries something until the heads are reached"
            );
            assert!(
                bytes.len() <= budget,
                "{} bytes over a budget of {budget}",
                bytes.len()
            );
            record.extend_from_slice(&bytes);
            sent = reached;
            writes += 1;
            assert!(writes < 50, "the writes reach the namespace's heads");
        }
        assert!(writes > 1, "the backlog went out in one write");
        let mut record = automerge::AutoCommit::load(&record).unwrap();
        assert!(
            record.get_missing_deps(&[]).is_empty(),
            "each write builds on the last"
        );
        let tree = fs_protocol::tree::Tree::of_record(&record.save()).unwrap();
        let files = tree
            .nodes
            .values()
            .filter(|node| node.name.starts_with(b"file-"))
            .count();
        assert_eq!(files, 200, "every file reached the record");
    }

    #[test]
    fn a_change_made_while_a_write_goes_out_goes_out_next() {
        let mut namespace = Namespace::new("laptop").unwrap();
        let file = |namespace: &mut Namespace, name: &str| {
            let hash = iroh_blobs::Hash::new(name.as_bytes());
            namespace
                .create_file(tilo_fs::namespace::ROOT_INO, name, Some(&hash), 4, 1)
                .unwrap();
        };
        file(&mut namespace, "contract.txt");
        let Outgoing::Write(DocumentMutation::Snapshot(first), covered) =
            outgoing(&mut namespace, &[], false)
        else {
            panic!("the first tree goes out whole");
        };
        // The mount writes another file before the write's answer is back.
        file(&mut namespace, "handbook.md");
        let Outgoing::Write(DocumentMutation::Incremental(next), _) =
            outgoing(&mut namespace, &covered, true)
        else {
            panic!("the file written meanwhile is still to send");
        };

        // What the host keeps: both writes, every change built on one it holds.
        let mut record = automerge::AutoCommit::load(&[first, next].concat()).unwrap();
        assert!(record.get_missing_deps(&[]).is_empty());
        let tree = fs_protocol::tree::Tree::of_record(&record.save()).unwrap();
        for name in [&b"contract.txt"[..], b"handbook.md"] {
            assert!(
                tree.nodes.values().any(|node| node.name.as_bytes() == name),
                "{tree:?}"
            );
        }
    }
}
