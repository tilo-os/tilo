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
//! Writes that come in a burst (a checkout writing thousands of files) go out
//! together: the changes the namespace made within [`BATCH`] of the first one
//! leave as one write per record they changed. Each file's version still
//! names who wrote it; only how many writes carry them changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::DocumentMutation;
use fs_protocol::TreeChunk;
use tilo_fs::namespace::Namespace;
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::{DocumentAnswer, WorkspaceId};
use tilo_session_client::engine::ResourceReading;
use tilo_session_client::{Cmd, DocumentRoute, Event, Link, Outbox as _};

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
        record: fs_protocol::tree::chunk_document_id(chunk),
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

/// The records kept in `dir`, each as the number of its chunk and its document.
fn kept_documents(dir: &Path) -> std::io::Result<Vec<(TreeChunk, Vec<u8>)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(chunk) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".automerge"))
            .and_then(|number| number.parse::<TreeChunk>().ok())
        else {
            continue;
        };
        records.push((chunk, std::fs::read(&path)?));
    }
    Ok(records)
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
        let kept = state.join(KEPT);
        std::fs::create_dir_all(&kept)?;
        let author = hex::encode(device);
        let records = kept_documents(&kept)?;
        let namespace = if records.is_empty() {
            Namespace::new(author).map_err(std::io::Error::other)?
        } else {
            Namespace::from_records(author, records)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?
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
            namespace: Arc::clone(&namespace),
            records: BTreeMap::new(),
            unread: BTreeSet::new(),
            unreadable: BTreeSet::new(),
            waiting: BTreeSet::new(),
            held_until: None,
            dirty_since: None,
            kept,
            kept_heads: BTreeMap::new(),
            keeping: Paced::new(KEEP_EVERY),
            pulling: Paced::new(LOOK_AGAIN),
            counting: Paced::new(COUNT_EVERY),
            counted_heads: None,
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
}

/// How long writes of a record the host refused wait before the next goes out. A refusal says
/// the host takes changes slower than this device made them, or not at all as they were:
/// waiting coalesces what is made meanwhile into one write, and each refusal in a row doubles
/// the wait, up to [`MOST_REFUSED_WAIT`]; a write that lands ends the run.
#[derive(Debug, Default)]
struct Refusals {
    /// The wait the last refusal set; zero when the last write landed.
    wait: Duration,
    /// When the next write may go out.
    until: Option<Instant>,
}

/// How long the host said to wait before it takes the next write, when it said.
fn host_wait(refusal: &tilo_relay_protocol::refusal::Refusal) -> Option<Duration> {
    use tilo_relay_protocol::refusal::Refusal;
    match refusal {
        Refusal::TooFast { wait_ms } | Refusal::TooMuchContent { wait_ms } => {
            Some(Duration::from_millis(*wait_ms))
        }
        _ => None,
    }
}

/// The first wait after a refusal.
const FIRST_REFUSED_WAIT: Duration = Duration::from_secs(1);
/// The longest wait after refusals in a row.
const MOST_REFUSED_WAIT: Duration = Duration::from_secs(30);

impl Refusals {
    /// A write was refused at `now`: the next waits as long as the host said, or else twice
    /// as long as the last. Whether it is the first of a run.
    fn refused(&mut self, now: Instant, said: Option<Duration>) -> bool {
        let first = self.wait.is_zero();
        let doubled = if first {
            FIRST_REFUSED_WAIT
        } else {
            self.wait.saturating_mul(2).min(MOST_REFUSED_WAIT)
        };
        self.wait = said.unwrap_or(doubled).max(FIRST_REFUSED_WAIT);
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
    wake: Arc<tokio::sync::Notify>,
    link: Link,
    workspace: WorkspaceId,
    namespace: Arc<Mutex<Namespace>>,
    /// Every record of the tree this device opened, by chunk: the tree's own (0) first.
    records: BTreeMap<TreeChunk, Held>,
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
    /// The directory each record is kept in, one file per record.
    kept: PathBuf,
    kept_heads: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    keeping: Paced,
    counting: Paced,
    pulling: Paced,
    /// The heads the files and bytes in `standing` were counted at.
    counted_heads: Option<Vec<automerge::ChangeHash>>,
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
            if !self.paused.load(std::sync::atomic::Ordering::Relaxed) {
                if sweep.due(Instant::now()) {
                    // Now and then every record, for a landing whose word was missed.
                    self.unread.extend(self.records.keys().copied());
                    sweep.ran(Instant::now(), Instant::now());
                }
                if session_moved || !self.unread.is_empty() {
                    self.pull_all().await;
                }
                self.push();
                self.content.reask(&self.link);
            }
            self.say_where_it_stands();
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
                moved = changes.changed() => if moved.is_err() { return },
                _ = keep.tick() => if self.keeping.due(Instant::now()) {
                    let started = Instant::now();
                    self.keep().await;
                    self.keeping.ran(started, Instant::now());
                },
                () = tokio::time::sleep(wait) => session_moved = self.dirty_since.is_none(),
            }
        }
    }

    /// Ask the host for record `chunk` of the tree, and to take part in its channel.
    fn open(&mut self, chunk: TreeChunk) {
        self.records.entry(chunk).or_default();
        self.unread.insert(chunk);
        let route = route(self.workspace, chunk);
        let _ = self
            .link
            .send(Cmd::OpenDocumentAt(self.workspace, route.clone()));
        let _ = self.link.send(Cmd::OpenRoute(route.clone()));
        // And on the workspace's plane, whose host answers what it holds.
        let _ = self.link.engine().open_resource(
            &self.workspace.to_string(),
            &route.app_id,
            &route.record,
        );
    }

    fn take(&mut self, event: Event) {
        match event {
            Event::HostReady { ws, .. } if ws == self.workspace => {
                let opened: Vec<TreeChunk> = self.records.keys().copied().collect();
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
                if let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.record) {
                    let held = self.records.entry(chunk).or_default();
                    held.sent = held
                        .merged
                        .iter()
                        .flatten()
                        .map(|hash| automerge::ChangeHash(*hash))
                        .collect();
                    if held.refused.refused(Instant::now(), host_wait(&refusal)) {
                        tracing::warn!(%refusal, workspace = %self.workspace, record = %route.record, "the workspace refused a change to its files; it goes again after a wait");
                    }
                }
            }
            Event::Landed { route, .. }
                if route.cluster_id == self.workspace.to_string() && route.app_id == FILES_APP =>
            {
                if let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.record) {
                    self.unread.insert(chunk);
                    self.records.entry(chunk).or_default().refused.landed();
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
                if let Some(chunk) = fs_protocol::tree::chunk_of_record(&route.record) {
                    self.records.entry(chunk).or_default().answered = Some(answer);
                    // A host that sent some of the tree holds one, which this device reads
                    // before it writes: a tree it founded beside that one would go out whole.
                    if chunk == 0 && holds_none(Some(answer)) {
                        self.standing.lock_recover().opened = true;
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
                .records
                .get(&chunk)
                .and_then(|held| held.merged.clone());
            let reading =
                engine.record_update(&workspace, &route.app_id, &route.record, merged.as_deref());
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
                            &route.record,
                        ))
                    {
                        self.standing.lock_recover().opened = true;
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
                .take_records(records)
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
                self.standing.lock_recover().opened = true;
            }
            let held = self.records.entry(chunk).or_default();
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
            if !self.records.contains_key(&chunk) {
                self.open(chunk);
            }
        }
    }

    /// Hand the session what the namespace changed since it last did, once the batch the
    /// first of those changes started is due: one write per record that changed.
    fn push(&mut self) {
        if !self.standing.lock_recover().opened {
            // The host has not said what it holds: a tree written now could found a second
            // one beside it.
            return;
        }
        let heads = self.namespace.lock_recover().record_heads();
        let dirty: Vec<TreeChunk> = heads
            .iter()
            .filter(|(chunk, heads)| self.records.get(chunk).map(|held| &held.sent) != Some(*heads))
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
        let link = self.link.clone();
        let engine = link.engine();
        let workspace = self.workspace.to_string();
        let mut waiting = false;
        let now = Instant::now();
        self.held_until = None;
        for chunk in dirty {
            if !self.records.contains_key(&chunk) {
                self.open(chunk);
            }
            if let Some(until) = self
                .records
                .get(&chunk)
                .and_then(|held| held.refused.held_until(now))
            {
                self.held_until = Some(self.held_until.map_or(until, |held| held.min(until)));
                waiting = true;
                continue;
            }
            let route = route(self.workspace, chunk);
            let founded = self.records.get(&chunk).is_some_and(|held| held.founded);
            if !founded {
                // A record this device starts: the host says first whether the workspace has
                // it. One it has is merged before this device's changes of it go out; one it
                // has not is founded here, with the empty start every device makes alike.
                // Its changes are saved only once they can go out: saving walks the record.
                // The session's own answer to its open, as a reader of the record hears it: a
                // record the host holds frames of is merged before this device's changes of it
                // go out, one it holds none of is founded here.
                let answered = self.records.get(&chunk).and_then(|held| held.answered);
                if !holds_none(answered)
                    || engine.holds_resource(&workspace, &route.app_id, &route.record)
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
                if let Some(held) = self.records.get_mut(&chunk) {
                    held.founded = true;
                }
            }
            let sent = self
                .records
                .get(&chunk)
                .map(|held| held.sent.clone())
                .unwrap_or_default();
            let Some(write) =
                self.namespace
                    .lock_recover()
                    .record_write(chunk, &sent, WRITE_BUDGET)
            else {
                continue;
            };
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
            if let Some(held) = self.records.get_mut(&chunk) {
                held.sent = write.heads;
            }
        }
        self.dirty_since = waiting.then_some(since);
    }

    fn say_where_it_stands(&mut self) {
        let (heads, records) = {
            let mut namespace = self.namespace.lock_recover();
            (namespace.heads(), namespace.record_heads())
        };
        let counted =
            if self.counted_heads.as_ref() != Some(&heads) && self.counting.due(Instant::now()) {
                let started = Instant::now();
                let counted = self.count();
                self.counting.ran(started, Instant::now());
                self.counted_heads = Some(heads);
                Some(counted)
            } else {
                None
            };
        let sent = records.iter().all(|(chunk, heads)| {
            self.records
                .get(chunk)
                .is_some_and(|held| held.sent == *heads)
        });
        let durable = !self.records.keys().any(|chunk| self.holding(*chunk))
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

    /// Keep the records that changed on disk, each in a file of its own, so a restart starts
    /// where this one was: what a write costs on disk is its record, not the tree. Saving a
    /// record walks it, so it runs away from the runtime's workers like a merge does.
    async fn keep(&mut self) {
        let namespace = Arc::clone(&self.namespace);
        let kept = self.kept.clone();
        let kept_heads = self.kept_heads.clone();
        match tokio::task::spawn_blocking(move || keep_records(&namespace, &kept, &kept_heads))
            .await
        {
            Ok(written) => self.kept_heads.extend(written),
            Err(error) => tracing::warn!(%error, "keeping the workspace's tree could not run"),
        }
    }

    /// [`Self::keep`], on this thread: where the sync stops there is no runtime to run it on.
    fn keep_now(&mut self) {
        let written = keep_records(&self.namespace, &self.kept, &self.kept_heads);
        self.kept_heads.extend(written);
    }
}

/// Write each record of `namespace` whose heads are not those `kept_heads` has for it to its own
/// file under `kept`; answers the records written, with the heads they were written at.
fn keep_records(
    namespace: &Mutex<Namespace>,
    kept: &std::path::Path,
    kept_heads: &BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
) -> Vec<(TreeChunk, Vec<automerge::ChangeHash>)> {
    let mut saved = Vec::new();
    {
        let mut namespace = namespace.lock_recover();
        for (chunk, heads) in namespace.record_heads() {
            if kept_heads.get(&chunk) == Some(&heads) {
                continue;
            }
            if let Some(bytes) = namespace.save_record(chunk) {
                saved.push((chunk, heads, bytes));
            }
        }
    }
    let mut written = Vec::new();
    for (chunk, heads, bytes) in saved {
        let path = kept.join(format!("{chunk}.automerge"));
        match tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600) {
            Ok(()) => written.push((chunk, heads)),
            Err(error) => tracing::warn!(%error, "the workspace's tree is not kept on disk"),
        }
    }
    written
}

impl Drop for Sync {
    fn drop(&mut self) {
        self.keep_now();
    }
}

/// Whether the host's `answer` about a record of the tree (the frames it sent of it, `None`
/// while it owes one) says the workspace has none of it.
fn holds_none(answer: Option<DocumentAnswer>) -> bool {
    answer == Some(DocumentAnswer::Sent { frames: 0 })
}

#[cfg(test)]
mod tests {
    use tilo_relay_protocol::DocumentAnswer;

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
            refusals.refused(start, None),
            "the first refusal of a run is said"
        );
        assert_eq!(refusals.held_until(start), Some(start + FIRST_REFUSED_WAIT));
        assert!(
            !refusals.refused(start, None),
            "the rest of the run is not said again"
        );
        assert_eq!(
            refusals.held_until(start),
            Some(start + FIRST_REFUSED_WAIT * 2)
        );
        for _ in 0..10 {
            refusals.refused(start, None);
        }
        assert_eq!(refusals.held_until(start), Some(start + MOST_REFUSED_WAIT));
        assert_eq!(
            refusals.held_until(start + MOST_REFUSED_WAIT),
            None,
            "the wait ends"
        );
        refusals.landed();
        assert!(
            refusals.refused(start, None),
            "a write that landed ends the run"
        );
        assert_eq!(refusals.held_until(start), Some(start + FIRST_REFUSED_WAIT));
        let said = std::time::Duration::from_secs(7);
        refusals.refused(start, Some(said));
        assert_eq!(
            refusals.held_until(start),
            Some(start + said),
            "a wait the host said is the wait"
        );
        assert_eq!(
            super::host_wait(&tilo_relay_protocol::refusal::Refusal::TooFast { wait_ms: 7000 }),
            Some(said)
        );
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
            .record_heads()
            .into_iter()
            .map(|(chunk, heads)| (chunk, (desktop.save_record(chunk).unwrap(), heads)))
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
            .take_records(
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
        host.take_records(writes.into_iter().map(|w| (w.chunk, w.changes)).collect())
            .unwrap();
        for name in ["contract.txt", "handbook.md.txt"] {
            assert!(
                host.lookup(tilo_fs::namespace::ROOT_INO, name).is_some(),
                "{name} reached the host"
            );
        }
        assert_eq!(host.record_heads(), laptop.record_heads());
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
                namespace.save_record(chunk).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(dir.path().join("stray"), b"x").unwrap();
        let read = Namespace::from_records("laptop", kept_documents(dir.path()).unwrap()).unwrap();
        let mut read = read;
        assert!(read.lookup(tilo_fs::namespace::ROOT_INO, "a.txt").is_some());
        assert_eq!(read.record_heads(), namespace.record_heads());
    }
}
